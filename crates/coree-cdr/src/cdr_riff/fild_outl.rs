// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDR `fild` (fills/gradients/patterns/meshes), `outl` (outlines/dashes/arrows),
//! `arrw` (arrowheads), `vpat` (vector pattern tiles), and `clrs` (color styles).

use super::legacy_v3_v5::{parse_v3_v5_fild, parse_v3_v5_outl};
use super::loda::decode_curve_points;
use super::parser::ParseContext;
use crate::coord::{
    cdr32_to_mm, decode_cdr_color, decode_cdr_text, derive_spot_fallback_cmyk, mm_to_cdr32,
    read_tagged_color, write_tagged_color, CODEPAGE_UTF8,
};
use crate::error::{CdrError, CdrResult};
use crate::limits::ByteReader;
use crate::riff::RiffNode;
use coree_core::{
    math::sanitize_f32, math::sanitize_f64, ArrowheadStyle, BezierPath, Color, ColorStyle,
    Document, Fill, GradientStop, ImageAssetId, LineCap, LineJoin, MeshNode, ObjectId,
    OutlineStyle, Point2D, Rect2D, SceneObject, SymbolDef, SymbolId,
};
use std::collections::HashMap;

pub(crate) fn parse_arrw_list(children: &[RiffNode], ctx: &mut ParseContext<'_>) {
    for node in children {
        if let RiffNode::Chunk { data, .. } = node {
            if data.len() >= 8 {
                let id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let arrow_style = if data.len() >= 10 {
                    let style_tag = data[6];
                    if style_tag != 0 && style_tag <= 6 {
                        decode_arrow(style_tag)
                    } else {
                        let node_cnt = usize::from(u16::from_le_bytes([data[4], data[5]]));
                        match node_cnt {
                            3 => ArrowheadStyle::Triangle,
                            4 => ArrowheadStyle::Diamond,
                            6 => ArrowheadStyle::Stealth,
                            _ => ArrowheadStyle::Triangle,
                        }
                    }
                } else {
                    decode_arrow(data[4])
                };
                if !matches!(arrow_style, ArrowheadStyle::None) {
                    ctx.arrows.insert(id, arrow_style);
                }
            }
        }
    }
}

pub(crate) fn parse_bmpf_list(
    children: &[RiffNode],
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<()> {
    for node in children {
        match node {
            RiffNode::Chunk { fourcc, data, .. } if fourcc == b"bmp " || fourcc == b"bmpf" => {
                if data.len() >= 8 {
                    let pat_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                    let before_len = doc.assets().len();
                    if super::parser::register_bmp_chunk_payload(data, Some(pat_id), doc, ctx)
                        .is_ok()
                        && (doc.assets().len() > before_len || ctx.bitmaps.contains_key(&pat_id))
                    {
                        if let Some(&aid) = ctx.bitmaps.get(&pat_id) {
                            ctx.pattern_bitmaps.insert(pat_id, aid);
                        }
                    }
                }
            }
            RiffNode::List { children: sub, .. } => parse_bmpf_list(sub, doc, ctx)?,
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn parse_vpat_list(
    children: &[RiffNode],
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) {
    for node in children {
        if let RiffNode::Chunk { data, .. } = node {
            if data.len() >= 12 {
                let pat_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let fg = find_embedded_cdr_color(&data[4..]).unwrap_or_else(|| Color::Spot {
                    name: format!("VPAT_{pat_id}"),
                    tint: 1.0,
                    fallback_cmyk: derive_spot_fallback_cmyk(&format!("VPAT_{pat_id}")),
                    alpha: 1.0,
                });
                // Decode optional tile dimensions + curve geometry when present (`>= 20` bytes with pt_count)
                let (tile_w, tile_h, tile_path) = if data.len() >= 20 {
                    let tw =
                        cdr32_to_mm(i32::from_le_bytes([data[4], data[5], data[6], data[7]])).abs();
                    let th =
                        cdr32_to_mm(i32::from_le_bytes([data[8], data[9], data[10], data[11]]))
                            .abs();
                    let w_mm = if (0.5..=500.0).contains(&tw) {
                        tw
                    } else {
                        10.0
                    };
                    let h_mm = if (0.5..=500.0).contains(&th) {
                        th
                    } else {
                        10.0
                    };
                    let pt_cnt =
                        u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
                    let path = if (2..=512).contains(&pt_cnt) && 16 + pt_cnt * 9 <= data.len() {
                        let mut pts = Vec::with_capacity(pt_cnt);
                        for i in 0..pt_cnt {
                            let b = 16 + i * 8;
                            let x = cdr32_to_mm(i32::from_le_bytes([
                                data[b],
                                data[b + 1],
                                data[b + 2],
                                data[b + 3],
                            ]));
                            let y = cdr32_to_mm(i32::from_le_bytes([
                                data[b + 4],
                                data[b + 5],
                                data[b + 6],
                                data[b + 7],
                            ]));
                            pts.push(Point2D::new(x, y));
                        }
                        let flags = &data[16 + pt_cnt * 8..16 + pt_cnt * 9];
                        let p = decode_curve_points(&pts, flags);
                        if p.subpaths.is_empty() {
                            BezierPath::from_rect(Rect2D::from_xywh(
                                0.0,
                                0.0,
                                w_mm * 0.8,
                                h_mm * 0.8,
                            ))
                        } else {
                            p
                        }
                    } else {
                        BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, w_mm * 0.8, h_mm * 0.8))
                    };
                    (w_mm, h_mm, path)
                } else {
                    (
                        10.0,
                        10.0,
                        BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 8.0, 8.0)),
                    )
                };

                let tile_obj = SceneObject::new(
                    ObjectId(doc.alloc_id()),
                    format!("VectorPattern Tile {pat_id}"),
                    coree_core::NodePayload::Path(tile_path),
                )
                .with_fill(Fill::Solid(fg))
                .with_outline(OutlineStyle::none());
                let sym = SymbolDef::new(format!("VPAT {pat_id}"), vec![tile_obj]);
                let symbol_id = sym.id;
                doc.insert_symbol(sym);
                let fill = Fill::VectorPattern {
                    symbol_id,
                    tile_w,
                    tile_h,
                };
                ctx.vector_patterns.insert(pat_id, fill);
            }
        }
    }
}

