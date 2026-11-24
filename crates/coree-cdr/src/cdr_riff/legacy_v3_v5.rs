// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room `.cdr` v3–v5 (`CDR3`–`CDR5`) 16-bit coordinate (`0.001 in`) and
//! legacy chunk decoders (`mcfg`, `loda`, `fild`, `outl`, `txsm`).

use super::loda::{
    decode_curve_points, ParsedLoda, ParsedObjProps, CDR_CTYPE_ARTISTIC_TEXT, CDR_CTYPE_CURVE,
    CDR_CTYPE_ELLIPSE, CDR_CTYPE_PARAGRAPH_TEXT, CDR_CTYPE_RECTANGLE,
};
use crate::coord::{decode_cdr_color, decode_cdr_text, derive_spot_fallback_cmyk};
use crate::error::CdrResult;
use crate::limits::ByteReader;
use coree_core::{
    math::MM_PER_INCH, ArtisticText, BlendMode, Color, ColorMode, CornerStyle, EllipseMode, Fill,
    LineCap, LineJoin, NodePayload, OutlineStyle, ParagraphText, ParametricShape, Point2D,
    TextStyle, Vec2D,
};

/// Converts a signed 16-bit CDR v3–v5 coordinate (`0.001 inch`) into millimeters (`mm`).
#[inline]
#[must_use]
pub fn cdr16_to_mm(units: i16) -> f64 {
    f64::from(units) * (MM_PER_INCH / 1_000.0)
}

