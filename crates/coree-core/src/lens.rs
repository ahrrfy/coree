// SPDX-License-Identifier: MPL-2.0

//! Complete 11-Lens non-destructive optical and color-filtering lens engine.

use crate::color::Color;
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
        let [r, g, b, a] = underlying.to_rgba_f32();
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
                let [cr, cg, cb, _] = color.to_rgba_f32();
                let f = rate_pct / 100.0;
                Color::rgba(
                    (r + cr * f).clamp(0.0, 1.0),
                    (g + cg * f).clamp(0.0, 1.0),
                    (b + cb * f).clamp(0.0, 1.0),
                    a,
                )
            }
            Self::ColorLimit { color, rate_pct } => {
                let [cr, cg, cb, _] = color.to_rgba_f32();
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
                let lum = underlying.luminance().clamp(0.0, 1.0);
                match palette_mode {
                    ColorMapMode::Direct => from_color.lerp(&to_color, lum).with_alpha(a),
                    ColorMapMode::ForwardRainbow => {
                        let [h0, s0, l0] = from_color.to_hsl();
                        let [h1, s1, l1] = to_color.to_hsl();
                        let dh = (h1 - h0).rem_euclid(360.0);
                        Color::Hsl {
                            h: (h0 + dh * lum).rem_euclid(360.0),
                            s: s0 + (s1 - s0) * lum,
                            l: l0 + (l1 - l0) * lum,
                            a,
                        }
                    }
                    ColorMapMode::ReverseRainbow => {
                        let [h0, s0, l0] = from_color.to_hsl();
                        let [h1, s1, l1] = to_color.to_hsl();
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
                let lum = underlying.luminance().clamp(0.0, 1.0);
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
                let lum = underlying.luminance().clamp(0.0, 1.0);
                tint.lerp(&Color::WHITE, lum).with_alpha(a)
            }
            Self::Transparency { color, rate_pct } => {
                let f = rate_pct / 100.0;
                underlying.lerp(&color, f).with_alpha(a)
            }
            Self::Wireframe { override_fill, .. } => {
                override_fill.unwrap_or_else(|| underlying.clone())
            }
            Self::FishEye { .. } | Self::Magnify { .. } => underlying.clone(),
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
}
