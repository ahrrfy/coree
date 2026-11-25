// SPDX-License-Identifier: MIT OR Apache-2.0

//! Complete 11-Lens non-destructive optical and color-filtering lens engine.

use crate::color::Color;
use crate::icc::IccColorPipeline;
use crate::math::{sanitize_f32, Point2D};
use serde::{Deserialize, Serialize};

/// Palette interpolation direction for [`LensKind::CustomColorMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ColorMapMode {
    #[default]
    Direct,
    ForwardRainbow,
    ReverseRainbow,
}

/// All 11 non-destructive vector lens types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LensKind {
    /// Lightens or darkens underlying artwork (`rate_pct` in `-100.0..=100.0`).
    Brighten { rate_pct: f32 },
    /// Additive color tinting (`rate_pct` in `0.0..=100.0`).
    ColorAdd { color: Color, rate_pct: f32 },
    /// Subtractive color filter limiting transmitted channels (`rate_pct` in `0.0..=100.0`).
    ColorLimit { color: Color, rate_pct: f32 },
    /// Remaps luminance across a two-color ramp or rainbow spectrum.
    CustomColorMap {
        from_color: Color,
        to_color: Color,
        palette_mode: ColorMapMode,
    },
    /// Spherical barrel/pincushion geometric distortion (`rate_pct` in `-1000.0..=1000.0`).
    FishEye { rate_pct: f32 },
    /// Infrared thermal palette mapping rotated by `palette_rotation_deg` (`0..360`).
    HeatMap { palette_rotation_deg: f32 },
    /// Complementary CMY/RGB channel inversion.
    Invert,
    /// Optical magnification (`amount` in `1.0..=100.0`).
    Magnify { amount: f32 },
    /// Luminance grayscale tinted toward `tint`.
    TintedGrayscale { tint: Color },
    /// Uniform tinted transparency over underlying artwork (`rate_pct` in `0.0..=100.0`).
    Transparency { color: Color, rate_pct: f32 },
    /// Renders underlying objects as wireframe with optional outline/fill overrides.
    Wireframe {
        override_outline: Option<Color>,
        override_fill: Option<Color>,
    },
}

impl LensKind {
    /// Total number of supported lens kinds (`11`).
    pub const VARIANT_COUNT: usize = 11;

