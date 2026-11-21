// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust print-ready PDF 1.7 writer supporting multipage vector artwork, true
//! `DeviceCMYK` (`k`/`K`) and `DeviceRGB` (`rg`/`RG`) color spaces, `ExtGState`
//! opacity/blend modes/overprint, `MediaBox`/`TrimBox`/`BleedBox`, print marks
//! (bleed, crop marks, registration marks, CMYK color calibration bars, page slug),
//! embedded raster XObjects, text + outline fallback, and byte-exact xref tables.

use crate::atomic::atomic_write_bytes;
use crate::error::{IoError, IoResult};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, BlendMode, Color, ColorMode, Document, Effect, Fill,
    GradientStop, LineCap, LineJoin, MasterLayerScope, NodePayload, ObjectId, OutlineStyle, Page,
    Point2D, SceneObject, TextDirection,
};
use coree_render::effects_render::{apply_geometry_effects, symmetry_effect_transforms};
use coree_render::{outline_arrowhead_geometries, render_isolated_object_to_rgba, TextEngine};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use kurbo::PathEl;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::io::Write as IoWrite;
use std::path::Path;

/// Exact millimeter to PDF typographic point conversion (`72 / 25.4`).
const MM_TO_PT: f64 = 72.0 / 25.4;
const MAX_PDF_DEPTH: usize = 64;

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

struct PdfObjectStore {
    objects: Vec<Vec<u8>>, // 1-based indexing via `index + 1`
}

impl PdfObjectStore {
    fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }

    fn alloc_placeholder(&mut self) -> usize {
        self.objects.push(Vec::new());
        self.objects.len()
    }

    fn add(&mut self, body: Vec<u8>) -> usize {
        self.objects.push(body);
        self.objects.len()
    }

    fn set(&mut self, id: usize, body: Vec<u8>) {
        self.objects[id - 1] = body;
    }

    fn serialize(self, root_id: usize, info_id: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(8192);
        // PDF 1.7 header + standard 4-byte binary comment
        out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");

        let mut offsets = Vec::with_capacity(self.objects.len() + 1);
        offsets.push(0usize); // object 0 (free head)

        for (idx, body) in self.objects.into_iter().enumerate() {
            let obj_num = idx + 1;
            offsets.push(out.len());
            let _ = writeln!(out, "{obj_num} 0 obj");
            out.extend_from_slice(&body);
            if !body.ends_with(b"\n") {
                out.push(b'\n');
            }
            out.extend_from_slice(b"endobj\n");
        }

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
            "<< /Size {total} /Root {root_id} 0 R /Info {info_id} 0 R >>"
        );
        let _ = writeln!(out, "startxref");
        let _ = writeln!(out, "{xref_offset}");
        out.extend_from_slice(b"%%EOF\n");
        out
    }
}