pub(crate) fn parse_clrs_list(
    children: &[RiffNode],
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) {
    for node in children {
        if let RiffNode::Chunk { data, .. } = node {
            if data.len() >= 16 {
                let color_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                if let Some(color) = find_embedded_cdr_color(&data[4..])
                    .or_else(|| decode_cdr_native_color_record(&data[4..]))
                {
                    let label = format!("Color Style {color_id}");
                    doc.document_palette_mut().add_unique(&label, color.clone());
                    doc.upsert_color_style(ColorStyle::new(label, color.clone(), None));
                    ctx.color_styles.insert(color_id, color);
                }
            }
        }
    }
}

/// Decodes a CDR 12-byte (or variable-length Spot) inline color record.
pub(crate) fn decode_cdr_native_color_record(raw: &[u8]) -> Option<Color> {
    if raw.len() < 12 {
        return None;
    }
    let model_id = u16::from_le_bytes([raw[0], raw[1]]);
    let payload = if raw.len() > 12 && matches!(model_id, 1 | 14 | 15 | 16 | 18 | 25) {
        &raw[8..]
    } else {
        &raw[8..12]
    };
    decode_cdr_color(model_id, payload, None).ok()
}

pub(crate) fn find_embedded_cdr_color(slice: &[u8]) -> Option<Color> {
    if slice.len() < 17 {
        return None;
    }
    for i in 0..=slice.len() - 17 {
        if slice[i] == 0x01
            && slice[i + 1] == 0x0C
            && slice[i + 2] == 0x00
            && slice[i + 3] == 0x00
            && slice[i + 4] == 0x00
        {
            if let Some(c) = decode_cdr_native_color_record(&slice[i + 5..i + 17]) {
                return Some(c);
            }
        }
    }
    None
}

pub(crate) fn rescale_normalized_fill_to_bounds(fill: &mut Fill, bb: Rect2D) {
    let w = bb.width().max(1e-3);
    let h = bb.height().max(1e-3);
    match fill {
        Fill::LinearGradient { start, end, .. } => {
            *start = Point2D::new(bb.min_x + start.x * w, bb.min_y + start.y * h);
            *end = Point2D::new(bb.min_x + end.x * w, bb.min_y + end.y * h);
        }
        Fill::RadialGradient {
            center,
            radius,
            focal,
            ..
        } => {
            *center = Point2D::new(bb.min_x + center.x * w, bb.min_y + center.y * h);
            *focal = Point2D::new(bb.min_x + focal.x * w, bb.min_y + focal.y * h);
            *radius = (*radius * w.max(h)).max(0.5);
        }
        Fill::ConicalGradient { center, .. } => {
            *center = Point2D::new(bb.min_x + center.x * w, bb.min_y + center.y * h);
        }
        Fill::SquareGradient { center, size, .. } => {
            *center = Point2D::new(bb.min_x + center.x * w, bb.min_y + center.y * h);
            *size = (*size * w.max(h)).max(0.5);
        }
        _ => {}
    }
}