    /// Human-readable clean-room label for this lens kind.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Brighten { .. } => "Brighten",
            Self::ColorAdd { .. } => "Color Add",
            Self::ColorLimit { .. } => "Color Limit",
            Self::CustomColorMap { .. } => "Custom Color Map",
            Self::FishEye { .. } => "Fish Eye",
            Self::HeatMap { .. } => "Heat Map",
            Self::Invert => "Invert",
            Self::Magnify { .. } => "Magnify",
            Self::TintedGrayscale { .. } => "Tinted Grayscale",
            Self::Transparency { .. } => "Transparency",
            Self::Wireframe { .. } => "Wireframe",
        }
    }

    /// Returns a sanitized copy with all numeric rates clamped to valid ranges.
    #[must_use]
    pub fn clamped(&self) -> Self {
        match self {
            Self::Brighten { rate_pct } => Self::Brighten {
                rate_pct: sanitize_f32(*rate_pct, 0.0).clamp(-100.0, 100.0),
            },
            Self::ColorAdd { color, rate_pct } => Self::ColorAdd {
                color: color.clone(),
                rate_pct: sanitize_f32(*rate_pct, 50.0).clamp(0.0, 100.0),
            },
            Self::ColorLimit { color, rate_pct } => Self::ColorLimit {
                color: color.clone(),
                rate_pct: sanitize_f32(*rate_pct, 50.0).clamp(0.0, 100.0),
            },
            Self::CustomColorMap {
                from_color,
                to_color,
                palette_mode,
            } => Self::CustomColorMap {
                from_color: from_color.clone(),
                to_color: to_color.clone(),
                palette_mode: *palette_mode,
            },
            Self::FishEye { rate_pct } => Self::FishEye {
                rate_pct: sanitize_f32(*rate_pct, 100.0).clamp(-1000.0, 1000.0),
            },
            Self::HeatMap {
                palette_rotation_deg,
            } => Self::HeatMap {
                palette_rotation_deg: sanitize_f32(*palette_rotation_deg, 0.0).rem_euclid(360.0),
            },
            Self::Invert => Self::Invert,
            Self::Magnify { amount } => Self::Magnify {
                amount: sanitize_f32(*amount, 2.0).clamp(1.0, 100.0),
            },
            Self::TintedGrayscale { tint } => Self::TintedGrayscale { tint: tint.clone() },
            Self::Transparency { color, rate_pct } => Self::Transparency {
                color: color.clone(),
                rate_pct: sanitize_f32(*rate_pct, 50.0).clamp(0.0, 100.0),
            },
            Self::Wireframe {
                override_outline,
                override_fill,
            } => Self::Wireframe {
                override_outline: override_outline.clone(),
                override_fill: override_fill.clone(),
            },
        }
    }

    /// Evaluates the color transformation produced by this lens on an `underlying` color.
    ///
    /// Geometric lenses (`FishEye`, `Magnify`) return `underlying` unchanged.
    #[must_use]
    pub fn apply_color(&self, underlying: &Color) -> Color {
        self.apply_color_icc(underlying, &IccColorPipeline::default())
    }

    /// Evaluates the color transformation produced by this lens on an `underlying` color
    /// using an explicit [`IccColorPipeline`].
    #[must_use]
    pub fn apply_color_icc(&self, underlying: &Color, pipeline: &IccColorPipeline) -> Color {
        let srgb_px = underlying.to_rgba_f32_icc(pipeline);
        self.apply_color_core(srgb_px, Some(underlying), pipeline)
    }

    /// Evaluates the color transformation produced by this lens on a display-sRGB
    /// `[r, g, b, a]` backdrop pixel (`0.0..=1.0`), converting only the lens's own
    /// `Color` parameters through `pipeline`.
    ///
    /// Unlike [`Self::apply_color_icc`], this never re-interprets `srgb_px` through
    /// `pipeline.rgb_profile`, preventing double-conversion when the document RGB
    /// profile is not sRGB (e.g., Adobe RGB 1998).
    #[must_use]
    pub fn apply_color_on_srgb_backdrop(
        &self,
        srgb_px: [f32; 4],
        pipeline: &IccColorPipeline,
    ) -> [f32; 4] {
        let srgb_px = [
            sanitize_f32(srgb_px[0], 0.0).clamp(0.0, 1.0),
            sanitize_f32(srgb_px[1], 0.0).clamp(0.0, 1.0),
            sanitize_f32(srgb_px[2], 0.0).clamp(0.0, 1.0),
            sanitize_f32(srgb_px[3], 0.0).clamp(0.0, 1.0),
        ];
        match self.apply_color_core(srgb_px, None, pipeline) {
            // When `underlying` is `None`, every `Color::Rgba` returned by `apply_color_core`
            // is already in display sRGB; only non-RGB results (e.g. CMYK `CustomColorMap`)
            // need ICC conversion to display sRGB.
            Color::Rgba { r, g, b, a } => [
                sanitize_f32(r, 0.0).clamp(0.0, 1.0),
                sanitize_f32(g, 0.0).clamp(0.0, 1.0),
                sanitize_f32(b, 0.0).clamp(0.0, 1.0),
                sanitize_f32(a, 0.0).clamp(0.0, 1.0),
            ],
            other => other.to_rgba_f32_icc(pipeline),
        }
    }

    fn apply_color_core(
        &self,
        srgb_px: [f32; 4],
        underlying: Option<&Color>,
        pipeline: &IccColorPipeline,
    ) -> Color {
        let [r, g, b, a] = srgb_px;
        match self.clamped() {
            Self::Brighten { rate_pct } => {
                let f = rate_pct / 100.0;
                if f >= 0.0 {
                    Color::rgba(r + (1.0 - r) * f, g + (1.0 - g) * f, b + (1.0 - b) * f, a)
                } else {
                    let s = 1.0 + f;
                    Color::rgba(r * s, g * s, b * s, a)
                }
            }
            Self::ColorAdd { color, rate_pct } => {
                let [cr, cg, cb, _] = color.to_rgba_f32_icc(pipeline);
                let f = rate_pct / 100.0;
                Color::rgba(
                    (r + cr * f).clamp(0.0, 1.0),
                    (g + cg * f).clamp(0.0, 1.0),
                    (b + cb * f).clamp(0.0, 1.0),
                    a,
                )
            }
            Self::ColorLimit { color, rate_pct } => {
                let [cr, cg, cb, _] = color.to_rgba_f32_icc(pipeline);
                let f = rate_pct / 100.0;
                // Subtractive filter: blends underlying toward multiplicative transmission
                Color::rgba(
                    r * (1.0 - f + f * cr),
                    g * (1.0 - f + f * cg),
                    b * (1.0 - f + f * cb),
                    a,
                )
            }
            Self::CustomColorMap {
                from_color,
                to_color,
                palette_mode,
            } => {
                let lum = (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 1.0);
                match palette_mode {
                    ColorMapMode::Direct => {
                        from_color.lerp_icc(&to_color, lum, pipeline).with_alpha(a)
                    }
                    ColorMapMode::ForwardRainbow => {
                        let [h0, s0, l0] = color_to_hsl_icc(&from_color, pipeline);
                        let [h1, s1, l1] = color_to_hsl_icc(&to_color, pipeline);
                        let dh = (h1 - h0).rem_euclid(360.0);
                        Color::Hsl {
                            h: (h0 + dh * lum).rem_euclid(360.0),
                            s: s0 + (s1 - s0) * lum,
                            l: l0 + (l1 - l0) * lum,
                            a,
                        }
                    }
                    ColorMapMode::ReverseRainbow => {
                        let [h0, s0, l0] = color_to_hsl_icc(&from_color, pipeline);
                        let [h1, s1, l1] = color_to_hsl_icc(&to_color, pipeline);
                        let dh = (h0 - h1).rem_euclid(360.0);
                        Color::Hsl {
                            h: (h0 - dh * lum).rem_euclid(360.0),
                            s: s0 + (s1 - s0) * lum,
                            l: l0 + (l1 - l0) * lum,
                            a,
                        }
                    }
                }
            }
            Self::HeatMap {
                palette_rotation_deg,
            } => {
                let lum = (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 1.0);
                // Classic thermal ramp: blue (240°) -> cyan -> green -> yellow -> red (0°)
                let hue = (240.0 * (1.0 - lum) + palette_rotation_deg).rem_euclid(360.0);
                Color::Hsl {
                    h: hue,
                    s: 0.95,
                    l: 0.15 + 0.70 * lum,
                    a,
                }
            }
            Self::Invert => Color::rgba(1.0 - r, 1.0 - g, 1.0 - b, a),
            Self::TintedGrayscale { tint } => {
                let lum = (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 1.0);
                let [tr, tg, tb, _] = tint.to_rgba_f32_icc(pipeline);
                Color::rgba(
                    tr + (1.0 - tr) * lum,
                    tg + (1.0 - tg) * lum,
                    tb + (1.0 - tb) * lum,
                    a,
                )
            }
            Self::Transparency { color, rate_pct } => {
                let f = rate_pct / 100.0;
                if let Some(u) = underlying {
                    u.lerp_icc(&color, f, pipeline).with_alpha(a)
                } else {
                    let [cr, cg, cb, _] = color.to_rgba_f32_icc(pipeline);
                    Color::rgba(r + (cr - r) * f, g + (cg - g) * f, b + (cb - b) * f, a)
                }
            }
            Self::Wireframe { override_fill, .. } => match (underlying, override_fill) {
                (Some(_), Some(fill)) => fill,
                (Some(u), None) => u.clone(),
                (None, Some(fill)) => {
                    let [fr, fg, fb, fa] = fill.to_rgba_f32_icc(pipeline);
                    Color::rgba(fr, fg, fb, fa)
                }
                (None, None) => Color::rgba(r, g, b, a),
            },
            Self::FishEye { .. } | Self::Magnify { .. } => underlying
                .cloned()
                .unwrap_or_else(|| Color::rgba(r, g, b, a)),
        }
    }

    /// Evaluates geometric point warping for `FishEye` and `Magnify` lenses around `(center, radius)`.
    #[must_use]
    pub fn map_point(&self, pt: Point2D, center: Point2D, radius: f64) -> Point2D {
        if !pt.is_finite() || !center.is_finite() || !radius.is_finite() || radius <= 1e-6 {
            return pt;
        }
        let rel = pt - center;
        let dist = rel.length();
        match self.clamped() {
            Self::Magnify { amount } => center + rel * f64::from(amount),
            Self::FishEye { rate_pct } => {
                let r_norm = dist / radius;
                if r_norm >= 1.0 || r_norm <= 1e-9 {
                    return pt;
                }
                let strength = f64::from(rate_pct) / 100.0;
                let exponent = if strength >= 0.0 {
                    1.0 / (1.0 + strength)
                } else {
                    1.0 + strength.abs()
                };
                let warped_norm = r_norm.powf(exponent);
                center + rel * (warped_norm / r_norm)
            }
            _ => pt,
        }
    }
}