/// Exports `doc` to a print-ready PDF 1.7 byte buffer.
pub fn export_pdf_to_bytes(doc: &Document, opts: &PdfExportOptions) -> IoResult<Vec<u8>> {
    if doc.pages.is_empty() {
        return Err(IoError::InvalidFormat {
            format: "pdf".into(),
            message: "document has no pages".into(),
        });
    }

    let page_indices: Vec<usize> = match &opts.pages {
        Some(list) => {
            for &idx in list {
                if idx >= doc.pages.len() {
                    return Err(IoError::PageOutOfRange {
                        page_index: idx,
                        page_count: doc.pages.len(),
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
        None => (0..doc.pages.len()).collect(),
    };

    let use_cmyk = match opts.color_space {
        PdfColorSpace::DeviceCmyk => true,
        PdfColorSpace::DeviceRgb => false,
        PdfColorSpace::DocumentDefault => doc.color_mode == ColorMode::Cmyk,
    };

    let mut store = PdfObjectStore::new();
    let catalog_id = store.alloc_placeholder(); // 1
    let info_id = store.alloc_placeholder(); // 2
    let pages_root_id = store.alloc_placeholder(); // 3

    // Standard PDF Base-14 Type1 fonts (no external font files required)
    let font_regular_id = store.add(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );
    let font_bold_id = store.add(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );

    // Pre-register embedded raster assets as PDF Image XObjects
    let mut image_xobjects: BTreeMap<coree_core::ImageAssetId, (String, usize)> = BTreeMap::new();
    for (img_idx, (&asset_id, asset)) in doc.assets.iter().enumerate() {
        let name = format!("Im{}", img_idx + 1);
        let xobj_id = add_raster_xobject(&mut store, asset, use_cmyk)?;
        image_xobjects.insert(asset_id, (name, xobj_id));
    }

    let mut text_engine = TextEngine::new();
    let mut dynamic_img_counter = image_xobjects.len();
    let mut spot_cache: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut page_obj_ids = Vec::with_capacity(page_indices.len());

    for &page_index in &page_indices {
        let page = &doc.pages[page_index];
        if !page.width_mm.is_finite()
            || page.width_mm <= 0.0
            || !page.height_mm.is_finite()
            || page.height_mm <= 0.0
        {
            return Err(IoError::InvalidFormat {
                format: "pdf".into(),
                message: format!(
                    "page {} has invalid non-finite or non-positive dimensions",
                    page_index + 1
                ),
            });
        }
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

        let mut emit_state = PdfPageEmit {
            store: &mut store,
            text_engine: &mut text_engine,
            gstates: BTreeMap::new(),
            used_images: BTreeMap::new(),
            shadings: BTreeMap::new(),
            used_spots: BTreeMap::new(),
            spot_cache: &mut spot_cache,
            dynamic_img_counter: &mut dynamic_img_counter,
        };
        let mut stream = String::with_capacity(4096);

        let ctx = PageRenderCtx {
            doc,
            page,
            page_index,
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

        // Emit applicable master layers from OTHER pages first (bottom), then target page's own layers.
        let mut ordered_layers = Vec::new();
        for (p_idx, p) in doc.pages.iter().enumerate() {
            if p_idx == page_index {
                continue;
            }
            for layer in &p.layers {
                if !layer.visible || !layer.printable {
                    continue;
                }
                let include = match layer.master_scope {
                    MasterLayerScope::None => false,
                    MasterLayerScope::AllPages => true,
                    MasterLayerScope::OddPages => (page_index + 1) % 2 == 1,
                    MasterLayerScope::EvenPages => (page_index + 1).is_multiple_of(2),
                };
                if include {
                    ordered_layers.push(layer);
                }
            }
        }
        for layer in &page.layers {
            if layer.visible && layer.printable {
                ordered_layers.push(layer);
            }
        }

        for layer in ordered_layers {
            let mut visited = HashSet::new();
            for &oid in &layer.objects {
                if let Some(obj) = doc.objects.get(&oid) {
                    emit_pdf_object(
                        &ctx,
                        obj,
                        Affine2D::IDENTITY,
                        1.0,
                        &mut emit_state,
                        &mut stream,
                        0,
                        &mut visited,
                    );
                }
            }
        }
        stream.push_str("Q\n");

        // Emit print marks outside the BleedBox
        if has_marks {
            emit_print_marks(
                &mut stream,
                page,
                page_index,
                margin_mm,
                bleed_mm,
                use_cmyk,
                opts,
            );
        }

        let gstates = emit_state.gstates;
        let used_images = emit_state.used_images;
        let page_shadings = emit_state.shadings;
        let page_spots = emit_state.used_spots;

        // Build ExtGState objects for this page
        let mut gstate_refs = String::new();
        for (key, gs_name) in &gstates {
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

        let mut resources =
            format!("<< /Font << /F1 {font_regular_id} 0 R /F2 {font_bold_id} 0 R >>");
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

        let page_dict = format!(
            "<< /Type /Page /Parent {pages_root_id} 0 R \
             /MediaBox [0 0 {media_w_pt:.4} {media_h_pt:.4}] \
             /TrimBox [{trim_x0:.4} {trim_y0:.4} {trim_x1:.4} {trim_y1:.4}] \
             /BleedBox [{bleed_x0:.4} {bleed_y0:.4} {bleed_x1:.4} {bleed_y1:.4}] \
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

    let catalog_dict = format!("<< /Type /Catalog /Pages {pages_root_id} 0 R >>");
    store.set(catalog_id, catalog_dict.into_bytes());

    let info_dict = format!(
        "<< /Title ({}) /Author ({}) /Creator (coree) /Producer (coree-io PDF 1.7 Writer) >>",
        escape_pdf_string(&doc.title),
        escape_pdf_string(&doc.metadata.author)
    );
    store.set(info_id, info_dict.into_bytes());

    Ok(store.serialize(catalog_id, info_id))
}

/// Atomically exports `doc` to a print-ready PDF 1.7 file at `path`.
pub fn export_pdf(doc: &Document, opts: &PdfExportOptions, path: &Path) -> IoResult<()> {
    let bytes = export_pdf_to_bytes(doc, opts)?;
    atomic_write_bytes(path, &bytes)
}

struct PageRenderCtx<'a> {
    doc: &'a Document,
    page: &'a Page,
    page_index: usize,
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
    text_engine: &'a mut TextEngine,
    gstates: BTreeMap<GStateKey, String>,
    used_images: BTreeMap<String, usize>,
    shadings: BTreeMap<String, usize>,
    used_spots: BTreeMap<String, usize>,
    spot_cache: &'a mut BTreeMap<String, (String, usize)>,
    dynamic_img_counter: &'a mut usize,
}

fn object_requires_raster_composite(obj: &SceneObject) -> bool {
    if obj.lens.is_some() {
        return true;
    }
    let has_raster_effect = obj.effects.iter().any(|e| {
        matches!(
            e,
            Effect::DropShadow { .. }
                | Effect::BlockShadow { .. }
                | Effect::Contour { .. }
                | Effect::Extrude25D { .. }
                | Effect::Blend { .. }
                | Effect::Transparency { .. }
        )
    });
    if has_raster_effect {
        return true;
    }
    matches!(
        obj.fill,
        Fill::ConicalGradient { .. }
            | Fill::SquareGradient { .. }
            | Fill::MeshFill { .. }
            | Fill::TwoColorPattern { .. }
            | Fill::VectorPattern { .. }
            | Fill::BitmapPattern { .. }
            | Fill::TextureFill { .. }
    )
}

fn has_geometry_effect(effects: &[Effect]) -> bool {
    effects.iter().any(|e| {
        matches!(
            e,
            Effect::Distort { .. } | Effect::Envelope { .. } | Effect::Perspective { .. }
        )
    })
}

fn emit_pdf_image_rect(
    ctx: &PageRenderCtx<'_>,
    im_name: &str,
    rect_mm: coree_core::Rect2D,
    opacity: f32,
    blend_mode: BlendMode,
    gstates: &mut BTreeMap<GStateKey, String>,
    out: &mut String,
) {
    let bl = Point2D::new(rect_mm.min_x, rect_mm.max_y);
    let br = Point2D::new(rect_mm.max_x, rect_mm.max_y);
    let tl = Point2D::new(rect_mm.min_x, rect_mm.min_y);
    let (x0, y0) = ctx.to_pdf_pt(bl);
    let (x1, y1) = ctx.to_pdf_pt(br);
    let (x2, y2) = ctx.to_pdf_pt(tl);
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
}

#[allow(clippy::too_many_arguments)]
fn emit_pdf_object(
    ctx: &PageRenderCtx<'_>,
    obj: &SceneObject,
    parent_xf: Affine2D,
    parent_opacity: f32,
    emit: &mut PdfPageEmit<'_>,
    out: &mut String,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) {
    if depth > MAX_PDF_DEPTH || !obj.visible || !obj.printable || !visited.insert(obj.id) {
        return;
    }
    let opacity = (parent_opacity * obj.opacity).clamp(0.0, 1.0);
    if !parent_xf.is_finite() || !obj.transform.is_finite() {
        visited.remove(&obj.id);
        return;
    }

    // Complex non-linear fills, shadows, blurs, extrusions, blends, transparencies, and live
    // optical lenses export cleanly at 300 DPI with SMask soft alpha so nothing is lost.
    if object_requires_raster_composite(obj)
        && !matches!(
            obj.payload,
            NodePayload::Group(_) | NodePayload::SymbolInstance(_)
        )
    {
        if let Ok(Some((rect_mm, rgba_buf))) =
            render_isolated_object_to_rgba(ctx.doc, ctx.page_index, obj.id, parent_xf, 300.0)
        {
            if let Ok(im_id) = add_rgba_bytes_xobject(
                emit.store,
                rgba_buf.width,
                rgba_buf.height,
                &rgba_buf.rgba,
                ctx.use_cmyk,
            ) {
                *emit.dynamic_img_counter += 1;
                let im_name = format!("DynIm{}", *emit.dynamic_img_counter);
                emit.used_images.insert(im_name.clone(), im_id);
                emit_pdf_image_rect(
                    ctx,
                    &im_name,
                    rect_mm,
                    parent_opacity,
                    obj.blend_mode,
                    &mut emit.gstates,
                    out,
                );

                // Preserve vector Spot /CutContour cutlines on top if present so plotters/RIPs see vector paths
                if matches!(obj.outline.color, Color::Spot { .. }) && obj.outline.is_visible() {
                    let sym_transforms = symmetry_effect_transforms(&obj.effects);
                    for sym_xf in sym_transforms {
                        let world_xf = parent_xf.compose(&sym_xf).compose(&obj.transform);
                        match &obj.payload {
                            NodePayload::Path(path) => {
                                let p = apply_geometry_effects(path, &obj.effects)
                                    .transformed(&world_xf);
                                emit_pdf_styled_path(
                                    ctx,
                                    &p,
                                    &Fill::None,
                                    &obj.outline,
                                    obj.blend_mode,
                                    opacity,
                                    Affine2D::IDENTITY,
                                    emit,
                                    out,
                                );
                            }
                            NodePayload::Shape(shape) => {
                                let p =
                                    apply_geometry_effects(&shape.to_bezier_path(), &obj.effects)
                                        .transformed(&world_xf);
                                emit_pdf_styled_path(
                                    ctx,
                                    &p,
                                    &Fill::None,
                                    &obj.outline,
                                    obj.blend_mode,
                                    opacity,
                                    Affine2D::IDENTITY,
                                    emit,
                                    out,
                                );
                            }
                            _ => {}
                        }
                    }
                }
                visited.remove(&obj.id);
                return;
            }
        }
    }

    let sym_transforms = symmetry_effect_transforms(&obj.effects);
    for sym_xf in sym_transforms {
        let effective_parent = parent_xf.compose(&sym_xf);
        let world_xf = effective_parent.compose(&obj.transform);
        if !world_xf.is_finite() {
            continue;
        }

        match &obj.payload {
            NodePayload::Path(path) => {
                let deformed = apply_geometry_effects(path, &obj.effects);
                let world_path = deformed.transformed(&world_xf);
                emit_pdf_styled_path(
                    ctx,
                    &world_path,
                    &obj.fill,
                    &obj.outline,
                    obj.blend_mode,
                    opacity,
                    world_xf,
                    emit,
                    out,
                );
            }
            NodePayload::Shape(shape) => {
                let deformed = apply_geometry_effects(&shape.to_bezier_path(), &obj.effects);
                let world_path = deformed.transformed(&world_xf);
                emit_pdf_styled_path(
                    ctx,
                    &world_path,
                    &obj.fill,
                    &obj.outline,
                    obj.blend_mode,
                    opacity,
                    world_xf,
                    emit,
                    out,
                );
            }
            NodePayload::ArtisticText(text) => {
                emit_pdf_artistic_text(
                    ctx,
                    text,
                    &obj.outline,
                    &obj.effects,
                    world_xf,
                    opacity,
                    obj.blend_mode,
                    emit,
                    out,
                );
            }
            NodePayload::ParagraphText(para) => {
                emit_pdf_paragraph_text(
                    ctx,
                    para,
                    &obj.outline,
                    &obj.effects,
                    world_xf,
                    opacity,
                    obj.blend_mode,
                    emit,
                    out,
                );
            }
            NodePayload::TextOnPath(top) => {
                if let Some(host_path) = ctx.doc.world_path_of(top.path_id) {
                    let pieces = emit.text_engine.layout_text_on_path_pieces(top, &host_path);
                    emit_pdf_shaped_pieces(
                        ctx,
                        &pieces,
                        &obj.outline,
                        effective_parent,
                        opacity,
                        obj.blend_mode,
                        emit,
                        out,
                    );
                } else {
                    emit_pdf_artistic_text(
                        ctx,
                        &top.text,
                        &obj.outline,
                        &obj.effects,
                        world_xf,
                        opacity,
                        obj.blend_mode,
                        emit,
                        out,
                    );
                }
            }
            NodePayload::Raster(raster) => {
                if let Some((im_name, im_id)) = ctx.image_xobjects.get(&raster.asset_id) {
                    emit.used_images.insert(im_name.clone(), *im_id);
                    let bl = world_xf.transform_point(Point2D::new(0.0, raster.height_mm));
                    let br =
                        world_xf.transform_point(Point2D::new(raster.width_mm, raster.height_mm));
                    let tl = world_xf.transform_point(Point2D::new(0.0, 0.0));
                    let (x0, y0) = ctx.to_pdf_pt(bl);
                    let (x1, y1) = ctx.to_pdf_pt(br);
                    let (x2, y2) = ctx.to_pdf_pt(tl);
                    let gs =
                        intern_gstate(&mut emit.gstates, opacity, opacity, obj.blend_mode, false);
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
                }
            }
            NodePayload::Table(table) => {
                for r in 0..table.rows {
                    for c in 0..table.cols {
                        if let (Some(cell), Some(rect)) = (table.cell(r, c), table.cell_rect(r, c))
                        {
                            let cell_path = BezierPath::from_rect(rect).transformed(&world_xf);
                            emit_pdf_styled_path(
                                ctx,
                                &cell_path,
                                &cell.fill,
                                &table.border_style,
                                obj.blend_mode,
                                opacity,
                                world_xf,
                                emit,
                                out,
                            );
                            if !cell.text.is_empty() {
                                let tx = Affine2D::translate(
                                    rect.min_x + cell.padding_mm,
                                    rect.min_y + cell.padding_mm,
                                );
                                let at = ArtisticText::from_plain(
                                    cell.text.clone(),
                                    cell.text_style.clone(),
                                );
                                emit_pdf_artistic_text(
                                    ctx,
                                    &at,
                                    &OutlineStyle::none(),
                                    &[],
                                    world_xf.compose(&tx),
                                    opacity,
                                    obj.blend_mode,
                                    emit,
                                    out,
                                );
                            }
                        }
                    }
                }
            }
            NodePayload::Dimension(dim) => {
                let line = BezierPath::from_line(
                    Point2D::new(dim.p1.x, dim.p1.y - dim.offset_mm),
                    Point2D::new(dim.p2.x, dim.p2.y - dim.offset_mm),
                )
                .transformed(&world_xf);
                emit_pdf_styled_path(
                    ctx,
                    &line,
                    &Fill::None,
                    &obj.outline,
                    obj.blend_mode,
                    opacity,
                    world_xf,
                    emit,
                    out,
                );
                let label =
                    dim.compute_measurement_with_dpi(ctx.doc.default_unit, f64::from(ctx.doc.dpi));
                let mid = dim.p1.midpoint(dim.p2);
                let at = ArtisticText::from_plain(label, dim.text_style.clone());
                emit_pdf_artistic_text(
                    ctx,
                    &at,
                    &OutlineStyle::none(),
                    &[],
                    world_xf.compose(&Affine2D::translate(mid.x, mid.y - dim.offset_mm - 2.0)),
                    opacity,
                    obj.blend_mode,
                    emit,
                    out,
                );
            }
            NodePayload::Connector(conn) => {
                let (a, b) = ctx.doc.resolve_connector_endpoints(conn);
                let path = conn.route_path(a, b);
                emit_pdf_styled_path(
                    ctx,
                    &path,
                    &Fill::None,
                    &obj.outline,
                    obj.blend_mode,
                    opacity,
                    world_xf,
                    emit,
                    out,
                );
            }
            NodePayload::Group(children) => {
                for &cid in children {
                    if let Some(child) = ctx.doc.objects.get(&cid) {
                        emit_pdf_object(
                            ctx,
                            child,
                            world_xf,
                            opacity,
                            emit,
                            out,
                            depth + 1,
                            visited,
                        );
                    }
                }
            }
            NodePayload::PowerClip {
                container,
                contents,
            } => {
                let clip_path =
                    apply_geometry_effects(container, &obj.effects).transformed(&world_xf);
                if obj.fill.is_visible() {
                    emit_pdf_styled_path(
                        ctx,
                        &clip_path,
                        &obj.fill,
                        &OutlineStyle::none(),
                        obj.blend_mode,
                        opacity,
                        world_xf,
                        emit,
                        out,
                    );
                }
                out.push_str("q\n");
                emit_pdf_path_ops(ctx, &clip_path, out);
                out.push_str("W n\n");
                for &cid in contents {
                    if let Some(child) = ctx.doc.objects.get(&cid) {
                        emit_pdf_object(
                            ctx,
                            child,
                            world_xf,
                            opacity,
                            emit,
                            out,
                            depth + 1,
                            visited,
                        );
                    }
                }
                out.push_str("Q\n");
                if obj.outline.is_visible() {
                    emit_pdf_styled_path(
                        ctx,
                        &clip_path,
                        &Fill::None,
                        &obj.outline,
                        obj.blend_mode,
                        opacity,
                        world_xf,
                        emit,
                        out,
                    );
                }
            }
            NodePayload::SymbolInstance(sym_id) => {
                if let Some(sym) = ctx.doc.symbols.get(sym_id) {
                    for sub in &sym.objects {
                        let mut sym_visited = HashSet::new();
                        emit_pdf_object(
                            ctx,
                            sub,
                            world_xf,
                            opacity,
                            emit,
                            out,
                            depth + 1,
                            &mut sym_visited,
                        );
                    }
                }
            }
        }
    }

    visited.remove(&obj.id);
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

    let fill_alpha = if shading_name.is_some() {
        fill.referenced_colors()
            .first()
            .map_or(opacity, |c| (c.alpha() * opacity).clamp(0.0, 1.0))
    } else {
        fill_color
            .as_ref()
            .map_or(1.0, |c| (c.alpha() * opacity).clamp(0.0, 1.0))
    };
    let stroke_alpha = (outline.color.alpha() * opacity).clamp(0.0, 1.0);

    out.push_str("q\n");
    if (fill_alpha - 1.0).abs() > 1e-3
        || (stroke_alpha - 1.0).abs() > 1e-3
        || blend_mode != BlendMode::Normal
        || outline.overprint
    {
        let gs = intern_gstate(
            &mut emit.gstates,
            fill_alpha,
            stroke_alpha,
            blend_mode,
            outline.overprint,
        );
        let _ = writeln!(out, "/{gs} gs");
    }

    if let Some(ref sh_name) = shading_name {
        if do_stroke && outline.behind_fill {
            emit_pdf_color(out, &outline.color, ctx.use_cmyk, true, emit);
            emit_pdf_stroke_state(out, outline);
            emit_pdf_path_ops(ctx, path, out);
            out.push_str("S\n");
            emit_pdf_arrowheads(ctx, path, outline, emit, out);
        }
        out.push_str("q\n");
        emit_pdf_path_ops(ctx, path, out);
        out.push_str("W n\n");
        let _ = writeln!(out, "/{sh_name} sh");
        out.push_str("Q\n");
        if do_stroke && !outline.behind_fill {
            emit_pdf_color(out, &outline.color, ctx.use_cmyk, true, emit);
            emit_pdf_stroke_state(out, outline);
            emit_pdf_path_ops(ctx, path, out);
            out.push_str("S\n");
            emit_pdf_arrowheads(ctx, path, outline, emit, out);
        }
    } else {
        if let Some(ref fc) = fill_color {
            emit_pdf_color(out, fc, ctx.use_cmyk, false, emit);
        }
        if do_stroke {
            emit_pdf_color(out, &outline.color, ctx.use_cmyk, true, emit);
            emit_pdf_stroke_state(out, outline);
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
        if do_stroke {
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
fn emit_pdf_artistic_text(
    ctx: &PageRenderCtx<'_>,
    text: &ArtisticText,
    outline: &OutlineStyle,
    effects: &[Effect],
    world_xf: Affine2D,
    opacity: f32,
    blend_mode: BlendMode,
    emit: &mut PdfPageEmit<'_>,
    out: &mut String,
) {
    let plain = text.plain_text();
    if plain.is_empty() || !world_xf.is_finite() {
        return;
    }
    let style = text
        .runs
        .first()
        .map(|r| r.style.clone())
        .unwrap_or_default();
    let is_simple_ascii_helvetica = !has_geometry_effect(effects)
        && (!outline.is_visible() || *outline == OutlineStyle::default())
        && text.runs.len() == 1
        && text.effective_direction() != TextDirection::Rtl
        && plain.chars().all(|ch| ch.is_ascii() && !ch.is_control())
        && !style.italic
        && !style.underline
        && !style.strikethrough
        && (style.font_family.trim().is_empty()
            || style.font_family.eq_ignore_ascii_case("Noto Sans")
            || style.font_family.eq_ignore_ascii_case("Helvetica")
            || style.font_family.eq_ignore_ascii_case("Arial"));

    if is_simple_ascii_helvetica {
        emit_pdf_ascii_type1_text(
            ctx, &plain, &style, world_xf, opacity, blend_mode, emit, out,
        );
        return;
    }

    let pieces = emit.text_engine.layout_artistic_pieces(text);
    if has_geometry_effect(effects) {
        let deformed_pieces: Vec<_> = pieces
            .into_iter()
            .map(|mut p| {
                p.path = apply_geometry_effects(&p.path, effects);
                p
            })
            .collect();
        emit_pdf_shaped_pieces(
            ctx,
            &deformed_pieces,
            outline,
            world_xf,
            opacity,
            blend_mode,
            emit,
            out,
        );
    } else {
        emit_pdf_shaped_pieces(
            ctx, &pieces, outline, world_xf, opacity, blend_mode, emit, out,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_pdf_paragraph_text(
    ctx: &PageRenderCtx<'_>,
    para: &coree_core::ParagraphText,
    outline: &OutlineStyle,
    effects: &[Effect],
    world_xf: Affine2D,
    opacity: f32,
    blend_mode: BlendMode,
    emit: &mut PdfPageEmit<'_>,
    out: &mut String,
) {
    let pieces = emit.text_engine.layout_paragraph_pieces(para);
    if has_geometry_effect(effects) {
        let deformed_pieces: Vec<_> = pieces
            .into_iter()
            .map(|mut p| {
                p.path = apply_geometry_effects(&p.path, effects);
                p
            })
            .collect();
        emit_pdf_shaped_pieces(
            ctx,
            &deformed_pieces,
            outline,
            world_xf,
            opacity,
            blend_mode,
            emit,
            out,
        );
    } else {
        emit_pdf_shaped_pieces(
            ctx, &pieces, outline, world_xf, opacity, blend_mode, emit, out,
        );
    }
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

#[allow(clippy::too_many_arguments)]
fn emit_pdf_ascii_type1_text(
    ctx: &PageRenderCtx<'_>,
    text: &str,
    style: &coree_core::TextStyle,
    world_xf: Affine2D,
    opacity: f32,
    blend_mode: BlendMode,
    emit: &mut PdfPageEmit<'_>,
    out: &mut String,
) {
    if text.is_empty() || !world_xf.is_finite() {
        return;
    }
    let alpha = (style.fill.alpha() * opacity).clamp(0.0, 1.0);
    let font_pt = if style.font_size_pt.is_finite() {
        style.font_size_pt.max(1.0)
    } else {
        12.0
    };
    let baseline_local = Point2D::new(0.0, style.font_size_mm());
    let origin = world_xf.transform_point(baseline_local);
    let (px, py) = ctx.to_pdf_pt(origin);
    let [a, b, c, d, _, _] = world_xf.m;

    out.push_str("q\n");
    if (alpha - 1.0).abs() > 1e-3 || blend_mode != BlendMode::Normal {
        let gs = intern_gstate(&mut emit.gstates, alpha, alpha, blend_mode, false);
        let _ = writeln!(out, "/{gs} gs");
    }
    emit_pdf_color(out, &style.fill, ctx.use_cmyk, false, emit);

    let font_ref = if style.bold { "F2" } else { "F1" };
    // Convert Y-down linear part `[a, b, c, d]` into Y-up PDF text matrix: `[a, -b, -c, d, px, py] Tm`.
    let is_pure_translate =
        (a - 1.0).abs() < 1e-6 && b.abs() < 1e-6 && c.abs() < 1e-6 && (d - 1.0).abs() < 1e-6;
    if is_pure_translate {
        let _ = writeln!(
            out,
            "BT /{font_ref} {font_pt:.2} Tf {px:.4} {py:.4} Td ({}) Tj ET",
            escape_pdf_string(text)
        );
    } else {
        let _ = writeln!(
            out,
            "BT /{font_ref} {font_pt:.2} Tf {a:.6} {:.6} {:.6} {d:.6} {px:.4} {py:.4} Tm ({}) Tj ET",
            -b,
            -c,
            escape_pdf_string(text)
        );
    }
    out.push_str("Q\n");
}

fn emit_pdf_path_ops(ctx: &PageRenderCtx<'_>, path: &BezierPath, out: &mut String) {
    let kb = path.to_kurbo();
    let mut cur = kurbo::Point::ZERO;
    let mut subpath_start = kurbo::Point::ZERO;
    let pt_ok = |p: kurbo::Point| p.x.is_finite() && p.y.is_finite();
    for el in kb.elements() {
        match *el {
            PathEl::MoveTo(p) if pt_ok(p) => {
                cur = p;
                subpath_start = p;
                let (x, y) = ctx.to_pdf_pt(Point2D::from_kurbo(p));
                let _ = writeln!(out, "{x:.4} {y:.4} m");
            }
            PathEl::LineTo(p) if pt_ok(p) => {
                cur = p;
                let (x, y) = ctx.to_pdf_pt(Point2D::from_kurbo(p));
                let _ = writeln!(out, "{x:.4} {y:.4} l");
            }
            PathEl::QuadTo(p1, p2) if pt_ok(cur) && pt_ok(p1) && pt_ok(p2) => {
                let cubic = kurbo::QuadBez::new(cur, p1, p2).raise();
                cur = p2;
                let (x1, y1) = ctx.to_pdf_pt(Point2D::from_kurbo(cubic.p1));
                let (x2, y2) = ctx.to_pdf_pt(Point2D::from_kurbo(cubic.p2));
                let (x3, y3) = ctx.to_pdf_pt(Point2D::from_kurbo(cubic.p3));
                let _ = writeln!(out, "{x1:.4} {y1:.4} {x2:.4} {y2:.4} {x3:.4} {y3:.4} c");
            }
            PathEl::CurveTo(p1, p2, p3) if pt_ok(p1) && pt_ok(p2) && pt_ok(p3) => {
                cur = p3;
                let (x1, y1) = ctx.to_pdf_pt(Point2D::from_kurbo(p1));
                let (x2, y2) = ctx.to_pdf_pt(Point2D::from_kurbo(p2));
                let (x3, y3) = ctx.to_pdf_pt(Point2D::from_kurbo(p3));
                let _ = writeln!(out, "{x1:.4} {y1:.4} {x2:.4} {y2:.4} {x3:.4} {y3:.4} c");
            }
            PathEl::ClosePath => {
                cur = subpath_start;
                out.push_str("h\n");
            }
            _ => {}
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

fn emit_pdf_stroke_state(out: &mut String, outline: &OutlineStyle) {
    let raw_w = outline.effective_width_mm();
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

fn emit_print_marks(
    out: &mut String,
    page: &Page,
    page_index: usize,
    margin_mm: f64,
    bleed_mm: f64,
    use_cmyk: bool,
    opts: &PdfExportOptions,
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
        let mode_str = if use_cmyk { "DeviceCMYK" } else { "DeviceRGB" };
        let slug = format!(
            "Page {} ({}) | {:.1}x{:.1}mm | Bleed {:.1}mm | {mode_str} | coree",
            page_index + 1,
            page.name,
            page.width_mm,
            page.height_mm,
            bleed_mm
        );
        if use_cmyk {
            out.push_str("0 0 0 1 k\n");
        } else {
            out.push_str("0 0 0 rg\n");
        }
        let slug_y = (ty0 - gap - 4.0 * MM_TO_PT).max(2.0);
        let _ = writeln!(
            out,
            "BT /F1 6 Tf {tx0:.4} {slug_y:.4} Td ({}) Tj ET",
            escape_pdf_string(&slug)
        );
    }

    out.push_str("Q\n");
}

fn add_raster_xobject(
    store: &mut PdfObjectStore,
    asset: &coree_core::RasterAsset,
    use_cmyk: bool,
) -> IoResult<usize> {
    add_rgba_bytes_xobject(
        store,
        asset.width,
        asset.height,
        &asset.rgba_bytes,
        use_cmyk,
    )
}

fn add_rgba_bytes_xobject(
    store: &mut PdfObjectStore,
    width: u32,
    height: u32,
    rgba_bytes: &[u8],
    use_cmyk: bool,
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
            let c = Color::rgba8(px[0], px[1], px[2], 255).to_cmyk();
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
        ArtisticText, CornerStyle, ParametricShape, RasterAsset, RasterLayerData, TextStyle,
    };

    #[test]
    fn pdf_1_7_cmyk_print_ready_multipage_with_marks_and_valid_xref() {
        let mut doc = Document::new_a4("Business Flyer");
        let p2 = doc.add_page("Back", 210.0, 297.0, 3.0);
        let p2_layer = doc.pages.iter().find(|p| p.id == p2).unwrap().layers[0].id;

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
        assert!(text.contains("Print Ready PDF"));
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
    }
}
