// SPDX-License-Identifier: MPL-2.0

//! Color spaces, conversions, harmonies, and clean-room procedural palettes.
//!
//! All built-in palettes are generated mathematically from CMYK and HSL grids.
//! No proprietary or licensed third-party color books are included.

use crate::error::{CoreError, CoreResult};
use crate::math::sanitize_f32;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Primary working color model of a document or palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ColorMode {
    #[default]
    Cmyk,
    Rgb,
}

/// Multi-space color representation supporting screen, print, perceptual, and spot colors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Color {
    /// sRGB + alpha, each channel normalized to `0.0..=1.0`.
    Rgba { r: f32, g: f32, b: f32, a: f32 },
    /// Print process CMYK + alpha, each channel normalized to `0.0..=1.0`.
    Cmyk {
        c: f32,
        m: f32,
        y: f32,
        k: f32,
        a: f32,
    },
    /// Cylindrical HSL + alpha (`h` in degrees `0.0..360.0`, `s`/`l`/`a` in `0.0..=1.0`).
    Hsl { h: f32, s: f32, l: f32, a: f32 },
    /// CIE L*a*b* (D65 illuminant) + alpha (`l` in `0.0..=100.0`, `a_ch`/`b_ch` in `-128.0..=127.0`).
    Lab {
        l: f32,
        a_ch: f32,
        b_ch: f32,
        alpha: f32,
    },
    /// Luminance grayscale (`v = 0.0` black .. `1.0` white) + alpha.
    Gray { v: f32, a: f32 },
    /// Named spot ink (e.g. `CutContour`, `Crease`, metallic/foil plate) with tint and CMYK fallback.
    Spot {
        name: String,
        tint: f32,
        fallback_cmyk: [f32; 4],
        alpha: f32,
    },
}

impl Default for Color {
    fn default() -> Self {
        Self::BLACK
    }
}

#[inline]
fn clamp01(v: f32) -> f32 {
    sanitize_f32(v, 0.0).clamp(0.0, 1.0)
}

