// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room coordinate conversions (0.1 µm CDR units & 16/32-bit CMX units),
//! Corel color model codecs (`CMYK255`, `CMYK100`, `RGB`, `BGR`, `Gray`, `Lab`,
//! `Spot`), and legacy Arabic Windows-1256 (CP1256) / UTF-16LE / UTF-8 text codecs.

use crate::error::CdrResult;
use crate::limits::ByteReader;
use crate::riff::RiffEndian;
use coree_core::{
    math::{sanitize_f32, sanitize_f64, CDR_UNITS_PER_MM, MM_PER_INCH},
    Color,
};

/// Windows Arabic codepage identifier (`Windows-1256`).
pub const CODEPAGE_ARABIC_CP1256: u16 = 1256;
/// Windows Western Latin codepage identifier (`Windows-1252`).
pub const CODEPAGE_LATIN1_CP1252: u16 = 1252;
/// UTF-16 Little-Endian codepage identifier.
pub const CODEPAGE_UTF16LE: u16 = 1200;
/// UTF-8 codepage identifier.
pub const CODEPAGE_UTF8: u16 = 65001;
/// Windows GDI `ARABIC_CHARSET` constant (`178 = 0xB2`) stored in legacy `stlt`/`font` chunks.
pub const CHARSET_ARABIC: u8 = 178;

/// Physical coordinate unit used inside `.cmx` containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CmxCoordUnit {
    /// `0.1 µm` (`1 mm = 10_000` units, standard 32-bit CMX & CDR).
    TenthMicron,
    /// `0.001 inch` (`1 inch = 1_000` units, standard 16-bit CMX).
    MilliInch,
    /// `0.01 mm` (`1 mm = 100` units, metric 16-bit CMX).
    HundredthMm,
}

impl CmxCoordUnit {
    /// Encoded numeric unit tag used in CMX `cont` headers.
    #[must_use]
    pub const fn to_tag(self) -> u16 {
        match self {
            Self::MilliInch => 0x0023,   // inch-based 16-bit unit
            Self::TenthMicron => 0x0064, // 0.1 µm metric 32-bit unit
            Self::HundredthMm => 0x0065, // 0.01 mm metric 16-bit unit
        }
    }

    /// Decodes a CMX `cont` unit tag, falling back by coordinate byte size.
    #[must_use]
    pub const fn from_tag(tag: u16, coord_bytes: u16) -> Self {
        match tag {
            0x0023 => Self::MilliInch,
            0x0064 => Self::TenthMicron,
            0x0065 => Self::HundredthMm,
            _ if coord_bytes == 2 => Self::HundredthMm,
            _ => Self::TenthMicron,
        }
    }

    /// Scale factor: millimeters per single raw CMX coordinate unit.
    #[must_use]
    pub const fn mm_per_unit(self) -> f64 {
        match self {
            Self::TenthMicron => 1.0 / CDR_UNITS_PER_MM,
            Self::MilliInch => MM_PER_INCH / 1_000.0,
            Self::HundredthMm => 0.01,
        }
    }
}

/// Converts a signed 32-bit CDR `0.1 µm` scalar into canonical millimeters (`mm`).
#[inline]
#[must_use]
pub fn cdr32_to_mm(units: i32) -> f64 {
    f64::from(units) / CDR_UNITS_PER_MM
}

