// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room `.cmx` 16-bit and 32-bit (`CMX1`/`CMX2`) RIFF/RIFX parser and
//! writer supporting `cont`, `ccmm`, `pack`, `page`, `rclr`, `rpen`, `rlin`,
//! `rstr`, `rimg`, `PolyCurve`, `BeginPage`/`BeginLayer`/`BeginGroup`,
//! `TextFrame`, and `DrawImage`.

use crate::cdr_riff::{
    decode_blend_mode, decode_curve_points, decode_dib_to_rgba_with_budget, encode_blend_mode,
    encode_curve_points, encode_dib_rgba, render_doc_mini_thumbnail_rgba,
};
use crate::coord::{
    cdr32_to_mm, cmx16_to_mm, cmx32_to_mm, contains_arabic, decode_cdr_text, mm_to_cdr32,
    mm_to_cmx16, mm_to_cmx32, read_tagged_color_with_endian, write_tagged_color_with_endian,
    CmxCoordUnit, CODEPAGE_UTF8,
};
use crate::error::{CdrError, CdrResult};
use crate::limits::{ByteReader, CodecLimits};
use crate::riff::{
    fourcc_to_string, parse_riff_with_budget, write_riff, RiffDocument, RiffEndian, RiffNode,
};
use crate::{CdrFormat, ImportReport};
use coree_core::{
    math::{sanitize_f32, sanitize_f64},
    Affine2D, ArtisticText, BezierPath, Color, ColorMode, Document, Fill, GradientStop,
    ImageAssetId, Layer, LayerId, LineCap, LineJoin, MasterLayerScope, NodePayload, ObjectId,
    OutlineStyle, Page, PageId, ParagraphText, Point2D, RasterAsset, RasterLayerData, SceneObject,
    SymbolDef, TextAlign, TextDirection, TextStyle, UnknownCdrChunk, Vec2D,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

// CMX Page Command IDs
const CMD_BEGIN_PAGE: u16 = 1;
const CMD_END_PAGE: u16 = 2;
const CMD_BEGIN_LAYER: u16 = 3;
const CMD_END_LAYER: u16 = 4;
const CMD_BEGIN_GROUP: u16 = 5;
const CMD_END_GROUP: u16 = 6;
const CMD_POLY_CURVE: u16 = 7;
const CMD_TEXT_FRAME: u16 = 10;
const CMD_DRAW_IMAGE: u16 = 11;
const CMD_EXT_OBJECT: u16 = 99;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CmxExtObject {
    source_oid: ObjectId,
    object: SceneObject,
    powerclip_children: Vec<SceneObject>,
    #[serde(default)]
    attaches_to_previous: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CmxSymbolTable {
    symbols: Vec<SymbolDef>,
}

/// Maximum number of entries addressable by a 16-bit CMX resource table index (`0..65535`).
const MAX_CMX_TABLE_ENTRIES: usize = u16::MAX as usize;

/// Coordinate bit-width precision for writing `.cmx` files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CmxPrecision {
    /// 16-bit signed integer coordinates (`CMX1`, `0.01 mm` unit).
    Bit16,
    /// 32-bit signed integer coordinates (`CMX2`, `0.1 µm` unit).
    #[default]
    Bit32,
}

/// Result of parsing a CMX file, including the underlying RIFF/RIFX tree for inspection.
#[derive(Debug, Clone)]
pub struct ParsedCmx {
    pub document: Document,
    pub precision: CmxPrecision,
    pub warnings: Vec<String>,
    pub riff_tree: RiffDocument,
}

#[derive(Debug, Clone)]
struct CmxPen {
    width_mm: f64,
    is_hairline: bool,
    behind_fill: bool,
    scale_with_object: bool,
    overprint: bool,
    miter_limit: f64,
    color_idx: u16,
}

#[derive(Debug, Clone)]
struct CmxLineStyle {
    cap: LineCap,
    join: LineJoin,
    dash_pattern: Vec<f64>,
}

/// Returns `true` if `form` is a recognized CMX RIFF form type (`CMX1` or `CMX2`).
#[must_use]
pub fn is_cmx_form_type(form: &[u8; 4]) -> bool {
    form == b"CMX1" || form == b"CMX2"
}

/// Parses a `.cmx` (16-bit or 32-bit RIFF/RIFX) container into a [`ParsedCmx`].
pub fn parse_cmx_bytes(bytes: &[u8], limits: &CodecLimits) -> CdrResult<ParsedCmx> {
    let mut budget_remaining = limits.max_uncompressed_bytes;
    let riff_tree = parse_riff_with_budget(bytes, limits, &mut budget_remaining, true)?;
    if !is_cmx_form_type(&riff_tree.form_type) {
        return Err(CdrError::UnsupportedFormat(format!(
            "RIFF form '{}' is not a .cmx container",
            fourcc_to_string(&riff_tree.form_type)
        )));
    }

    let endian = riff_tree.endian;
    let mut coord_bytes: u16 = if &riff_tree.form_type == b"CMX1" {
        2
    } else {
        4
    };
    let mut coord_unit = if coord_bytes == 2 {
        CmxCoordUnit::HundredthMm
    } else {
        CmxCoordUnit::TenthMicron
    };

    let mut doc = Document::new_a4("Imported CMX");
    doc.clear_pages_for_load();
    let mut warnings = Vec::new();

    // Parse header chunks (`cont`, `ccmm`, `pack`) and resource tables (`rclr`, `rpen`, `rlin`, `rstr`, `rimg`)
    let mut colors: Vec<Color> = Vec::new();
    let mut pens: Vec<CmxPen> = Vec::new();
    let mut lines: Vec<CmxLineStyle> = Vec::new();
    let mut strings: Vec<String> = Vec::new();
    let mut images: Vec<ImageAssetId> = Vec::new();
    let mut doc_page_w_mm = 210.0_f64;
    let mut doc_page_h_mm = 297.0_f64;
    let mut y_up = false;

    for node in &riff_tree.children {
        if let RiffNode::Chunk { fourcc, data, .. } = node {
            match fourcc {
                b"cont" => {
                    let mut r = ByteReader::new(data);
                    if data.len() >= 10 {
                        let _sig = r.read_fourcc("cont sig")?;
                        let cb = endian.read_u16(&mut r, "cont coord_size")?;
                        if cb == 2 || cb == 4 {
                            coord_bytes = cb;
                        }
                        let unit_tag = endian.read_u16(&mut r, "cont unit")?;
                        coord_unit = CmxCoordUnit::from_tag(unit_tag, coord_bytes);
                        let _page_count = endian.read_u16(&mut r, "cont page_count")?;
                        if r.remaining() >= 16 {
                            let x0 = cdr32_to_mm(endian.read_i32(&mut r, "cont x0")?);
                            let y0 = cdr32_to_mm(endian.read_i32(&mut r, "cont y0")?);
                            let x1 = cdr32_to_mm(endian.read_i32(&mut r, "cont x1")?);
                            let y1 = cdr32_to_mm(endian.read_i32(&mut r, "cont y1")?);
                            if (x1 - x0).abs() > 1.0 && (y1 - y0).abs() > 1.0 {
                                doc_page_w_mm = (x1 - x0).abs();
                                doc_page_h_mm = (y1 - y0).abs();
                                if x0 < -0.01 || y0 > y1 + 0.01 {
                                    y_up = true;
                                }
                            }
                        }
                        if r.remaining() >= 2 {
                            let tlen = usize::from(endian.read_u16(&mut r, "cont title_len")?);
                            if tlen <= r.remaining() {
                                let tb = r.read_bytes(tlen, "cont title")?;
                                let title = decode_cdr_text(tb, Some(CODEPAGE_UTF8), None);
                                if !title.is_empty() {
                                    doc.set_title(title);
                                }
                            }
                        }
                    }
                }
                b"ccmm" => {
                    let mut r = ByteReader::new(data);
                    if let Ok(cm) = r.read_u8("ccmm color_mode") {
                        doc.set_color_mode(if cm == 1 {
                            ColorMode::Rgb
                        } else {
                            ColorMode::Cmyk
                        });
                    }
                    if let Ok(dpi) = endian.read_f32(&mut r, "ccmm dpi") {
                        doc.set_dpi(dpi);
                    }
                }
                b"pack" | b"disp" | b"ixpg" | b"ixlr" | b"page" => {}
                b"symb" => {
                    if let Ok(st) = serde_json::from_slice::<CmxSymbolTable>(data) {
                        for sym in st.symbols {
                            doc.insert_symbol(sym);
                        }
                    }
                }
                b"rclr" => parse_rclr(data, endian, &mut colors, &mut warnings),
                b"rpen" => parse_rpen(data, endian, &mut pens, &mut warnings),
                b"rlin" => parse_rlin(data, endian, &mut lines, &mut warnings),
                b"rstr" => parse_rstr(data, endian, &mut strings, &mut warnings),
                b"rimg" => parse_rimg(
                    data,
                    endian,
                    limits,
                    &mut budget_remaining,
                    &mut doc,
                    &mut images,
                )?,
                _ => {
                    doc.push_unknown_doc_chunk(UnknownCdrChunk {
                        fourcc: *fourcc,
                        version_hint: u32::from(coord_bytes) * 8,
                        raw_bytes: data.clone(),
                    });
                }
            }
        }
    }

    let precision = if coord_bytes == 2 {
        CmxPrecision::Bit16
    } else {
        CmxPrecision::Bit32
    };
    doc.metadata_mut().source_format = Some(match precision {
        CmxPrecision::Bit16 => "CMX 16-bit".to_owned(),
        CmxPrecision::Bit32 => "CMX 32-bit".to_owned(),
    });

    // Second pass: parse `page` command stream chunks
    let res = CmxResources {
        colors: &colors,
        pens: &pens,
        lines: &lines,
        strings: &strings,
        images: &images,
        precision,
        unit: coord_unit,
        endian,
        y_up,
        default_page_w_mm: doc_page_w_mm,
        default_page_h_mm: doc_page_h_mm,
    };

    let mut oid_remap: HashMap<ObjectId, ObjectId> = HashMap::new();
    for node in &riff_tree.children {
        if let RiffNode::Chunk { fourcc, data, .. } = node {
            if fourcc == b"page" {
                parse_cmx_page_chunk(data, &res, limits, &mut doc, &mut oid_remap, &mut warnings)?;
            }
        }
    }

    if !oid_remap.is_empty() {
        let oids: Vec<ObjectId> = doc.objects().keys().copied().collect();
        for oid in oids {
            if let Some(obj) = doc.object_mut_for_load(oid) {
                match &mut obj.payload {
                    NodePayload::TextOnPath(ref mut top) => {
                        if let Some(&mapped) = oid_remap.get(&top.path_id) {
                            top.path_id = mapped;
                        }
                    }
                    NodePayload::Connector(ref mut conn) => {
                        if let Some(ref mut fid) = conn.from_obj {
                            if let Some(&mapped) = oid_remap.get(fid) {
                                *fid = mapped;
                            }
                        }
                        if let Some(ref mut tid) = conn.to_obj {
                            if let Some(&mapped) = oid_remap.get(tid) {
                                *tid = mapped;
                            }
                        }
                    }
                    NodePayload::ParagraphText(ref mut para) => {
                        if let Some(ref mut next_id) = para.linked_next_frame {
                            if let Some(&mapped) = oid_remap.get(next_id) {
                                *next_id = mapped;
                            }
                        }
                    }
                    _ => {}
                }
                for eff in &mut obj.effects {
                    if let coree_core::Effect::Blend {
                        ref mut target_id,
                        ref mut along_path,
                        ..
                    } = eff
                    {
                        if let Some(&mapped) = oid_remap.get(target_id) {
                            *target_id = mapped;
                        }
                        if let Some(ref mut path_oid) = along_path {
                            if let Some(&mapped) = oid_remap.get(path_oid) {
                                *path_oid = mapped;
                            }
                        }
                    }
                }
            }
        }
    }

    if !images.is_empty() {
        let mut asset_remap: HashMap<ImageAssetId, ImageAssetId> =
            HashMap::with_capacity(images.len());
        for (idx, &new_aid) in images.iter().enumerate() {
            if let Ok(idx_u16) = u16::try_from(idx) {
                asset_remap.insert(cmx_placeholder_asset_id(idx_u16), new_aid);
            }
        }
        let oids: Vec<ObjectId> = doc.objects().keys().copied().collect();
        for oid in oids {
            if let Some(obj) = doc.object_mut_for_load(oid) {
                remap_scene_object_assets_from_map(obj, &asset_remap);
            }
        }
        let syms: Vec<SymbolDef> = doc
            .symbols()
            .values()
            .cloned()
            .map(|mut sym| {
                for sub in &mut sym.objects {
                    remap_scene_object_assets_from_map(sub, &asset_remap);
                }
                sym
            })
            .collect();
        for sym in syms {
            doc.insert_symbol(sym);
        }
    }

    if doc.pages().is_empty() {
        let pid = PageId(doc.alloc_id());
        let lid = LayerId(doc.alloc_id());
        doc.push_loaded_page(Page::new(pid, "Page 1", 210.0, 297.0, 3.0, lid));
    }
    doc.refresh_document_palette();

    Ok(ParsedCmx {
        document: doc,
        precision,
        warnings,
        riff_tree,
    })
}

/// Imports a `.cmx` file from raw bytes into an [`ImportReport`].
pub fn import_cmx_bytes(bytes: &[u8], limits: &CodecLimits) -> CdrResult<ImportReport> {
    let parsed = parse_cmx_bytes(bytes, limits)?;
    let detected_format = match parsed.precision {
        CmxPrecision::Bit16 => CdrFormat::Cmx16,
        CmxPrecision::Bit32 => CdrFormat::Cmx32,
    };
    Ok(ImportReport {
        document: parsed.document,
        detected_format,
        warnings: parsed.warnings,
    })
}

struct CmxResources<'a> {
    colors: &'a [Color],
    pens: &'a [CmxPen],
    lines: &'a [CmxLineStyle],
    strings: &'a [String],
    images: &'a [ImageAssetId],
    precision: CmxPrecision,
    unit: CmxCoordUnit,
    endian: RiffEndian,
    y_up: bool,
    default_page_w_mm: f64,
    default_page_h_mm: f64,
}