#[inline]
fn srgb_to_linear(c: f32) -> f64 {
    let c = f64::from(clamp01(c));
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn linear_to_srgb(c: f64) -> f32 {
    let c = c.clamp(0.0, 1.0);
    let out = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    out.clamp(0.0, 1.0) as f32
}

fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> [f32; 3] {
    let c = clamp01(c);
    let m = clamp01(m);
    let y = clamp01(y);
    let k = clamp01(k);
    let inv_k = 1.0 - k;
    [(1.0 - c) * inv_k, (1.0 - m) * inv_k, (1.0 - y) * inv_k]
}

fn rgb_to_cmyk(r: f32, g: f32, b: f32) -> [f32; 4] {
    let r = clamp01(r);
    let g = clamp01(g);
    let b = clamp01(b);
    let max_rgb = r.max(g).max(b);
    let k = 1.0 - max_rgb;
    if max_rgb <= 1e-6 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let c = ((1.0 - r - k) / max_rgb).clamp(0.0, 1.0);
    let m = ((1.0 - g - k) / max_rgb).clamp(0.0, 1.0);
    let y = ((1.0 - b - k) / max_rgb).clamp(0.0, 1.0);
    [c, m, y, k.clamp(0.0, 1.0)]
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    let h = sanitize_f32(h, 0.0).rem_euclid(360.0);
    let s = clamp01(s);
    let l = clamp01(l);
    if s <= 1e-6 {
        return [l, l, l];
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c * 0.5;
    let (r1, g1, b1) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    [
        (r1 + m).clamp(0.0, 1.0),
        (g1 + m).clamp(0.0, 1.0),
        (b1 + m).clamp(0.0, 1.0),
    ]
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> [f32; 3] {
    let r = clamp01(r);
    let g = clamp01(g);
    let b = clamp01(b);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let d = max - min;
    if d <= 1e-6 {
        return [0.0, 0.0, l];
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let mut h = if (max - r).abs() <= 1e-6 {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if (max - g).abs() <= 1e-6 {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    h *= 60.0;
    [h.rem_euclid(360.0), s.clamp(0.0, 1.0), l.clamp(0.0, 1.0)]
}

// CIE D65 reference white
const D65_X: f64 = 0.950_47;
const D65_Y: f64 = 1.000_00;
const D65_Z: f64 = 1.088_83;
const LAB_EPSILON: f64 = 216.0 / 24_389.0;
const LAB_KAPPA: f64 = 24_389.0 / 27.0;

fn lab_f(t: f64) -> f64 {
    if t > LAB_EPSILON {
        t.cbrt()
    } else {
        (LAB_KAPPA * t + 16.0) / 116.0
    }
}

fn lab_f_inv(t: f64) -> f64 {
    let t3 = t * t * t;
    if t3 > LAB_EPSILON {
        t3
    } else {
        (116.0 * t - 16.0) / LAB_KAPPA
    }
}

fn rgb_to_lab(r: f32, g: f32, b: f32) -> [f32; 3] {
    let rl = srgb_to_linear(r);
    let gl = srgb_to_linear(g);
    let bl = srgb_to_linear(b);

    let x = (0.412_456_4 * rl + 0.357_576_1 * gl + 0.180_437_5 * bl) / D65_X;
    let y = (0.212_672_9 * rl + 0.715_152_2 * gl + 0.072_175_0 * bl) / D65_Y;
    let z = (0.019_333_9 * rl + 0.119_192_0 * gl + 0.950_304_1 * bl) / D65_Z;

    let fx = lab_f(x);
    let fy = lab_f(y);
    let fz = lab_f(z);

    let l = (116.0 * fy - 16.0).clamp(0.0, 100.0);
    let a = 500.0 * (fx - fy);
    let b_ch = 200.0 * (fy - fz);
    [l as f32, a as f32, b_ch as f32]
}

fn lab_to_rgb(l: f32, a_ch: f32, b_ch: f32) -> [f32; 3] {
    let l = f64::from(sanitize_f32(l, 0.0).clamp(0.0, 100.0));
    let a = f64::from(sanitize_f32(a_ch, 0.0));
    let b = f64::from(sanitize_f32(b_ch, 0.0));

    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;

    let x = lab_f_inv(fx) * D65_X;
    let y = if l > LAB_KAPPA * LAB_EPSILON {
        fy * fy * fy * D65_Y
    } else {
        (l / LAB_KAPPA) * D65_Y
    };
    let z = lab_f_inv(fz) * D65_Z;

    let rl = 3.240_454_2 * x - 1.537_138_5 * y - 0.498_531_4 * z;
    let gl = -0.969_266_0 * x + 1.876_010_8 * y + 0.041_556_0 * z;
    let bl = 0.055_643_4 * x - 0.204_025_9 * y + 1.057_225_2 * z;

    [linear_to_srgb(rl), linear_to_srgb(gl), linear_to_srgb(bl)]
}

impl Color {
    /// Solid opaque black (`#000000`).
    pub const BLACK: Self = Self::Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    /// Solid opaque white (`#FFFFFF`).
    pub const WHITE: Self = Self::Rgba {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };

    /// Fully transparent black.
    pub const TRANSPARENT: Self = Self::Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    /// Process Cyan (`C=100%`).
    pub const CYAN: Self = Self::Cmyk {
        c: 1.0,
        m: 0.0,
        y: 0.0,
        k: 0.0,
        a: 1.0,
    };

    /// Process Magenta (`M=100%`).
    pub const MAGENTA: Self = Self::Cmyk {
        c: 0.0,
        m: 1.0,
        y: 0.0,
        k: 0.0,
        a: 1.0,
    };

    /// Process Yellow (`Y=100%`).
    pub const YELLOW: Self = Self::Cmyk {
        c: 0.0,
        m: 0.0,
        y: 1.0,
        k: 0.0,
        a: 1.0,
    };

    /// Process Key Black (`K=100%`).
    pub const KEY_BLACK: Self = Self::Cmyk {
        c: 0.0,
        m: 0.0,
        y: 0.0,
        k: 1.0,
        a: 1.0,
    };

    /// Creates an opaque sRGB color.
    #[must_use]
    pub fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self::Rgba {
            r: clamp01(r),
            g: clamp01(g),
            b: clamp01(b),
            a: 1.0,
        }
    }

    /// Creates an sRGB color with explicit alpha.
    #[must_use]
    pub fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self::Rgba {
            r: clamp01(r),
            g: clamp01(g),
            b: clamp01(b),
            a: clamp01(a),
        }
    }

    /// Creates an sRGB color from 8-bit `0..=255` channels.
    #[must_use]
    pub fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::Rgba {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: f32::from(a) / 255.0,
        }
    }

    /// Creates an opaque process CMYK color.
    #[must_use]
    pub fn cmyk(c: f32, m: f32, y: f32, k: f32) -> Self {
        Self::Cmyk {
            c: clamp01(c),
            m: clamp01(m),
            y: clamp01(y),
            k: clamp01(k),
            a: 1.0,
        }
    }

    /// Creates an opaque HSL color (`h` in degrees `0..360`).
    #[must_use]
    pub fn hsl(h: f32, s: f32, l: f32) -> Self {
        Self::Hsl {
            h: sanitize_f32(h, 0.0).rem_euclid(360.0),
            s: clamp01(s),
            l: clamp01(l),
            a: 1.0,
        }
    }

    /// Creates an opaque CIE L*a*b* color.
    #[must_use]
    pub fn lab(l: f32, a_ch: f32, b_ch: f32) -> Self {
        Self::Lab {
            l: sanitize_f32(l, 0.0).clamp(0.0, 100.0),
            a_ch: sanitize_f32(a_ch, 0.0),
            b_ch: sanitize_f32(b_ch, 0.0),
            alpha: 1.0,
        }
    }

    /// Creates an opaque grayscale color (`0.0` black .. `1.0` white).
    #[must_use]
    pub fn gray(v: f32) -> Self {
        Self::Gray {
            v: clamp01(v),
            a: 1.0,
        }
    }

    /// Creates a named spot ink color (such as `"CutContour"`).
    #[must_use]
    pub fn spot(name: impl Into<String>, tint: f32, fallback_cmyk: [f32; 4]) -> Self {
        Self::Spot {
            name: name.into(),
            tint: clamp01(tint),
            fallback_cmyk: [
                clamp01(fallback_cmyk[0]),
                clamp01(fallback_cmyk[1]),
                clamp01(fallback_cmyk[2]),
                clamp01(fallback_cmyk[3]),
            ],
            alpha: 1.0,
        }
    }

    /// Standard print/cutter `"CutContour"` spot ink (100% magenta fallback).
    #[must_use]
    pub fn cut_contour_spot() -> Self {
        Self::spot("CutContour", 1.0, [0.0, 1.0, 0.0, 0.0])
    }

    /// Returns the alpha channel in `0.0..=1.0`.
    #[must_use]
    pub fn alpha(&self) -> f32 {
        match *self {
            Self::Rgba { a, .. }
            | Self::Cmyk { a, .. }
            | Self::Hsl { a, .. }
            | Self::Gray { a, .. } => clamp01(a),
            Self::Lab { alpha, .. } | Self::Spot { alpha, .. } => clamp01(alpha),
        }
    }

    /// Returns a clone of this color with its alpha replaced by `new_alpha`.
    #[must_use]
    pub fn with_alpha(&self, new_alpha: f32) -> Self {
        let a = clamp01(new_alpha);
        match self.clone() {
            Self::Rgba { r, g, b, .. } => Self::Rgba { r, g, b, a },
            Self::Cmyk { c, m, y, k, .. } => Self::Cmyk { c, m, y, k, a },
            Self::Hsl { h, s, l, .. } => Self::Hsl { h, s, l, a },
            Self::Lab { l, a_ch, b_ch, .. } => Self::Lab {
                l,
                a_ch,
                b_ch,
                alpha: a,
            },
            Self::Gray { v, .. } => Self::Gray { v, a },
            Self::Spot {
                name,
                tint,
                fallback_cmyk,
                ..
            } => Self::Spot {
                name,
                tint,
                fallback_cmyk,
                alpha: a,
            },
        }
    }

    /// Converts any color variant into normalized `[r, g, b, a]` in `0.0..=1.0`.
    #[must_use]
    pub fn to_rgba_f32(&self) -> [f32; 4] {
        match *self {
            Self::Rgba { r, g, b, a } => [clamp01(r), clamp01(g), clamp01(b), clamp01(a)],
            Self::Cmyk { c, m, y, k, a } => {
                let [r, g, b] = cmyk_to_rgb(c, m, y, k);
                [r, g, b, clamp01(a)]
            }
            Self::Hsl { h, s, l, a } => {
                let [r, g, b] = hsl_to_rgb(h, s, l);
                [r, g, b, clamp01(a)]
            }
            Self::Lab {
                l,
                a_ch,
                b_ch,
                alpha,
            } => {
                let [r, g, b] = lab_to_rgb(l, a_ch, b_ch);
                [r, g, b, clamp01(alpha)]
            }
            Self::Gray { v, a } => {
                let v = clamp01(v);
                [v, v, v, clamp01(a)]
            }
            Self::Spot {
                tint,
                fallback_cmyk,
                alpha,
                ..
            } => {
                let t = clamp01(tint);
                let [r, g, b] = cmyk_to_rgb(
                    fallback_cmyk[0] * t,
                    fallback_cmyk[1] * t,
                    fallback_cmyk[2] * t,
                    fallback_cmyk[3] * t,
                );
                [r, g, b, clamp01(alpha)]
            }
        }
    }

    /// Converts any color variant into 8-bit `[r, g, b, a]` (`0..=255`).
    #[must_use]
    pub fn to_rgba8(&self) -> [u8; 4] {
        let [r, g, b, a] = self.to_rgba_f32();
        [
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8,
            (a * 255.0).round() as u8,
        ]
    }

    /// Converts any color variant into process CMYK `[c, m, y, k]` (`0.0..=1.0`).
    #[must_use]
    pub fn to_cmyk(&self) -> [f32; 4] {
        match *self {
            Self::Cmyk { c, m, y, k, .. } => [clamp01(c), clamp01(m), clamp01(y), clamp01(k)],
            Self::Spot {
                tint,
                fallback_cmyk,
                ..
            } => {
                let t = clamp01(tint);
                [
                    clamp01(fallback_cmyk[0] * t),
                    clamp01(fallback_cmyk[1] * t),
                    clamp01(fallback_cmyk[2] * t),
                    clamp01(fallback_cmyk[3] * t),
                ]
            }
            Self::Gray { v, .. } => [0.0, 0.0, 0.0, 1.0 - clamp01(v)],
            _ => {
                let [r, g, b, _] = self.to_rgba_f32();
                rgb_to_cmyk(r, g, b)
            }
        }
    }

    /// Converts this color into a [`Color::Cmyk`] variant preserving alpha.
    #[must_use]
    pub fn as_cmyk_color(&self) -> Self {
        let [c, m, y, k] = self.to_cmyk();
        Self::Cmyk {
            c,
            m,
            y,
            k,
            a: self.alpha(),
        }
    }

    /// Converts any color variant into CIE L*a*b* `[l, a, b]` (D65).
    #[must_use]
    pub fn to_lab(&self) -> [f32; 3] {
        if let Self::Lab { l, a_ch, b_ch, .. } = *self {
            return [
                sanitize_f32(l, 0.0).clamp(0.0, 100.0),
                sanitize_f32(a_ch, 0.0),
                sanitize_f32(b_ch, 0.0),
            ];
        }
        let [r, g, b, _] = self.to_rgba_f32();
        rgb_to_lab(r, g, b)
    }

    /// Converts this color into a [`Color::Lab`] variant preserving alpha.
    #[must_use]
    pub fn as_lab_color(&self) -> Self {
        let [l, a_ch, b_ch] = self.to_lab();
        Self::Lab {
            l,
            a_ch,
            b_ch,
            alpha: self.alpha(),
        }
    }

    /// Converts any color variant into `[h_deg, s, l]`.
    #[must_use]
    pub fn to_hsl(&self) -> [f32; 3] {
        if let Self::Hsl { h, s, l, .. } = *self {
            return [
                sanitize_f32(h, 0.0).rem_euclid(360.0),
                clamp01(s),
                clamp01(l),
            ];
        }
        let [r, g, b, _] = self.to_rgba_f32();
        rgb_to_hsl(r, g, b)
    }

    /// Converts this color into a [`Color::Hsl`] variant preserving alpha.
    #[must_use]
    pub fn as_hsl_color(&self) -> Self {
        let [h, s, l] = self.to_hsl();
        Self::Hsl {
            h,
            s,
            l,
            a: self.alpha(),
        }
    }

    /// Parses `#RGB`, `#RGBA`, `#RRGGBB`, or `#RRGGBBAA` (leading `#` optional).
    pub fn from_hex(hex: &str) -> CoreResult<Self> {
        let trimmed = hex.trim();
        let s = trimmed.strip_prefix('#').unwrap_or(trimmed);
        if !s.is_ascii() {
            return Err(CoreError::InvalidColor(format!(
                "non-ASCII characters in hex color '{hex}'"
            )));
        }
        let parse_byte = |slice: &str| -> CoreResult<u8> {
            u8::from_str_radix(slice, 16)
                .map_err(|_| CoreError::InvalidColor(format!("invalid hex color '{hex}'")))
        };
        let parse_nibble = |ch: &str| -> CoreResult<u8> {
            let n = u8::from_str_radix(ch, 16)
                .map_err(|_| CoreError::InvalidColor(format!("invalid hex color '{hex}'")))?;
            Ok((n << 4) | n)
        };
        match s.len() {
            3 => Ok(Self::rgba8(
                parse_nibble(&s[0..1])?,
                parse_nibble(&s[1..2])?,
                parse_nibble(&s[2..3])?,
                255,
            )),
            4 => Ok(Self::rgba8(
                parse_nibble(&s[0..1])?,
                parse_nibble(&s[1..2])?,
                parse_nibble(&s[2..3])?,
                parse_nibble(&s[3..4])?,
            )),
            6 => Ok(Self::rgba8(
                parse_byte(&s[0..2])?,
                parse_byte(&s[2..4])?,
                parse_byte(&s[4..6])?,
                255,
            )),
            8 => Ok(Self::rgba8(
                parse_byte(&s[0..2])?,
                parse_byte(&s[2..4])?,
                parse_byte(&s[4..6])?,
                parse_byte(&s[6..8])?,
            )),
            _ => Err(CoreError::InvalidColor(format!(
                "expected 3, 4, 6, or 8 hex digits in '{hex}'"
            ))),
        }
    }

    /// Formats as `#RRGGBB` when opaque, or `#RRGGBBAA` when alpha `< 255`.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let [r, g, b, a] = self.to_rgba8();
        if a == 255 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }

    /// Always formats as `#RRGGBBAA`.
    #[must_use]
    pub fn to_hex_rgba(&self) -> String {
        let [r, g, b, a] = self.to_rgba8();
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }

    /// Linearly interpolates between `self` (`t = 0.0`) and `other` (`t = 1.0`).
    ///
    /// If both inputs are `Color::Cmyk`, interpolation is performed directly in CMYK space;
    /// otherwise interpolation is performed in normalized RGBA space.
    #[must_use]
    pub fn lerp(&self, other: &Self, t: f32) -> Self {
        let t = clamp01(t);
        if let (
            Self::Cmyk {
                c: c0,
                m: m0,
                y: y0,
                k: k0,
                a: a0,
            },
            Self::Cmyk {
                c: c1,
                m: m1,
                y: y1,
                k: k1,
                a: a1,
            },
        ) = (self, other)
        {
            return Self::Cmyk {
                c: c0 + (c1 - c0) * t,
                m: m0 + (m1 - m0) * t,
                y: y0 + (y1 - y0) * t,
                k: k0 + (k1 - k0) * t,
                a: a0 + (a1 - a0) * t,
            };
        }
        let a = self.to_rgba_f32();
        let b = other.to_rgba_f32();
        Self::Rgba {
            r: a[0] + (b[0] - a[0]) * t,
            g: a[1] + (b[1] - a[1]) * t,
            b: a[2] + (b[2] - a[2]) * t,
            a: a[3] + (b[3] - a[3]) * t,
        }
    }

    /// WCAG relative luminance (`0.0` black .. `1.0` white).
    #[must_use]
    pub fn luminance(&self) -> f32 {
        let [r, g, b, _] = self.to_rgba_f32();
        (0.2126 * srgb_to_linear(r) + 0.7152 * srgb_to_linear(g) + 0.0722 * srgb_to_linear(b))
            as f32
    }

    /// CIE76 Delta-E perceptual distance in L*a*b* space.
    #[must_use]
    pub fn delta_e76(&self, other: &Self) -> f32 {
        let a = self.to_lab();
        let b = other.to_lab();
        let dl = a[0] - b[0];
        let da = a[1] - b[1];
        let db = a[2] - b[2];
        (dl * dl + da * da + db * db).sqrt()
    }
}

/// Named color swatch entry in a [`ColorPalette`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorSwatch {
    pub name: String,
    pub color: Color,
}

impl ColorSwatch {
    /// Creates a new named swatch.
    #[must_use]
    pub fn new(name: impl Into<String>, color: Color) -> Self {
        Self {
            name: name.into(),
            color,
        }
    }
}

/// Collection of named color swatches (built-in, custom, or Document Palette).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorPalette {
    pub id: Uuid,
    pub name: String,
    pub color_mode: ColorMode,
    pub is_read_only: bool,
    pub swatches: Vec<ColorSwatch>,
}