/// Converts a millimeter scalar into a clamped signed 32-bit CDR `0.1 µm` value.
#[inline]
#[must_use]
pub fn mm_to_cdr32(mm: f64) -> i32 {
    let scaled = (sanitize_f64(mm, 0.0) * CDR_UNITS_PER_MM).round();
    scaled.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// Converts a signed 16-bit CMX coordinate into millimeters (`mm`).
#[inline]
#[must_use]
pub fn cmx16_to_mm(units: i16, unit: CmxCoordUnit) -> f64 {
    f64::from(units) * unit.mm_per_unit()
}

/// Converts a millimeter scalar into a clamped signed 16-bit CMX coordinate.
#[inline]
#[must_use]
pub fn mm_to_cmx16(mm: f64, unit: CmxCoordUnit) -> i16 {
    let scaled = (sanitize_f64(mm, 0.0) / unit.mm_per_unit()).round();
    scaled.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

/// Converts a 32-bit CMX coordinate into millimeters (`mm`).
#[inline]
#[must_use]
pub fn cmx32_to_mm(units: i32, unit: CmxCoordUnit) -> f64 {
    f64::from(units) * unit.mm_per_unit()
}

/// Converts a millimeter scalar into a clamped 32-bit CMX coordinate.
#[inline]
#[must_use]
pub fn mm_to_cmx32(mm: f64, unit: CmxCoordUnit) -> i32 {
    let scaled = (sanitize_f64(mm, 0.0) / unit.mm_per_unit()).round();
    scaled.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// Converts a page-centered Y-up CDR coordinate `(x, y)` in `0.1 µm` units into
/// `coree-core` top-left Y-down millimeter space.
#[inline]
#[must_use]
pub fn cdr_point_to_page_mm(
    x_cdr: i32,
    y_cdr: i32,
    page_w_mm: f64,
    page_h_mm: f64,
) -> coree_core::Point2D {
    coree_core::Point2D::new(cdr32_to_mm(x_cdr), cdr32_to_mm(y_cdr))
        .from_centered_y_up(page_w_mm, page_h_mm)
}

/// Converts a `coree-core` top-left Y-down millimeter point into page-centered Y-up
/// `0.1 µm` CDR units.
#[inline]
#[must_use]
pub fn page_mm_to_cdr_point(pt: coree_core::Point2D, page_w_mm: f64, page_h_mm: f64) -> (i32, i32) {
    let cx = pt.x - page_w_mm * 0.5;
    let cy = page_h_mm * 0.5 - pt.y;
    (mm_to_cdr32(cx), mm_to_cdr32(cy))
}

// =============================================================================
// Corel Color Model Codecs
// =============================================================================

/// Clean-room Corel color model identifiers used across CDR and CMX chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum CdrColorModel {
    /// CMYK channels in `0..=100` (4 bytes: `c, m, y, k`).
    Cmyk100 = 2,
    /// CMYK channels in `0..=255` (4 bytes: `c, m, y, k`).
    Cmyk255 = 3,
    /// sRGB channels in `0..=255` (3 or 4 bytes: `r, g, b[, a]`).
    Rgb = 4,
    /// Windows BGR channels in `0..=255` (3 or 4 bytes: `b, g, r[, a]`).
    Bgr = 5,
    /// Cylindrical HSL (`h: u16` 0..360, `s: u8` 0..255, `l: u8` 0..255).
    Hsl = 6,
    /// Grayscale (`v: u8` 0..255, where 0 = black and 255 = white).
    Gray = 9,
    /// CIE L*a*b* (`l: u8` 0..100 or 0..255, `a: i8` -128..127, `b: i8` -128..127).
    Lab = 12,
    /// Named spot ink (`tint: u8` 0..100, fallback CMYK255 `[4]`, `alpha: u8`, name string).
    Spot = 14,
    /// Extended RGBA (`r, g, b, a` in `0..=255`).
    Rgba = 20,
    /// Extended CMYK255 + alpha (`c, m, y, k, a` in `0..=255`).
    Cmyka255 = 21,
}

impl CdrColorModel {
    /// Maps a numeric Corel color-model ID into [`CdrColorModel`].
    #[must_use]
    pub const fn from_u16(raw: u16) -> Option<Self> {
        match raw {
            2 => Some(Self::Cmyk100),
            3 | 17 => Some(Self::Cmyk255),
            4 => Some(Self::Rgb),
            5 => Some(Self::Bgr),
            6 => Some(Self::Hsl),
            9 => Some(Self::Gray),
            12 => Some(Self::Lab),
            14 | 1 => Some(Self::Spot),
            20 => Some(Self::Rgba),
            21 => Some(Self::Cmyka255),
            _ => None,
        }
    }

    /// Human-readable color model name for inspection reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cmyk100 => "CMYK100",
            Self::Cmyk255 | Self::Cmyka255 => "CMYK255",
            Self::Rgb | Self::Rgba => "RGB",
            Self::Bgr => "BGR",
            Self::Hsl => "HSL",
            Self::Gray => "Gray",
            Self::Lab => "Lab",
            Self::Spot => "Spot",
        }
    }
}

