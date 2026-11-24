// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fill models, outline stroke styling, blend modes, and reusable object styles.

use crate::color::{Color, ColorStyleId};
use crate::ids::{ImageAssetId, ObjectStyleId, SymbolId};
use crate::math::{sanitize_f64, Affine2D, Point2D};
use serde::{Deserialize, Serialize};

/// Canonical hairline stroke width in millimeters (`0.003 in = 0.0762 mm`).
pub const HAIRLINE_WIDTH_MM: f64 = 0.0762;

/// A single color stop along a fountain/gradient fill (`offset` in `0.0..=1.0`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    pub offset: f32,
    pub color: Color,
}

impl GradientStop {
    /// Creates a gradient stop with offset clamped to `0.0..=1.0`.
    #[must_use]
    pub fn new(offset: f32, color: Color) -> Self {
        let safe = if offset.is_finite() {
            offset.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            offset: safe,
            color,
        }
    }
}

/// Control node inside a [`Fill::MeshFill`] color interpolation grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeshNode {
    pub row: usize,
    pub col: usize,
    pub pos: Point2D,
    pub color: Color,
}

/// Built-in PostScript-style procedural vector/hatching fill presets encoded via
/// [`Fill::TextureFill`] with `"ps:<id>"` preset identifiers for 100% enum compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum PostScriptFillPreset {
    #[default]
    Hatch,
    CrossHatch,
    Checker,
    PolkaDots,
    Waves,
    Bricks,
    RadialBurst,
    HexGrid,
    Woodgrain,
    HalftoneDots,
}

impl PostScriptFillPreset {
    /// All 10 built-in PostScript procedural fill presets.
    pub const ALL: [Self; 10] = [
        Self::Hatch,
        Self::CrossHatch,
        Self::Checker,
        Self::PolkaDots,
        Self::Waves,
        Self::Bricks,
        Self::RadialBurst,
        Self::HexGrid,
        Self::Woodgrain,
        Self::HalftoneDots,
    ];

    /// Canonical lowercase identifier suffix (without the `"ps:"` prefix).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Hatch => "hatch",
            Self::CrossHatch => "cross_hatch",
            Self::Checker => "checker",
            Self::PolkaDots => "polka_dots",
            Self::Waves => "waves",
            Self::Bricks => "bricks",
            Self::RadialBurst => "radial_burst",
            Self::HexGrid => "hex_grid",
            Self::Woodgrain => "woodgrain",
            Self::HalftoneDots => "halftone_dots",
        }
    }

    /// Human-readable display label for UI pickers.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Hatch => "Hatch",
            Self::CrossHatch => "Cross-Hatch",
            Self::Checker => "Checker",
            Self::PolkaDots => "Polka Dots",
            Self::Waves => "Waves",
            Self::Bricks => "Bricks",
            Self::RadialBurst => "Radial Burst",
            Self::HexGrid => "Hex Grid",
            Self::Woodgrain => "Woodgrain",
            Self::HalftoneDots => "Halftone Dots",
        }
    }

    /// Encoded preset string (`"ps:<id>"`) stored in [`Fill::TextureFill::preset`].
    #[must_use]
    pub fn preset_str(self) -> String {
        format!("ps:{}", self.id())
    }

    /// Encodes this PostScript preset as a backwards-compatible [`Fill::TextureFill`].
    #[must_use]
    pub fn to_fill(self, fg: Color, bg: Color, seed: u64) -> Fill {
        Fill::TextureFill {
            preset: self.preset_str(),
            seed,
            fg,
            bg,
        }
    }

    /// Parses a `"ps:<id>"` preset string into its typed [`PostScriptFillPreset`].
    ///
    /// Returns `None` for non-PostScript texture presets (such as `"clouds"` or `"marble"`).
    #[must_use]
    pub fn from_preset_str(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        let rest = trimmed
            .strip_prefix("ps:")
            .or_else(|| trimmed.strip_prefix("PS:"))?;
        let normalized = rest.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "hatch" => Some(Self::Hatch),
            "cross_hatch" | "crosshatch" => Some(Self::CrossHatch),
            "checker" | "checkerboard" => Some(Self::Checker),
            "polka_dots" | "polkadots" | "dots" => Some(Self::PolkaDots),
            "waves" | "wave" => Some(Self::Waves),
            "bricks" | "brick" => Some(Self::Bricks),
            "radial_burst" | "radialburst" | "sunburst" => Some(Self::RadialBurst),
            "hex_grid" | "hexgrid" | "honeycomb" => Some(Self::HexGrid),
            "woodgrain" | "wood_grain" => Some(Self::Woodgrain),
            "halftone_dots" | "halftonedots" | "halftone" => Some(Self::HalftoneDots),
            _ => None,
        }
    }
}