impl ColorPalette {
    /// Creates an empty mutable palette.
    #[must_use]
    pub fn new(name: impl Into<String>, color_mode: ColorMode) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            color_mode,
            is_read_only: false,
            swatches: Vec::new(),
        }
    }

    /// Appends a swatch to the palette.
    pub fn add_swatch(&mut self, swatch: ColorSwatch) {
        self.swatches.push(swatch);
    }

    /// Adds a color only if an equivalent RGBA8 color does not already exist in the palette.
    pub fn add_unique(&mut self, name: impl Into<String>, color: Color) {
        let target = color.to_rgba8();
        if !self.swatches.iter().any(|s| s.color.to_rgba8() == target) {
            self.swatches.push(ColorSwatch::new(name, color));
        }
    }

    /// Looks up a swatch by case-insensitive name.
    #[must_use]
    pub fn find_by_name(&self, name: &str) -> Option<&ColorSwatch> {
        self.swatches
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
    }
}

/// Unique identifier for a linked [`ColorStyle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ColorStyleId(pub Uuid);

impl ColorStyleId {
    /// Generates a new random v4 color style identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ColorStyleId {
    fn default() -> Self {
        Self::new()
    }
}

/// Color harmony rules for generating linked color sets from a base color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorHarmony {
    Complementary,
    Analogous,
    Triadic,
    Monochromatic,
    SplitComplementary,
    Tetradic,
}

