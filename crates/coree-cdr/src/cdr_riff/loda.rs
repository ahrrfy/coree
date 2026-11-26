// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDR `loda` argument codecs (`20B` reverse-ordered writer + dual `20B`/`12B` reader),
//! curve point/flag encoding, and lossless non-flattening structured payload args.

use super::text_style::{decode_blend_mode, encode_blend_mode};
use crate::coord::{cdr32_to_mm, decode_cdr_text, mm_to_cdr32, CODEPAGE_UTF8};
use crate::error::{CdrError, CdrResult};
use crate::limits::ByteReader;
use crate::zcf::{parse_c9_style_override, CdssStyle};
use coree_core::{
    math::sanitize_f64, ArtisticMediaStyle, BSplineCurve, BasicShapeKind, BezierPath, BlendMode,
    ColorStyleId, ConnectorObject, CornerStyle, DimensionObject, Effect, EllipseMode, ImpactStyle,
    MasterLayerScope, NodeKind, ObjectId, ObjectStyleId, ParametricShape, PathNode, Perspective3x3,
    Point2D, Rect2D, SceneObject, Subpath, SymbolId, TextOnPath, VectorTable,
};
use serde::{Deserialize, Serialize};

// LODA argument type IDs
pub(crate) const LODA_ARG_RECTANGLE: u32 = 0x0001;
pub(crate) const LODA_ARG_ELLIPSE: u32 = 0x0002;
pub(crate) const LODA_ARG_POLYGON: u32 = 0x0003;
pub(crate) const LODA_ARG_STAR: u32 = 0x0004;
pub(crate) const LODA_ARG_BITMAP_REF: u32 = 0x0005;
pub(crate) const LODA_ARG_TEXT_META: u32 = 0x0006;
pub(crate) const LODA_ARG_OUTLINE_ID: u32 = 0x000A;
pub(crate) const LODA_ARG_FILL_ID: u32 = 0x0014;
pub(crate) const LODA_ARG_CURVE: u32 = 0x001E;
pub(crate) const LODA_ARG_STYLE_ID: u32 = 0x00C8;
pub(crate) const LODA_ARG_STYLE_JSON: u32 = 0x00C9;
pub(crate) const LODA_ARG_OBJ_PROPS: u32 = 0x00D0;
pub(crate) const LODA_ARG_EXTRA_SHAPE: u32 = 0x00D1;
pub(crate) const LODA_ARG_TABLE: u32 = 0x00D2;
pub(crate) const LODA_ARG_DIMENSION: u32 = 0x00D3;
pub(crate) const LODA_ARG_CONNECTOR: u32 = 0x00D4;
pub(crate) const LODA_ARG_SYMBOL_INSTANCE: u32 = 0x00D5;
pub(crate) const LODA_ARG_TEXT_ON_PATH: u32 = 0x00D6;
pub(crate) const LODA_ARG_POWERCLIP_META: u32 = 0x00D7;
pub(crate) const LODA_ARG_EFFECTS: u32 = 0x00D8;
pub(crate) const LODA_ARG_OBJECT_STYLE_LINK: u32 = 0x00D9;
pub(crate) const LODA_ARG_SOURCE_OID: u32 = 0x00DA;
pub(crate) const LODA_ARG_LAYER_PROPS: u32 = 0x00DB;
pub(crate) const LODA_ARG_BSPLINE: u32 = 0x00DC;
pub(crate) const LODA_ARG_ARTISTIC_MEDIA: u32 = 0x00DD;
pub(crate) const LODA_ARG_CLONE: u32 = 0x00DE;
pub(crate) const LODA_ARG_PERSPECTIVE_3X3: u32 = 0x00DF;
pub(crate) const LODA_ARG_COLOR_STYLE_LINK: u32 = 0x00E0;
pub(crate) const LODA_ARG_NAME: u32 = 0x03E8;

// Native CDR LODA chunk types (offset 16 in 20-byte LODA header)
pub(crate) const CDR_CTYPE_GROUP_OR_LAYER: u32 = 0x00;
pub(crate) const CDR_CTYPE_RECTANGLE: u32 = 0x01;
pub(crate) const CDR_CTYPE_ELLIPSE: u32 = 0x02;
pub(crate) const CDR_CTYPE_CURVE: u32 = 0x03;
pub(crate) const CDR_CTYPE_POLYGON: u32 = 0x04;
pub(crate) const CDR_CTYPE_BITMAP: u32 = 0x05;
pub(crate) const CDR_CTYPE_ARTISTIC_TEXT: u32 = 0x06;
pub(crate) const CDR_CTYPE_PARAGRAPH_TEXT: u32 = 0x10;
pub(crate) const CDR_CTYPE_LAYER_GRID: u32 = 0x0B;
pub(crate) const CDR_CTYPE_LAYER_GUIDES: u32 = 0x0C;
pub(crate) const CDR_CTYPE_LAYER_DESKTOP: u32 = 0x11;

// Curve node flags (bitmask)
const NODE_KIND_MASK: u8 = 0xC0;
const NODE_MOVE_TO: u8 = 0x00;
const NODE_LINE_TO: u8 = 0x40;
const NODE_CURVE_TO: u8 = 0x80;
const NODE_CTRL_PT: u8 = 0xC0;
const NODE_CONT_MASK: u8 = 0x30;
const NODE_CONT_CUSP: u8 = 0x00;
const NODE_CONT_SMOOTH: u8 = 0x10;
const NODE_CONT_SYMMETRICAL: u8 = 0x20;
const NODE_FLAG_CLOSED: u8 = 0x08;
const NODE_CTRL_PRESENT: u8 = 0x01;
const NODE_CTRL_ABSENT: u8 = 0x02;

#[derive(Debug, Clone)]
pub(crate) struct ParsedObjProps {
    pub name: String,
    pub visible: bool,
    pub locked: bool,
    pub printable: bool,
    pub blend_mode: BlendMode,
    pub opacity: f32,
}

