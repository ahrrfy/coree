// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust PostScript / EPS Level 3 writer (`%!PS-Adobe-3.0 EPSF-3.0`) and
//! hardened EPS / PostScript vector & text importer.

use crate::atomic::atomic_write_bytes;
use crate::display_list::{DisplayListOptions, DisplayOp, DisplayTextKind, ExportDisplayList};
use crate::error::{IoError, IoResult, DEFAULT_MAX_SVG_BYTES};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, Color, ColorMode, Document, Fill, GradientStop, LineCap,
    LineJoin, NodePayload, ObjectId, OutlineStyle, Page, Point2D, SceneObject, TextDirection,
    TextStyle,
};
use coree_render::{outline_arrowhead_geometries, TextEngine};
use kurbo::{BezPath, PathEl};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

const MM_TO_PT: f64 = 72.0 / 25.4;
const PT_TO_MM: f64 = 25.4 / 72.0;
const MAX_EPS_DEPTH: usize = 64;
const MAX_PS_GSTATE_DEPTH: usize = 128;
const MAX_PS_CALL_DEPTH: usize = 32;
const MAX_PS_STEPS: usize = 2_000_000;
const MAX_PS_TOKENS: usize = 2_000_000;
const MAX_PS_STACK_LEN: usize = 4_096;
const MAX_PS_STACK_NODES: usize = 100_000;
const MAX_PS_EMITTED_OBJECTS: usize = 100_000;
const MAX_PS_TOTAL_VERTICES: usize = 2_000_000;
const MAX_EPS_INPUT_BYTES: usize = DEFAULT_MAX_SVG_BYTES * 2; // 128 MiB
const MAX_PS_STREAM_BYTES: usize = MAX_EPS_INPUT_BYTES;

/// Exports `page_index` of `doc` as an Encapsulated PostScript Level 3 string.
pub fn export_eps_to_string(doc: &Document, page_index: usize) -> IoResult<String> {
    let page = doc.page(page_index).ok_or(IoError::PageOutOfRange {
        page_index,
        page_count: doc.pages().len(),
    })?;

    if !page.width_mm.is_finite()
        || page.width_mm <= 0.0
        || !page.height_mm.is_finite()
        || page.height_mm <= 0.0
    {
        return Err(IoError::InvalidFormat {
            format: "eps".into(),
            message: format!(
                "page {} has invalid non-finite or non-positive dimensions",
                page_index + 1
            ),
        });
    }

    let w_pt = page.width_mm * MM_TO_PT;
    let h_pt = page.height_mm * MM_TO_PT;
    let use_cmyk = doc.color_mode() == ColorMode::Cmyk;

    let mut out = String::with_capacity(4096);
    out.push_str("%!PS-Adobe-3.0 EPSF-3.0\n");
    let _ = writeln!(
        out,
        "%%BoundingBox: 0 0 {} {}",
        w_pt.ceil() as i64,
        h_pt.ceil() as i64
    );
    let _ = writeln!(out, "%%HiResBoundingBox: 0.0000 0.0000 {w_pt:.4} {h_pt:.4}");
    out.push_str("%%Creator: coree (coree-io EPS Level 3 Writer)\n");
    let _ = writeln!(out, "%%Title: {}", sanitize_dsc_comment(doc.title()));
    out.push_str("%%LanguageLevel: 3\n");
    out.push_str("%%Pages: 1\n");
    out.push_str("%%EndComments\n");
    out.push_str("%%BeginProlog\n");
    out.push_str("gsave\n");
    out.push_str("%%EndProlog\n");

    let mut text_engine = TextEngine::new();
    let dl = ExportDisplayList::build(
        doc,
        page_index,
        &DisplayListOptions::eps(),
        &mut text_engine,
    )?;
    for op in &dl.ops {
        emit_eps_display_op(page, doc, op, use_cmyk, &mut out);
    }

    out.push_str("grestore\n");
    out.push_str("%%Trailer\n");
    out.push_str("%%EOF\n");
    Ok(out)
}

/// Atomically exports `page_index` of `doc` as an EPS Level 3 file at `path`.
pub fn export_eps(doc: &Document, page_index: usize, path: &Path) -> IoResult<()> {
    let eps = export_eps_to_string(doc, page_index)?;
    atomic_write_bytes(path, eps.as_bytes())
}

fn emit_eps_display_op(
    page: &Page,
    doc: &Document,
    op: &DisplayOp,
    use_cmyk: bool,
    out: &mut String,
) {
    match op {
        DisplayOp::PushLayer { name, .. } => {
            let _ = writeln!(out, "% Layer: {}", sanitize_dsc_comment(name));
        }
        DisplayOp::PopLayer
        | DisplayOp::PushGroup { .. }
        | DisplayOp::PopGroup
        | DisplayOp::PushTransparencyGroup { .. }
        | DisplayOp::PopTransparencyGroup => {}
        DisplayOp::PushClip {
            path, fill_rule, ..
        } => {
            out.push_str("gsave\n");
            emit_eps_path_ops(page, path, out);
            match fill_rule {
                coree_render::FillRule::EvenOdd => out.push_str("eoclip newpath\n"),
                coree_render::FillRule::Winding => out.push_str("clip newpath\n"),
            }
        }
        DisplayOp::PopClip => {
            out.push_str("grestore\n");
        }
        DisplayOp::DrawVector {
            path,
            fill,
            outline,
            fill_xf,
            opacity,
            emit_arrowheads,
            ..
        } => {
            emit_eps_styled_path_opts(
                page,
                path,
                fill,
                outline,
                *fill_xf,
                *opacity,
                use_cmyk,
                *emit_arrowheads,
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
            ..
        } => {
            let scale_alpha = |bytes: &mut [u8], op: f32| {
                if (op - 1.0).abs() > 1e-3 {
                    let op_c = op.clamp(0.0, 1.0);
                    for px in bytes.as_chunks_mut::<4>().0 {
                        px[3] = (f32::from(px[3]) * op_c).round().clamp(0.0, 255.0) as u8;
                    }
                }
            };
            if let Some(aid) = asset_id {
                if let Some(asset) = doc.asset(*aid) {
                    let mut rgba8 = asset.to_rgba8_vec();
                    scale_alpha(&mut rgba8, *opacity);
                    emit_eps_rgba_image(
                        page,
                        asset.width,
                        asset.height,
                        &rgba8,
                        *bl,
                        *br,
                        *tl,
                        use_cmyk,
                        out,
                    );
                }
            } else if let Some(ref arc_rgba) = inline_rgba {
                let (w, h, ref bytes) = **arc_rgba;
                if (*opacity - 1.0).abs() > 1e-3 {
                    let mut scaled = bytes.clone();
                    scale_alpha(&mut scaled, *opacity);
                    emit_eps_rgba_image(page, w, h, &scaled, *bl, *br, *tl, use_cmyk, out);
                } else {
                    emit_eps_rgba_image(page, w, h, bytes, *bl, *br, *tl, use_cmyk, out);
                }
            }
        }
        DisplayOp::DrawTextRuns {
            kind,
            artistic,
            fallback_curves,
            outline,
            opacity,
            world_xf,
            is_perspective,
            ..
        } => {
            if *opacity <= 1e-4 {
                return;
            }
            let plain = artistic.plain_text();
            let style = artistic
                .runs
                .first()
                .map(|r| r.style.clone())
                .unwrap_or_default();
            let is_simple_ascii_helvetica = !*is_perspective
                && !matches!(kind, DisplayTextKind::OnPath { .. })
                && (!outline.is_visible() || *outline == OutlineStyle::default())
                && artistic.runs.len() == 1
                && artistic.effective_direction() != TextDirection::Rtl
                && !plain.is_empty()
                && plain.chars().all(|ch| ch.is_ascii() && !ch.is_control())
                && !style.italic
                && !style.underline
                && !style.strikethrough
                && (style.font_family.trim().is_empty()
                    || style.font_family.eq_ignore_ascii_case("Noto Sans")
                    || style.font_family.eq_ignore_ascii_case("Helvetica")
                    || style.font_family.eq_ignore_ascii_case("Arial"));
            if is_simple_ascii_helvetica {
                let mut eff_style = style.clone();
                eff_style.fill = apply_eps_opacity_to_color(&style.fill, *opacity);
                emit_eps_ascii_type1_text(page, &plain, &eff_style, *world_xf, use_cmyk, out);
            } else {
                let xf = if *is_perspective {
                    Affine2D::IDENTITY
                } else {
                    *world_xf
                };
                emit_eps_shaped_pieces(page, fallback_curves, outline, *opacity, xf, use_cmyk, out);
            }
        }
    }
}

/// 8x8 Bayer ordered dithering threshold matrix (`0..255` scale) for soft-alpha PS Level 3 ImageType 3 masks.
#[rustfmt::skip]
const BAYER_8X8: [[u8; 8]; 8] = [
    [  0, 128,  32, 160,   8, 136,  40, 168],
    [192,  64, 224,  96, 200,  72, 232, 104],
    [ 48, 176,  16, 144,  56, 184,  24, 152],
    [240, 112, 208,  80, 248, 120, 216,  88],
    [ 12, 140,  44, 172,   4, 132,  36, 164],
    [204,  76, 236, 108, 196,  68, 228, 100],
    [ 60, 188,  28, 156,  52, 180,  20, 148],
    [252, 124, 220,  92, 244, 116, 212,  84],
];

#[allow(clippy::too_many_arguments)]
fn emit_eps_rgba_image(
    page: &Page,
    width: u32,
    height: u32,
    rgba_bytes: &[u8],
    bl: Point2D,
    br: Point2D,
    tl: Point2D,
    use_cmyk: bool,
    out: &mut String,
) {
    if width == 0 || height == 0 || rgba_bytes.len() < (width as usize) * (height as usize) * 4 {
        return;
    }
    let total_px = (width as usize) * (height as usize);
    let has_alpha = rgba_bytes[..total_px * 4]
        .as_chunks::<4>()
        .0
        .iter()
        .any(|px| px[3] < 255);

    let (x0, y0) = to_eps_pt(page, bl);
    let (x1, y1) = to_eps_pt(page, br);
    let (x2, y2) = to_eps_pt(page, tl);

    out.push_str("gsave\n");
    let _ = writeln!(
        out,
        "[{:.4} {:.4} {:.4} {:.4} {:.4} {:.4}] concat",
        x1 - x0,
        y1 - y0,
        x2 - x0,
        y2 - y0,
        x0,
        y0
    );
    if use_cmyk {
        out.push_str("/DeviceCMYK setcolorspace\n");
    } else {
        out.push_str("/DeviceRGB setcolorspace\n");
    }
    let decode = if use_cmyk {
        "[0 1 0 1 0 1 0 1]"
    } else {
        "[0 1 0 1 0 1]"
    };

    if has_alpha {
        // PostScript Level 3 ImageType 3 (`/InterleaveType 3`) with separate 1-bit `/MaskDict`
        // (0 = paint, 1 = mask out with `/Decode [0 1]`) and straight `/DataDict` color samples.
        // Place `def` AFTER the inline `>` EOD delimiter so `/ReusableStreamDecode filter`
        // reads only the hex stream from `currentfile` (instead of consuming `def`).
        out.push_str(
            "/coree_mask currentfile /ASCIIHexDecode filter /ReusableStreamDecode filter\n",
        );
        let w_us = width as usize;
        let h_us = height as usize;
        let mut mask_hex_len = 0usize;
        for y in 0..h_us {
            let mut byte_acc = 0u8;
            let mut bit_idx = 0u8;
            for x in 0..w_us {
                let a = rgba_bytes[(y * w_us + x) * 4 + 3];
                let paint = if a == 255 {
                    true
                } else if a == 0 {
                    false
                } else {
                    a > BAYER_8X8[y & 7][x & 7]
                };
                // `/Decode [0 1]` on MaskDict: 0 = paint, 1 = transparent
                let mask_bit = u8::from(!paint);
                byte_acc = (byte_acc << 1) | mask_bit;
                bit_idx += 1;
                if bit_idx == 8 {
                    let _ = write!(out, "{byte_acc:02X}");
                    mask_hex_len += 2;
                    if mask_hex_len >= 76 {
                        out.push('\n');
                        mask_hex_len = 0;
                    }
                    byte_acc = 0;
                    bit_idx = 0;
                }
            }
            if bit_idx > 0 {
                byte_acc <<= 8 - bit_idx;
                let _ = write!(out, "{byte_acc:02X}");
                mask_hex_len += 2;
                if mask_hex_len >= 76 {
                    out.push('\n');
                    mask_hex_len = 0;
                }
            }
        }
        if mask_hex_len > 0 {
            out.push('\n');
        }
        out.push_str(">\ndef\n");

        out.push_str(
            "/coree_data currentfile /ASCIIHexDecode filter /ReusableStreamDecode filter\n",
        );
        let mut data_hex_len = 0usize;
        for px in rgba_bytes[..total_px * 4].as_chunks::<4>().0 {
            let (r, g, b) = if px[3] == 0 {
                (0u8, 0u8, 0u8)
            } else {
                (px[0], px[1], px[2])
            };
            if use_cmyk {
                let c = Color::rgba8(r, g, b, 255).to_cmyk();
                for ch in c {
                    let byte = (ch * 255.0).round().clamp(0.0, 255.0) as u8;
                    let _ = write!(out, "{byte:02X}");
                    data_hex_len += 2;
                    if data_hex_len >= 76 {
                        out.push('\n');
                        data_hex_len = 0;
                    }
                }
            } else {
                for byte in [r, g, b] {
                    let _ = write!(out, "{byte:02X}");
                    data_hex_len += 2;
                    if data_hex_len >= 76 {
                        out.push('\n');
                        data_hex_len = 0;
                    }
                }
            }
        }
        if data_hex_len > 0 {
            out.push('\n');
        }
        out.push_str(">\ndef\n");

        let _ = writeln!(
            out,
            "<<\n  /ImageType 3\n  /InterleaveType 3\n  /MaskDict <<\n    /ImageType 1\n    /Width {width}\n    /Height {height}\n    /BitsPerComponent 1\n    /Decode [0 1]\n    /ImageMatrix [{width} 0 0 -{height} 0 {height}]\n    /DataSource coree_mask\n  >>\n  /DataDict <<\n    /ImageType 1\n    /Width {width}\n    /Height {height}\n    /BitsPerComponent 8\n    /Decode {decode}\n    /ImageMatrix [{width} 0 0 -{height} 0 {height}]\n    /DataSource coree_data\n  >>\n>> image\ncoree_mask closefile\ncoree_data closefile\ngrestore"
        );
        return;
    }

    let _ = writeln!(
        out,
        "<<\n  /ImageType 1\n  /Width {width}\n  /Height {height}\n  /BitsPerComponent 8\n  /Decode {decode}\n  /ImageMatrix [{width} 0 0 -{height} 0 {height}]\n  /DataSource currentfile /ASCIIHexDecode filter\n>> image"
    );

    let mut hex_line_len = 0usize;
    for px in rgba_bytes[..total_px * 4].as_chunks::<4>().0 {
        let (r, g, b) = (px[0], px[1], px[2]);
        if use_cmyk {
            let c = Color::rgba8(r, g, b, 255).to_cmyk();
            for ch in c {
                let byte = (ch * 255.0).round().clamp(0.0, 255.0) as u8;
                let _ = write!(out, "{byte:02X}");
                hex_line_len += 2;
                if hex_line_len >= 76 {
                    out.push('\n');
                    hex_line_len = 0;
                }
            }
        } else {
            for byte in [r, g, b] {
                let _ = write!(out, "{byte:02X}");
                hex_line_len += 2;
                if hex_line_len >= 76 {
                    out.push('\n');
                    hex_line_len = 0;
                }
            }
        }
    }
    if hex_line_len > 0 {
        out.push('\n');
    }
    out.push_str(">\ngrestore\n");
}

fn format_eps_color_comps(c: &Color, use_cmyk: bool) -> String {
    match c {
        Color::Cmyk { .. } | Color::Spot { .. } => {
            let [cc, m, y, k] = c.to_cmyk().map(sanitize_unit_f32);
            format!("{cc:.4} {m:.4} {y:.4} {k:.4}")
        }
        _ if use_cmyk => {
            let [cc, m, y, k] = c.to_cmyk().map(sanitize_unit_f32);
            format!("{cc:.4} {m:.4} {y:.4} {k:.4}")
        }
        _ => {
            let [r, g, b, _] = c.to_rgba_f32().map(sanitize_unit_f32);
            format!("{r:.4} {g:.4} {b:.4}")
        }
    }
}

fn build_eps_gradient_function_dict(stops: &[GradientStop], use_cmyk: bool) -> Option<String> {
    if stops.is_empty() {
        return None;
    }
    let mut sorted = stops.to_vec();
    sorted.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    if sorted.len() == 1 {
        let c = format_eps_color_comps(&sorted[0].color, use_cmyk);
        return Some(format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [{c}] /C1 [{c}] /N 1 >>"
        ));
    }
    if sorted.len() == 2 {
        let c0 = format_eps_color_comps(&sorted[0].color, use_cmyk);
        let c1 = format_eps_color_comps(&sorted[1].color, use_cmyk);
        return Some(format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >>"
        ));
    }
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
        let c0 = format_eps_color_comps(&sorted[0].color, use_cmyk);
        let c1 = format_eps_color_comps(&last.color, use_cmyk);
        return Some(format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >>"
        ));
    }
    let mut fns = Vec::new();
    let mut bounds = Vec::new();
    let mut encode = Vec::new();
    for (i, &(_t0, t1, c0_col, c1_col)) in segments.iter().enumerate() {
        let c0 = format_eps_color_comps(c0_col, use_cmyk);
        let c1 = format_eps_color_comps(c1_col, use_cmyk);
        fns.push(format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >>"
        ));
        if i + 1 < segments.len() {
            bounds.push(format!("{t1:.4}"));
        }
        encode.push("0 1".to_string());
    }
    Some(format!(
        "<< /FunctionType 3 /Domain [0 1] /Functions [{}] /Bounds [{}] /Encode [{}] >>",
        fns.join(" "),
        bounds.join(" "),
        encode.join(" ")
    ))
}