impl CmxResources<'_> {
    fn color(&self, idx: u16) -> Color {
        self.colors
            .get(usize::from(idx))
            .cloned()
            .unwrap_or(Color::BLACK)
    }

    fn string(&self, idx: u16, fallback: &str) -> String {
        self.strings
            .get(usize::from(idx))
            .cloned()
            .unwrap_or_else(|| fallback.to_owned())
    }

    fn read_coord(&self, r: &mut ByteReader<'_>, ctx: &'static str) -> CdrResult<f64> {
        match self.precision {
            CmxPrecision::Bit16 => Ok(cmx16_to_mm(self.endian.read_i16(r, ctx)?, self.unit)),
            CmxPrecision::Bit32 => Ok(cmx32_to_mm(self.endian.read_i32(r, ctx)?, self.unit)),
        }
    }

    fn cmx_pt_to_page_mm(&self, x: f64, y: f64, pw: f64, ph: f64) -> Point2D {
        if self.y_up {
            Point2D::new(x + pw * 0.5, ph * 0.5 - y)
        } else {
            Point2D::new(x, y)
        }
    }

    fn cmx_affine_to_doc(&self, xf: Affine2D, pw: f64, ph: f64) -> Affine2D {
        if !self.y_up || xf == Affine2D::IDENTITY {
            return xf;
        }
        let ox = pw * 0.5;
        let oy = ph * 0.5;
        let [a, b, c, d, tx, ty] = xf.m;
        Affine2D::new(
            a,
            -b,
            -c,
            d,
            -a * ox + c * oy + tx + ox,
            oy + b * ox - d * oy - ty,
        )
    }
}

fn parse_rclr(data: &[u8], endian: RiffEndian, out: &mut Vec<Color>, warnings: &mut Vec<String>) {
    let mut r = ByteReader::new(data);
    let Ok(count) = endian.read_u16(&mut r, "rclr count") else {
        warnings.push("truncated rclr table".into());
        return;
    };
    for _ in 0..count {
        match read_tagged_color_with_endian(&mut r, endian, "rclr entry") {
            Ok(c) => out.push(c),
            Err(_) => break,
        }
    }
}

fn parse_rpen(data: &[u8], endian: RiffEndian, out: &mut Vec<CmxPen>, warnings: &mut Vec<String>) {
    let mut r = ByteReader::new(data);
    let Ok(count) = endian.read_u16(&mut r, "rpen count") else {
        warnings.push("truncated rpen table".into());
        return;
    };
    for _ in 0..count {
        let Ok(w_cdr) = endian.read_i32(&mut r, "rpen width") else {
            break;
        };
        let Ok(flags) = r.read_u8("rpen flags") else {
            break;
        };
        let Ok(miter) = endian.read_f64(&mut r, "rpen miter") else {
            break;
        };
        let Ok(color_idx) = endian.read_u16(&mut r, "rpen color_idx") else {
            break;
        };
        out.push(CmxPen {
            width_mm: cdr32_to_mm(w_cdr).max(0.0),
            is_hairline: (flags & 0x01) != 0,
            behind_fill: (flags & 0x02) != 0,
            scale_with_object: (flags & 0x04) != 0,
            overprint: (flags & 0x08) != 0,
            miter_limit: sanitize_f64(miter, 4.0).max(1.0),
            color_idx,
        });
    }
}

fn parse_rlin(
    data: &[u8],
    endian: RiffEndian,
    out: &mut Vec<CmxLineStyle>,
    warnings: &mut Vec<String>,
) {
    let mut r = ByteReader::new(data);
    let Ok(count) = endian.read_u16(&mut r, "rlin count") else {
        warnings.push("truncated rlin table".into());
        return;
    };
    for _ in 0..count {
        let Ok(cap_b) = r.read_u8("rlin cap") else {
            break;
        };
        let Ok(join_b) = r.read_u8("rlin join") else {
            break;
        };
        let Ok(dcount_raw) = endian.read_u16(&mut r, "rlin dash_count") else {
            break;
        };
        let dcount = usize::from(dcount_raw);
        if dcount > 256 || dcount.saturating_mul(4) > r.remaining() {
            warnings.push(format!(
                "truncated or excessive rlin dash_count {dcount}; stopping rlin parse"
            ));
            break;
        }
        let mut dashes = Vec::with_capacity(dcount);
        for _ in 0..dcount {
            if let Ok(d) = endian.read_i32(&mut r, "rlin dash") {
                dashes.push(cdr32_to_mm(d).max(0.0));
            }
        }
        out.push(CmxLineStyle {
            cap: match cap_b {
                1 => LineCap::Round,
                2 => LineCap::Square,
                _ => LineCap::Butt,
            },
            join: match join_b {
                1 => LineJoin::Round,
                2 => LineJoin::Bevel,
                _ => LineJoin::Miter,
            },
            dash_pattern: dashes,
        });
    }
}

fn parse_rstr(data: &[u8], endian: RiffEndian, out: &mut Vec<String>, warnings: &mut Vec<String>) {
    let mut r = ByteReader::new(data);
    let Ok(count) = endian.read_u16(&mut r, "rstr count") else {
        warnings.push("truncated rstr table".into());
        return;
    };
    for _ in 0..count {
        let Ok(cp) = endian.read_u16(&mut r, "rstr codepage") else {
            break;
        };
        let Ok(len) = endian.read_u16(&mut r, "rstr len") else {
            break;
        };
        let Ok(bytes) = r.read_bytes(usize::from(len), "rstr bytes") else {
            break;
        };
        out.push(decode_cdr_text(bytes, (cp != 0).then_some(cp), None));
    }
}

fn parse_rimg(
    data: &[u8],
    endian: RiffEndian,
    limits: &CodecLimits,
    budget_remaining: &mut u64,
    doc: &mut Document,
    out: &mut Vec<ImageAssetId>,
) -> CdrResult<()> {
    let mut r = ByteReader::new(data);
    let count = usize::from(endian.read_u16(&mut r, "rimg count")?);
    for _ in 0..count {
        let dpi_x100 = endian.read_u32(&mut r, "rimg dpi")?;
        let cm_b = r.read_u8("rimg cm")?;
        let dib_len = usize::try_from(endian.read_u32(&mut r, "rimg dib_len")?)
            .map_err(|_| CdrError::ArithmeticOverflow("rimg dib_len"))?;
        let dib = r.read_bytes(dib_len, "rimg dib")?;
        let (w, h, rgba) = decode_dib_to_rgba_with_budget(dib, limits, budget_remaining)?;
        let dpi = if dpi_x100 > 0 {
            (dpi_x100 as f32) / 100.0
        } else {
            300.0
        };
        let cm = if cm_b == 1 {
            ColorMode::Rgb
        } else {
            ColorMode::Cmyk
        };
        let asset = RasterAsset::new(w, h, rgba, dpi, cm)?;
        let aid = doc.register_asset(asset);
        out.push(aid);
    }
    Ok(())
}

struct OpenGroup {
    name: String,
    transform: Affine2D,
    children: Vec<ObjectId>,
}

fn flush_open_groups(
    group_stack: &mut Vec<OpenGroup>,
    current_page_idx: &mut Option<usize>,
    current_layer_id: &mut Option<LayerId>,
    doc: &mut Document,
) {
    while let Some(grp) = group_stack.pop() {
        let gid = finalize_group(grp, doc);
        attach_object(gid, group_stack, current_page_idx, current_layer_id, doc);
    }
}

