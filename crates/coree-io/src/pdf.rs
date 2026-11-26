// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust print-ready PDF 1.7 writer supporting multipage vector artwork, true
//! `DeviceCMYK` (`k`/`K`) and `DeviceRGB` (`rg`/`RG`) color spaces, `ExtGState`
//! opacity/blend modes/overprint, `MediaBox`/`TrimBox`/`BleedBox`, print marks
//! (bleed, crop marks, registration marks, CMYK color calibration bars, page slug),
//! embedded raster XObjects, text + outline fallback, and byte-exact xref tables.

use crate::atomic::atomic_write_bytes;
use crate::display_list::{DisplayListOptions, DisplayOp, ExportDisplayList};
use crate::error::{IoError, IoResult};
use crate::font_subset::{EmbeddedPdfFont, FontFaceKey, PdfFontRegistry};
use coree_core::{
    generate_minimal_cmyk_icc_v2, generate_minimal_srgb_icc_v2, Affine2D, BezierPath, BlendMode,
    BuiltinIccProfile, Color, ColorMode, Document, Fill, GradientStop, IccProfile, LineCap,
    LineJoin, OutlineStyle, Page, Point2D,
};
use coree_render::{outline_arrowhead_geometries, ShapedPdfRun, TextEngine};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as IoWrite;
use std::path::Path;

/// Exact millimeter to PDF typographic point conversion (`72 / 25.4`).
const MM_TO_PT: f64 = 72.0 / 25.4;

/// Output color space mode for PDF vector painting operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PdfColorSpace {
    /// Follows `doc.color_mode` (`ColorMode::Cmyk` -> `DeviceCMYK`, `ColorMode::Rgb` -> `DeviceRGB`).
    #[default]
    DocumentDefault,
    /// Forces `DeviceCMYK` (`k` / `K`) operators for commercial offset/print press output.
    DeviceCmyk,
    /// Forces `DeviceRGB` (`rg` / `RG`) operators for digital/screen PDF output.
    DeviceRgb,
}

/// PDF/X conformance standard emitted in `/Info` and `/OutputIntents`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PdfXConformance {
    /// Selects `PDF/X-1a:2001` for opaque CMYK output, or `PDF/X-4` when transparency or RGB is used.
    #[default]
    Auto,
    /// ISO 15930-1 `PDF/X-1a:2001` (CMYK + Spot).
    PdfX1a2001,
    /// ISO 15930-7 `PDF/X-4` (ICC + transparency groups).
    PdfX4,
    /// Plain PDF 1.7 without `/GTS_PDFXVersion` conformance tags.
    None,
}

/// Configuration for [`export_pdf`] and [`export_pdf_to_bytes`].
#[derive(Debug, Clone, PartialEq)]
pub struct PdfExportOptions {
    pub color_space: PdfColorSpace,
    /// Expands `BleedBox` by `page.bleed_mm` around `TrimBox`.
    pub include_bleed: bool,
    /// Renders hairline trim/crop corner marks outside `BleedBox`.
    pub crop_marks: bool,
    /// Renders 4 crosshair + target registration marks centered on all four margins.
    pub registration_marks: bool,
    /// Renders CMYK + RGB + grayscale tint calibration strip along the top margin.
    pub color_calibration_bars: bool,
    /// Prints page title, index, trim size, and color space slug in the bottom margin.
    pub page_info_slug: bool,
    /// Compresses page content streams using `/FlateDecode` when `true`.
    pub compress_streams: bool,
    /// Optional explicit list of 0-based page indices to export (`None` = all pages).
    pub pages: Option<Vec<usize>>,
    /// PDF/X conformance mode (`Auto` defaults to `PDF/X-1a:2001` or `PDF/X-4`).
    pub pdfx_conformance: PdfXConformance,
}

impl Default for PdfExportOptions {
    fn default() -> Self {
        Self {
            color_space: PdfColorSpace::DocumentDefault,
            include_bleed: true,
            crop_marks: false,
            registration_marks: false,
            color_calibration_bars: false,
            page_info_slug: false,
            compress_streams: false,
            pages: None,
            pdfx_conformance: PdfXConformance::Auto,
        }
    }
}