fn emit_eps_gradient_fill(
    page: &Page,
    path: &BezierPath,
    fill: &Fill,
    fill_xf: Affine2D,
    use_cmyk: bool,
    out: &mut String,
) -> bool {
    let cs = if use_cmyk {
        "/DeviceCMYK"
    } else {
        "/DeviceRGB"
    };
    match fill {
        Fill::LinearGradient { start, end, stops } if !stops.is_empty() => {
            let Some(func_dict) = build_eps_gradient_function_dict(stops, use_cmyk) else {
                return false;
            };
            let (x0, y0) = to_eps_pt(page, fill_xf.transform_point(*start));
            let (mut x1, mut y1) = to_eps_pt(page, fill_xf.transform_point(*end));
            if (x1 - x0).hypot(y1 - y0) < 1e-3 {
                x1 = x0 + 1.0;
                y1 = y0;
            }
            out.push_str("gsave\n");
            emit_eps_path_ops(page, path, out);
            out.push_str("eoclip newpath\n");
            let _ = writeln!(
                out,
                "<< /ShadingType 2 /ColorSpace {cs} /Coords [{x0:.4} {y0:.4} {x1:.4} {y1:.4}] \
                 /Function {func_dict} /Extend [true true] >> shfill"
            );
            out.push_str("grestore\n");
            true
        }
        Fill::RadialGradient {
            center,
            radius,
            focal,
            stops,
        } if !stops.is_empty() => {
            let Some(func_dict) = build_eps_gradient_function_dict(stops, use_cmyk) else {
                return false;
            };
            let (cx, cy) = to_eps_pt(page, fill_xf.transform_point(*center));
            let (fx, fy) = to_eps_pt(page, fill_xf.transform_point(*focal));
            let (ex, ey) = to_eps_pt(
                page,
                fill_xf.transform_point(Point2D::new(center.x + radius.max(0.1), center.y)),
            );
            let r_pt = (ex - cx).hypot(ey - cy).max(0.5);
            out.push_str("gsave\n");
            emit_eps_path_ops(page, path, out);
            out.push_str("eoclip newpath\n");
            let _ = writeln!(
                out,
                "<< /ShadingType 3 /ColorSpace {cs} /Coords [{fx:.4} {fy:.4} 0.0000 {cx:.4} {cy:.4} {r_pt:.4}] \
                 /Function {func_dict} /Extend [true true] >> shfill"
            );
            out.push_str("grestore\n");
            true
        }
        _ => false,
    }
}

fn apply_eps_opacity_to_color(c: &Color, opacity: f32) -> Color {
    let op = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if c.alpha() <= 1e-6 {
        return c.clone();
    }
    if (op - 1.0).abs() <= 1e-4 && (c.alpha() - 1.0).abs() <= 1e-4 {
        return c.clone();
    }
    match c {
        Color::Cmyk { c, m, y, k, a } => {
            let eff = (op * a.clamp(0.0, 1.0)).clamp(0.0, 1.0);
            Color::Cmyk {
                c: c * eff,
                m: m * eff,
                y: y * eff,
                k: k * eff,
                a: 1.0,
            }
        }
        Color::Spot {
            name,
            tint,
            fallback_cmyk,
            alpha,
        } => {
            let eff = (op * alpha.clamp(0.0, 1.0)).clamp(0.0, 1.0);
            Color::Spot {
                name: name.clone(),
                tint: tint * eff,
                fallback_cmyk: *fallback_cmyk,
                alpha: 1.0,
            }
        }
        _ => {
            let [r, g, b, a] = c.to_rgba_f32();
            let eff = (op * a.clamp(0.0, 1.0)).clamp(0.0, 1.0);
            Color::rgba(
                r * eff + (1.0 - eff),
                g * eff + (1.0 - eff),
                b * eff + (1.0 - eff),
                1.0,
            )
        }
    }
}

fn apply_eps_opacity_to_fill(fill: &Fill, opacity: f32) -> Fill {
    let op = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if (op - 1.0).abs() <= 1e-4 {
        return fill.clone();
    }
    match fill {
        Fill::Solid(c) => Fill::Solid(apply_eps_opacity_to_color(c, op)),
        Fill::LinearGradient { start, end, stops } => Fill::LinearGradient {
            start: *start,
            end: *end,
            stops: stops
                .iter()
                .map(|s| GradientStop::new(s.offset, apply_eps_opacity_to_color(&s.color, op)))
                .collect(),
        },
        Fill::RadialGradient {
            center,
            radius,
            focal,
            stops,
        } => Fill::RadialGradient {
            center: *center,
            radius: *radius,
            focal: *focal,
            stops: stops
                .iter()
                .map(|s| GradientStop::new(s.offset, apply_eps_opacity_to_color(&s.color, op)))
                .collect(),
        },
        Fill::ConicalGradient {
            center,
            angle_deg,
            stops,
        } => Fill::ConicalGradient {
            center: *center,
            angle_deg: *angle_deg,
            stops: stops
                .iter()
                .map(|s| GradientStop::new(s.offset, apply_eps_opacity_to_color(&s.color, op)))
                .collect(),
        },
        Fill::SquareGradient {
            center,
            size,
            angle_deg,
            stops,
        } => Fill::SquareGradient {
            center: *center,
            size: *size,
            angle_deg: *angle_deg,
            stops: stops
                .iter()
                .map(|s| GradientStop::new(s.offset, apply_eps_opacity_to_color(&s.color, op)))
                .collect(),
        },
        Fill::TwoColorPattern {
            fg,
            bg,
            pattern_name,
            tile_w,
            tile_h,
        } => Fill::TwoColorPattern {
            fg: apply_eps_opacity_to_color(fg, op),
            bg: apply_eps_opacity_to_color(bg, op),
            pattern_name: pattern_name.clone(),
            tile_w: *tile_w,
            tile_h: *tile_h,
        },
        Fill::TextureFill {
            preset,
            seed,
            fg,
            bg,
        } => Fill::TextureFill {
            preset: preset.clone(),
            seed: *seed,
            fg: apply_eps_opacity_to_color(fg, op),
            bg: apply_eps_opacity_to_color(bg, op),
        },
        Fill::MeshFill { rows, cols, nodes } => Fill::MeshFill {
            rows: *rows,
            cols: *cols,
            nodes: nodes
                .iter()
                .map(|n| coree_core::MeshNode {
                    color: apply_eps_opacity_to_color(&n.color, op),
                    ..n.clone()
                })
                .collect(),
        },
        other => other.clone(),
    }
}

fn emit_eps_styled_path(
    page: &Page,
    path: &BezierPath,
    fill: &Fill,
    outline: &OutlineStyle,
    fill_xf: Affine2D,
    use_cmyk: bool,
    out: &mut String,
) {
    emit_eps_styled_path_opts(page, path, fill, outline, fill_xf, 1.0, use_cmyk, true, out);
}