impl ColorHarmony {
    /// Computes harmony companion colors (including the base color first) using HSL wheel math.
    #[must_use]
    pub fn generate(&self, base: &Color) -> Vec<Color> {
        let [h, s, l] = base.to_hsl();
        let a = base.alpha();
        let shift = |dh: f32, ds: f32, dl: f32| Color::Hsl {
            h: (h + dh).rem_euclid(360.0),
            s: (s + ds).clamp(0.0, 1.0),
            l: (l + dl).clamp(0.05, 0.95),
            a,
        };
        match self {
            Self::Complementary => vec![base.clone(), shift(180.0, 0.0, 0.0)],
            Self::Analogous => vec![shift(-30.0, 0.0, 0.0), base.clone(), shift(30.0, 0.0, 0.0)],
            Self::Triadic => vec![base.clone(), shift(120.0, 0.0, 0.0), shift(240.0, 0.0, 0.0)],
            Self::Monochromatic => vec![
                shift(0.0, 0.0, -0.30),
                shift(0.0, 0.0, -0.15),
                base.clone(),
                shift(0.0, 0.0, 0.15),
                shift(0.0, 0.0, 0.30),
            ],
            Self::SplitComplementary => {
                vec![base.clone(), shift(150.0, 0.0, 0.0), shift(210.0, 0.0, 0.0)]
            }
            Self::Tetradic => vec![
                base.clone(),
                shift(90.0, 0.0, 0.0),
                shift(180.0, 0.0, 0.0),
                shift(270.0, 0.0, 0.0),
            ],
        }
    }
}