/// Converts millimeters (`mm`) into a clamped signed 16-bit CDR v3–v5 coordinate (`0.001 inch`).
#[inline]
#[must_use]
pub fn mm_to_cdr16(mm: f64) -> i16 {
    let scaled = (mm / (MM_PER_INCH / 1_000.0)).round();
    scaled.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

/// Returns `true` when the RIFF form/version belongs to CDR v3–v5 (`300..=599`).
#[must_use]
pub fn is_legacy_v3_v5(version: u16, form_type: &[u8; 4]) -> bool {
    (300..600).contains(&version)
        || matches!(form_type, b"CDR3" | b"CDR4" | b"CDR5" | b"CDT4" | b"CDT5")
}

/// Parses a 16-bit CDR v3–v5 document `mcfg` chunk (`(w: i16, h: i16)` in `0.001 in`).
pub(crate) fn parse_v3_v5_doc_mcfg(data: &[u8]) -> Option<(f64, f64, ColorMode, f32, String)> {
    if data.len() < 4 || data.len() >= 14 {
        return None;
    }
    let mut r = ByteReader::new(data);
    let w = cdr16_to_mm(r.read_i16_le("v3-5 doc w").ok()?).abs();
    let h = cdr16_to_mm(r.read_i16_le("v3-5 doc h").ok()?).abs();
    if !(1.0..=50_000.0).contains(&w) || !(1.0..=50_000.0).contains(&h) {
        return None;
    }
    let mode = match r.read_u8("v3-5 color_mode").unwrap_or(0) {
        1 => ColorMode::Rgb,
        _ => ColorMode::Cmyk,
    };
    let dpi = f32::from(r.read_u16_le("v3-5 dpi").unwrap_or(300)).max(1.0);
    let title = if r.remaining() >= 1 {
        let len = usize::from(r.read_u8("v3-5 title_len").unwrap_or(0)).min(r.remaining());
        decode_cdr_text(r.read_bytes(len, "v3-5 title").unwrap_or(&[]), None, None)
    } else {
        String::new()
    };
    Some((w, h, mode, dpi, title))
}

/// Parses a 16-bit CDR v3–v5 page `mcfg` chunk (`(w: i16, h: i16)` in `0.001 in`).
pub(crate) fn parse_v3_v5_page_mcfg(data: &[u8]) -> Option<(f64, f64, f64, String)> {
    if data.len() < 4 || data.len() >= 14 {
        return None;
    }
    let mut r = ByteReader::new(data);
    let w = cdr16_to_mm(r.read_i16_le("v3-5 page w").ok()?).abs();
    let h = cdr16_to_mm(r.read_i16_le("v3-5 page h").ok()?).abs();
    if !(1.0..=50_000.0).contains(&w) || !(1.0..=50_000.0).contains(&h) {
        return None;
    }
    let bleed = cdr16_to_mm(r.read_i16_le("v3-5 page bleed").unwrap_or(0)).max(0.0);
    let name = if r.remaining() >= 1 {
        let len = usize::from(r.read_u8("v3-5 page name_len").unwrap_or(0)).min(r.remaining());
        decode_cdr_text(
            r.read_bytes(len, "v3-5 page name").unwrap_or(&[]),
            None,
            None,
        )
    } else {
        String::new()
    };
    Some((w, h, bleed, name))
}

/// Parses a 16-bit CDR v3–v5 `loda` object chunk (compact header: `[u16 kind, u16 fill_id, u16 outl_id, ...]`).
pub(crate) fn parse_v3_v5_loda(data: &[u8], page_w_mm: f64, page_h_mm: f64) -> Option<ParsedLoda> {
    if data.len() < 10 {
        return None;
    }
    // Never intercept 20B or 12B v6+ loda tables
    let mut r = ByteReader::new(data);
    let magic = r.read_u16_le("v3-5 loda magic").ok()?;
    if magic != 0x0003 && magic != 0x0004 && magic != 0x0005 {
        return None;
    }
    let ctype = u32::from(r.read_u16_le("v3-5 ctype").ok()?);
    let fill_id = u32::from(r.read_u16_le("v3-5 fill_id").ok()?);
    let outl_id = u32::from(r.read_u16_le("v3-5 outl_id").ok()?);
    let name_len = usize::from(r.read_u8("v3-5 name_len").ok()?);
    let name = if name_len <= r.remaining() {
        let s = decode_cdr_text(r.read_bytes(name_len, "v3-5 name").ok()?, None, None);
        if s.is_empty() {
            "Legacy Object".to_owned()
        } else {
            s
        }
    } else {
        "Legacy Object".to_owned()
    };

    let mut out = ParsedLoda {
        is_real_cdr: false,
        fill_id: (fill_id != 0).then_some(fill_id),
        outline_id: (outl_id != 0).then_some(outl_id),
        props: Some(ParsedObjProps {
            name,
            visible: true,
            locked: false,
            printable: true,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
        }),
        ..ParsedLoda::default()
    };

    match ctype {
        CDR_CTYPE_RECTANGLE if r.remaining() >= 10 => {
            let x0 = cdr16_to_mm(r.read_i16_le("v3-5 rect x0").ok()?) + page_w_mm * 0.5;
            let y0 = page_h_mm * 0.5 - cdr16_to_mm(r.read_i16_le("v3-5 rect y0").ok()?);
            let x1 = cdr16_to_mm(r.read_i16_le("v3-5 rect x1").ok()?) + page_w_mm * 0.5;
            let y1 = page_h_mm * 0.5 - cdr16_to_mm(r.read_i16_le("v3-5 rect y1").ok()?);
            let corner = cdr16_to_mm(r.read_i16_le("v3-5 rect r").ok()?).max(0.0);
            let min_x = x0.min(x1);
            let min_y = y0.min(y1);
            let w = (x1 - x0).abs().max(0.1);
            let h = (y1 - y0).abs().max(0.1);
            let mut path = coree_core::BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                min_x, min_y, w, h,
            ));
            if corner > 0.01 {
                path = ParametricShape::Rectangle {
                    width: w,
                    height: h,
                    corner_radii: [corner; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }
                .to_bezier_path()
                .transformed(&coree_core::Affine2D::translate(min_x, min_y));
            }
            out.curve = Some(path);
            Some(out)
        }
        CDR_CTYPE_ELLIPSE if r.remaining() >= 8 => {
            let cx = cdr16_to_mm(r.read_i16_le("v3-5 ell cx").ok()?) + page_w_mm * 0.5;
            let cy = page_h_mm * 0.5 - cdr16_to_mm(r.read_i16_le("v3-5 ell cy").ok()?);
            let rx = cdr16_to_mm(r.read_i16_le("v3-5 ell rx").ok()?)
                .abs()
                .max(0.1);
            let ry = cdr16_to_mm(r.read_i16_le("v3-5 ell ry").ok()?)
                .abs()
                .max(0.1);
            let path = ParametricShape::Ellipse {
                rx,
                ry,
                mode: EllipseMode::Full,
                start_deg: 0.0,
                end_deg: 360.0,
            }
            .to_bezier_path()
            .transformed(&coree_core::Affine2D::translate(cx, cy));
            out.curve = Some(path);
            Some(out)
        }
        CDR_CTYPE_CURVE if r.remaining() >= 2 => {
            let pt_count = usize::from(r.read_u16_le("v3-5 curve pts").ok()?);
            if pt_count == 0 || pt_count > 16_384 || pt_count.saturating_mul(5) > r.remaining() {
                return None;
            }
            let mut pts = Vec::with_capacity(pt_count);
            for _ in 0..pt_count {
                let x = cdr16_to_mm(r.read_i16_le("v3-5 pt x").ok()?) + page_w_mm * 0.5;
                let y = page_h_mm * 0.5 - cdr16_to_mm(r.read_i16_le("v3-5 pt y").ok()?);
                pts.push(Point2D::new(x, y));
            }
            let flags = r.read_bytes(pt_count, "v3-5 curve flags").ok()?;
            out.curve = Some(decode_curve_points(&pts, flags));
            Some(out)
        }
        CDR_CTYPE_ARTISTIC_TEXT | CDR_CTYPE_PARAGRAPH_TEXT if r.remaining() >= 4 => {
            let tid = u32::from(r.read_u16_le("v3-5 text id").ok()?);
            out.text_stream_id = Some(tid);
            Some(out)
        }
        _ => None,
    }
}