#[allow(clippy::too_many_arguments)]
fn emit_eps_styled_path_opts(
    page: &Page,
    path: &BezierPath,
    fill: &Fill,
    outline: &OutlineStyle,
    fill_xf: Affine2D,
    opacity: f32,
    use_cmyk: bool,
    emit_arrowheads: bool,
    out: &mut String,
) {
    if path.is_empty() {
        return;
    }
    let eff_op = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if eff_op <= 1e-4 {
        return;
    }
    let eff_fill_owned = apply_eps_opacity_to_fill(fill, eff_op);
    let fill = &eff_fill_owned;
    let eff_outline_color = apply_eps_opacity_to_color(&outline.color, eff_op);

    let emit_stroke_block = |out: &mut String| {
        if !outline.is_visible() {
            return;
        }
        out.push_str("gsave\n");
        if outline.overprint {
            out.push_str("true setoverprint\n");
        }
        emit_eps_color(out, &eff_outline_color, use_cmyk);
        let raw_w = outline.scaled_width_mm_for_transform(&fill_xf);
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
        let _ = writeln!(
            out,
            "{width_pt:.4} setlinewidth {cap} setlinecap {join} setlinejoin {miter:.3} setmiterlimit"
        );
        if !outline.dash_pattern.is_empty() {
            let parts: Vec<String> = outline
                .dash_pattern
                .iter()
                .filter(|d| d.is_finite() && **d > 0.0)
                .map(|d| format!("{:.4}", (d * MM_TO_PT).max(0.1)))
                .collect();
            if !parts.is_empty() {
                let _ = writeln!(out, "[{}] 0 setdash", parts.join(" "));
            }
        }
        emit_eps_path_ops(page, path, out);
        out.push_str("stroke\n");
        if emit_arrowheads {
            for (geom, filled) in outline_arrowhead_geometries(path, outline) {
                emit_eps_path_ops(page, &geom, out);
                out.push_str(if filled { "fill\n" } else { "stroke\n" });
            }
        }
        out.push_str("grestore\n");
    };

    if outline.behind_fill {
        emit_stroke_block(out);
    }

    if !emit_eps_gradient_fill(page, path, fill, fill_xf, use_cmyk, out) {
        let fill_color = match fill {
            Fill::None | Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => None,
            Fill::Solid(c) => Some(c.clone()),
            Fill::LinearGradient { stops, .. }
            | Fill::RadialGradient { stops, .. }
            | Fill::ConicalGradient { stops, .. }
            | Fill::SquareGradient { stops, .. } => stops.first().map(|s| s.color.clone()),
            Fill::TwoColorPattern { fg, .. } | Fill::TextureFill { fg, .. } => Some(fg.clone()),
            Fill::MeshFill { nodes, .. } => nodes.first().map(|n| n.color.clone()),
        };
        if let Some(ref fc) = fill_color.filter(|c| c.alpha() > 1e-6) {
            out.push_str("gsave\n");
            emit_eps_color(out, fc, use_cmyk);
            emit_eps_path_ops(page, path, out);
            out.push_str("eofill\ngrestore\n");
        }
    }

    if !outline.behind_fill {
        emit_stroke_block(out);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_eps_shaped_pieces(
    page: &Page,
    pieces: &[coree_render::ShapedGlyphDraw],
    outline: &OutlineStyle,
    opacity: f32,
    world_xf: Affine2D,
    use_cmyk: bool,
    out: &mut String,
) {
    let mut eff_outline = outline.clone();
    if eff_outline.is_visible() && (opacity - 1.0).abs() > 1e-4 {
        eff_outline.color = apply_eps_opacity_to_color(&eff_outline.color, opacity);
    }
    for piece in pieces {
        if piece.path.is_empty() {
            continue;
        }
        let world_path = piece.path.transformed(&world_xf);
        let fill = if piece.fill.alpha() > 1e-6 {
            Fill::Solid(apply_eps_opacity_to_color(&piece.fill, opacity))
        } else {
            Fill::None
        };
        emit_eps_styled_path(
            page,
            &world_path,
            &fill,
            &eff_outline,
            world_xf,
            use_cmyk,
            out,
        );
    }
}

fn emit_eps_ascii_type1_text(
    page: &Page,
    text: &str,
    style: &TextStyle,
    world_xf: Affine2D,
    use_cmyk: bool,
    out: &mut String,
) {
    if text.is_empty() || !world_xf.is_finite() {
        return;
    }
    let origin = world_xf.transform_point(Point2D::new(0.0, style.font_size_mm()));
    let (x, y) = to_eps_pt(page, origin);
    let [a, b, c, d, _, _] = world_xf.m;
    let font_pt = if style.font_size_pt.is_finite() {
        style.font_size_pt.max(1.0)
    } else {
        12.0
    };
    out.push_str("gsave\n");
    emit_eps_color(out, &style.fill, use_cmyk);
    let ps_font = if style.bold {
        "Helvetica-Bold"
    } else {
        "Helvetica"
    };
    let is_pure_translate =
        (a - 1.0).abs() < 1e-6 && b.abs() < 1e-6 && c.abs() < 1e-6 && (d - 1.0).abs() < 1e-6;
    if is_pure_translate {
        let _ = writeln!(
            out,
            "/{ps_font} findfont {font_pt:.2} scalefont setfont {x:.4} {y:.4} moveto ({}) show",
            escape_ps_string(text)
        );
    } else {
        let _ = writeln!(
            out,
            "[{a:.6} {:.6} {:.6} {d:.6} {x:.4} {y:.4}] concat /{ps_font} findfont {font_pt:.2} scalefont setfont 0 0 moveto ({}) show",
            -b,
            -c,
            escape_ps_string(text)
        );
    }
    out.push_str("grestore\n");
}

fn emit_eps_path_ops(page: &Page, path: &BezierPath, out: &mut String) {
    out.push_str("newpath\n");
    let kb = path.to_kurbo();
    let mut cur = kurbo::Point::ZERO;
    let mut subpath_start = kurbo::Point::ZERO;
    let pt_ok = |p: kurbo::Point| p.x.is_finite() && p.y.is_finite();
    for el in kb.elements() {
        match *el {
            PathEl::MoveTo(p) if pt_ok(p) => {
                cur = p;
                subpath_start = p;
                let (x, y) = to_eps_pt(page, Point2D::from_kurbo(p));
                let _ = writeln!(out, "{x:.4} {y:.4} moveto");
            }
            PathEl::LineTo(p) if pt_ok(p) => {
                cur = p;
                let (x, y) = to_eps_pt(page, Point2D::from_kurbo(p));
                let _ = writeln!(out, "{x:.4} {y:.4} lineto");
            }
            PathEl::QuadTo(p1, p2) if pt_ok(cur) && pt_ok(p1) && pt_ok(p2) => {
                let cubic = kurbo::QuadBez::new(cur, p1, p2).raise();
                cur = p2;
                let (x1, y1) = to_eps_pt(page, Point2D::from_kurbo(cubic.p1));
                let (x2, y2) = to_eps_pt(page, Point2D::from_kurbo(cubic.p2));
                let (x3, y3) = to_eps_pt(page, Point2D::from_kurbo(cubic.p3));
                let _ = writeln!(
                    out,
                    "{x1:.4} {y1:.4} {x2:.4} {y2:.4} {x3:.4} {y3:.4} curveto"
                );
            }
            PathEl::CurveTo(p1, p2, p3) if pt_ok(p1) && pt_ok(p2) && pt_ok(p3) => {
                cur = p3;
                let (x1, y1) = to_eps_pt(page, Point2D::from_kurbo(p1));
                let (x2, y2) = to_eps_pt(page, Point2D::from_kurbo(p2));
                let (x3, y3) = to_eps_pt(page, Point2D::from_kurbo(p3));
                let _ = writeln!(
                    out,
                    "{x1:.4} {y1:.4} {x2:.4} {y2:.4} {x3:.4} {y3:.4} curveto"
                );
            }
            PathEl::ClosePath => {
                cur = subpath_start;
                out.push_str("closepath\n");
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

fn emit_eps_color(out: &mut String, color: &Color, use_cmyk: bool) {
    match color {
        Color::Spot {
            name,
            tint,
            fallback_cmyk,
            ..
        } => {
            let [c, m, y, k] = fallback_cmyk.map(sanitize_unit_f32);
            let t = sanitize_unit_f32(*tint);
            let _ = writeln!(
                out,
                "[ /Separation ({}) /DeviceCMYK {{ dup {c:.4} mul exch dup {m:.4} mul exch dup {y:.4} mul exch {k:.4} mul }} ] setcolorspace {t:.4} setcolor",
                escape_ps_string(name)
            );
        }
        Color::Cmyk { .. } => {
            let [c, m, y, k] = color.to_cmyk().map(sanitize_unit_f32);
            let _ = writeln!(out, "{c:.4} {m:.4} {y:.4} {k:.4} setcmykcolor");
        }
        _ if use_cmyk => {
            let [c, m, y, k] = color.to_cmyk().map(sanitize_unit_f32);
            let _ = writeln!(out, "{c:.4} {m:.4} {y:.4} {k:.4} setcmykcolor");
        }
        _ => {
            let [r, g, b, _] = color.to_rgba_f32().map(sanitize_unit_f32);
            let _ = writeln!(out, "{r:.4} {g:.4} {b:.4} setrgbcolor");
        }
    }
}

fn to_eps_pt(page: &Page, pt: Point2D) -> (f64, f64) {
    (pt.x * MM_TO_PT, (page.height_mm - pt.y) * MM_TO_PT)
}

fn sanitize_dsc_comment(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

fn escape_ps_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            c if c.is_ascii() && !c.is_control() => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}

// =============================================================================
// EPS / PostScript Importer
// =============================================================================

/// Imports an Encapsulated PostScript (`.eps`) or PostScript file from `path`.
pub fn import_eps(path: &Path) -> IoResult<Document> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((MAX_EPS_INPUT_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_EPS_INPUT_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_eps_bytes={MAX_EPS_INPUT_BYTES}"),
            actual: format!("file exceeds {MAX_EPS_INPUT_BYTES} bytes"),
        });
    }
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Imported EPS");
    import_eps_bytes(&bytes, title)
}

/// Imports an EPS (ASCII or DOS binary `C5 D0 D3 C6`) or PostScript byte stream into a [`Document`].
pub fn import_eps_bytes(bytes: &[u8], title: &str) -> IoResult<Document> {
    if bytes.is_empty() {
        return Err(IoError::InvalidFormat {
            format: "eps".into(),
            message: "empty EPS input".into(),
        });
    }
    if bytes.len() > MAX_EPS_INPUT_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_eps_bytes={MAX_EPS_INPUT_BYTES}"),
            actual: format!("{} bytes", bytes.len()),
        });
    }

    let ps_slice = unwrap_dos_binary_eps(bytes)?;
    let dsc = parse_eps_dsc(ps_slice);

    // Validate that the payload actually looks like PostScript/EPS
    let trimmed_start = ps_slice
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(ps_slice.len());
    let head = &ps_slice[trimmed_start..];
    let has_ps_header = head.starts_with(b"%!")
        || head
            .windows(4)
            .take(1024)
            .any(|w| w == b"%!PS" || w == b"EPSF");
    if !has_ps_header && dsc.bbox.is_none() {
        // Also allow bare PostScript snippets that contain known drawing operators
        let preview = String::from_utf8_lossy(&head[..head.len().min(4096)]);
        let looks_like_ps = [
            "moveto",
            "lineto",
            "curveto",
            "rectfill",
            "rectstroke",
            "show",
        ]
        .iter()
        .any(|kw| preview.contains(kw));
        if !looks_like_ps {
            return Err(IoError::InvalidFormat {
                format: "eps".into(),
                message: "missing '%!PS' PostScript header or bounding box".into(),
            });
        }
    }

    let tokens = tokenize_ps(ps_slice)?;
    let mut interp = PsInterpreter::new();
    interp.exec_tokens(&tokens, 0)?;

    // Determine page bounds in EPS points
    let bbox = dsc.bbox.or_else(|| {
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for item in &interp.emitted {
            if let Some(bb) = item.eps_bounds() {
                min_x = min_x.min(bb.min_x);
                min_y = min_y.min(bb.min_y);
                max_x = max_x.max(bb.max_x);
                max_y = max_y.max(bb.max_y);
            }
        }
        if min_x.is_finite() && max_x > min_x && max_y > min_y {
            let pad = 10.0 * MM_TO_PT;
            Some([min_x - pad, min_y - pad, max_x + pad, max_y + pad])
        } else {
            None
        }
    });

    let [llx, lly, urx, ury] = bbox.unwrap_or([0.0, 0.0, 210.0 * MM_TO_PT, 297.0 * MM_TO_PT]);
    let w_pt = (urx - llx).abs().max(1.0);
    let h_pt = (ury - lly).abs().max(1.0);
    let min_x = llx.min(urx);
    let max_y = lly.max(ury);
    let page_w_mm = (w_pt * PT_TO_MM).max(1.0);
    let page_h_mm = (h_pt * PT_TO_MM).max(1.0);

    let eps_to_mm = Affine2D::new(
        PT_TO_MM,
        0.0,
        0.0,
        -PT_TO_MM,
        -min_x * PT_TO_MM,
        max_y * PT_TO_MM,
    );

    let color_mode = if interp.saw_cmyk && !interp.saw_rgb {
        ColorMode::Cmyk
    } else {
        ColorMode::Rgb
    };
    let doc_title = dsc
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| {
            if title.trim().is_empty() {
                "Imported EPS".to_owned()
            } else {
                title.to_owned()
            }
        });

    let mut doc = Document::new(doc_title, page_w_mm, page_h_mm, color_mode, 300.0);
    doc.metadata_mut().source_format = Some("eps".to_owned());

    let mut clip_stack: Vec<(BezierPath, Vec<ObjectId>)> = Vec::new();
    let record_oid = |clip_stack: &mut Vec<(BezierPath, Vec<ObjectId>)>, oid: ObjectId| {
        if let Some((_, children)) = clip_stack.last_mut() {
            children.push(oid);
        }
    };
    let flush_one_clip = |doc: &mut Document,
                          clip_stack: &mut Vec<(BezierPath, Vec<ObjectId>)>,
                          idx: usize|
     -> IoResult<()> {
        if let Some((clip_path, children)) = clip_stack.pop() {
            if !children.is_empty() {
                let container = SceneObject::new(
                    ObjectId(0),
                    format!("EPS Clip {}", idx + 1),
                    NodePayload::Path(clip_path),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none());
                let pc_id = doc.add_object(container, None)?;
                if let Ok(created) = doc.create_powerclip(pc_id, &children) {
                    if let Some((_, parent_children)) = clip_stack.last_mut() {
                        parent_children.push(created);
                    }
                } else if let Some((_, parent_children)) = clip_stack.last_mut() {
                    parent_children.push(pc_id);
                }
            }
        }
        Ok(())
    };

    for (idx, item) in interp.emitted.drain(..).enumerate() {
        match item {
            EpsEmitted::ClipPush { eps_path } => {
                let mut bez = BezierPath::from_kurbo(&eps_path);
                bez.transform(&eps_to_mm);
                if !bez.is_empty() {
                    clip_stack.push((bez, Vec::new()));
                }
            }
            EpsEmitted::ClipPop => {
                flush_one_clip(&mut doc, &mut clip_stack, idx)?;
            }
            EpsEmitted::Path {
                eps_path,
                mut fill,
                outline,
            } => {
                let mut bez = BezierPath::from_kurbo(&eps_path);
                bez.transform(&eps_to_mm);
                if bez.is_empty() {
                    continue;
                }
                match &mut fill {
                    Fill::LinearGradient { start, end, .. } => {
                        *start = eps_to_mm.transform_point(*start);
                        *end = eps_to_mm.transform_point(*end);
                    }
                    Fill::RadialGradient {
                        center,
                        radius,
                        focal,
                        ..
                    } => {
                        *center = eps_to_mm.transform_point(*center);
                        *focal = eps_to_mm.transform_point(*focal);
                        *radius *= PT_TO_MM;
                    }
                    _ => {}
                }
                let obj = SceneObject::new(
                    ObjectId(0),
                    format!("EPS Path {}", idx + 1),
                    NodePayload::Path(bez),
                )
                .with_fill(fill)
                .with_outline(outline);
                let oid = doc.add_object(obj, None)?;
                record_oid(&mut clip_stack, oid);
            }
            EpsEmitted::Text {
                text,
                font_family,
                font_size_pt,
                bold,
                italic,
                color,
                ctm,
                origin_eps,
            } => {
                if text.is_empty() {
                    continue;
                }
                let world_xf = eps_to_mm.compose(&ctm);
                let text_local_to_eps = Affine2D::new(
                    MM_TO_PT,
                    0.0,
                    0.0,
                    -MM_TO_PT,
                    origin_eps.x,
                    origin_eps.y + font_size_pt,
                );
                let text_xf = world_xf.compose(&text_local_to_eps);
                if !text_xf.is_finite() {
                    continue;
                }
                let style = TextStyle {
                    font_family,
                    font_size_pt: font_size_pt.max(1.0),
                    bold,
                    italic,
                    fill: color,
                    direction: TextDirection::Auto,
                    ..TextStyle::default()
                };
                let obj = SceneObject::new(
                    ObjectId(0),
                    format!("EPS Text {}", idx + 1),
                    NodePayload::ArtisticText(ArtisticText::from_plain(text, style)),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none())
                .with_transform(text_xf);
                let oid = doc.add_object(obj, None)?;
                record_oid(&mut clip_stack, oid);
            }
            EpsEmitted::Raster {
                width,
                height,
                rgba_bytes,
                ctm,
            } => {
                if let Ok(asset) =
                    coree_core::RasterAsset::new(width, height, rgba_bytes, 300.0, doc.color_mode())
                {
                    let asset_id = doc.register_asset(asset);
                    // PostScript image maps [0,1]x[0,1] bottom-to-top in CTM -> convert corners via `eps_to_mm * ctm`
                    let world_xf = eps_to_mm.compose(&ctm);
                    let tl_mm = world_xf.transform_point(Point2D::new(0.0, 1.0));
                    let br_x_mm = world_xf.transform_point(Point2D::new(1.0, 1.0));
                    let bl_mm = world_xf.transform_point(Point2D::new(0.0, 0.0));
                    let w_mm = (br_x_mm - tl_mm).length().max(0.1);
                    let h_mm = (bl_mm - tl_mm).length().max(0.1);
                    let raster_xf = Affine2D::translate(tl_mm.x, tl_mm.y);
                    let obj = SceneObject::new(
                        ObjectId(0),
                        format!("EPS Raster {}", idx + 1),
                        NodePayload::Raster(coree_core::RasterLayerData {
                            asset_id,
                            width_mm: w_mm,
                            height_mm: h_mm,
                            filters: Vec::new(),
                            filter_enabled: Vec::new(),
                            crop_rect: None,
                        }),
                    )
                    .with_fill(Fill::None)
                    .with_outline(OutlineStyle::none())
                    .with_transform(raster_xf);
                    let oid = doc.add_object(obj, None)?;
                    record_oid(&mut clip_stack, oid);
                }
            }
        }
    }
    while !clip_stack.is_empty() {
        flush_one_clip(&mut doc, &mut clip_stack, usize::MAX - 1)?;
    }
    drop(interp);

    doc.refresh_document_palette();
    Ok(doc)
}

/// Strips a DOS Binary EPS header (`0xC5 0xD0 0xD3 0xC6`) when present, returning the
/// embedded PostScript slice; otherwise returns `bytes` unchanged.
fn unwrap_dos_binary_eps(bytes: &[u8]) -> IoResult<&[u8]> {
    if bytes.len() >= 4 && bytes[0..4] == [0xC5, 0xD0, 0xD3, 0xC6] {
        if bytes.len() < 30 {
            return Err(IoError::InvalidFormat {
                format: "eps".into(),
                message: "truncated DOS binary EPS header".into(),
            });
        }
        let ps_offset = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let ps_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        let ps_end = ps_offset
            .checked_add(ps_len)
            .ok_or_else(|| IoError::InvalidFormat {
                format: "eps".into(),
                message: "DOS binary EPS offset overflow".into(),
            })?;
        if ps_len == 0 || ps_end > bytes.len() {
            return Err(IoError::InvalidFormat {
                format: "eps".into(),
                message: format!(
                    "DOS binary EPS PostScript slice ({ps_offset}..{ps_end}) out of bounds ({})",
                    bytes.len()
                ),
            });
        }
        return Ok(&bytes[ps_offset..ps_end]);
    }
    Ok(bytes)
}

#[derive(Default)]
struct EpsDscHeader {
    bbox: Option<[f64; 4]>,
    title: Option<String>,
}

fn parse_eps_dsc(bytes: &[u8]) -> EpsDscHeader {
    let mut dsc = EpsDscHeader::default();
    let mut hires_bbox: Option<[f64; 4]> = None;
    let mut std_bbox: Option<[f64; 4]> = None;

    for raw_line in bytes.split(|&b| b == b'\n' || b == b'\r') {
        if !raw_line.starts_with(b"%%") {
            continue;
        }
        let line = String::from_utf8_lossy(raw_line);
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("%%HiResBoundingBox:") {
            if let Some(bb) = parse_four_floats(rest) {
                hires_bbox = Some(bb);
            }
        } else if let Some(rest) = trimmed.strip_prefix("%%BoundingBox:") {
            if let Some(bb) = parse_four_floats(rest) {
                std_bbox = Some(bb);
            }
        } else if let Some(rest) = trimmed.strip_prefix("%%Title:") {
            let t = rest.trim().trim_matches(|c| c == '(' || c == ')').trim();
            if !t.is_empty() && dsc.title.is_none() {
                dsc.title = Some(t.to_owned());
            }
        }
    }
    dsc.bbox = hires_bbox.or(std_bbox);
    dsc
}

fn parse_four_floats(s: &str) -> Option<[f64; 4]> {
    let mut iter = s.split_whitespace();
    let a = iter.next()?.parse::<f64>().ok()?;
    let b = iter.next()?.parse::<f64>().ok()?;
    let c = iter.next()?.parse::<f64>().ok()?;
    let d = iter.next()?.parse::<f64>().ok()?;
    if a.is_finite() && b.is_finite() && c.is_finite() && d.is_finite() {
        Some([a, b, c, d])
    } else {
        None
    }
}

#[derive(Debug, Clone)]
enum PsToken {
    Number(f64),
    LiteralName(String),
    ExecName(String),
    Str(Vec<u8>),
    Bool(bool),
    Null,
    ArrayStart,
    ArrayEnd,
    Proc(Vec<PsToken>),
}

#[derive(Debug, Clone)]
enum PsVal {
    Number(f64),
    Name(String),
    Str(Vec<u8>),
    Bool(bool),
    Null,
    Mark,
    Array(Vec<PsVal>),
    Dict(Vec<(String, PsVal)>),
    Proc(Vec<PsToken>),
    Font {
        family: String,
        size_pt: f64,
        bold: bool,
        italic: bool,
    },
}

impl PsVal {
    fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::Number(v) if v.is_finite() => Some(v),
            _ => None,
        }
    }

    fn node_count(&self, limit: usize) -> usize {
        match self {
            Self::Array(items) => {
                let mut total = 1usize;
                for item in items {
                    total = total.saturating_add(item.node_count(limit.saturating_sub(total)));
                    if total > limit {
                        return total;
                    }
                }
                total
            }
            Self::Dict(entries) => {
                let mut total = 1usize;
                for (_, val) in entries {
                    total = total.saturating_add(val.node_count(limit.saturating_sub(total)));
                    if total > limit {
                        return total;
                    }
                }
                total
            }
            Self::Proc(toks) => 1usize.saturating_add(toks.len()),
            Self::Str(_) => 1,
            _ => 1,
        }
    }

    fn byte_size(&self, limit: usize) -> usize {
        match self {
            Self::Array(items) => {
                let mut total = 0usize;
                for item in items {
                    total = total.saturating_add(item.byte_size(limit.saturating_sub(total)));
                    if total > limit {
                        return total;
                    }
                }
                total
            }
            Self::Dict(entries) => {
                let mut total = 0usize;
                for (_, val) in entries {
                    total = total.saturating_add(val.byte_size(limit.saturating_sub(total)));
                    if total > limit {
                        return total;
                    }
                }
                total
            }
            Self::Str(b) => b.len(),
            _ => 0,
        }
    }
}

fn is_ps_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn tokenize_ps(bytes: &[u8]) -> IoResult<Vec<PsToken>> {
    let mut pos = 0usize;
    let mut total_tokens = 0usize;
    tokenize_ps_inner(bytes, &mut pos, false, 0, &mut total_tokens)
}