/// Linked document color style that can propagate base color edits across harmony children.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorStyle {
    pub id: ColorStyleId,
    pub name: String,
    pub base_color: Color,
    pub harmony: Option<ColorHarmony>,
    pub derived_colors: Vec<Color>,
}

impl ColorStyle {
    /// Creates a new color style and immediately computes any harmony colors.
    #[must_use]
    pub fn new(name: impl Into<String>, base_color: Color, harmony: Option<ColorHarmony>) -> Self {
        let derived_colors = harmony.map_or_else(Vec::new, |h| h.generate(&base_color));
        Self {
            id: ColorStyleId::new(),
            name: name.into(),
            base_color,
            harmony,
            derived_colors,
        }
    }

    /// Updates the base color and refreshes harmony-derived colors.
    pub fn set_base_color(&mut self, color: Color) {
        self.base_color = color;
        if let Some(harmony) = self.harmony {
            self.derived_colors = harmony.generate(&self.base_color);
        }
    }
}

/// Clean-room mathematically generated CMYK process palette (84 swatches).
///
/// Contains a 10% neutral gray ramp, a 12-hue CMYK chromatic wheel across 5 tonal
/// steps, rich print blacks, and clean-room print/cutting spot inks.
#[must_use]
pub fn default_cmyk_palette() -> ColorPalette {
    let mut swatches = Vec::with_capacity(84);

    // 1. Neutral K ramp: 11 swatches (0% .. 100% K in 10% steps)
    for step in 0..=10 {
        let k = (step as f32) * 0.1;
        let pct = step * 10;
        let name = match pct {
            0 => "Paper White".to_owned(),
            100 => "Process Black".to_owned(),
            _ => format!("Black {pct}%"),
        };
        swatches.push(ColorSwatch::new(name, Color::cmyk(0.0, 0.0, 0.0, k)));
    }

    // 2. Specialty print blacks & technical spot colors: 5 swatches
    swatches.push(ColorSwatch::new(
        "Rich Black",
        Color::cmyk(0.40, 0.30, 0.30, 1.0),
    ));
    swatches.push(ColorSwatch::new(
        "Registration",
        Color::cmyk(1.0, 1.0, 1.0, 1.0),
    ));
    swatches.push(ColorSwatch::new("CutContour", Color::cut_contour_spot()));
    swatches.push(ColorSwatch::new(
        "Crease",
        Color::spot("Crease", 1.0, [1.0, 0.0, 0.0, 0.0]),
    ));
    swatches.push(ColorSwatch::new(
        "Bleed Guide",
        Color::spot("BleedGuide", 1.0, [0.0, 0.5, 1.0, 0.0]),
    ));

    // 3. 12-hue CMYK chromatic wheel × 5 tonal levels = 60 swatches
    const BASE_HUES: [(&str, [f32; 3]); 12] = [
        ("Cyan", [1.0, 0.0, 0.0]),
        ("Azure", [1.0, 0.5, 0.0]),
        ("Cobalt", [1.0, 1.0, 0.0]),
        ("Violet", [0.5, 1.0, 0.0]),
        ("Magenta", [0.0, 1.0, 0.0]),
        ("Crimson", [0.0, 1.0, 0.5]),
        ("Process Red", [0.0, 1.0, 1.0]),
        ("Orange", [0.0, 0.5, 1.0]),
        ("Yellow", [0.0, 0.0, 1.0]),
        ("Chartreuse", [0.5, 0.0, 1.0]),
        ("Process Green", [1.0, 0.0, 1.0]),
        ("Teal", [1.0, 0.0, 0.5]),
    ];
    const TONES: [(&str, f32, f32); 5] = [
        ("Tint 25%", 0.25, 0.0),
        ("Tint 55%", 0.55, 0.0),
        ("Pure", 1.0, 0.0),
        ("Shade 25%", 1.0, 0.25),
        ("Shade 55%", 1.0, 0.55),
    ];

    for (hue_name, [bc, bm, by]) in BASE_HUES {
        for (tone_name, scale, k) in TONES {
            let label = if tone_name == "Pure" {
                hue_name.to_owned()
            } else {
                format!("{hue_name} {tone_name}")
            };
            swatches.push(ColorSwatch::new(
                label,
                Color::cmyk(bc * scale, bm * scale, by * scale, k),
            ));
        }
    }

    // 4. Warm & cool neutral earth/studio tones: 8 swatches
    const EARTH_TONES: [(&str, [f32; 4]); 8] = [
        ("Warm Sand", [0.05, 0.12, 0.24, 0.04]),
        ("Ochre", [0.10, 0.35, 0.75, 0.10]),
        ("Terracotta", [0.15, 0.65, 0.70, 0.15]),
        ("Umber", [0.25, 0.55, 0.75, 0.45]),
        ("Slate Light", [0.20, 0.12, 0.08, 0.15]),
        ("Slate Mid", [0.35, 0.22, 0.15, 0.35]),
        ("Slate Deep", [0.50, 0.35, 0.25, 0.65]),
        ("Midnight Ink", [0.85, 0.65, 0.35, 0.70]),
    ];
    for (name, [c, m, y, k]) in EARTH_TONES {
        swatches.push(ColorSwatch::new(name, Color::cmyk(c, m, y, k)));
    }

    ColorPalette {
        id: Uuid::from_u128(0x636f_7265_655f_636d_796b_5f70_616c_0001),
        name: "Default CMYK".to_owned(),
        color_mode: ColorMode::Cmyk,
        is_read_only: true,
        swatches,
    }
}