/// Returns a display label for the color model of a [`Color`].
#[must_use]
pub const fn color_model_name(color: &Color) -> &'static str {
    match color {
        Color::Rgba { .. } => "RGB",
        Color::Cmyk { .. } => "CMYK",
        Color::Hsl { .. } => "HSL",
        Color::Lab { .. } => "Lab",
        Color::Gray { .. } => "Gray",
        Color::Spot { .. } => "Spot",
    }
}

/// Decodes a Corel binary color record (`model_id` + payload bytes) into a [`Color`].
pub fn decode_cdr_color(
    model_id: u16,
    raw: &[u8],
    spot_name_override: Option<&str>,
) -> CdrResult<Color> {
    let mut r = ByteReader::new(raw);
    let Some(model) = CdrColorModel::from_u16(model_id) else {
        // Fallback: interpret first 4 bytes as CMYK255 or RGB if unrecognized model ID
        if raw.len() >= 4 {
            return Ok(Color::cmyk(
                f32::from(raw[0]) / 255.0,
                f32::from(raw[1]) / 255.0,
                f32::from(raw[2]) / 255.0,
                f32::from(raw[3]) / 255.0,
            ));
        }
        return Ok(Color::BLACK);
    };

    match model {
        CdrColorModel::Cmyk100 => {
            let c = f32::from(r.read_u8("CMYK100 C")?.min(100)) / 100.0;
            let m = f32::from(r.read_u8("CMYK100 M")?.min(100)) / 100.0;
            let y = f32::from(r.read_u8("CMYK100 Y")?.min(100)) / 100.0;
            let k = f32::from(r.read_u8("CMYK100 K")?.min(100)) / 100.0;
            Ok(Color::cmyk(c, m, y, k))
        }
        CdrColorModel::Cmyk255 => {
            let c = f32::from(r.read_u8("CMYK255 C")?) / 255.0;
            let m = f32::from(r.read_u8("CMYK255 M")?) / 255.0;
            let y = f32::from(r.read_u8("CMYK255 Y")?) / 255.0;
            let k = f32::from(r.read_u8("CMYK255 K")?) / 255.0;
            Ok(Color::cmyk(c, m, y, k))
        }
        CdrColorModel::Cmyka255 => {
            let c = f32::from(r.read_u8("CMYKA C")?) / 255.0;
            let m = f32::from(r.read_u8("CMYKA M")?) / 255.0;
            let y = f32::from(r.read_u8("CMYKA Y")?) / 255.0;
            let k = f32::from(r.read_u8("CMYKA K")?) / 255.0;
            let a = if r.remaining() >= 1 {
                f32::from(r.read_u8("CMYKA A")?) / 255.0
            } else {
                1.0
            };
            Ok(Color::Cmyk { c, m, y, k, a })
        }
        CdrColorModel::Rgb => {
            let red = r.read_u8("RGB R")?;
            let green = r.read_u8("RGB G")?;
            let blue = r.read_u8("RGB B")?;
            Ok(Color::rgba8(red, green, blue, 255))
        }
        CdrColorModel::Bgr => {
            let blue = r.read_u8("BGR B")?;
            let green = r.read_u8("BGR G")?;
            let red = r.read_u8("BGR R")?;
            Ok(Color::rgba8(red, green, blue, 255))
        }
        CdrColorModel::Rgba => {
            let red = r.read_u8("RGBA R")?;
            let green = r.read_u8("RGBA G")?;
            let blue = r.read_u8("RGBA B")?;
            let alpha = if r.remaining() >= 1 {
                r.read_u8("RGBA A")?
            } else {
                255
            };
            Ok(Color::rgba8(red, green, blue, alpha))
        }
        CdrColorModel::Hsl => {
            let h = f32::from(r.read_u16_le("HSL H")? % 360);
            let s = f32::from(r.read_u8("HSL S")?) / 255.0;
            let l = f32::from(r.read_u8("HSL L")?) / 255.0;
            let a = if r.remaining() >= 1 {
                f32::from(r.read_u8("HSL A")?) / 255.0
            } else {
                1.0
            };
            Ok(Color::Hsl { h, s, l, a })
        }
        CdrColorModel::Gray => {
            let v = f32::from(r.read_u8("Gray V")?) / 255.0;
            let a = if r.remaining() >= 1 {
                f32::from(r.read_u8("Gray A")?) / 255.0
            } else {
                1.0
            };
            Ok(Color::Gray { v, a })
        }
        CdrColorModel::Lab => {
            let l_raw = r.read_u8("Lab L")?;
            let a_ch = f32::from(r.read_i8("Lab a*")?);
            let b_ch = f32::from(r.read_i8("Lab b*")?);
            let alpha = if r.remaining() >= 1 {
                f32::from(r.read_u8("Lab A")?) / 255.0
            } else {
                1.0
            };
            // Standard Corel Lab stores L* in 0..=100; clamp safely if > 100
            let l = if l_raw <= 100 {
                f32::from(l_raw)
            } else {
                (f32::from(l_raw) / 255.0) * 100.0
            };
            Ok(Color::Lab {
                l,
                a_ch,
                b_ch,
                alpha,
            })
        }
        CdrColorModel::Spot => {
            let tint = f32::from(r.read_u8("Spot tint")?.min(100)) / 100.0;
            let c = f32::from(r.read_u8("Spot fallback C")?) / 255.0;
            let m = f32::from(r.read_u8("Spot fallback M")?) / 255.0;
            let y = f32::from(r.read_u8("Spot fallback Y")?) / 255.0;
            let k = f32::from(r.read_u8("Spot fallback K")?) / 255.0;
            let alpha = if r.remaining() >= 1 {
                f32::from(r.read_u8("Spot alpha")?) / 255.0
            } else {
                1.0
            };
            let name = if let Some(ov) = spot_name_override {
                ov.to_owned()
            } else if r.remaining() >= 2 {
                let name_len = usize::from(r.read_u16_le("Spot name len")?);
                if name_len <= r.remaining() {
                    let nb = r.read_bytes(name_len, "Spot name")?;
                    decode_cdr_text(nb, Some(CODEPAGE_UTF8), None)
                } else {
                    "Spot".to_owned()
                }
            } else {
                "Spot".to_owned()
            };
            Ok(Color::Spot {
                name,
                tint,
                fallback_cmyk: [c, m, y, k],
                alpha,
            })
        }
    }
}

