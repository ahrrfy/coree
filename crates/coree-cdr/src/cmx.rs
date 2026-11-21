// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room `.cmx` 16-bit and 32-bit (`CMX1`/`CMX2`) RIFF/RIFX parser and
//! writer supporting `cont`, `ccmm`, `pack`, `page`, `rclr`, `rpen`, `rlin`,
//! `rstr`, `rimg`, `PolyCurve`, `BeginPage`/`BeginLayer`/`BeginGroup`,
//! `TextFrame`, and `DrawImage`.

use crate::cdr_riff::{
    decode_blend_mode, decode_curve_points, decode_dib_to_rgba_with_budget, encode_blend_mode,
    encode_curve_points, encode_dib_rgba,
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
use crate::{CorelFormat, ImportReport};
use coree_core::{
    math::{sanitize_f32, sanitize_f64},
    Affine2D, ArtisticText, BezierPath, Color, ColorMode, Document, Fill, GradientStop,
    ImageAssetId, Layer, LayerId, LineCap, LineJoin, MasterLayerScope, NodePayload, ObjectId,
    OutlineStyle, Page, PageId, ParagraphText, Point2D, RasterAsset, RasterLayerData, SceneObject,
    TextAlign, TextDirection, TextStyle, UnknownCdrChunk, Vec2D,
};
use std::collections::HashMap;
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
    doc.pages.clear();
    let mut warnings = Vec::new();

    // Parse header chunks (`cont`, `ccmm`, `pack`) and resource tables (`rclr`, `rpen`, `rlin`, `rstr`, `rimg`)
    let mut colors: Vec<Color> = Vec::new();
    let mut pens: Vec<CmxPen> = Vec::new();
    let mut lines: Vec<CmxLineStyle> = Vec::new();
    let mut strings: Vec<String> = Vec::new();
    let mut images: Vec<ImageAssetId> = Vec::new();

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
                            let _bbox = r.read_bytes(16, "cont bbox")?;
                        }
                        if r.remaining() >= 2 {
                            let tlen = usize::from(endian.read_u16(&mut r, "cont title_len")?);
                            if tlen <= r.remaining() {
                                let tb = r.read_bytes(tlen, "cont title")?;
                                let title = decode_cdr_text(tb, Some(CODEPAGE_UTF8), None);
                                if !title.is_empty() {
                                    doc.title = title;
                                }
                            }
                        }
                    }
                }
                b"ccmm" => {
                    let mut r = ByteReader::new(data);
                    if let Ok(cm) = r.read_u8("ccmm color_mode") {
                        doc.color_mode = if cm == 1 {
                            ColorMode::Rgb
                        } else {
                            ColorMode::Cmyk
                        };
                    }
                    if let Ok(dpi) = endian.read_f32(&mut r, "ccmm dpi") {
                        doc.dpi = sanitize_f32(dpi, 300.0).max(1.0);
                    }
                }
                b"pack" => {}
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
                b"page" => {}
                _ => {
                    doc.unknown_doc_chunks.push(UnknownCdrChunk {
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
    doc.metadata.source_format = Some(match precision {
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
    };

    for node in &riff_tree.children {
        if let RiffNode::Chunk { fourcc, data, .. } = node {
            if fourcc == b"page" {
                parse_cmx_page_chunk(data, &res, limits, &mut doc, &mut warnings)?;
            }
        }
    }

    if doc.pages.is_empty() {
        let pid = PageId(doc.alloc_id());
        let lid = LayerId(doc.alloc_id());
        doc.pages
            .push(Page::new(pid, "Page 1", 210.0, 297.0, 3.0, lid));
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
        CmxPrecision::Bit16 => CorelFormat::Cmx16,
        CmxPrecision::Bit32 => CorelFormat::Cmx32,
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
    warnings: &mut Vec<String>,
) -> CdrResult<()> {
    let endian = res.endian;
    let mut r = ByteReader::new(data);
    let mut current_page_idx: Option<usize> = None;
    let mut current_layer_id: Option<LayerId> = None;
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
                let bleed = cdr32_to_mm(endian.read_i32(&mut cr, "BeginPage bleed")?).max(0.0);
                let name_idx = endian.read_u16(&mut cr, "BeginPage name")?;
                let name = res.string(name_idx, &format!("Page {}", doc.pages.len() + 1));
                let pid = PageId(doc.alloc_id());
                doc.pages.push(Page {
                    id: pid,
                    name,
                    width_mm: w,
                    height_mm: h,
                    bleed_mm: bleed,
                    facing_pages: false,
                    layers: Vec::new(),
                });
                current_page_idx = Some(doc.pages.len() - 1);
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
                    if doc.pages[pidx].layers.is_empty() {
                        let lid = LayerId(doc.alloc_id());
                        doc.pages[pidx].layers.push(Layer::new(lid, "Layer 1"));
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
                        doc.pages.push(Page {
                            id: pid,
                            name: format!("Page {}", doc.pages.len() + 1),
                            width_mm: 210.0,
                            height_mm: 297.0,
                            bleed_mm: 3.0,
                            facing_pages: false,
                            layers: Vec::new(),
                        });
                        let idx = doc.pages.len() - 1;
                        current_page_idx = Some(idx);
                        idx
                    }
                };
                let flags = cr.read_u8("BeginLayer flags")?;
                let ms = cr.read_u8("BeginLayer master_scope")?;
                let name_idx = endian.read_u16(&mut cr, "BeginLayer name")?;
                let name = res.string(
                    name_idx,
                    &format!("Layer {}", doc.pages[pidx].layers.len() + 1),
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
                doc.pages[pidx].layers.push(layer);
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
                    read_cmx_affine(&mut cr, endian)?
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
                if let Some(grp) = group_stack.pop() {
                    let gid = finalize_group(grp, doc);
                    attach_object(
                        gid,
                        &mut group_stack,
                        &mut current_page_idx,
                        &mut current_layer_id,
                        doc,
                    );
                }
            }
            CMD_POLY_CURVE => {
                let name_idx = endian.read_u16(&mut cr, "PolyCurve name")?;
                let flags = cr.read_u8("PolyCurve flags")?;
                let blend = decode_blend_mode(cr.read_u8("PolyCurve blend")?);
                let opacity = sanitize_f32(endian.read_f32(&mut cr, "PolyCurve opacity")?, 1.0)
                    .clamp(0.0, 1.0);
                let has_xf = cr.read_u8("PolyCurve has_xf")? != 0;
                let transform = if has_xf {
                    read_cmx_affine(&mut cr, endian)?
                } else {
                    Affine2D::IDENTITY
                };

                let fill = read_cmx_fill(&mut cr, res)?;
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
                    pts.push(Point2D::new(x, y));
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
                doc.objects.insert(oid, obj);
                attach_object(
                    oid,
                    &mut group_stack,
                    &mut current_page_idx,
                    &mut current_layer_id,
                    doc,
                );
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
                    read_cmx_affine(&mut cr, endian)?
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
                doc.objects.insert(oid, obj);
                attach_object(
                    oid,
                    &mut group_stack,
                    &mut current_page_idx,
                    &mut current_layer_id,
                    doc,
                );
            }
            CMD_DRAW_IMAGE => {
                let name_idx = endian.read_u16(&mut cr, "DrawImage name")?;
                let img_idx = usize::from(endian.read_u16(&mut cr, "DrawImage img_idx")?);
                let w_mm = res.read_coord(&mut cr, "DrawImage w")?.abs();
                let h_mm = res.read_coord(&mut cr, "DrawImage h")?.abs();
                let has_xf = cr.read_u8("DrawImage has_xf")? != 0;
                let transform = if has_xf {
                    read_cmx_affine(&mut cr, endian)?
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
                    doc.objects.insert(oid, obj);
                    attach_object(
                        oid,
                        &mut group_stack,
                        &mut current_page_idx,
                        &mut current_layer_id,
                        doc,
                    );
                } else {
                    warnings.push(format!("DrawImage referenced missing rimg #{img_idx}"));
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
            doc.pages
                .push(Page::new(pid, "Page 1", 210.0, 297.0, 3.0, lid));
            *page_idx = Some(doc.pages.len() - 1);
            *layer_id = Some(lid);
            return lid;
        }
    };
    let lid = LayerId(doc.alloc_id());
    doc.pages[pidx].layers.push(Layer::new(lid, "Layer 1"));
    *layer_id = Some(lid);
    lid
}

fn finalize_group(grp: OpenGroup, doc: &mut Document) -> ObjectId {
    let gid = doc.alloc_object_id();
    let mut obj = SceneObject::new(gid, grp.name, NodePayload::Group(grp.children));
    obj.transform = grp.transform;
    obj.fill = Fill::None;
    obj.outline = OutlineStyle::none();
    doc.objects.insert(gid, obj);
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

fn read_cmx_fill(r: &mut ByteReader<'_>, res: &CmxResources<'_>) -> CdrResult<Fill> {
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
                    center: Point2D::new(x0, y0),
                    radius: scalar.abs(),
                    focal: Point2D::new(x1, y1),
                    stops,
                },
                4 => Fill::ConicalGradient {
                    center: Point2D::new(x0, y0),
                    angle_deg: angle,
                    stops,
                },
                5 => Fill::SquareGradient {
                    center: Point2D::new(x0, y0),
                    size: scalar.abs(),
                    angle_deg: angle,
                    stops,
                },
                _ => Fill::LinearGradient {
                    start: Point2D::new(x0, y0),
                    end: Point2D::new(x1, y1),
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
        }
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
        let Some(asset) = doc.assets.get(&aid) else {
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
    let mut page_chunks: Vec<Vec<u8>> = Vec::with_capacity(doc.pages.len());

    for page in &doc.pages {
        let mut stream = Vec::new();

        // BeginPage
        let mut bp = Vec::with_capacity(14);
        endian.write_i32(&mut bp, mm_to_cdr32(page.width_mm));
        endian.write_i32(&mut bp, mm_to_cdr32(page.height_mm));
        endian.write_i32(&mut bp, mm_to_cdr32(page.bleed_mm));
        let pname_idx = tables.intern_string(&page.name)?;
        endian.write_u16(&mut bp, pname_idx);
        emit_cmd(&mut stream, endian, CMD_BEGIN_PAGE, &bp);

        for layer in &page.layers {
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
            emit_cmd(&mut stream, endian, CMD_BEGIN_LAYER, &bl);

            for &oid in &layer.objects {
                emit_cmx_object(doc, oid, &mut tables, &mut stream, 0)?;
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

    // Build `cont`
    let mut cont = Vec::new();
    cont.extend_from_slice(&form_type);
    endian.write_u16(&mut cont, coord_bytes);
    endian.write_u16(&mut cont, tables.unit.to_tag());
    endian.write_u16(&mut cont, doc.pages.len().min(u16::MAX as usize) as u16);
    let (pw, ph) = doc
        .pages
        .first()
        .map_or((210.0, 297.0), |p| (p.width_mm, p.height_mm));
    endian.write_i32(&mut cont, 0);
    endian.write_i32(&mut cont, 0);
    endian.write_i32(&mut cont, mm_to_cdr32(pw));
    endian.write_i32(&mut cont, mm_to_cdr32(ph));
    let tb = doc.title.as_bytes();
    let tlen = tb.len().min(u16::MAX as usize);
    endian.write_u16(&mut cont, tlen as u16);
    cont.extend_from_slice(&tb[..tlen]);

    // Build `ccmm`
    let mut ccmm = Vec::with_capacity(5);
    ccmm.push(if doc.color_mode == ColorMode::Rgb {
        1
    } else {
        0
    });
    endian.write_f32(&mut ccmm, sanitize_f32(doc.dpi, 300.0));

    // Build `pack`
    let mut pack = Vec::new();
    endian.write_u32(&mut pack, 0);
    let ver_b = b"coree-cmx";
    endian.write_u16(&mut pack, ver_b.len() as u16);
    pack.extend_from_slice(ver_b);

    let mut nodes = vec![
        RiffNode::chunk(*b"cont", cont),
        RiffNode::chunk(*b"ccmm", ccmm),
        RiffNode::chunk(*b"pack", pack),
        RiffNode::chunk(*b"rclr", encode_rclr(&tables.colors, endian)),
        RiffNode::chunk(*b"rpen", encode_rpen(&tables.pens, endian)),
        RiffNode::chunk(*b"rlin", encode_rlin(&tables.lines, endian)),
        RiffNode::chunk(*b"rstr", encode_rstr(&tables.strings, endian)),
    ];
    if !tables.images.is_empty() {
        nodes.push(RiffNode::chunk(
            *b"rimg",
            encode_rimg(&tables.images, endian),
        ));
    }
    for pbytes in page_chunks {
        nodes.push(RiffNode::chunk(*b"page", pbytes));
    }
    for unk in &doc.unknown_doc_chunks {
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

fn emit_cmx_object(
    doc: &Document,
    oid: ObjectId,
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
    let Some(obj) = doc.objects.get(&oid) else {
        return Ok(());
    };
    let endian = tables.endian;

    match &obj.payload {
        NodePayload::Group(children) => {
            let mut bg = Vec::new();
            let name_idx = tables.intern_string(&obj.name)?;
            endian.write_u16(&mut bg, name_idx);
            write_cmx_affine(&mut bg, endian, &obj.transform);
            emit_cmd(stream, endian, CMD_BEGIN_GROUP, &bg);
            for &cid in children {
                emit_cmx_object(doc, cid, tables, stream, depth + 1)?;
            }
            emit_cmd(stream, endian, CMD_END_GROUP, &[]);
        }
        NodePayload::Path(path) => {
            emit_cmx_polycurve(obj, path, tables, stream)?;
        }
        NodePayload::Shape(shape) => {
            emit_cmx_polycurve(obj, &shape.to_bezier_path(), tables, stream)?;
        }
        NodePayload::PowerClip {
            container,
            contents,
            ..
        } => {
            emit_cmx_polycurve(obj, container, tables, stream)?;
            for &cid in contents {
                emit_cmx_object(doc, cid, tables, stream, depth + 1)?;
            }
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
                write_cmx_affine(&mut di, endian, &obj.transform);
                emit_cmd(stream, endian, CMD_DRAW_IMAGE, &di);
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
        NodePayload::SymbolInstance(_) => {}
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
    write_cmx_affine(&mut b, endian, &obj.transform);

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

    // Geometry points + node flags
    let (pts, node_flags) = encode_curve_points(path);
    endian.write_u32(&mut b, pts.len() as u32);
    for pt in &pts {
        tables.write_coord(&mut b, pt.x);
        tables.write_coord(&mut b, pt.y);
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
    tables.write_coord(out, p0.x);
    tables.write_coord(out, p0.y);
    tables.write_coord(out, p1.x);
    tables.write_coord(out, p1.y);
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
    write_cmx_affine(&mut b, endian, &obj.transform);
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