fn tokenize_ps_inner(
    bytes: &[u8],
    pos: &mut usize,
    stop_on_rbrace: bool,
    depth: usize,
    total_tokens: &mut usize,
) -> IoResult<Vec<PsToken>> {
    if depth > MAX_EPS_DEPTH {
        return Err(IoError::LimitExceeded {
            limit: format!("max_ps_proc_depth={MAX_EPS_DEPTH}"),
            actual: format!("depth={depth}"),
        });
    }
    let mut tokens = Vec::new();
    let len = bytes.len();
    let mut saw_asciihex_datasource = false;
    let mut saw_ascii85_datasource = false;
    let mut saw_reusable_filter = false;

    while *pos < len {
        if *total_tokens >= MAX_PS_TOKENS {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_tokens={MAX_PS_TOKENS}"),
                actual: format!("exceeded {MAX_PS_TOKENS} tokens"),
            });
        }
        let b = bytes[*pos];
        if b.is_ascii_whitespace() || b == 0 {
            *pos += 1;
            continue;
        }
        if b == b'%' {
            while *pos < len && bytes[*pos] != b'\n' && bytes[*pos] != b'\r' {
                *pos += 1;
            }
            continue;
        }
        if b == b'}' {
            *pos += 1;
            if stop_on_rbrace {
                return Ok(tokens);
            }
            continue;
        }
        if b == b'{' {
            *pos += 1;
            *total_tokens += 1;
            let proc_tokens = tokenize_ps_inner(bytes, pos, true, depth + 1, total_tokens)?;
            tokens.push(PsToken::Proc(proc_tokens));
            continue;
        }
        if b == b'[' {
            *pos += 1;
            *total_tokens += 1;
            tokens.push(PsToken::ArrayStart);
            continue;
        }
        if b == b']' {
            *pos += 1;
            *total_tokens += 1;
            tokens.push(PsToken::ArrayEnd);
            continue;
        }
        if b == b'(' {
            let s = read_ps_literal_string(bytes, pos);
            *total_tokens += 1;
            tokens.push(PsToken::Str(s));
            continue;
        }
        if b == b')' {
            // Unmatched closing paren — consume 1 byte cleanly to guarantee forward progress.
            *pos += 1;
            continue;
        }
        if b == b'<' {
            if *pos + 1 < len && bytes[*pos + 1] == b'<' {
                *pos += 2;
                *total_tokens += 1;
                tokens.push(PsToken::ExecName("<<".to_owned()));
            } else if *pos + 1 < len && bytes[*pos + 1] == b'~' {
                *pos += 2;
                let s = read_ps_inline_ascii85_stream(bytes, pos)?;
                *total_tokens += 1;
                tokens.push(PsToken::Str(s));
            } else {
                let s = read_ps_hex_string(bytes, pos);
                *total_tokens += 1;
                tokens.push(PsToken::Str(s));
            }
            continue;
        }
        if b == b'>' {
            if *pos + 1 < len && bytes[*pos + 1] == b'>' {
                *pos += 2;
                *total_tokens += 1;
                tokens.push(PsToken::ExecName(">>".to_owned()));
            } else {
                *pos += 1;
            }
            continue;
        }
        if b == b'/' {
            *pos += 1;
            // Support immediately evaluated `//name` as name too
            if *pos < len && bytes[*pos] == b'/' {
                *pos += 1;
            }
            let start = *pos;
            while *pos < len && !bytes[*pos].is_ascii_whitespace() && !is_ps_delim(bytes[*pos]) {
                *pos += 1;
            }
            let name = String::from_utf8_lossy(&bytes[start..*pos]).into_owned();
            if name == "ASCIIHexDecode" {
                saw_asciihex_datasource = true;
                saw_ascii85_datasource = false;
            } else if name == "ASCII85Decode" {
                saw_ascii85_datasource = true;
                saw_asciihex_datasource = false;
            } else if name == "ReusableStreamDecode" {
                saw_reusable_filter = true;
            }
            *total_tokens += 1;
            tokens.push(PsToken::LiteralName(name));
            continue;
        }

        // Regular token (number, boolean, null, or executable name)
        let start = *pos;
        while *pos < len && !bytes[*pos].is_ascii_whitespace() && !is_ps_delim(bytes[*pos]) {
            *pos += 1;
        }
        if *pos == start {
            // Defensive progress guarantee: never spin on an unhandled delimiter byte
            *pos += 1;
            continue;
        }
        let word = String::from_utf8_lossy(&bytes[start..*pos]);
        *total_tokens += 1;
        if word == "filter"
            && saw_reusable_filter
            && (saw_asciihex_datasource || saw_ascii85_datasource)
        {
            saw_reusable_filter = false;
            let use_a85 = saw_ascii85_datasource;
            saw_asciihex_datasource = false;
            saw_ascii85_datasource = false;
            // Truncate back to `currentfile` so the stack holds only the preceding
            // `/name` followed by the decoded inline stream string (for `def` or `exch def`).
            if let Some(cf_idx) = tokens
                .iter()
                .rposition(|t| matches!(t, PsToken::ExecName(n) if n == "currentfile"))
            {
                if tokens.len().saturating_sub(cf_idx) <= 12 {
                    tokens.truncate(cf_idx);
                }
            }
            // Check if `def` precedes or follows the inline stream.
            let mut look = *pos;
            while look < len && bytes[look].is_ascii_whitespace() {
                look += 1;
            }
            let def_before = bytes[look..].starts_with(b"def")
                && (look + 3 >= len
                    || bytes[look + 3].is_ascii_whitespace()
                    || is_ps_delim(bytes[look + 3]));
            if def_before {
                *pos = look + 3;
                let raw_img = if use_a85 {
                    read_ps_inline_ascii85_stream(bytes, pos)?
                } else {
                    read_ps_inline_hex_stream(bytes, pos)
                };
                tokens.push(PsToken::Str(raw_img));
                tokens.push(PsToken::ExecName("def".to_owned()));
            } else {
                let raw_img = if use_a85 {
                    read_ps_inline_ascii85_stream(bytes, pos)?
                } else {
                    read_ps_inline_hex_stream(bytes, pos)
                };
                tokens.push(PsToken::Str(raw_img));
            }
        } else if (word == "image" || word == "def")
            && (saw_asciihex_datasource || saw_ascii85_datasource)
            && !saw_reusable_filter
        {
            let use_a85 = saw_ascii85_datasource;
            saw_asciihex_datasource = false;
            saw_ascii85_datasource = false;
            if word == "def" {
                tokens.push(PsToken::ExecName("def".to_owned()));
                let raw_img = if use_a85 {
                    read_ps_inline_ascii85_stream(bytes, pos)?
                } else {
                    read_ps_inline_hex_stream(bytes, pos)
                };
                if let Some(PsToken::LiteralName(var_name)) =
                    tokens.iter().rev().find_map(|t| match t {
                        PsToken::LiteralName(_) => Some(t.clone()),
                        _ => None,
                    })
                {
                    tokens.push(PsToken::LiteralName(var_name));
                    tokens.push(PsToken::Str(raw_img));
                    tokens.push(PsToken::ExecName("def".to_owned()));
                }
            } else {
                let raw_img = if use_a85 {
                    read_ps_inline_ascii85_stream(bytes, pos)?
                } else {
                    read_ps_inline_hex_stream(bytes, pos)
                };
                tokens.push(PsToken::Str(raw_img));
                tokens.push(PsToken::ExecName("image".to_owned()));
            }
        } else if let Some(num) = parse_ps_number(&word) {
            tokens.push(PsToken::Number(num));
        } else if word == "true" {
            tokens.push(PsToken::Bool(true));
        } else if word == "false" {
            tokens.push(PsToken::Bool(false));
        } else if word == "null" {
            tokens.push(PsToken::Null);
        } else {
            if word == "shfill" || word == "colorimage" {
                saw_asciihex_datasource = false;
                saw_ascii85_datasource = false;
            }
            tokens.push(PsToken::ExecName(word.into_owned()));
        }
    }

    Ok(tokens)
}