/// Encodes a [`Color`] into a `(CdrColorModel u16, payload bytes)` tuple.
#[must_use]
pub fn encode_cdr_color(color: &Color) -> (u16, Vec<u8>) {
    match color {
        Color::Cmyk { c, m, y, k, a } => {
            let cb = (sanitize_f32(*c, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let mb = (sanitize_f32(*m, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let yb = (sanitize_f32(*y, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let kb = (sanitize_f32(*k, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let ab = (sanitize_f32(*a, 1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            if ab == 255 {
                (CdrColorModel::Cmyk255 as u16, vec![cb, mb, yb, kb])
            } else {
                (CdrColorModel::Cmyka255 as u16, vec![cb, mb, yb, kb, ab])
            }
        }
        Color::Rgba { r, g, b, a } => {
            let rb = (sanitize_f32(*r, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let gb = (sanitize_f32(*g, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let bb = (sanitize_f32(*b, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let ab = (sanitize_f32(*a, 1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            if ab == 255 {
                (CdrColorModel::Bgr as u16, vec![bb, gb, rb, 0])
            } else {
                (CdrColorModel::Rgba as u16, vec![rb, gb, bb, ab])
            }
        }
        Color::Hsl { h, s, l, a } => {
            let hv = (sanitize_f32(*h, 0.0).rem_euclid(360.0)).round() as u16 % 360;
            let sb = (sanitize_f32(*s, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let lb = (sanitize_f32(*l, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let ab = (sanitize_f32(*a, 1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let mut out = Vec::with_capacity(5);
            out.extend_from_slice(&hv.to_le_bytes());
            out.push(sb);
            out.push(lb);
            out.push(ab);
            (CdrColorModel::Hsl as u16, out)
        }
        Color::Gray { v, a } => {
            let vb = (sanitize_f32(*v, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let ab = (sanitize_f32(*a, 1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            (CdrColorModel::Gray as u16, vec![vb, ab])
        }
        Color::Lab {
            l,
            a_ch,
            b_ch,
            alpha,
        } => {
            let lb = sanitize_f32(*l, 0.0).clamp(0.0, 100.0).round() as u8;
            let ab_ch = sanitize_f32(*a_ch, 0.0).clamp(-128.0, 127.0).round() as i8;
            let bb_ch = sanitize_f32(*b_ch, 0.0).clamp(-128.0, 127.0).round() as i8;
            let al = (sanitize_f32(*alpha, 1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            (
                CdrColorModel::Lab as u16,
                vec![lb, ab_ch as u8, bb_ch as u8, al],
            )
        }
        Color::Spot {
            name,
            tint,
            fallback_cmyk,
            alpha,
        } => {
            let tb = (sanitize_f32(*tint, 1.0).clamp(0.0, 1.0) * 100.0).round() as u8;
            let c = (sanitize_f32(fallback_cmyk[0], 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let m = (sanitize_f32(fallback_cmyk[1], 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let y = (sanitize_f32(fallback_cmyk[2], 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let k = (sanitize_f32(fallback_cmyk[3], 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let al = (sanitize_f32(*alpha, 1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let nb = name.as_bytes();
            let nlen = nb.len().min(u16::MAX as usize);
            let mut out = Vec::with_capacity(8 + nlen);
            out.extend_from_slice(&[tb, c, m, y, k, al]);
            out.extend_from_slice(&(nlen as u16).to_le_bytes());
            out.extend_from_slice(&nb[..nlen]);
            (CdrColorModel::Spot as u16, out)
        }
    }
}

/// Writes a length-prefixed color record (`u16 model_id`, `u16 payload_len`, `payload`) into `out`.
pub fn write_tagged_color(out: &mut Vec<u8>, color: &Color) {
    write_tagged_color_with_endian(out, color, RiffEndian::Little);
}

/// Writes a length-prefixed color record using explicit RIFF/RIFX byte order.
pub(crate) fn write_tagged_color_with_endian(out: &mut Vec<u8>, color: &Color, endian: RiffEndian) {
    let (model, payload) = encode_cdr_color(color);
    let len = payload.len().min(u16::MAX as usize) as u16;
    endian.write_u16(out, model);
    endian.write_u16(out, len);
    out.extend_from_slice(&payload[..usize::from(len)]);
}

/// Reads a length-prefixed color record (`u16 model_id`, `u16 payload_len`, `payload`).
pub fn read_tagged_color(reader: &mut ByteReader<'_>, ctx: &'static str) -> CdrResult<Color> {
    read_tagged_color_with_endian(reader, RiffEndian::Little, ctx)
}

/// Reads a length-prefixed color record using explicit RIFF/RIFX byte order.
pub(crate) fn read_tagged_color_with_endian(
    reader: &mut ByteReader<'_>,
    endian: RiffEndian,
    ctx: &'static str,
) -> CdrResult<Color> {
    let model = endian.read_u16(reader, ctx)?;
    let len = usize::from(endian.read_u16(reader, ctx)?);
    let payload = reader.read_bytes(len, ctx)?;
    decode_cdr_color(model, payload, None)
}

// =============================================================================
// Text Encoding: Arabic Windows-1256 (CP1256), UTF-16LE, UTF-8
// =============================================================================

/// Returns `true` if `text` contains any Arabic Unicode codepoint (`U+0600..=U+06FF`,
/// Supplement, Extended-A, or Presentation Forms).
#[must_use]
pub fn contains_arabic(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(
            ch as u32,
            0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF
        )
    })
}

/// Encodes `text` into Windows-1256 (Arabic CP1256) bytes via `encoding_rs`.
/// Returns `(bytes, had_unmappable_chars)`.
#[must_use]
pub fn encode_cp1256(text: &str) -> (Vec<u8>, bool) {
    let (cow, _, had_errors) = encoding_rs::WINDOWS_1256.encode(text);
    (cow.into_owned(), had_errors)
}

/// Encodes `text` into UTF-16LE bytes (without BOM).
#[must_use]
pub fn encode_utf16le(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len().saturating_mul(2));
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// Decodes Corel text bytes with full support for Arabic Windows-1256 (`CP1256`),
/// Windows `ARABIC_CHARSET` (`178`), UTF-16LE, UTF-8, and heuristic CP1256 auto-detection.
#[must_use]
pub fn decode_cdr_text(raw: &[u8], codepage_hint: Option<u16>, charset_hint: Option<u8>) -> String {
    if raw.is_empty() {
        return String::new();
    }

    // 1. Explicit UTF-16LE hint or BOM
    if codepage_hint == Some(CODEPAGE_UTF16LE)
        || (raw.len() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE)
    {
        let slice = if raw.len() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE {
            &raw[2..]
        } else {
            raw
        };
        return decode_utf16le_trimmed(slice);
    }

    // 2. Explicit UTF-8 hint or BOM
    if codepage_hint == Some(CODEPAGE_UTF8)
        || (raw.len() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF)
    {
        let slice = if raw.len() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF {
            &raw[3..]
        } else {
            raw
        };
        let trimmed = trim_trailing_nul_bytes(slice);
        return String::from_utf8_lossy(trimmed).into_owned();
    }

    // 3. Explicit Windows-1256 Arabic codepage or Windows ARABIC_CHARSET (178)
    if codepage_hint == Some(CODEPAGE_ARABIC_CP1256) || charset_hint == Some(CHARSET_ARABIC) {
        let trimmed = trim_trailing_nul_bytes(raw);
        let (cow, _, _) = encoding_rs::WINDOWS_1256.decode(trimmed);
        return cow.into_owned();
    }

    // 4. Explicit Windows-1252 Latin-1 codepage
    if codepage_hint == Some(CODEPAGE_LATIN1_CP1252) {
        let trimmed = trim_trailing_nul_bytes(raw);
        let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(trimmed);
        return cow.into_owned();
    }

    // 5. Auto-detect UTF-16LE without BOM (even length >= 4 with alternating 0x00 or 0x06 high bytes)
    if looks_like_utf16le(raw) {
        return decode_utf16le_trimmed(raw);
    }

    let trimmed = trim_trailing_nul_bytes(raw);
    if trimmed.is_empty() {
        return String::new();
    }

    // 6. Pure ASCII or valid multi-byte UTF-8
    if let Ok(s) = std::str::from_utf8(trimmed) {
        return s.to_owned();
    }

    // 7. Heuristic Arabic CP1256 auto-detection for legacy 8-bit `txsm`/`stlt` chunks
    if looks_like_cp1256_arabic(trimmed) {
        let (cow, _, _) = encoding_rs::WINDOWS_1256.decode(trimmed);
        return cow.into_owned();
    }

    // 8. Legacy Western fallback (Windows-1252)
    let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(trimmed);
    cow.into_owned()
}

fn trim_trailing_nul_bytes(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    &bytes[..end]
}

fn decode_utf16le_trimmed(bytes: &[u8]) -> String {
    let even_len = bytes.len() - (bytes.len() % 2);
    let mut slice = &bytes[..even_len];
    while slice.len() >= 2 && slice[slice.len() - 2] == 0 && slice[slice.len() - 1] == 0 {
        slice = &slice[..slice.len() - 2];
    }
    let (cow, _, _) = encoding_rs::UTF_16LE.decode(slice);
    cow.into_owned()
}

fn looks_like_utf16le(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || !bytes.len().is_multiple_of(2) {
        return false;
    }
    let pairs = bytes.len() / 2;
    let mut nul_hi = 0usize;
    let mut arabic_hi = 0usize;
    let mut ascii_lo_with_nul_hi = 0usize;
    let (chunks, _) = bytes.as_chunks::<2>();
    for chunk in chunks {
        let lo = chunk[0];
        let hi = chunk[1];
        if hi == 0x00 {
            nul_hi += 1;
            if (0x20..=0x7E).contains(&lo) || lo == b'\n' || lo == b'\r' || lo == b'\t' {
                ascii_lo_with_nul_hi += 1;
            }
        } else if hi == 0x06 {
            arabic_hi += 1;
        }
    }
    // Strong UTF-16LE signal: mostly 0x00 (Basic Latin) or 0x06 (Arabic block) high bytes
    (ascii_lo_with_nul_hi + arabic_hi) * 4 >= pairs * 3 && (nul_hi > 0 || arabic_hi >= 2)
}

fn looks_like_cp1256_arabic(bytes: &[u8]) -> bool {
    let mut high_total = 0usize;
    let mut arabic_hits = 0usize;
    for &b in bytes {
        if b >= 0x80 {
            high_total += 1;
            // Core Windows-1256 Arabic letters (Hamza..Ghain, Feh..Yeh, Kashida, diacritics)
            if matches!(b, 0xC1..=0xDA | 0xDC..=0xDF | 0xE1 | 0xE3..=0xE6 | 0xEC..=0xED | 0xF0..=0xFA)
            {
                arabic_hits += 1;
            }
        }
    }
    high_total >= 2 && arabic_hits * 2 >= high_total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinate_conversions_roundtrip_accurately() {
        assert!((cdr32_to_mm(10_000) - 1.0).abs() < 1e-12);
        assert_eq!(mm_to_cdr32(210.0), 2_100_000);
        assert!((cmx16_to_mm(1000, CmxCoordUnit::MilliInch) - 25.4).abs() < 1e-9);
        assert_eq!(mm_to_cmx16(25.4, CmxCoordUnit::MilliInch), 1000);

        let pt = coree_core::Point2D::new(35.0, 80.0);
        let (cx, cy) = page_mm_to_cdr_point(pt, 210.0, 297.0);
        let back = cdr_point_to_page_mm(cx, cy, 210.0, 297.0);
        assert!((back.x - pt.x).abs() < 1e-3);
        assert!((back.y - pt.y).abs() < 1e-3);
    }

    #[test]
    fn all_color_models_decode_and_roundtrip() {
        let colors = [
            Color::cmyk(0.2, 0.4, 0.6, 0.1),
            Color::rgb(0.25, 0.5, 0.75),
            Color::rgba(0.1, 0.9, 0.3, 0.5),
            Color::gray(0.6),
            Color::lab(55.0, 24.0, -32.0),
            Color::spot("CutContour", 1.0, [0.0, 1.0, 0.0, 0.0]),
        ];
        for c in &colors {
            let (model, payload) = encode_cdr_color(c);
            let decoded = decode_cdr_color(model, &payload, None).unwrap();
            let a = c.to_rgba_f32();
            let b = decoded.to_rgba_f32();
            for i in 0..4 {
                assert!(
                    (a[i] - b[i]).abs() < 0.02,
                    "color mismatch: {c:?} vs {decoded:?}"
                );
            }
        }
        // Explicit CMYK100 decode
        let cmyk100 =
            decode_cdr_color(CdrColorModel::Cmyk100 as u16, &[100, 50, 0, 25], None).unwrap();
        assert_eq!(cmyk100.to_cmyk(), [1.0, 0.5, 0.0, 0.25]);
    }

    #[test]
    fn cp1256_arabic_explicit_and_autodetected_decoding() {
        let sample = "مرحبا بكم في كوري";
        let (cp1256_bytes, had_err) = encode_cp1256(sample);
        assert!(!had_err);
        // Explicit codepage 1256
        assert_eq!(
            decode_cdr_text(&cp1256_bytes, Some(CODEPAGE_ARABIC_CP1256), None),
            sample
        );
        // Explicit Windows ARABIC_CHARSET = 178
        assert_eq!(
            decode_cdr_text(&cp1256_bytes, None, Some(CHARSET_ARABIC)),
            sample
        );
        // Heuristic auto-detection with no codepage hint!
        assert_eq!(decode_cdr_text(&cp1256_bytes, None, None), sample);
        // UTF-16LE Arabic round-trip (auto-detected without BOM)
        let utf16 = encode_utf16le(sample);
        assert_eq!(decode_cdr_text(&utf16, None, None), sample);
    }
}
