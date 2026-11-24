// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDR `stlt`/`font`/`txsm` text-stream, font-table, and blend-mode codecs.

use super::legacy_v3_v5::parse_v3_v5_txsm;
use super::loda::ParsedTextMeta;
use crate::coord::{
    cdr32_to_mm, contains_arabic, decode_cdr_text, mm_to_cdr32, read_tagged_color,
    write_tagged_color, CODEPAGE_ARABIC_CP1256, CODEPAGE_UTF16LE, CODEPAGE_UTF8,
};
use crate::error::{CdrError, CdrResult};
use crate::limits::ByteReader;
use crate::riff::RiffNode;
use coree_core::{
    math::sanitize_f32, math::sanitize_f64, ArtisticText, BlendMode, NodePayload, ParagraphText,
    TextAlign, TextDirection, TextRun, TextStyle, Vec2D,
};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(crate) struct ParsedStyleEntry {
    pub style: TextStyle,
    pub codepage: Option<u16>,
    pub charset: Option<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedFontEntry {
    pub family: String,
    pub codepage: Option<u16>,
    pub charset: Option<u8>,
    #[allow(dead_code)]
    pub weight: u16,
}

/// Parses a `LIST font` container (`font` chunks mapping font ID -> family/codepage/charset/weight).
pub(crate) fn parse_font_list(
    children: &[RiffNode],
    styles: &mut HashMap<u32, ParsedStyleEntry>,
    fonts: &mut HashMap<u32, ParsedFontEntry>,
) {
    for ch in children {
        if let RiffNode::Chunk { fourcc, data, .. } = ch {
            if fourcc == b"stlt" {
                if let Ok((id, entry)) = parse_stlt_chunk(data) {
                    styles.insert(id, entry);
                }
            } else if fourcc == b"font" && data.len() >= 8 {
                let mut r = ByteReader::new(data);
                let Ok(font_id) = r.read_u32_le("font id") else {
                    continue;
                };
                let codepage = r.read_u16_le("font codepage").ok().filter(|&c| c != 0);
                let charset = r.read_u8("font charset").ok().filter(|&c| c != 0);
                let weight = r.read_u16_le("font weight").unwrap_or(400).clamp(100, 900);
                let family = if r.remaining() >= 2 {
                    let flen = usize::from(r.read_u16_le("font name len").unwrap_or(0));
                    if flen > 0 && flen <= r.remaining() {
                        decode_cdr_text(
                            r.read_bytes(flen, "font family").unwrap_or(&[]),
                            codepage,
                            charset,
                        )
                    } else {
                        decode_cdr_text(r.read_rest(), codepage, charset)
                    }
                } else {
                    String::new()
                };
                if !family.trim().is_empty() {
                    let fam = family.trim().to_owned();
                    fonts.insert(
                        font_id,
                        ParsedFontEntry {
                            family: fam.clone(),
                            codepage,
                            charset,
                            weight,
                        },
                    );
                    styles.entry(font_id).or_insert_with(|| {
                        let mut ts = TextStyle {
                            font_family: fam,
                            ..TextStyle::default()
                        };
                        ts.set_weight(weight);
                        if charset == Some(crate::coord::CHARSET_ARABIC) {
                            ts.direction = TextDirection::Rtl;
                        }
                        ParsedStyleEntry {
                            style: ts,
                            codepage,
                            charset,
                        }
                    });
                }
            }
        }
    }
}

pub(crate) fn parse_stlt_chunk(data: &[u8]) -> CdrResult<(u32, ParsedStyleEntry)> {
    let mut r = ByteReader::new(data);
    let style_id = r.read_u32_le("stlt id")?;
    let entry = parse_stlt_body(&mut r)?;
    Ok((style_id, entry))
}

pub(crate) fn parse_stlt_body(r: &mut ByteReader<'_>) -> CdrResult<ParsedStyleEntry> {
    let codepage_raw = r.read_u16_le("stlt codepage")?;
    let charset_raw = r.read_u8("stlt charset")?;
    let flags = r.read_u8("stlt flags")?;
    let direction = decode_text_direction(r.read_u8("stlt dir")?);
    let align = decode_text_align(r.read_u8("stlt align")?);
    let font_size_pt = sanitize_f64(r.read_f64_le("stlt font_size")?, 12.0).max(1.0);
    let letter_spacing_mm = sanitize_f64(r.read_f64_le("stlt letter_spacing")?, 0.0);
    let line_height_ratio = sanitize_f64(r.read_f64_le("stlt line_height")?, 1.2).max(0.5);
    let kashida_stretch = sanitize_f32(r.read_f32_le("stlt kashida")?, 0.0).clamp(0.0, 2.0);
    let font_len = usize::from(r.read_u16_le("stlt font_len")?);
    let fb = r.read_bytes(font_len, "stlt font_family")?;
    let font_family = decode_cdr_text(
        fb,
        (codepage_raw != 0).then_some(codepage_raw),
        (charset_raw != 0).then_some(charset_raw),
    );
    let fill = read_tagged_color(r, "stlt color")?;

    Ok(ParsedStyleEntry {
        style: TextStyle {
            font_family: if font_family.is_empty() {
                "Noto Sans".to_owned()
            } else {
                font_family
            },
            font_size_pt,
            bold: (flags & 0x01) != 0,
            font_weight: if (flags & 0x01) != 0 { 700 } else { 400 },
            italic: (flags & 0x02) != 0,
            underline: (flags & 0x04) != 0,
            strikethrough: (flags & 0x08) != 0,
            letter_spacing_mm,
            line_height_ratio,
            fill,
            direction,
            align,
            kashida_stretch,
            ..TextStyle::default()
        },
        codepage: (codepage_raw != 0).then_some(codepage_raw),
        charset: (charset_raw != 0).then_some(charset_raw),
    })
}

pub(crate) fn parse_txsm_chunk(
    data: &[u8],
    loda_style_id: Option<u32>,
    inline_style: Option<&ParsedStyleEntry>,
    text_meta: Option<ParsedTextMeta>,
    styles: &HashMap<u32, ParsedStyleEntry>,
    fonts: &HashMap<u32, ParsedFontEntry>,
    cdss_default: Option<&TextStyle>,
) -> CdrResult<NodePayload> {
    let mut r = ByteReader::new(data);
    let format_ver = r.read_u8("txsm version")?;

    match format_ver {
        0x01 if data.len() >= 21 => {
            let declared_len =
                u32::from_le_bytes([data[17], data[18], data[19], data[20]]) as usize;
            if 21usize.checked_add(declared_len) == Some(data.len()) {
                let style_id = r.read_u32_le("txsm style_id")?;
                let cp_raw = r.read_u16_le("txsm codepage")?;
                let cs_raw = r.read_u8("txsm charset")?;
                let is_para = r.read_u8("txsm is_paragraph")? != 0;
                let frame_w = cdr32_to_mm(r.read_i32_le("txsm frame_w")?).abs();
                let frame_h = cdr32_to_mm(r.read_i32_le("txsm frame_h")?).abs();
                let text_len = usize::try_from(r.read_u32_le("txsm text_len")?)
                    .map_err(|_| CdrError::ArithmeticOverflow("txsm text_len"))?;
                let raw_text = r.read_bytes(text_len, "txsm text bytes")?;

                let resolved_entry = inline_style
                    .or_else(|| styles.get(&style_id))
                    .or_else(|| loda_style_id.and_then(|sid| styles.get(&sid)));

                let font_hint = fonts.get(&style_id);
                let codepage = (cp_raw != 0)
                    .then_some(cp_raw)
                    .or_else(|| resolved_entry.and_then(|e| e.codepage))
                    .or_else(|| font_hint.and_then(|f| f.codepage));
                let charset = (cs_raw != 0)
                    .then_some(cs_raw)
                    .or_else(|| resolved_entry.and_then(|e| e.charset))
                    .or_else(|| font_hint.and_then(|f| f.charset));

                let decoded = decode_cdr_text(raw_text, codepage, charset);
                let mut style = resolved_entry.map_or_else(
                    || cdss_default.cloned().unwrap_or_default(),
                    |e| e.style.clone(),
                );
                if let Some(f) = font_hint {
                    if style.font_family == "Noto Sans" && !f.family.is_empty() {
                        style.font_family = f.family.clone();
                    }
                }
                if contains_arabic(&decoded) && style.direction == TextDirection::Ltr {
                    style.direction = TextDirection::Rtl;
                }

                if is_para || text_meta.is_some_and(|m| m.is_paragraph) {
                    let (w, h, cols, gutter) = match text_meta {
                        Some(m) if m.frame_w_mm > 0.1 && m.frame_h_mm > 0.1 => {
                            (m.frame_w_mm, m.frame_h_mm, m.columns, m.gutter_mm)
                        }
                        _ => (frame_w.max(10.0), frame_h.max(10.0), 1, 5.0),
                    };
                    let mut para = ParagraphText::new(decoded, Vec2D::new(w, h), style);
                    para.columns = cols;
                    para.column_gutter_mm = gutter;
                    para.linked_next_frame = text_meta.and_then(|m| m.linked_next_frame);
                    return Ok(NodePayload::ParagraphText(para));
                }
                return Ok(NodePayload::ArtisticText(ArtisticText::from_plain(
                    decoded, style,
                )));
            }
            parse_cdr_native_txsm(data, inline_style, text_meta, cdss_default)
        }
        0x02 if data.len() >= 22 => {
            let is_para = r.read_u8("txsm2 is_para")? != 0;
            let direction = decode_text_direction(r.read_u8("txsm2 dir")?);
            let align = decode_text_align(r.read_u8("txsm2 align")?);
            let frame_w = cdr32_to_mm(r.read_i32_le("txsm2 frame_w")?).abs();
            let frame_h = cdr32_to_mm(r.read_i32_le("txsm2 frame_h")?).abs();
            let columns = r.read_u32_le("txsm2 cols")?.max(1);
            let gutter = cdr32_to_mm(r.read_i32_le("txsm2 gutter")?).max(0.0);
            let run_count = usize::from(r.read_u16_le("txsm2 run_count")?);
            if run_count > 4096 || run_count.saturating_mul(12) > r.remaining() {
                return Err(CdrError::InvalidChunk {
                    fourcc: "txsm".to_owned(),
                    reason: format!("invalid run_count {run_count}"),
                });
            }

            let mut runs = Vec::with_capacity(run_count);
            for _ in 0..run_count {
                let cp_raw = r.read_u16_le("txsm2 run cp")?;
                let cs_raw = r.read_u8("txsm2 run cs")?;
                let style_id = r.read_u32_le("txsm2 run style_id")?;
                let has_inline = r.read_u8("txsm2 run has_inline")? != 0;
                let entry = if has_inline {
                    Some(parse_stlt_body(&mut r)?)
                } else {
                    styles
                        .get(&style_id)
                        .cloned()
                        .or_else(|| inline_style.cloned())
                };
                let text_len = usize::try_from(r.read_u32_le("txsm2 run text_len")?)
                    .map_err(|_| CdrError::ArithmeticOverflow("txsm2 text_len"))?;
                let raw_bytes = r.read_bytes(text_len, "txsm2 run text")?;

                let cp = (cp_raw != 0)
                    .then_some(cp_raw)
                    .or_else(|| entry.as_ref().and_then(|e| e.codepage));
                let cs = (cs_raw != 0)
                    .then_some(cs_raw)
                    .or_else(|| entry.as_ref().and_then(|e| e.charset));

                let text = decode_cdr_text(raw_bytes, cp, cs);
                let style =
                    entry.map_or_else(|| cdss_default.cloned().unwrap_or_default(), |e| e.style);
                runs.push(TextRun::new(text, style));
            }

            if is_para {
                Ok(NodePayload::ParagraphText(ParagraphText {
                    runs,
                    frame_size: Vec2D::new(frame_w.max(1.0), frame_h.max(1.0)),
                    columns,
                    column_gutter_mm: gutter,
                    linked_next_frame: text_meta.and_then(|m| m.linked_next_frame),
                    wrap_mode: coree_core::TextWrapMode::None,
                    drop_cap: None,
                    direction,
                    align,
                }))
            } else {
                Ok(NodePayload::ArtisticText(ArtisticText {
                    runs,
                    direction,
                    align,
                }))
            }
        }
        0x03 if data.len() >= 8 => {
            if let Some(p) = parse_v3_v5_txsm(data)? {
                return Ok(p);
            }
            parse_cdr_native_txsm(data, inline_style, text_meta, cdss_default)
        }
        _ => parse_cdr_native_txsm(data, inline_style, text_meta, cdss_default),
    }
}

/// Decodes a native CDR v12–26 `txsm`/`txst` binary text stream (supporting single-run and
/// multi-run UTF-16LE / CP1256 / CP1251 segments).
fn parse_cdr_native_txsm(
    data: &[u8],
    inline_style: Option<&ParsedStyleEntry>,
    text_meta: Option<ParsedTextMeta>,
    cdss_default: Option<&TextStyle>,
) -> CdrResult<NodePayload> {
    let cp = inline_style.and_then(|s| s.codepage);
    let cs = inline_style.and_then(|s| s.charset);

    let decoded = if data.len() >= 16 {
        let encoding = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let char_count = u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
        if encoding == 1
            && char_count > 0
            && char_count
                .checked_mul(2)
                .and_then(|b| 16usize.checked_add(b))
                .is_some_and(|end| end <= data.len())
        {
            let byte_len = char_count * 2;
            decode_cdr_text(&data[16..16 + byte_len], Some(CODEPAGE_UTF16LE), None)
        } else if encoding == 0
            && char_count > 0
            && 16usize
                .checked_add(char_count)
                .is_some_and(|end| end <= data.len())
        {
            decode_cdr_text(&data[16..16 + char_count], cp, cs)
        } else {
            decode_cdr_text(data, cp, cs)
        }
    } else {
        decode_cdr_text(data, cp, cs)
    };

    let mut style = inline_style.map_or_else(
        || cdss_default.cloned().unwrap_or_default(),
        |s| s.style.clone(),
    );
    if contains_arabic(&decoded) && style.direction == TextDirection::Ltr {
        style.direction = TextDirection::Rtl;
    }

    if let Some(m) = text_meta {
        if m.is_paragraph {
            let mut para = ParagraphText::new(
                decoded,
                Vec2D::new(m.frame_w_mm.max(10.0), m.frame_h_mm.max(10.0)),
                style,
            );
            para.columns = m.columns.max(1);
            para.column_gutter_mm = m.gutter_mm;
            para.linked_next_frame = m.linked_next_frame;
            return Ok(NodePayload::ParagraphText(para));
        }
    }

    Ok(NodePayload::ArtisticText(ArtisticText::from_plain(
        decoded, style,
    )))
}

pub(crate) fn encode_stlt_chunk(
    style_id: u32,
    style: &TextStyle,
    codepage: u16,
    charset: u8,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&style_id.to_le_bytes());
    encode_stlt_body(&mut out, style, codepage, charset);
    out
}