/// Complete vector fill model covering solid, fountain/gradient, pattern, texture, and mesh fills.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum Fill {
    #[default]
    None,
    Solid(Color),
    LinearGradient {
        start: Point2D,
        end: Point2D,
        stops: Vec<GradientStop>,
    },
    RadialGradient {
        center: Point2D,
        radius: f64,
        focal: Point2D,
        stops: Vec<GradientStop>,
    },
    ConicalGradient {
        center: Point2D,
        angle_deg: f64,
        stops: Vec<GradientStop>,
    },
    SquareGradient {
        center: Point2D,
        size: f64,
        angle_deg: f64,
        stops: Vec<GradientStop>,
    },
    TwoColorPattern {
        fg: Color,
        bg: Color,
        pattern_name: String,
        tile_w: f64,
        tile_h: f64,
    },
    VectorPattern {
        symbol_id: SymbolId,
        tile_w: f64,
        tile_h: f64,
    },
    BitmapPattern {
        image_id: ImageAssetId,
        tile_w: f64,
        tile_h: f64,
    },
    TextureFill {
        preset: String,
        seed: u64,
        fg: Color,
        bg: Color,
    },
    MeshFill {
        rows: usize,
        cols: usize,
        nodes: Vec<MeshNode>,
    },
}

impl Fill {
    /// Returns `true` if this fill paints visible ink/pixels.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Collects all direct [`Color`] values referenced by this fill (used to populate the Document Palette).
    #[must_use]
    pub fn referenced_colors(&self) -> Vec<Color> {
        match self {
            Self::None | Self::VectorPattern { .. } | Self::BitmapPattern { .. } => Vec::new(),
            Self::Solid(c) => vec![c.clone()],
            Self::LinearGradient { stops, .. }
            | Self::RadialGradient { stops, .. }
            | Self::ConicalGradient { stops, .. }
            | Self::SquareGradient { stops, .. } => stops.iter().map(|s| s.color.clone()).collect(),
            Self::TwoColorPattern { fg, bg, .. } | Self::TextureFill { fg, bg, .. } => {
                vec![fg.clone(), bg.clone()]
            }
            Self::MeshFill { nodes, .. } => nodes.iter().map(|n| n.color.clone()).collect(),
        }
    }

    /// Evaluates a gradient stop ramp at normalized position `t` in `0.0..=1.0`.
    #[must_use]
    pub fn sample_stops(stops: &[GradientStop], t: f32) -> Color {
        if stops.is_empty() {
            return Color::TRANSPARENT;
        }
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if stops.len() == 1 || t <= stops[0].offset {
            return stops[0].color.clone();
        }
        for win in stops.windows(2) {
            let a = &win[0];
            let b = &win[1];
            if t >= a.offset && t <= b.offset {
                let span = b.offset - a.offset;
                if span.abs() <= 1e-6 {
                    return b.color.clone();
                }
                return a.color.lerp(&b.color, (t - a.offset) / span);
            }
        }
        stops.last().map_or(Color::TRANSPARENT, |s| s.color.clone())
    }
}

/// Stroke endpoint cap geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

/// Stroke corner join geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

/// Start/end marker arrowheads attached to open strokes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ArrowheadStyle {
    #[default]
    None,
    Triangle,
    OpenVee,
    Diamond,
    Circle,
    Square,
    Stealth,
    Custom(String),
}

/// Complete vector outline/stroke configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutlineStyle {
    pub color: Color,
    pub width_mm: f64,
    pub is_hairline: bool,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f64,
    pub dash_pattern: Vec<f64>,
    pub start_arrow: ArrowheadStyle,
    pub end_arrow: ArrowheadStyle,
    pub behind_fill: bool,
    pub scale_with_object: bool,
    pub overprint: bool,
    pub calligraphy_angle_deg: f64,
    pub calligraphy_stretch: f64,
}

impl Default for OutlineStyle {
    fn default() -> Self {
        Self::solid(Color::BLACK, 0.2)
    }
}

impl OutlineStyle {
    /// No outline (`width_mm = 0.0`, `is_hairline = false`).
    #[must_use]
    pub fn none() -> Self {
        Self {
            color: Color::TRANSPARENT,
            width_mm: 0.0,
            is_hairline: false,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 4.0,
            dash_pattern: Vec::new(),
            start_arrow: ArrowheadStyle::None,
            end_arrow: ArrowheadStyle::None,
            behind_fill: false,
            scale_with_object: false,
            overprint: false,
            calligraphy_angle_deg: 0.0,
            calligraphy_stretch: 1.0,
        }
    }