pub(crate) fn parse_real_fild_v16(data: &[u8]) -> CdrResult<(u32, Fill, bool)> {
    let mut r = ByteReader::new(data);
    let fill_id = r.read_u32_le("real fild id")?;
    if data.len() < 14 {
        return Ok((fill_id, Fill::None, true));
    }
    let sub_tag = u16::from_le_bytes([data[4], data[5]]);
    let fill_type = if sub_tag > 16 || data.len() == 22 {
        u16::from_le_bytes([data[12], data[13]])
    } else {
        sub_tag
    };

    let collect_embedded_stops = |slice: &[u8], limit: usize| -> Vec<(Option<f32>, Color)> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos + 17 <= slice.len() && out.len() < limit {
            if slice[pos] == 0x01
                && slice[pos + 1] == 0x0C
                && slice[pos + 2] == 0x00
                && slice[pos + 3] == 0x00
                && slice[pos + 4] == 0x00
            {
                if let Some(c) = decode_cdr_native_color_record(&slice[pos + 5..pos + 17]) {
                    // Check if a valid 0..100% stop offset precedes/follows in native CDR table
                    let stop_pos = if pos + 21 <= slice.len() {
                        let raw_pct = u32::from_le_bytes([
                            slice[pos + 17],
                            slice[pos + 18],
                            slice[pos + 19],
                            slice[pos + 20],
                        ]);
                        (raw_pct <= 100 && slice.get(pos + 21) != Some(&0x01))
                            .then_some(raw_pct as f32 / 100.0)
                    } else {
                        None
                    };
                    out.push((stop_pos, c));
                }
                pos += 17;
            } else {
                pos += 1;
            }
        }
        out
    };

    let fill = match fill_type {
        0 => Fill::None,
        1 => {
            let color = find_embedded_cdr_color(&data[12..])
                .or_else(|| {
                    if data.len() >= 25 {
                        decode_cdr_native_color_record(&data[13..25])
                    } else {
                        None
                    }
                })
                .unwrap_or(Color::BLACK);
            Fill::Solid(color)
        }
        2 => {
            let fountain_sub = if data.len() >= 20 {
                u16::from_le_bytes([data[16], data[17]])
            } else {
                0
            };
            let angle_deg = if data.len() >= 28 {
                let raw_a = i32::from_le_bytes([data[24], data[25], data[26], data[27]]);
                let deg = f64::from(raw_a) / 1_000_000.0;
                if deg.is_finite() && deg.abs() <= 360.0 {
                    deg
                } else {
                    0.0
                }
            } else {
                0.0
            };
            let stops = collect_embedded_stops(&data[14..], 16);
            if stops.len() >= 2 {
                let last = (stops.len() - 1) as f32;
                let all_have_pos = stops.iter().all(|(p, _)| p.is_some());
                let grad_stops: Vec<GradientStop> = stops
                    .into_iter()
                    .enumerate()
                    .map(|(i, (pos_opt, c))| {
                        let off = if all_have_pos {
                            pos_opt.unwrap_or(i as f32 / last)
                        } else {
                            i as f32 / last
                        };
                        GradientStop::new(off, c)
                    })
                    .collect();
                match fountain_sub {
                    1 | 4 => Fill::RadialGradient {
                        center: Point2D::new(0.5, 0.5),
                        radius: 0.5,
                        focal: Point2D::new(0.5, 0.5),
                        stops: grad_stops,
                    },
                    2 => Fill::ConicalGradient {
                        center: Point2D::new(0.5, 0.5),
                        angle_deg,
                        stops: grad_stops,
                    },
                    3 => Fill::SquareGradient {
                        center: Point2D::new(0.5, 0.5),
                        size: 1.0,
                        angle_deg,
                        stops: grad_stops,
                    },
                    _ => {
                        let rad = angle_deg.to_radians();
                        let (s, c) = rad.sin_cos();
                        Fill::LinearGradient {
                            start: Point2D::new(0.5 - 0.5 * c, 0.5 - 0.5 * s),
                            end: Point2D::new(0.5 + 0.5 * c, 0.5 + 0.5 * s),
                            stops: grad_stops,
                        }
                    }
                }
            } else if let Some((_, c)) = stops.into_iter().next() {
                Fill::Solid(c)
            } else {
                Fill::None
            }
        }
        6 => {
            let cols = collect_embedded_stops(&data[14..], 2);
            let fg = cols.first().map(|(_, c)| c.clone()).unwrap_or(Color::BLACK);
            let bg = cols.get(1).map(|(_, c)| c.clone()).unwrap_or(Color::WHITE);
            Fill::TwoColorPattern {
                fg,
                bg,
                pattern_name: "Checker".to_owned(),
                tile_w: 8.0,
                tile_h: 8.0,
            }
        }
        9 => {
            let cols = collect_embedded_stops(&data[14..], 16);
            if cols.len() >= 4 {
                let nodes = cols
                    .into_iter()
                    .enumerate()
                    .map(|(i, (_, color))| {
                        let r = i / 2;
                        let c = i % 2;
                        MeshNode {
                            row: r,
                            col: c,
                            pos: Point2D::new(c as f64, r as f64),
                            color,
                        }
                    })
                    .collect();
                Fill::MeshFill {
                    rows: 2,
                    cols: 2,
                    nodes,
                }
            } else if let Some((_, fg)) = cols.first() {
                Fill::TextureFill {
                    preset: "marble".to_owned(),
                    seed: u64::from(fill_id).max(1),
                    fg: fg.clone(),
                    bg: cols.get(1).map(|(_, c)| c.clone()).unwrap_or(Color::WHITE),
                }
            } else {
                Fill::None
            }
        }
        7 | 10 | 11 => {
            let cols = collect_embedded_stops(&data[14..], 2);
            if cols.is_empty() {
                Fill::None
            } else {
                let fg = cols[0].1.clone();
                let bg = cols.get(1).map(|(_, c)| c.clone()).unwrap_or(Color::WHITE);
                Fill::TextureFill {
                    preset: "marble".to_owned(),
                    seed: u64::from(fill_id).max(1),
                    fg,
                    bg,
                }
            }
        }
        _ => Fill::None,
    };
    Ok((fill_id, fill, true))
}

fn is_real_fild_v16_signature(data: &[u8]) -> bool {
    if data.windows(5).any(|w| w == [0x01, 0x0C, 0x00, 0x00, 0x00]) {
        return true;
    }
    data.len() >= 14 && u16::from_le_bytes([data[4], data[5]]) > 16
}

