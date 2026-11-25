// SPDX-License-Identifier: MIT OR Apache-2.0

//! Multilingual text models supporting LTR, Arabic/Hebrew RTL, Kashida justification,
//! linked paragraph text frames, and text fitted along vector paths.

use crate::color::Color;
use crate::ids::ObjectId;
use crate::math::{sanitize_f64, Rect2D, Unit, Vec2D};
use serde::{Deserialize, Serialize};

/// Text bidi reading direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TextDirection {
    /// Automatically detects RTL when the first strong character is Arabic/Hebrew/etc.
    #[default]
    Auto,
    Ltr,
    Rtl,
}

impl TextDirection {
    /// Detects the strong base direction of `text` according to Unicode RTL blocks
    /// (Arabic, Syriac, Thaana, Hebrew, Arabic Presentation Forms).
    #[must_use]
    pub fn detect(text: &str) -> Self {
        for ch in text.chars() {
            if is_rtl_char(ch) {
                return Self::Rtl;
            }
            if ch.is_alphabetic() {
                return Self::Ltr;
            }
        }
        Self::Ltr
    }

    /// Resolves `Auto` into a concrete `Ltr` or `Rtl` using `text`.
    #[must_use]
    pub fn resolve(self, text: &str) -> Self {
        match self {
            Self::Auto => Self::detect(text),
            concrete => concrete,
        }
    }
}

fn is_rtl_char(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        0x0590..=0x05FF   // Hebrew
        | 0x0600..=0x06FF // Arabic
        | 0x0700..=0x074F // Syriac
        | 0x0750..=0x077F // Arabic Supplement
        | 0x0780..=0x07BF // Thaana
        | 0x08A0..=0x08FF // Arabic Extended-A
        | 0xFB1D..=0xFDFF // Hebrew/Arabic Presentation Forms-A
        | 0xFE70..=0xFEFF // Arabic Presentation Forms-B
    )
}

/// Paragraph and artistic text horizontal alignment modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
    /// Arabic Kashida (tatweel) elongation justification.
    ForceJustifyKashida,
}

/// Text wrap behavior around overlapping graphics inside a [`ParagraphText`] frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TextWrapMode {
    #[default]
    None,
    Straddle,
    ContourLeft,
    ContourRight,
    SquareBoth,
    AboveBelow,
}

/// Drop-cap typography configuration for paragraph frames.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DropCapConfig {
    pub enabled: bool,
    pub lines: u32,
    pub gap_mm: f64,
}

impl Default for DropCapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lines: 3,
            gap_mm: 1.5,
        }
    }
}

/// Controllable OpenType layout features passed to text shaping (`liga`, `calt`, `kern`, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OpenTypeFeatures {
    #[serde(default = "default_true")]
    pub liga: bool,
    #[serde(default = "default_true")]
    pub calt: bool,
    #[serde(default = "default_true")]
    pub kern: bool,
    #[serde(default)]
    pub dlig: bool,
    #[serde(default)]
    pub smcp: bool,
}

const fn default_true() -> bool {
    true
}

const fn default_font_weight() -> u16 {
    400
}

impl Default for OpenTypeFeatures {
    fn default() -> Self {
        Self {
            liga: true,
            calt: true,
            kern: true,
            dlig: false,
            smcp: false,
        }
    }
}

/// Horizontal tab-stop alignment within a [`ParagraphText`] frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ParagraphTabAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// Explicit tab-stop position in millimeters within a [`ParagraphText`] frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ParagraphTabStop {
    pub position_mm: f64,
    #[serde(default)]
    pub align: ParagraphTabAlign,
}

/// Bulleted or numbered paragraph list formatting.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ParagraphListStyle {
    #[default]
    None,
    Bullet(String),
    Numbered {
        start: u32,
        suffix: String,
    },
}