fn parse_cmx_page_chunk(
    data: &[u8],
    res: &CmxResources<'_>,
    limits: &CodecLimits,
    doc: &mut Document,
    oid_remap: &mut HashMap<ObjectId, ObjectId>,
    warnings: &mut Vec<String>,
) -> CdrResult<()> {
    let endian = res.endian;
    let mut r = ByteReader::new(data);
    let mut current_page_idx: Option<usize> = None;
    let mut current_layer_id: Option<LayerId> = None;
    let mut current_pw = res.default_page_w_mm;
    let mut current_ph = res.default_page_h_mm;
    let mut last_attached_oid: Option<ObjectId> = None;
    let mut group_stack: Vec<OpenGroup> = Vec::new();
    let mut cmd_count = 0usize;

    while !r.is_eof() {
        limits.check_and_inc_chunks(&mut cmd_count)?;
        let cmd_id = endian.read_u16(&mut r, "cmx cmd_id")?;
        let cmd_len = usize::try_from(endian.read_u32(&mut r, "cmx cmd_len")?)
            .map_err(|_| CdrError::ArithmeticOverflow("cmx cmd_len"))?;
        let body = r.read_bytes(cmd_len, "cmx cmd body")?;
        let mut cr = ByteReader::new(body);

        match cmd_id {
            CMD_BEGIN_PAGE => {
                // Flush any unclosed groups belonging to the previous page/layer before switching!
                flush_open_groups(
                    &mut group_stack,
                    &mut current_page_idx,
                    &mut current_layer_id,
                    doc,
                );
                let w = cdr32_to_mm(endian.read_i32(&mut cr, "BeginPage w")?).max(1.0);
                let h = cdr32_to_mm(endian.read_i32(&mut cr, "BeginPage h")?).max(1.0);
                current_pw = w;
                current_ph = h;
                last_attached_oid = None;
                let bleed = cdr32_to_mm(endian.read_i32(&mut cr, "BeginPage bleed")?).max(0.0);
                let name_idx = endian.read_u16(&mut cr, "BeginPage name")?;
                let name = res.string(name_idx, &format!("Page {}", doc.pages().len() + 1));
                let pid = PageId(doc.alloc_id());
                doc.push_loaded_page(Page {
                    id: pid,
                    name,
                    width_mm: w,
                    height_mm: h,
                    bleed_mm: bleed,
                    facing_pages: false,
                    layers: Vec::new(),
                    unknown_chunks: Vec::new(),
                });
                current_page_idx = Some(doc.pages().len() - 1);
                current_layer_id = None;
            }
            CMD_END_PAGE => {
                flush_open_groups(
                    &mut group_stack,
                    &mut current_page_idx,
                    &mut current_layer_id,
                    doc,
                );
                if let Some(pidx) = current_page_idx {
                    if doc.pages()[pidx].layers.is_empty() {
                        let lid = LayerId(doc.alloc_id());
                        if let Some(p) = doc.page_mut(pidx) {
                            p.layers.push(Layer::new(lid, "Layer 1"));
                        }
                    }
                }
            }
            CMD_BEGIN_LAYER => {
                // Flush any unclosed groups belonging to the previous layer before switching!
                flush_open_groups(
                    &mut group_stack,
                    &mut current_page_idx,
                    &mut current_layer_id,
                    doc,
                );
                let pidx = match current_page_idx {
                    Some(idx) => idx,
                    None => {
                        let pid = PageId(doc.alloc_id());
                        let page_num = doc.pages().len() + 1;
                        doc.push_loaded_page(Page {
                            id: pid,
                            name: format!("Page {page_num}"),
                            width_mm: 210.0,
                            height_mm: 297.0,
                            bleed_mm: 3.0,
                            facing_pages: false,
                            layers: Vec::new(),
                            unknown_chunks: Vec::new(),
                        });
                        let idx = doc.pages().len() - 1;
                        current_page_idx = Some(idx);
                        idx
                    }
                };
                let flags = cr.read_u8("BeginLayer flags")?;
                let ms = cr.read_u8("BeginLayer master_scope")?;
                let name_idx = endian.read_u16(&mut cr, "BeginLayer name")?;
                let name = res.string(
                    name_idx,
                    &format!("Layer {}", doc.pages()[pidx].layers.len() + 1),
                );
                let lid = LayerId(doc.alloc_id());
                let mut layer = Layer::new(lid, name);
                layer.visible = (flags & 0x01) != 0;
                layer.locked = (flags & 0x02) != 0;
                layer.printable = (flags & 0x04) != 0;
                layer.is_cut_layer = (flags & 0x08) != 0;
                layer.master_scope = match ms {
                    1 => MasterLayerScope::AllPages,
                    2 => MasterLayerScope::OddPages,
                    3 => MasterLayerScope::EvenPages,
                    _ => MasterLayerScope::None,
                };
                if let Some(p) = doc.page_mut(pidx) {
                    p.layers.push(layer);
                }
                current_layer_id = Some(lid);
            }
            CMD_END_LAYER => {
                flush_open_groups(
                    &mut group_stack,
                    &mut current_page_idx,
                    &mut current_layer_id,
                    doc,
                );
            }
            CMD_BEGIN_GROUP => {
                limits.check_depth(group_stack.len() + 1)?;
                let name_idx = endian.read_u16(&mut cr, "BeginGroup name")?;
                let has_xf = cr.read_u8("BeginGroup has_xf")? != 0;
                let transform = if has_xf {
                    res.cmx_affine_to_doc(read_cmx_affine(&mut cr, endian)?, current_pw, current_ph)
                } else {
                    Affine2D::IDENTITY
                };
                group_stack.push(OpenGroup {
                    name: res.string(name_idx, "Group"),
                    transform,
                    children: Vec::new(),
                });
            }
            CMD_END_GROUP => {
                last_attached_oid = None;
                if let Some(grp) = group_stack.pop() {
                    let gid = finalize_group(grp, doc);
                    attach_object(
                        gid,
                        &mut group_stack,
                        &mut current_page_idx,
                        &mut current_layer_id,
                        doc,
                    );
                    last_attached_oid = Some(gid);
                }
            }
            CMD_POLY_CURVE => {
                let name_idx = endian.read_u16(&mut cr, "PolyCurve name")?;
                let flags = cr.read_u8("PolyCurve flags")?;
                let blend = decode_blend_mode(cr.read_u8("PolyCurve blend")?);
                let opacity = sanitize_f32(endian.read_f32(&mut cr, "PolyCurve opacity")?, 1.0)
                    .clamp(0.0, 1.0);
                let has_xf = cr.read_u8("PolyCurve has_xf")? != 0;
                let raw_transform = if has_xf {
                    read_cmx_affine(&mut cr, endian)?
                } else {
                    Affine2D::IDENTITY
                };
                let transform = res.cmx_affine_to_doc(raw_transform, current_pw, current_ph);

                let fill = read_cmx_fill(&mut cr, res, current_pw, current_ph)?;
                let has_outl = cr.read_u8("PolyCurve has_outl")? != 0;
                let outline = if has_outl {
                    let pen_idx = usize::from(endian.read_u16(&mut cr, "PolyCurve pen_idx")?);
                    let line_idx = usize::from(endian.read_u16(&mut cr, "PolyCurve line_idx")?);
                    resolve_cmx_outline(pen_idx, line_idx, res)
                } else {
                    OutlineStyle::none()
                };

                let pt_count = usize::try_from(endian.read_u32(&mut cr, "PolyCurve pt_count")?)
                    .map_err(|_| CdrError::ArithmeticOverflow("PolyCurve pt_count"))?;
                let per_pt = match res.precision {
                    CmxPrecision::Bit16 => 5usize,
                    CmxPrecision::Bit32 => 9usize,
                };
                let needed = pt_count
                    .checked_mul(per_pt)
                    .ok_or(CdrError::ArithmeticOverflow("PolyCurve bytes"))?;
                if needed > cr.remaining() {
                    return Err(CdrError::UnexpectedEof {
                        context: "CMX PolyCurve points",
                        needed,
                        available: cr.remaining(),
                    });
                }
                let mut pts = Vec::with_capacity(pt_count);
                for _ in 0..pt_count {
                    let x = res.read_coord(&mut cr, "PolyCurve x")?;
                    let y = res.read_coord(&mut cr, "PolyCurve y")?;
                    pts.push(res.cmx_pt_to_page_mm(x, y, current_pw, current_ph));
                }
                let node_flags = cr.read_bytes(pt_count, "PolyCurve flags")?;
                let path = decode_curve_points(&pts, node_flags);

                let oid = doc.alloc_object_id();
                let mut obj =
                    SceneObject::new(oid, res.string(name_idx, "Curve"), NodePayload::Path(path));
                obj.transform = transform;
                obj.fill = fill;
                obj.outline = outline;
                obj.blend_mode = blend;
                obj.opacity = opacity;
                obj.visible = (flags & 0x01) != 0;
                obj.locked = (flags & 0x02) != 0;
                obj.printable = (flags & 0x04) != 0;
                let _ = doc.insert_loaded_object(obj);
                attach_object(
                    oid,
                    &mut group_stack,
                    &mut current_page_idx,
                    &mut current_layer_id,
                    doc,
                );
                last_attached_oid = Some(oid);
            }
            CMD_TEXT_FRAME => {
                let name_idx = endian.read_u16(&mut cr, "TextFrame name")?;
                let is_para = cr.read_u8("TextFrame is_para")? != 0;
                let dir_b = cr.read_u8("TextFrame dir")?;
                let align_b = cr.read_u8("TextFrame align")?;
                let frame_w = res.read_coord(&mut cr, "TextFrame w")?.abs();
                let frame_h = res.read_coord(&mut cr, "TextFrame h")?.abs();
                let font_idx = endian.read_u16(&mut cr, "TextFrame font")?;
                let text_idx = endian.read_u16(&mut cr, "TextFrame text")?;
                let font_size_pt =
                    sanitize_f64(endian.read_f64(&mut cr, "TextFrame font_size")?, 18.0).max(1.0);
                let sflags = cr.read_u8("TextFrame style_flags")?;
                let clr_idx = endian.read_u16(&mut cr, "TextFrame color")?;
                let kashida = sanitize_f32(endian.read_f32(&mut cr, "TextFrame kashida")?, 0.0)
                    .clamp(0.0, 2.0);
                let has_xf = cr.read_u8("TextFrame has_xf")? != 0;
                let transform = if has_xf {
                    res.cmx_affine_to_doc(read_cmx_affine(&mut cr, endian)?, current_pw, current_ph)
                } else {
                    Affine2D::IDENTITY
                };

                let text_str = res.string(text_idx, "");
                let mut direction = match dir_b {
                    1 => TextDirection::Ltr,
                    2 => TextDirection::Rtl,
                    _ => TextDirection::Auto,
                };
                if contains_arabic(&text_str) && direction == TextDirection::Ltr {
                    direction = TextDirection::Rtl;
                }
                let align = match align_b {
                    1 => TextAlign::Center,
                    2 => TextAlign::Right,
                    3 => TextAlign::Justify,
                    4 => TextAlign::ForceJustifyKashida,
                    _ => TextAlign::Left,
                };
                let style = TextStyle {
                    font_family: res.string(font_idx, "Noto Sans"),
                    font_size_pt,
                    bold: (sflags & 0x01) != 0,
                    italic: (sflags & 0x02) != 0,
                    underline: (sflags & 0x04) != 0,
                    strikethrough: (sflags & 0x08) != 0,
                    fill: res.color(clr_idx),
                    direction,
                    align,
                    kashida_stretch: kashida,
                    ..TextStyle::default()
                };
                let payload = if is_para {
                    NodePayload::ParagraphText(ParagraphText::new(
                        text_str,
                        Vec2D::new(frame_w.max(10.0), frame_h.max(10.0)),
                        style,
                    ))
                } else {
                    NodePayload::ArtisticText(ArtisticText::from_plain(text_str, style))
                };
                let oid = doc.alloc_object_id();
                let mut obj = SceneObject::new(oid, res.string(name_idx, "Text"), payload);
                obj.transform = transform;
                obj.fill = Fill::None;
                obj.outline = OutlineStyle::none();
                let _ = doc.insert_loaded_object(obj);
                attach_object(
                    oid,
                    &mut group_stack,
                    &mut current_page_idx,
                    &mut current_layer_id,
                    doc,
                );
                last_attached_oid = Some(oid);
            }
            CMD_DRAW_IMAGE => {
                let name_idx = endian.read_u16(&mut cr, "DrawImage name")?;
                let img_idx = usize::from(endian.read_u16(&mut cr, "DrawImage img_idx")?);
                let w_mm = res.read_coord(&mut cr, "DrawImage w")?.abs();
                let h_mm = res.read_coord(&mut cr, "DrawImage h")?.abs();
                let has_xf = cr.read_u8("DrawImage has_xf")? != 0;
                let transform = if has_xf {
                    res.cmx_affine_to_doc(read_cmx_affine(&mut cr, endian)?, current_pw, current_ph)
                } else {
                    Affine2D::IDENTITY
                };
                if let Some(&asset_id) = res.images.get(img_idx) {
                    let oid = doc.alloc_object_id();
                    let mut obj = SceneObject::new(
                        oid,
                        res.string(name_idx, "Image"),
                        NodePayload::Raster(RasterLayerData {
                            asset_id,
                            width_mm: w_mm.max(1.0),
                            height_mm: h_mm.max(1.0),
                            filters: Vec::new(),
                            crop_rect: None,
                        }),
                    );
                    obj.transform = transform;
                    obj.fill = Fill::None;
                    obj.outline = OutlineStyle::none();
                    let _ = doc.insert_loaded_object(obj);
                    attach_object(
                        oid,
                        &mut group_stack,
                        &mut current_page_idx,
                        &mut current_layer_id,
                        doc,
                    );
                    last_attached_oid = Some(oid);
                } else {
                    last_attached_oid = None;
                    warnings.push(format!("DrawImage referenced missing rimg #{img_idx}"));
                }
            }
            CMD_EXT_OBJECT => {
                if let Ok(mut ext) = serde_json::from_slice::<CmxExtObject>(body) {
                    if !ext.powerclip_children.is_empty() {
                        let mut pc_remap = HashMap::with_capacity(ext.powerclip_children.len());
                        let mut inserted_pc_oids = Vec::with_capacity(ext.powerclip_children.len());
                        for mut child in ext.powerclip_children {
                            let src_cid = child.id;
                            let new_cid = doc.alloc_object_id();
                            child.id = new_cid;
                            pc_remap.insert(src_cid, new_cid);
                            oid_remap.insert(src_cid, new_cid);
                            let _ = doc.insert_loaded_object(child);
                            inserted_pc_oids.push(new_cid);
                        }
                        if let NodePayload::PowerClip { contents, .. } = &mut ext.object.payload {
                            for cid in contents.iter_mut() {
                                if let Some(&mapped) = pc_remap.get(cid) {
                                    *cid = mapped;
                                }
                            }
                        }
                        for pc_oid in inserted_pc_oids {
                            if let Some(child_obj) = doc.object_mut_for_load(pc_oid) {
                                match &mut child_obj.payload {
                                    NodePayload::Group(children) => {
                                        for cid in children.iter_mut() {
                                            if let Some(&mapped) = pc_remap.get(cid) {
                                                *cid = mapped;
                                            }
                                        }
                                    }
                                    NodePayload::PowerClip { contents, .. } => {
                                        for cid in contents.iter_mut() {
                                            if let Some(&mapped) = pc_remap.get(cid) {
                                                *cid = mapped;
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    let attach_target = if ext.attaches_to_previous {
                        last_attached_oid.take()
                    } else {
                        last_attached_oid = None;
                        None
                    };
                    if let Some(target_oid) = attach_target {
                        oid_remap.insert(ext.source_oid, target_oid);
                        ext.object.id = target_oid;
                        if let Some(slot) = doc.object_mut_for_load(target_oid) {
                            if let (NodePayload::Group(existing_children), NodePayload::Group(_)) =
                                (&slot.payload, &ext.object.payload)
                            {
                                let preserved_children = existing_children.clone();
                                *slot = ext.object;
                                slot.payload = NodePayload::Group(preserved_children);
                            } else {
                                if let (
                                    NodePayload::Raster(prev_r),
                                    NodePayload::Raster(ref mut new_r),
                                ) = (&slot.payload, &mut ext.object.payload)
                                {
                                    new_r.asset_id = prev_r.asset_id;
                                }
                                *slot = ext.object;
                            }
                        }
                    } else {
                        let new_oid = doc.alloc_object_id();
                        oid_remap.insert(ext.source_oid, new_oid);
                        ext.object.id = new_oid;
                        let _ = doc.insert_loaded_object(ext.object);
                        attach_object(
                            new_oid,
                            &mut group_stack,
                            &mut current_page_idx,
                            &mut current_layer_id,
                            doc,
                        );
                    }
                }
            }
            other => {
                warnings.push(format!("skipped unknown CMX command ID {other}"));
            }
        }
    }

    flush_open_groups(
        &mut group_stack,
        &mut current_page_idx,
        &mut current_layer_id,
        doc,
    );

    Ok(())
}

fn ensure_current_layer(
    page_idx: &mut Option<usize>,
    layer_id: &mut Option<LayerId>,
    doc: &mut Document,
) -> LayerId {
    if let Some(lid) = *layer_id {
        return lid;
    }
    let pidx = match *page_idx {
        Some(i) => i,
        None => {
            let pid = PageId(doc.alloc_id());
            let lid = LayerId(doc.alloc_id());
            doc.push_loaded_page(Page::new(pid, "Page 1", 210.0, 297.0, 3.0, lid));
            *page_idx = Some(doc.pages().len() - 1);
            *layer_id = Some(lid);
            return lid;
        }
    };
    let lid = LayerId(doc.alloc_id());
    if let Some(p) = doc.page_mut(pidx) {
        p.layers.push(Layer::new(lid, "Layer 1"));
    }
    *layer_id = Some(lid);
    lid
}

fn finalize_group(grp: OpenGroup, doc: &mut Document) -> ObjectId {
    let gid = doc.alloc_object_id();
    let mut obj = SceneObject::new(gid, grp.name, NodePayload::Group(grp.children));
    obj.transform = grp.transform;
    obj.fill = Fill::None;
    obj.outline = OutlineStyle::none();
    let _ = doc.insert_loaded_object(obj);
    gid
}

fn attach_object(
    oid: ObjectId,
    group_stack: &mut [OpenGroup],
    page_idx: &mut Option<usize>,
    layer_id: &mut Option<LayerId>,
    doc: &mut Document,
) {
    if let Some(parent) = group_stack.last_mut() {
        parent.children.push(oid);
    } else {
        let lid = ensure_current_layer(page_idx, layer_id, doc);
        if let Some(layer) = doc.find_layer_mut(lid) {
            layer.objects.push(oid);
        }
    }
}

fn read_cmx_affine(r: &mut ByteReader<'_>, endian: RiffEndian) -> CdrResult<Affine2D> {
    let a = sanitize_f64(endian.read_f64(r, "cmx xf a")?, 1.0);
    let b = sanitize_f64(endian.read_f64(r, "cmx xf b")?, 0.0);
    let c = sanitize_f64(endian.read_f64(r, "cmx xf c")?, 0.0);
    let d = sanitize_f64(endian.read_f64(r, "cmx xf d")?, 1.0);
    let tx = sanitize_f64(endian.read_f64(r, "cmx xf tx")?, 0.0);
    let ty = sanitize_f64(endian.read_f64(r, "cmx xf ty")?, 0.0);
    Ok(Affine2D::new(a, b, c, d, tx, ty))
}

fn read_cmx_fill(
    r: &mut ByteReader<'_>,
    res: &CmxResources<'_>,
    pw: f64,
    ph: f64,
) -> CdrResult<Fill> {
    let endian = res.endian;
    let kind = r.read_u8("cmx fill kind")?;
    match kind {
        0 => Ok(Fill::None),
        1 => {
            let clr_idx = endian.read_u16(r, "cmx solid fill clr")?;
            Ok(Fill::Solid(res.color(clr_idx)))
        }
        2..=5 => {
            let x0 = res.read_coord(r, "cmx grad x0")?;
            let y0 = res.read_coord(r, "cmx grad y0")?;
            let x1 = res.read_coord(r, "cmx grad x1")?;
            let y1 = res.read_coord(r, "cmx grad y1")?;
            let p0 = res.cmx_pt_to_page_mm(x0, y0, pw, ph);
            let p1 = res.cmx_pt_to_page_mm(x1, y1, pw, ph);
            let scalar = sanitize_f64(endian.read_f64(r, "cmx grad scalar")?, 0.0);
            let angle = sanitize_f64(endian.read_f64(r, "cmx grad angle")?, 0.0);
            let count = usize::from(endian.read_u16(r, "cmx grad stops")?);
            if count > 512 || count.saturating_mul(6) > r.remaining() {
                return Err(CdrError::InvalidChunk {
                    fourcc: "page".to_owned(),
                    reason: format!("invalid CMX gradient stop count {count}"),
                });
            }
            let mut stops = Vec::with_capacity(count);
            for _ in 0..count {
                let offset = sanitize_f32(endian.read_f32(r, "cmx stop off")?, 0.0);
                let cidx = endian.read_u16(r, "cmx stop clr")?;
                stops.push(GradientStop::new(offset, res.color(cidx)));
            }
            Ok(match kind {
                3 => Fill::RadialGradient {
                    center: p0,
                    radius: scalar.abs(),
                    focal: p1,
                    stops,
                },
                4 => Fill::ConicalGradient {
                    center: p0,
                    angle_deg: angle,
                    stops,
                },
                5 => Fill::SquareGradient {
                    center: p0,
                    size: scalar.abs(),
                    angle_deg: angle,
                    stops,
                },
                _ => Fill::LinearGradient {
                    start: p0,
                    end: p1,
                    stops,
                },
            })
        }
        _ => Ok(Fill::None),
    }
}

fn resolve_cmx_outline(pen_idx: usize, line_idx: usize, res: &CmxResources<'_>) -> OutlineStyle {
    let Some(pen) = res.pens.get(pen_idx) else {
        return OutlineStyle::none();
    };
    let line = res.lines.get(line_idx);
    let mut style = OutlineStyle::solid(res.color(pen.color_idx), pen.width_mm);
    style.is_hairline = pen.is_hairline;
    style.behind_fill = pen.behind_fill;
    style.scale_with_object = pen.scale_with_object;
    style.overprint = pen.overprint;
    style.miter_limit = pen.miter_limit;
    if let Some(ls) = line {
        style.cap = ls.cap;
        style.join = ls.join;
        style.dash_pattern = ls.dash_pattern.clone();
    }
    style
}

// =============================================================================
// CMX Writer (16-bit & 32-bit RIFF / RIFX)
// =============================================================================

struct CmxWriteTables {
    colors: Vec<Color>,
    pens: Vec<CmxPen>,
    lines: Vec<CmxLineStyle>,
    strings: Vec<String>,
    string_map: HashMap<String, u16>,
    images: Vec<(u32, ColorMode, Vec<u8>)>,
    image_map: HashMap<ImageAssetId, u16>,
    precision: CmxPrecision,
    unit: CmxCoordUnit,
    endian: RiffEndian,
    page_w_mm: f64,
    page_h_mm: f64,
}

impl CmxWriteTables {
    fn new(precision: CmxPrecision, endian: RiffEndian) -> Self {
        let unit = match precision {
            CmxPrecision::Bit16 => CmxCoordUnit::HundredthMm,
            CmxPrecision::Bit32 => CmxCoordUnit::TenthMicron,
        };
        Self {
            colors: Vec::new(),
            pens: Vec::new(),
            lines: Vec::new(),
            strings: Vec::new(),
            string_map: HashMap::new(),
            images: Vec::new(),
            image_map: HashMap::new(),
            precision,
            unit,
            endian,
            page_w_mm: 210.0,
            page_h_mm: 297.0,
        }
    }

    fn page_pt_to_cmx(&self, pt: Point2D) -> Point2D {
        Point2D::new(pt.x - self.page_w_mm * 0.5, self.page_h_mm * 0.5 - pt.y)
    }

    fn doc_affine_to_cmx(&self, xf: &Affine2D) -> Affine2D {
        if *xf == Affine2D::IDENTITY {
            return *xf;
        }
        let ox = self.page_w_mm * 0.5;
        let oy = self.page_h_mm * 0.5;
        let [a, b, c, d, tx, ty] = xf.m;
        Affine2D::new(
            a,
            -b,
            -c,
            d,
            a * ox + c * oy + tx - ox,
            oy - b * ox - d * oy - ty,
        )
    }

    fn intern_color(&mut self, c: &Color) -> CdrResult<u16> {
        if let Some(pos) = self.colors.iter().position(|x| x == c) {
            return Ok(pos as u16);
        }
        if self.colors.len() >= MAX_CMX_TABLE_ENTRIES {
            return Err(CdrError::ArithmeticOverflow("CMX rclr table overflow"));
        }
        let idx = self.colors.len() as u16;
        self.colors.push(c.clone());
        Ok(idx)
    }

    fn intern_string(&mut self, s: &str) -> CdrResult<u16> {
        if let Some(&idx) = self.string_map.get(s) {
            return Ok(idx);
        }
        if self.strings.len() >= MAX_CMX_TABLE_ENTRIES {
            return Err(CdrError::ArithmeticOverflow("CMX rstr table overflow"));
        }
        let idx = self.strings.len() as u16;
        self.strings.push(s.to_owned());
        self.string_map.insert(s.to_owned(), idx);
        Ok(idx)
    }

    fn intern_outline(&mut self, outl: &OutlineStyle) -> CdrResult<(u16, u16)> {
        let color_idx = self.intern_color(&outl.color)?;
        if self.pens.len() >= MAX_CMX_TABLE_ENTRIES {
            return Err(CdrError::ArithmeticOverflow("CMX rpen table overflow"));
        }
        if self.lines.len() >= MAX_CMX_TABLE_ENTRIES {
            return Err(CdrError::ArithmeticOverflow("CMX rlin table overflow"));
        }
        let pen_idx = self.pens.len() as u16;
        self.pens.push(CmxPen {
            width_mm: outl.width_mm,
            is_hairline: outl.is_hairline,
            behind_fill: outl.behind_fill,
            scale_with_object: outl.scale_with_object,
            overprint: outl.overprint,
            miter_limit: outl.miter_limit,
            color_idx,
        });
        let line_idx = self.lines.len() as u16;
        self.lines.push(CmxLineStyle {
            cap: outl.cap,
            join: outl.join,
            dash_pattern: outl.dash_pattern.clone(),
        });
        Ok((pen_idx, line_idx))
    }

    fn intern_image(&mut self, doc: &Document, aid: ImageAssetId) -> CdrResult<Option<u16>> {
        if let Some(&idx) = self.image_map.get(&aid) {
            return Ok(Some(idx));
        }
        let Some(asset) = doc.asset(aid) else {
            return Ok(None);
        };
        if self.images.len() >= MAX_CMX_TABLE_ENTRIES {
            return Err(CdrError::ArithmeticOverflow("CMX rimg table overflow"));
        }
        let dib = encode_dib_rgba(asset.width, asset.height, &asset.rgba_bytes)?;
        let dpi_x100 = (sanitize_f32(asset.source_dpi, 300.0).max(1.0) * 100.0).round() as u32;
        let idx = self.images.len() as u16;
        self.images.push((dpi_x100, asset.color_mode, dib));
        self.image_map.insert(aid, idx);
        Ok(Some(idx))
    }

    fn write_coord(&self, out: &mut Vec<u8>, mm: f64) {
        match self.precision {
            CmxPrecision::Bit16 => self.endian.write_i16(out, mm_to_cmx16(mm, self.unit)),
            CmxPrecision::Bit32 => self.endian.write_i32(out, mm_to_cmx32(mm, self.unit)),
        }
    }
}

/// Exports `doc` to little-endian `RIFF` `.cmx` bytes at the requested precision (`Bit16` or `Bit32`).
pub fn export_cmx_bytes(doc: &Document, precision: CmxPrecision) -> CdrResult<Vec<u8>> {
    export_cmx_bytes_with_endian(doc, precision, RiffEndian::Little)
}

/// Exports `doc` to `.cmx` bytes with explicit `RIFF` (little-endian) or `RIFX` (big-endian) byte order.
pub fn export_cmx_bytes_with_endian(
    doc: &Document,
    precision: CmxPrecision,
    endian: RiffEndian,
) -> CdrResult<Vec<u8>> {
    let mut tables = CmxWriteTables::new(precision, endian);
    for obj in doc.objects().values() {
        intern_scene_object_assets(obj, doc, &mut tables)?;
    }
    for sym in doc.symbols().values() {
        for sub in &sym.objects {
            intern_scene_object_assets(sub, doc, &mut tables)?;
        }
    }
    let referenced_oids = collect_referenced_oids(doc);
    let pages = doc.pages();
    let mut page_chunks: Vec<Vec<u8>> = Vec::with_capacity(pages.len());
    let mut ixpg_entries: Vec<(u16, u32, i32, i32)> = Vec::with_capacity(pages.len());
    let mut ixlr_entries: Vec<(u16, u16, u32, u16)> = Vec::new();

    for (pidx, page) in pages.iter().enumerate() {
        tables.page_w_mm = page.width_mm.max(1.0);
        tables.page_h_mm = page.height_mm.max(1.0);
        ixpg_entries.push((
            pidx as u16,
            0,
            mm_to_cdr32(page.width_mm),
            mm_to_cdr32(page.height_mm),
        ));
        let mut stream = Vec::new();

        // BeginPage
        let mut bp = Vec::with_capacity(14);
        endian.write_i32(&mut bp, mm_to_cdr32(page.width_mm));
        endian.write_i32(&mut bp, mm_to_cdr32(page.height_mm));
        endian.write_i32(&mut bp, mm_to_cdr32(page.bleed_mm));
        let pname_idx = tables.intern_string(&page.name)?;
        endian.write_u16(&mut bp, pname_idx);
        emit_cmd(&mut stream, endian, CMD_BEGIN_PAGE, &bp);

        for (lidx, layer) in page.layers.iter().enumerate() {
            let mut bl = Vec::with_capacity(4);
            let mut lflags = 0u8;
            if layer.visible {
                lflags |= 0x01;
            }
            if layer.locked {
                lflags |= 0x02;
            }
            if layer.printable {
                lflags |= 0x04;
            }
            if layer.is_cut_layer {
                lflags |= 0x08;
            }
            bl.push(lflags);
            bl.push(match layer.master_scope {
                MasterLayerScope::None => 0,
                MasterLayerScope::AllPages => 1,
                MasterLayerScope::OddPages => 2,
                MasterLayerScope::EvenPages => 3,
            });
            let lname_idx = tables.intern_string(&layer.name)?;
            endian.write_u16(&mut bl, lname_idx);
            ixlr_entries.push((pidx as u16, lidx as u16, u32::from(lflags), lname_idx));
            emit_cmd(&mut stream, endian, CMD_BEGIN_LAYER, &bl);

            for &oid in &layer.objects {
                emit_cmx_object(doc, oid, &referenced_oids, &mut tables, &mut stream, 0)?;
            }

            emit_cmd(&mut stream, endian, CMD_END_LAYER, &[]);
        }

        emit_cmd(&mut stream, endian, CMD_END_PAGE, &[]);
        page_chunks.push(stream);
    }

    let form_type = match precision {
        CmxPrecision::Bit16 => *b"CMX1",
        CmxPrecision::Bit32 => *b"CMX2",
    };
    let coord_bytes: u16 = match precision {
        CmxPrecision::Bit16 => 2,
        CmxPrecision::Bit32 => 4,
    };

    // Build `cont` (centered Y-up bounding box: [-pw/2, +ph/2, +pw/2, -ph/2])
    let mut cont = Vec::new();
    cont.extend_from_slice(&form_type);
    endian.write_u16(&mut cont, coord_bytes);
    endian.write_u16(&mut cont, tables.unit.to_tag());
    endian.write_u16(&mut cont, doc.pages().len().min(u16::MAX as usize) as u16);
    let (pw, ph) = doc
        .pages()
        .first()
        .map_or((210.0, 297.0), |p| (p.width_mm, p.height_mm));
    endian.write_i32(&mut cont, mm_to_cdr32(-pw * 0.5));
    endian.write_i32(&mut cont, mm_to_cdr32(ph * 0.5));
    endian.write_i32(&mut cont, mm_to_cdr32(pw * 0.5));
    endian.write_i32(&mut cont, mm_to_cdr32(-ph * 0.5));
    let tb = doc.title().as_bytes();
    let tlen = tb.len().min(u16::MAX as usize);
    endian.write_u16(&mut cont, tlen as u16);
    cont.extend_from_slice(&tb[..tlen]);

    // Build `ccmm`
    let mut ccmm = Vec::with_capacity(5);
    ccmm.push(if doc.color_mode() == ColorMode::Rgb {
        1
    } else {
        0
    });
    endian.write_f32(&mut ccmm, sanitize_f32(doc.dpi(), 300.0));

    // Build `pack`
    let mut pack = Vec::new();
    endian.write_u32(&mut pack, 0);
    let ver_b = b"coree-cmx";
    endian.write_u16(&mut pack, ver_b.len() as u16);
    pack.extend_from_slice(ver_b);

    // Build `disp` (rendered 64x64 DIB thumbnail preview of page 0)
    let thumb_rgba = render_doc_mini_thumbnail_rgba(doc, 64, 64);
    let disp_dib = encode_dib_rgba(64, 64, &thumb_rgba)?;

    // Build `ixpg` & `ixlr` index tables
    let mut ixpg = Vec::new();
    endian.write_u16(&mut ixpg, ixpg_entries.len().min(u16::MAX as usize) as u16);
    for (pidx, flg, w_c, h_c) in ixpg_entries {
        endian.write_u16(&mut ixpg, pidx);
        endian.write_u32(&mut ixpg, flg);
        endian.write_i32(&mut ixpg, w_c);
        endian.write_i32(&mut ixpg, h_c);
    }
    let mut ixlr = Vec::new();
    endian.write_u16(&mut ixlr, ixlr_entries.len().min(u16::MAX as usize) as u16);
    for (pidx, lidx, flg, nidx) in ixlr_entries {
        endian.write_u16(&mut ixlr, pidx);
        endian.write_u16(&mut ixlr, lidx);
        endian.write_u32(&mut ixlr, flg);
        endian.write_u16(&mut ixlr, nidx);
    }

    let mut nodes = vec![
        RiffNode::chunk(*b"cont", cont),
        RiffNode::chunk(*b"ccmm", ccmm),
        RiffNode::chunk(*b"pack", pack),
        RiffNode::chunk(*b"disp", disp_dib),
        RiffNode::chunk(*b"ixpg", ixpg),
        RiffNode::chunk(*b"ixlr", ixlr),
        RiffNode::chunk(*b"rclr", encode_rclr(&tables.colors, endian)),
        RiffNode::chunk(*b"rpen", encode_rpen(&tables.pens, endian)),
        RiffNode::chunk(*b"rlin", encode_rlin(&tables.lines, endian)),
        RiffNode::chunk(*b"rstr", encode_rstr(&tables.strings, endian)),
    ];
    if !doc.symbols().is_empty() {
        let mut symbols = Vec::with_capacity(doc.symbols().len());
        for sym in doc.symbols().values() {
            let mut sym_clone = sym.clone();
            for sub in &mut sym_clone.objects {
                rewrite_scene_object_assets_to_placeholders(sub, doc, &mut tables)?;
            }
            symbols.push(sym_clone);
        }
        let sym_tbl = CmxSymbolTable { symbols };
        if let Ok(sb) = serde_json::to_vec(&sym_tbl) {
            nodes.push(RiffNode::chunk(*b"symb", sb));
        }
    }
    if !tables.images.is_empty() {
        nodes.push(RiffNode::chunk(
            *b"rimg",
            encode_rimg(&tables.images, endian),
        ));
    }
    for pbytes in page_chunks {
        nodes.push(RiffNode::chunk(*b"page", pbytes));
    }
    for unk in doc.unknown_doc_chunks() {
        nodes.push(RiffNode::chunk(unk.fourcc, unk.raw_bytes.clone()));
    }

    write_riff(&RiffDocument {
        endian,
        form_type,
        children: nodes,
    })
}

/// Exports `doc` to `path` as a 32-bit `.cmx` file.
pub fn export_cmx(doc: &Document, path: &Path) -> CdrResult<()> {
    let bytes = export_cmx_bytes(doc, CmxPrecision::Bit32)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

fn emit_cmd(stream: &mut Vec<u8>, endian: RiffEndian, cmd_id: u16, body: &[u8]) {
    endian.write_u16(stream, cmd_id);
    endian.write_u32(stream, body.len() as u32);
    stream.extend_from_slice(body);
}

fn write_cmx_affine(out: &mut Vec<u8>, endian: RiffEndian, xf: &Affine2D) {
    if *xf == Affine2D::IDENTITY {
        out.push(0);
    } else {
        out.push(1);
        for v in xf.m {
            endian.write_f64(out, sanitize_f64(v, 0.0));
        }
    }
}

fn obj_needs_cmx_ext(obj: &SceneObject) -> bool {
    if !obj.effects.is_empty()
        || obj.lens.is_some()
        || obj.style_id.is_some()
        || !obj.unknown_chunks.is_empty()
        || matches!(
            obj.fill,
            Fill::TwoColorPattern { .. }
                | Fill::VectorPattern { .. }
                | Fill::BitmapPattern { .. }
                | Fill::TextureFill { .. }
                | Fill::MeshFill { .. }
        )
        || !matches!(obj.outline.start_arrow, coree_core::ArrowheadStyle::None)
        || !matches!(obj.outline.end_arrow, coree_core::ArrowheadStyle::None)
        || (obj.outline.calligraphy_stretch - 1.0).abs() > 1e-4
        || obj.outline.calligraphy_angle_deg.abs() > 1e-4
    {
        return true;
    }
    let has_non_default_obj_attrs = !obj.visible
        || obj.locked
        || !obj.printable
        || (obj.opacity - 1.0).abs() > 1e-4
        || obj.blend_mode != coree_core::BlendMode::Normal;
    match &obj.payload {
        NodePayload::Shape(_)
        | NodePayload::PowerClip { .. }
        | NodePayload::TextOnPath(_)
        | NodePayload::Table(_)
        | NodePayload::Dimension(_)
        | NodePayload::Connector(_)
        | NodePayload::SymbolInstance(_) => true,
        NodePayload::ArtisticText(t) => t.runs.len() > 1 || has_non_default_obj_attrs,
        NodePayload::ParagraphText(p) => {
            p.runs.len() > 1 || p.linked_next_frame.is_some() || has_non_default_obj_attrs
        }
        NodePayload::Raster(r) => {
            !r.filters.is_empty() || r.crop_rect.is_some() || has_non_default_obj_attrs
        }
        NodePayload::Group(_) => has_non_default_obj_attrs,
        NodePayload::Path(_) => false,
    }
}

fn collect_powerclip_descendants(
    doc: &Document,
    contents: &[ObjectId],
    visited: &mut HashSet<ObjectId>,
    out: &mut Vec<SceneObject>,
) {
    for &cid in contents {
        if !visited.insert(cid) {
            continue;
        }
        let Some(child) = doc.object(cid) else {
            continue;
        };
        out.push(child.clone());
        match &child.payload {
            NodePayload::Group(children) => {
                collect_powerclip_descendants(doc, children, visited, out);
            }
            NodePayload::PowerClip {
                contents: inner_contents,
                ..
            } => {
                collect_powerclip_descendants(doc, inner_contents, visited, out);
            }
            _ => {}
        }
    }
}

fn emit_cmx_ext_payload(
    doc: &Document,
    obj: &SceneObject,
    tables: &mut CmxWriteTables,
    attaches_to_previous: bool,
    stream: &mut Vec<u8>,
) -> CdrResult<()> {
    let mut powerclip_children = Vec::new();
    if let NodePayload::PowerClip { contents, .. } = &obj.payload {
        let mut visited = HashSet::new();
        visited.insert(obj.id);
        collect_powerclip_descendants(doc, contents, &mut visited, &mut powerclip_children);
    }
    let mut obj_clone = obj.clone();
    rewrite_scene_object_assets_to_placeholders(&mut obj_clone, doc, tables)?;
    for child in &mut powerclip_children {
        rewrite_scene_object_assets_to_placeholders(child, doc, tables)?;
    }
    let ext = CmxExtObject {
        source_oid: obj.id,
        object: obj_clone,
        powerclip_children,
        attaches_to_previous,
    };
    if let Ok(jb) = serde_json::to_vec(&ext) {
        emit_cmd(stream, tables.endian, CMD_EXT_OBJECT, &jb);
    }
    Ok(())
}

const CMX_ASSET_PLACEHOLDER_PREFIX: u128 = 0xC0EE_0000_0000_0000_0000_0000_0000_0000;

fn cmx_placeholder_asset_id(img_idx: u16) -> ImageAssetId {
    ImageAssetId(uuid::Uuid::from_u128(
        CMX_ASSET_PLACEHOLDER_PREFIX | u128::from(img_idx),
    ))
}

fn intern_scene_object_assets(
    obj: &SceneObject,
    doc: &Document,
    tables: &mut CmxWriteTables,
) -> CdrResult<()> {
    match &obj.payload {
        NodePayload::Raster(r) => {
            let _ = tables.intern_image(doc, r.asset_id)?;
        }
        NodePayload::Table(table) => {
            for cell in &table.cells {
                if let Fill::BitmapPattern { image_id, .. } = cell.fill {
                    let _ = tables.intern_image(doc, image_id)?;
                }
            }
        }
        _ => {}
    }
    if let Fill::BitmapPattern { image_id, .. } = obj.fill {
        let _ = tables.intern_image(doc, image_id)?;
    }
    Ok(())
}

fn rewrite_scene_object_assets_to_placeholders(
    obj: &mut SceneObject,
    doc: &Document,
    tables: &mut CmxWriteTables,
) -> CdrResult<()> {
    match &mut obj.payload {
        NodePayload::Raster(ref mut r) => {
            if let Some(idx) = tables.intern_image(doc, r.asset_id)? {
                r.asset_id = cmx_placeholder_asset_id(idx);
            }
        }
        NodePayload::Table(ref mut table) => {
            for cell in &mut table.cells {
                if let Fill::BitmapPattern {
                    ref mut image_id, ..
                } = cell.fill
                {
                    if let Some(idx) = tables.intern_image(doc, *image_id)? {
                        *image_id = cmx_placeholder_asset_id(idx);
                    }
                }
            }
        }
        _ => {}
    }
    if let Fill::BitmapPattern {
        ref mut image_id, ..
    } = obj.fill
    {
        if let Some(idx) = tables.intern_image(doc, *image_id)? {
            *image_id = cmx_placeholder_asset_id(idx);
        }
    }
    Ok(())
}

fn remap_scene_object_assets_from_map(
    obj: &mut SceneObject,
    asset_remap: &HashMap<ImageAssetId, ImageAssetId>,
) {
    match &mut obj.payload {
        NodePayload::Raster(ref mut r) => {
            if let Some(&mapped) = asset_remap.get(&r.asset_id) {
                r.asset_id = mapped;
            }
        }
        NodePayload::Table(ref mut table) => {
            for cell in &mut table.cells {
                if let Fill::BitmapPattern {
                    ref mut image_id, ..
                } = cell.fill
                {
                    if let Some(&mapped) = asset_remap.get(image_id) {
                        *image_id = mapped;
                    }
                }
            }
        }
        _ => {}
    }
    if let Fill::BitmapPattern {
        ref mut image_id, ..
    } = obj.fill
    {
        if let Some(&mapped) = asset_remap.get(image_id) {
            *image_id = mapped;
        }
    }
}

fn collect_referenced_oids(doc: &Document) -> HashSet<ObjectId> {
    let mut out = HashSet::new();
    for obj in doc.objects().values() {
        match &obj.payload {
            NodePayload::TextOnPath(top) => {
                out.insert(top.path_id);
            }
            NodePayload::Connector(conn) => {
                if let Some(fid) = conn.from_obj {
                    out.insert(fid);
                }
                if let Some(tid) = conn.to_obj {
                    out.insert(tid);
                }
            }
            NodePayload::ParagraphText(para) => {
                if let Some(next_id) = para.linked_next_frame {
                    out.insert(next_id);
                }
            }
            _ => {}
        }
        for eff in &obj.effects {
            if let coree_core::Effect::Blend {
                target_id,
                along_path,
                ..
            } = eff
            {
                out.insert(*target_id);
                if let Some(path_oid) = along_path {
                    out.insert(*path_oid);
                }
            }
        }
    }
    out
}

fn emit_cmx_object(
    doc: &Document,
    oid: ObjectId,
    referenced_oids: &HashSet<ObjectId>,
    tables: &mut CmxWriteTables,
    stream: &mut Vec<u8>,
    depth: usize,
) -> CdrResult<()> {
    if depth > crate::limits::MAX_TREE_DEPTH {
        return Err(CdrError::DepthLimitExceeded {
            depth,
            limit: crate::limits::MAX_TREE_DEPTH,
        });
    }
    let Some(obj) = doc.object(oid) else {
        return Ok(());
    };
    let endian = tables.endian;
    let mut emitted_base_cmd = true;

    match &obj.payload {
        NodePayload::Group(children) => {
            let mut bg = Vec::new();
            let name_idx = tables.intern_string(&obj.name)?;
            endian.write_u16(&mut bg, name_idx);
            let cmx_xf = tables.doc_affine_to_cmx(&obj.transform);
            write_cmx_affine(&mut bg, endian, &cmx_xf);
            emit_cmd(stream, endian, CMD_BEGIN_GROUP, &bg);
            for &cid in children {
                emit_cmx_object(doc, cid, referenced_oids, tables, stream, depth + 1)?;
            }
            emit_cmd(stream, endian, CMD_END_GROUP, &[]);
        }
        NodePayload::Path(path) => {
            emit_cmx_polycurve(obj, path, tables, stream)?;
        }
        NodePayload::Shape(shape) => {
            emit_cmx_polycurve(obj, &shape.to_bezier_path(), tables, stream)?;
        }
        NodePayload::PowerClip { container, .. } => {
            emit_cmx_polycurve(obj, container, tables, stream)?;
        }
        NodePayload::ArtisticText(text) => {
            let style = text
                .runs
                .first()
                .map_or_else(TextStyle::default, |r| r.style.clone());
            emit_cmx_text(
                obj,
                false,
                Vec2D::ZERO,
                text.direction,
                text.align,
                &text.plain_text(),
                &style,
                tables,
                stream,
            )?;
        }
        NodePayload::ParagraphText(para) => {
            let style = para
                .runs
                .first()
                .map_or_else(TextStyle::default, |r| r.style.clone());
            emit_cmx_text(
                obj,
                true,
                para.frame_size,
                para.direction,
                para.align,
                &para.plain_text(),
                &style,
                tables,
                stream,
            )?;
        }
        NodePayload::TextOnPath(top) => {
            let style = top
                .text
                .runs
                .first()
                .map_or_else(TextStyle::default, |r| r.style.clone());
            emit_cmx_text(
                obj,
                false,
                Vec2D::ZERO,
                top.text.direction,
                top.text.align,
                &top.text.plain_text(),
                &style,
                tables,
                stream,
            )?;
        }
        NodePayload::Raster(r) => {
            if let Some(img_idx) = tables.intern_image(doc, r.asset_id)? {
                let mut di = Vec::new();
                let name_idx = tables.intern_string(&obj.name)?;
                endian.write_u16(&mut di, name_idx);
                endian.write_u16(&mut di, img_idx);
                tables.write_coord(&mut di, r.width_mm);
                tables.write_coord(&mut di, r.height_mm);
                let cmx_xf = tables.doc_affine_to_cmx(&obj.transform);
                write_cmx_affine(&mut di, endian, &cmx_xf);
                emit_cmd(stream, endian, CMD_DRAW_IMAGE, &di);
            } else {
                emitted_base_cmd = false;
            }
        }
        NodePayload::Table(t) => {
            emit_cmx_polycurve(obj, &t.to_grid_paths(), tables, stream)?;
        }
        NodePayload::Dimension(d) => {
            emit_cmx_polycurve(
                obj,
                &BezierPath::from_rect(d.bounding_box()),
                tables,
                stream,
            )?;
        }
        NodePayload::Connector(c) => {
            let (a, b) = doc.resolve_connector_endpoints(c);
            emit_cmx_polycurve(obj, &c.route_path(a, b), tables, stream)?;
        }
        NodePayload::SymbolInstance(_) => {
            emitted_base_cmd = false;
        }
    }
    if obj_needs_cmx_ext(obj) || referenced_oids.contains(&obj.id) {
        emit_cmx_ext_payload(doc, obj, tables, emitted_base_cmd, stream)?;
    }
    Ok(())
}

fn emit_cmx_polycurve(
    obj: &SceneObject,
    path: &BezierPath,
    tables: &mut CmxWriteTables,
    stream: &mut Vec<u8>,
) -> CdrResult<()> {
    let endian = tables.endian;
    let mut b = Vec::new();
    let name_idx = tables.intern_string(&obj.name)?;
    endian.write_u16(&mut b, name_idx);

    let mut flags = 0u8;
    if obj.visible {
        flags |= 0x01;
    }
    if obj.locked {
        flags |= 0x02;
    }
    if obj.printable {
        flags |= 0x04;
    }
    b.push(flags);
    b.push(encode_blend_mode(obj.blend_mode));
    endian.write_f32(&mut b, sanitize_f32(obj.opacity, 1.0));
    let cmx_xf = tables.doc_affine_to_cmx(&obj.transform);
    write_cmx_affine(&mut b, endian, &cmx_xf);

    // Fill
    match &obj.fill {
        Fill::Solid(c) => {
            b.push(1);
            let idx = tables.intern_color(c)?;
            endian.write_u16(&mut b, idx);
        }
        Fill::LinearGradient { start, end, stops } => {
            b.push(2);
            write_cmx_grad(&mut b, *start, *end, 0.0, 0.0, stops, tables)?;
        }
        Fill::RadialGradient {
            center,
            radius,
            focal,
            stops,
        } => {
            b.push(3);
            write_cmx_grad(&mut b, *center, *focal, *radius, 0.0, stops, tables)?;
        }
        Fill::ConicalGradient {
            center,
            angle_deg,
            stops,
        } => {
            b.push(4);
            write_cmx_grad(
                &mut b,
                *center,
                Point2D::ZERO,
                0.0,
                *angle_deg,
                stops,
                tables,
            )?;
        }
        Fill::SquareGradient {
            center,
            size,
            angle_deg,
            stops,
        } => {
            b.push(5);
            write_cmx_grad(
                &mut b,
                *center,
                Point2D::ZERO,
                *size,
                *angle_deg,
                stops,
                tables,
            )?;
        }
        _ => b.push(0),
    }

    // Outline
    if obj.outline.is_visible() {
        b.push(1);
        let (pidx, lidx) = tables.intern_outline(&obj.outline)?;
        endian.write_u16(&mut b, pidx);
        endian.write_u16(&mut b, lidx);
    } else {
        b.push(0);
    }

    // Geometry points + node flags (converted to centered Y-up CMX coordinates)
    let (pts, node_flags) = encode_curve_points(path);
    endian.write_u32(&mut b, pts.len() as u32);
    for pt in &pts {
        let cmx_pt = tables.page_pt_to_cmx(*pt);
        tables.write_coord(&mut b, cmx_pt.x);
        tables.write_coord(&mut b, cmx_pt.y);
    }
    b.extend_from_slice(&node_flags);

    emit_cmd(stream, endian, CMD_POLY_CURVE, &b);
    Ok(())
}

fn write_cmx_grad(
    out: &mut Vec<u8>,
    p0: Point2D,
    p1: Point2D,
    scalar: f64,
    angle: f64,
    stops: &[GradientStop],
    tables: &mut CmxWriteTables,
) -> CdrResult<()> {
    let endian = tables.endian;
    let cp0 = tables.page_pt_to_cmx(p0);
    let cp1 = tables.page_pt_to_cmx(p1);
    tables.write_coord(out, cp0.x);
    tables.write_coord(out, cp0.y);
    tables.write_coord(out, cp1.x);
    tables.write_coord(out, cp1.y);
    endian.write_f64(out, sanitize_f64(scalar, 0.0));
    endian.write_f64(out, sanitize_f64(angle, 0.0));
    let cnt = stops.len().min(u16::MAX as usize);
    endian.write_u16(out, cnt as u16);
    for s in &stops[..cnt] {
        endian.write_f32(out, sanitize_f32(s.offset, 0.0));
        let cidx = tables.intern_color(&s.color)?;
        endian.write_u16(out, cidx);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_cmx_text(
    obj: &SceneObject,
    is_para: bool,
    frame_size: Vec2D,
    direction: TextDirection,
    align: TextAlign,
    text: &str,
    style: &TextStyle,
    tables: &mut CmxWriteTables,
    stream: &mut Vec<u8>,
) -> CdrResult<()> {
    let endian = tables.endian;
    let mut b = Vec::new();
    let name_idx = tables.intern_string(&obj.name)?;
    endian.write_u16(&mut b, name_idx);
    b.push(u8::from(is_para));
    b.push(match direction {
        TextDirection::Auto => 0,
        TextDirection::Ltr => 1,
        TextDirection::Rtl => 2,
    });
    b.push(match align {
        TextAlign::Left => 0,
        TextAlign::Center => 1,
        TextAlign::Right => 2,
        TextAlign::Justify => 3,
        TextAlign::ForceJustifyKashida => 4,
    });
    tables.write_coord(&mut b, frame_size.x);
    tables.write_coord(&mut b, frame_size.y);
    let font_idx = tables.intern_string(&style.font_family)?;
    let text_idx = tables.intern_string(text)?;
    endian.write_u16(&mut b, font_idx);
    endian.write_u16(&mut b, text_idx);
    endian.write_f64(&mut b, sanitize_f64(style.font_size_pt, 18.0));
    let mut sflags = 0u8;
    if style.bold {
        sflags |= 0x01;
    }
    if style.italic {
        sflags |= 0x02;
    }
    if style.underline {
        sflags |= 0x04;
    }
    if style.strikethrough {
        sflags |= 0x08;
    }
    b.push(sflags);
    let cidx = tables.intern_color(&style.fill)?;
    endian.write_u16(&mut b, cidx);
    endian.write_f32(&mut b, sanitize_f32(style.kashida_stretch, 0.0));
    let cmx_xf = tables.doc_affine_to_cmx(&obj.transform);
    write_cmx_affine(&mut b, endian, &cmx_xf);
    emit_cmd(stream, endian, CMD_TEXT_FRAME, &b);
    Ok(())
}

fn encode_rclr(colors: &[Color], endian: RiffEndian) -> Vec<u8> {
    let mut out = Vec::new();
    endian.write_u16(&mut out, colors.len() as u16);
    for c in colors {
        write_tagged_color_with_endian(&mut out, c, endian);
    }
    out
}

fn encode_rpen(pens: &[CmxPen], endian: RiffEndian) -> Vec<u8> {
    let mut out = Vec::new();
    endian.write_u16(&mut out, pens.len() as u16);
    for p in pens {
        endian.write_i32(&mut out, mm_to_cdr32(p.width_mm));
        let mut flags = 0u8;
        if p.is_hairline {
            flags |= 0x01;
        }
        if p.behind_fill {
            flags |= 0x02;
        }
        if p.scale_with_object {
            flags |= 0x04;
        }
        if p.overprint {
            flags |= 0x08;
        }
        out.push(flags);
        endian.write_f64(&mut out, sanitize_f64(p.miter_limit, 4.0));
        endian.write_u16(&mut out, p.color_idx);
    }
    out
}

fn encode_rlin(lines: &[CmxLineStyle], endian: RiffEndian) -> Vec<u8> {
    let mut out = Vec::new();
    endian.write_u16(&mut out, lines.len() as u16);
    for l in lines {
        out.push(match l.cap {
            LineCap::Butt => 0,
            LineCap::Round => 1,
            LineCap::Square => 2,
        });
        out.push(match l.join {
            LineJoin::Miter => 0,
            LineJoin::Round => 1,
            LineJoin::Bevel => 2,
        });
        let dc = l.dash_pattern.len().min(256);
        endian.write_u16(&mut out, dc as u16);
        for &d in &l.dash_pattern[..dc] {
            endian.write_i32(&mut out, mm_to_cdr32(d));
        }
    }
    out
}

fn encode_rstr(strings: &[String], endian: RiffEndian) -> Vec<u8> {
    let mut out = Vec::new();
    endian.write_u16(&mut out, strings.len() as u16);
    for s in strings {
        endian.write_u16(&mut out, CODEPAGE_UTF8);
        let sb = s.as_bytes();
        let len = sb.len().min(u16::MAX as usize);
        endian.write_u16(&mut out, len as u16);
        out.extend_from_slice(&sb[..len]);
    }
    out
}

fn encode_rimg(images: &[(u32, ColorMode, Vec<u8>)], endian: RiffEndian) -> Vec<u8> {
    let mut out = Vec::new();
    endian.write_u16(&mut out, images.len() as u16);
    for (dpi_x100, cm, dib) in images {
        endian.write_u32(&mut out, *dpi_x100);
        out.push(if *cm == ColorMode::Rgb { 1 } else { 0 });
        endian.write_u32(&mut out, dib.len() as u32);
        out.extend_from_slice(dib);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        AnchorPoint, ArrowheadStyle, ConnectorObject, ConnectorStyle, CornerStyle, DimensionKind,
        DimensionObject, Effect, FeatherDirection, LensConfig, LensKind, ParametricShape, Rect2D,
        VectorTable,
    };

    #[test]
    fn cmx_emits_disp_ixpg_ixlr_and_roundtrips_y_up_coords_riff_and_rifx() {
        let mut doc = Document::new_a4("CMX Y-Up Test");
        let lid = doc.pages()[0].layers[0].id;
        let rect = SceneObject::new(
            ObjectId(0),
            "Box",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                20.0, 40.0, 60.0, 30.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgb(0.2, 0.6, 0.9)));
        doc.add_object(rect, Some(lid)).unwrap();

        for endian in [RiffEndian::Little, RiffEndian::Big] {
            for prec in [CmxPrecision::Bit16, CmxPrecision::Bit32] {
                let bytes = export_cmx_bytes_with_endian(&doc, prec, endian).unwrap();
                let parsed = parse_cmx_bytes(&bytes, &CodecLimits::default()).unwrap();
                assert!(parsed.riff_tree.find_chunk(b"disp").is_some());
                assert!(parsed.riff_tree.find_chunk(b"ixpg").is_some());
                assert!(parsed.riff_tree.find_chunk(b"ixlr").is_some());

                let preview = crate::zcf::extract_cdr_preview_png(&bytes, &CodecLimits::default())
                    .unwrap()
                    .expect("disp preview png");
                assert!(preview.starts_with(b"\x89PNG\r\n\x1a\n"));

                let oid = parsed.document.pages()[0].layers[0].objects[0];
                let bb = parsed.document.object_bounds(oid).unwrap();
                assert!((bb.min_x - 20.0).abs() < 0.05);
                assert!((bb.min_y - 40.0).abs() < 0.05);
                assert!((bb.width() - 60.0).abs() < 0.05);
                assert!((bb.height() - 30.0).abs() < 0.05);
            }
        }
    }

    #[test]
    fn cmx_legacy_top_left_cont_bbox_parses_without_flip() {
        let mut pts_blob = Vec::new();
        // Manually build a legacy top-left CMX2 with cont bbox [0, 0, pw, ph]
        let mut cont = Vec::new();
        cont.extend_from_slice(b"CMX2");
        cont.extend_from_slice(&4u16.to_le_bytes());
        cont.extend_from_slice(&CmxCoordUnit::TenthMicron.to_tag().to_le_bytes());
        cont.extend_from_slice(&1u16.to_le_bytes());
        cont.extend_from_slice(&0i32.to_le_bytes());
        cont.extend_from_slice(&0i32.to_le_bytes());
        cont.extend_from_slice(&mm_to_cdr32(210.0).to_le_bytes());
        cont.extend_from_slice(&mm_to_cdr32(297.0).to_le_bytes());
        cont.extend_from_slice(&0u16.to_le_bytes());

        // Single PolyCurve box at (15, 25)
        let mut poly = Vec::new();
        poly.extend_from_slice(&0u16.to_le_bytes());
        poly.push(0x07);
        poly.push(0);
        poly.extend_from_slice(&1.0f32.to_le_bytes());
        poly.push(0);
        poly.push(0);
        poly.push(0);
        let (pts, flags) = encode_curve_points(&BezierPath::from_rect(Rect2D::from_xywh(
            15.0, 25.0, 40.0, 20.0,
        )));
        poly.extend_from_slice(&(pts.len() as u32).to_le_bytes());
        for p in &pts {
            poly.extend_from_slice(&mm_to_cmx32(p.x, CmxCoordUnit::TenthMicron).to_le_bytes());
            poly.extend_from_slice(&mm_to_cmx32(p.y, CmxCoordUnit::TenthMicron).to_le_bytes());
        }
        poly.extend_from_slice(&flags);
        emit_cmd(&mut pts_blob, RiffEndian::Little, CMD_POLY_CURVE, &poly);

        let tree = RiffDocument::new(
            *b"CMX2",
            vec![
                RiffNode::chunk(*b"cont", cont),
                RiffNode::chunk(*b"page", pts_blob),
            ],
        );
        let bytes = write_riff(&tree).unwrap();
        let parsed = parse_cmx_bytes(&bytes, &CodecLimits::default()).unwrap();
        let oid = parsed.document.pages()[0].layers[0].objects[0];
        let bb = parsed.document.object_bounds(oid).unwrap();
        assert!((bb.min_x - 15.0).abs() < 0.05);
        assert!((bb.min_y - 25.0).abs() < 0.05);
    }

    #[test]
    fn cmx_preserves_non_flattened_complex_objects_and_symbols() {
        let mut doc = Document::new_a4("CMX Complex");
        let sym = SymbolDef::new(
            "Tag",
            vec![SceneObject::new(
                ObjectId(1),
                "R",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 10.0,
                    height: 10.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::default(),
                    lock_corners: false,
                }),
            )],
        );
        let sid = sym.id;
        doc.insert_symbol(sym);
        let lid = doc.pages()[0].layers[0].id;

        let mut tbl = SceneObject::new(
            ObjectId(0),
            "Tbl",
            NodePayload::Table(VectorTable::new(2, 2, 20.0, 10.0)),
        );
        tbl.effects.push(Effect::DropShadow {
            dx_mm: 1.0,
            dy_mm: 1.0,
            blur_mm: 2.0,
            opacity: 0.4,
            color: Color::BLACK,
            feather_direction: FeatherDirection::Middle,
        });
        tbl.lens = Some(LensConfig {
            kind: LensKind::Invert,
            frozen: false,
            viewpoint: None,
            remove_face: false,
        });
        tbl.outline.end_arrow = ArrowheadStyle::Stealth;
        let oid1 = doc.add_object(tbl, Some(lid)).unwrap();
        let dim = SceneObject::new(
            ObjectId(0),
            "Dim",
            NodePayload::Dimension(DimensionObject::linear(
                DimensionKind::Parallel,
                Point2D::new(5.0, 5.0),
                Point2D::new(45.0, 5.0),
                4.0,
            )),
        );
        let oid2 = doc.add_object(dim, Some(lid)).unwrap();
        let conn = SceneObject::new(
            ObjectId(0),
            "Conn",
            NodePayload::Connector(ConnectorObject::between_objects(
                oid1,
                AnchorPoint::Right,
                oid2,
                AnchorPoint::Left,
                ConnectorStyle::Straight,
            )),
        );
        doc.add_object(conn, Some(lid)).unwrap();
        let sym_inst = SceneObject::new(ObjectId(0), "SymI", NodePayload::SymbolInstance(sid));
        doc.add_object(sym_inst, Some(lid)).unwrap();

        let pc_child = doc
            .insert_loaded_object(SceneObject::new(
                ObjectId(0),
                "Inner",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 8.0,
                    height: 8.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::default(),
                    lock_corners: false,
                }),
            ))
            .unwrap();
        let pc = SceneObject::new(
            ObjectId(0),
            "PC",
            NodePayload::PowerClip {
                container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 10.0, 30.0, 30.0,
                ))),
                container_parametric: None,
                lock_contents: true,
                contents: vec![pc_child],
            },
        );
        doc.add_object(pc, Some(lid)).unwrap();

        let bytes = export_cmx_bytes(&doc, CmxPrecision::Bit32).unwrap();
        let parsed = parse_cmx_bytes(&bytes, &CodecLimits::default()).unwrap();
        let objs = &parsed.document.pages()[0].layers[0].objects;
        assert_eq!(parsed.document.symbols().len(), 1);
        assert!(matches!(
            parsed.document.object(objs[0]).unwrap().payload,
            NodePayload::Table(_)
        ));
        assert_eq!(
            parsed.document.object(objs[0]).unwrap().outline.end_arrow,
            ArrowheadStyle::Stealth
        );
        assert!(parsed.document.object(objs[0]).unwrap().lens.is_some());
        assert!(!parsed.document.object(objs[0]).unwrap().effects.is_empty());
        assert!(matches!(
            parsed.document.object(objs[1]).unwrap().payload,
            NodePayload::Dimension(_)
        ));
        assert!(matches!(
            parsed.document.object(objs[2]).unwrap().payload,
            NodePayload::Connector(_)
        ));
        assert!(matches!(
            parsed.document.object(objs[3]).unwrap().payload,
            NodePayload::SymbolInstance(_)
        ));
        assert!(matches!(
            parsed.document.object(objs[4]).unwrap().payload,
            NodePayload::PowerClip { .. }
        ));
    }

    #[test]
    fn cmx_plain_path_before_symbol_instance_and_group_with_effects_survive() {
        use coree_core::{
            CornerStyle, Effect, FeatherDirection, LensConfig, LensKind, ParametricShape, Rect2D,
        };

        let mut doc = Document::new_a4("CMX Ext Attach & Group");
        let sym = SymbolDef::new(
            "Badge",
            vec![SceneObject::new(
                ObjectId(1),
                "BadgeRect",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 12.0,
                    height: 12.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::default(),
                    lock_corners: false,
                }),
            )],
        );
        let sid = sym.id;
        doc.insert_symbol(sym);
        let lid = doc.pages()[0].layers[0].id;

        // Plain Path (obj_needs_cmx_ext == false) immediately followed by SymbolInstance
        let mut plain_path = SceneObject::new(
            ObjectId(0),
            "PlainPath",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                5.0, 5.0, 20.0, 15.0,
            ))),
        );
        plain_path.fill = Fill::Solid(Color::CYAN);
        doc.add_object(plain_path, Some(lid)).unwrap();

        let sym_inst = SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid));
        doc.add_object(sym_inst, Some(lid)).unwrap();

        // Group carrying DropShadow + LensConfig with two children
        let c1 = doc
            .insert_loaded_object(SceneObject::new(
                ObjectId(0),
                "Child1",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    30.0, 10.0, 10.0, 10.0,
                ))),
            ))
            .unwrap();
        let c2 = doc
            .insert_loaded_object(SceneObject::new(
                ObjectId(0),
                "Child2",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    45.0, 10.0, 10.0, 10.0,
                ))),
            ))
            .unwrap();
        let mut grp =
            SceneObject::new(ObjectId(0), "StyledGroup", NodePayload::Group(vec![c1, c2]));
        grp.effects.push(Effect::DropShadow {
            dx_mm: 2.0,
            dy_mm: 2.0,
            blur_mm: 1.5,
            opacity: 0.5,
            color: Color::BLACK,
            feather_direction: FeatherDirection::Middle,
        });
        grp.lens = Some(LensConfig {
            kind: LensKind::Brighten { rate_pct: 25.0 },
            frozen: false,
            viewpoint: None,
            remove_face: false,
        });
        doc.add_object(grp, Some(lid)).unwrap();

        let bytes = export_cmx_bytes(&doc, CmxPrecision::Bit32).unwrap();
        let parsed = parse_cmx_bytes(&bytes, &CodecLimits::default()).unwrap();
        let top_objs = &parsed.document.pages()[0].layers[0].objects;
        assert_eq!(
            top_objs.len(),
            3,
            "PlainPath, SymInst, StyledGroup must all exist"
        );

        let p0 = parsed.document.object(top_objs[0]).unwrap();
        assert_eq!(p0.name, "PlainPath");
        assert!(matches!(p0.payload, NodePayload::Path(_)));

        let p1 = parsed.document.object(top_objs[1]).unwrap();
        assert_eq!(p1.name, "SymInst");
        assert_eq!(p1.payload, NodePayload::SymbolInstance(sid));

        let p2 = parsed.document.object(top_objs[2]).unwrap();
        assert_eq!(p2.name, "StyledGroup");
        assert!(!p2.effects.is_empty());
        assert!(p2.lens.is_some());
        if let NodePayload::Group(ref children) = p2.payload {
            assert_eq!(children.len(), 2);
            for &cid in children {
                let child = parsed.document.object(cid).expect("group child must exist");
                assert!(matches!(child.payload, NodePayload::Path(_)));
            }
        } else {
            panic!("expected Group payload, got {:?}", p2.payload);
        }
    }

    #[test]
    fn cmx_remaps_blend_and_linked_next_frame_when_object_ids_shift() {
        use coree_core::{Effect, Rect2D};

        let mut doc = Document::new_a4("CMX Shifted IDs");
        let lid = doc.pages()[0].layers[0].id;

        // Allocate and remove a dummy object first so remaining ObjectIds start > 1
        let dummy = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "DummyToDelete",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 5.0, 5.0))),
                ),
                Some(lid),
            )
            .unwrap();
        let _ = doc.remove_object(dummy);

        let path_oid = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "BlendSpine",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        10.0, 10.0, 40.0, 10.0,
                    ))),
                ),
                Some(lid),
            )
            .unwrap();
        let target_oid = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "BlendTarget",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        60.0, 10.0, 20.0, 20.0,
                    ))),
                ),
                Some(lid),
            )
            .unwrap();
        let mut src = SceneObject::new(
            ObjectId(0),
            "BlendSource",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                10.0, 30.0, 15.0, 15.0,
            ))),
        );
        src.effects.push(Effect::Blend {
            target_id: target_oid,
            steps: 5,
            rotation_deg: 15.0,
            along_path: Some(path_oid),
        });
        doc.add_object(src, Some(lid)).unwrap();

        let next_para_oid = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Para2",
                    NodePayload::ParagraphText(ParagraphText::new(
                        "Second frame",
                        Vec2D::new(40.0, 20.0),
                        TextStyle::default(),
                    )),
                ),
                Some(lid),
            )
            .unwrap();
        let mut p1 =
            ParagraphText::new("First frame", Vec2D::new(40.0, 20.0), TextStyle::default());
        p1.linked_next_frame = Some(next_para_oid);
        doc.add_object(
            SceneObject::new(ObjectId(0), "Para1", NodePayload::ParagraphText(p1)),
            Some(lid),
        )
        .unwrap();

        let bytes = export_cmx_bytes(&doc, CmxPrecision::Bit32).unwrap();
        let parsed = parse_cmx_bytes(&bytes, &CodecLimits::default()).unwrap();
        let objs = &parsed.document.pages()[0].layers[0].objects;
        assert_eq!(objs.len(), 5);
        let new_spine = objs[0];
        let new_target = objs[1];
        let new_src = objs[2];
        let new_para2 = objs[3];
        let new_para1 = objs[4];

        assert_ne!(new_target, target_oid, "IDs must shift on re-import");
        match parsed.document.object(new_src).unwrap().effects.first() {
            Some(Effect::Blend {
                target_id,
                along_path,
                ..
            }) => {
                assert_eq!(*target_id, new_target);
                assert_eq!(*along_path, Some(new_spine));
            }
            other => panic!("expected Blend effect, got {other:?}"),
        }
        match &parsed.document.object(new_para1).unwrap().payload {
            NodePayload::ParagraphText(para) => {
                assert_eq!(para.linked_next_frame, Some(new_para2));
            }
            other => panic!("expected ParagraphText, got {other:?}"),
        }
    }

    #[test]
    fn cmx_powerclip_preserves_nested_group_nested_powerclip_and_inner_references() {
        use coree_core::{Effect, Rect2D, TextOnPath};

        let mut doc = Document::new_a4("CMX Nested PowerClip");
        let lid = doc.pages()[0].layers[0].id;

        // Shift ObjectIds so src_id != imported_id (exposes any missed remapping)
        let dummy = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "DummyToDelete",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 4.0, 4.0))),
                ),
                Some(lid),
            )
            .unwrap();
        let _ = doc.remove_object(dummy);

        // Top-level Group to ensure top-level Group children are never double-remapped
        let top_p1 = doc
            .insert_loaded_object(SceneObject::new(
                ObjectId(0),
                "TopP1",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(5.0, 5.0, 8.0, 8.0))),
            ))
            .unwrap();
        let top_p2 = doc
            .insert_loaded_object(SceneObject::new(
                ObjectId(0),
                "TopP2",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    15.0, 5.0, 8.0, 8.0,
                ))),
            ))
            .unwrap();
        let top_grp = SceneObject::new(
            ObjectId(0),
            "TopGroup",
            NodePayload::Group(vec![top_p1, top_p2]),
        );
        doc.add_object(top_grp, Some(lid)).unwrap();

        // Inner Group inside PowerClip: 2 Paths + TextOnPath + Blend referencing inner_p1
        let inner_p1 = doc
            .insert_loaded_object(SceneObject::new(
                ObjectId(0),
                "InnerPath1",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    12.0, 12.0, 10.0, 10.0,
                ))),
            ))
            .unwrap();
        let mut inner_p2_obj = SceneObject::new(
            ObjectId(0),
            "InnerPath2",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                24.0, 12.0, 10.0, 10.0,
            ))),
        );
        inner_p2_obj.effects.push(Effect::Blend {
            target_id: inner_p1,
            steps: 3,
            rotation_deg: 0.0,
            along_path: None,
        });
        let inner_p2 = doc.insert_loaded_object(inner_p2_obj).unwrap();
        let inner_top = doc
            .insert_loaded_object(SceneObject::new(
                ObjectId(0),
                "InnerTextOnPath",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("Curved", TextStyle::default()),
                    path_id: inner_p1,
                    offset_mm: 1.0,
                    distance_mm: 0.5,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ))
            .unwrap();
        let inner_grp = doc
            .insert_loaded_object(SceneObject::new(
                ObjectId(0),
                "InnerGroup",
                NodePayload::Group(vec![inner_p1, inner_p2, inner_top]),
            ))
            .unwrap();

        // Nested PowerClip inside outer PowerClip containing a Path
        let deep_p3 = doc
            .insert_loaded_object(SceneObject::new(
                ObjectId(0),
                "DeepPath",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    16.0, 26.0, 6.0, 6.0,
                ))),
            ))
            .unwrap();
        let nested_pc = doc
            .insert_loaded_object(SceneObject::new(
                ObjectId(0),
                "NestedPowerClip",
                NodePayload::PowerClip {
                    container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                        14.0, 24.0, 12.0, 12.0,
                    ))),
                    container_parametric: None,
                    lock_contents: false,
                    contents: vec![deep_p3],
                },
            ))
            .unwrap();

        let outer_pc = SceneObject::new(
            ObjectId(0),
            "OuterPowerClip",
            NodePayload::PowerClip {
                container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 10.0, 40.0, 40.0,
                ))),
                container_parametric: None,
                lock_contents: true,
                contents: vec![inner_grp, nested_pc],
            },
        );
        doc.add_object(outer_pc, Some(lid)).unwrap();

        let bytes = export_cmx_bytes(&doc, CmxPrecision::Bit32).unwrap();
        let parsed = parse_cmx_bytes(&bytes, &CodecLimits::default()).unwrap();
        let rt = &parsed.document;
        let top_objs = &rt.pages()[0].layers[0].objects;
        assert_eq!(top_objs.len(), 2);

        // Verify top-level Group wasn't double-remapped
        let rt_top_grp = rt.object(top_objs[0]).unwrap();
        if let NodePayload::Group(ref children) = rt_top_grp.payload {
            assert_eq!(children.len(), 2);
            assert_eq!(rt.object(children[0]).unwrap().name, "TopP1");
            assert_eq!(rt.object(children[1]).unwrap().name, "TopP2");
        } else {
            panic!("expected top-level Group");
        }

        // Verify outer PowerClip keeps exact 2 direct children (not flattened)
        let rt_outer = rt.object(top_objs[1]).unwrap();
        let outer_contents = match &rt_outer.payload {
            NodePayload::PowerClip { contents, .. } => contents,
            other => panic!("expected OuterPowerClip, got {other:?}"),
        };
        assert_eq!(
            outer_contents.len(),
            2,
            "outer PowerClip must hold [InnerGroup, NestedPowerClip] without flattening"
        );

        let rt_inner_grp = rt.object(outer_contents[0]).expect("InnerGroup must exist");
        assert_eq!(rt_inner_grp.name, "InnerGroup");
        let grp_children = match &rt_inner_grp.payload {
            NodePayload::Group(c) => c,
            other => panic!("expected InnerGroup Group payload, got {other:?}"),
        };
        assert_eq!(grp_children.len(), 3);
        let new_p1 = grp_children[0];
        let new_p2 = grp_children[1];
        let new_top = grp_children[2];
        assert_ne!(new_p1, inner_p1, "IDs must shift on re-import");
        assert_eq!(rt.object(new_p1).unwrap().name, "InnerPath1");
        assert!(matches!(
            rt.object(new_p1).unwrap().payload,
            NodePayload::Path(_)
        ));
        let new_p2_obj = rt.object(new_p2).unwrap();
        assert_eq!(new_p2_obj.name, "InnerPath2");
        assert!(matches!(new_p2_obj.payload, NodePayload::Path(_)));
        assert_eq!(
            new_p2_obj.effects.first(),
            Some(&Effect::Blend {
                target_id: new_p1,
                steps: 3,
                rotation_deg: 0.0,
                along_path: None,
            })
        );
        match &rt.object(new_top).unwrap().payload {
            NodePayload::TextOnPath(top) => {
                assert_eq!(top.path_id, new_p1);
            }
            other => panic!("expected TextOnPath, got {other:?}"),
        }

        let rt_nested_pc = rt
            .object(outer_contents[1])
            .expect("NestedPowerClip must exist");
        assert_eq!(rt_nested_pc.name, "NestedPowerClip");
        let nested_contents = match &rt_nested_pc.payload {
            NodePayload::PowerClip { contents, .. } => contents,
            other => panic!("expected NestedPowerClip PowerClip payload, got {other:?}"),
        };
        assert_eq!(nested_contents.len(), 1);
        let rt_deep = rt.object(nested_contents[0]).expect("DeepPath must exist");
        assert_eq!(rt_deep.name, "DeepPath");
        assert!(matches!(rt_deep.payload, NodePayload::Path(_)));
    }
}