fn parse_fild_chunk_clean(
    data: &[u8],
    ctx: Option<&ParseContext<'_>>,
) -> CdrResult<(u32, Fill, bool)> {
    let mut r = ByteReader::new(data);
    let fill_id = r.read_u32_le("fild id")?;
    let fill_type = r.read_u16_le("fild type")?;
    let fill = match fill_type {
        0 => Fill::None,
        1 => Fill::Solid(read_tagged_color(&mut r, "fild solid color")?),
        2 => {
            let subtype = r.read_u8("fild fountain subtype")?;
            let p0_x = cdr32_to_mm(r.read_i32_le("fild p0.x")?);
            let p0_y = cdr32_to_mm(r.read_i32_le("fild p0.y")?);
            let p1_x = cdr32_to_mm(r.read_i32_le("fild p1.x")?);
            let p1_y = cdr32_to_mm(r.read_i32_le("fild p1.y")?);
            let scalar = sanitize_f64(r.read_f64_le("fild scalar")?, 0.0);
            let angle = sanitize_f64(r.read_f64_le("fild angle")?, 0.0);
            let stop_count = usize::from(r.read_u16_le("fild stop_count")?);
            if stop_count > 1024 || stop_count.saturating_mul(8) > r.remaining() {
                return Err(CdrError::InvalidChunk {
                    fourcc: "fild".to_owned(),
                    reason: format!("invalid gradient stop count {stop_count}"),
                });
            }
            let mut stops = Vec::with_capacity(stop_count);
            for _ in 0..stop_count {
                let offset = sanitize_f32(r.read_f32_le("fild stop offset")?, 0.0);
                let color = read_tagged_color(&mut r, "fild stop color")?;
                stops.push(GradientStop::new(offset, color));
            }
            match subtype {
                1 => Fill::RadialGradient {
                    center: Point2D::new(p0_x, p0_y),
                    radius: scalar.abs(),
                    focal: Point2D::new(p1_x, p1_y),
                    stops,
                },
                2 => Fill::ConicalGradient {
                    center: Point2D::new(p0_x, p0_y),
                    angle_deg: angle,
                    stops,
                },
                3 => Fill::SquareGradient {
                    center: Point2D::new(p0_x, p0_y),
                    size: scalar.abs(),
                    angle_deg: angle,
                    stops,
                },
                _ => Fill::LinearGradient {
                    start: Point2D::new(p0_x, p0_y),
                    end: Point2D::new(p1_x, p1_y),
                    stops,
                },
            }
        }
        6 => {
            let fg = read_tagged_color(&mut r, "fild 2c fg")?;
            let bg = read_tagged_color(&mut r, "fild 2c bg")?;
            let tile_w = cdr32_to_mm(r.read_i32_le("fild 2c tile_w")?).abs();
            let tile_h = cdr32_to_mm(r.read_i32_le("fild 2c tile_h")?).abs();
            let nlen = usize::from(r.read_u16_le("fild 2c name len")?);
            let nb = r.read_bytes(nlen, "fild 2c name")?;
            let pattern_name = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
            Fill::TwoColorPattern {
                fg,
                bg,
                pattern_name,
                tile_w,
                tile_h,
            }
        }
        7 => {
            let fg = read_tagged_color(&mut r, "fild tex fg")?;
            let bg = read_tagged_color(&mut r, "fild tex bg")?;
            let seed_lo = u64::from(r.read_u32_le("fild tex seed lo")?);
            let seed_hi = u64::from(r.read_u32_le("fild tex seed hi")?);
            let nlen = usize::from(r.read_u16_le("fild tex preset len")?);
            let nb = r.read_bytes(nlen, "fild tex preset")?;
            let preset = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
            Fill::TextureFill {
                preset,
                seed: seed_lo | (seed_hi << 32),
                fg,
                bg,
            }
        }
        9 => {
            let rows = usize::from(r.read_u16_le("fild mesh rows")?);
            let cols = usize::from(r.read_u16_le("fild mesh cols")?);
            let count = usize::from(r.read_u16_le("fild mesh count")?);
            if count > 4096 || count.saturating_mul(16) > r.remaining() {
                return Err(CdrError::InvalidChunk {
                    fourcc: "fild".to_owned(),
                    reason: format!("invalid mesh node count {count}"),
                });
            }
            let mut nodes = Vec::with_capacity(count);
            for _ in 0..count {
                let row = usize::from(r.read_u16_le("mesh row")?);
                let col = usize::from(r.read_u16_le("mesh col")?);
                let x = cdr32_to_mm(r.read_i32_le("mesh x")?);
                let y = cdr32_to_mm(r.read_i32_le("mesh y")?);
                let color = read_tagged_color(&mut r, "mesh color")?;
                nodes.push(MeshNode {
                    row,
                    col,
                    pos: Point2D::new(x, y),
                    color,
                });
            }
            Fill::MeshFill { rows, cols, nodes }
        }
        10 => {
            let uuid_bytes: [u8; 16] = r
                .read_bytes(16, "fild vpat uuid")?
                .try_into()
                .unwrap_or([0; 16]);
            let tile_w = cdr32_to_mm(r.read_i32_le("fild vpat tile_w")?)
                .abs()
                .max(0.5);
            let tile_h = cdr32_to_mm(r.read_i32_le("fild vpat tile_h")?)
                .abs()
                .max(0.5);
            Fill::VectorPattern {
                symbol_id: SymbolId(uuid::Uuid::from_bytes(uuid_bytes)),
                tile_w,
                tile_h,
            }
        }
        11 => {
            let bmp_id = r.read_u32_le("fild bpat bmp_id")?;
            let tile_w = cdr32_to_mm(r.read_i32_le("fild bpat tile_w")?)
                .abs()
                .max(0.5);
            let tile_h = cdr32_to_mm(r.read_i32_le("fild bpat tile_h")?)
                .abs()
                .max(0.5);
            if let Some(aid) = ctx.and_then(|c| c.bitmaps.get(&bmp_id).copied()) {
                Fill::BitmapPattern {
                    image_id: aid,
                    tile_w,
                    tile_h,
                }
            } else {
                Fill::BitmapPattern {
                    image_id: ImageAssetId(uuid::Uuid::from_u128(
                        0xC0EE_0000_0000_0000_0000_0000_0000_0000u128 | u128::from(bmp_id),
                    )),
                    tile_w,
                    tile_h,
                }
            }
        }
        _ => {
            return Err(CdrError::InvalidChunk {
                fourcc: "fild".to_owned(),
                reason: format!("unknown clean fild type {fill_type}"),
            });
        }
    };
    if !r.is_eof() {
        return Err(CdrError::InvalidChunk {
            fourcc: "fild".to_owned(),
            reason: "trailing bytes in clean fild".to_owned(),
        });
    }
    Ok((fill_id, fill, false))
}