impl PdfExportOptions {
    /// Print-press ready preset: `DeviceCMYK`, bleed, crop marks, registration marks,
    /// color calibration bars, and page info slug enabled.
    #[must_use]
    pub fn print_ready() -> Self {
        Self {
            color_space: PdfColorSpace::DeviceCmyk,
            include_bleed: true,
            crop_marks: true,
            registration_marks: true,
            color_calibration_bars: true,
            page_info_slug: true,
            compress_streams: false,
            pages: None,
            pdfx_conformance: PdfXConformance::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GStateKey {
    fill_alpha_milli: u16,
    stroke_alpha_milli: u16,
    blend_mode: &'static str,
    overprint: bool,
}

enum PdfObjectSink {
    Memory(Vec<Vec<u8>>),
    Streaming {
        writer: std::io::BufWriter<std::fs::File>,
        offsets: Vec<u64>, // 0-based index => obj_num = idx + 1; 0 = pending placeholder
        byte_pos: u64,
    },
}

struct PdfObjectStore {
    sink: PdfObjectSink,
}

impl PdfObjectStore {
    fn new() -> Self {
        Self {
            sink: PdfObjectSink::Memory(Vec::new()),
        }
    }

    fn new_streaming(path: &Path) -> IoResult<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(path)?;
        let mut writer = std::io::BufWriter::with_capacity(256 * 1024, file);
        let header = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n";
        writer.write_all(header)?;
        Ok(Self {
            sink: PdfObjectSink::Streaming {
                writer,
                offsets: Vec::with_capacity(1024),
                byte_pos: header.len() as u64,
            },
        })
    }

    fn alloc_placeholder(&mut self) -> usize {
        match &mut self.sink {
            PdfObjectSink::Memory(objs) => {
                objs.push(Vec::new());
                objs.len()
            }
            PdfObjectSink::Streaming { offsets, .. } => {
                offsets.push(0);
                offsets.len()
            }
        }
    }

    fn write_streaming_obj(
        writer: &mut std::io::BufWriter<std::fs::File>,
        offsets: &mut [u64],
        byte_pos: &mut u64,
        obj_num: usize,
        body: &[u8],
    ) -> IoResult<()> {
        offsets[obj_num - 1] = *byte_pos;
        let head = format!("{obj_num} 0 obj\n");
        writer.write_all(head.as_bytes())?;
        writer.write_all(body)?;
        let trail: &[u8] = if body.ends_with(b"\n") {
            b"endobj\n"
        } else {
            b"\nendobj\n"
        };
        writer.write_all(trail)?;
        *byte_pos += (head.len() + body.len() + trail.len()) as u64;
        Ok(())
    }

    fn add(&mut self, body: Vec<u8>) -> usize {
        match &mut self.sink {
            PdfObjectSink::Memory(objs) => {
                objs.push(body);
                objs.len()
            }
            PdfObjectSink::Streaming {
                writer,
                offsets,
                byte_pos,
            } => {
                offsets.push(0);
                let id = offsets.len();
                let _ = Self::write_streaming_obj(writer, offsets, byte_pos, id, &body);
                id
            }
        }
    }

    fn set(&mut self, id: usize, body: Vec<u8>) {
        match &mut self.sink {
            PdfObjectSink::Memory(objs) => {
                objs[id - 1] = body;
            }
            PdfObjectSink::Streaming {
                writer,
                offsets,
                byte_pos,
            } => {
                let _ = Self::write_streaming_obj(writer, offsets, byte_pos, id, &body);
            }
        }
    }

    fn finish_streaming(self, root_id: usize, info_id: usize, title: &str) -> IoResult<()> {
        let PdfObjectSink::Streaming {
            mut writer,
            offsets,
            byte_pos,
        } = self.sink
        else {
            return Err(IoError::InvalidFormat {
                format: "pdf".into(),
                message: "finish_streaming called on in-memory store".into(),
            });
        };

        let mut h1 = 0xcbf2_9ce4_8422_2325_u64;
        let mut h2 = 0x8422_2325_cbf2_9ce4_u64;
        for b in title.as_bytes() {
            h1 = (h1 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
            h2 = (h2 ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        h1 ^= byte_pos;
        h2 ^= (offsets.len() + 1) as u64;
        let file_id_hex = format!("{h1:016X}{h2:016X}");

        let xref_offset = byte_pos;
        let total = offsets.len() + 1;
        writeln!(writer, "xref")?;
        writeln!(writer, "0 {total}")?;
        writer.write_all(b"0000000000 65535 f \n")?;
        for &off in &offsets {
            writeln!(writer, "{off:010} 00000 n ")?;
        }
        writeln!(writer, "trailer")?;
        writeln!(
            writer,
            "<< /Size {total} /Root {root_id} 0 R /Info {info_id} 0 R /ID [<{file_id_hex}> <{file_id_hex}>] >>"
        )?;
        writeln!(writer, "startxref")?;
        writeln!(writer, "{xref_offset}")?;
        writer.write_all(b"%%EOF\n")?;
        writer.flush()?;
        Ok(())
    }

    fn serialize(self, root_id: usize, info_id: usize, title: &str) -> Vec<u8> {
        let PdfObjectSink::Memory(objects) = self.sink else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(8192);
        // PDF 1.7 header + standard 4-byte binary comment
        out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");

        let mut offsets = Vec::with_capacity(objects.len() + 1);
        offsets.push(0usize); // object 0 (free head)

        for (idx, body) in objects.into_iter().enumerate() {
            let obj_num = idx + 1;
            offsets.push(out.len());
            let _ = writeln!(out, "{obj_num} 0 obj");
            out.extend_from_slice(&body);
            if !body.ends_with(b"\n") {
                out.push(b'\n');
            }
            out.extend_from_slice(b"endobj\n");
        }

        let mut h1 = 0xcbf2_9ce4_8422_2325_u64;
        let mut h2 = 0x8422_2325_cbf2_9ce4_u64;
        for b in title.as_bytes() {
            h1 = (h1 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
            h2 = (h2 ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        h1 ^= out.len() as u64;
        h2 ^= offsets.len() as u64;
        let file_id_hex = format!("{h1:016X}{h2:016X}");

        let xref_offset = out.len();
        let total = offsets.len();
        let _ = writeln!(out, "xref");
        let _ = writeln!(out, "0 {total}");
        // Strictly 20-byte xref entries per ISO 32000-1 §7.5.4
        out.extend_from_slice(b"0000000000 65535 f \n");
        for &off in &offsets[1..] {
            let _ = writeln!(out, "{off:010} 00000 n ");
        }

        let _ = writeln!(out, "trailer");
        let _ = writeln!(
            out,
            "<< /Size {total} /Root {root_id} 0 R /Info {info_id} 0 R /ID [<{file_id_hex}> <{file_id_hex}>] >>"
        );
        let _ = writeln!(out, "startxref");
        let _ = writeln!(out, "{xref_offset}");
        out.extend_from_slice(b"%%EOF\n");
        out
    }
}

/// Exports `doc` to a print-ready PDF/X-1a:2001, PDF/X-4, or PDF 1.7 byte buffer.
pub fn export_pdf_to_bytes(doc: &Document, opts: &PdfExportOptions) -> IoResult<Vec<u8>> {
    if doc.pages().is_empty() {
        return Err(IoError::InvalidFormat {
            format: "pdf".into(),
            message: "document has no pages".into(),
        });
    }

    let page_indices: Vec<usize> = match &opts.pages {
        Some(list) => {
            for &idx in list {
                if idx >= doc.pages().len() {
                    return Err(IoError::PageOutOfRange {
                        page_index: idx,
                        page_count: doc.pages().len(),
                    });
                }
            }
            if list.is_empty() {
                return Err(IoError::InvalidFormat {
                    format: "pdf".into(),
                    message: "selected page list is empty".into(),
                });
            }
            list.clone()
        }
        None => (0..doc.pages().len()).collect(),
    };

    let use_cmyk = match opts.color_space {
        PdfColorSpace::DeviceCmyk => true,
        PdfColorSpace::DeviceRgb => false,
        PdfColorSpace::DocumentDefault => doc.color_mode() == ColorMode::Cmyk,
    };
    let doc_icc = doc.icc_pipeline();
    let use_icc_for_raster =
        use_cmyk && (doc.cmyk_icc_profile().is_some() || doc.custom_icc_profile().is_some());

    let mut store = PdfObjectStore::new();
    let catalog_id = store.alloc_placeholder(); // 1
    let info_id = store.alloc_placeholder(); // 2
    let pages_root_id = store.alloc_placeholder(); // 3

    // 1. Single-pass scene traversal into `ExportDisplayList` (`CdrPsi`) per exported page
    let mut text_engine = TextEngine::new();
    let dl_opts = DisplayListOptions::pdf();
    let mut display_lists = Vec::with_capacity(page_indices.len());
    let mut slug_runs_per_page = Vec::with_capacity(page_indices.len());
    let mut font_registry = PdfFontRegistry::new();

    for &page_index in &page_indices {
        let dl = ExportDisplayList::build(doc, page_index, &dl_opts, &mut text_engine)?;
        for op in &dl.ops {
            if let DisplayOp::DrawTextRuns { shaped_runs, .. } = op {
                for r in shaped_runs {
                    font_registry.register_run(r);
                }
            }
        }
        let slug_layout = if opts.page_info_slug {
            let page = &doc.pages()[page_index];
            let bleed_mm = if opts.include_bleed && page.bleed_mm.is_finite() {
                page.bleed_mm.max(0.0)
            } else {
                0.0
            };
            let mode_str = if use_cmyk { "DeviceCMYK" } else { "DeviceRGB" };
            let slug = format!(
                "Page {} ({}) | {:.1}x{:.1}mm | Bleed {:.1}mm | {mode_str} | coree",
                page_index + 1,
                page.name,
                page.width_mm,
                page.height_mm,
                bleed_mm
            );
            let slug_color = if use_cmyk {
                Color::cmyk(0.0, 0.0, 0.0, 1.0)
            } else {
                Color::BLACK
            };
            let at = coree_core::ArtisticText::from_plain(
                slug,
                coree_core::TextStyle {
                    font_size_pt: 6.0,
                    fill: slug_color,
                    ..coree_core::TextStyle::default()
                },
            );
            let (s_runs, s_curves) = text_engine.layout_artistic_pdf_runs(&at);
            for r in &s_runs {
                font_registry.register_run(r);
            }
            Some((s_runs, s_curves))
        } else {
            None
        };
        slug_runs_per_page.push(slug_layout);
        display_lists.push(dl);
    }

    // 2. Emit subsetted `/Type0` + `/CIDFontType2` + `/ToUnicode` fonts (zero unused `/F1`/`/F2`!)
    let embedded_fonts = font_registry.emit_all(&mut |bytes| store.add(bytes))?;

    // Pre-register document raster assets referenced by `DrawRaster`
    let mut image_xobjects: BTreeMap<coree_core::ImageAssetId, (String, usize)> = BTreeMap::new();
    for (img_idx, (&asset_id, asset)) in doc.assets().iter().enumerate() {
        let name = format!("Im{}", img_idx + 1);
        let xobj_id = add_raster_xobject(
            &mut store,
            asset,
            use_cmyk,
            use_icc_for_raster.then_some(&doc_icc),
        )?;
        image_xobjects.insert(asset_id, (name, xobj_id));
    }

    let any_transparency = display_lists.iter().any(|dl| dl.has_transparency);
    let resolved_pdfx = match opts.pdfx_conformance {
        PdfXConformance::Auto => {
            if use_cmyk && !any_transparency {
                PdfXConformance::PdfX1a2001
            } else {
                PdfXConformance::PdfX4
            }
        }
        other => other,
    };

    let mut dynamic_img_counter = image_xobjects.len();
    let mut spot_cache: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut page_obj_ids = Vec::with_capacity(display_lists.len());

    for (dl, slug_opt) in display_lists.iter().zip(slug_runs_per_page.iter()) {
        let page_index = dl.page_index;
        let page = &doc.pages()[page_index];
        let bleed_mm = if opts.include_bleed && page.bleed_mm.is_finite() {
            page.bleed_mm.max(0.0)
        } else {
            0.0
        };
        let has_marks = opts.crop_marks
            || opts.registration_marks
            || opts.color_calibration_bars
            || opts.page_info_slug;
        let margin_mm = if has_marks { bleed_mm + 12.0 } else { bleed_mm };

        let media_w_pt = (page.width_mm + 2.0 * margin_mm) * MM_TO_PT;
        let media_h_pt = (page.height_mm + 2.0 * margin_mm) * MM_TO_PT;
        let trim_x0 = margin_mm * MM_TO_PT;
        let trim_y0 = margin_mm * MM_TO_PT;
        let trim_x1 = (margin_mm + page.width_mm) * MM_TO_PT;
        let trim_y1 = (margin_mm + page.height_mm) * MM_TO_PT;
        let bleed_x0 = (margin_mm - bleed_mm) * MM_TO_PT;
        let bleed_y0 = (margin_mm - bleed_mm) * MM_TO_PT;
        let bleed_x1 = (margin_mm + page.width_mm + bleed_mm) * MM_TO_PT;
        let bleed_y1 = (margin_mm + page.height_mm + bleed_mm) * MM_TO_PT;

        let is_pdfx1a = resolved_pdfx == PdfXConformance::PdfX1a2001;
        let mut emit_state = PdfPageEmit {
            store: &mut store,
            embedded_fonts: &embedded_fonts,
            font_ptr_cache: font_registry.ptr_cache(),
            used_fonts: BTreeMap::new(),
            gstates: BTreeMap::new(),
            used_images: BTreeMap::new(),
            shadings: BTreeMap::new(),
            used_spots: BTreeMap::new(),
            spot_cache: &mut spot_cache,
            dynamic_img_counter: &mut dynamic_img_counter,
            use_icc_for_raster,
            doc_icc: &doc_icc,
            is_pdfx1a,
        };
        let mut stream = String::with_capacity(4096);

        let ctx = PageRenderCtx {
            page,
            margin_mm,
            use_cmyk,
            image_xobjects: &image_xobjects,
        };

        // Clip artwork to BleedBox so bleed extends cleanly without crossing marks
        let _ = writeln!(
            stream,
            "q\n{bleed_x0:.4} {bleed_y0:.4} {:.4} {:.4} re W n",
            bleed_x1 - bleed_x0,
            bleed_y1 - bleed_y0
        );

        for op in &dl.ops {
            emit_display_op(&ctx, op, &mut emit_state, &mut stream);
        }
        stream.push_str("Q\n");

        // Emit print marks outside the BleedBox
        if has_marks {
            emit_print_marks(
                &ctx,
                &mut stream,
                page,
                margin_mm,
                bleed_mm,
                use_cmyk,
                opts,
                slug_opt.as_ref(),
                &mut emit_state,
            );
        }

        let used_fonts = emit_state.used_fonts;
        let gstates = emit_state.gstates;
        let used_images = emit_state.used_images;
        let page_shadings = emit_state.shadings;
        let page_spots = emit_state.used_spots;

        let mut font_refs = String::new();
        for (f_name, f_id) in &used_fonts {
            let _ = write!(font_refs, " /{f_name} {f_id} 0 R");
        }

        // Build ExtGState objects for this page
        let gstate_refs = build_extgstate_objects(&mut store, &gstates, is_pdfx1a);

        let mut xobj_refs = String::new();
        for (im_name, im_id) in &used_images {
            let _ = write!(xobj_refs, " /{im_name} {im_id} 0 R");
        }

        let mut shading_refs = String::new();
        for (sh_name, sh_id) in &page_shadings {
            let _ = write!(shading_refs, " /{sh_name} {sh_id} 0 R");
        }

        let mut colorspace_refs = String::new();
        for (cs_name, cs_id) in &page_spots {
            let _ = write!(colorspace_refs, " /{cs_name} {cs_id} 0 R");
        }

        // Build content stream object (optionally FlateDecode compressed)
        let content_obj_bytes = if opts.compress_streams {
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
            enc.write_all(stream.as_bytes())?;
            let compressed = enc.finish()?;
            let mut buf = format!(
                "<< /Length {} /Filter /FlateDecode >>\nstream\n",
                compressed.len()
            )
            .into_bytes();
            buf.extend_from_slice(&compressed);
            buf.extend_from_slice(b"\nendstream");
            buf
        } else {
            let raw = stream.as_bytes();
            let mut buf = format!("<< /Length {} >>\nstream\n", raw.len()).into_bytes();
            buf.extend_from_slice(raw);
            buf.extend_from_slice(b"\nendstream");
            buf
        };
        let content_id = store.add(content_obj_bytes);

        let mut resources = String::from("<<");
        if !font_refs.is_empty() {
            let _ = write!(resources, " /Font <<{font_refs} >>");
        }
        if !gstate_refs.is_empty() {
            let _ = write!(resources, " /ExtGState <<{gstate_refs} >>");
        }
        if !xobj_refs.is_empty() {
            let _ = write!(resources, " /XObject <<{xobj_refs} >>");
        }
        if !shading_refs.is_empty() {
            let _ = write!(resources, " /Shading <<{shading_refs} >>");
        }
        if !colorspace_refs.is_empty() {
            let _ = write!(resources, " /ColorSpace <<{colorspace_refs} >>");
        }
        resources.push_str(" >>");

        let group_entry = if dl.has_transparency {
            let grp_cs = if use_cmyk {
                "/DeviceCMYK"
            } else {
                "/DeviceRGB"
            };
            format!(" /Group << /S /Transparency /CS {grp_cs} /I true >>")
        } else {
            String::new()
        };

        let page_dict = format!(
            "<< /Type /Page /Parent {pages_root_id} 0 R \
             /MediaBox [0 0 {media_w_pt:.4} {media_h_pt:.4}] \
             /TrimBox [{trim_x0:.4} {trim_y0:.4} {trim_x1:.4} {trim_y1:.4}] \
             /BleedBox [{bleed_x0:.4} {bleed_y0:.4} {bleed_x1:.4} {bleed_y1:.4}]{group_entry} \
             /Resources {resources} \
             /Contents {content_id} 0 R >>"
        );
        let page_obj_id = store.add(page_dict.into_bytes());
        page_obj_ids.push(page_obj_id);
    }

    let kids: Vec<String> = page_obj_ids.iter().map(|id| format!("{id} 0 R")).collect();
    let pages_dict = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        page_obj_ids.len()
    );
    store.set(pages_root_id, pages_dict.into_bytes());

    // Embed ISO 32000-1 /OutputIntents ICC profile stream (custom doc ICC or clean-room CMYK/sRGB ICC v2)
    let raw_icc = if let Some(bytes) = doc.custom_icc_profile() {
        bytes.to_vec()
    } else if use_cmyk {
        generate_minimal_cmyk_icc_v2(
            "ISO Coated v2 CMYK (coree Clean-Room)",
            BuiltinIccProfile::IsoCoatedV2Cmyk,
        )
    } else {
        generate_minimal_srgb_icc_v2("sRGB IEC61966-2.1 (coree Clean-Room)")
    };
    let parsed_icc = IccProfile::from_bytes(&raw_icc).ok();
    let icc_n = parsed_icc
        .as_ref()
        .map_or(if use_cmyk { 4 } else { 3 }, |p| {
            p.header.color_space.channel_count().clamp(1, 4)
        });
    let icc_info = parsed_icc
        .as_ref()
        .map(|p| p.description.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if use_cmyk {
                "ISO Coated v2 CMYK".to_owned()
            } else {
                "sRGB IEC61966-2.1".to_owned()
            }
        });
    let icc_cond = if icc_n == 4 { "FOGRA39" } else { "sRGB" };

    let mut compressed_icc = Vec::new();
    let mut enc_icc = ZlibEncoder::new(Vec::new(), Compression::fast());
    let icc_obj_bytes = if enc_icc.write_all(&raw_icc).is_ok()
        && enc_icc.finish().map(|v| compressed_icc = v).is_ok()
    {
        let mut buf = format!(
            "<< /N {icc_n} /Length {} /Filter /FlateDecode >>\nstream\n",
            compressed_icc.len()
        )
        .into_bytes();
        buf.extend_from_slice(&compressed_icc);
        buf.extend_from_slice(b"\nendstream");
        buf
    } else {
        let mut buf = format!("<< /N {icc_n} /Length {} >>\nstream\n", raw_icc.len()).into_bytes();
        buf.extend_from_slice(&raw_icc);
        buf.extend_from_slice(b"\nendstream");
        buf
    };
    let icc_stream_id = store.add(icc_obj_bytes);
    let oi_dict = format!(
        "<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier ({cond}) /Info ({info}) /RegistryName (http://www.color.org) /DestOutputProfile {icc_stream_id} 0 R >>",
        cond = escape_pdf_string(icc_cond),
        info = escape_pdf_string(&icc_info),
    );
    let output_intent_id = store.add(oi_dict.into_bytes());

    let catalog_dict = format!(
        "<< /Type /Catalog /Pages {pages_root_id} 0 R /OutputIntents [{output_intent_id} 0 R] >>"
    );
    store.set(catalog_id, catalog_dict.into_bytes());

    let now_pdf = chrono::Utc::now().format("D:%Y%m%d%H%M%SZ").to_string();
    let pdfx_meta = match resolved_pdfx {
        PdfXConformance::PdfX1a2001 => {
            " /GTS_PDFXVersion (PDF/X-1a:2001) /GTS_PDFXConformance (PDF/X-1a:2001) /Trapped /False"
        }
        PdfXConformance::PdfX4 => {
            " /GTS_PDFXVersion (PDF/X-4) /GTS_PDFXConformance (PDF/X-4) /Trapped /False"
        }
        PdfXConformance::Auto | PdfXConformance::None => "",
    };
    let info_dict = format!(
        "<< /Title ({}) /Author ({}) /Creator (coree) /Producer (coree-io PDF/X Writer) /CreationDate ({now_pdf}) /ModDate ({now_pdf}){pdfx_meta} >>",
        escape_pdf_string(doc.title()),
        escape_pdf_string(&doc.metadata().author)
    );
    store.set(info_id, info_dict.into_bytes());

    Ok(store.serialize(catalog_id, info_id, doc.title()))
}

/// Atomically exports `doc` to a print-ready PDF file at `path`.
pub fn export_pdf(doc: &Document, opts: &PdfExportOptions, path: &Path) -> IoResult<()> {
    let bytes = export_pdf_to_bytes(doc, opts)?;
    atomic_write_bytes(path, &bytes)
}

/// Incremental, constant-memory PDF 1.7 writer for large variable-data merges (`10,000+` pages).
///
/// Reuses one `TextEngine`, one `PdfFontRegistry`, shared raster XObjects, and writes each page's
/// `/FlateDecode` content stream and `/Page` dictionary directly to `BufWriter<File>` before
/// emitting `/Type0` + `/ToUnicode` fonts, `/Pages`, `/Catalog`, and `xref` on [`Self::finish`].
pub struct StreamingPdfDocument {
    store: PdfObjectStore,
    catalog_id: usize,
    info_id: usize,
    pages_root_id: usize,
    shared_fonts_dict_id: usize,
    opts: PdfExportOptions,
    use_cmyk: bool,
    use_icc_for_raster: bool,
    doc_icc: coree_core::IccColorPipeline,
    custom_icc: Option<Vec<u8>>,
    title: String,
    author: String,
    text_engine: TextEngine,
    font_registry: PdfFontRegistry,
    embedded_fonts: std::collections::HashMap<FontFaceKey, EmbeddedPdfFont>,
    image_xobjects: BTreeMap<coree_core::ImageAssetId, (String, usize)>,
    spot_cache: BTreeMap<String, (String, usize)>,
    dynamic_img_counter: usize,
    any_transparency: bool,
    page_obj_ids: Vec<usize>,
    deflater: flate2::Compress,
    stream_buf: String,
    comp_buf: Vec<u8>,
}

impl StreamingPdfDocument {
    /// Initializes a streaming PDF file at `path` from `template_doc`'s metadata, ICC pipeline,
    /// and static raster assets.
    pub fn create(template_doc: &Document, opts: &PdfExportOptions, path: &Path) -> IoResult<Self> {
        let mut store = PdfObjectStore::new_streaming(path)?;
        let catalog_id = store.alloc_placeholder(); // 1
        let info_id = store.alloc_placeholder(); // 2
        let pages_root_id = store.alloc_placeholder(); // 3
        let shared_fonts_dict_id = store.alloc_placeholder(); // 4

        let use_cmyk = match opts.color_space {
            PdfColorSpace::DeviceCmyk => true,
            PdfColorSpace::DeviceRgb => false,
            PdfColorSpace::DocumentDefault => template_doc.color_mode() == ColorMode::Cmyk,
        };
        let doc_icc = template_doc.icc_pipeline();
        let use_icc_for_raster = use_cmyk
            && (template_doc.cmyk_icc_profile().is_some()
                || template_doc.custom_icc_profile().is_some());

        let mut image_xobjects = BTreeMap::new();
        for (img_idx, (&asset_id, asset)) in template_doc.assets().iter().enumerate() {
            let name = format!("Im{}", img_idx + 1);
            let xobj_id = add_raster_xobject(
                &mut store,
                asset,
                use_cmyk,
                use_icc_for_raster.then_some(&doc_icc),
            )?;
            image_xobjects.insert(asset_id, (name, xobj_id));
        }

        let dynamic_img_counter = image_xobjects.len();
        Ok(Self {
            store,
            catalog_id,
            info_id,
            pages_root_id,
            shared_fonts_dict_id,
            opts: opts.clone(),
            use_cmyk,
            use_icc_for_raster,
            doc_icc,
            custom_icc: template_doc.custom_icc_profile().map(|b| b.to_vec()),
            title: template_doc.title().to_owned(),
            author: template_doc.metadata().author.clone(),
            text_engine: TextEngine::new(),
            font_registry: PdfFontRegistry::new(),
            embedded_fonts: std::collections::HashMap::new(),
            image_xobjects,
            spot_cache: BTreeMap::new(),
            dynamic_img_counter,
            any_transparency: false,
            page_obj_ids: Vec::new(),
            deflater: flate2::Compress::new(Compression::fast(), true),
            stream_buf: String::with_capacity(8192),
            comp_buf: Vec::with_capacity(8192),
        })
    }

    /// Renders `page_index` of `page_doc` and flushes its PDF page objects straight to disk.
    pub fn push_page(&mut self, page_doc: &Document, page_index: usize) -> IoResult<()> {
        let dl_opts = DisplayListOptions::pdf();
        let dl = ExportDisplayList::build(page_doc, page_index, &dl_opts, &mut self.text_engine)?;
        for op in &dl.ops {
            if let DisplayOp::DrawTextRuns { shaped_runs, .. } = op {
                for r in shaped_runs {
                    self.font_registry
                        .register_run_identity(r, &mut self.embedded_fonts);
                }
            }
        }
        if dl.has_transparency {
            self.any_transparency = true;
        }

        let page = &page_doc.pages()[page_index];
        let bleed_mm = if self.opts.include_bleed && page.bleed_mm.is_finite() {
            page.bleed_mm.max(0.0)
        } else {
            0.0
        };
        let has_marks = self.opts.crop_marks
            || self.opts.registration_marks
            || self.opts.color_calibration_bars
            || self.opts.page_info_slug;
        let margin_mm = if has_marks { bleed_mm + 12.0 } else { bleed_mm };

        let media_w_pt = (page.width_mm + 2.0 * margin_mm) * MM_TO_PT;
        let media_h_pt = (page.height_mm + 2.0 * margin_mm) * MM_TO_PT;
        let trim_x0 = margin_mm * MM_TO_PT;
        let trim_y0 = margin_mm * MM_TO_PT;
        let trim_x1 = (margin_mm + page.width_mm) * MM_TO_PT;
        let trim_y1 = (margin_mm + page.height_mm) * MM_TO_PT;
        let bleed_x0 = (margin_mm - bleed_mm) * MM_TO_PT;
        let bleed_y0 = (margin_mm - bleed_mm) * MM_TO_PT;
        let bleed_x1 = (margin_mm + page.width_mm + bleed_mm) * MM_TO_PT;
        let bleed_y1 = (margin_mm + page.height_mm + bleed_mm) * MM_TO_PT;

        let slug_layout = if self.opts.page_info_slug {
            let mode_str = if self.use_cmyk {
                "DeviceCMYK"
            } else {
                "DeviceRGB"
            };
            let slug = format!(
                "Page {} ({}) | {:.1}x{:.1}mm | Bleed {:.1}mm | {mode_str} | coree",
                self.page_obj_ids.len() + 1,
                page.name,
                page.width_mm,
                page.height_mm,
                bleed_mm
            );
            let slug_color = if self.use_cmyk {
                Color::cmyk(0.0, 0.0, 0.0, 1.0)
            } else {
                Color::BLACK
            };
            let at = coree_core::ArtisticText::from_plain(
                slug,
                coree_core::TextStyle {
                    font_size_pt: 6.0,
                    fill: slug_color,
                    ..coree_core::TextStyle::default()
                },
            );
            let (s_runs, s_curves) = self.text_engine.layout_artistic_pdf_runs(&at);
            for r in &s_runs {
                self.font_registry
                    .register_run_identity(r, &mut self.embedded_fonts);
            }
            Some((s_runs, s_curves))
        } else {
            None
        };

        let page_is_pdfx1a = self.opts.pdfx_conformance == PdfXConformance::PdfX1a2001
            || (self.opts.pdfx_conformance == PdfXConformance::Auto
                && self.use_cmyk
                && !dl.has_transparency);
        let mut emit_state = PdfPageEmit {
            store: &mut self.store,
            embedded_fonts: &self.embedded_fonts,
            font_ptr_cache: self.font_registry.ptr_cache(),
            used_fonts: BTreeMap::new(),
            gstates: BTreeMap::new(),
            used_images: BTreeMap::new(),
            shadings: BTreeMap::new(),
            used_spots: BTreeMap::new(),
            spot_cache: &mut self.spot_cache,
            dynamic_img_counter: &mut self.dynamic_img_counter,
            use_icc_for_raster: self.use_icc_for_raster,
            doc_icc: &self.doc_icc,
            is_pdfx1a: page_is_pdfx1a,
        };
        let mut stream = std::mem::take(&mut self.stream_buf);
        stream.clear();
        let ctx = PageRenderCtx {
            page,
            margin_mm,
            use_cmyk: self.use_cmyk,
            image_xobjects: &self.image_xobjects,
        };

        let _ = writeln!(
            stream,
            "q\n{bleed_x0:.4} {bleed_y0:.4} {:.4} {:.4} re W n",
            bleed_x1 - bleed_x0,
            bleed_y1 - bleed_y0
        );
        for op in &dl.ops {
            emit_display_op(&ctx, op, &mut emit_state, &mut stream);
        }
        stream.push_str("Q\n");

        if has_marks {
            emit_print_marks(
                &ctx,
                &mut stream,
                page,
                margin_mm,
                bleed_mm,
                self.use_cmyk,
                &self.opts,
                slug_layout.as_ref(),
                &mut emit_state,
            );
        }

        let gstates = emit_state.gstates;
        let used_images = emit_state.used_images;
        let page_shadings = emit_state.shadings;
        let page_spots = emit_state.used_spots;
        let has_fonts = !emit_state.used_fonts.is_empty();

        let gstate_refs = build_extgstate_objects(&mut self.store, &gstates, page_is_pdfx1a);

        let mut xobj_refs = String::new();
        for (im_name, im_id) in &used_images {
            let _ = write!(xobj_refs, " /{im_name} {im_id} 0 R");
        }
        let mut shading_refs = String::new();
        for (sh_name, sh_id) in &page_shadings {
            let _ = write!(shading_refs, " /{sh_name} {sh_id} 0 R");
        }
        let mut colorspace_refs = String::new();
        for (cs_name, cs_id) in &page_spots {
            let _ = write!(colorspace_refs, " /{cs_name} {cs_id} 0 R");
        }

        let content_obj_bytes = if self.opts.compress_streams {
            compress_zlib_reusable(&mut self.deflater, stream.as_bytes(), &mut self.comp_buf)
                .map_err(|e| IoError::InvalidFormat {
                    format: "pdf".into(),
                    message: format!("zlib compression failed: {e}"),
                })?;
            let mut buf = Vec::with_capacity(self.comp_buf.len() + 64);
            let _ = write!(
                buf,
                "<< /Length {} /Filter /FlateDecode >>\nstream\n",
                self.comp_buf.len()
            );
            buf.extend_from_slice(&self.comp_buf);
            buf.extend_from_slice(b"\nendstream");
            buf
        } else {
            let raw = stream.as_bytes();
            let mut buf = Vec::with_capacity(raw.len() + 64);
            let _ = write!(buf, "<< /Length {} >>\nstream\n", raw.len());
            buf.extend_from_slice(raw);
            buf.extend_from_slice(b"\nendstream");
            buf
        };
        self.stream_buf = stream;
        let content_id = self.store.add(content_obj_bytes);

        let mut resources = String::from("<<");
        if has_fonts {
            let _ = write!(resources, " /Font {} 0 R", self.shared_fonts_dict_id);
        }
        if !gstate_refs.is_empty() {
            let _ = write!(resources, " /ExtGState <<{gstate_refs} >>");
        }
        if !xobj_refs.is_empty() {
            let _ = write!(resources, " /XObject <<{xobj_refs} >>");
        }
        if !shading_refs.is_empty() {
            let _ = write!(resources, " /Shading <<{shading_refs} >>");
        }
        if !colorspace_refs.is_empty() {
            let _ = write!(resources, " /ColorSpace <<{colorspace_refs} >>");
        }
        resources.push_str(" >>");

        let group_entry = if dl.has_transparency {
            let grp_cs = if self.use_cmyk {
                "/DeviceCMYK"
            } else {
                "/DeviceRGB"
            };
            format!(" /Group << /S /Transparency /CS {grp_cs} /I true >>")
        } else {
            String::new()
        };

        let page_dict = format!(
            "<< /Type /Page /Parent {} 0 R \
             /MediaBox [0 0 {media_w_pt:.4} {media_h_pt:.4}] \
             /TrimBox [{trim_x0:.4} {trim_y0:.4} {trim_x1:.4} {trim_y1:.4}] \
             /BleedBox [{bleed_x0:.4} {bleed_y0:.4} {bleed_x1:.4} {bleed_y1:.4}]{group_entry} \
             /Resources {resources} \
             /Contents {content_id} 0 R >>",
            self.pages_root_id
        );
        let page_obj_id = self.store.add(page_dict.into_bytes());
        self.page_obj_ids.push(page_obj_id);
        Ok(())
    }

    /// Finalizes shared `/Type0` fonts, `/Pages`, `/Catalog`, `/Info`, and `xref` trailer to disk.
    pub fn finish(mut self) -> IoResult<()> {
        let font_entries = self
            .font_registry
            .emit_all_identity(&mut |bytes| self.store.add(bytes))?;
        let mut shared_font_dict = String::from("<<");
        for (res_name, obj_id) in font_entries {
            let _ = write!(shared_font_dict, " /{res_name} {obj_id} 0 R");
        }
        shared_font_dict.push_str(" >>");
        self.store
            .set(self.shared_fonts_dict_id, shared_font_dict.into_bytes());

        let kids: Vec<String> = self
            .page_obj_ids
            .iter()
            .map(|id| format!("{id} 0 R"))
            .collect();
        let pages_dict = format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            self.page_obj_ids.len()
        );
        self.store.set(self.pages_root_id, pages_dict.into_bytes());

        let raw_icc = if let Some(bytes) = self.custom_icc.as_deref() {
            bytes.to_vec()
        } else if self.use_cmyk {
            generate_minimal_cmyk_icc_v2(
                "ISO Coated v2 CMYK (coree Clean-Room)",
                BuiltinIccProfile::IsoCoatedV2Cmyk,
            )
        } else {
            generate_minimal_srgb_icc_v2("sRGB IEC61966-2.1 (coree Clean-Room)")
        };
        let parsed_icc = IccProfile::from_bytes(&raw_icc).ok();
        let icc_n = parsed_icc
            .as_ref()
            .map_or(if self.use_cmyk { 4 } else { 3 }, |p| {
                p.header.color_space.channel_count().clamp(1, 4)
            });
        let icc_info = parsed_icc
            .as_ref()
            .map(|p| p.description.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if self.use_cmyk {
                    "ISO Coated v2 CMYK".to_owned()
                } else {
                    "sRGB IEC61966-2.1".to_owned()
                }
            });
        let icc_cond = if icc_n == 4 { "FOGRA39" } else { "sRGB" };

        let mut enc_icc = ZlibEncoder::new(Vec::new(), Compression::fast());
        let icc_obj_bytes = if enc_icc.write_all(&raw_icc).is_ok() {
            let compressed_icc = enc_icc.finish().unwrap_or_default();
            let mut buf = format!(
                "<< /N {icc_n} /Length {} /Filter /FlateDecode >>\nstream\n",
                compressed_icc.len()
            )
            .into_bytes();
            buf.extend_from_slice(&compressed_icc);
            buf.extend_from_slice(b"\nendstream");
            buf
        } else {
            let mut buf =
                format!("<< /N {icc_n} /Length {} >>\nstream\n", raw_icc.len()).into_bytes();
            buf.extend_from_slice(&raw_icc);
            buf.extend_from_slice(b"\nendstream");
            buf
        };
        let icc_stream_id = self.store.add(icc_obj_bytes);
        let oi_dict = format!(
            "<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier ({cond}) /Info ({info}) /RegistryName (http://www.color.org) /DestOutputProfile {icc_stream_id} 0 R >>",
            cond = escape_pdf_string(icc_cond),
            info = escape_pdf_string(&icc_info),
        );
        let output_intent_id = self.store.add(oi_dict.into_bytes());

        let catalog_dict = format!(
            "<< /Type /Catalog /Pages {} 0 R /OutputIntents [{output_intent_id} 0 R] >>",
            self.pages_root_id
        );
        self.store.set(self.catalog_id, catalog_dict.into_bytes());

        let resolved_pdfx = match self.opts.pdfx_conformance {
            PdfXConformance::Auto => {
                if self.use_cmyk && !self.any_transparency {
                    PdfXConformance::PdfX1a2001
                } else {
                    PdfXConformance::PdfX4
                }
            }
            other => other,
        };
        let now_pdf = chrono::Utc::now().format("D:%Y%m%d%H%M%SZ").to_string();
        let pdfx_meta = match resolved_pdfx {
            PdfXConformance::PdfX1a2001 => {
                " /GTS_PDFXVersion (PDF/X-1a:2001) /GTS_PDFXConformance (PDF/X-1a:2001) /Trapped /False"
            }
            PdfXConformance::PdfX4 => {
                " /GTS_PDFXVersion (PDF/X-4) /GTS_PDFXConformance (PDF/X-4) /Trapped /False"
            }
            PdfXConformance::Auto | PdfXConformance::None => "",
        };
        let info_dict = format!(
            "<< /Title ({}) /Author ({}) /Creator (coree) /Producer (coree-io Streaming PDF/X Writer) /CreationDate ({now_pdf}) /ModDate ({now_pdf}){pdfx_meta} >>",
            escape_pdf_string(&self.title),
            escape_pdf_string(&self.author)
        );
        self.store.set(self.info_id, info_dict.into_bytes());

        self.store
            .finish_streaming(self.catalog_id, self.info_id, &self.title)
    }
}

struct PageRenderCtx<'a> {
    page: &'a Page,
    margin_mm: f64,
    use_cmyk: bool,
    image_xobjects: &'a BTreeMap<coree_core::ImageAssetId, (String, usize)>,
}

impl PageRenderCtx<'_> {
    fn to_pdf_pt(&self, pt_mm: Point2D) -> (f64, f64) {
        let x = (self.margin_mm + pt_mm.x) * MM_TO_PT;
        let y = (self.margin_mm + (self.page.height_mm - pt_mm.y)) * MM_TO_PT;
        (x, y)
    }
}

struct PdfPageEmit<'a> {
    store: &'a mut PdfObjectStore,
    embedded_fonts: &'a std::collections::HashMap<FontFaceKey, EmbeddedPdfFont>,
    font_ptr_cache: &'a std::collections::HashMap<(usize, usize, u32), FontFaceKey>,
    used_fonts: BTreeMap<String, usize>,
    gstates: BTreeMap<GStateKey, String>,
    used_images: BTreeMap<String, usize>,
    shadings: BTreeMap<String, usize>,
    used_spots: BTreeMap<String, usize>,
    spot_cache: &'a mut BTreeMap<String, (String, usize)>,
    dynamic_img_counter: &'a mut usize,
    use_icc_for_raster: bool,
    doc_icc: &'a coree_core::IccColorPipeline,
    is_pdfx1a: bool,
}

fn emit_display_op(
    ctx: &PageRenderCtx<'_>,
    op: &DisplayOp,
    emit: &mut PdfPageEmit<'_>,
    out: &mut String,
) {
    match op {
        DisplayOp::PushLayer { .. }
        | DisplayOp::PopLayer
        | DisplayOp::PushGroup { .. }
        | DisplayOp::PopGroup
        | DisplayOp::PushTransparencyGroup { .. }
        | DisplayOp::PopTransparencyGroup => {}
        DisplayOp::PushClip {
            path, fill_rule, ..
        } => {
            out.push_str("q\n");
            emit_pdf_path_ops(ctx, path, out);
            match fill_rule {
                coree_render::FillRule::EvenOdd => out.push_str("W* n\n"),
                coree_render::FillRule::Winding => out.push_str("W n\n"),
            }
        }
        DisplayOp::PopClip => {
            out.push_str("Q\n");
        }
        DisplayOp::DrawVector {
            path,
            fill,
            outline,
            fill_xf,
            opacity,
            blend_mode,
            emit_arrowheads,
            ..
        } => {
            emit_pdf_styled_path_opts(
                ctx,
                path,
                fill,
                outline,
                *blend_mode,
                *opacity,
                *fill_xf,
                *emit_arrowheads,
                emit,
                out,
            );
        }
        DisplayOp::DrawRaster {
            asset_id,
            inline_rgba,
            bl,
            br,
            tl,
            opacity,
            blend_mode,
            ..
        } => {
            let im_name_opt = if let Some(aid) = asset_id {
                ctx.image_xobjects.get(aid).map(|(name, id)| {
                    emit.used_images.insert(name.clone(), *id);
                    name.clone()
                })
            } else if let Some(ref arc_rgba) = inline_rgba {
                let (w, h, ref bytes) = **arc_rgba;
                let icc_opt = emit.use_icc_for_raster.then_some(emit.doc_icc);
                add_rgba_bytes_xobject(emit.store, w, h, bytes, ctx.use_cmyk, icc_opt)
                    .ok()
                    .map(|im_id| {
                        *emit.dynamic_img_counter += 1;
                        let name = format!("DynIm{}", *emit.dynamic_img_counter);
                        emit.used_images.insert(name.clone(), im_id);
                        name
                    })
            } else {
                None
            };
            if let Some(im_name) = im_name_opt {
                emit_pdf_image_quad(
                    ctx,
                    &im_name,
                    *bl,
                    *br,
                    *tl,
                    *opacity,
                    *blend_mode,
                    emit.is_pdfx1a,
                    &mut emit.gstates,
                    out,
                );
            }
        }
        DisplayOp::DrawTextRuns {
            shaped_runs,
            fallback_curves,
            outline,
            world_xf,
            opacity,
            blend_mode,
            is_perspective,
            ..
        } => {
            for run in shaped_runs {
                if !emit_pdf_embedded_run(
                    ctx,
                    run,
                    outline,
                    *world_xf,
                    *opacity,
                    *blend_mode,
                    emit,
                    out,
                ) {
                    // Unembeddable font fallback (rare): outline runs would already be in curves
                }
            }
            if !fallback_curves.is_empty() {
                let curve_xf = if *is_perspective {
                    Affine2D::IDENTITY
                } else {
                    *world_xf
                };
                emit_pdf_shaped_pieces(
                    ctx,
                    fallback_curves,
                    outline,
                    curve_xf,
                    *opacity,
                    *blend_mode,
                    emit,
                    out,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_pdf_image_quad(
    ctx: &PageRenderCtx<'_>,
    im_name: &str,
    bl: Point2D,
    br: Point2D,
    tl: Point2D,
    opacity: f32,
    blend_mode: BlendMode,
    is_pdfx1a: bool,
    gstates: &mut BTreeMap<GStateKey, String>,
    out: &mut String,
) {
    let (x0, y0) = ctx.to_pdf_pt(bl);
    let (x1, y1) = ctx.to_pdf_pt(br);
    let (x2, y2) = ctx.to_pdf_pt(tl);
    if !is_pdfx1a && ((opacity - 1.0).abs() > 1e-3 || blend_mode != BlendMode::Normal) {
        let gs = intern_gstate(gstates, opacity, opacity, blend_mode, false);
        let _ = writeln!(
            out,
            "q /{gs} gs {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} cm /{im_name} Do Q",
            x1 - x0,
            y1 - y0,
            x2 - x0,
            y2 - y0,
            x0,
            y0
        );
    } else {
        let _ = writeln!(
            out,
            "q {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} cm /{im_name} Do Q",
            x1 - x0,
            y1 - y0,
            x2 - x0,
            y2 - y0,
            x0,
            y0
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_pdf_embedded_run(
    ctx: &PageRenderCtx<'_>,
    run: &ShapedPdfRun,
    outline: &OutlineStyle,
    world_xf: Affine2D,
    opacity: f32,
    blend_mode: BlendMode,
    emit: &mut PdfPageEmit<'_>,
    out: &mut String,
) -> bool {
    if run.glyphs.is_empty() || !world_xf.is_finite() {
        return true;
    }
    let ptr_key = (
        std::sync::Arc::as_ptr(&run.font_bytes).cast::<u8>() as usize,
        run.font_bytes.len(),
        run.face_index,
    );
    let key = emit
        .font_ptr_cache
        .get(&ptr_key)
        .copied()
        .unwrap_or_else(|| FontFaceKey::new(&run.font_bytes, run.face_index));
    let Some(emb) = emit.embedded_fonts.get(&key) else {
        return false;
    };
    emit.used_fonts
        .insert(emb.res_name.clone(), emb.type0_obj_id);

    let raw_fill_alpha = (run.fill.alpha() * opacity).clamp(0.0, 1.0);
    let raw_stroke_alpha = (outline.color.alpha() * opacity).clamp(0.0, 1.0);
    let has_fill = raw_fill_alpha > 1e-6;
    let has_stroke = outline.is_visible() && raw_stroke_alpha > 1e-6;
    if !has_fill && !has_stroke {
        return true;
    }
    let fill_alpha = if has_fill { raw_fill_alpha } else { 1.0 };
    let stroke_alpha = if has_stroke { raw_stroke_alpha } else { 1.0 };

    let font_pt = if run.font_size_pt.is_finite() {
        run.font_size_pt.max(0.5)
    } else {
        12.0
    };

    out.push_str("q\n");
    let need_gstate = if emit.is_pdfx1a {
        outline.overprint
    } else {
        (fill_alpha - 1.0).abs() > 1e-3
            || (stroke_alpha - 1.0).abs() > 1e-3
            || blend_mode != BlendMode::Normal
            || outline.overprint
    };
    if need_gstate {
        let gs = if emit.is_pdfx1a {
            intern_gstate(&mut emit.gstates, 1.0, 1.0, BlendMode::Normal, true)
        } else {
            intern_gstate(
                &mut emit.gstates,
                fill_alpha,
                stroke_alpha,
                blend_mode,
                outline.overprint,
            )
        };
        let _ = writeln!(out, "/{gs} gs");
    }
    if has_fill {
        emit_pdf_color(out, &run.fill, ctx.use_cmyk, false, emit);
    }
    if has_stroke {
        emit_pdf_color(out, &outline.color, ctx.use_cmyk, true, emit);
        emit_pdf_stroke_state(out, outline, world_xf);
    }

    // Normalize RTL lines emitted in right-to-left order (`x` decreasing) into visual
    // left-to-right order (`x` increasing) so PDF `TJ` advances monotonically left-to-right
    // and Poppler `/ActualText` + Bidi reconstruction preserves exact ligatures & word bounds.
    let mut ordered_glyphs: Vec<coree_render::ShapedPdfGlyph> =
        Vec::with_capacity(run.glyphs.len());
    let mut line_start = 0usize;
    while line_start < run.glyphs.len() {
        let line_y = run.glyphs[line_start].y_mm;
        let mut line_end = line_start + 1;
        while line_end < run.glyphs.len() && (run.glyphs[line_end].y_mm - line_y).abs() < 0.05 {
            line_end += 1;
        }
        let line_slice = &run.glyphs[line_start..line_end];
        let is_decreasing_x = line_slice.len() >= 2
            && line_slice.first().map(|g| g.x_mm).unwrap_or(0.0)
                > line_slice.last().map(|g| g.x_mm).unwrap_or(0.0) + 0.05;
        if is_decreasing_x {
            // Group each base glyph with any immediately following zero-advance combining marks,
            // then emit clusters in reverse (left-to-right) order.
            let mut clusters: Vec<(usize, usize)> = Vec::new();
            let mut ci = 0usize;
            while ci < line_slice.len() {
                let c_start = ci;
                ci += 1;
                while ci < line_slice.len() && line_slice[ci].advance_mm <= 1e-4 {
                    ci += 1;
                }
                clusters.push((c_start, ci));
            }
            for &(cs, ce) in clusters.iter().rev() {
                ordered_glyphs.extend(line_slice[cs..ce].iter().cloned());
            }
        } else {
            ordered_glyphs.extend(line_slice.iter().cloned());
        }
        line_start = line_end;
    }

    let strip_injected_kashida = run
        .actual_text
        .as_deref()
        .is_some_and(|a| !a.contains('\u{0640}'));

    let g0 = &ordered_glyphs[0];
    let p0_world = world_xf.transform_point(Point2D::new(g0.x_mm, g0.y_mm));
    let (px0, py0) = ctx.to_pdf_pt(p0_world);
    let [a, b, c, d, _, _] = world_xf.m;

    let _ = writeln!(
        out,
        "BT /{} {font_pt:.4} Tf {a:.6} {:.6} {:.6} {d:.6} {px0:.4} {py0:.4} Tm",
        emb.res_name, -b, -c
    );
    match (has_fill, has_stroke) {
        (true, true) => out.push_str("2 Tr\n"),
        (false, true) => out.push_str("1 Tr\n"),
        _ => {}
    }

    // When Kashidas were injected (`strip_injected_kashida`), pair each run of injected
    // `\u{0640}` glyphs with the adjacent non-kashida glyph inside a single-char
    // `/Span << /ActualText <FEFF...> >> BDC ... EMC` so the physical kashida span
    // bridges the bounding box seamlessly without introducing spaces or reversing RTL order.
    let mut kashida_partner: Vec<Option<usize>> = vec![None; ordered_glyphs.len()];
    if strip_injected_kashida {
        for (idx, g) in ordered_glyphs.iter().enumerate() {
            if g.unicode == "\u{0640}" {
                let next_host = (idx + 1..ordered_glyphs.len()).find(|&j| {
                    let ug = &ordered_glyphs[j];
                    ug.unicode != "\u{0640}" && !ug.unicode.is_empty()
                });
                let host = next_host.or_else(|| {
                    (0..idx).rev().find(|&j| {
                        let ug = &ordered_glyphs[j];
                        ug.unicode != "\u{0640}" && !ug.unicode.is_empty()
                    })
                });
                if let Some(h) = host {
                    kashida_partner[idx] = Some(h);
                    kashida_partner[h] = Some(h);
                }
            }
        }
    }

    let mut line_start_x_pt = 0.0_f64;
    let mut line_start_y_pt = 0.0_f64;
    let mut cur_x_pt = 0.0_f64;
    let mut cur_y_pt = 0.0_f64;
    let mut tj_buf = String::new();
    let mut active_span_host: Option<usize> = None;

    for (idx, g) in ordered_glyphs.iter().enumerate() {
        let tx_pt = (g.x_mm - g0.x_mm) * MM_TO_PT;
        let ty_pt = -(g.y_mm - g0.y_mm) * MM_TO_PT;
        let target_host = kashida_partner[idx];

        if target_host != active_span_host {
            if !tj_buf.is_empty() {
                let _ = writeln!(out, "[{tj_buf}] TJ");
                tj_buf.clear();
            }
            if active_span_host.is_some() {
                out.push_str("EMC\n");
            }
            if let Some(h) = target_host {
                let actual = &ordered_glyphs[h].unicode;
                let mut hex = String::with_capacity(4 + actual.len() * 4);
                hex.push_str("FEFF");
                for unit in actual.encode_utf16() {
                    let _ = write!(hex, "{unit:04X}");
                }
                let _ = writeln!(out, "/Span << /ActualText <{hex}> >> BDC");
            }
            active_span_host = target_host;
        }

        if (ty_pt - cur_y_pt).abs() >= 0.02 {
            if !tj_buf.is_empty() {
                let _ = writeln!(out, "[{tj_buf}] TJ");
                tj_buf.clear();
            }
            let dx = tx_pt - line_start_x_pt;
            let dy = ty_pt - line_start_y_pt;
            let _ = writeln!(out, "{dx:.4} {dy:.4} Td");
            line_start_x_pt = tx_pt;
            line_start_y_pt = ty_pt;
            cur_x_pt = tx_pt;
            cur_y_pt = ty_pt;
        }
        let adj = (cur_x_pt - tx_pt) * 1000.0 / font_pt;
        if adj.abs() >= 0.5 {
            let _ = write!(tj_buf, "{adj:.1} ");
        }
        let sub_gid = emb.map_gid(g.glyph_id);
        let _ = write!(tj_buf, "<{sub_gid:04X}>");
        let w_1000 = emb.width_1000(g.glyph_id);
        cur_x_pt = tx_pt + (f64::from(w_1000) * font_pt / 1000.0);
    }
    if !tj_buf.is_empty() {
        let _ = writeln!(out, "[{tj_buf}] TJ");
    }
    if active_span_host.is_some() {
        out.push_str("EMC\n");
    }
    out.push_str("ET\n");
    out.push_str("Q\n");
    true
}

#[allow(clippy::too_many_arguments)]
fn emit_pdf_styled_path(
    ctx: &PageRenderCtx<'_>,
    path: &BezierPath,
    fill: &Fill,
    outline: &OutlineStyle,
    blend_mode: BlendMode,
    opacity: f32,
    fill_xf: Affine2D,
    emit: &mut PdfPageEmit<'_>,
    out: &mut String,
) {
    emit_pdf_styled_path_opts(
        ctx, path, fill, outline, blend_mode, opacity, fill_xf, true, emit, out,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_pdf_styled_path_opts(
    ctx: &PageRenderCtx<'_>,
    path: &BezierPath,
    fill: &Fill,
    outline: &OutlineStyle,
    blend_mode: BlendMode,
    opacity: f32,
    fill_xf: Affine2D,
    emit_arrowheads: bool,
    emit: &mut PdfPageEmit<'_>,
    out: &mut String,
) {
    if path.is_empty() {
        return;
    }
    let shading_name = register_pdf_gradient_shading(ctx, fill, fill_xf, emit);
    let fill_color = if shading_name.is_none() {
        fill_representative_color(fill).filter(|c| c.alpha() > 1e-6)
    } else {
        None
    };
    let do_fill = shading_name.is_some() || fill_color.is_some();
    let do_stroke = outline.is_visible();
    if !do_fill && !do_stroke {
        return;
    }

    let fill_alpha = if !do_fill {
        1.0
    } else if shading_name.is_some() {
        fill.referenced_colors()
            .first()
            .map_or(opacity, |c| (c.alpha() * opacity).clamp(0.0, 1.0))
    } else {
        fill_color
            .as_ref()
            .map_or(1.0, |c| (c.alpha() * opacity).clamp(0.0, 1.0))
    };
    let stroke_alpha = if do_stroke {
        (outline.color.alpha() * opacity).clamp(0.0, 1.0)
    } else {
        1.0
    };

    out.push_str("q\n");
    let need_gstate = if emit.is_pdfx1a {
        outline.overprint
    } else {
        (fill_alpha - 1.0).abs() > 1e-3
            || (stroke_alpha - 1.0).abs() > 1e-3
            || blend_mode != BlendMode::Normal
            || outline.overprint
    };
    if need_gstate {
        let gs = if emit.is_pdfx1a {
            intern_gstate(&mut emit.gstates, 1.0, 1.0, BlendMode::Normal, true)
        } else {
            intern_gstate(
                &mut emit.gstates,
                fill_alpha,
                stroke_alpha,
                blend_mode,
                outline.overprint,
            )
        };
        let _ = writeln!(out, "/{gs} gs");
    }

    if let Some(ref sh_name) = shading_name {
        if do_stroke && outline.behind_fill {
            emit_pdf_color(out, &outline.color, ctx.use_cmyk, true, emit);
            emit_pdf_stroke_state(out, outline, fill_xf);
            emit_pdf_path_ops(ctx, path, out);
            out.push_str("S\n");
            if emit_arrowheads {
                emit_pdf_arrowheads(ctx, path, outline, emit, out);
            }
        }
        out.push_str("q\n");
        emit_pdf_path_ops(ctx, path, out);
        out.push_str("W n\n");
        let _ = writeln!(out, "/{sh_name} sh");
        out.push_str("Q\n");
        if do_stroke && !outline.behind_fill {
            emit_pdf_color(out, &outline.color, ctx.use_cmyk, true, emit);
            emit_pdf_stroke_state(out, outline, fill_xf);
            emit_pdf_path_ops(ctx, path, out);
            out.push_str("S\n");
            if emit_arrowheads {
                emit_pdf_arrowheads(ctx, path, outline, emit, out);
            }
        }
    } else {
        if let Some(ref fc) = fill_color {
            emit_pdf_color(out, fc, ctx.use_cmyk, false, emit);
        }
        if do_stroke {
            emit_pdf_color(out, &outline.color, ctx.use_cmyk, true, emit);
            emit_pdf_stroke_state(out, outline, fill_xf);
        }

        if do_fill && do_stroke && outline.behind_fill {
            emit_pdf_path_ops(ctx, path, out);
            out.push_str("S\n");
            emit_pdf_path_ops(ctx, path, out);
            out.push_str("f*\n");
        } else {
            emit_pdf_path_ops(ctx, path, out);
            match (do_fill, do_stroke) {
                (true, true) => out.push_str("B*\n"),
                (true, false) => out.push_str("f*\n"),
                (false, true) => out.push_str("S\n"),
                (false, false) => {}
            }
        }
        if do_stroke && emit_arrowheads {
            emit_pdf_arrowheads(ctx, path, outline, emit, out);
        }
    }
    out.push_str("Q\n");
}

fn emit_pdf_arrowheads(
    ctx: &PageRenderCtx<'_>,
    path: &BezierPath,
    outline: &OutlineStyle,
    emit: &mut PdfPageEmit<'_>,
    out: &mut String,
) {
    let heads = outline_arrowhead_geometries(path, outline);
    if heads.is_empty() {
        return;
    }
    emit_pdf_color(out, &outline.color, ctx.use_cmyk, false, emit);
    emit_pdf_color(out, &outline.color, ctx.use_cmyk, true, emit);
    for (geom, filled) in heads {
        emit_pdf_path_ops(ctx, &geom, out);
        out.push_str(if filled { "f\n" } else { "S\n" });
    }
}

fn register_pdf_gradient_shading(
    ctx: &PageRenderCtx<'_>,
    fill: &Fill,
    fill_xf: Affine2D,
    emit: &mut PdfPageEmit<'_>,
) -> Option<String> {
    match fill {
        Fill::LinearGradient { start, end, stops } if !stops.is_empty() => {
            let fn_id = build_pdf_gradient_function(emit.store, stops, ctx.use_cmyk)?;
            let p0 = fill_xf.transform_point(*start);
            let p1 = fill_xf.transform_point(*end);
            let (x0, y0) = ctx.to_pdf_pt(p0);
            let (mut x1, mut y1) = ctx.to_pdf_pt(p1);
            if (x1 - x0).hypot(y1 - y0) < 1e-3 {
                x1 = x0 + 1.0;
                y1 = y0;
            }
            let cs = if ctx.use_cmyk {
                "/DeviceCMYK"
            } else {
                "/DeviceRGB"
            };
            let sh_body = format!(
                "<< /ShadingType 2 /ColorSpace {cs} /Coords [{x0:.4} {y0:.4} {x1:.4} {y1:.4}] \
                 /Function {fn_id} 0 R /Extend [true true] >>"
            );
            let sh_id = emit.store.add(sh_body.into_bytes());
            let sh_name = format!("Sh{}", emit.shadings.len() + 1);
            emit.shadings.insert(sh_name.clone(), sh_id);
            Some(sh_name)
        }
        Fill::RadialGradient {
            center,
            radius,
            focal,
            stops,
        } if !stops.is_empty() => {
            let fn_id = build_pdf_gradient_function(emit.store, stops, ctx.use_cmyk)?;
            let c_world = fill_xf.transform_point(*center);
            let f_world = fill_xf.transform_point(*focal);
            let edge_world =
                fill_xf.transform_point(Point2D::new(center.x + radius.max(0.1), center.y));
            let (cx, cy) = ctx.to_pdf_pt(c_world);
            let (fx, fy) = ctx.to_pdf_pt(f_world);
            let (ex, ey) = ctx.to_pdf_pt(edge_world);
            let r_pt = (ex - cx).hypot(ey - cy).max(0.5);
            let cs = if ctx.use_cmyk {
                "/DeviceCMYK"
            } else {
                "/DeviceRGB"
            };
            let sh_body = format!(
                "<< /ShadingType 3 /ColorSpace {cs} /Coords [{fx:.4} {fy:.4} 0.0000 {cx:.4} {cy:.4} {r_pt:.4}] \
                 /Function {fn_id} 0 R /Extend [true true] >>"
            );
            let sh_id = emit.store.add(sh_body.into_bytes());
            let sh_name = format!("Sh{}", emit.shadings.len() + 1);
            emit.shadings.insert(sh_name.clone(), sh_id);
            Some(sh_name)
        }
        _ => None,
    }
}

fn color_components_pdf(c: &Color, use_cmyk: bool) -> String {
    if use_cmyk {
        let [cc, m, y, k] = c.to_cmyk().map(sanitize_unit_f32);
        format!("{cc:.4} {m:.4} {y:.4} {k:.4}")
    } else {
        let [r, g, b, _] = c.to_rgba_f32().map(sanitize_unit_f32);
        format!("{r:.4} {g:.4} {b:.4}")
    }
}

fn build_pdf_gradient_function(
    store: &mut PdfObjectStore,
    stops: &[GradientStop],
    use_cmyk: bool,
) -> Option<usize> {
    if stops.is_empty() {
        return None;
    }
    let mut sorted = stops.to_vec();
    sorted.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    if sorted.len() == 1 {
        let c = color_components_pdf(&sorted[0].color, use_cmyk);
        let body = format!("<< /FunctionType 2 /Domain [0 1] /C0 [{c}] /C1 [{c}] /N 1 >>");
        return Some(store.add(body.into_bytes()));
    }
    if sorted.len() == 2 {
        let c0 = color_components_pdf(&sorted[0].color, use_cmyk);
        let c1 = color_components_pdf(&sorted[1].color, use_cmyk);
        let body = format!("<< /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >>");
        return Some(store.add(body.into_bytes()));
    }

    // Multi-stop PDF Type 3 Stitching Function across [0, 1]
    let mut fn_refs = Vec::new();
    let mut bounds = Vec::new();
    let mut encode = Vec::new();

    // Ensure leading [0..first.offset] and trailing [last.offset..1] coverage cleanly
    let mut segments: Vec<(f32, f32, &Color, &Color)> = Vec::new();
    if sorted[0].offset > 1e-4 {
        segments.push((0.0, sorted[0].offset, &sorted[0].color, &sorted[0].color));
    }
    for win in sorted.windows(2) {
        if win[1].offset > win[0].offset + 1e-5 {
            segments.push((win[0].offset, win[1].offset, &win[0].color, &win[1].color));
        }
    }
    let last = sorted.last()?;
    if last.offset < 1.0 - 1e-4 {
        segments.push((last.offset, 1.0, &last.color, &last.color));
    }
    if segments.is_empty() {
        let c0 = color_components_pdf(&sorted[0].color, use_cmyk);
        let c1 = color_components_pdf(&last.color, use_cmyk);
        let body = format!("<< /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >>");
        return Some(store.add(body.into_bytes()));
    }

    for (i, &(_t0, t1, c0_col, c1_col)) in segments.iter().enumerate() {
        let c0 = color_components_pdf(c0_col, use_cmyk);
        let c1 = color_components_pdf(c1_col, use_cmyk);
        let sub_fn = store.add(
            format!("<< /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >>").into_bytes(),
        );
        fn_refs.push(format!("{sub_fn} 0 R"));
        if i + 1 < segments.len() {
            bounds.push(format!("{t1:.4}"));
        }
        encode.push("0 1".to_string());
    }
    let stitch = format!(
        "<< /FunctionType 3 /Domain [0 1] /Functions [{}] /Bounds [{}] /Encode [{}] >>",
        fn_refs.join(" "),
        bounds.join(" "),
        encode.join(" ")
    );
    Some(store.add(stitch.into_bytes()))
}

#[allow(clippy::too_many_arguments)]
fn emit_pdf_shaped_pieces(
    ctx: &PageRenderCtx<'_>,
    pieces: &[coree_render::ShapedGlyphDraw],
    outline: &OutlineStyle,
    world_xf: Affine2D,
    opacity: f32,
    blend_mode: BlendMode,
    emit: &mut PdfPageEmit<'_>,
    out: &mut String,
) {
    for piece in pieces {
        if piece.path.is_empty() {
            continue;
        }
        let world_path = piece.path.transformed(&world_xf);
        let fill = if piece.fill.alpha() > 1e-6 {
            Fill::Solid(piece.fill.clone())
        } else {
            Fill::None
        };
        emit_pdf_styled_path(
            ctx,
            &world_path,
            &fill,
            outline,
            blend_mode,
            opacity,
            world_xf,
            emit,
            out,
        );
    }
}

fn compress_zlib_reusable(
    deflater: &mut flate2::Compress,
    input: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), flate2::CompressError> {
    deflater.reset();
    out.clear();
    out.reserve(input.len() / 2 + 64);
    let mut in_off = 0usize;
    loop {
        let before_in = deflater.total_in() as usize;
        let flush = if in_off >= input.len() {
            flate2::FlushCompress::Finish
        } else {
            flate2::FlushCompress::None
        };
        let prev_len = out.len();
        if out.capacity() == prev_len {
            out.reserve(4096);
        }
        let status = deflater.compress_vec(&input[in_off..], out, flush)?;
        let consumed = (deflater.total_in() as usize).saturating_sub(before_in);
        in_off += consumed;
        if status == flate2::Status::StreamEnd {
            break;
        }
    }
    Ok(())
}

#[inline]
fn push_pdf_f4(out: &mut String, val: f64) {
    if !val.is_finite() {
        out.push_str("0.0000");
        return;
    }
    let scaled = (val * 10_000.0).round();
    if scaled.abs() > 9.0e14 {
        let _ = write!(out, "{val:.4}");
        return;
    }
    let iv = scaled as i64;
    if iv == 0 {
        out.push_str("0.0000");
        return;
    }
    let abs_v = iv.unsigned_abs();
    if iv < 0 {
        out.push('-');
    }
    let whole = abs_v / 10_000;
    let frac = (abs_v % 10_000) as u32;
    let mut digits = [b'0'; 24];
    let mut i = digits.len();
    let mut n = whole;
    if n == 0 {
        i -= 1;
        digits[i] = b'0';
    } else {
        while n > 0 {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
        }
    }
    for &b in &digits[i..] {
        out.push(b as char);
    }
    out.push('.');
    out.push((b'0' + ((frac / 1000) % 10) as u8) as char);
    out.push((b'0' + ((frac / 100) % 10) as u8) as char);
    out.push((b'0' + ((frac / 10) % 10) as u8) as char);
    out.push((b'0' + (frac % 10) as u8) as char);
}

fn emit_pdf_path_ops(ctx: &PageRenderCtx<'_>, path: &BezierPath, out: &mut String) {
    let pt_ok = |p: kurbo::Point| p.x.is_finite() && p.y.is_finite();
    for sp in &path.subpaths {
        if sp.nodes.is_empty() || !sp.nodes[0].pos.is_finite() {
            continue;
        }
        let (x0, y0) = ctx.to_pdf_pt(sp.nodes[0].pos);
        push_pdf_f4(out, x0);
        out.push(' ');
        push_pdf_f4(out, y0);
        out.push_str(" m\n");
        let seg_count = sp.segment_count();
        for s in 0..seg_count {
            if let Some(seg) = sp.segment_to_kurbo(s) {
                match seg {
                    kurbo::PathSeg::Line(l) if pt_ok(l.p1) => {
                        let (bx, by) = ctx.to_pdf_pt(Point2D::from_kurbo(l.p1));
                        push_pdf_f4(out, bx);
                        out.push(' ');
                        push_pdf_f4(out, by);
                        out.push_str(" l\n");
                    }
                    kurbo::PathSeg::Quad(q) if pt_ok(q.p0) && pt_ok(q.p1) && pt_ok(q.p2) => {
                        let c = q.raise();
                        let (x1, y1) = ctx.to_pdf_pt(Point2D::from_kurbo(c.p1));
                        let (x2, y2) = ctx.to_pdf_pt(Point2D::from_kurbo(c.p2));
                        let (x3, y3) = ctx.to_pdf_pt(Point2D::from_kurbo(c.p3));
                        push_pdf_f4(out, x1);
                        out.push(' ');
                        push_pdf_f4(out, y1);
                        out.push(' ');
                        push_pdf_f4(out, x2);
                        out.push(' ');
                        push_pdf_f4(out, y2);
                        out.push(' ');
                        push_pdf_f4(out, x3);
                        out.push(' ');
                        push_pdf_f4(out, y3);
                        out.push_str(" c\n");
                    }
                    kurbo::PathSeg::Cubic(c) if pt_ok(c.p1) && pt_ok(c.p2) && pt_ok(c.p3) => {
                        let (x1, y1) = ctx.to_pdf_pt(Point2D::from_kurbo(c.p1));
                        let (x2, y2) = ctx.to_pdf_pt(Point2D::from_kurbo(c.p2));
                        let (x3, y3) = ctx.to_pdf_pt(Point2D::from_kurbo(c.p3));
                        push_pdf_f4(out, x1);
                        out.push(' ');
                        push_pdf_f4(out, y1);
                        out.push(' ');
                        push_pdf_f4(out, x2);
                        out.push(' ');
                        push_pdf_f4(out, y2);
                        out.push(' ');
                        push_pdf_f4(out, x3);
                        out.push(' ');
                        push_pdf_f4(out, y3);
                        out.push_str(" c\n");
                    }
                    _ => {}
                }
            }
        }
        if sp.closed {
            out.push_str("h\n");
        }
    }
}

fn sanitize_unit_f32(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn sanitize_pdf_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .collect();
    if cleaned.is_empty() {
        "SpotInk".to_string()
    } else {
        cleaned
    }
}

fn intern_spot_colorspace(
    spot_name: &str,
    fallback_cmyk: [f32; 4],
    emit: &mut PdfPageEmit<'_>,
) -> String {
    let key = sanitize_pdf_name(spot_name);
    if let Some((cs_res, cs_id)) = emit.spot_cache.get(&key) {
        let cs_res = cs_res.clone();
        emit.used_spots.insert(cs_res.clone(), *cs_id);
        return cs_res;
    }
    let [c, m, y, k] = fallback_cmyk.map(sanitize_unit_f32);
    let tint_fn_id = emit.store.add(
        format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [{c:.4} {m:.4} {y:.4} {k:.4}] /N 1 >>"
        )
        .into_bytes(),
    );
    let cs_id = emit
        .store
        .add(format!("[/Separation /{key} /DeviceCMYK {tint_fn_id} 0 R]").into_bytes());
    let cs_res = format!("Sep{}", emit.spot_cache.len() + 1);
    emit.spot_cache.insert(key, (cs_res.clone(), cs_id));
    emit.used_spots.insert(cs_res.clone(), cs_id);
    cs_res
}

fn emit_pdf_process_color(out: &mut String, color: &Color, use_cmyk: bool, stroke: bool) {
    if use_cmyk {
        let [c, m, y, k] = color.to_cmyk().map(sanitize_unit_f32);
        let op = if stroke { "K" } else { "k" };
        let _ = writeln!(out, "{c:.4} {m:.4} {y:.4} {k:.4} {op}");
    } else {
        let [r, g, b, _] = color.to_rgba_f32().map(sanitize_unit_f32);
        let op = if stroke { "RG" } else { "rg" };
        let _ = writeln!(out, "{r:.4} {g:.4} {b:.4} {op}");
    }
}

fn emit_pdf_color(
    out: &mut String,
    color: &Color,
    use_cmyk: bool,
    stroke: bool,
    emit: &mut PdfPageEmit<'_>,
) {
    if let Color::Spot {
        name,
        tint,
        fallback_cmyk,
        ..
    } = color
    {
        if !name.trim().is_empty() {
            let cs_res = intern_spot_colorspace(name, *fallback_cmyk, emit);
            let t = sanitize_unit_f32(*tint);
            if stroke {
                let _ = writeln!(out, "/{cs_res} CS {t:.4} SCN");
            } else {
                let _ = writeln!(out, "/{cs_res} cs {t:.4} scn");
            }
            return;
        }
    }
    emit_pdf_process_color(out, color, use_cmyk, stroke);
}

fn emit_pdf_stroke_state(out: &mut String, outline: &OutlineStyle, world_xf: Affine2D) {
    let raw_w = outline.scaled_width_mm_for_transform(&world_xf);
    let width_pt = if raw_w.is_finite() {
        (raw_w * MM_TO_PT).max(0.1)
    } else {
        0.25
    };
    let cap = match outline.cap {
        LineCap::Butt => 0,
        LineCap::Round => 1,
        LineCap::Square => 2,
    };
    let join = match outline.join {
        LineJoin::Miter => 0,
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
    };
    let miter = if outline.miter_limit.is_finite() {
        outline.miter_limit.max(1.0)
    } else {
        4.0
    };
    let _ = writeln!(out, "{width_pt:.4} w {cap} J {join} j {miter:.3} M");
    if !outline.dash_pattern.is_empty() {
        let parts: Vec<String> = outline
            .dash_pattern
            .iter()
            .filter(|d| d.is_finite() && **d > 0.0)
            .map(|d| format!("{:.4}", (d * MM_TO_PT).max(0.1)))
            .collect();
        if !parts.is_empty() {
            let _ = writeln!(out, "[{}] 0 d", parts.join(" "));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_print_marks(
    ctx: &PageRenderCtx<'_>,
    out: &mut String,
    page: &Page,
    margin_mm: f64,
    bleed_mm: f64,
    use_cmyk: bool,
    opts: &PdfExportOptions,
    slug_opt: Option<&(Vec<ShapedPdfRun>, Vec<coree_render::ShapedGlyphDraw>)>,
    emit: &mut PdfPageEmit<'_>,
) {
    let tx0 = margin_mm * MM_TO_PT;
    let ty0 = margin_mm * MM_TO_PT;
    let tx1 = (margin_mm + page.width_mm) * MM_TO_PT;
    let ty1 = (margin_mm + page.height_mm) * MM_TO_PT;
    let gap = (bleed_mm.max(2.0) + 1.0) * MM_TO_PT;
    let mark_len = 6.0 * MM_TO_PT;

    out.push_str("q\n0.25 w 0 J\n");
    // Registration black (100% all plates in CMYK, black in RGB)
    if use_cmyk {
        out.push_str("1 1 1 1 K\n1 1 1 1 k\n");
    } else {
        out.push_str("0 0 0 RG\n0 0 0 rg\n");
    }

    if opts.crop_marks {
        // Bottom-left
        let _ = writeln!(
            out,
            "{:.4} {ty0:.4} m {:.4} {ty0:.4} l S",
            tx0 - gap - mark_len,
            tx0 - gap
        );
        let _ = writeln!(
            out,
            "{tx0:.4} {:.4} m {tx0:.4} {:.4} l S",
            ty0 - gap - mark_len,
            ty0 - gap
        );
        // Bottom-right
        let _ = writeln!(
            out,
            "{:.4} {ty0:.4} m {:.4} {ty0:.4} l S",
            tx1 + gap,
            tx1 + gap + mark_len
        );
        let _ = writeln!(
            out,
            "{tx1:.4} {:.4} m {tx1:.4} {:.4} l S",
            ty0 - gap - mark_len,
            ty0 - gap
        );
        // Top-left
        let _ = writeln!(
            out,
            "{:.4} {ty1:.4} m {:.4} {ty1:.4} l S",
            tx0 - gap - mark_len,
            tx0 - gap
        );
        let _ = writeln!(
            out,
            "{tx0:.4} {:.4} m {tx0:.4} {:.4} l S",
            ty1 + gap,
            ty1 + gap + mark_len
        );
        // Top-right
        let _ = writeln!(
            out,
            "{:.4} {ty1:.4} m {:.4} {ty1:.4} l S",
            tx1 + gap,
            tx1 + gap + mark_len
        );
        let _ = writeln!(
            out,
            "{tx1:.4} {:.4} m {tx1:.4} {:.4} l S",
            ty1 + gap,
            ty1 + gap + mark_len
        );
    }

    if opts.registration_marks {
        let cx = 0.5 * (tx0 + tx1);
        let cy = 0.5 * (ty0 + ty1);
        let offset = gap + mark_len * 0.5;
        let targets = [
            (cx, ty1 + offset),
            (cx, ty0 - offset),
            (tx0 - offset, cy),
            (tx1 + offset, cy),
        ];
        let r = 2.0 * MM_TO_PT;
        let k = 0.552_284_75 * r;
        let cross = 3.2 * MM_TO_PT;
        for (rx, ry) in targets {
            let _ = writeln!(
                out,
                "{:.4} {ry:.4} m {:.4} {ry:.4} l S",
                rx - cross,
                rx + cross
            );
            let _ = writeln!(
                out,
                "{rx:.4} {:.4} m {rx:.4} {:.4} l S",
                ry - cross,
                ry + cross
            );
            // Circle via 4 cubic arcs
            let _ = writeln!(
                out,
                "{:.4} {:.4} m {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} c \
                 {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} c \
                 {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} c \
                 {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} c h S",
                rx + r,
                ry,
                rx + r,
                ry + k,
                rx + k,
                ry + r,
                rx,
                ry + r,
                rx - k,
                ry + r,
                rx - r,
                ry + k,
                rx - r,
                ry,
                rx - r,
                ry - k,
                rx - k,
                ry - r,
                rx,
                ry - r,
                rx + k,
                ry - r,
                rx + r,
                ry - k,
                rx + r,
                ry,
            );
        }
    }

    if opts.color_calibration_bars {
        // 11 CMYK calibration swatches placed along the top margin
        let swatches: &[[f32; 4]] = &[
            [1.0, 0.0, 0.0, 0.0], // C
            [0.0, 1.0, 0.0, 0.0], // M
            [0.0, 0.0, 1.0, 0.0], // Y
            [0.0, 0.0, 0.0, 1.0], // K
            [0.0, 1.0, 1.0, 0.0], // R (M+Y)
            [1.0, 0.0, 1.0, 0.0], // G (C+Y)
            [1.0, 1.0, 0.0, 0.0], // B (C+M)
            [0.0, 0.0, 0.0, 0.25],
            [0.0, 0.0, 0.0, 0.50],
            [0.0, 0.0, 0.0, 0.75],
            [0.0, 0.0, 0.0, 1.00],
        ];
        let sw = 4.0 * MM_TO_PT;
        let sh = 3.5 * MM_TO_PT;
        let bar_y = ty1 + gap + 1.0 * MM_TO_PT;
        let start_x = tx0;
        for (i, &[c, m, y, k]) in swatches.iter().enumerate() {
            let x = start_x + (i as f64) * sw;
            let color = Color::cmyk(c, m, y, k);
            emit_pdf_process_color(out, &color, use_cmyk, false);
            let _ = writeln!(out, "{x:.4} {bar_y:.4} {sw:.4} {sh:.4} re f");
        }
    }

    if opts.page_info_slug {
        let slug_y = (ty0 - gap - 4.0 * MM_TO_PT).max(2.0);
        if let Some((slug_runs, slug_curves)) = slug_opt {
            let baseline_local_y = slug_runs
                .first()
                .and_then(|r| r.glyphs.first())
                .map(|g| g.y_mm)
                .unwrap_or(2.0);
            let target_y_mm = page.height_mm + margin_mm - (slug_y / MM_TO_PT) - baseline_local_y;
            let slug_xf = Affine2D::translate(0.0, target_y_mm);
            let none_outline = OutlineStyle::none();
            for run in slug_runs {
                emit_pdf_embedded_run(
                    ctx,
                    run,
                    &none_outline,
                    slug_xf,
                    1.0,
                    BlendMode::Normal,
                    emit,
                    out,
                );
            }
            if !slug_curves.is_empty() {
                emit_pdf_shaped_pieces(
                    ctx,
                    slug_curves,
                    &none_outline,
                    slug_xf,
                    1.0,
                    BlendMode::Normal,
                    emit,
                    out,
                );
            }
        }
    }

    out.push_str("Q\n");
}

fn add_raster_xobject(
    store: &mut PdfObjectStore,
    asset: &coree_core::RasterAsset,
    use_cmyk: bool,
    doc_icc: Option<&coree_core::IccColorPipeline>,
) -> IoResult<usize> {
    let rgba8 = asset.to_rgba8_vec();
    add_rgba_bytes_xobject(store, asset.width, asset.height, &rgba8, use_cmyk, doc_icc)
}

fn add_rgba_bytes_xobject(
    store: &mut PdfObjectStore,
    width: u32,
    height: u32,
    rgba_bytes: &[u8],
    use_cmyk: bool,
    doc_icc: Option<&coree_core::IccColorPipeline>,
) -> IoResult<usize> {
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "pdf".into(),
            message: "raster asset dimensions overflow".into(),
        })?;
    let expected_bytes = pixel_count
        .checked_mul(4)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "pdf".into(),
            message: "raster asset byte size overflow".into(),
        })?;
    if rgba_bytes.len() != expected_bytes {
        return Err(IoError::InvalidFormat {
            format: "pdf".into(),
            message: format!(
                "raster image has {} RGBA bytes, expected {expected_bytes}",
                rgba_bytes.len()
            ),
        });
    }
    let mut color_data = Vec::with_capacity(pixel_count * if use_cmyk { 4 } else { 3 });
    let mut alpha_data = Vec::with_capacity(pixel_count);
    let mut has_transparency = false;

    for px in rgba_bytes.as_chunks::<4>().0 {
        if use_cmyk {
            let c = if let Some(icc) = doc_icc {
                icc.srgb_to_cmyk([
                    f32::from(px[0]) / 255.0,
                    f32::from(px[1]) / 255.0,
                    f32::from(px[2]) / 255.0,
                ])
            } else {
                Color::rgba8(px[0], px[1], px[2], 255).to_cmyk()
            };
            color_data.push((c[0] * 255.0).round() as u8);
            color_data.push((c[1] * 255.0).round() as u8);
            color_data.push((c[2] * 255.0).round() as u8);
            color_data.push((c[3] * 255.0).round() as u8);
        } else {
            color_data.push(px[0]);
            color_data.push(px[1]);
            color_data.push(px[2]);
        }
        if px[3] < 255 {
            has_transparency = true;
        }
        alpha_data.push(px[3]);
    }

    let smask_ref = if has_transparency {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&alpha_data)?;
        let compressed = enc.finish()?;
        let mut smask_buf = format!(
            "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
             /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>\nstream\n",
            compressed.len()
        )
        .into_bytes();
        smask_buf.extend_from_slice(&compressed);
        smask_buf.extend_from_slice(b"\nendstream");
        let smask_id = store.add(smask_buf);
        format!(" /SMask {smask_id} 0 R")
    } else {
        String::new()
    };

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&color_data)?;
    let compressed = enc.finish()?;
    let cs = if use_cmyk {
        "/DeviceCMYK"
    } else {
        "/DeviceRGB"
    };
    let mut img_buf = format!(
        "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
         /ColorSpace {cs} /BitsPerComponent 8{smask_ref} /Filter /FlateDecode /Length {} >>\nstream\n",
        compressed.len()
    )
    .into_bytes();
    img_buf.extend_from_slice(&compressed);
    img_buf.extend_from_slice(b"\nendstream");
    Ok(store.add(img_buf))
}

fn build_extgstate_objects(
    store: &mut PdfObjectStore,
    gstates: &BTreeMap<GStateKey, String>,
    is_pdfx1a: bool,
) -> String {
    let mut gstate_refs = String::new();
    for (key, gs_name) in gstates {
        if is_pdfx1a {
            if !key.overprint {
                continue;
            }
            let gs_id = store.add(b"<< /Type /ExtGState /OP true /op true /OPM 1 >>".to_vec());
            let _ = write!(gstate_refs, " /{gs_name} {gs_id} 0 R");
        } else {
            let mut dict = format!(
                "<< /Type /ExtGState /ca {:.4} /CA {:.4} /BM /{}",
                f64::from(key.fill_alpha_milli) / 1000.0,
                f64::from(key.stroke_alpha_milli) / 1000.0,
                key.blend_mode
            );
            if key.overprint {
                dict.push_str(" /OP true /op true /OPM 1");
            }
            dict.push_str(" >>");
            let gs_id = store.add(dict.into_bytes());
            let _ = write!(gstate_refs, " /{gs_name} {gs_id} 0 R");
        }
    }
    gstate_refs
}

fn intern_gstate(
    gstates: &mut BTreeMap<GStateKey, String>,
    fill_alpha: f32,
    stroke_alpha: f32,
    blend_mode: BlendMode,
    overprint: bool,
) -> String {
    let key = GStateKey {
        fill_alpha_milli: (fill_alpha.clamp(0.0, 1.0) * 1000.0).round() as u16,
        stroke_alpha_milli: (stroke_alpha.clamp(0.0, 1.0) * 1000.0).round() as u16,
        blend_mode: pdf_blend_mode_name(blend_mode),
        overprint,
    };
    if let Some(existing) = gstates.get(&key) {
        return existing.clone();
    }
    let name = format!("GS{}", gstates.len());
    gstates.insert(key, name.clone());
    name
}

const fn pdf_blend_mode_name(bm: BlendMode) -> &'static str {
    match bm {
        BlendMode::Normal => "Normal",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
        BlendMode::Overlay => "Overlay",
        BlendMode::Darken => "Darken",
        BlendMode::Lighten => "Lighten",
        BlendMode::Difference => "Difference",
        BlendMode::Exclusion => "Exclusion",
        BlendMode::SoftLight => "SoftLight",
        BlendMode::HardLight => "HardLight",
        BlendMode::ColorDodge => "ColorDodge",
        BlendMode::ColorBurn => "ColorBurn",
        BlendMode::Hue => "Hue",
        BlendMode::Saturation => "Saturation",
        BlendMode::Color => "Color",
        BlendMode::Luminosity => "Luminosity",
    }
}

fn fill_representative_color(fill: &Fill) -> Option<Color> {
    match fill {
        Fill::None | Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => None,
        Fill::Solid(c) => Some(c.clone()),
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => stops.first().map(|s| s.color.clone()),
        Fill::TwoColorPattern { fg, .. } | Fill::TextureFill { fg, .. } => Some(fg.clone()),
        Fill::MeshFill { nodes, .. } => nodes.first().map(|n| n.color.clone()),
    }
}

fn escape_pdf_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_ascii() && !c.is_control() => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        ArtisticText, CornerStyle, Effect, NodePayload, ObjectId, ParametricShape, RasterAsset,
        RasterLayerData, SceneObject, TextDirection, TextStyle,
    };

    #[test]
    fn pdf_1_7_cmyk_print_ready_multipage_with_marks_and_valid_xref() {
        let mut doc = Document::new_a4("Business Flyer");
        let p2 = doc.add_page("Back", 210.0, 297.0, 3.0);
        let p2_layer = doc.page_by_id(p2).unwrap().layers[0].id;

        let mut rect = SceneObject::new(
            ObjectId(0),
            "Cyan Banner",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 100.0,
                height: 50.0,
                corner_radii: [5.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::cut_contour())
        .with_transform(Affine2D::translate(20.0, 20.0));
        rect.opacity = 0.75;
        rect.blend_mode = BlendMode::Multiply;
        doc.add_object(rect, None).unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Heading",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "Print Ready PDF",
                    TextStyle::default(),
                )),
            )
            .with_transform(Affine2D::translate(20.0, 90.0)),
            None,
        )
        .unwrap();

        let asset =
            RasterAsset::new(2, 2, [255, 0, 128, 128].repeat(4), 300.0, ColorMode::Cmyk).unwrap();
        let asset_id = doc.register_asset(asset);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Back Photo",
                NodePayload::Raster(RasterLayerData {
                    asset_id,
                    width_mm: 40.0,
                    height_mm: 40.0,
                    filters: vec![],
                    filter_enabled: Vec::new(),
                    crop_rect: None,
                }),
            ),
            Some(p2_layer),
        )
        .unwrap();