/// Parses a legacy CDR v3–v5 `fild` chunk (`[u16 fill_id, u16 fill_type, u16 color_model, ...]`).
pub(crate) fn parse_v3_v5_fild(data: &[u8]) -> Option<(u32, Fill)> {
    if data.len() < 8 || data.len() > 24 {
        return None;
    }
    let mut r = ByteReader::new(data);
    let fill_id = u32::from(r.read_u16_le("v3-5 fild id").ok()?);
    let fill_type = r.read_u16_le("v3-5 fild type").ok()?;
    if fill_type == 0 {
        return Some((fill_id, Fill::None));
    }
    let model = r.read_u16_le("v3-5 fild model").ok()?;
    let payload = r.read_rest();
    if let Ok(c) = decode_cdr_color(model, payload, None) {
        return Some((fill_id, Fill::Solid(c)));
    }
    if fill_type == 1 {
        return Some((
            fill_id,
            Fill::Solid(Color::Spot {
                name: format!("Spot {fill_id}"),
                tint: 1.0,
                fallback_cmyk: derive_spot_fallback_cmyk(&format!("Spot {fill_id}")),
                alpha: 1.0,
            }),
        ));
    }
    None
}

/// Parses a legacy CDR v3–v5 `outl` chunk (`[u16 outl_id, u16 flags, u16 width_16, u16 model, ...]`).
pub(crate) fn parse_v3_v5_outl(data: &[u8]) -> Option<(u32, OutlineStyle, bool)> {
    if data.len() < 8 || data.len() > 24 {
        return None;
    }
    let mut r = ByteReader::new(data);
    let outl_id = u32::from(r.read_u16_le("v3-5 outl id").ok()?);
    let flags = r.read_u16_le("v3-5 outl flags").ok()?;
    if (flags & 0x0001) != 0 {
        return Some((outl_id, OutlineStyle::none(), true));
    }
    let w_mm = cdr16_to_mm(r.read_i16_le("v3-5 outl w").ok()?).abs();
    let model = r.read_u16_le("v3-5 outl model").ok()?;
    let color = decode_cdr_color(model, r.read_rest(), None).unwrap_or(Color::BLACK);
    Some((
        outl_id,
        OutlineStyle {
            color,
            width_mm: w_mm,
            is_hairline: w_mm > 0.0 && w_mm <= 0.08,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            ..OutlineStyle::default()
        },
        false,
    ))
}

/// Parses a legacy CDR v3–v5 `txsm` chunk (`[u8 0x03, u8 is_para, u16 codepage, u16 pt_size, u16 len, bytes...]`).
pub(crate) fn parse_v3_v5_txsm(data: &[u8]) -> CdrResult<Option<NodePayload>> {
    if data.len() < 8 || data[0] != 0x03 {
        return Ok(None);
    }
    let mut r = ByteReader::new(data);
    let _ver = r.read_u8("v3-5 txsm ver")?;
    let is_para = r.read_u8("v3-5 txsm para")? != 0;
    let cp = r.read_u16_le("v3-5 txsm cp")?;
    let pt_size = f64::from(r.read_u16_le("v3-5 txsm pt")?.max(4));
    let len = usize::from(r.read_u16_le("v3-5 txsm len")?);
    let text_bytes = r.read_bytes(len, "v3-5 txsm text")?;
    let decoded = decode_cdr_text(text_bytes, (cp != 0).then_some(cp), None);
    let style = TextStyle {
        font_size_pt: pt_size,
        ..TextStyle::default()
    };
    if is_para {
        Ok(Some(NodePayload::ParagraphText(ParagraphText::new(
            decoded,
            Vec2D::new(100.0, 40.0),
            style,
        ))))
    } else {
        Ok(Some(NodePayload::ArtisticText(ArtisticText::from_plain(
            decoded, style,
        ))))
    }
}