pub(crate) fn parse_fild_chunk_with_ctx(
    data: &[u8],
    is_zcf: bool,
    ctx: Option<&ParseContext<'_>>,
) -> CdrResult<(u32, Fill, bool)> {
    if is_real_fild_v16_signature(data) {
        return parse_real_fild_v16_with_ctx(data, ctx);
    }

    if let Ok(parsed) = parse_fild_chunk_clean(data, ctx) {
        return Ok(parsed);
    }

    if is_zcf {
        return parse_real_fild_v16_with_ctx(data, ctx);
    }

    if let Some((fid, fill)) = parse_v3_v5_fild(data) {
        return Ok((fid, fill, false));
    }

    // Legacy CDR v7–v15 RIFF binary fill fallback
    if data.len() >= 20 {
        let fill_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let fill_type = u16::from_le_bytes([data[4], data[5]]);
        if fill_type == 0 {
            return Ok((fill_id, Fill::None, false));
        }
        if let Some(c) = find_embedded_cdr_color(&data[4..])
            .or_else(|| {
                (data.len() >= 24)
                    .then(|| decode_cdr_native_color_record(&data[12..24]))
                    .flatten()
            })
            .or_else(|| decode_cdr_native_color_record(&data[8..20]))
        {
            if fill_type == 2 {
                let mut cols = Vec::new();
                let mut i = 4usize;
                while i + 12 <= data.len() && cols.len() < 8 {
                    if data.get(i..i + 5) == Some(&[0x01, 0x0C, 0x00, 0x00, 0x00])
                        && i + 17 <= data.len()
                    {
                        if let Some(gc) = decode_cdr_native_color_record(&data[i + 5..i + 17]) {
                            cols.push(gc);
                        }
                        i += 17;
                    } else {
                        i += 1;
                    }
                }
                if cols.len() >= 2 {
                    let last = (cols.len() - 1) as f32;
                    let stops = cols
                        .into_iter()
                        .enumerate()
                        .map(|(idx, col)| GradientStop::new(idx as f32 / last, col))
                        .collect();
                    return Ok((
                        fill_id,
                        Fill::LinearGradient {
                            start: Point2D::new(0.0, 0.5),
                            end: Point2D::new(1.0, 0.5),
                            stops,
                        },
                        true,
                    ));
                }
            }
            return Ok((fill_id, Fill::Solid(c), false));
        }
    }
    Err(CdrError::InvalidChunk {
        fourcc: "fild".to_owned(),
        reason: "unrecognized fild chunk layout".to_owned(),
    })
}

fn parse_real_fild_v16_with_ctx(
    data: &[u8],
    ctx: Option<&ParseContext<'_>>,
) -> CdrResult<(u32, Fill, bool)> {
    let (fill_id, mut fill, is_norm) = parse_real_fild_v16(data)?;
    if let Some(pctx) = ctx {
        if data.len() >= 18 {
            let sub_tag = u16::from_le_bytes([data[4], data[5]]);
            let fill_type = if sub_tag > 16 || data.len() == 22 {
                u16::from_le_bytes([data[12], data[13]])
            } else {
                sub_tag
            };
            if matches!(fill_type, 7 | 9 | 10 | 11) {
                let pat_id = u32::from_le_bytes([data[14], data[15], data[16], data[17]]);
                if let Some(&image_id) = pctx
                    .pattern_bitmaps
                    .get(&pat_id)
                    .or_else(|| pctx.pattern_bitmaps.get(&fill_id))
                {
                    fill = Fill::BitmapPattern {
                        image_id,
                        tile_w: 15.0,
                        tile_h: 15.0,
                    };
                } else if let Some(vp) = pctx
                    .vector_patterns
                    .get(&pat_id)
                    .or_else(|| pctx.vector_patterns.get(&fill_id))
                {
                    fill = vp.clone();
                }
            }
        }
    }
    Ok((fill_id, fill, is_norm))
}

fn is_real_outl_v16(data: &[u8]) -> bool {
    if data.len() < 90 {
        return false;
    }
    let color_block_len = u32::from_le_bytes([data[20], data[21], data[22], data[23]]) as usize;
    color_block_len >= 17
        && 24usize
            .checked_add(color_block_len)
            .and_then(|b| b.checked_add(24))
            .is_some_and(|end| end <= data.len())
        && data[24] == 0x01
}