fn encode_stlt_body(out: &mut Vec<u8>, style: &TextStyle, codepage: u16, charset: u8) {
    out.extend_from_slice(&codepage.to_le_bytes());
    out.push(charset);
    let mut flags = 0u8;
    if style.bold {
        flags |= 0x01;
    }
    if style.italic {
        flags |= 0x02;
    }
    if style.underline {
        flags |= 0x04;
    }
    if style.strikethrough {
        flags |= 0x08;
    }
    out.push(flags);
    out.push(encode_text_direction(style.direction));
    out.push(encode_text_align(style.align));
    out.extend_from_slice(&sanitize_f64(style.font_size_pt, 12.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(style.letter_spacing_mm, 0.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(style.line_height_ratio, 1.2).to_le_bytes());
    out.extend_from_slice(&sanitize_f32(style.kashida_stretch, 0.0).to_le_bytes());
    let fb = style.font_family.as_bytes();
    let flen = fb.len().min(u16::MAX as usize);
    out.extend_from_slice(&(flen as u16).to_le_bytes());
    out.extend_from_slice(&fb[..flen]);
    write_tagged_color(out, &style.fill);
}

pub(crate) fn encode_txsm_artistic(text: &ArtisticText) -> Vec<u8> {
    encode_txsm_runs(
        false,
        text.direction,
        text.align,
        Vec2D::ZERO,
        1,
        5.0,
        &text.runs,
    )
}

pub(crate) fn encode_txsm_paragraph(para: &ParagraphText) -> Vec<u8> {
    encode_txsm_runs(
        true,
        para.direction,
        para.align,
        para.frame_size,
        para.columns,
        para.column_gutter_mm,
        &para.runs,
    )
}

fn encode_txsm_runs(
    is_para: bool,
    direction: TextDirection,
    align: TextAlign,
    frame_size: Vec2D,
    columns: u32,
    gutter_mm: f64,
    runs: &[TextRun],
) -> Vec<u8> {
    let mut out = vec![
        0x02,
        u8::from(is_para),
        encode_text_direction(direction),
        encode_text_align(align),
    ];
    out.extend_from_slice(&mm_to_cdr32(frame_size.x).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(frame_size.y).to_le_bytes());
    out.extend_from_slice(&columns.to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(gutter_mm).to_le_bytes());
    let count = runs.len().min(u16::MAX as usize);
    out.extend_from_slice(&(count as u16).to_le_bytes());
    for run in &runs[..count] {
        out.extend_from_slice(&CODEPAGE_UTF8.to_le_bytes());
        out.push(0);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(1);
        encode_stlt_body(&mut out, &run.style, CODEPAGE_UTF8, 0);
        let tb = run.text.as_bytes();
        out.extend_from_slice(&(tb.len() as u32).to_le_bytes());
        out.extend_from_slice(tb);
    }
    out
}

/// Helper used by fixtures to construct an authentic legacy Windows-1256 (CP1256) `txsm` chunk.
#[must_use]
pub(crate) fn encode_legacy_cp1256_txsm(
    style_id: u32,
    cp1256_bytes: &[u8],
    explicit_codepage: bool,
    charset: u8,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + cp1256_bytes.len());
    out.push(0x01);
    out.extend_from_slice(&style_id.to_le_bytes());
    let cp = if explicit_codepage {
        CODEPAGE_ARABIC_CP1256
    } else {
        0
    };
    out.extend_from_slice(&cp.to_le_bytes());
    out.push(charset);
    out.push(0);
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&(cp1256_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(cp1256_bytes);
    out
}

pub(crate) fn encode_blend_mode(m: BlendMode) -> u8 {
    match m {
        BlendMode::Normal => 0,
        BlendMode::Multiply => 1,
        BlendMode::Screen => 2,
        BlendMode::Overlay => 3,
        BlendMode::Darken => 4,
        BlendMode::Lighten => 5,
        BlendMode::Difference => 6,
        BlendMode::Exclusion => 7,
        BlendMode::SoftLight => 8,
        BlendMode::HardLight => 9,
        BlendMode::ColorDodge => 10,
        BlendMode::ColorBurn => 11,
        BlendMode::Hue => 12,
        BlendMode::Saturation => 13,
        BlendMode::Color => 14,
        BlendMode::Luminosity => 15,
    }
}

pub(crate) fn decode_blend_mode(v: u8) -> BlendMode {
    match v {
        1 => BlendMode::Multiply,
        2 => BlendMode::Screen,
        3 => BlendMode::Overlay,
        4 => BlendMode::Darken,
        5 => BlendMode::Lighten,
        6 => BlendMode::Difference,
        7 => BlendMode::Exclusion,
        8 => BlendMode::SoftLight,
        9 => BlendMode::HardLight,
        10 => BlendMode::ColorDodge,
        11 => BlendMode::ColorBurn,
        12 => BlendMode::Hue,
        13 => BlendMode::Saturation,
        14 => BlendMode::Color,
        15 => BlendMode::Luminosity,
        _ => BlendMode::Normal,
    }
}

fn encode_text_direction(d: TextDirection) -> u8 {
    match d {
        TextDirection::Auto => 0,
        TextDirection::Ltr => 1,
        TextDirection::Rtl => 2,
    }
}

fn decode_text_direction(v: u8) -> TextDirection {
    match v {
        1 => TextDirection::Ltr,
        2 => TextDirection::Rtl,
        _ => TextDirection::Auto,
    }
}

fn encode_text_align(a: TextAlign) -> u8 {
    match a {
        TextAlign::Left => 0,
        TextAlign::Center => 1,
        TextAlign::Right => 2,
        TextAlign::Justify => 3,
        TextAlign::ForceJustifyKashida => 4,
    }
}

fn decode_text_align(v: u8) -> TextAlign {
    match v {
        1 => TextAlign::Center,
        2 => TextAlign::Right,
        3 => TextAlign::Justify,
        4 => TextAlign::ForceJustifyKashida,
        _ => TextAlign::Left,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{ObjectId, Vec2D};

    #[test]
    fn parse_cdr_native_txsm_propagates_linked_next_frame() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&0x01u32.to_le_bytes()); // native txsm header (<> 0x02 / 0x03)
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes()); // encoding = 0 (single-byte)
        raw.extend_from_slice(&5u32.to_le_bytes()); // char_count = 5
        raw.extend_from_slice(b"Hello");

        let meta = ParsedTextMeta {
            is_paragraph: true,
            frame_w_mm: 50.0,
            frame_h_mm: 25.0,
            columns: 2,
            gutter_mm: 4.0,
            linked_next_frame: Some(ObjectId(42)),
        };

        let payload = parse_cdr_native_txsm(&raw, None, Some(meta), None).unwrap();
        match payload {
            NodePayload::ParagraphText(para) => {
                assert_eq!(para.plain_text(), "Hello");
                assert_eq!(para.frame_size, Vec2D::new(50.0, 25.0));
                assert_eq!(para.columns, 2);
                assert_eq!(para.linked_next_frame, Some(ObjectId(42)));
            }
            other => panic!("expected ParagraphText, got {other:?}"),
        }
    }

    #[test]
    fn parse_txsm_01_propagates_linked_next_frame() {
        let text = b"Linked 0x01";
        let mut raw = Vec::new();
        raw.push(0x01); // format version 0x01
        raw.extend_from_slice(&0u32.to_le_bytes()); // style_id
        raw.extend_from_slice(&CODEPAGE_UTF8.to_le_bytes()); // codepage
        raw.push(0); // charset
        raw.push(1); // is_paragraph = true
        raw.extend_from_slice(&mm_to_cdr32(60.0).to_le_bytes()); // frame_w
        raw.extend_from_slice(&mm_to_cdr32(30.0).to_le_bytes()); // frame_h
        raw.extend_from_slice(&(text.len() as u32).to_le_bytes()); // text_len
        raw.extend_from_slice(text);

        let meta = ParsedTextMeta {
            is_paragraph: true,
            frame_w_mm: 80.0,
            frame_h_mm: 40.0,
            columns: 3,
            gutter_mm: 5.0,
            linked_next_frame: Some(ObjectId(42)),
        };

        let payload = parse_txsm_chunk(
            &raw,
            None,
            None,
            Some(meta),
            &HashMap::new(),
            &HashMap::new(),
            None,
        )
        .unwrap();
        match payload {
            NodePayload::ParagraphText(para) => {
                assert_eq!(para.plain_text(), "Linked 0x01");
                assert_eq!(para.frame_size, Vec2D::new(80.0, 40.0));
                assert_eq!(para.columns, 3);
                assert!((para.column_gutter_mm - 5.0).abs() < 1e-6);
                assert_eq!(para.linked_next_frame, Some(ObjectId(42)));
            }
            other => panic!("expected ParagraphText, got {other:?}"),
        }
    }
}