        let bytes = export_pdf_to_bytes(&doc, &PdfExportOptions::print_ready()).unwrap();
        let text = String::from_utf8_lossy(&bytes);

        assert!(bytes.starts_with(b"%PDF-1.7\n"));
        assert!(text.contains("/Count 2"));
        assert!(text.contains("/MediaBox"));
        assert!(text.contains("/TrimBox"));
        assert!(text.contains("/BleedBox"));
        assert!(text.contains("/ExtGState"));
        assert!(text.contains("/BM /Multiply"));
        assert!(text.contains("/OP true"));
        assert!(text.contains("1.0000 0.0000 0.0000 0.0000 k")); // DeviceCMYK Cyan fill
        assert!(text.contains("1 1 1 1 K")); // Registration crop marks
        assert!(text.contains("/Type0"));
        assert!(text.contains("/CIDFontType2"));
        assert!(text.contains("/ToUnicode"));
        assert!(text.contains("/FontFile2"));
        assert!(text.contains("/GTS_PDFXVersion"));
        assert!(text.contains("/ID ["));
        assert!(!text.contains("/Helvetica"));
        assert!(!text.contains("/Subtype /Type1"));
        assert!(text.contains("/Subtype /Image"));
        assert!(text.contains("/SMask"));
        assert!(text.contains("xref\n"));
        assert!(text.contains("trailer\n"));
        assert!(text.contains("%%EOF\n"));