    /// Standard hairline stroke (used by laser cutters, plotters, and fine vector work).
    #[must_use]
    pub fn hairline(color: Color) -> Self {
        Self {
            color,
            width_mm: HAIRLINE_WIDTH_MM,
            is_hairline: true,
            ..Self::none()
        }
    }

    /// Standard print/cutter `CutContour` spot hairline with overprint enabled.
    #[must_use]
    pub fn cut_contour() -> Self {
        Self {
            overprint: true,
            ..Self::hairline(Color::cut_contour_spot())
        }
    }

    /// Solid outline with explicit width in millimeters.
    #[must_use]
    pub fn solid(color: Color, width_mm: f64) -> Self {
        Self {
            color,
            width_mm: sanitize_f64(width_mm, 0.0).max(0.0),
            is_hairline: false,
            ..Self::none()
        }
    }

    /// Effective stroke width in millimeters (`HAIRLINE_WIDTH_MM` when `is_hairline` is true).
    #[must_use]
    pub fn effective_width_mm(&self) -> f64 {
        if self.is_hairline {
            HAIRLINE_WIDTH_MM
        } else {
            sanitize_f64(self.width_mm, 0.0).max(0.0)
        }
    }

    /// Computes effective millimeter stroke width under `xf`, scaling isotropically when
    /// `scale_with_object` is enabled (`1.0` scale otherwise).
    #[must_use]
    pub fn scaled_width_mm_for_transform(&self, xf: &Affine2D) -> f64 {
        let base = self.effective_width_mm();
        if !self.scale_with_object {
            return base;
        }
        let sx = sanitize_f64(xf.m[0].hypot(xf.m[1]), 1.0);
        let sy = sanitize_f64(xf.m[2].hypot(xf.m[3]), 1.0);
        let scale = (0.5 * (sx + sy)).clamp(1e-4, 1e6);
        sanitize_f64(base * scale, base).max(0.0)
    }

    /// Returns `true` if this outline has positive width and non-zero alpha.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        (self.is_hairline || self.effective_width_mm() > 1e-6) && self.color.alpha() > 1e-6
    }
}

/// Porter-Duff / PDF / studio compositing blend modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    Difference,
    Exclusion,
    SoftLight,
    HardLight,
    ColorDodge,
    ColorBurn,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

/// Reusable named object style capturing fill, outline, blend mode, and opacity overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectStyle {
    pub id: ObjectStyleId,
    pub name: String,
    pub fill: Option<Fill>,
    pub outline: Option<OutlineStyle>,
    pub blend_mode: Option<BlendMode>,
    pub opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_style_id: Option<ColorStyleId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_style_slot: Option<usize>,
}

impl ObjectStyle {
    /// Creates a new empty object style.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: ObjectStyleId::new(),
            name: name.into(),
            fill: None,
            outline: None,
            blend_mode: None,
            opacity: None,
            color_style_id: None,
            color_style_slot: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_sampling_and_outline_helpers() {
        let stops = vec![
            GradientStop::new(0.0, Color::BLACK),
            GradientStop::new(1.0, Color::WHITE),
        ];
        let mid = Fill::sample_stops(&stops, 0.5);
        assert!((mid.to_rgba_f32()[0] - 0.5).abs() < 1e-5);

        let cut = OutlineStyle::cut_contour();
        assert!(cut.is_hairline);
        assert!(cut.overprint);
        assert!(cut.is_visible());
        assert!((cut.effective_width_mm() - HAIRLINE_WIDTH_MM).abs() < 1e-9);
        assert!(!OutlineStyle::none().is_visible());

        for preset in PostScriptFillPreset::ALL {
            let fill = preset.to_fill(Color::BLACK, Color::WHITE, 42);
            match fill {
                Fill::TextureFill {
                    preset: p_str,
                    seed,
                    ..
                } => {
                    assert!(p_str.starts_with("ps:"));
                    assert_eq!(seed, 42);
                    assert_eq!(PostScriptFillPreset::from_preset_str(&p_str), Some(preset));
                }
                _ => panic!("expected Fill::TextureFill"),
            }
        }
        assert_eq!(PostScriptFillPreset::from_preset_str("clouds"), None);
        assert_eq!(PostScriptFillPreset::from_preset_str("ps:unknown"), None);
    }
}