pub(crate) fn parse_real_outl_v16(
    data: &[u8],
    arrows: Option<&HashMap<u32, ArrowheadStyle>>,
) -> CdrResult<(u32, OutlineStyle, bool)> {
    if data.len() < 42 {
        return Err(CdrError::UnexpectedEof {
            context: "real outl v16 header",
            needed: 42,
            available: data.len(),
        });
    }
    let outline_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let miter_e6 = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    let miter_limit = if miter_e6 > 0 {
        (f64::from(miter_e6) / 1_000_000.0).clamp(1.0, 90.0)
    } else {
        4.0
    };
    let color_block_len = u32::from_le_bytes([data[20], data[21], data[22], data[23]]) as usize;
    let color = decode_cdr_native_color_record(&data[29..41]).unwrap_or(Color::BLACK);

    let Some(base) = 24usize.checked_add(color_block_len) else {
        return Ok((outline_id, OutlineStyle::none(), true));
    };
    if base.saturating_add(24) > data.len() {
        return Err(CdrError::UnexpectedEof {
            context: "real outl v16 body",
            needed: base.saturating_add(24),
            available: data.len(),
        });
    }
    let line_type = u16::from_le_bytes([data[base + 8], data[base + 9]]);
    let caps_raw = u16::from_le_bytes([data[base + 10], data[base + 11]]);
    let join_raw = u16::from_le_bytes([data[base + 12], data[base + 13]]);
    let width_cdr = i32::from_le_bytes([
        data[base + 14],
        data[base + 15],
        data[base + 16],
        data[base + 17],
    ]);
    let stretch_raw = u16::from_le_bytes([data[base + 18], data[base + 19]]);
    let angle_e6 = i32::from_le_bytes([
        data[base + 20],
        data[base + 21],
        data[base + 22],
        data[base + 23],
    ]);

    if (line_type & 0x0001) != 0 && (line_type & 0x0006) == 0 {
        return Ok((outline_id, OutlineStyle::none(), true));
    }

    let width_mm = cdr32_to_mm(width_cdr).max(0.0);
    let cap = match caps_raw {
        1 => LineCap::Round,
        2 => LineCap::Square,
        _ => LineCap::Butt,
    };
    let join = match join_raw {
        1 => LineJoin::Round,
        2 => LineJoin::Bevel,
        _ => LineJoin::Miter,
    };
    let mut dash_pattern = Vec::new();
    let mut dash_bytes = 0usize;
    if (line_type & 0x0004) != 0 {
        if base + 28 <= data.len() {
            let count = usize::from(u16::from_le_bytes([data[base + 24], data[base + 25]]));
            if (1..=32).contains(&count) && base + 26 + count * 2 <= data.len() {
                for i in 0..count {
                    let seg =
                        u16::from_le_bytes([data[base + 26 + i * 2], data[base + 27 + i * 2]]);
                    if seg > 0 {
                        dash_pattern.push((f64::from(seg) * 0.25).max(0.25));
                    }
                }
                dash_bytes = 2 + count * 2;
            }
        }
        if dash_pattern.is_empty() {
            dash_pattern = vec![2.0, 1.0];
        }
    }

    let mut start_arrow = ArrowheadStyle::None;
    let mut end_arrow = ArrowheadStyle::None;
    let arr_off = base + 24 + dash_bytes;
    if arr_off + 8 <= data.len() {
        let s_id = u32::from_le_bytes([
            data[arr_off],
            data[arr_off + 1],
            data[arr_off + 2],
            data[arr_off + 3],
        ]);
        let e_id = u32::from_le_bytes([
            data[arr_off + 4],
            data[arr_off + 5],
            data[arr_off + 6],
            data[arr_off + 7],
        ]);
        if let Some(a) = arrows.and_then(|m| m.get(&s_id)) {
            start_arrow = a.clone();
        } else if (1..=6).contains(&s_id) {
            start_arrow = decode_arrow(s_id as u8);
        }
        if let Some(a) = arrows.and_then(|m| m.get(&e_id)) {
            end_arrow = a.clone();
        } else if (1..=6).contains(&e_id) {
            end_arrow = decode_arrow(e_id as u8);
        }
    }

    Ok((
        outline_id,
        OutlineStyle {
            color,
            width_mm,
            is_hairline: width_mm > 0.0 && width_mm <= 0.08,
            cap,
            join,
            miter_limit,
            dash_pattern,
            start_arrow,
            end_arrow,
            behind_fill: (line_type & 0x0010) != 0,
            scale_with_object: (line_type & 0x0020) != 0,
            overprint: (line_type & 0x0040) != 0,
            calligraphy_angle_deg: f64::from(angle_e6) / 1_000_000.0,
            calligraphy_stretch: if stretch_raw > 0 {
                (f64::from(stretch_raw) / 100.0).clamp(0.01, 10.0)
            } else {
                1.0
            },
        },
        false,
    ))
}

pub(crate) fn parse_outl_chunk_with_ctx(
    data: &[u8],
    is_zcf: bool,
    ctx: Option<&ParseContext<'_>>,
) -> CdrResult<(u32, OutlineStyle, bool)> {
    let arrows = ctx.map(|c| &c.arrows);
    if is_real_outl_v16(data) {
        return parse_real_outl_v16(data, arrows);
    }
    if let Ok(parsed) = parse_outl_chunk_clean(data) {
        return Ok(parsed);
    }
    if let Some((oid, outl, is_none)) = parse_v3_v5_outl(data) {
        return Ok((oid, outl, is_none));
    }
    if is_zcf && data.len() >= 12 {
        let id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        return Ok((id, OutlineStyle::none(), true));
    }
    if data.len() >= 24 {
        let outline_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let line_type = u16::from_le_bytes([data[4], data[5]]);
        if (line_type & 0x0001) != 0 && (line_type & 0x0006) == 0 {
            return Ok((outline_id, OutlineStyle::none(), true));
        }
        let width_mm =
            cdr32_to_mm(i32::from_le_bytes([data[12], data[13], data[14], data[15]])).max(0.0);
        let color = find_embedded_cdr_color(&data[16..])
            .or_else(|| {
                (data.len() >= 36)
                    .then(|| decode_cdr_native_color_record(&data[24..36]))
                    .flatten()
            })
            .unwrap_or(Color::BLACK);
        let mut outl = OutlineStyle {
            color,
            width_mm,
            is_hairline: width_mm > 0.0 && width_mm <= 0.08,
            behind_fill: (line_type & 0x0010) != 0,
            scale_with_object: (line_type & 0x0020) != 0,
            overprint: (line_type & 0x0040) != 0,
            ..OutlineStyle::default()
        };
        if let Some(arr_map) = arrows {
            if !arr_map.is_empty() && data.len() >= 44 {
                let tail = &data[data.len() - 8..];
                let s_id = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
                let e_id = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]);
                if let Some(a) = arr_map.get(&s_id) {
                    outl.start_arrow = a.clone();
                }
                if let Some(a) = arr_map.get(&e_id) {
                    outl.end_arrow = a.clone();
                }
            } else if !arr_map.is_empty() && data.len() >= 28 {
                let e_id = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
                if let Some(a) = arr_map.get(&e_id) {
                    outl.end_arrow = a.clone();
                }
            }
        }
        return Ok((outline_id, outl, false));
    }
    Err(CdrError::InvalidChunk {
        fourcc: "outl".to_owned(),
        reason: "unrecognized outl chunk layout".to_owned(),
    })
}