impl Default for ParsedObjProps {
    fn default() -> Self {
        Self {
            name: "Object".to_owned(),
            visible: true,
            locked: false,
            printable: true,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ParsedBitmapRef {
    pub bmp_id: u32,
    pub width_mm: f64,
    pub height_mm: f64,
    pub crop_rect: Option<Rect2D>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ParsedTextMeta {
    pub is_paragraph: bool,
    pub frame_w_mm: f64,
    pub frame_h_mm: f64,
    pub columns: u32,
    pub gutter_mm: f64,
    pub linked_next_frame: Option<ObjectId>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CdrNativeRect {
    pub local_w_mm: f64,
    pub local_h_mm: f64,
    pub corners: [f64; 4],
    pub relative_corners: bool,
    pub corner_style: CornerStyle,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CdrNativeEllipse {
    pub local_w_mm: f64,
    pub local_h_mm: f64,
    pub mode: EllipseMode,
    pub start_deg: f64,
    pub end_deg: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PowerClipLodaMeta {
    pub container_parametric: Option<ParametricShape>,
    pub lock_contents: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ParsedLayerLodaProps {
    pub visible: bool,
    pub locked: bool,
    pub printable: bool,
    pub is_cut_layer: bool,
    pub master_scope: MasterLayerScope,
}

#[derive(Debug, Default)]
pub(crate) struct ParsedLoda {
    pub is_real_cdr: bool,
    pub curve: Option<BezierPath>,
    pub shape: Option<ParametricShape>,
    pub real_rect: Option<CdrNativeRect>,
    pub real_ellipse: Option<CdrNativeEllipse>,
    pub bitmap_ref: Option<ParsedBitmapRef>,
    pub text_meta: Option<ParsedTextMeta>,
    pub text_stream_id: Option<u32>,
    pub fill_id: Option<u32>,
    pub outline_id: Option<u32>,
    pub style_id: Option<u32>,
    pub style_json: Option<CdssStyle>,
    pub real_name: Option<String>,
    pub props: Option<ParsedObjProps>,
    pub table: Option<VectorTable>,
    pub dimension: Option<DimensionObject>,
    pub connector: Option<ConnectorObject>,
    pub symbol_instance: Option<SymbolId>,
    pub text_on_path: Option<TextOnPath>,
    pub powerclip_meta: Option<PowerClipLodaMeta>,
    pub effects: Vec<Effect>,
    pub object_style_id: Option<ObjectStyleId>,
    pub source_oid: Option<ObjectId>,
    pub bspline: Option<BSplineCurve>,
    pub artistic_media: Option<(BezierPath, ArtisticMediaStyle)>,
    pub clone_ref: Option<(ObjectId, bool, bool, bool)>,
    pub perspective: Option<Perspective3x3>,
    pub color_style_link: Option<(Option<ColorStyleId>, Option<usize>)>,
    pub unknown_args: Vec<(u32, Vec<u8>)>,
}

/// Returns `true` if `data` matches the native CDR 20-byte `loda` header layout:
/// `[0..4] chunk_len == data.len()`, `[4..8] num_args`, `[8..12] args_offset >= 20`,
/// `[12..16] types_offset`, `[16..20] chunk_type`.
pub(crate) fn is_cdr_native_loda(data: &[u8]) -> bool {
    if data.len() < 20 {
        return false;
    }
    let chunk_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let num_args = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let args_off = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    let types_off = u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
    chunk_len == data.len()
        && num_args <= 1024
        && args_off >= 20
        && types_off >= args_off
        && types_off
            .checked_add(num_args.saturating_mul(4))
            .is_some_and(|end| end <= data.len())
}

type RealLodaArgs<'a> = (u32, Vec<(u32, &'a [u8])>);

pub(crate) fn parse_real_loda_args(data: &[u8]) -> Option<RealLodaArgs<'_>> {
    if !is_cdr_native_loda(data) {
        return None;
    }
    let num_args = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let args_off = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    let types_off = u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
    let chunk_type = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);

    let table_bytes = num_args.checked_mul(4)?;
    let offsets_slice = data.get(args_off..args_off.checked_add(table_bytes)?)?;
    let types_slice = data.get(types_off..types_off.checked_add(table_bytes)?)?;

    // Optional trailing end-offset entry at `args_off + num_args * 4` (present in v16–26)
    let trailing_end = if types_off >= args_off.saturating_add(table_bytes).saturating_add(4) {
        let p = args_off + table_bytes;
        Some(u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize)
    } else {
        None
    };

    let mut offsets = Vec::with_capacity(num_args);
    let mut types = Vec::with_capacity(num_args);
    for i in 0..num_args {
        let o = i * 4;
        offsets.push(u32::from_le_bytes([
            offsets_slice[o],
            offsets_slice[o + 1],
            offsets_slice[o + 2],
            offsets_slice[o + 3],
        ]) as usize);
        types.push(u32::from_le_bytes([
            types_slice[o],
            types_slice[o + 1],
            types_slice[o + 2],
            types_slice[o + 3],
        ]));
    }

    // Native CDR `loda` stores `types` in REVERSE order relative to `offsets`!
    let mut out = Vec::with_capacity(num_args);
    let mut prev_end = 0usize;
    let mut total_bytes = 0usize;
    for i in 0..num_args {
        let start = offsets[i];
        let end = offsets
            .get(i + 1)
            .copied()
            .or(trailing_end)
            .unwrap_or(data.len());
        if start < prev_end || start > end || end > data.len() {
            return None;
        }
        let seg_len = end - start;
        total_bytes = total_bytes.checked_add(seg_len)?;
        if total_bytes > data.len() {
            return None;
        }
        prev_end = end;
        let arg_type = types[num_args - 1 - i];
        out.push((arg_type, &data[start..end]));
    }
    Some((chunk_type, out))
}

pub(crate) fn parse_real_layer_loda(
    data: &[u8],
) -> Option<(u32, Option<String>, Option<ParsedLayerLodaProps>)> {
    let (ctype, args) = parse_real_loda_args(data)?;
    let mut name = None;
    let mut props = None;
    for (typ, seg) in args {
        if typ == LODA_ARG_NAME && !seg.is_empty() {
            let decoded = decode_cdr_text(seg, None, None);
            let trimmed = decoded.trim_matches('\0').trim();
            if !trimmed.is_empty() {
                name = Some(trimmed.to_owned());
            }
        } else if typ == LODA_ARG_LAYER_PROPS && seg.len() >= 2 {
            let flags = seg[0];
            let ms = match seg[1] {
                1 => MasterLayerScope::AllPages,
                2 => MasterLayerScope::OddPages,
                3 => MasterLayerScope::EvenPages,
                _ => MasterLayerScope::None,
            };
            props = Some(ParsedLayerLodaProps {
                visible: (flags & 0x01) != 0,
                locked: (flags & 0x02) != 0,
                printable: (flags & 0x04) != 0,
                is_cut_layer: (flags & 0x08) != 0,
                master_scope: ms,
            });
        }
    }
    Some((ctype, name, props))
}

pub(crate) fn extract_real_loda_name(data: &[u8]) -> Option<String> {
    parse_real_layer_loda(data).and_then(|(_, name, _)| name)
}

pub(crate) fn parse_loda_chunk(data: &[u8]) -> CdrResult<ParsedLoda> {
    if is_cdr_native_loda(data) {
        let Some((chunk_type, args)) = parse_real_loda_args(data) else {
            return Err(CdrError::InvalidChunk {
                fourcc: "loda".to_owned(),
                reason: "invalid native CDR loda header".to_owned(),
            });
        };
        let is_clean_room = args.iter().any(|(t, _)| {
            matches!(
                *t,
                LODA_ARG_OBJ_PROPS
                    | LODA_ARG_RECTANGLE..=LODA_ARG_TEXT_META
                    | LODA_ARG_EXTRA_SHAPE
                    | LODA_ARG_TABLE..=LODA_ARG_COLOR_STYLE_LINK
            )
        });
        let owned_args: Vec<(u32, &[u8])> = args;
        return decode_loda_arg_list(&owned_args, chunk_type, !is_clean_room);
    }

    // Legacy 12-byte synthetic RIFF `loda` compatibility
    let mut r = ByteReader::new(data);
    let num_args = usize::try_from(r.read_u32_le("loda num_args")?)
        .map_err(|_| CdrError::ArithmeticOverflow("loda num_args"))?;
    let args_offset = usize::try_from(r.read_u32_le("loda args_offset")?)
        .map_err(|_| CdrError::ArithmeticOverflow("loda args_offset"))?;
    let types_offset = usize::try_from(r.read_u32_le("loda types_offset")?)
        .map_err(|_| CdrError::ArithmeticOverflow("loda types_offset"))?;

    if num_args > 1024 {
        return Err(CdrError::InvalidChunk {
            fourcc: "loda".to_owned(),
            reason: format!("excessive loda argument count {num_args}"),
        });
    }

    let table_bytes = num_args
        .checked_mul(4)
        .ok_or(CdrError::ArithmeticOverflow("loda table bytes"))?;
    let offsets_slice = data
        .get(
            args_offset
                ..args_offset
                    .checked_add(table_bytes)
                    .ok_or(CdrError::ArithmeticOverflow("loda args_offset end"))?,
        )
        .ok_or(CdrError::UnexpectedEof {
            context: "loda offsets table",
            needed: table_bytes,
            available: data.len().saturating_sub(args_offset),
        })?;
    let types_slice = data
        .get(
            types_offset
                ..types_offset
                    .checked_add(table_bytes)
                    .ok_or(CdrError::ArithmeticOverflow("loda types_offset end"))?,
        )
        .ok_or(CdrError::UnexpectedEof {
            context: "loda types table",
            needed: table_bytes,
            available: data.len().saturating_sub(types_offset),
        })?;

    let mut off_r = ByteReader::new(offsets_slice);
    let mut typ_r = ByteReader::new(types_slice);
    let mut args = Vec::with_capacity(num_args);
    for _ in 0..num_args {
        let off = usize::try_from(off_r.read_u32_le("loda arg offset")?)
            .map_err(|_| CdrError::ArithmeticOverflow("loda arg offset"))?;
        let typ = typ_r.read_u32_le("loda arg type")?;
        args.push((typ, off));
    }

    let payload_limit = args_offset.min(types_offset).min(data.len());
    let mut prev_end = 0usize;
    let mut total_bytes = 0usize;
    let mut arg_slices = Vec::with_capacity(num_args);
    for (i, &(typ, off)) in args.iter().enumerate() {
        let end = args
            .get(i + 1)
            .map(|&(_, next_off)| next_off)
            .unwrap_or(payload_limit);
        if off > data.len() {
            return Err(CdrError::UnexpectedEof {
                context: "loda arg payload",
                needed: off,
                available: data.len(),
            });
        }
        if off < prev_end || off > end || end > payload_limit {
            return Err(CdrError::InvalidChunk {
                fourcc: "loda".to_owned(),
                reason: format!(
                    "overlapping or non-monotonic loda arg range {off}..{end} (prev_end={prev_end}, limit={payload_limit})"
                ),
            });
        }
        total_bytes = total_bytes
            .checked_add(end - off)
            .ok_or(CdrError::ArithmeticOverflow("loda total arg bytes"))?;
        if total_bytes > data.len() {
            return Err(CdrError::InvalidChunk {
                fourcc: "loda".to_owned(),
                reason: "cumulative loda arg bytes exceed chunk length".to_owned(),
            });
        }
        prev_end = end;
        arg_slices.push((typ, &data[off..end]));
    }

    decode_loda_arg_list(&arg_slices, CDR_CTYPE_CURVE, false)
}

fn decode_loda_arg_list(
    args: &[(u32, &[u8])],
    chunk_type: u32,
    is_real_cdr: bool,
) -> CdrResult<ParsedLoda> {
    let mut out = ParsedLoda {
        is_real_cdr,
        ..ParsedLoda::default()
    };

    for &(typ, seg) in args {
        let mut ar = ByteReader::new(seg);
        match typ {
            LODA_ARG_CURVE => {
                if is_real_cdr {
                    match chunk_type {
                        CDR_CTYPE_RECTANGLE => {
                            if let Some(rect) = parse_real_rect_arg(seg) {
                                out.real_rect = Some(rect);
                            }
                        }
                        CDR_CTYPE_ELLIPSE => {
                            if let Some(ell) = parse_real_ellipse_arg(seg) {
                                out.real_ellipse = Some(ell);
                            }
                        }
                        CDR_CTYPE_CURVE | CDR_CTYPE_POLYGON => {
                            if let Ok(path) = parse_loda_curve(&mut ar) {
                                if !path.subpaths.is_empty() {
                                    out.curve = Some(path);
                                }
                            }
                        }
                        CDR_CTYPE_BITMAP => {
                            if seg.len() >= 36 {
                                let w = cdr32_to_mm(i32::from_le_bytes([
                                    seg[0], seg[1], seg[2], seg[3],
                                ]))
                                .abs();
                                let h = cdr32_to_mm(i32::from_le_bytes([
                                    seg[4], seg[5], seg[6], seg[7],
                                ]))
                                .abs();
                                let bmp_id =
                                    u32::from_le_bytes([seg[32], seg[33], seg[34], seg[35]]);
                                out.bitmap_ref = Some(ParsedBitmapRef {
                                    bmp_id,
                                    width_mm: w.max(1.0),
                                    height_mm: h.max(1.0),
                                    crop_rect: None,
                                });
                            }
                        }
                        CDR_CTYPE_ARTISTIC_TEXT | CDR_CTYPE_PARAGRAPH_TEXT => {
                            let is_para = chunk_type == CDR_CTYPE_PARAGRAPH_TEXT;
                            if seg.len() >= 4 {
                                out.text_stream_id =
                                    Some(u32::from_le_bytes([seg[0], seg[1], seg[2], seg[3]]));
                            }
                            let (fw, fh) = if is_para && seg.len() >= 12 {
                                (
                                    cdr32_to_mm(i32::from_le_bytes([
                                        seg[4], seg[5], seg[6], seg[7],
                                    ]))
                                    .abs(),
                                    cdr32_to_mm(i32::from_le_bytes([
                                        seg[8], seg[9], seg[10], seg[11],
                                    ]))
                                    .abs(),
                                )
                            } else {
                                (0.0, 0.0)
                            };
                            out.text_meta = Some(ParsedTextMeta {
                                is_paragraph: is_para,
                                frame_w_mm: fw,
                                frame_h_mm: fh,
                                columns: 1,
                                gutter_mm: 5.0,
                                linked_next_frame: None,
                            });
                        }
                        CDR_CTYPE_GROUP_OR_LAYER => {}
                        _ => {
                            if let Ok(path) = parse_loda_curve(&mut ar) {
                                if !path.subpaths.is_empty() {
                                    out.curve = Some(path);
                                }
                            }
                        }
                    }
                } else {
                    out.curve = Some(parse_loda_curve(&mut ar)?);
                }
            }
            LODA_ARG_RECTANGLE => {
                let w = cdr32_to_mm(ar.read_i32_le("rect w")?).abs();
                let h = cdr32_to_mm(ar.read_i32_le("rect h")?).abs();
                let r0 = cdr32_to_mm(ar.read_i32_le("rect r0")?).max(0.0);
                let r1 = cdr32_to_mm(ar.read_i32_le("rect r1")?).max(0.0);
                let r2 = cdr32_to_mm(ar.read_i32_le("rect r2")?).max(0.0);
                let r3 = cdr32_to_mm(ar.read_i32_le("rect r3")?).max(0.0);
                let corner_style = match ar.read_u8("rect corner_style").unwrap_or(0) {
                    1 => CornerStyle::Scalloped,
                    2 => CornerStyle::Chamfered,
                    _ => CornerStyle::Rounded,
                };
                let lock_corners = ar.read_u8("rect lock_corners").unwrap_or(1) != 0;
                out.shape = Some(ParametricShape::Rectangle {
                    width: w,
                    height: h,
                    corner_radii: [r0, r1, r2, r3],
                    corner_style,
                    lock_corners,
                });
            }
            LODA_ARG_ELLIPSE => {
                let rx = cdr32_to_mm(ar.read_i32_le("ellipse rx")?).abs();
                let ry = cdr32_to_mm(ar.read_i32_le("ellipse ry")?).abs();
                let mode = match ar.read_u8("ellipse mode").unwrap_or(0) {
                    1 => EllipseMode::Pie,
                    2 => EllipseMode::Arc,
                    _ => EllipseMode::Full,
                };
                let start_deg = sanitize_f64(ar.read_f64_le("ellipse start").unwrap_or(0.0), 0.0);
                let end_deg = sanitize_f64(ar.read_f64_le("ellipse end").unwrap_or(360.0), 360.0);
                out.shape = Some(ParametricShape::Ellipse {
                    rx,
                    ry,
                    mode,
                    start_deg,
                    end_deg,
                });
            }
            LODA_ARG_POLYGON => {
                let sides = ar.read_u32_le("poly sides")?.clamp(3, 512);
                let rx = cdr32_to_mm(ar.read_i32_le("poly rx")?).abs();
                let ry = cdr32_to_mm(ar.read_i32_le("poly ry")?).abs();
                out.shape = Some(ParametricShape::Polygon {
                    sides,
                    radius_x: rx,
                    radius_y: ry,
                });
            }
            LODA_ARG_STAR => {
                let points = ar.read_u32_le("star points")?.clamp(3, 256);
                let outer_radius = cdr32_to_mm(ar.read_i32_le("star outer_radius")?).abs();
                let inner_ratio = sanitize_f64(ar.read_f64_le("star inner_ratio")?, 0.5);
                let complex = ar.read_u8("star complex")? != 0;
                out.shape = Some(ParametricShape::Star {
                    points,
                    outer_radius,
                    inner_ratio,
                    complex,
                });
            }
            LODA_ARG_EXTRA_SHAPE => {
                out.shape = Some(parse_extra_shape(&mut ar)?);
            }
            LODA_ARG_BITMAP_REF => {
                let bmp_id = ar.read_u32_le("bmp ref id")?;
                let width_mm = cdr32_to_mm(ar.read_i32_le("bmp ref w")?).abs();
                let height_mm = cdr32_to_mm(ar.read_i32_le("bmp ref h")?).abs();
                let has_crop = ar.read_u8("bmp has_crop").unwrap_or(0) != 0;
                let crop_rect = if has_crop {
                    let x0 = cdr32_to_mm(ar.read_i32_le("bmp crop x0")?);
                    let y0 = cdr32_to_mm(ar.read_i32_le("bmp crop y0")?);
                    let x1 = cdr32_to_mm(ar.read_i32_le("bmp crop x1")?);
                    let y1 = cdr32_to_mm(ar.read_i32_le("bmp crop y1")?);
                    Some(Rect2D::new(x0, y0, x1, y1))
                } else {
                    None
                };
                out.bitmap_ref = Some(ParsedBitmapRef {
                    bmp_id,
                    width_mm,
                    height_mm,
                    crop_rect,
                });
            }
            LODA_ARG_TEXT_META => {
                let is_paragraph = ar.read_u8("text is_paragraph")? != 0;
                let frame_w_mm = cdr32_to_mm(ar.read_i32_le("text frame_w")?).abs();
                let frame_h_mm = cdr32_to_mm(ar.read_i32_le("text frame_h")?).abs();
                let columns = ar.read_u32_le("text columns").unwrap_or(1).max(1);
                let gutter_mm =
                    cdr32_to_mm(ar.read_i32_le("text gutter").unwrap_or(50_000)).max(0.0);
                let linked_next_frame = if ar.remaining() >= 9 {
                    let has_link = ar.read_u8("text has_linked_next_frame")? != 0;
                    let rb = ar.read_bytes(8, "text linked_next_frame")?;
                    let raw_id = u64::from_le_bytes([
                        rb[0], rb[1], rb[2], rb[3], rb[4], rb[5], rb[6], rb[7],
                    ]);
                    has_link.then_some(ObjectId(raw_id))
                } else {
                    None
                };
                out.text_meta = Some(ParsedTextMeta {
                    is_paragraph,
                    frame_w_mm,
                    frame_h_mm,
                    columns,
                    gutter_mm,
                    linked_next_frame,
                });
            }
            LODA_ARG_FILL_ID if seg.len() >= 4 => {
                out.fill_id = Some(ar.read_u32_le("loda fill_id")?);
            }
            LODA_ARG_OUTLINE_ID if seg.len() >= 4 => {
                out.outline_id = Some(ar.read_u32_le("loda outline_id")?);
            }
            LODA_ARG_STYLE_ID if seg.len() >= 4 => {
                out.style_id = Some(ar.read_u32_le("loda style_id")?);
            }
            LODA_ARG_STYLE_JSON => {
                out.style_json = parse_c9_style_override(seg);
            }
            LODA_ARG_NAME if !seg.is_empty() => {
                let decoded = decode_cdr_text(seg, None, None);
                let trimmed = decoded.trim_matches('\0').trim();
                if !trimmed.is_empty() {
                    out.real_name = Some(trimmed.to_owned());
                }
            }
            LODA_ARG_OBJ_PROPS => {
                let flags = ar.read_u8("obj flags")?;
                let blend_mode = decode_blend_mode(ar.read_u8("obj blend_mode")?);
                let opacity =
                    sanitize_f32_local(ar.read_f32_le("obj opacity")?, 1.0).clamp(0.0, 1.0);
                let nlen = usize::from(ar.read_u16_le("obj name len")?);
                let nb = ar.read_bytes(nlen, "obj name")?;
                let name = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
                out.props = Some(ParsedObjProps {
                    name,
                    visible: (flags & 0x01) != 0,
                    locked: (flags & 0x02) != 0,
                    printable: (flags & 0x04) != 0,
                    blend_mode,
                    opacity,
                });
            }
            LODA_ARG_TABLE => {
                if let Ok(t) = serde_json::from_slice::<VectorTable>(seg) {
                    out.table = Some(t);
                }
            }
            LODA_ARG_DIMENSION => {
                if let Ok(d) = serde_json::from_slice::<DimensionObject>(seg) {
                    out.dimension = Some(d);
                }
            }
            LODA_ARG_CONNECTOR => {
                if let Ok(c) = serde_json::from_slice::<ConnectorObject>(seg) {
                    out.connector = Some(c);
                }
            }
            LODA_ARG_SYMBOL_INSTANCE => {
                if seg.len() == 16 {
                    let bytes: [u8; 16] = seg.try_into().unwrap_or([0; 16]);
                    out.symbol_instance = Some(SymbolId(uuid::Uuid::from_bytes(bytes)));
                } else if let Ok(sid) = serde_json::from_slice::<SymbolId>(seg) {
                    out.symbol_instance = Some(sid);
                }
            }
            LODA_ARG_TEXT_ON_PATH => {
                if let Ok(top) = serde_json::from_slice::<TextOnPath>(seg) {
                    out.text_on_path = Some(top);
                }
            }
            LODA_ARG_POWERCLIP_META => {
                if let Ok(pm) = serde_json::from_slice::<PowerClipLodaMeta>(seg) {
                    out.powerclip_meta = Some(pm);
                }
            }
            LODA_ARG_EFFECTS => {
                if let Ok(effs) = serde_json::from_slice::<Vec<Effect>>(seg) {
                    out.effects = effs;
                }
            }
            LODA_ARG_OBJECT_STYLE_LINK => {
                if seg.len() == 16 {
                    let bytes: [u8; 16] = seg.try_into().unwrap_or([0; 16]);
                    out.object_style_id = Some(ObjectStyleId(uuid::Uuid::from_bytes(bytes)));
                } else if let Ok(sid) = serde_json::from_slice::<ObjectStyleId>(seg) {
                    out.object_style_id = Some(sid);
                }
            }
            LODA_ARG_SOURCE_OID if seg.len() >= 8 => {
                out.source_oid = Some(ObjectId(u64::from_le_bytes([
                    seg[0], seg[1], seg[2], seg[3], seg[4], seg[5], seg[6], seg[7],
                ])));
            }
            LODA_ARG_LAYER_PROPS => {}
            LODA_ARG_BSPLINE => {
                if let Ok(bs) = serde_json::from_slice::<BSplineCurve>(seg) {
                    out.bspline = Some(bs);
                }
            }
            LODA_ARG_ARTISTIC_MEDIA => {
                if let Ok((spine, style)) =
                    serde_json::from_slice::<(BezierPath, ArtisticMediaStyle)>(seg)
                {
                    out.artistic_media = Some((spine, style));
                }
            }
            LODA_ARG_CLONE => {
                if let Ok((master_id, lf, lo, ls)) =
                    serde_json::from_slice::<(ObjectId, bool, bool, bool)>(seg)
                {
                    out.clone_ref = Some((master_id, lf, lo, ls));
                }
            }
            LODA_ARG_PERSPECTIVE_3X3 => {
                if let Ok(p) = serde_json::from_slice::<Perspective3x3>(seg) {
                    out.perspective = Some(p);
                }
            }
            LODA_ARG_COLOR_STYLE_LINK => {
                if let Ok(cs) = serde_json::from_slice::<(Option<ColorStyleId>, Option<usize>)>(seg)
                {
                    out.color_style_link = Some(cs);
                }
            }
            other => {
                out.unknown_args.push((other, seg.to_vec()));
            }
        }
    }

    Ok(out)
}

#[inline]
fn sanitize_f32_local(v: f32, default: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        default
    }
}

fn parse_real_rect_arg(seg: &[u8]) -> Option<CdrNativeRect> {
    if seg.len() >= 32 {
        let mut r = ByteReader::new(seg);
        let w_raw = sanitize_f64(r.read_f64_le("rect f64 w").ok()?, 0.0);
        let h_raw = sanitize_f64(r.read_f64_le("rect f64 h").ok()?, 0.0);
        if w_raw.abs() > 10.0 && h_raw.abs() > 10.0 && w_raw.abs() < 1e11 && h_raw.abs() < 1e11 {
            let local_w_mm = w_raw.abs() / coree_core::math::CDR_UNITS_PER_MM;
            let local_h_mm = h_raw.abs() / coree_core::math::CDR_UNITS_PER_MM;
            let mut corners = [0.0_f64; 4];
            let mut corner_style = CornerStyle::Rounded;
            let mut relative_corners = true;

            if seg.len() >= 128 {
                let mut any_non_zero_style = None;
                for (i, slot) in corners.iter_mut().enumerate() {
                    let base = 32 + i * 24;
                    let abs_r = i32::from_le_bytes([
                        seg[base],
                        seg[base + 1],
                        seg[base + 2],
                        seg[base + 3],
                    ]);
                    let mode_flag = u32::from_le_bytes([
                        seg[base + 4],
                        seg[base + 5],
                        seg[base + 6],
                        seg[base + 7],
                    ]);
                    let frac = sanitize_f64(
                        f64::from_le_bytes([
                            seg[base + 8],
                            seg[base + 9],
                            seg[base + 10],
                            seg[base + 11],
                            seg[base + 12],
                            seg[base + 13],
                            seg[base + 14],
                            seg[base + 15],
                        ]),
                        0.0,
                    );
                    let style_raw = u32::from_le_bytes([
                        seg[base + 16],
                        seg[base + 17],
                        seg[base + 18],
                        seg[base + 19],
                    ]);
                    if mode_flag == 0x7FFF || frac > 1e-9 {
                        *slot = frac.clamp(0.0, 1.0);
                    } else if abs_r > 0 {
                        relative_corners = false;
                        *slot = cdr32_to_mm(abs_r);
                    }
                    if *slot > 1e-9 || any_non_zero_style.is_none() {
                        let st = match style_raw {
                            1 => CornerStyle::Scalloped,
                            2 => CornerStyle::Chamfered,
                            _ => CornerStyle::Rounded,
                        };
                        if *slot > 1e-9 {
                            any_non_zero_style = Some(st);
                        } else {
                            corner_style = st;
                        }
                    }
                }
                if let Some(st) = any_non_zero_style {
                    corner_style = st;
                }
            }
            return Some(CdrNativeRect {
                local_w_mm,
                local_h_mm,
                corners,
                relative_corners,
                corner_style,
            });
        }
    }

    if seg.len() >= 8 {
        let local_w_mm = cdr32_to_mm(i32::from_le_bytes([seg[0], seg[1], seg[2], seg[3]])).abs();
        let local_h_mm = cdr32_to_mm(i32::from_le_bytes([seg[4], seg[5], seg[6], seg[7]])).abs();
        let mut corners = [0.0_f64; 4];
        if seg.len() >= 24 {
            for (i, slot) in corners.iter_mut().enumerate() {
                let b = 8 + i * 4;
                *slot = cdr32_to_mm(i32::from_le_bytes([
                    seg[b],
                    seg[b + 1],
                    seg[b + 2],
                    seg[b + 3],
                ]))
                .max(0.0);
            }
        } else if seg.len() >= 12 {
            let r = cdr32_to_mm(i32::from_le_bytes([seg[8], seg[9], seg[10], seg[11]])).max(0.0);
            corners = [r; 4];
        }
        return Some(CdrNativeRect {
            local_w_mm,
            local_h_mm,
            corners,
            relative_corners: false,
            corner_style: CornerStyle::Rounded,
        });
    }
    None
}

fn parse_real_ellipse_arg(seg: &[u8]) -> Option<CdrNativeEllipse> {
    if seg.len() < 8 {
        return None;
    }
    let local_w_mm = cdr32_to_mm(i32::from_le_bytes([seg[0], seg[1], seg[2], seg[3]])).abs();
    let local_h_mm = cdr32_to_mm(i32::from_le_bytes([seg[4], seg[5], seg[6], seg[7]])).abs();
    let start_deg = if seg.len() >= 12 {
        f64::from(i32::from_le_bytes([seg[8], seg[9], seg[10], seg[11]])) / 1_000_000.0
    } else {
        0.0
    };
    let end_deg = if seg.len() >= 16 {
        f64::from(i32::from_le_bytes([seg[12], seg[13], seg[14], seg[15]])) / 1_000_000.0
    } else {
        0.0
    };
    let pie_flag = if seg.len() >= 20 {
        u32::from_le_bytes([seg[16], seg[17], seg[18], seg[19]])
    } else {
        0
    };
    let (mode, s_deg, e_deg) = if (start_deg - end_deg).abs() < 1e-4 {
        (EllipseMode::Full, 0.0, 360.0)
    } else if pie_flag != 0 {
        (EllipseMode::Pie, start_deg, end_deg)
    } else {
        (EllipseMode::Arc, start_deg, end_deg)
    };
    Some(CdrNativeEllipse {
        local_w_mm,
        local_h_mm,
        mode,
        start_deg: s_deg,
        end_deg: e_deg,
    })
}

fn parse_extra_shape(r: &mut ByteReader<'_>) -> CdrResult<ParametricShape> {
    let tag = r.read_u8("extra shape tag")?;
    match tag {
        1 => {
            let revolutions = sanitize_f64(r.read_f64_le("spiral rev")?, 3.0);
            let radius = cdr32_to_mm(r.read_i32_le("spiral rad")?).abs();
            let logarithmic = r.read_u8("spiral log")? != 0;
            let expansion = sanitize_f64(r.read_f64_le("spiral exp")?, 2.0);
            Ok(ParametricShape::Spiral {
                revolutions,
                radius,
                logarithmic,
                expansion,
            })
        }
        2 => {
            let cols = r.read_u32_le("gp cols")?.clamp(1, 200);
            let rows = r.read_u32_le("gp rows")?.clamp(1, 200);
            let width = cdr32_to_mm(r.read_i32_le("gp w")?).abs();
            let height = cdr32_to_mm(r.read_i32_le("gp h")?).abs();
            Ok(ParametricShape::GraphPaper {
                cols,
                rows,
                width,
                height,
            })
        }
        3 => {
            let rays = r.read_u32_le("impact rays")?.clamp(3, 512);
            let inner_radius = cdr32_to_mm(r.read_i32_le("impact r_in")?).abs();
            let outer_radius = cdr32_to_mm(r.read_i32_le("impact r_out")?).abs();
            let style = if r.read_u8("impact style")? == 1 {
                ImpactStyle::Parallel
            } else {
                ImpactStyle::Radial
            };
            Ok(ParametricShape::Impact {
                rays,
                inner_radius,
                outer_radius,
                style,
            })
        }
        4 => {
            let kind = match r.read_u8("basic shape kind")? {
                1 => BasicShapeKind::ArrowLeft,
                2 => BasicShapeKind::DoubleArrow,
                3 => BasicShapeKind::Banner,
                4 => BasicShapeKind::Callout,
                5 => BasicShapeKind::Heart,
                6 => BasicShapeKind::Cross,
                7 => BasicShapeKind::Diamond,
                8 => BasicShapeKind::Trapezoid,
                _ => BasicShapeKind::ArrowRight,
            };
            let width = cdr32_to_mm(r.read_i32_le("basic w")?).abs();
            let height = cdr32_to_mm(r.read_i32_le("basic h")?).abs();
            Ok(ParametricShape::BasicShape {
                kind,
                width,
                height,
            })
        }
        other => Err(CdrError::InvalidChunk {
            fourcc: "loda".to_owned(),
            reason: format!("unknown extra shape tag {other}"),
        }),
    }
}

/// Parses a CDR point+flag curve array (`u32 point_count`, `(x: i32, y: i32) * N`, `flags: u8 * N`).
pub(crate) fn parse_loda_curve(r: &mut ByteReader<'_>) -> CdrResult<BezierPath> {
    let count = usize::try_from(r.read_u32_le("curve point count")?)
        .map_err(|_| CdrError::ArithmeticOverflow("curve point count"))?;
    if count > crate::limits::MAX_CDR_VERTICES {
        return Err(CdrError::InvalidChunk {
            fourcc: "loda".to_owned(),
            reason: format!(
                "curve point count {count} exceeds limit {}",
                crate::limits::MAX_CDR_VERTICES
            ),
        });
    }
    let needed = count
        .checked_mul(9)
        .ok_or(CdrError::ArithmeticOverflow("curve byte size"))?;
    if needed > r.remaining() {
        return Err(CdrError::UnexpectedEof {
            context: "loda curve points",
            needed,
            available: r.remaining(),
        });
    }

    let mut pts = Vec::with_capacity(count);
    for _ in 0..count {
        let x = cdr32_to_mm(r.read_i32_le("curve pt x")?);
        let y = cdr32_to_mm(r.read_i32_le("curve pt y")?);
        pts.push(Point2D::new(x, y));
    }
    let flags = r.read_bytes(count, "curve flags")?;
    Ok(decode_curve_points(&pts, flags))
}

/// Reconstructs a [`BezierPath`] from parallel `(points, node_flags)` arrays.
#[must_use]
pub(crate) fn decode_curve_points(pts: &[Point2D], flags: &[u8]) -> BezierPath {
    let mut subpaths: Vec<Subpath> = Vec::new();
    let mut current: Option<Subpath> = None;
    let mut pending_ctrls: Vec<(Point2D, bool)> = Vec::new();

    for (&pt, &flag) in pts.iter().zip(flags.iter()) {
        let kind_bits = flag & NODE_KIND_MASK;
        let closed = (flag & NODE_FLAG_CLOSED) != 0;
        let cont = match flag & NODE_CONT_MASK {
            NODE_CONT_SMOOTH => NodeKind::Smooth,
            NODE_CONT_SYMMETRICAL => NodeKind::Symmetrical,
            _ => NodeKind::Cusp,
        };

        match kind_bits {
            NODE_MOVE_TO => {
                if let Some(sp) = current.take() {
                    if !sp.nodes.is_empty() {
                        subpaths.push(sp);
                    }
                }
                pending_ctrls.clear();
                let mut sp = Subpath::new(closed);
                sp.nodes.push(PathNode::new(pt, None, None, cont));
                current = Some(sp);
            }
            NODE_CTRL_PT => {
                let present = (flag & NODE_CTRL_ABSENT) == 0;
                pending_ctrls.push((pt, present));
            }
            NODE_LINE_TO => {
                pending_ctrls.clear();
                let sp = current.get_or_insert_with(|| Subpath::new(false));
                if closed {
                    sp.closed = true;
                    if sp
                        .nodes
                        .first()
                        .is_some_and(|n0| n0.pos.distance_to(pt) < 1e-4)
                    {
                        if let Some(finished) = current.take() {
                            subpaths.push(finished);
                        }
                        continue;
                    }
                }
                sp.nodes.push(PathNode::new(pt, None, None, cont));
                if closed {
                    if let Some(finished) = current.take() {
                        subpaths.push(finished);
                    }
                }
            }
            NODE_CURVE_TO => {
                let sp = current.get_or_insert_with(|| Subpath::new(false));
                let (c1, c2) = if pending_ctrls.len() >= 2 {
                    let (p1, has1) = pending_ctrls[0];
                    let (p2, has2) = pending_ctrls[1];
                    (has1.then_some(p1), has2.then_some(p2))
                } else {
                    (None, None)
                };
                pending_ctrls.clear();

                if let Some(prev) = sp.nodes.last_mut() {
                    prev.handle_out = c1;
                }
                if closed {
                    sp.closed = true;
                    if sp
                        .nodes
                        .first()
                        .is_some_and(|n0| n0.pos.distance_to(pt) < 1e-4)
                    {
                        if let Some(n0) = sp.nodes.first_mut() {
                            n0.handle_in = c2;
                        }
                        if let Some(finished) = current.take() {
                            subpaths.push(finished);
                        }
                        continue;
                    }
                }
                sp.nodes.push(PathNode::new(pt, c2, None, cont));
                if closed {
                    if let Some(finished) = current.take() {
                        subpaths.push(finished);
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(sp) = current {
        if !sp.nodes.is_empty() {
            subpaths.push(sp);
        }
    }

    BezierPath { subpaths }
}

/// Encodes a [`BezierPath`] into parallel `(points, node_flags)` arrays preserving
/// exact `NodeKind` (`Cusp`, `Smooth`, `Symmetrical`), cubic handles, and subpath closure.
#[must_use]
pub(crate) fn encode_curve_points(path: &BezierPath) -> (Vec<Point2D>, Vec<u8>) {
    let mut pts = Vec::new();
    let mut flags = Vec::new();

    for sp in &path.subpaths {
        if sp.nodes.is_empty() {
            continue;
        }
        let first = sp.nodes[0];
        let mut first_flag = NODE_MOVE_TO | encode_node_kind(first.kind);
        if sp.closed && sp.nodes.len() == 1 {
            first_flag |= NODE_FLAG_CLOSED;
        }
        pts.push(first.pos);
        flags.push(first_flag);

        let seg_count = sp.segment_count();
        for s in 0..seg_count {
            let n0 = sp.nodes[s];
            let next_idx = (s + 1) % sp.nodes.len();
            let n1 = sp.nodes[next_idx];
            let is_closing = sp.closed && next_idx == 0;
            let closed_bit = if is_closing { NODE_FLAG_CLOSED } else { 0 };

            if n0.handle_out.is_none() && n1.handle_in.is_none() {
                pts.push(n1.pos);
                flags.push(NODE_LINE_TO | encode_node_kind(n1.kind) | closed_bit);
            } else {
                let (c1, f1) = match n0.handle_out {
                    Some(h) => (h, NODE_CTRL_PT | NODE_CTRL_PRESENT),
                    None => (n0.pos, NODE_CTRL_PT | NODE_CTRL_ABSENT),
                };
                let (c2, f2) = match n1.handle_in {
                    Some(h) => (h, NODE_CTRL_PT | NODE_CTRL_PRESENT),
                    None => (n1.pos, NODE_CTRL_PT | NODE_CTRL_ABSENT),
                };
                pts.push(c1);
                flags.push(f1);
                pts.push(c2);
                flags.push(f2);
                pts.push(n1.pos);
                flags.push(NODE_CURVE_TO | encode_node_kind(n1.kind) | closed_bit);
            }
        }
    }

    (pts, flags)
}

fn encode_node_kind(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Cusp => NODE_CONT_CUSP,
        NodeKind::Smooth => NODE_CONT_SMOOTH,
        NodeKind::Symmetrical => NODE_CONT_SYMMETRICAL,
    }
}

/// Encodes `args` into an authentic 20-byte CDR `loda` chunk with reversed `types` table
/// (`[chunk_len, num_args, args_offset, types_offset, chunk_type]`, offsets with trailing
/// `end_offset`, reversed `types`, then concatenated payloads).
pub(crate) fn encode_loda_args(chunk_type: u32, args: &[(u32, Vec<u8>)]) -> Vec<u8> {
    if args.is_empty() {
        let mut out = Vec::with_capacity(20);
        out.extend_from_slice(&20u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&20u32.to_le_bytes());
        out.extend_from_slice(&20u32.to_le_bytes());
        out.extend_from_slice(&chunk_type.to_le_bytes());
        return out;
    }

    let num_args = args.len() as u32;
    let args_offset = 20u32;
    let offsets_bytes = (args.len() + 1) * 4;
    let types_offset = args_offset + offsets_bytes as u32;
    let data_start = types_offset as usize + args.len() * 4;

    let payload_total: usize = args.iter().map(|(_, d)| d.len()).sum();
    let total_len = data_start + payload_total;

    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&(total_len as u32).to_le_bytes());
    out.extend_from_slice(&num_args.to_le_bytes());
    out.extend_from_slice(&args_offset.to_le_bytes());
    out.extend_from_slice(&types_offset.to_le_bytes());
    out.extend_from_slice(&chunk_type.to_le_bytes());

    let mut cursor = data_start;
    for (_, data) in args {
        out.extend_from_slice(&(cursor as u32).to_le_bytes());
        cursor += data.len();
    }
    out.extend_from_slice(&(cursor as u32).to_le_bytes()); // trailing end_offset

    // Native CDR stores argument types in REVERSE order relative to offsets
    for (typ, _) in args.iter().rev() {
        out.extend_from_slice(&typ.to_le_bytes());
    }

    for (_, data) in args {
        out.extend_from_slice(data);
    }
    out
}

pub(crate) fn encode_obj_props(obj: &SceneObject) -> Vec<u8> {
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
    let nb = obj.name.as_bytes();
    let nlen = nb.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(8 + nlen);
    out.push(flags);
    out.push(encode_blend_mode(obj.blend_mode));
    out.extend_from_slice(&sanitize_f32_local(obj.opacity, 1.0).to_le_bytes());
    out.extend_from_slice(&(nlen as u16).to_le_bytes());
    out.extend_from_slice(&nb[..nlen]);
    out
}

pub(crate) fn encode_loda_curve_arg(path: &BezierPath) -> Vec<u8> {
    let (pts, flags) = encode_curve_points(path);
    let mut out = Vec::with_capacity(4 + pts.len() * 9);
    out.extend_from_slice(&(pts.len() as u32).to_le_bytes());
    for pt in &pts {
        out.extend_from_slice(&mm_to_cdr32(pt.x).to_le_bytes());
        out.extend_from_slice(&mm_to_cdr32(pt.y).to_le_bytes());
    }
    out.extend_from_slice(&flags);
    out
}

pub(crate) fn encode_shape_loda_arg(shape: &ParametricShape, args: &mut Vec<(u32, Vec<u8>)>) {
    match *shape {
        ParametricShape::Rectangle {
            width,
            height,
            corner_radii,
            corner_style,
            lock_corners,
        } => {
            let mut b = Vec::with_capacity(26);
            b.extend_from_slice(&mm_to_cdr32(width).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(height).to_le_bytes());
            for r in corner_radii {
                b.extend_from_slice(&mm_to_cdr32(r).to_le_bytes());
            }
            b.push(match corner_style {
                CornerStyle::Rounded => 0,
                CornerStyle::Scalloped => 1,
                CornerStyle::Chamfered => 2,
            });
            b.push(u8::from(lock_corners));
            args.push((LODA_ARG_RECTANGLE, b));
        }
        ParametricShape::Ellipse {
            rx,
            ry,
            mode,
            start_deg,
            end_deg,
        } => {
            let mut b = Vec::with_capacity(25);
            b.extend_from_slice(&mm_to_cdr32(rx).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(ry).to_le_bytes());
            b.push(match mode {
                EllipseMode::Full => 0,
                EllipseMode::Pie => 1,
                EllipseMode::Arc => 2,
            });
            b.extend_from_slice(&sanitize_f64(start_deg, 0.0).to_le_bytes());
            b.extend_from_slice(&sanitize_f64(end_deg, 360.0).to_le_bytes());
            args.push((LODA_ARG_ELLIPSE, b));
        }
        ParametricShape::Polygon {
            sides,
            radius_x,
            radius_y,
        } => {
            let mut b = Vec::with_capacity(12);
            b.extend_from_slice(&sides.to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(radius_x).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(radius_y).to_le_bytes());
            args.push((LODA_ARG_POLYGON, b));
        }
        ParametricShape::Star {
            points,
            outer_radius,
            inner_ratio,
            complex,
        } => {
            let mut b = Vec::with_capacity(17);
            b.extend_from_slice(&points.to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(outer_radius).to_le_bytes());
            b.extend_from_slice(&sanitize_f64(inner_ratio, 0.5).to_le_bytes());
            b.push(u8::from(complex));
            args.push((LODA_ARG_STAR, b));
        }
        ParametricShape::Spiral {
            revolutions,
            radius,
            logarithmic,
            expansion,
        } => {
            let mut b = Vec::with_capacity(22);
            b.push(1);
            b.extend_from_slice(&sanitize_f64(revolutions, 3.0).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(radius).to_le_bytes());
            b.push(u8::from(logarithmic));
            b.extend_from_slice(&sanitize_f64(expansion, 2.0).to_le_bytes());
            args.push((LODA_ARG_EXTRA_SHAPE, b));
        }
        ParametricShape::GraphPaper {
            cols,
            rows,
            width,
            height,
        } => {
            let mut b = Vec::with_capacity(17);
            b.push(2);
            b.extend_from_slice(&cols.to_le_bytes());
            b.extend_from_slice(&rows.to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(width).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(height).to_le_bytes());
            args.push((LODA_ARG_EXTRA_SHAPE, b));
        }
        ParametricShape::Impact {
            rays,
            inner_radius,
            outer_radius,
            style,
        } => {
            let mut b = Vec::with_capacity(14);
            b.push(3);
            b.extend_from_slice(&rays.to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(inner_radius).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(outer_radius).to_le_bytes());
            b.push(if style == ImpactStyle::Parallel { 1 } else { 0 });
            args.push((LODA_ARG_EXTRA_SHAPE, b));
        }
        ParametricShape::BasicShape {
            kind,
            width,
            height,
        } => {
            let mut b = Vec::with_capacity(10);
            b.push(4);
            b.push(match kind {
                BasicShapeKind::ArrowRight => 0,
                BasicShapeKind::ArrowLeft => 1,
                BasicShapeKind::DoubleArrow => 2,
                BasicShapeKind::Banner => 3,
                BasicShapeKind::Callout => 4,
                BasicShapeKind::Heart => 5,
                BasicShapeKind::Cross => 6,
                BasicShapeKind::Diamond => 7,
                BasicShapeKind::Trapezoid => 8,
            });
            b.extend_from_slice(&mm_to_cdr32(width).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(height).to_le_bytes());
            args.push((LODA_ARG_EXTRA_SHAPE, b));
        }
    }
}