#[inline]
fn color_to_hsl_icc(color: &Color, pipeline: &IccColorPipeline) -> [f32; 3] {
    if let Color::Hsl { h, s, l, .. } = *color {
        return [
            sanitize_f32(h, 0.0).rem_euclid(360.0),
            sanitize_f32(s, 0.0).clamp(0.0, 1.0),
            sanitize_f32(l, 0.0).clamp(0.0, 1.0),
        ];
    }
    let [r, g, b, _] = color.to_rgba_f32_icc(pipeline);
    Color::rgba(r, g, b, 1.0).to_hsl()
}

/// Full lens attachment configuration stored on a [`crate::document::SceneObject`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LensConfig {
    pub kind: LensKind,
    /// When `true`, the lens snapshot is frozen in place when moved.
    pub frozen: bool,
    /// Optional custom viewpoint center in canvas mm coordinates.
    pub viewpoint: Option<Point2D>,
    /// When `true`, hides areas of the lens not covering underlying objects.
    pub remove_face: bool,
    /// Optional persisted PNG snapshot bytes captured when `frozen == true`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::document::opt_serde_bytes_base64"
    )]
    pub frozen_snapshot: Option<Vec<u8>>,
}

impl LensConfig {
    /// Creates an active, unfrozen lens configuration.
    #[must_use]
    pub fn new(kind: LensKind) -> Self {
        Self {
            kind: kind.clamped(),
            frozen: false,
            viewpoint: None,
            remove_face: false,
            frozen_snapshot: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_eleven_lenses_serialize_and_evaluate() {
        let all_lenses = vec![
            LensConfig::new(LensKind::Brighten { rate_pct: 40.0 }),
            LensConfig::new(LensKind::ColorAdd {
                color: Color::rgb(0.2, 0.4, 0.8),
                rate_pct: 50.0,
            }),
            LensConfig::new(LensKind::ColorLimit {
                color: Color::rgb(1.0, 0.2, 0.2),
                rate_pct: 80.0,
            }),
            LensConfig::new(LensKind::CustomColorMap {
                from_color: Color::rgb(0.1, 0.0, 0.4),
                to_color: Color::rgb(1.0, 0.9, 0.2),
                palette_mode: ColorMapMode::ForwardRainbow,
            }),
            LensConfig::new(LensKind::FishEye { rate_pct: 150.0 }),
            LensConfig::new(LensKind::HeatMap {
                palette_rotation_deg: 45.0,
            }),
            LensConfig::new(LensKind::Invert),
            LensConfig::new(LensKind::Magnify { amount: 2.5 }),
            LensConfig::new(LensKind::TintedGrayscale {
                tint: Color::rgb(0.2, 0.1, 0.0),
            }),
            LensConfig::new(LensKind::Transparency {
                color: Color::CYAN,
                rate_pct: 35.0,
            }),
            LensConfig::new(LensKind::Wireframe {
                override_outline: Some(Color::BLACK),
                override_fill: Some(Color::WHITE),
            }),
        ];

        assert_eq!(all_lenses.len(), LensKind::VARIANT_COUNT);

        for cfg in &all_lenses {
            let json = serde_json::to_string(cfg).unwrap();
            let roundtrip: LensConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(*cfg, roundtrip);
            let out = cfg.kind.apply_color(&Color::rgb(0.4, 0.5, 0.6));
            assert!(out.to_rgba_f32().iter().all(|c| c.is_finite()));
        }

        // Verify Invert flips black -> white
        assert_eq!(
            LensKind::Invert.apply_color(&Color::BLACK).to_rgba8(),
            [255, 255, 255, 255]
        );
        // Verify Magnify doubles displacement from center
        let p = LensKind::Magnify { amount: 2.0 }.map_point(
            Point2D::new(15.0, 10.0),
            Point2D::new(10.0, 10.0),
            20.0,
        );
        assert!((p.x - 20.0).abs() < 1e-9);
    }

    fn build_wide_gamut_rgb_icc_v2() -> Vec<u8> {
        let mut tags: Vec<(&[u8; 4], Vec<u8>)> = Vec::new();
        let xyz_tag = |xyz: [f64; 3]| -> Vec<u8> {
            let mut v = Vec::with_capacity(20);
            v.extend_from_slice(b"XYZ ");
            v.extend_from_slice(&0u32.to_be_bytes());
            for c in xyz {
                v.extend_from_slice(&((c * 65536.0).round() as i32).to_be_bytes());
            }
            v
        };
        let curv_gamma = |gamma: f64| -> Vec<u8> {
            let mut v = Vec::with_capacity(14);
            v.extend_from_slice(b"curv");
            v.extend_from_slice(&0u32.to_be_bytes());
            v.extend_from_slice(&1u32.to_be_bytes());
            v.extend_from_slice(&((gamma * 256.0).round() as u16).to_be_bytes());
            v
        };
        let mut desc = Vec::new();
        let label = b"WideGamutAdobeTest";
        desc.extend_from_slice(b"desc");
        desc.extend_from_slice(&0u32.to_be_bytes());
        desc.extend_from_slice(&((label.len() + 1) as u32).to_be_bytes());
        desc.extend_from_slice(label);
        desc.push(0);
        desc.extend_from_slice(&0u32.to_be_bytes());
        desc.extend_from_slice(&0u32.to_be_bytes());
        desc.extend_from_slice(&0u16.to_be_bytes());
        desc.push(0);
        desc.resize(desc.len() + 67, 0);

        tags.push((b"desc", desc));
        tags.push((b"wtpt", xyz_tag([0.95047, 1.0, 1.08883])));
        // Adobe RGB (1998) D50-adapted column primaries
        tags.push((b"rXYZ", xyz_tag([0.60974, 0.31111, 0.01947])));
        tags.push((b"gXYZ", xyz_tag([0.20528, 0.62567, 0.06087])));
        tags.push((b"bXYZ", xyz_tag([0.14919, 0.06322, 0.74457])));
        tags.push((b"rTRC", curv_gamma(2.19921875)));
        tags.push((b"gTRC", curv_gamma(2.19921875)));
        tags.push((b"bTRC", curv_gamma(2.19921875)));

        let header_size = 128usize;
        let tag_table_size = 4 + tags.len() * 12;
        let mut data_offset = (header_size + tag_table_size + 3) & !3;
        let mut table = Vec::with_capacity(tag_table_size);
        table.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        let mut body = Vec::new();
        for (sig, payload) in &tags {
            let aligned_off = data_offset + body.len();
            table.extend_from_slice(*sig);
            table.extend_from_slice(&(aligned_off as u32).to_be_bytes());
            table.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            body.extend_from_slice(payload);
            while body.len() % 4 != 0 {
                body.push(0);
            }
        }
        data_offset = (header_size + table.len() + 3) & !3;
        let total_size = data_offset + body.len();
        let mut out = vec![0u8; 128];
        out[0..4].copy_from_slice(&(total_size as u32).to_be_bytes());
        out[4..8].copy_from_slice(b"core");
        out[8] = 2;
        out[12..16].copy_from_slice(b"mntr");
        out[16..20].copy_from_slice(b"RGB ");
        out[20..24].copy_from_slice(b"XYZ ");
        out[36..40].copy_from_slice(b"acsp");
        out[68..72].copy_from_slice(&((0.9642_f64 * 65536.0).round() as i32).to_be_bytes());
        out[72..76].copy_from_slice(&((1.0_f64 * 65536.0).round() as i32).to_be_bytes());
        out[76..80].copy_from_slice(&((0.8249_f64 * 65536.0).round() as i32).to_be_bytes());
        out.extend_from_slice(&table);
        out.resize(data_offset, 0);
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn srgb_backdrop_helper_avoids_double_conversion_and_converts_lens_colors() {
        let wide_rgb = build_wide_gamut_rgb_icc_v2();
        let cmyk_icc = crate::icc::generate_minimal_cmyk_icc_v2(
            "TestISO",
            crate::icc::BuiltinIccProfile::IsoCoatedV2Cmyk,
        );
        let pipe = IccColorPipeline::from_document_profiles(
            Some(&wide_rgb),
            Some(&cmyk_icc),
            None,
            None,
            crate::icc::RenderingIntent::RelativeColorimetric,
            true,
        );

        let cmyk_bg = Color::cmyk(0.7, 0.2, 0.1, 0.1);
        let backdrop_srgb = cmyk_bg.to_rgba_f32_icc(&pipe);

        // Confirm that feeding `backdrop_srgb` back through `rgb_profile` WOULD shift it
        let double_conv = Color::rgba(
            backdrop_srgb[0],
            backdrop_srgb[1],
            backdrop_srgb[2],
            backdrop_srgb[3],
        )
        .to_rgba_f32_icc(&pipe);
        assert!(
            (double_conv[0] - backdrop_srgb[0]).abs() > 0.05,
            "test precondition: wide-gamut RGB profile must differ from sRGB"
        );

        // 0% no-op lenses and geometric/passthrough lenses must produce exact zero drift
        let noop_lenses = [
            LensKind::Brighten { rate_pct: 0.0 },
            LensKind::ColorAdd {
                color: Color::rgb(1.0, 0.0, 0.0),
                rate_pct: 0.0,
            },
            LensKind::ColorLimit {
                color: Color::rgb(0.2, 0.6, 0.8),
                rate_pct: 0.0,
            },
            LensKind::Transparency {
                color: Color::rgb(0.0, 0.0, 1.0),
                rate_pct: 0.0,
            },
            LensKind::FishEye { rate_pct: 120.0 },
            LensKind::Magnify { amount: 2.0 },
            LensKind::Wireframe {
                override_outline: None,
                override_fill: None,
            },
        ];
        for kind in &noop_lenses {
            let out = kind.apply_color_on_srgb_backdrop(backdrop_srgb, &pipe);
            for c in 0..4 {
                assert!(
                    (out[c] - backdrop_srgb[c]).abs() < 1e-6,
                    "{kind:?} drifted channel {c}: out={out:?} vs in={backdrop_srgb:?}"
                );
            }
        }

        // Lens-owned Color parameters must still convert through `pipe`
        let lens_cmyk = Color::cmyk(0.1, 0.8, 0.2, 0.0);
        let lens_cmyk_srgb = lens_cmyk.to_rgba_f32_icc(&pipe);

        let add_out = LensKind::ColorAdd {
            color: lens_cmyk.clone(),
            rate_pct: 50.0,
        }
        .apply_color_on_srgb_backdrop(backdrop_srgb, &pipe);
        for c in 0..3 {
            let expected = (backdrop_srgb[c] + lens_cmyk_srgb[c] * 0.5).clamp(0.0, 1.0);
            assert!((add_out[c] - expected).abs() < 1e-5);
        }

        let transp_out = LensKind::Transparency {
            color: lens_cmyk.clone(),
            rate_pct: 50.0,
        }
        .apply_color_on_srgb_backdrop(backdrop_srgb, &pipe);
        for c in 0..3 {
            let expected = backdrop_srgb[c] + (lens_cmyk_srgb[c] - backdrop_srgb[c]) * 0.5;
            assert!((transp_out[c] - expected).abs() < 1e-5);
        }

        let limit_out = LensKind::ColorLimit {
            color: lens_cmyk.clone(),
            rate_pct: 80.0,
        }
        .apply_color_on_srgb_backdrop(backdrop_srgb, &pipe);
        for c in 0..3 {
            let expected = backdrop_srgb[c] * (0.2 + 0.8 * lens_cmyk_srgb[c]);
            assert!((limit_out[c] - expected).abs() < 1e-5);
        }

        let lum =
            (0.2126 * backdrop_srgb[0] + 0.7152 * backdrop_srgb[1] + 0.0722 * backdrop_srgb[2])
                .clamp(0.0, 1.0);
        let tint_out = LensKind::TintedGrayscale {
            tint: lens_cmyk.clone(),
        }
        .apply_color_on_srgb_backdrop(backdrop_srgb, &pipe);
        for c in 0..3 {
            let expected = lens_cmyk_srgb[c] + (1.0 - lens_cmyk_srgb[c]) * lum;
            assert!((tint_out[c] - expected).abs() < 1e-5);
        }

        let to_cmyk = Color::cmyk(0.8, 0.1, 0.7, 0.0);
        let direct_out = LensKind::CustomColorMap {
            from_color: lens_cmyk.clone(),
            to_color: to_cmyk.clone(),
            palette_mode: ColorMapMode::Direct,
        }
        .apply_color_on_srgb_backdrop(backdrop_srgb, &pipe);
        let expected_direct = lens_cmyk
            .lerp_icc(&to_cmyk, lum, &pipe)
            .to_rgba_f32_icc(&pipe);
        for c in 0..3 {
            assert!((direct_out[c] - expected_direct[c]).abs() < 1e-5);
        }

        let wire_fill_rgb = Color::rgb(0.8, 0.2, 0.1);
        let wire_out = LensKind::Wireframe {
            override_outline: None,
            override_fill: Some(wire_fill_rgb.clone()),
        }
        .apply_color_on_srgb_backdrop(backdrop_srgb, &pipe);
        let expected_wire = wire_fill_rgb.to_rgba_f32_icc(&pipe);
        for c in 0..4 {
            assert!((wire_out[c] - expected_wire[c]).abs() < 1e-5);
        }
    }
}