fn parse_outl_chunk_clean(data: &[u8]) -> CdrResult<(u32, OutlineStyle, bool)> {
    let mut r = ByteReader::new(data);
    let outline_id = r.read_u32_le("outl id")?;
    let flags = r.read_u16_le("outl flags")?;
    if (flags & 0x0001) != 0 {
        return Ok((outline_id, OutlineStyle::none(), true));
    }
    let width_mm = cdr32_to_mm(r.read_i32_le("outl width")?).max(0.0);
    let cap = match r.read_u8("outl cap")? {
        1 => LineCap::Round,
        2 => LineCap::Square,
        _ => LineCap::Butt,
    };
    let join = match r.read_u8("outl join")? {
        1 => LineJoin::Round,
        2 => LineJoin::Bevel,
        _ => LineJoin::Miter,
    };
    let miter_limit = sanitize_f64(r.read_f64_le("outl miter")?, 4.0).max(1.0);
    let calligraphy_angle_deg = sanitize_f64(r.read_f64_le("outl cal_angle")?, 0.0);
    let calligraphy_stretch = sanitize_f64(r.read_f64_le("outl cal_stretch")?, 1.0).max(0.01);
    let start_arrow = decode_arrow(r.read_u8("outl start_arrow")?);
    let end_arrow = decode_arrow(r.read_u8("outl end_arrow")?);
    let dash_count = usize::from(r.read_u16_le("outl dash_count")?);
    if dash_count > 256 || dash_count.saturating_mul(4) > r.remaining() {
        return Err(CdrError::InvalidChunk {
            fourcc: "outl".to_owned(),
            reason: format!("invalid dash count {dash_count}"),
        });
    }
    let mut dash_pattern = Vec::with_capacity(dash_count);
    for _ in 0..dash_count {
        dash_pattern.push(cdr32_to_mm(r.read_i32_le("outl dash len")?).max(0.0));
    }
    let color = read_tagged_color(&mut r, "outl color")?;

    Ok((
        outline_id,
        OutlineStyle {
            color,
            width_mm,
            is_hairline: (flags & 0x0002) != 0,
            cap,
            join,
            miter_limit,
            dash_pattern,
            start_arrow,
            end_arrow,
            behind_fill: (flags & 0x0004) != 0,
            scale_with_object: (flags & 0x0008) != 0,
            overprint: (flags & 0x0010) != 0,
            calligraphy_angle_deg,
            calligraphy_stretch,
        },
        false,
    ))
}