/// Clean-room mathematically generated sRGB palette (80 swatches).
///
/// Contains an 11-step neutral ramp, a 12-hue HSL chromatic grid across 5 lightness
/// levels, and 9 muted studio accents.
#[must_use]
pub fn default_rgb_palette() -> ColorPalette {
    let mut swatches = Vec::with_capacity(80);

    // 1. Neutral sRGB gray ramp: 11 swatches
    for step in 0..=10 {
        let v = 1.0 - (step as f32) * 0.1;
        let pct = step * 10;
        let name = match pct {
            0 => "White".to_owned(),
            100 => "Black".to_owned(),
            _ => format!("Gray {pct}%"),
        };
        swatches.push(ColorSwatch::new(name, Color::rgb(v, v, v)));
    }

    // 2. 12-hue HSL wheel × 5 lightness steps = 60 swatches
    const HUE_NAMES: [(&str, f32); 12] = [
        ("Red", 0.0),
        ("Orange", 30.0),
        ("Amber", 60.0),
        ("Lime", 90.0),
        ("Green", 120.0),
        ("Mint", 150.0),
        ("Cyan", 180.0),
        ("Sky", 210.0),
        ("Blue", 240.0),
        ("Indigo", 270.0),
        ("Magenta", 300.0),
        ("Rose", 330.0),
    ];
    const LIGHTNESS_LEVELS: [(&str, f32, f32); 5] = [
        ("90", 0.85, 0.90),
        ("72", 0.85, 0.72),
        ("50", 0.90, 0.50),
        ("35", 0.85, 0.35),
        ("20", 0.80, 0.20),
    ];

    for (hue_name, h) in HUE_NAMES {
        for (level_name, s, l) in LIGHTNESS_LEVELS {
            let [r, g, b] = hsl_to_rgb(h, s, l);
            swatches.push(ColorSwatch::new(
                format!("{hue_name} {level_name}"),
                Color::rgb(r, g, b),
            ));
        }
    }

    // 3. Muted studio UI / editorial neutrals: 9 swatches
    for i in 0..9 {
        let h = (i as f32) * 40.0;
        let [r, g, b] = hsl_to_rgb(h, 0.28, 0.48);
        swatches.push(ColorSwatch::new(
            format!("Muted {}°", (i * 40)),
            Color::rgb(r, g, b),
        ));
    }

    ColorPalette {
        id: Uuid::from_u128(0x636f_7265_655f_7267_625f_7061_6c00_0002),
        name: "Default RGB".to_owned(),
        color_mode: ColorMode::Rgb,
        is_read_only: true,
        swatches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_cmyk_lab_hsl_conversions_roundtrip() {
        let samples = [
            Color::rgb(1.0, 0.0, 0.0),
            Color::rgb(0.0, 1.0, 0.0),
            Color::rgb(0.0, 0.0, 1.0),
            Color::rgb(0.25, 0.62, 0.88),
            Color::rgb(0.0, 0.0, 0.0),
            Color::rgb(1.0, 1.0, 1.0),
        ];

        for orig in &samples {
            let orig_rgba = orig.to_rgba_f32();

            // Lab round-trip
            let lab = orig.as_lab_color();
            let back_lab = lab.to_rgba_f32();
            for ch in 0..3 {
                assert!(
                    (orig_rgba[ch] - back_lab[ch]).abs() < 1e-3,
                    "Lab round-trip mismatch on {orig:?}"
                );
            }

            // HSL round-trip
            let hsl = orig.as_hsl_color();
            let back_hsl = hsl.to_rgba_f32();
            for ch in 0..3 {
                assert!(
                    (orig_rgba[ch] - back_hsl[ch]).abs() < 1e-3,
                    "HSL round-trip mismatch on {orig:?}"
                );
            }

            // CMYK round-trip
            let cmyk = orig.as_cmyk_color();
            let back_cmyk = cmyk.to_rgba_f32();
            for ch in 0..3 {
                assert!(
                    (orig_rgba[ch] - back_cmyk[ch]).abs() < 1e-3,
                    "CMYK round-trip mismatch on {orig:?}"
                );
            }
        }
    }

    #[test]
    fn hex_spot_lerp_and_nan_safety() {
        assert_eq!(
            Color::from_hex("#ff8000").unwrap().to_rgba8(),
            [255, 128, 0, 255]
        );
        assert_eq!(Color::from_hex("#f80").unwrap().to_hex(), "#ff8800");
        assert_eq!(
            Color::from_hex("11223380").unwrap().to_rgba8(),
            [0x11, 0x22, 0x33, 0x80]
        );
        assert!(Color::from_hex("#zzzzzz").is_err());

        let spot = Color::spot("CutContour", 0.5, [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(spot.to_cmyk(), [0.0, 0.5, 0.0, 0.0]);

        let mid = Color::BLACK.lerp(&Color::WHITE, 0.5);
        assert!((mid.to_rgba_f32()[0] - 0.5).abs() < 1e-5);

        // NaN safety
        let bad = Color::Rgba {
            r: f32::NAN,
            g: f32::INFINITY,
            b: -5.0,
            a: f32::NAN,
        };
        let rgba = bad.to_rgba_f32();
        assert!(rgba
            .iter()
            .all(|c| c.is_finite() && (0.0..=1.0).contains(c)));
    }

    #[test]
    fn default_palettes_and_harmonies() {
        let cmyk = default_cmyk_palette();
        let rgb = default_rgb_palette();
        assert!(cmyk.swatches.len() >= 64);
        assert!(rgb.swatches.len() >= 64);
        assert!(cmyk.find_by_name("CutContour").is_some());
        // Clean-room verification: zero Pantone strings anywhere in palettes
        assert!(cmyk
            .swatches
            .iter()
            .chain(&rgb.swatches)
            .all(|s| !s.name.to_ascii_lowercase().contains("pantone")));

        let base = Color::hsl(210.0, 0.8, 0.5);
        assert_eq!(ColorHarmony::Complementary.generate(&base).len(), 2);
        assert_eq!(ColorHarmony::Analogous.generate(&base).len(), 3);
        assert_eq!(ColorHarmony::Triadic.generate(&base).len(), 3);
        assert_eq!(ColorHarmony::Monochromatic.generate(&base).len(), 5);
    }
}