/// Character and paragraph formatting attributes applied to a [`TextRun`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub font_family: String,
    pub font_size_pt: f64,
    pub bold: bool,
    #[serde(default = "default_font_weight")]
    pub font_weight: u16,
    #[serde(default)]
    pub bidi_font_family: Option<String>,
    #[serde(default)]
    pub opentype_features: OpenTypeFeatures,
    /// OpenType variable-font (`fvar`) axis tag-value overrides (e.g. `("wdth", 125.0)`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variation_axes: Vec<(String, f32)>,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub letter_spacing_mm: f64,
    pub line_height_ratio: f64,
    pub fill: Color,
    pub direction: TextDirection,
    pub align: TextAlign,
    /// Stretch multiplier for Arabic Kashida elongation (`0.0..=2.0`).
    pub kashida_stretch: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_family: "Noto Sans".to_owned(),
            font_size_pt: 24.0,
            bold: false,
            font_weight: 400,
            bidi_font_family: None,
            opentype_features: OpenTypeFeatures::default(),
            variation_axes: Vec::new(),
            italic: false,
            underline: false,
            strikethrough: false,
            letter_spacing_mm: 0.0,
            line_height_ratio: 1.2,
            fill: Color::BLACK,
            direction: TextDirection::Auto,
            align: TextAlign::Left,
            kashida_stretch: 0.0,
        }
    }
}

impl TextStyle {
    /// Converts `font_size_pt` to canonical millimeters.
    #[must_use]
    pub fn font_size_mm(&self) -> f64 {
        Unit::Point.to_mm(sanitize_f64(self.font_size_pt, 12.0).max(1.0), 96.0)
    }

    /// Resolves effective numeric CSS/OpenType font weight (`100..=900`), honoring
    /// explicit `font_weight != 400` and legacy `bold` toggles cleanly.
    #[must_use]
    pub fn effective_weight(&self) -> u16 {
        let clamped = self.font_weight.clamp(100, 900);
        if clamped != 400 {
            clamped
        } else if self.bold {
            700
        } else {
            400
        }
    }

    /// Sets numeric `font_weight` (`100..=900`) and synchronizes `bold` (`>= 600`).
    pub fn set_weight(&mut self, weight: u16) {
        let clamped = weight.clamp(100, 900);
        self.font_weight = clamped;
        self.bold = clamped >= 600;
    }

    /// Returns the preferred font family for the given directional script context
    /// (`bidi_font_family` when RTL and configured, otherwise `font_family`).
    #[must_use]
    pub fn family_for_direction(&self, dir: TextDirection) -> &str {
        if dir == TextDirection::Rtl {
            if let Some(bidi) = self
                .bidi_font_family
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return bidi;
            }
        }
        self.font_family.as_str()
    }
}

/// Dynamic text field resolved at render/export time (e.g. page numbers, document title).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TextField {
    AutoPageNumber,
    PageCount,
    DocumentTitle,
    CurrentDate,
}

/// Styled contiguous span of Unicode text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextRun {
    pub text: String,
    pub style: TextStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<TextField>,
}

impl TextRun {
    /// Creates a new text run.
    #[must_use]
    pub fn new(text: impl Into<String>, style: TextStyle) -> Self {
        Self {
            text: text.into(),
            style,
            field: None,
        }
    }

    /// Attaches a dynamic [`TextField`] marker to this text run.
    #[must_use]
    pub fn with_field(mut self, field: TextField) -> Self {
        self.field = Some(field);
        self
    }
}

/// Unconstrained artistic display text (headings, logos, signage).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ArtisticText {
    pub runs: Vec<TextRun>,
    pub direction: TextDirection,
    pub align: TextAlign,
}

impl ArtisticText {
    /// Creates an artistic text object from a plain string and style.
    #[must_use]
    pub fn from_plain(text: impl Into<String>, style: TextStyle) -> Self {
        let direction = style.direction;
        let align = style.align;
        Self {
            runs: vec![TextRun::new(text, style)],
            direction,
            align,
        }
    }

    /// Concatenates the plain UTF-8 text of all runs.
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// Resolves effective LTR/RTL direction for the text contents.
    #[must_use]
    pub fn effective_direction(&self) -> TextDirection {
        self.direction.resolve(&self.plain_text())
    }