fn read_ps_literal_string(bytes: &[u8], pos: &mut usize) -> Vec<u8> {
    let mut out = Vec::new();
    let len = bytes.len();
    *pos += 1; // skip '('
    let mut paren_depth = 1u32;

    while *pos < len && paren_depth > 0 {
        let b = bytes[*pos];
        *pos += 1;
        match b {
            b'(' => {
                paren_depth += 1;
                out.push(b'(');
            }
            b')' => {
                paren_depth -= 1;
                if paren_depth > 0 {
                    out.push(b')');
                }
            }
            b'\\' => {
                if *pos >= len {
                    break;
                }
                let esc = bytes[*pos];
                *pos += 1;
                match esc {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b'\n' => {}
                    b'\r' => {
                        if *pos < len && bytes[*pos] == b'\n' {
                            *pos += 1;
                        }
                    }
                    b'0'..=b'7' => {
                        let mut val = u32::from(esc - b'0');
                        for _ in 0..2 {
                            if *pos < len && matches!(bytes[*pos], b'0'..=b'7') {
                                val = val * 8 + u32::from(bytes[*pos] - b'0');
                                *pos += 1;
                            } else {
                                break;
                            }
                        }
                        out.push((val & 0xFF) as u8);
                    }
                    other => out.push(other),
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn read_ps_hex_string(bytes: &[u8], pos: &mut usize) -> Vec<u8> {
    *pos += 1; // skip '<'
    read_ps_inline_hex_stream(bytes, pos)
}

fn read_ps_inline_ascii85_stream(bytes: &[u8], pos: &mut usize) -> IoResult<Vec<u8>> {
    read_ps_inline_ascii85_stream_with_limit(bytes, pos, MAX_PS_STREAM_BYTES)
}

fn read_ps_inline_ascii85_stream_with_limit(
    bytes: &[u8],
    pos: &mut usize,
    max_bytes: usize,
) -> IoResult<Vec<u8>> {
    let len = bytes.len();
    while *pos < len && (bytes[*pos].is_ascii_whitespace() || bytes[*pos] == 0) {
        *pos += 1;
    }
    if *pos + 1 < len && bytes[*pos] == b'<' && bytes[*pos + 1] == b'~' {
        *pos += 2;
    }
    let mut out = Vec::new();
    let mut tuple = [0u8; 5];
    let mut count = 0usize;
    while *pos < len {
        let b = bytes[*pos];
        *pos += 1;
        if b == b'~' {
            if *pos < len && bytes[*pos] == b'>' {
                *pos += 1;
            }
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'z' {
            if count != 0 {
                return Err(IoError::InvalidFormat {
                    format: "eps".into(),
                    message: "unexpected 'z' inside ASCII85 5-tuple".into(),
                });
            }
            if out.len().saturating_add(4) > max_bytes {
                return Err(IoError::LimitExceeded {
                    limit: format!("max_ps_stream_bytes={max_bytes}"),
                    actual: format!("ASCII85 stream exceeded {max_bytes} bytes"),
                });
            }
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if (b'!'..=b'u').contains(&b) {
            tuple[count] = b - b'!';
            count += 1;
            if count == 5 {
                let mut val = 0u64;
                for &d in &tuple {
                    val = val * 85 + u64::from(d);
                }
                if val > u64::from(u32::MAX) {
                    return Err(IoError::InvalidFormat {
                        format: "eps".into(),
                        message: format!("ASCII85 5-tuple value {val} exceeds u32::MAX"),
                    });
                }
                if out.len().saturating_add(4) > max_bytes {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_ps_stream_bytes={max_bytes}"),
                        actual: format!("ASCII85 stream exceeded {max_bytes} bytes"),
                    });
                }
                out.extend_from_slice(&(val as u32).to_be_bytes());
                count = 0;
            }
        }
    }
    if count == 1 {
        return Err(IoError::InvalidFormat {
            format: "eps".into(),
            message: "invalid 1-character ASCII85 tail".into(),
        });
    }
    if count >= 2 {
        for slot in tuple.iter_mut().skip(count) {
            *slot = 84;
        }
        let mut val = 0u64;
        for &d in &tuple {
            val = val * 85 + u64::from(d);
        }
        if val > u64::from(u32::MAX) {
            return Err(IoError::InvalidFormat {
                format: "eps".into(),
                message: format!("ASCII85 partial tail value {val} exceeds u32::MAX"),
            });
        }
        let rem = count - 1;
        if out.len().saturating_add(rem) > max_bytes {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_stream_bytes={max_bytes}"),
                actual: format!("ASCII85 stream exceeded {max_bytes} bytes"),
            });
        }
        let bytes4 = (val as u32).to_be_bytes();
        out.extend_from_slice(&bytes4[..rem]);
    }
    Ok(out)
}

fn read_ps_inline_hex_stream(bytes: &[u8], pos: &mut usize) -> Vec<u8> {
    let mut out = Vec::new();
    let len = bytes.len();
    let mut hi: Option<u8> = None;
    while *pos < len {
        let b = bytes[*pos];
        *pos += 1;
        if b == b'>' {
            break;
        }
        let nib = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue,
        };
        if let Some(h) = hi.take() {
            out.push((h << 4) | nib);
        } else {
            hi = Some(nib);
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    out
}

fn parse_ps_number(word: &str) -> Option<f64> {
    // Support PostScript radix integers like `16#FF`
    if let Some((base_s, digits)) = word.split_once('#') {
        if let Ok(base) = base_s.parse::<u32>() {
            if (2..=36).contains(&base) {
                if let Ok(val) = u64::from_str_radix(digits, base) {
                    return Some(val as f64);
                }
            }
        }
        return None;
    }
    let first = word.as_bytes().first().copied()?;
    if !matches!(first, b'0'..=b'9' | b'+' | b'-' | b'.') {
        return None;
    }
    word.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[derive(Debug, Clone)]
struct PsGState {
    ctm: Affine2D,
    fill_color: Color,
    stroke_color: Color,
    spot_space: Option<(String, [f32; 4])>,
    clip_count: usize,
    line_width_pt: f64,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: f64,
    dash_pattern_pt: Vec<f64>,
    overprint: bool,
    font_family: String,
    font_size_pt: f64,
    font_bold: bool,
    font_italic: bool,
}

impl Default for PsGState {
    fn default() -> Self {
        Self {
            ctm: Affine2D::IDENTITY,
            fill_color: Color::BLACK,
            stroke_color: Color::BLACK,
            spot_space: None,
            clip_count: 0,
            line_width_pt: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash_pattern_pt: Vec::new(),
            overprint: false,
            font_family: "Helvetica".to_owned(),
            font_size_pt: 12.0,
            font_bold: false,
            font_italic: false,
        }
    }
}

enum EpsEmitted {
    ClipPush {
        eps_path: BezPath,
    },
    ClipPop,
    Path {
        eps_path: BezPath,
        fill: Fill,
        outline: OutlineStyle,
    },
    Text {
        text: String,
        font_family: String,
        font_size_pt: f64,
        bold: bool,
        italic: bool,
        color: Color,
        ctm: Affine2D,
        origin_eps: Point2D,
    },
    Raster {
        width: u32,
        height: u32,
        rgba_bytes: Vec<u8>,
        ctm: Affine2D,
    },
}

impl EpsEmitted {
    fn eps_bounds(&self) -> Option<coree_core::Rect2D> {
        match self {
            Self::Raster { ctm, .. } => {
                let p0 = ctm.transform_point(Point2D::new(0.0, 0.0));
                let p1 = ctm.transform_point(Point2D::new(1.0, 1.0));
                let r = coree_core::Rect2D::from_points(p0, p1);
                r.is_finite().then_some(r)
            }
            Self::ClipPush { eps_path } | Self::Path { eps_path, .. } => {
                if eps_path.is_empty() {
                    None
                } else {
                    use kurbo::Shape;
                    let r = coree_core::Rect2D::from_kurbo(eps_path.bounding_box());
                    r.is_finite().then_some(r)
                }
            }
            Self::ClipPop => None,
            Self::Text {
                ctm,
                origin_eps,
                font_size_pt,
                text,
                ..
            } => {
                let p0 = ctm.transform_point(*origin_eps);
                let p1 = ctm.transform_point(Point2D::new(
                    origin_eps.x + (text.chars().count().max(1) as f64) * font_size_pt * 0.6,
                    origin_eps.y + font_size_pt,
                ));
                let r = coree_core::Rect2D::from_points(p0, p1);
                r.is_finite().then_some(r)
            }
        }
    }
}

struct PsInterpreter {
    stack: Vec<PsVal>,
    stack_nodes: usize,
    stack_bytes: usize,
    dict: HashMap<String, PsVal>,
    dict_bytes: usize,
    gstate: PsGState,
    gstack: Vec<PsGState>,
    path: BezPath,
    cur_pt: Option<Point2D>,
    subpath_start: Option<Point2D>,
    emitted: Vec<EpsEmitted>,
    steps: usize,
    total_vertices: usize,
    saw_cmyk: bool,
    saw_rgb: bool,
}

impl PsInterpreter {
    fn new() -> Self {
        Self {
            stack: Vec::new(),
            stack_nodes: 0,
            stack_bytes: 0,
            dict: HashMap::new(),
            dict_bytes: 0,
            gstate: PsGState::default(),
            gstack: Vec::new(),
            path: BezPath::new(),
            cur_pt: None,
            subpath_start: None,
            emitted: Vec::new(),
            steps: 0,
            total_vertices: 0,
            saw_cmyk: false,
            saw_rgb: false,
        }
    }

    fn check_current_path_vertices(&self, incoming: usize) -> IoResult<()> {
        let projected = self
            .total_vertices
            .saturating_add(self.path.elements().len())
            .saturating_add(incoming);
        if projected > MAX_PS_TOTAL_VERTICES {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_vertices={MAX_PS_TOTAL_VERTICES}"),
                actual: format!("EPS vertex count {projected} exceeded {MAX_PS_TOTAL_VERTICES}"),
            });
        }
        Ok(())
    }

    fn push_emitted(&mut self, item: EpsEmitted) -> IoResult<()> {
        if self.emitted.len() >= MAX_PS_EMITTED_OBJECTS {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_objects={MAX_PS_EMITTED_OBJECTS}"),
                actual: format!("EPS emitted object count exceeded {MAX_PS_EMITTED_OBJECTS}"),
            });
        }
        let verts = match &item {
            EpsEmitted::Path { eps_path, .. } | EpsEmitted::ClipPush { eps_path } => {
                eps_path.elements().len()
            }
            _ => 0,
        };
        let next_verts = self.total_vertices.saturating_add(verts);
        if next_verts > MAX_PS_TOTAL_VERTICES {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_vertices={MAX_PS_TOTAL_VERTICES}"),
                actual: format!("EPS vertex count {next_verts} exceeded {MAX_PS_TOTAL_VERTICES}"),
            });
        }
        self.total_vertices = next_verts;
        self.emitted.push(item);
        Ok(())
    }

    fn charge_step(&mut self) -> IoResult<()> {
        self.steps += 1;
        if self.steps > MAX_PS_STEPS {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_steps={MAX_PS_STEPS}"),
                actual: format!("exceeded {MAX_PS_STEPS} PostScript operations"),
            });
        }
        Ok(())
    }

    fn check_push_budget(&self, incoming_nodes: usize, incoming_bytes: usize) -> IoResult<()> {
        if self.stack.len() >= MAX_PS_STACK_LEN {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_stack_len={MAX_PS_STACK_LEN}"),
                actual: format!("stack depth exceeded {MAX_PS_STACK_LEN}"),
            });
        }
        if self.stack_nodes.saturating_add(incoming_nodes) > MAX_PS_STACK_NODES {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_stack_nodes={MAX_PS_STACK_NODES}"),
                actual: format!("stack value nodes exceeded {MAX_PS_STACK_NODES}"),
            });
        }
        if self.stack_bytes.saturating_add(incoming_bytes) > MAX_PS_STREAM_BYTES {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_stream_bytes={MAX_PS_STREAM_BYTES}"),
                actual: format!("stack stream bytes exceeded {MAX_PS_STREAM_BYTES}"),
            });
        }
        Ok(())
    }

    fn push(&mut self, val: PsVal) -> IoResult<()> {
        let cost = val.node_count(MAX_PS_STACK_NODES);
        let byte_cost = val.byte_size(MAX_PS_STREAM_BYTES);
        self.check_push_budget(cost, byte_cost)?;
        self.stack_nodes = self.stack_nodes.saturating_add(cost);
        self.stack_bytes = self.stack_bytes.saturating_add(byte_cost);
        self.stack.push(val);
        Ok(())
    }

    fn pop(&mut self) -> Option<PsVal> {
        let val = self.stack.pop()?;
        let cost = val.node_count(MAX_PS_STACK_NODES);
        let byte_cost = val.byte_size(MAX_PS_STREAM_BYTES);
        self.stack_nodes = self.stack_nodes.saturating_sub(cost);
        self.stack_bytes = self.stack_bytes.saturating_sub(byte_cost);
        Some(val)
    }

    fn pop_f64(&mut self) -> Option<f64> {
        self.pop().and_then(|v| v.as_f64())
    }

    fn exec_tokens(&mut self, tokens: &[PsToken], depth: usize) -> IoResult<()> {
        if depth > MAX_PS_CALL_DEPTH {
            return Ok(());
        }
        for tok in tokens {
            self.charge_step()?;
            match tok {
                PsToken::Number(n) => self.push(PsVal::Number(*n))?,
                PsToken::LiteralName(s) => self.push(PsVal::Name(s.clone()))?,
                PsToken::Str(b) => self.push(PsVal::Str(b.clone()))?,
                PsToken::Bool(b) => self.push(PsVal::Bool(*b))?,
                PsToken::Null => self.push(PsVal::Null)?,
                PsToken::ArrayStart => self.push(PsVal::Mark)?,
                PsToken::ArrayEnd => {
                    let mut items = Vec::new();
                    while let Some(v) = self.pop() {
                        if matches!(v, PsVal::Mark) {
                            break;
                        }
                        items.push(v);
                    }
                    items.reverse();
                    self.push(PsVal::Array(items))?;
                }
                PsToken::Proc(p) => self.push(PsVal::Proc(p.clone()))?,
                PsToken::ExecName(name) => {
                    if let Some(bound_ref) = self.dict.get(name) {
                        let cost = bound_ref.node_count(MAX_PS_STACK_NODES);
                        let byte_cost = bound_ref.byte_size(MAX_PS_STREAM_BYTES);
                        if !matches!(bound_ref, PsVal::Proc(_)) {
                            self.check_push_budget(cost, byte_cost)?;
                        }
                        let bound = bound_ref.clone();
                        match bound {
                            PsVal::Proc(proc_toks) => {
                                self.exec_tokens(&proc_toks, depth + 1)?;
                            }
                            other => self.push(other)?,
                        }
                    } else {
                        self.exec_builtin(name, depth)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn exec_builtin(&mut self, op: &str, depth: usize) -> IoResult<()> {
        match op {
            // Stack & dictionary primitives
            "pop" => {
                let _ = self.pop();
            }
            "dup" => {
                if let Some(top_ref) = self.stack.last() {
                    let cost = top_ref.node_count(MAX_PS_STACK_NODES);
                    let byte_cost = top_ref.byte_size(MAX_PS_STREAM_BYTES);
                    self.check_push_budget(cost, byte_cost)?;
                    let top = top_ref.clone();
                    self.push(top)?;
                }
            }
            "exch" => {
                let len = self.stack.len();
                if len >= 2 {
                    self.stack.swap(len - 1, len - 2);
                }
            }
            "clear" => {
                self.stack.clear();
                self.stack_nodes = 0;
                self.stack_bytes = 0;
            }
            "mark" | "<<" => self.push(PsVal::Mark)?,
            "cleartomark" => {
                while let Some(v) = self.pop() {
                    if matches!(v, PsVal::Mark) {
                        break;
                    }
                }
            }
            ">>" => {
                let mut pairs = Vec::new();
                let mut collected = Vec::new();
                while let Some(v) = self.pop() {
                    if matches!(v, PsVal::Mark) {
                        break;
                    }
                    collected.push(v);
                }
                collected.reverse();
                for chunk in collected.as_chunks::<2>().0 {
                    if let PsVal::Name(k) = &chunk[0] {
                        pairs.push((k.clone(), chunk[1].clone()));
                    }
                }
                self.push(PsVal::Dict(pairs))?;
            }
            "count" => {
                let n = self.stack.len() as f64;
                self.push(PsVal::Number(n))?;
            }
            "index" => {
                if let Some(n) = self.pop_f64() {
                    let idx = n.max(0.0) as usize;
                    if idx < self.stack.len() {
                        let target_ref = &self.stack[self.stack.len() - 1 - idx];
                        let cost = target_ref.node_count(MAX_PS_STACK_NODES);
                        let byte_cost = target_ref.byte_size(MAX_PS_STREAM_BYTES);
                        self.check_push_budget(cost, byte_cost)?;
                        let val = target_ref.clone();
                        self.push(val)?;
                    }
                }
            }
            "roll" => {
                if let (Some(j), Some(n)) = (self.pop_f64(), self.pop_f64()) {
                    let n = n.max(0.0) as usize;
                    if n > 0 && n <= self.stack.len() {
                        let start = self.stack.len() - n;
                        let shift = (j as isize).rem_euclid(n as isize) as usize;
                        self.stack[start..].rotate_right(shift);
                    }
                }
            }
            "def" => {
                if let (Some(val), Some(PsVal::Name(k))) = (self.pop(), self.pop()) {
                    let nodes = val.node_count(MAX_PS_STACK_NODES);
                    let bytes = val.byte_size(MAX_PS_STREAM_BYTES);
                    let prev_bytes = self
                        .dict
                        .get(&k)
                        .map_or(0, |old| old.byte_size(MAX_PS_STREAM_BYTES));
                    let next_dict_bytes = self
                        .dict_bytes
                        .saturating_sub(prev_bytes)
                        .saturating_add(bytes);
                    if self.dict.len() < 4096
                        && nodes <= 10_000
                        && next_dict_bytes <= MAX_PS_STREAM_BYTES
                    {
                        self.dict_bytes = next_dict_bytes;
                        self.dict.insert(k, val);
                    }
                }
            }
            "closefile" => {
                let _ = self.pop();
            }
            "bind" | "begin" | "end" | "currentdict" | "showpage" => {}
            "dict" => {
                let _ = self.pop();
                self.push(PsVal::Null)?;
            }
            "load" => {
                if let Some(PsVal::Name(k)) = self.pop() {
                    if let Some(v_ref) = self.dict.get(&k) {
                        let cost = v_ref.node_count(MAX_PS_STACK_NODES);
                        let byte_cost = v_ref.byte_size(MAX_PS_STREAM_BYTES);
                        self.check_push_budget(cost, byte_cost)?;
                        let v = v_ref.clone();
                        self.push(v)?;
                    } else {
                        self.push(PsVal::Proc(vec![PsToken::ExecName(k)]))?;
                    }
                }
            }
            "where" => {
                let _ = self.pop();
                self.push(PsVal::Bool(false))?;
            }
            "exec" => {
                if let Some(PsVal::Proc(toks)) = self.pop() {
                    self.exec_tokens(&toks, depth + 1)?;
                }
            }
            "repeat" => {
                if let (Some(PsVal::Proc(toks)), Some(n)) = (self.pop(), self.pop_f64()) {
                    let count = (n.max(0.0) as usize).min(10_000);
                    for _ in 0..count {
                        self.charge_step()?;
                        self.exec_tokens(&toks, depth + 1)?;
                    }
                }
            }

            // Arithmetic
            "add" => {
                if let (Some(b), Some(a)) = (self.pop_f64(), self.pop_f64()) {
                    self.push(PsVal::Number(a + b))?;
                }
            }
            "sub" => {
                if let (Some(b), Some(a)) = (self.pop_f64(), self.pop_f64()) {
                    self.push(PsVal::Number(a - b))?;
                }
            }
            "mul" => {
                if let (Some(b), Some(a)) = (self.pop_f64(), self.pop_f64()) {
                    self.push(PsVal::Number(a * b))?;
                }
            }
            "div" => {
                if let (Some(b), Some(a)) = (self.pop_f64(), self.pop_f64()) {
                    self.push(PsVal::Number(if b.abs() > 1e-12 { a / b } else { 0.0 }))?;
                }
            }
            "neg" => {
                if let Some(a) = self.pop_f64() {
                    self.push(PsVal::Number(-a))?;
                }
            }
            "abs" => {
                if let Some(a) = self.pop_f64() {
                    self.push(PsVal::Number(a.abs()))?;
                }
            }
            "sin" => {
                if let Some(a) = self.pop_f64() {
                    self.push(PsVal::Number(a.to_radians().sin()))?;
                }
            }
            "cos" => {
                if let Some(a) = self.pop_f64() {
                    self.push(PsVal::Number(a.to_radians().cos()))?;
                }
            }
            "sqrt" => {
                if let Some(a) = self.pop_f64() {
                    self.push(PsVal::Number(a.max(0.0).sqrt()))?;
                }
            }

            // Graphics state
            "gsave" | "q" => {
                if self.gstack.len() < MAX_PS_GSTATE_DEPTH {
                    self.gstack.push(self.gstate.clone());
                    self.gstate.clip_count = 0;
                }
            }
            "grestore" | "Q" => {
                if let Some(prev) = self.gstack.pop() {
                    for _ in 0..self.gstate.clip_count {
                        self.push_emitted(EpsEmitted::ClipPop)?;
                    }
                    self.gstate = prev;
                }
            }
            "initgraphics" => {
                self.gstate = PsGState::default();
                self.clear_path();
            }

            // Transforms
            "translate" => {
                if let (Some(ty), Some(tx)) = (self.pop_f64(), self.pop_f64()) {
                    self.gstate.ctm = self.gstate.ctm.compose(&Affine2D::translate(tx, ty));
                }
            }
            "scale" => {
                if let (Some(sy), Some(sx)) = (self.pop_f64(), self.pop_f64()) {
                    self.gstate.ctm = self.gstate.ctm.compose(&Affine2D::scale(sx, sy));
                }
            }
            "rotate" => {
                if let Some(deg) = self.pop_f64() {
                    self.gstate.ctm = self.gstate.ctm.compose(&Affine2D::rotate_deg(deg));
                }
            }
            "concat" => {
                if let Some(PsVal::Array(arr)) = self.pop() {
                    if arr.len() >= 6 {
                        let nums: Vec<f64> = arr.iter().take(6).filter_map(PsVal::as_f64).collect();
                        if nums.len() == 6 {
                            let m =
                                Affine2D::new(nums[0], nums[1], nums[2], nums[3], nums[4], nums[5]);
                            self.gstate.ctm = self.gstate.ctm.compose(&m);
                        }
                    }
                }
            }

            // Colors
            "setrgbcolor" => {
                if let (Some(b), Some(g), Some(r)) =
                    (self.pop_f64(), self.pop_f64(), self.pop_f64())
                {
                    let c = Color::rgb(r as f32, g as f32, b as f32);
                    self.gstate.fill_color = c.clone();
                    self.gstate.stroke_color = c;
                    self.saw_rgb = true;
                }
            }
            "setcmykcolor" => {
                if let (Some(k), Some(y), Some(m), Some(c)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    let col = Color::cmyk(c as f32, m as f32, y as f32, k as f32);
                    self.gstate.fill_color = col.clone();
                    self.gstate.stroke_color = col;
                    self.saw_cmyk = true;
                }
            }
            "setgray" => {
                if let Some(g) = self.pop_f64() {
                    let col = Color::gray(g as f32);
                    self.gstate.fill_color = col.clone();
                    self.gstate.stroke_color = col;
                }
            }
            // Adobe Illustrator EPS shorthand colors
            "k" => {
                if let (Some(k), Some(y), Some(m), Some(c)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.gstate.fill_color = Color::cmyk(c as f32, m as f32, y as f32, k as f32);
                    self.saw_cmyk = true;
                }
            }
            "K" => {
                if let (Some(k), Some(y), Some(m), Some(c)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.gstate.stroke_color = Color::cmyk(c as f32, m as f32, y as f32, k as f32);
                    self.saw_cmyk = true;
                }
            }
            "g" => {
                if let Some(g) = self.pop_f64() {
                    self.gstate.fill_color = Color::gray(g as f32);
                }
            }
            "G" => {
                if let Some(g) = self.pop_f64() {
                    self.gstate.stroke_color = Color::gray(g as f32);
                }
            }
            "setcolorspace" => {
                if let Some(val) = self.pop() {
                    match val {
                        PsVal::Array(arr) => {
                            if arr.first().and_then(|v| match v {
                                PsVal::Name(n) => Some(n.as_str()),
                                _ => None,
                            }) == Some("Separation")
                            {
                                let spot_name = arr
                                    .get(1)
                                    .and_then(|v| match v {
                                        PsVal::Str(b) => {
                                            Some(String::from_utf8_lossy(b).into_owned())
                                        }
                                        PsVal::Name(n) => Some(n.clone()),
                                        _ => None,
                                    })
                                    .unwrap_or_else(|| "Spot".to_owned());
                                let mut cmyk = [0.0f32, 0.0, 0.0, 1.0];
                                if let Some(PsVal::Proc(toks)) = arr.get(3) {
                                    let nums: Vec<f32> = toks
                                        .iter()
                                        .filter_map(|t| match t {
                                            PsToken::Number(v) => Some((*v as f32).clamp(0.0, 1.0)),
                                            _ => None,
                                        })
                                        .collect();
                                    if nums.len() >= 4 {
                                        cmyk = [nums[0], nums[1], nums[2], nums[3]];
                                    }
                                }
                                self.gstate.spot_space = Some((spot_name, cmyk));
                                self.saw_cmyk = true;
                            } else {
                                self.gstate.spot_space = None;
                            }
                        }
                        PsVal::Name(n) => {
                            self.gstate.spot_space = None;
                            if n == "DeviceCMYK" {
                                self.saw_cmyk = true;
                            } else if n == "DeviceRGB" {
                                self.saw_rgb = true;
                            }
                        }
                        _ => {
                            self.gstate.spot_space = None;
                        }
                    }
                }
            }
            "setcolor" | "sc" | "scn" => {
                if let Some((spot_name, cmyk)) = self.gstate.spot_space.clone() {
                    if let Some(tint) = self.pop_f64() {
                        let c = Color::spot(spot_name, tint as f32, cmyk);
                        self.gstate.fill_color = c.clone();
                        self.gstate.stroke_color = c;
                    }
                } else if let Some(v) = self.pop_f64() {
                    let c = Color::gray(v as f32);
                    self.gstate.fill_color = c.clone();
                    self.gstate.stroke_color = c;
                }
            }
            "setoverprint" => {
                if let Some(PsVal::Bool(b)) = self.pop() {
                    self.gstate.overprint = b;
                }
            }

            // Stroke styling
            "setlinewidth" | "w" => {
                if let Some(w) = self.pop_f64() {
                    self.gstate.line_width_pt = w.max(0.0);
                }
            }
            "setlinecap" | "J" => {
                if let Some(c) = self.pop_f64() {
                    self.gstate.line_cap = match c.round() as i32 {
                        1 => LineCap::Round,
                        2 => LineCap::Square,
                        _ => LineCap::Butt,
                    };
                }
            }
            "setlinejoin" | "j" => {
                if let Some(j) = self.pop_f64() {
                    self.gstate.line_join = match j.round() as i32 {
                        1 => LineJoin::Round,
                        2 => LineJoin::Bevel,
                        _ => LineJoin::Miter,
                    };
                }
            }
            "setmiterlimit" | "M" => {
                if let Some(m) = self.pop_f64() {
                    self.gstate.miter_limit = m.max(1.0);
                }
            }
            "setdash" | "d" => {
                let _phase = self.pop_f64();
                if let Some(PsVal::Array(arr)) = self.pop() {
                    self.gstate.dash_pattern_pt = arr
                        .iter()
                        .filter_map(PsVal::as_f64)
                        .filter(|v| *v > 0.0)
                        .collect();
                }
            }

            // Path construction
            "newpath" | "n" | "N" => self.clear_path(),
            "moveto" | "m" => {
                if let (Some(y), Some(x)) = (self.pop_f64(), self.pop_f64()) {
                    self.check_current_path_vertices(1)?;
                    let p = self.gstate.ctm.transform_point(Point2D::new(x, y));
                    self.path.move_to(p.to_kurbo());
                    self.cur_pt = Some(p);
                    self.subpath_start = Some(p);
                }
            }
            "rmoveto" => {
                if let (Some(dy), Some(dx)) = (self.pop_f64(), self.pop_f64()) {
                    self.check_current_path_vertices(1)?;
                    let base = self.cur_pt.unwrap_or(Point2D::ZERO);
                    let delta = self
                        .gstate
                        .ctm
                        .transform_vec(coree_core::Vec2D::new(dx, dy));
                    let p = base + delta;
                    self.path.move_to(p.to_kurbo());
                    self.cur_pt = Some(p);
                    self.subpath_start = Some(p);
                }
            }
            "lineto" | "l" | "L" => {
                if let (Some(y), Some(x)) = (self.pop_f64(), self.pop_f64()) {
                    self.check_current_path_vertices(1)?;
                    let p = self.gstate.ctm.transform_point(Point2D::new(x, y));
                    if self.cur_pt.is_none() {
                        self.path.move_to(p.to_kurbo());
                        self.subpath_start = Some(p);
                    } else {
                        self.path.line_to(p.to_kurbo());
                    }
                    self.cur_pt = Some(p);
                }
            }
            "rlineto" => {
                if let (Some(dy), Some(dx)) = (self.pop_f64(), self.pop_f64()) {
                    self.check_current_path_vertices(1)?;
                    let base = self.cur_pt.unwrap_or(Point2D::ZERO);
                    let delta = self
                        .gstate
                        .ctm
                        .transform_vec(coree_core::Vec2D::new(dx, dy));
                    let p = base + delta;
                    if self.cur_pt.is_none() {
                        self.path.move_to(p.to_kurbo());
                        self.subpath_start = Some(p);
                    } else {
                        self.path.line_to(p.to_kurbo());
                    }
                    self.cur_pt = Some(p);
                }
            }
            "curveto" | "c" | "C" => {
                if let (Some(y3), Some(x3), Some(y2), Some(x2), Some(y1), Some(x1)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.check_current_path_vertices(2)?;
                    let p1 = self.gstate.ctm.transform_point(Point2D::new(x1, y1));
                    let p2 = self.gstate.ctm.transform_point(Point2D::new(x2, y2));
                    let p3 = self.gstate.ctm.transform_point(Point2D::new(x3, y3));
                    if self.cur_pt.is_none() {
                        self.path.move_to(p1.to_kurbo());
                        self.subpath_start = Some(p1);
                    }
                    self.path
                        .curve_to(p1.to_kurbo(), p2.to_kurbo(), p3.to_kurbo());
                    self.cur_pt = Some(p3);
                }
            }
            "rcurveto" => {
                if let (Some(dy3), Some(dx3), Some(dy2), Some(dx2), Some(dy1), Some(dx1)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.check_current_path_vertices(2)?;
                    let base = self.cur_pt.unwrap_or(Point2D::ZERO);
                    let p1 = base
                        + self
                            .gstate
                            .ctm
                            .transform_vec(coree_core::Vec2D::new(dx1, dy1));
                    let p2 = base
                        + self
                            .gstate
                            .ctm
                            .transform_vec(coree_core::Vec2D::new(dx2, dy2));
                    let p3 = base
                        + self
                            .gstate
                            .ctm
                            .transform_vec(coree_core::Vec2D::new(dx3, dy3));
                    if self.cur_pt.is_none() {
                        self.path.move_to(base.to_kurbo());
                        self.subpath_start = Some(base);
                    }
                    self.path
                        .curve_to(p1.to_kurbo(), p2.to_kurbo(), p3.to_kurbo());
                    self.cur_pt = Some(p3);
                }
            }
            "v" | "V" => {
                if let (Some(y3), Some(x3), Some(y2), Some(x2)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.check_current_path_vertices(1)?;
                    let p1 = self.cur_pt.unwrap_or(Point2D::ZERO);
                    let p2 = self.gstate.ctm.transform_point(Point2D::new(x2, y2));
                    let p3 = self.gstate.ctm.transform_point(Point2D::new(x3, y3));
                    self.path
                        .curve_to(p1.to_kurbo(), p2.to_kurbo(), p3.to_kurbo());
                    self.cur_pt = Some(p3);
                }
            }
            "y" | "Y" => {
                if let (Some(y3), Some(x3), Some(y1), Some(x1)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.check_current_path_vertices(1)?;
                    let p1 = self.gstate.ctm.transform_point(Point2D::new(x1, y1));
                    let p3 = self.gstate.ctm.transform_point(Point2D::new(x3, y3));
                    self.path
                        .curve_to(p1.to_kurbo(), p3.to_kurbo(), p3.to_kurbo());
                    self.cur_pt = Some(p3);
                }
            }
            "arc" => {
                if let (Some(a2), Some(a1), Some(r), Some(yc), Some(xc)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.append_ps_arc(xc, yc, r, a1, a2, false)?;
                }
            }
            "arcn" => {
                if let (Some(a2), Some(a1), Some(r), Some(yc), Some(xc)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.append_ps_arc(xc, yc, r, a1, a2, true)?;
                }
            }
            "closepath" => {
                if self.cur_pt.is_some() {
                    self.path.close_path();
                    self.cur_pt = self.subpath_start;
                }
            }
            "rectfill" => {
                if let (Some(h), Some(w), Some(y), Some(x)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    let rp = self.build_rect_path(x, y, w, h);
                    self.push_emitted(EpsEmitted::Path {
                        eps_path: rp,
                        fill: Fill::Solid(self.gstate.fill_color.clone()),
                        outline: OutlineStyle::none(),
                    })?;
                }
            }
            "rectstroke" => {
                if let (Some(h), Some(w), Some(y), Some(x)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    let rp = self.build_rect_path(x, y, w, h);
                    let outline = self.current_outline();
                    self.push_emitted(EpsEmitted::Path {
                        eps_path: rp,
                        fill: Fill::None,
                        outline,
                    })?;
                }
            }
            "rectclip" => {
                if let (Some(h), Some(w), Some(y), Some(x)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    let rp = self.build_rect_path(x, y, w, h);
                    if !rp.is_empty() {
                        self.gstate.clip_count += 1;
                        self.push_emitted(EpsEmitted::ClipPush { eps_path: rp })?;
                    }
                }
            }
            "clip" | "eoclip" => {
                if !self.path.is_empty() {
                    self.gstate.clip_count += 1;
                    self.push_emitted(EpsEmitted::ClipPush {
                        eps_path: self.path.clone(),
                    })?;
                }
            }

            // Painting
            "fill" | "eofill" => self.emit_current_path(true, false, false)?,
            "f" => self.emit_current_path(true, false, true)?,
            "F" => self.emit_current_path(true, false, false)?,
            "stroke" | "S" => self.emit_current_path(false, true, false)?,
            "s" => self.emit_current_path(false, true, true)?,
            "b" => self.emit_current_path(true, true, true)?,
            "B" => self.emit_current_path(true, true, false)?,

            // Fonts & Text
            "findfont" => {
                if let Some(PsVal::Name(fname)) = self.pop() {
                    let (family, bold, italic) = parse_ps_font_name(&fname);
                    self.push(PsVal::Font {
                        family,
                        size_pt: 1.0,
                        bold,
                        italic,
                    })?;
                }
            }
            "scalefont" => {
                if let (
                    Some(scale),
                    Some(PsVal::Font {
                        family,
                        size_pt,
                        bold,
                        italic,
                    }),
                ) = (self.pop_f64(), self.pop())
                {
                    self.push(PsVal::Font {
                        family,
                        size_pt: (size_pt * scale.abs()).max(1.0),
                        bold,
                        italic,
                    })?;
                }
            }
            "makefont" => {
                if let (
                    Some(PsVal::Array(mat)),
                    Some(PsVal::Font {
                        family,
                        size_pt,
                        bold,
                        italic,
                    }),
                ) = (self.pop(), self.pop())
                {
                    let sy = mat
                        .get(3)
                        .and_then(PsVal::as_f64)
                        .map(f64::abs)
                        .unwrap_or(1.0);
                    self.push(PsVal::Font {
                        family,
                        size_pt: (size_pt * sy).max(1.0),
                        bold,
                        italic,
                    })?;
                }
            }
            "setfont" => {
                if let Some(PsVal::Font {
                    family,
                    size_pt,
                    bold,
                    italic,
                }) = self.pop()
                {
                    self.gstate.font_family = family;
                    self.gstate.font_size_pt = size_pt.max(1.0);
                    self.gstate.font_bold = bold;
                    self.gstate.font_italic = italic;
                }
            }
            "selectfont" => {
                if let (Some(scale), Some(PsVal::Name(fname))) = (self.pop_f64(), self.pop()) {
                    let (family, bold, italic) = parse_ps_font_name(&fname);
                    self.gstate.font_family = family;
                    self.gstate.font_size_pt = scale.abs().max(1.0);
                    self.gstate.font_bold = bold;
                    self.gstate.font_italic = italic;
                }
            }
            "show" => {
                if let Some(PsVal::Str(raw)) = self.pop() {
                    self.emit_text(&raw)?;
                }
            }
            "ashow" => {
                if let (Some(PsVal::Str(raw)), Some(_ay), Some(_ax)) =
                    (self.pop(), self.pop_f64(), self.pop_f64())
                {
                    self.emit_text(&raw)?;
                }
            }
            "widthshow" => {
                if let (Some(PsVal::Str(raw)), Some(_ch), Some(_cy), Some(_cx)) =
                    (self.pop(), self.pop_f64(), self.pop_f64(), self.pop_f64())
                {
                    self.emit_text(&raw)?;
                }
            }
            "kshow" => {
                if let (Some(PsVal::Str(raw)), Some(_proc)) = (self.pop(), self.pop()) {
                    self.emit_text(&raw)?;
                }
            }
            "shfill" => {
                if let Some(PsVal::Dict(entries)) = self.pop() {
                    if let Some((eps_path, fill)) = parse_ps_shfill_dict(&entries, &self.gstate.ctm)
                    {
                        self.push_emitted(EpsEmitted::Path {
                            eps_path,
                            fill,
                            outline: OutlineStyle::none(),
                        })?;
                    }
                }
            }
            "image" => {
                let top = self.pop();
                let (raw_opt, dict_opt) = match top {
                    Some(PsVal::Str(raw)) => (Some(raw), self.pop()),
                    other => (None, other),
                };
                if let Some(PsVal::Dict(entries)) = dict_opt {
                    fn find_val<'a>(list: &'a [(String, PsVal)], key: &str) -> Option<&'a PsVal> {
                        list.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
                    }
                    let img_type = find_val(&entries, "ImageType")
                        .and_then(PsVal::as_f64)
                        .unwrap_or(1.0)
                        .round() as i32;
                    if img_type == 3 {
                        if let (Some(PsVal::Dict(mask_entries)), Some(PsVal::Dict(data_entries))) = (
                            find_val(&entries, "MaskDict"),
                            find_val(&entries, "DataDict"),
                        ) {
                            let w = find_val(data_entries, "Width")
                                .and_then(PsVal::as_f64)
                                .unwrap_or(0.0)
                                .round() as u32;
                            let h = find_val(data_entries, "Height")
                                .and_then(PsVal::as_f64)
                                .unwrap_or(0.0)
                                .round() as u32;
                            let mask_raw = match find_val(mask_entries, "DataSource") {
                                Some(PsVal::Str(b)) => Some(b.clone()),
                                _ => raw_opt.clone(),
                            };
                            let data_raw = match find_val(data_entries, "DataSource") {
                                Some(PsVal::Str(b)) => Some(b.clone()),
                                _ => None,
                            };
                            let mask_decode0 = find_val(mask_entries, "Decode")
                                .and_then(|v| match v {
                                    PsVal::Array(a) => a.first().and_then(PsVal::as_f64),
                                    _ => None,
                                })
                                .unwrap_or(0.0);
                            if w > 0 && h > 0 && w <= 4096 && h <= 4096 {
                                if let (Some(mask_b), Some(data_b)) = (mask_raw, data_raw) {
                                    let px_cnt = (w as usize).saturating_mul(h as usize);
                                    let decode_len = find_val(data_entries, "Decode")
                                        .and_then(|v| match v {
                                            PsVal::Array(a) => Some(a.len()),
                                            _ => None,
                                        })
                                        .unwrap_or(6);
                                    let is_cmyk = decode_len >= 8 || data_b.len() == px_cnt * 4;
                                    let stride = if is_cmyk { 4 } else { 3 };
                                    if data_b.len() >= px_cnt * stride {
                                        let row_bytes = (w as usize).div_ceil(8);
                                        let mut rgba = Vec::with_capacity(px_cnt * 4);
                                        let paint_bit = if mask_decode0 > 0.5 { 1u8 } else { 0u8 };
                                        for y in 0..(h as usize) {
                                            for x in 0..(w as usize) {
                                                let i = y * (w as usize) + x;
                                                let base = i * stride;
                                                let m_byte = mask_b
                                                    .get(y * row_bytes + x / 8)
                                                    .copied()
                                                    .unwrap_or(0xFF);
                                                let bit = (m_byte >> (7 - (x % 8))) & 1;
                                                let alpha =
                                                    if bit == paint_bit { 255u8 } else { 0u8 };
                                                if is_cmyk {
                                                    let c = f32::from(data_b[base]) / 255.0;
                                                    let m = f32::from(data_b[base + 1]) / 255.0;
                                                    let yy = f32::from(data_b[base + 2]) / 255.0;
                                                    let k = f32::from(data_b[base + 3]) / 255.0;
                                                    let mut px =
                                                        Color::cmyk(c, m, yy, k).to_rgba8();
                                                    px[3] = alpha;
                                                    rgba.extend_from_slice(&px);
                                                } else {
                                                    rgba.extend_from_slice(&[
                                                        data_b[base],
                                                        data_b[base + 1],
                                                        data_b[base + 2],
                                                        alpha,
                                                    ]);
                                                }
                                            }
                                        }
                                        self.push_emitted(EpsEmitted::Raster {
                                            width: w,
                                            height: h,
                                            rgba_bytes: rgba,
                                            ctm: self.gstate.ctm,
                                        })?;
                                    }
                                }
                            }
                        }
                    } else if let Some(raw) = raw_opt {
                        let find_num = |key: &str| {
                            entries
                                .iter()
                                .rev()
                                .find(|(k, _)| k == key)
                                .and_then(|(_, v)| v.as_f64())
                        };
                        if let (Some(w_f), Some(h_f)) = (find_num("Width"), find_num("Height")) {
                            let w = (w_f.round() as u32).clamp(1, 4096);
                            let h = (h_f.round() as u32).clamp(1, 4096);
                            let px_cnt = (w as usize).saturating_mul(h as usize);
                            let decode_len = entries
                                .iter()
                                .rev()
                                .find(|(k, _)| k == "Decode")
                                .and_then(|(_, v)| match v {
                                    PsVal::Array(a) => Some(a.len()),
                                    _ => None,
                                })
                                .unwrap_or(6);
                            let is_cmyk = decode_len >= 8 || raw.len() == px_cnt * 4;
                            let stride = if is_cmyk { 4 } else { 3 };
                            if raw.len() >= px_cnt * stride {
                                let mut rgba = Vec::with_capacity(px_cnt * 4);
                                for i in 0..px_cnt {
                                    let base = i * stride;
                                    if is_cmyk {
                                        let c = f32::from(raw[base]) / 255.0;
                                        let m = f32::from(raw[base + 1]) / 255.0;
                                        let y = f32::from(raw[base + 2]) / 255.0;
                                        let k = f32::from(raw[base + 3]) / 255.0;
                                        let rgb = Color::cmyk(c, m, y, k).to_rgba8();
                                        rgba.extend_from_slice(&rgb);
                                    } else {
                                        rgba.extend_from_slice(&[
                                            raw[base],
                                            raw[base + 1],
                                            raw[base + 2],
                                            255,
                                        ]);
                                    }
                                }
                                self.push_emitted(EpsEmitted::Raster {
                                    width: w,
                                    height: h,
                                    rgba_bytes: rgba,
                                    ctm: self.gstate.ctm,
                                })?;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn clear_path(&mut self) {
        self.path = BezPath::new();
        self.cur_pt = None;
        self.subpath_start = None;
    }

    fn build_rect_path(&self, x: f64, y: f64, w: f64, h: f64) -> BezPath {
        let p0 = self.gstate.ctm.transform_point(Point2D::new(x, y));
        let p1 = self.gstate.ctm.transform_point(Point2D::new(x + w, y));
        let p2 = self.gstate.ctm.transform_point(Point2D::new(x + w, y + h));
        let p3 = self.gstate.ctm.transform_point(Point2D::new(x, y + h));
        let mut p = BezPath::new();
        p.move_to(p0.to_kurbo());
        p.line_to(p1.to_kurbo());
        p.line_to(p2.to_kurbo());
        p.line_to(p3.to_kurbo());
        p.close_path();
        p
    }

    fn append_ps_arc(
        &mut self,
        xc: f64,
        yc: f64,
        r: f64,
        ang1_deg: f64,
        ang2_deg: f64,
        cw: bool,
    ) -> IoResult<()> {
        if !(xc.is_finite()
            && yc.is_finite()
            && r.is_finite()
            && ang1_deg.is_finite()
            && ang2_deg.is_finite())
            || r <= 0.0
        {
            return Ok(());
        }
        // O(1) modular angle normalization (never loops on extreme angles like 1e20)
        let mut a1 = ang1_deg.rem_euclid(360.0).to_radians();
        let total_sweep = if cw {
            let mut delta_cw = (ang1_deg - ang2_deg).rem_euclid(360.0);
            if delta_cw <= 1e-9 && (ang1_deg - ang2_deg).abs() >= 1e-9 {
                delta_cw = 360.0;
            }
            -delta_cw.to_radians()
        } else {
            let mut delta_ccw = (ang2_deg - ang1_deg).rem_euclid(360.0);
            if delta_ccw <= 1e-9 && (ang2_deg - ang1_deg).abs() >= 1e-9 {
                delta_ccw = 360.0;
            }
            delta_ccw.to_radians()
        };

        let segs = ((total_sweep.abs() / std::f64::consts::FRAC_PI_2).ceil() as usize).clamp(1, 32);
        self.check_current_path_vertices(1 + segs)?;

        let start_u = Point2D::new(xc + r * a1.cos(), yc + r * a1.sin());
        let start_d = self.gstate.ctm.transform_point(start_u);
        if self.cur_pt.is_none() {
            self.path.move_to(start_d.to_kurbo());
            self.subpath_start = Some(start_d);
        } else {
            self.path.line_to(start_d.to_kurbo());
        }
        self.cur_pt = Some(start_d);

        let step = total_sweep / (segs as f64);
        let kappa = (4.0 / 3.0) * (step / 4.0).tan();

        for _ in 0..segs {
            let next_a = a1 + step;
            let (s0, c0) = a1.sin_cos();
            let (s1, c1) = next_a.sin_cos();

            let u1 = Point2D::new(xc + r * (c0 - kappa * s0), yc + r * (s0 + kappa * c0));
            let u2 = Point2D::new(xc + r * (c1 + kappa * s1), yc + r * (s1 - kappa * c1));
            let u3 = Point2D::new(xc + r * c1, yc + r * s1);

            let d1 = self.gstate.ctm.transform_point(u1);
            let d2 = self.gstate.ctm.transform_point(u2);
            let d3 = self.gstate.ctm.transform_point(u3);
            self.path
                .curve_to(d1.to_kurbo(), d2.to_kurbo(), d3.to_kurbo());
            self.cur_pt = Some(d3);
            a1 = next_a;
        }
        Ok(())
    }

    fn current_outline(&self) -> OutlineStyle {
        let scale = effective_affine_scale(&self.gstate.ctm);
        let width_mm = (self.gstate.line_width_pt * scale * PT_TO_MM).max(0.05);
        let mut out = OutlineStyle::solid(self.gstate.stroke_color.clone(), width_mm);
        out.cap = self.gstate.line_cap;
        out.join = self.gstate.line_join;
        out.miter_limit = self.gstate.miter_limit;
        out.overprint = self.gstate.overprint;
        out.dash_pattern = self
            .gstate
            .dash_pattern_pt
            .iter()
            .map(|d| d * scale * PT_TO_MM)
            .filter(|d| d.is_finite() && *d > 0.0)
            .collect();
        out
    }

    fn emit_current_path(&mut self, do_fill: bool, do_stroke: bool, close: bool) -> IoResult<()> {
        if close && self.cur_pt.is_some() {
            self.path.close_path();
        }
        let path = std::mem::take(&mut self.path);
        self.cur_pt = None;
        self.subpath_start = None;
        if path.is_empty() {
            return Ok(());
        }
        let fill = if do_fill {
            Fill::Solid(self.gstate.fill_color.clone())
        } else {
            Fill::None
        };
        let outline = if do_stroke {
            self.current_outline()
        } else {
            OutlineStyle::none()
        };
        self.push_emitted(EpsEmitted::Path {
            eps_path: path,
            fill,
            outline,
        })
    }

    fn emit_text(&mut self, raw: &[u8]) -> IoResult<()> {
        let text: String = raw.iter().map(|&b| char::from(b)).collect();
        if text.is_empty() {
            return Ok(());
        }
        // Recover current point in user space so `ctm` + `origin_eps` compose cleanly
        let cur_dev = self.cur_pt.unwrap_or(Point2D::ZERO);
        let origin_user = self
            .gstate
            .ctm
            .inverse()
            .map_or(cur_dev, |inv| inv.transform_point(cur_dev));
        self.push_emitted(EpsEmitted::Text {
            text,
            font_family: self.gstate.font_family.clone(),
            font_size_pt: self.gstate.font_size_pt,
            bold: self.gstate.font_bold,
            italic: self.gstate.font_italic,
            color: self.gstate.fill_color.clone(),
            ctm: self.gstate.ctm,
            origin_eps: origin_user,
        })
    }
}

fn parse_ps_font_name(raw: &str) -> (String, bool, bool) {
    let lower = raw.to_ascii_lowercase();
    let bold = lower.contains("bold") || lower.contains("black") || lower.contains("heavy");
    let italic = lower.contains("italic") || lower.contains("oblique");
    let family = raw.split('-').next().unwrap_or(raw).trim();
    let clean = if family.is_empty() {
        "Helvetica".to_owned()
    } else {
        family.to_owned()
    };
    (clean, bold, italic)
}

fn effective_affine_scale(xf: &Affine2D) -> f64 {
    let sx = xf.m[0].hypot(xf.m[1]);
    let sy = xf.m[2].hypot(xf.m[3]);
    let avg = 0.5 * (sx + sy);
    if avg.is_finite() && avg > 1e-6 {
        avg
    } else {
        1.0
    }
}

fn parse_ps_shfill_dict(entries: &[(String, PsVal)], ctm: &Affine2D) -> Option<(BezPath, Fill)> {
    let find = |k: &str| entries.iter().rev().find(|(ek, _)| ek == k).map(|(_, v)| v);
    let sh_type = find("ShadingType").and_then(PsVal::as_f64)?.round() as i32;
    if sh_type != 2 && sh_type != 3 {
        return None;
    }
    let is_cmyk = matches!(find("ColorSpace"), Some(PsVal::Name(n)) if n == "DeviceCMYK");
    let coords: Vec<f64> = match find("Coords") {
        Some(PsVal::Array(arr)) => arr.iter().filter_map(PsVal::as_f64).collect(),
        _ => return None,
    };
    let mut stops = Vec::new();
    if let Some(PsVal::Dict(fn_entries)) = find("Function") {
        let fn_find = |k: &str| {
            fn_entries
                .iter()
                .rev()
                .find(|(ek, _)| ek == k)
                .map(|(_, v)| v)
        };
        let parse_col = |val: &PsVal| -> Color {
            if let PsVal::Array(arr) = val {
                let nums: Vec<f32> = arr
                    .iter()
                    .filter_map(PsVal::as_f64)
                    .map(|v| v as f32)
                    .collect();
                if is_cmyk && nums.len() >= 4 {
                    return Color::cmyk(nums[0], nums[1], nums[2], nums[3]);
                } else if nums.len() >= 3 {
                    return Color::rgb(nums[0], nums[1], nums[2]);
                } else if !nums.is_empty() {
                    return Color::gray(nums[0]);
                }
            }
            Color::BLACK
        };
        let ftype = fn_find("FunctionType")
            .and_then(PsVal::as_f64)
            .unwrap_or(2.0)
            .round() as i32;
        if ftype == 2 {
            let c0 = fn_find("C0").map(parse_col).unwrap_or(Color::BLACK);
            let c1 = fn_find("C1").map(parse_col).unwrap_or(Color::WHITE);
            stops.push(GradientStop::new(0.0, c0));
            stops.push(GradientStop::new(1.0, c1));
        } else if ftype == 3 {
            let bounds: Vec<f64> = match fn_find("Bounds") {
                Some(PsVal::Array(a)) => a.iter().filter_map(PsVal::as_f64).collect(),
                _ => Vec::new(),
            };
            if let Some(PsVal::Array(subs)) = fn_find("Functions") {
                for (idx, sub) in subs.iter().enumerate() {
                    if let PsVal::Dict(sub_e) = sub {
                        let c0 = sub_e
                            .iter()
                            .rev()
                            .find(|(k, _)| k == "C0")
                            .map(|(_, v)| parse_col(v))
                            .unwrap_or(Color::BLACK);
                        let c1 = sub_e
                            .iter()
                            .rev()
                            .find(|(k, _)| k == "C1")
                            .map(|(_, v)| parse_col(v))
                            .unwrap_or(Color::WHITE);
                        let t0 = if idx == 0 {
                            0.0
                        } else {
                            bounds.get(idx - 1).copied().unwrap_or(0.0)
                        };
                        let t1 = if idx < bounds.len() { bounds[idx] } else { 1.0 };
                        if stops.is_empty() {
                            stops.push(GradientStop::new(t0 as f32, c0));
                        }
                        stops.push(GradientStop::new(t1 as f32, c1));
                    }
                }
            }
        }
    }
    if stops.is_empty() {
        return None;
    }
    let fill = if sh_type == 2 && coords.len() >= 4 {
        let p0 = ctm.transform_point(Point2D::new(coords[0], coords[1]));
        let p1 = ctm.transform_point(Point2D::new(coords[2], coords[3]));
        Fill::LinearGradient {
            start: p0,
            end: p1,
            stops,
        }
    } else if sh_type == 3 && coords.len() >= 6 {
        let f = ctm.transform_point(Point2D::new(coords[0], coords[1]));
        let c = ctm.transform_point(Point2D::new(coords[3], coords[4]));
        Fill::RadialGradient {
            center: c,
            radius: coords[5].abs().max(0.5),
            focal: f,
            stops,
        }
    } else {
        return None;
    };
    let mut bp = BezPath::new();
    let (x0, y0, x1, y1) = if sh_type == 2 {
        (
            coords[0].min(coords[2]) - 100.0,
            coords[1].min(coords[3]) - 100.0,
            coords[0].max(coords[2]) + 100.0,
            coords[1].max(coords[3]) + 100.0,
        )
    } else {
        (
            coords[3] - coords[5].abs(),
            coords[4] - coords[5].abs(),
            coords[3] + coords[5].abs(),
            coords[4] + coords[5].abs(),
        )
    };
    let p0 = ctm.transform_point(Point2D::new(x0, y0));
    let p1 = ctm.transform_point(Point2D::new(x1, y0));
    let p2 = ctm.transform_point(Point2D::new(x1, y1));
    let p3 = ctm.transform_point(Point2D::new(x0, y1));
    bp.move_to(p0.to_kurbo());
    bp.line_to(p1.to_kurbo());
    bp.line_to(p2.to_kurbo());
    bp.line_to(p3.to_kurbo());
    bp.close_path();
    Some((bp, fill))
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{CornerStyle, ParametricShape, Rect2D};

    #[test]
    fn eps_level_3_emits_valid_dsc_cmyk_rgb_and_curves() {
        let mut doc = Document::new_a4("EPS Vector Logo");
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Rounded Badge",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 60.0,
                    height: 30.0,
                    corner_radii: [5.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::MAGENTA))
            .with_outline(OutlineStyle::solid(Color::BLACK, 0.5)),
            None,
        )
        .unwrap();

        let eps = export_eps_to_string(&doc, 0).unwrap();
        assert!(eps.starts_with("%!PS-Adobe-3.0 EPSF-3.0\n"));
        assert!(eps.contains("%%BoundingBox: 0 0 "));
        assert!(eps.contains("%%LanguageLevel: 3"));
        assert!(eps.contains("moveto"));
        assert!(eps.contains("curveto"));
        assert!(eps.contains("setcmykcolor"));
        assert!(eps.contains("fill"));
        assert!(eps.contains("stroke"));
        assert!(eps.ends_with("%%EOF\n"));

        // Verify RGB mode uses setrgbcolor
        let mut rgb_doc = Document::new("RGB EPS", 50.0, 50.0, ColorMode::Rgb, 96.0);
        rgb_doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Line",
                    NodePayload::Path(BezierPath::from_line(
                        Point2D::new(0.0, 0.0),
                        Point2D::new(20.0, 20.0),
                    )),
                )
                .with_outline(OutlineStyle::solid(Color::rgb(0.2, 0.4, 0.8), 1.0)),
                None,
            )
            .unwrap();
        let rgb_eps = export_eps_to_string(&rgb_doc, 0).unwrap();
        assert!(rgb_eps.contains("setrgbcolor"));
        assert!(rgb_eps.contains("lineto"));
    }

    #[test]
    fn eps_roundtrip_export_then_import_preserves_dimensions_geometry_colors_and_text() {
        let mut doc = Document::new("Brand Card", 120.0, 80.0, ColorMode::Rgb, 300.0);
        let mut outline = OutlineStyle::solid(Color::rgb(0.1, 0.3, 0.9), 1.2);
        outline.cap = LineCap::Round;
        outline.join = LineJoin::Bevel;
        outline.dash_pattern = vec![3.0, 1.5];

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Card Box",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    15.0, 10.0, 60.0, 35.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::rgb(0.85, 0.25, 0.15)))
            .with_outline(outline),
            None,
        )
        .unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Heading",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "Studio (EPS) 2026",
                    TextStyle {
                        font_family: "Helvetica".to_owned(),
                        font_size_pt: 18.0,
                        bold: true,
                        fill: Color::rgb(0.05, 0.05, 0.05),
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(20.0, 52.0)),
            None,
        )
        .unwrap();

        let eps_str = export_eps_to_string(&doc, 0).unwrap();
        let imported = import_eps_bytes(eps_str.as_bytes(), "fallback").unwrap();

        assert_eq!(imported.title(), "Brand Card");
        assert!((imported.pages()[0].width_mm - 120.0).abs() < 0.05);
        assert!((imported.pages()[0].height_mm - 80.0).abs() < 0.05);

        // Expect filled rect path + stroked rect path + ArtisticText
        assert_eq!(imported.object_count(), 3);

        let mut saw_fill = false;
        let mut saw_stroke = false;
        let mut saw_text = false;
        for (&id, obj) in imported.objects() {
            match &obj.payload {
                NodePayload::Path(_) => {
                    let bb = imported.object_bounds(id).unwrap();
                    assert!((bb.min_x - 15.0).abs() < 0.05);
                    assert!((bb.min_y - 10.0).abs() < 0.05);
                    assert!((bb.width() - 60.0).abs() < 0.05);
                    assert!((bb.height() - 35.0).abs() < 0.05);
                    if let Fill::Solid(c) = &obj.fill {
                        let [r, g, b, _] = c.to_rgba_f32();
                        assert!((r - 0.85).abs() < 0.01);
                        assert!((g - 0.25).abs() < 0.01);
                        assert!((b - 0.15).abs() < 0.01);
                        saw_fill = true;
                    }
                    if obj.outline.is_visible() {
                        assert!((obj.outline.width_mm - 1.2).abs() < 0.05);
                        assert_eq!(obj.outline.cap, LineCap::Round);
                        assert_eq!(obj.outline.join, LineJoin::Bevel);
                        assert_eq!(obj.outline.dash_pattern.len(), 2);
                        saw_stroke = true;
                    }
                }
                NodePayload::ArtisticText(t) => {
                    assert_eq!(t.plain_text(), "Studio (EPS) 2026");
                    assert!(t.runs[0].style.bold);
                    assert!((t.runs[0].style.font_size_pt - 18.0).abs() < 0.1);
                    assert!((obj.transform.m[4] - 20.0).abs() < 0.1);
                    assert!((obj.transform.m[5] - 52.0).abs() < 0.1);
                    saw_text = true;
                }
                _ => {}
            }
        }
        assert!(saw_fill && saw_stroke && saw_text);
    }

    #[test]
    fn eps_dos_binary_wrapper_arcs_rectfill_and_illustrator_shorthand() {
        let ps = b"%!PS-Adobe-3.0 EPSF-3.0\n\
%%BoundingBox: 0 0 200 100\n\
gsave\n\
10 10 40 20 rectfill\n\
0 1 0 0 k\n\
0 0 0 1 K\n\
100 50 25 0 360 arc closepath B\n\
10 80 m 50 80 l S\n\
/Courier findfont 14 scalefont setfont 10 20 moveto (A\\050B\\051) show\n\
grestore\n\
%%EOF\n";

        // Wrap in a 30-byte DOS binary EPS header (C5 D0 D3 C6)
        let mut dos_eps = vec![0u8; 30];
        dos_eps[0..4].copy_from_slice(&[0xC5, 0xD0, 0xD3, 0xC6]);
        dos_eps[4..8].copy_from_slice(&30u32.to_le_bytes());
        dos_eps[8..12].copy_from_slice(&(ps.len() as u32).to_le_bytes());
        dos_eps.extend_from_slice(ps);
        // Trailing TIFF preview bytes should be ignored safely
        dos_eps.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00, 0xDE, 0xAD]);

        let doc = import_eps_bytes(&dos_eps, "DOS Binary").unwrap();
        assert_eq!(doc.color_mode(), ColorMode::Cmyk);
        assert_eq!(doc.object_count(), 4); // rectfill, arc B, line S, text show
        let text_obj = doc
            .objects()
            .values()
            .find_map(|o| match &o.payload {
                NodePayload::ArtisticText(t) => Some(t.plain_text()),
                _ => None,
            })
            .unwrap();
        assert_eq!(text_obj, "A(B)");
    }

    #[test]
    fn eps_adversarial_unmatched_paren_extreme_arc_empty_repeat_and_array_bomb() {
        // 1. Unmatched `)` must terminate immediately without infinite loop or OOM
        let doc = import_eps_bytes(b"%!PS\n)\n", "UnmatchedParen").unwrap();
        assert_eq!(doc.pages().len(), 1);

        // 2. Extreme finite arc angles (1e20) must finish in O(1) without hanging
        let extreme_arc =
            b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n50 50 20 1e20 0 arc stroke\n50 50 20 0 1e20 arcn stroke\n%%EOF\n";
        let arc_doc = import_eps_bytes(extreme_arc, "ExtremeArc").unwrap();
        assert_eq!(arc_doc.object_count(), 2);

        // 3. Nested empty repeat loop bomb must trip MAX_PS_STEPS immediately
        let empty_loop_bomb = b"%!PS\n10000 { 10000 { } repeat } repeat\n";
        let err1 = import_eps_bytes(empty_loop_bomb, "LoopBomb").unwrap_err();
        assert!(matches!(err1, IoError::LimitExceeded { .. }));

        // 4. Exponential array doubling bomb must trip MAX_PS_STACK_NODES without OOM
        let array_bomb = b"%!PS\n[1 2] 30 { dup [ exch dup ] } repeat\n";
        let err2 = import_eps_bytes(array_bomb, "ArrayBomb").unwrap_err();
        assert!(matches!(err2, IoError::LimitExceeded { .. }));
    }

    #[test]
    fn eps_arabic_curves_raster_image_roundtrip_and_shfill() {
        let mut doc = Document::new("EPS Full", 120.0, 90.0, ColorMode::Rgb, 300.0);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Arabic",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "كوري",
                    TextStyle {
                        font_size_pt: 24.0,
                        direction: TextDirection::Rtl,
                        ..TextStyle::default()
                    },
                )),
            )
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(20.0, 20.0)),
            None,
        )
        .unwrap();

        let asset = coree_core::RasterAsset::new(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
            ],
            300.0,
            ColorMode::Rgb,
        )
        .unwrap();
        let asset_id = doc.register_asset(asset);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Bitmap",
                NodePayload::Raster(coree_core::RasterLayerData {
                    asset_id,
                    width_mm: 20.0,
                    height_mm: 20.0,
                    filters: Vec::new(),
                    filter_enabled: Vec::new(),
                    crop_rect: None,
                }),
            )
            .with_transform(Affine2D::translate(50.0, 30.0)),
            None,
        )
        .unwrap();

        let eps = export_eps_to_string(&doc, 0).unwrap();
        assert!(eps.contains("curveto"));
        assert!(eps.contains("/ImageType 1"));
        assert!(eps.contains("FF0000"));

        let imported = import_eps_bytes(eps.as_bytes(), "EPS Full").unwrap();
        assert!(!imported.assets().is_empty());
        assert!(imported
            .objects()
            .values()
            .any(|o| matches!(o.payload, NodePayload::Raster(_))));
    }

    #[test]
    fn eps_imagetype3_alpha_transparency_gs_pngalpha_and_import_rem08() {
        let mut doc = Document::new("EPS Alpha", 100.0, 100.0, ColorMode::Rgb, 150.0);
        // Transparent RGBA raster + DropShadow shape + Spot CutContour
        let mut rgba = Vec::with_capacity(16 * 16 * 4);
        for y in 0..16u32 {
            for x in 0..16u32 {
                if (4..12).contains(&x) && (4..12).contains(&y) {
                    rgba.extend_from_slice(&[20, 80, 220, 180]);
                } else {
                    rgba.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        let asset = coree_core::RasterAsset::new(16, 16, rgba, 150.0, ColorMode::Rgb).unwrap();
        let aid = doc.register_asset(asset);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "AlphaImg",
                NodePayload::Raster(coree_core::RasterLayerData {
                    asset_id: aid,
                    width_mm: 40.0,
                    height_mm: 40.0,
                    filters: Vec::new(),
                    filter_enabled: Vec::new(),
                    crop_rect: None,
                }),
            )
            .with_transform(Affine2D::translate(30.0, 30.0)),
            None,
        )
        .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "SpotBadge",
                NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                    10.0, 10.0, 20.0, 15.0,
                ))),
            )
            .with_fill(Fill::LinearGradient {
                start: Point2D::new(10.0, 10.0),
                end: Point2D::new(30.0, 10.0),
                stops: vec![
                    GradientStop::new(0.0, Color::rgb(1.0, 0.0, 0.0)),
                    GradientStop::new(1.0, Color::rgb(0.0, 0.0, 1.0)),
                ],
            })
            .with_outline(OutlineStyle::cut_contour()),
            None,
        )
        .unwrap();

        let eps = export_eps_to_string(&doc, 0).unwrap();
        assert!(eps.contains("/ImageType 3"));
        assert!(eps.contains("/InterleaveType 3"));
        assert!(eps.contains("/Separation (CutContour)"));

        // Verify Ghostscript pngalpha renders transparent background with ZERO white knockout pixels!
        if std::path::Path::new("/usr/bin/gs").exists() {
            let dir = tempfile::tempdir().unwrap();
            let eps_path = dir.path().join("alpha.eps");
            let png_path = dir.path().join("alpha.png");
            std::fs::write(&eps_path, eps.as_bytes()).unwrap();
            let out = std::process::Command::new("/usr/bin/gs")
                .args([
                    "-dSAFER",
                    "-dEPSCrop",
                    "-dNOPAUSE",
                    "-dBATCH",
                    "-sDEVICE=pngalpha",
                    "-r96",
                    &format!("-sOutputFile={}", png_path.display()),
                ])
                .arg(&eps_path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "gs pngalpha failed: {}\n---EPS---\n{}",
                String::from_utf8_lossy(&out.stderr),
                eps
            );
            let img = image::open(&png_path).unwrap().to_rgba8();
            let mut opaque_white = 0usize;
            let mut near_white_opaque = 0usize;
            let mut transparent_px = 0usize;
            for p in img.pixels() {
                if p[3] == 0 {
                    transparent_px += 1;
                } else if p[3] >= 250 && p[0] >= 250 && p[1] >= 250 && p[2] >= 250 {
                    opaque_white += 1;
                } else if p[3] >= 245 && p[0] >= 245 && p[1] >= 245 && p[2] >= 245 {
                    near_white_opaque += 1;
                }
            }
            assert!(transparent_px > 100, "expected transparent EPS pixels");
            assert_eq!(
                opaque_white, 0,
                "opaque_white_pixels must be 0 in EPS pngalpha"
            );
            assert_eq!(
                near_white_opaque, 0,
                "near_white_opaque_pixels must be 0 in EPS pngalpha"
            );
        }

        // Verify importer roundtrips ImageType 3 + /Separation + shfill + clip + ASCII85
        let imported = import_eps_bytes(eps.as_bytes(), "EPS REM08").unwrap();
        assert!(!imported.assets().is_empty());
        assert!(imported
            .objects()
            .values()
            .any(|o| matches!(&o.outline.color, Color::Spot { name, .. } if name == "CutContour")));

        let ps_clip_a85 = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n\
            gsave 10 10 40 40 rectclip 1 0 0 setrgbcolor 0 0 80 80 rectfill grestore\n\
            (<~9jqo^~>) pop\n";
        let clip_doc = import_eps_bytes(ps_clip_a85, "EPS Clip A85").unwrap();
        assert!(clip_doc
            .objects()
            .values()
            .any(|o| matches!(o.payload, NodePayload::PowerClip { .. })));
    }

    #[test]
    fn eps_import_ascii85_inline_image_with_open_delimiter() {
        let eps = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n\
            gsave 10 10 translate 50 50 scale /DeviceRGB setcolorspace\n\
            << /ImageType 1 /Width 2 /Height 2 /BitsPerComponent 8 \
            /ImageMatrix [2 0 0 -2 0 2] /Decode [0 1 0 1 0 1] \
            /DataSource currentfile /ASCII85Decode filter >> image\n\
            <~rr<$!rr<$!s8W*!~>\n\
            grestore\nshowpage\n%%EOF\n";
        let doc = import_eps_bytes(eps.as_bytes(), "A85 Inline").unwrap();
        assert_eq!(doc.assets().len(), 1);
        assert_eq!(doc.objects().len(), 1);
        let asset = doc.assets().values().next().unwrap();
        assert_eq!((asset.width, asset.height), (2, 2));
        assert_eq!(
            asset.to_rgba8_vec(),
            vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,]
        );

        // Verify valid ASCII85 digit `'>'` and `'z'` inside `<~...~>` are decoded rather than truncating.
        let eps_gt = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n\
            gsave 10 10 translate 50 50 scale /DeviceRGB setcolorspace\n\
            << /ImageType 1 /Width 2 /Height 2 /BitsPerComponent 8 \
            /ImageMatrix [2 0 0 -2 0 2] /Decode [0 1 0 1 0 1] \
            /DataSource currentfile /ASCII85Decode filter >> image\n\
            <~>!!!!zz~>\n\
            grestore\nshowpage\n%%EOF\n";
        let doc_gt = import_eps_bytes(eps_gt.as_bytes(), "A85 GT").unwrap();
        assert_eq!(doc_gt.assets().len(), 1);
        let asset_gt = doc_gt.assets().values().next().unwrap();
        assert_eq!(
            asset_gt.to_rgba8_vec(),
            vec![0x5A, 0x3B, 0x08, 255, 0x0D, 0x00, 0x00, 255, 0, 0, 0, 255, 0, 0, 0, 255,]
        );
    }

    #[test]
    fn eps_imagetype3_large_300dpi_roundtrip_preserves_asset() {
        let mut doc = Document::new("EPS Large Alpha", 100.0, 100.0, ColorMode::Rgb, 300.0);
        let mut rect = SceneObject::new(
            ObjectId(0),
            "Transp50mm",
            NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                10.0, 10.0, 50.0, 50.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgb(0.2, 0.5, 0.8)))
        .with_outline(OutlineStyle::none());
        rect.effects.push(coree_core::Effect::Transparency {
            mode: coree_core::TransparencyMode::Uniform(0.5),
            blend_mode: coree_core::BlendMode::Normal,
        });
        doc.add_object(rect, None).unwrap();

        let eps = export_eps_to_string(&doc, 0).unwrap();
        assert!(eps.contains("/ImageType 3"));
        assert!(eps.contains("/coree_data"));
        // Verify /coree_data binary stream exceeds the old 640 KB (10_000 * 64) def cutoff.
        assert!(eps.len() > 1_280_000);

        let imported = import_eps_bytes(eps.as_bytes(), "EPS Large Roundtrip").unwrap();
        assert_eq!(imported.assets().len(), 1);
        assert_eq!(imported.objects().len(), 1);
    }

    #[test]
    fn eps_import_ascii85_overflow_and_z_flood_adversarial() {
        // 1. Exact u32::MAX (0xFFFF_FFFF) valid boundary: <~s8W-!~>
        let mut pos = 0;
        let max_valid = read_ps_inline_ascii85_stream(b"<~s8W-!~>", &mut pos).unwrap();
        assert_eq!(max_valid, vec![0xFF, 0xFF, 0xFF, 0xFF]);

        // Round-trip <~s8W-!~> through an EPS CMYK 1x1 image (4 bytes = 0xFF_FF_FF_FF)
        let eps_max_valid = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n\
            gsave /DeviceCMYK setcolorspace \
            << /ImageType 1 /Width 1 /Height 1 /BitsPerComponent 8 \
            /ImageMatrix [1 0 0 -1 0 1] /Decode [0 1 0 1 0 1 0 1] \
            /DataSource currentfile /ASCII85Decode filter >> image\n\
            <~s8W-!~>\ngrestore\n%%EOF\n";
        let max_doc = import_eps_bytes(eps_max_valid.as_bytes(), "A85 u32::MAX").unwrap();
        assert_eq!(max_doc.assets().len(), 1);

        // 2. Overflow > u32::MAX cases: <~s8W-"~> (u32::MAX + 1), <~uuuuu~>, <~uuuu~>, <~t!~>,
        //    plus invalid 1-char tail <~u~> and mid-tuple 'z' <~az~>.
        for payload in [
            "<~s8W-\"~>",
            "<~uuuuu~>",
            "<~uuuu~>",
            "<~t!~>",
            "<~u~>",
            "<~az~>",
        ] {
            let mut p = 0;
            let direct = read_ps_inline_ascii85_stream(payload.as_bytes(), &mut p);
            assert!(
                matches!(direct, Err(IoError::InvalidFormat { .. })),
                "expected InvalidFormat for {payload}, got {direct:?}"
            );

            let eps = format!(
                "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n{payload} pop\n0 0 10 10 rectfill\n%%EOF\n"
            );
            let res = import_eps_bytes(eps.as_bytes(), "A85 Overflow");
            assert!(
                matches!(res, Err(IoError::InvalidFormat { .. })),
                "expected clean InvalidFormat error for {payload}, got {res:?}"
            );
        }

        // 3. 'z'-flood expansion capped cleanly at max_bytes without panic/OOM
        let mut p = 0;
        let z_small = format!("<~{}~>", "z".repeat(300));
        let capped = read_ps_inline_ascii85_stream_with_limit(z_small.as_bytes(), &mut p, 1024);
        assert!(
            matches!(capped, Err(IoError::LimitExceeded { .. })),
            "expected LimitExceeded for 'z'-flood, got {capped:?}"
        );

        let mut z_flood = Vec::with_capacity(MAX_PS_STREAM_BYTES / 4 + 128);
        z_flood.extend_from_slice(b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n<~");
        z_flood.resize(z_flood.len() + MAX_PS_STREAM_BYTES / 4 + 16, b'z');
        z_flood.extend_from_slice(b"~> pop\n%%EOF\n");
        let flood_res = import_eps_bytes(&z_flood, "A85 Z-Flood");
        assert!(
            matches!(flood_res, Err(IoError::LimitExceeded { .. })),
            "expected LimitExceeded for full 'z'-flood, got {flood_res:?}"
        );
    }

    #[test]
    fn tier5_eps_export_applies_opacity_to_text_runs_and_pattern_fills() {
        let mut doc = Document::new("EPS Opacity", 100.0, 100.0, ColorMode::Rgb, 300.0);
        let mut txt = SceneObject::new(
            ObjectId(0),
            "AlphaText",
            NodePayload::ArtisticText(coree_core::ArtisticText::from_plain(
                "TEST",
                coree_core::TextStyle {
                    font_size_pt: 18.0,
                    fill: Color::BLACK,
                    ..coree_core::TextStyle::default()
                },
            )),
        )
        .with_transform(Affine2D::translate(10.0, 25.0));
        txt.opacity = 0.2;
        doc.add_object(txt, None).unwrap();

        let mut pat = SceneObject::new(
            ObjectId(0),
            "AlphaPattern",
            NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                10.0, 40.0, 30.0, 30.0,
            ))),
        )
        .with_fill(Fill::TwoColorPattern {
            fg: Color::BLACK,
            bg: Color::WHITE,
            pattern_name: "Checker".to_owned(),
            tile_w: 5.0,
            tile_h: 5.0,
        })
        .with_outline(OutlineStyle::none());
        pat.opacity = 0.2;
        doc.add_object(pat, None).unwrap();

        let eps = export_eps_to_string(&doc, 0).unwrap();
        assert!(
            eps.contains("0.8000 0.8000 0.8000 setrgbcolor"),
            "EPS must blend 0.2-opacity black text and TwoColorPattern fg against paper white:\n{eps}"
        );
        assert!(
            !eps.contains("0.0000 0.0000 0.0000 setrgbcolor"),
            "EPS must NOT emit full 0.0000 opaque black when opacity=0.2:\n{eps}"
        );
    }

    #[test]
    fn eps_repeat_object_bomb_rejects_cleanly() {
        let bomb = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n30 { 10000 { 0 0 10 10 rectfill } repeat } repeat\n%%EOF\n";
        let err = import_eps_bytes(bomb, "EpsObjBomb").unwrap_err();
        assert!(
            matches!(err, IoError::LimitExceeded { .. }),
            "expected LimitExceeded on 300k rectfill repeat bomb, got {err:?}"
        );
    }

    #[test]
    fn eps_clip_and_arc_vertex_amplification_rejects_cleanly() {
        let clip_bomb = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n0 0 moveto 10000 { 1 0 lineto } repeat 500 { clip } repeat\n%%EOF\n";
        let err = import_eps_bytes(clip_bomb, "EpsClipVertexBomb").unwrap_err();
        assert!(
            matches!(err, IoError::LimitExceeded { ref limit, .. } if limit.contains("max_ps_vertices")),
            "expected LimitExceeded(max_ps_vertices) on repeated clip bomb, got {err:?}"
        );

        let arc_bomb = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n/a { 0 0 10 0 360 arc } def 100 { 10000 { a } repeat } repeat fill\n%%EOF\n";
        let err2 = import_eps_bytes(arc_bomb, "EpsArcVertexBomb").unwrap_err();
        assert!(
            matches!(err2, IoError::LimitExceeded { .. }),
            "expected LimitExceeded on repeated arc bomb, got {err2:?}"
        );
    }
}