pub(crate) fn encode_fild_chunk(
    fill_id: u32,
    fill: &Fill,
    bmp_map: &HashMap<ImageAssetId, u32>,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&fill_id.to_le_bytes());
    match fill {
        Fill::None => {
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        Fill::Solid(c) => {
            out.extend_from_slice(&1u16.to_le_bytes());
            write_tagged_color(&mut out, c);
        }
        Fill::LinearGradient { start, end, stops } => {
            out.extend_from_slice(&2u16.to_le_bytes());
            encode_fountain_body(&mut out, 0, *start, *end, 0.0, 0.0, stops);
        }
        Fill::RadialGradient {
            center,
            radius,
            focal,
            stops,
        } => {
            out.extend_from_slice(&2u16.to_le_bytes());
            encode_fountain_body(&mut out, 1, *center, *focal, *radius, 0.0, stops);
        }
        Fill::ConicalGradient {
            center,
            angle_deg,
            stops,
        } => {
            out.extend_from_slice(&2u16.to_le_bytes());
            encode_fountain_body(&mut out, 2, *center, Point2D::ZERO, 0.0, *angle_deg, stops);
        }
        Fill::SquareGradient {
            center,
            size,
            angle_deg,
            stops,
        } => {
            out.extend_from_slice(&2u16.to_le_bytes());
            encode_fountain_body(
                &mut out,
                3,
                *center,
                Point2D::ZERO,
                *size,
                *angle_deg,
                stops,
            );
        }
        Fill::TwoColorPattern {
            fg,
            bg,
            pattern_name,
            tile_w,
            tile_h,
        } => {
            out.extend_from_slice(&6u16.to_le_bytes());
            write_tagged_color(&mut out, fg);
            write_tagged_color(&mut out, bg);
            out.extend_from_slice(&mm_to_cdr32(*tile_w).to_le_bytes());
            out.extend_from_slice(&mm_to_cdr32(*tile_h).to_le_bytes());
            let nb = pattern_name.as_bytes();
            let nlen = nb.len().min(u16::MAX as usize);
            out.extend_from_slice(&(nlen as u16).to_le_bytes());
            out.extend_from_slice(&nb[..nlen]);
        }
        Fill::TextureFill {
            preset,
            seed,
            fg,
            bg,
        } => {
            out.extend_from_slice(&7u16.to_le_bytes());
            write_tagged_color(&mut out, fg);
            write_tagged_color(&mut out, bg);
            out.extend_from_slice(&(*seed as u32).to_le_bytes());
            out.extend_from_slice(&((*seed >> 32) as u32).to_le_bytes());
            let nb = preset.as_bytes();
            let nlen = nb.len().min(u16::MAX as usize);
            out.extend_from_slice(&(nlen as u16).to_le_bytes());
            out.extend_from_slice(&nb[..nlen]);
        }
        Fill::MeshFill { rows, cols, nodes } => {
            out.extend_from_slice(&9u16.to_le_bytes());
            out.extend_from_slice(&((*rows).min(u16::MAX as usize) as u16).to_le_bytes());
            out.extend_from_slice(&((*cols).min(u16::MAX as usize) as u16).to_le_bytes());
            let count = nodes.len().min(u16::MAX as usize);
            out.extend_from_slice(&(count as u16).to_le_bytes());
            for n in &nodes[..count] {
                out.extend_from_slice(&((n.row).min(u16::MAX as usize) as u16).to_le_bytes());
                out.extend_from_slice(&((n.col).min(u16::MAX as usize) as u16).to_le_bytes());
                out.extend_from_slice(&mm_to_cdr32(n.pos.x).to_le_bytes());
                out.extend_from_slice(&mm_to_cdr32(n.pos.y).to_le_bytes());
                write_tagged_color(&mut out, &n.color);
            }
        }
        Fill::VectorPattern {
            symbol_id,
            tile_w,
            tile_h,
        } => {
            out.extend_from_slice(&10u16.to_le_bytes());
            out.extend_from_slice(symbol_id.0.as_bytes());
            out.extend_from_slice(&mm_to_cdr32(*tile_w).to_le_bytes());
            out.extend_from_slice(&mm_to_cdr32(*tile_h).to_le_bytes());
        }
        Fill::BitmapPattern {
            image_id,
            tile_w,
            tile_h,
        } => {
            out.extend_from_slice(&11u16.to_le_bytes());
            let bmp_id = bmp_map
                .get(image_id)
                .copied()
                .unwrap_or((image_id.0.as_u128() & 0xFFFF_FFFF) as u32);
            out.extend_from_slice(&bmp_id.to_le_bytes());
            out.extend_from_slice(&mm_to_cdr32(*tile_w).to_le_bytes());
            out.extend_from_slice(&mm_to_cdr32(*tile_h).to_le_bytes());
        }
    }
    out
}

fn encode_fountain_body(
    out: &mut Vec<u8>,
    subtype: u8,
    p0: Point2D,
    p1: Point2D,
    scalar: f64,
    angle: f64,
    stops: &[GradientStop],
) {
    out.push(subtype);
    out.extend_from_slice(&mm_to_cdr32(p0.x).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(p0.y).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(p1.x).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(p1.y).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(scalar, 0.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(angle, 0.0).to_le_bytes());
    let count = stops.len().min(u16::MAX as usize);
    out.extend_from_slice(&(count as u16).to_le_bytes());
    for s in &stops[..count] {
        out.extend_from_slice(&sanitize_f32(s.offset, 0.0).to_le_bytes());
        write_tagged_color(out, &s.color);
    }
}

pub(crate) fn encode_outl_chunk(outline_id: u32, outline: &OutlineStyle) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&outline_id.to_le_bytes());
    let mut flags = 0u16;
    if !outline.is_visible() {
        flags |= 0x0001;
        out.extend_from_slice(&flags.to_le_bytes());
        return out;
    }
    if outline.is_hairline {
        flags |= 0x0002;
    }
    if outline.behind_fill {
        flags |= 0x0004;
    }
    if outline.scale_with_object {
        flags |= 0x0008;
    }
    if outline.overprint {
        flags |= 0x0010;
    }
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(outline.width_mm).to_le_bytes());
    out.push(match outline.cap {
        LineCap::Butt => 0,
        LineCap::Round => 1,
        LineCap::Square => 2,
    });
    out.push(match outline.join {
        LineJoin::Miter => 0,
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
    });
    out.extend_from_slice(&sanitize_f64(outline.miter_limit, 4.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(outline.calligraphy_angle_deg, 0.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(outline.calligraphy_stretch, 1.0).to_le_bytes());
    out.push(encode_arrow(&outline.start_arrow));
    out.push(encode_arrow(&outline.end_arrow));
    let dcount = outline.dash_pattern.len().min(256);
    out.extend_from_slice(&(dcount as u16).to_le_bytes());
    for &d in &outline.dash_pattern[..dcount] {
        out.extend_from_slice(&mm_to_cdr32(d).to_le_bytes());
    }
    write_tagged_color(&mut out, &outline.color);
    out
}

fn encode_arrow(a: &ArrowheadStyle) -> u8 {
    match a {
        ArrowheadStyle::None => 0,
        ArrowheadStyle::Triangle => 1,
        ArrowheadStyle::OpenVee => 2,
        ArrowheadStyle::Diamond => 3,
        ArrowheadStyle::Circle => 4,
        ArrowheadStyle::Square => 5,
        ArrowheadStyle::Stealth => 6,
        ArrowheadStyle::Custom(_) => 1,
    }
}

fn decode_arrow(v: u8) -> ArrowheadStyle {
    match v {
        1 => ArrowheadStyle::Triangle,
        2 => ArrowheadStyle::OpenVee,
        3 => ArrowheadStyle::Diamond,
        4 => ArrowheadStyle::Circle,
        5 => ArrowheadStyle::Square,
        6 => ArrowheadStyle::Stealth,
        _ => ArrowheadStyle::None,
    }
}