    /// Conservative metric bounding box estimate in millimeters (used when full font
    /// shaping hasn't been cached by the renderer yet).
    #[must_use]
    pub fn estimated_bounds(&self) -> Rect2D {
        let full = self.plain_text();
        if full.is_empty() {
            return Rect2D::from_xywh(0.0, 0.0, 1.0, 1.0);
        }
        let max_size_mm = self
            .runs
            .iter()
            .map(|r| r.style.font_size_mm())
            .fold(4.0_f64, f64::max);
        let line_ratio = self.runs.first().map_or(1.2, |r| {
            sanitize_f64(r.style.line_height_ratio, 1.2).max(0.5)
        });
        let lines: Vec<&str> = full.split('\n').collect();
        let max_chars = lines
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(1)
            .max(1);
        let est_w = (max_chars as f64) * max_size_mm * 0.6;
        let est_h = (lines.len().max(1) as f64) * max_size_mm * line_ratio;
        Rect2D::from_xywh(0.0, 0.0, est_w, est_h)
    }
}

/// Multi-column framed paragraph text supporting linked frame overflow and wrapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParagraphText {
    pub runs: Vec<TextRun>,
    pub frame_size: Vec2D,
    pub columns: u32,
    pub column_gutter_mm: f64,
    pub linked_next_frame: Option<ObjectId>,
    pub wrap_mode: TextWrapMode,
    pub drop_cap: Option<DropCapConfig>,
    #[serde(default)]
    pub tab_stops_mm: Vec<ParagraphTabStop>,
    #[serde(default)]
    pub list_style: Option<ParagraphListStyle>,
    pub direction: TextDirection,
    pub align: TextAlign,
}

impl ParagraphText {
    /// Creates a single-column paragraph text frame with the given millimeter size.
    #[must_use]
    pub fn new(text: impl Into<String>, frame_size: Vec2D, style: TextStyle) -> Self {
        let direction = style.direction;
        let align = style.align;
        Self {
            runs: vec![TextRun::new(text, style)],
            frame_size: Vec2D::new(
                sanitize_f64(frame_size.x, 80.0).max(1.0),
                sanitize_f64(frame_size.y, 40.0).max(1.0),
            ),
            columns: 1,
            column_gutter_mm: 5.0,
            linked_next_frame: None,
            wrap_mode: TextWrapMode::None,
            drop_cap: None,
            tab_stops_mm: Vec::new(),
            list_style: None,
            direction,
            align,
        }
    }

    /// Concatenates the plain UTF-8 text of all runs.
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// Resolves effective LTR/RTL direction for the paragraph contents.
    #[must_use]
    pub fn effective_direction(&self) -> TextDirection {
        self.direction.resolve(&self.plain_text())
    }

    /// Local-space bounding box matching `frame_size`.
    #[must_use]
    pub fn bounding_box(&self) -> Rect2D {
        Rect2D::from_xywh(
            0.0,
            0.0,
            sanitize_f64(self.frame_size.x, 1.0).max(0.0),
            sanitize_f64(self.frame_size.y, 1.0).max(0.0),
        )
    }
}

/// Artistic text fitted along a referenced vector path object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextOnPath {
    pub text: ArtisticText,
    pub path_id: ObjectId,
    /// Starting offset along the host path in millimeters.
    pub offset_mm: f64,
    /// Perpendicular baseline lift/distance from the host path in millimeters.
    pub distance_mm: f64,
    pub flip_vertical: bool,
    pub flip_horizontal: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtl_arabic_detection_and_kashida_serialization() {
        let arabic = ArtisticText::from_plain(
            "مرحباً بكم في كوري",
            TextStyle {
                direction: TextDirection::Auto,
                align: TextAlign::ForceJustifyKashida,
                kashida_stretch: 0.75,
                ..TextStyle::default()
            },
        );
        assert_eq!(arabic.effective_direction(), TextDirection::Rtl);
        assert!(arabic.estimated_bounds().width() > 0.0);

        let latin = ArtisticText::from_plain("Hello coree", TextStyle::default());
        assert_eq!(latin.effective_direction(), TextDirection::Ltr);

        let para = ParagraphText::new(
            "Multi-column text",
            Vec2D::new(120.0, 60.0),
            TextStyle::default(),
        );
        assert_eq!(para.bounding_box().width(), 120.0);
        let json = serde_json::to_string(&arabic).unwrap();
        let back: ArtisticText = serde_json::from_str(&json).unwrap();
        assert_eq!(arabic, back);
    }
}