        // Verify byte-exact xref offset actually points to `xref`
        let startxref_pos = text.rfind("startxref\n").unwrap();
        let offset_line = text[startxref_pos + "startxref\n".len()..]
            .lines()
            .next()
            .unwrap()
            .trim();
        let xref_off: usize = offset_line.parse().unwrap();
        assert_eq!(&bytes[xref_off..xref_off + 4], b"xref");
    }

    #[test]
    fn pdf_rgb_and_compressed_streams_export_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("screen.pdf");
        let mut doc = Document::new("RGB Flyer", 100.0, 100.0, ColorMode::Rgb, 96.0);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Red Box",
                NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                    10.0, 10.0, 30.0, 30.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::rgb(1.0, 0.0, 0.0)))
            .with_outline(OutlineStyle::solid(Color::rgb(0.0, 0.0, 1.0), 1.0)),
            None,
        )
        .unwrap();

        let opts_uncompressed = PdfExportOptions {
            color_space: PdfColorSpace::DeviceRgb,
            ..PdfExportOptions::default()
        };
        let bytes = export_pdf_to_bytes(&doc, &opts_uncompressed).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("1.0000 0.0000 0.0000 rg"));
        assert!(text.contains("0.0000 0.0000 1.0000 RG"));

        let opts_compressed = PdfExportOptions {
            compress_streams: true,
            ..opts_uncompressed
        };
        export_pdf(&doc, &opts_compressed, &path).unwrap();
        let file_bytes = std::fs::read(&path).unwrap();
        assert!(file_bytes.starts_with(b"%PDF-1.7\n"));
        assert!(String::from_utf8_lossy(&file_bytes).contains("/Filter /FlateDecode"));
    }

    #[test]
    fn pdf_arabic_curves_spot_separation_gradients_and_effects_export() {
        let mut doc = Document::new("Pro Print Proof", 150.0, 100.0, ColorMode::Cmyk, 300.0);

        // 1. Arabic RTL ArtisticText -> must produce real glyph curves (no Tofu boxes!)
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Arabic Title",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "درع الشكر والتقدير",
                    TextStyle {
                        font_size_pt: 28.0,
                        direction: TextDirection::Rtl,
                        fill: Color::cmyk(0.0, 0.9, 0.9, 0.0),
                        ..TextStyle::default()
                    },
                )),
            )
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(30.0, 25.0)),
            None,
        )
        .unwrap();

        // 2. Multi-stop Linear Gradient + Spot CutContour outline
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Badge",
                NodePayload::Shape(coree_core::ParametricShape::Rectangle {
                    width: 60.0,
                    height: 30.0,
                    corner_radii: [4.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::LinearGradient {
                start: Point2D::new(0.0, 0.0),
                end: Point2D::new(60.0, 0.0),
                stops: vec![
                    GradientStop::new(0.0, Color::cmyk(1.0, 0.0, 0.0, 0.0)),
                    GradientStop::new(0.5, Color::cmyk(0.0, 1.0, 0.0, 0.0)),
                    GradientStop::new(1.0, Color::cmyk(0.0, 0.0, 1.0, 0.0)),
                ],
            })
            .with_outline(OutlineStyle::cut_contour())
            .with_transform(Affine2D::translate(20.0, 50.0)),
            None,
        )
        .unwrap();

        // 3. Radial gradient + DropShadow -> complex effect rasterized cleanly with SMask
        let mut shadowed = SceneObject::new(
            ObjectId(0),
            "Shadowed Orb",
            NodePayload::Shape(coree_core::ParametricShape::Ellipse {
                rx: 12.0,
                ry: 12.0,
                mode: coree_core::EllipseMode::Full,
                start_deg: 0.0,
                end_deg: 360.0,
            }),
        )
        .with_fill(Fill::RadialGradient {
            center: Point2D::new(0.0, 0.0),
            radius: 12.0,
            focal: Point2D::new(-3.0, -3.0),
            stops: vec![
                GradientStop::new(0.0, Color::WHITE),
                GradientStop::new(1.0, Color::cmyk(0.8, 0.2, 0.0, 0.0)),
            ],
        })
        .with_transform(Affine2D::translate(115.0, 60.0));
        shadowed.effects.push(Effect::DropShadow {
            dx_mm: 2.0,
            dy_mm: 2.0,
            blur_mm: 2.5,
            opacity: 0.5,
            color: Color::BLACK,
            feather_direction: coree_core::FeatherDirection::Outside,
        });
        doc.add_object(shadowed, None).unwrap();

        let opts = PdfExportOptions {
            compress_streams: false,
            ..PdfExportOptions::default()
        };
        let pdf_bytes = export_pdf_to_bytes(&doc, &opts).unwrap();
        let text = String::from_utf8_lossy(&pdf_bytes);

        // True Separation spot color space for CutContour
        assert!(text.contains("/Separation /CutContour /DeviceCMYK"));
        assert!(text.contains("/ColorSpace <<"));
        assert!(text.contains(" SCN"));
        // True multi-stop Type 3 stitching shading
        assert!(text.contains("/ShadingType 2"));
        assert!(text.contains("/FunctionType 3"));
        assert!(text.contains(" sh"));
        // Real Arabic Bezier glyph curves (quadratic/cubic converted to PDF ` c` curves)
        assert!(text.contains(" c\n"));
        // Complex shadow composite emitted as RGBA Image XObject + SMask
        assert!(text.contains("/Subtype /Image"));
        assert!(text.contains("/SMask"));
        // ISO 32000-1 /OutputIntents CMYK ICC v2 embedding in /Catalog
        assert!(text.contains("/OutputIntents"));
        assert!(text.contains("/GTS_PDFX"));
        assert!(text.contains("/DestOutputProfile"));
    }

    #[test]
    fn pdf_embeds_custom_icc_profile_and_exports_desktop_layer_across_pages() {
        let mut doc = Document::new_a4("Desktop & Custom ICC");
        doc.add_page("Page 2", 210.0, 297.0, 3.0);
        let srgb_icc = coree_core::generate_minimal_srgb_icc_v2("Custom Studio sRGB");
        doc.set_custom_icc_profile(srgb_icc).unwrap();

        let desk_rect = SceneObject::new(
            ObjectId(0),
            "Desktop Swatch Strip",
            NodePayload::Shape(coree_core::ParametricShape::Rectangle {
                width: 30.0,
                height: 15.0,
                corner_radii: [0.0; 4],
                corner_style: coree_core::CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::cmyk(0.95, 0.0, 0.0, 0.0)));
        let did = doc.add_object(desk_rect, None).unwrap();
        doc.move_object_to_desktop_layer(did).unwrap();

        let opts = PdfExportOptions {
            compress_streams: false,
            pages: Some(vec![1]),
            ..PdfExportOptions::default()
        };
        let bytes = export_pdf_to_bytes(&doc, &opts).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/OutputIntents"));
        assert!(text.contains("Custom") || text.contains("sRGB"));
        // Desktop layer rect appears cleanly even when exporting ONLY Page 2 (`pages: Some(vec![1])`)!
        assert!(text.contains("0.9500 0.0000 0.0000 0.0000 k"));
    }

    #[test]
    fn pdfx_auto_selects_1a_for_opaque_cmyk_and_pdfx4_with_page_group_for_transparency() {
        let mut opaque_doc = Document::new_a4("Opaque CMYK");
        opaque_doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Solid Box",
                    NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                        10.0, 10.0, 40.0, 30.0,
                    ))),
                )
                .with_fill(Fill::Solid(Color::cmyk(1.0, 0.2, 0.0, 0.0)))
                .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();
        let bytes_1a = export_pdf_to_bytes(&opaque_doc, &PdfExportOptions::default()).unwrap();
        let text_1a = String::from_utf8_lossy(&bytes_1a);
        assert!(text_1a.contains("PDF/X-1a:2001"));
        assert!(text_1a.contains("/Trapped /False"));
        assert!(text_1a.contains("/OutputIntents"));
        assert!(text_1a.contains("/ID ["));
        assert!(!text_1a.contains("/Type /Font"));

        // Transparent RGB doc -> PDF/X-4 with `/Group << /S /Transparency /CS /DeviceRGB ... >>`
        let mut trans_doc = Document::new("Trans RGB", 100.0, 100.0, ColorMode::Rgb, 96.0);
        trans_doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Alpha Box",
                    NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                        10.0, 10.0, 40.0, 30.0,
                    ))),
                )
                .with_fill(Fill::Solid(Color::rgba8(20, 120, 220, 140)))
                .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();
        let bytes_x4 = export_pdf_to_bytes(
            &trans_doc,
            &PdfExportOptions {
                compress_streams: false,
                ..PdfExportOptions::default()
            },
        )
        .unwrap();
        let text_x4 = String::from_utf8_lossy(&bytes_x4);
        assert!(text_x4.contains("PDF/X-4"));
        assert!(text_x4.contains("/Group << /S /Transparency /CS /DeviceRGB"));
    }

    #[test]
    fn pdf_powerclip_cutcontour_under_inner_shadow_and_clone_have_vector_separation() {
        let mut doc = Document::new_a4("PowerClip CutContour InnerShadow");
        let child_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Clipped",
                    NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                        0.0, 0.0, 30.0, 30.0,
                    ))),
                )
                .with_fill(Fill::Solid(Color::cmyk(0.0, 0.0, 1.0, 0.0))),
                None,
            )
            .unwrap();

        let mut container = SceneObject::new(
            ObjectId(0),
            "Container",
            NodePayload::PowerClip {
                container: Box::new(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                    10.0, 10.0, 60.0, 40.0,
                ))),
                container_parametric: None,
                contents: vec![child_id],
                lock_contents: false,
            },
        )
        .with_fill(Fill::Solid(Color::cmyk(0.9, 0.1, 0.0, 0.0)))
        .with_outline(OutlineStyle::cut_contour());
        container.effects.push(Effect::InnerShadow {
            dx_mm: 1.5,
            dy_mm: 1.5,
            blur_mm: 2.0,
            opacity: 0.6,
            color: Color::BLACK,
            feather_direction: coree_core::FeatherDirection::Middle,
        });
        let container_id = doc.add_object(container, None).unwrap();

        let clone_obj = SceneObject::new(
            ObjectId(0),
            "Clone PC",
            NodePayload::Clone {
                master_id: container_id,
                link_fill: true,
                link_outline: true,
                link_shape: true,
            },
        )
        .with_transform(Affine2D::translate(80.0, 0.0));
        let clone_id = doc.add_object(clone_obj, None).unwrap();
        assert!(doc.object(clone_id).is_some());

        let bytes = export_pdf_to_bytes(
            &doc,
            &PdfExportOptions {
                compress_streams: false,
                ..PdfExportOptions::default()
            },
        )
        .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/Separation /CutContour /DeviceCMYK"));
        assert!(text.contains("/Subtype /Image"));
    }

    #[test]
    fn pdf_subsetting_bilingual_kashida_and_external_poppler_gs_oracles() {
        let mut doc = Document::new_a4("Bilingual Brochure");
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Headline",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "coree Commercial Brochure 2026",
                    TextStyle {
                        font_size_pt: 22.0,
                        bold: true,
                        fill: Color::cmyk(1.0, 0.4, 0.0, 0.0),
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(20.0, 25.0)),
            None,
        )
        .unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Arabic Kashida",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "تصميم احترافي",
                    TextStyle {
                        font_size_pt: 20.0,
                        direction: TextDirection::Rtl,
                        kashida_stretch: 0.6,
                        fill: Color::cmyk(0.0, 0.9, 0.8, 0.0),
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(150.0, 55.0)),
            None,
        )
        .unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Mixed Bidi",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "Second Page الصفحة الثانية",
                    TextStyle {
                        font_size_pt: 16.0,
                        fill: Color::cmyk(0.0, 0.0, 0.0, 1.0),
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(20.0, 90.0)),
            None,
        )
        .unwrap();

        let bytes = export_pdf_to_bytes(&doc, &PdfExportOptions::print_ready()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pdf_path = dir.path().join("brochure.pdf");
        std::fs::write(&pdf_path, &bytes).unwrap();

        if std::path::Path::new("/usr/bin/pdffonts").exists() {
            let out = std::process::Command::new("/usr/bin/pdffonts")
                .arg(&pdf_path)
                .output()
                .unwrap();
            assert!(out.status.success(), "pdffonts failed");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.contains("Type 0")
                    || stdout.contains("CID TrueType")
                    || stdout.contains("CID Type 0"),
                "expected Type0/CID fonts in pdffonts:\n{stdout}"
            );
            for line in stdout.lines().skip(2).filter(|l| !l.trim().is_empty()) {
                assert!(
                    line.contains("yes"),
                    "every embedded font must report emb=yes sub=yes uni=yes, got: {line}\n{stdout}"
                );
            }
        }

        if std::path::Path::new("/usr/bin/pdftotext").exists() {
            let out = std::process::Command::new("/usr/bin/pdftotext")
                .args(["-enc", "UTF-8"])
                .arg(&pdf_path)
                .arg("-")
                .output()
                .unwrap();
            assert!(out.status.success(), "pdftotext failed");
            let extracted = String::from_utf8_lossy(&out.stdout);
            assert!(
                extracted.contains("coree Commercial Brochure 2026"),
                "pdftotext missing Latin headline:\n{extracted}"
            );
            let bbox_out = std::process::Command::new("/usr/bin/pdftotext")
                .args(["-bbox", "-enc", "UTF-8"])
                .arg(&pdf_path)
                .arg("-")
                .output()
                .unwrap();
            let bbox_txt = String::from_utf8_lossy(&bbox_out.stdout);
            let raw_text = String::from_utf8_lossy(&bytes);
            assert!(
                extracted.contains("تصميم احترافي"),
                "pdftotext missing Arabic Kashida string:\n{extracted}\nbbox:\n{bbox_txt}\npdf_stream:\n{}",
                raw_text.lines().filter(|l| l.contains("BT ") || l.contains("TJ") || l.contains("BDC") || l.contains("EMC") || l.contains("bfchar")).collect::<Vec<_>>().join("\n")
            );
            assert!(
                extracted.contains("Second Page") && extracted.contains("الصفحة الثانية"),
                "pdftotext missing mixed bidi string:\n{extracted}"
            );
        }

        if std::path::Path::new("/usr/bin/pdfinfo").exists() {
            let out = std::process::Command::new("/usr/bin/pdfinfo")
                .arg("-box")
                .arg(&pdf_path)
                .output()
                .unwrap();
            assert!(out.status.success(), "pdfinfo failed");
            let info = String::from_utf8_lossy(&out.stdout);
            assert!(
                info.contains("TrimBox:") && info.contains("BleedBox:"),
                "{info}"
            );
        }

        if std::path::Path::new("/usr/bin/gs").exists() {
            let png_out = dir.path().join("gs_check.png");
            let out = std::process::Command::new("/usr/bin/gs")
                .args([
                    "-dSAFER",
                    "-dBATCH",
                    "-dNOPAUSE",
                    "-sDEVICE=png16m",
                    "-r72",
                    &format!("-sOutputFile={}", png_out.display()),
                ])
                .arg(&pdf_path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "gs failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[test]
    fn pdfx1a_omits_transparency_extgstate_on_unstroked_and_unfilled_elements() {
        let mut doc = Document::new("PDFX1a Opaque", 120.0, 80.0, ColorMode::Cmyk, 300.0);
        // 1. Unstroked opaque path (`OutlineStyle::none()` has `Color::TRANSPARENT`)
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "UnstrokedRect",
                NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                    10.0, 10.0, 40.0, 20.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::cmyk(0.0, 0.8, 0.9, 0.0)))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();
        // 2. Unfilled opaque stroked path (`Fill::None`)
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "UnfilledStroke",
                NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                    60.0, 10.0, 40.0, 20.0,
                ))),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::solid(Color::cmyk(1.0, 0.0, 0.0, 0.0), 1.0)),
            None,
        )
        .unwrap();
        // 3. Unstroked opaque text (`OutlineStyle::none()`)
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "OpaqueText",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "Hello PDFX1a",
                    TextStyle {
                        font_size_pt: 14.0,
                        fill: Color::cmyk(0.0, 0.0, 0.0, 1.0),
                        ..TextStyle::default()
                    },
                )),
            )
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(15.0, 50.0)),
            None,
        )
        .unwrap();

        let opts = PdfExportOptions {
            color_space: PdfColorSpace::DeviceCmyk,
            crop_marks: true,
            compress_streams: false,
            pdfx_conformance: PdfXConformance::Auto,
            ..PdfExportOptions::default()
        };
        let bytes = export_pdf_to_bytes(&doc, &opts).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("/GTS_PDFXVersion (PDF/X-1a:2001)"),
            "opaque CMYK document must resolve to PDF/X-1a:2001"
        );
        assert!(
            !text.contains("/ExtGState"),
            "opaque PDF/X-1a without overprint must not emit /ExtGState:\n{text}"
        );
        assert!(!text.contains("/ca "), "PDF/X-1a must never emit /ca");
        assert!(!text.contains("/CA "), "PDF/X-1a must never emit /CA");
        assert!(!text.contains("/BM "), "PDF/X-1a must never emit /BM");

        // Also test StreamingPdfDocument and overprint-only ExtGState in PDF/X-1a:2001
        let mut overprint_outline = OutlineStyle::solid(Color::cmyk(0.0, 0.0, 0.0, 1.0), 0.5);
        overprint_outline.overprint = true;
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "OverprintStroke",
                NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                    15.0, 55.0, 30.0, 10.0,
                ))),
            )
            .with_fill(Fill::None)
            .with_outline(overprint_outline),
            None,
        )
        .unwrap();

        let bytes_op = export_pdf_to_bytes(&doc, &opts).unwrap();
        let text_op = String::from_utf8_lossy(&bytes_op);
        assert!(text_op.contains("/GTS_PDFXVersion (PDF/X-1a:2001)"));
        assert!(text_op.contains("/ExtGState"));
        assert!(text_op.contains("/OP true /op true /OPM 1"));
        assert!(
            !text_op.contains("/ca ") && !text_op.contains("/CA ") && !text_op.contains("/BM "),
            "PDF/X-1a overprint ExtGState must contain zero /ca, /CA, or /BM:\n{text_op}"
        );

        let dir = tempfile::tempdir().unwrap();
        let stream_path = dir.path().join("stream_pdfx1a.pdf");
        let mut sdoc = StreamingPdfDocument::create(&doc, &opts, &stream_path).unwrap();
        sdoc.push_page(&doc, 0).unwrap();
        sdoc.finish().unwrap();
        let stream_bytes = std::fs::read(&stream_path).unwrap();
        let stream_text = String::from_utf8_lossy(&stream_bytes);
        assert!(stream_text.contains("/GTS_PDFXVersion (PDF/X-1a:2001)"));
        assert!(stream_text.contains("/OP true /op true /OPM 1"));
        assert!(
            !stream_text.contains("/ca ")
                && !stream_text.contains("/CA ")
                && !stream_text.contains("/BM "),
            "streaming PDF/X-1a must contain zero /ca, /CA, or /BM"
        );
    }
}
