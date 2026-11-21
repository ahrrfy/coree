// SPDX-License-Identifier: MPL-2.0

//! Clean-room design system tokens (`Dark Studio`, `Classic Light`, `High Contrast`),
//! WCAG AA semantic palette, layout metrics, `egui` / `egui_dock` theme application,
//! and runtime Arabic-capable system font loader.

use coree_core::Color;
use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Margin, Stroke, Visuals};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Exact layout metrics from `replica/design/tokens.json`.
pub const MENUBAR_HEIGHT: f32 = 28.0;
pub const PROPERTY_BAR_HEIGHT: f32 = 34.0;
pub const TOOLBOX_WIDTH: f32 = 44.0;
pub const RULER_THICKNESS: f32 = 20.0;
pub const PAGE_TABS_HEIGHT: f32 = 26.0;
pub const STATUS_BAR_HEIGHT: f32 = 26.0;
pub const PALETTE_BAR_WIDTH: f32 = 32.0;
pub const DOCKER_DEFAULT_WIDTH: f32 = 300.0;
pub const DOCKER_MIN_WIDTH: f32 = 240.0;
pub const DOCKER_MAX_WIDTH: f32 = 520.0;

/// Selectable studio visual theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum StudioTheme {
    /// Default color-neutral Dark Studio (`bg=#111318`, Electric Cobalt accent `#3ea6ff`).
    #[default]
    DarkStudio,
    /// Bright daylight Classic Light studio theme.
    ClassicLight,
    /// WCAG AAA High Contrast accessibility theme.
    HighContrast,
}

impl StudioTheme {
    /// All available themes in UI menu order.
    pub const ALL: [Self; 3] = [Self::DarkStudio, Self::ClassicLight, Self::HighContrast];

    /// Display label for menus & Preferences.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DarkStudio => "Dark Studio",
            Self::ClassicLight => "Classic Light",
            Self::HighContrast => "High Contrast",
        }
    }

    /// Resolves semantic color tokens for this theme.
    #[must_use]
    pub const fn tokens(self) -> ThemeTokens {
        match self {
            Self::DarkStudio => ThemeTokens::dark_studio(),
            Self::ClassicLight => ThemeTokens::classic_light(),
            Self::HighContrast => ThemeTokens::high_contrast(),
        }
    }
}

/// Complete semantic color palette consumed by all `coree-app` widgets (zero raw hex literals in widgets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeTokens {
    pub is_dark: bool,
    pub bg: Color32,
    pub canvas_well: Color32,
    pub surface: Color32,
    pub surface_elevated: Color32,
    pub border: Color32,
    pub border_input: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub accent_secondary: Color32,
    pub on_accent_secondary: Color32,
    pub danger: Color32,
    pub on_danger: Color32,
    pub success: Color32,
    pub on_success: Color32,
    pub warning: Color32,
    pub on_warning: Color32,
    pub cutcontour: Color32,
}

impl ThemeTokens {
    /// Exact Dark Studio tokens from `replica/design/tokens.json`.
    #[must_use]
    pub const fn dark_studio() -> Self {
        Self {
            is_dark: true,
            bg: Color32::from_rgb(0x11, 0x13, 0x18),
            canvas_well: Color32::from_rgb(0x0c, 0x0e, 0x12),
            surface: Color32::from_rgb(0x19, 0x1c, 0x24),
            surface_elevated: Color32::from_rgb(0x23, 0x27, 0x34),
            border: Color32::from_rgb(0x2e, 0x34, 0x45),
            border_input: Color32::from_rgb(0x7c, 0x87, 0xa3),
            text: Color32::from_rgb(0xf1, 0xf4, 0xf9),
            text_muted: Color32::from_rgb(0xad, 0xb6, 0xca),
            accent: Color32::from_rgb(0x3e, 0xa6, 0xff),
            accent_hover: Color32::from_rgb(0x6b, 0xc0, 0xff),
            on_accent: Color32::from_rgb(0x07, 0x10, 0x1c),
            accent_secondary: Color32::from_rgb(0xf6, 0xad, 0x3c),
            on_accent_secondary: Color32::from_rgb(0x1c, 0x11, 0x02),
            danger: Color32::from_rgb(0xff, 0x70, 0x78),
            on_danger: Color32::from_rgb(0x1a, 0x05, 0x07),
            success: Color32::from_rgb(0x38, 0xd9, 0xa9),
            on_success: Color32::from_rgb(0x04, 0x1a, 0x13),
            warning: Color32::from_rgb(0xfb, 0xbf, 0x24),
            on_warning: Color32::from_rgb(0x1a, 0x12, 0x02),
            cutcontour: Color32::from_rgb(0xff, 0x66, 0xc4),
        }
    }

    /// Neutral daylight Classic Light studio theme with Electric Cobalt focus accent.
    #[must_use]
    pub const fn classic_light() -> Self {
        Self {
            is_dark: false,
            bg: Color32::from_rgb(0xf0, 0xf2, 0xf5),
            canvas_well: Color32::from_rgb(0xdc, 0xe0, 0xe8),
            surface: Color32::from_rgb(0xf8, 0xf9, 0xfb),
            surface_elevated: Color32::from_rgb(0xff, 0xff, 0xff),
            border: Color32::from_rgb(0xc8, 0xd0, 0xdc),
            border_input: Color32::from_rgb(0x64, 0x70, 0x85),
            text: Color32::from_rgb(0x14, 0x18, 0x20),
            text_muted: Color32::from_rgb(0x4b, 0x55, 0x67),
            accent: Color32::from_rgb(0x0b, 0x66, 0xc2),
            accent_hover: Color32::from_rgb(0x1d, 0x7b, 0xd8),
            on_accent: Color32::from_rgb(0xff, 0xff, 0xff),
            accent_secondary: Color32::from_rgb(0xb4, 0x53, 0x09),
            on_accent_secondary: Color32::from_rgb(0xff, 0xff, 0xff),
            danger: Color32::from_rgb(0xc8, 0x1e, 0x2b),
            on_danger: Color32::from_rgb(0xff, 0xff, 0xff),
            success: Color32::from_rgb(0x0f, 0x76, 0x6e),
            on_success: Color32::from_rgb(0xff, 0xff, 0xff),
            warning: Color32::from_rgb(0xb4, 0x69, 0x0e),
            on_warning: Color32::from_rgb(0xff, 0xff, 0xff),
            cutcontour: Color32::from_rgb(0xd6, 0x1f, 0x8a),
        }
    }

    /// Maximum-contrast accessibility theme for low-vision & studio projection environments.
    #[must_use]
    pub const fn high_contrast() -> Self {
        Self {
            is_dark: true,
            bg: Color32::from_rgb(0x00, 0x00, 0x00),
            canvas_well: Color32::from_rgb(0x05, 0x05, 0x08),
            surface: Color32::from_rgb(0x0a, 0x0a, 0x0e),
            surface_elevated: Color32::from_rgb(0x16, 0x16, 0x20),
            border: Color32::from_rgb(0x88, 0x99, 0xbb),
            border_input: Color32::from_rgb(0xff, 0xff, 0xff),
            text: Color32::from_rgb(0xff, 0xff, 0xff),
            text_muted: Color32::from_rgb(0xdd, 0xe4, 0xf2),
            accent: Color32::from_rgb(0x55, 0xc8, 0xff),
            accent_hover: Color32::from_rgb(0x88, 0xde, 0xff),
            on_accent: Color32::from_rgb(0x00, 0x00, 0x00),
            accent_secondary: Color32::from_rgb(0xff, 0xcc, 0x33),
            on_accent_secondary: Color32::from_rgb(0x00, 0x00, 0x00),
            danger: Color32::from_rgb(0xff, 0x66, 0x70),
            on_danger: Color32::from_rgb(0x00, 0x00, 0x00),
            success: Color32::from_rgb(0x44, 0xff, 0xb8),
            on_success: Color32::from_rgb(0x00, 0x00, 0x00),
            warning: Color32::from_rgb(0xff, 0xd8, 0x44),
            on_warning: Color32::from_rgb(0x00, 0x00, 0x00),
            cutcontour: Color32::from_rgb(0xff, 0x77, 0xdd),
        }
    }

    /// Converts `canvas_well` into a `coree_core::Color` for `SceneRenderer::RenderOptions`.
    #[must_use]
    pub fn canvas_well_core_color(&self) -> Color {
        Color::rgba8(
            self.canvas_well.r(),
            self.canvas_well.g(),
            self.canvas_well.b(),
            255,
        )
    }
}

/// Applies `theme` onto `ctx`'s `egui::Style` and `egui::Visuals`.
pub fn apply_theme(ctx: &egui::Context, theme: StudioTheme) {
    let t = theme.tokens();
    let mut visuals = if t.is_dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    visuals.dark_mode = t.is_dark;
    visuals.override_text_color = Some(t.text);
    visuals.weak_text_color = Some(t.text_muted);
    visuals.panel_fill = t.surface;
    visuals.window_fill = t.surface_elevated;
    visuals.extreme_bg_color = t.bg;
    visuals.faint_bg_color = t.surface;
    visuals.code_bg_color = t.bg;
    visuals.window_stroke = Stroke::new(1.0, t.border_input);
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.menu_corner_radius = CornerRadius::same(6);
    visuals.hyperlink_color = t.accent;
    visuals.warn_fg_color = t.warning;
    visuals.error_fg_color = t.danger;

    visuals.selection.bg_fill = t.accent.gamma_multiply(0.32);
    visuals.selection.stroke = Stroke::new(1.5, t.accent);

    visuals.widgets.noninteractive.bg_fill = t.surface;
    visuals.widgets.noninteractive.weak_bg_fill = t.surface;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, t.text);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(4);

    visuals.widgets.inactive.bg_fill = t.surface_elevated;
    visuals.widgets.inactive.weak_bg_fill = t.surface_elevated;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, t.border);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, t.text);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(5);

    visuals.widgets.hovered.bg_fill = t.surface_elevated;
    visuals.widgets.hovered.weak_bg_fill = t.surface_elevated;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, t.accent);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.2, t.text);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(5);

    visuals.widgets.active.bg_fill = t.accent;
    visuals.widgets.active.weak_bg_fill = t.accent;
    visuals.widgets.active.bg_stroke = Stroke::new(1.5, t.accent_hover);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5, t.on_accent);
    visuals.widgets.active.corner_radius = CornerRadius::same(5);

    visuals.widgets.open.bg_fill = t.surface_elevated;
    visuals.widgets.open.weak_bg_fill = t.surface_elevated;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, t.accent);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, t.text);
    visuals.widgets.open.corner_radius = CornerRadius::same(5);

    ctx.set_visuals(visuals);
    let egui_theme = ctx.theme();
    ctx.style_mut_of(egui_theme, |style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(6.0, 3.0);
        style.spacing.window_margin = Margin::same(8);
    });
}

/// Builds a token-matched [`egui_dock::Style`] for rearrangeable studio dockers.
#[must_use]
pub fn dock_style(tokens: &ThemeTokens, egui_style: &egui::Style) -> egui_dock::Style {
    let mut style = egui_dock::Style::from_egui(egui_style);
    style.dock_area_padding = Some(Margin::same(0));
    style.main_surface_border_stroke = Stroke::new(1.0, tokens.border);
    style.main_surface_border_rounding = CornerRadius::ZERO;

    style.tab_bar.bg_fill = tokens.bg;
    style.tab_bar.hline_color = tokens.border;
    style.tab_bar.height = 28.0;

    style.tab.tab_body.bg_fill = tokens.surface;
    style.tab.tab_body.stroke = Stroke::new(1.0, tokens.border);
    style.tab.tab_body.inner_margin = Margin::same(8);

    style.tab.active.bg_fill = tokens.surface_elevated;
    style.tab.active.text_color = tokens.text;
    style.tab.active.outline_color = tokens.accent;

    style.tab.inactive.bg_fill = tokens.bg;
    style.tab.inactive.text_color = tokens.text_muted;
    style.tab.inactive.outline_color = tokens.border;

    style.tab.focused.bg_fill = tokens.surface_elevated;
    style.tab.focused.text_color = tokens.accent;
    style.tab.focused.outline_color = tokens.accent;

    style.tab.hovered.bg_fill = tokens.surface;
    style.tab.hovered.text_color = tokens.text;

    style.separator.color_idle = tokens.border;
    style.separator.color_hovered = tokens.accent;
    style.separator.color_dragged = tokens.accent_hover;
    style
}

/// Searches standard system font paths for an Arabic-capable font (`Noto Sans Arabic`,
/// `Noto Naskh Arabic`, `DejaVu Sans`, etc.) and appends it as a fallback to `egui`'s
/// `Proportional` and `Monospace` font families. Fails softly when unavailable.
pub fn configure_arabic_capable_fonts(ctx: &egui::Context) -> Option<PathBuf> {
    let font_path = find_arabic_system_font()?;
    let bytes = std::fs::read(&font_path).ok()?;
    let mut defs = FontDefinitions::default();
    let key = "coree_arabic_fallback".to_owned();
    defs.font_data
        .insert(key.clone(), Arc::new(FontData::from_owned(bytes)));
    if let Some(fam) = defs.families.get_mut(&FontFamily::Proportional) {
        fam.push(key.clone());
    }
    if let Some(fam) = defs.families.get_mut(&FontFamily::Monospace) {
        fam.push(key);
    }
    ctx.set_fonts(defs);
    Some(font_path)
}

/// Locates a preferred Arabic-capable `.ttf`/`.otf` font file on Linux/Windows/macOS.
#[must_use]
pub fn find_arabic_system_font() -> Option<PathBuf> {
    let preferred_paths = [
        "/usr/share/fonts/noto/NotoSansArabic-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf",
        "/usr/share/fonts/google-noto/NotoSansArabic-Regular.ttf",
        "/usr/share/fonts/noto/NotoNaskhArabic-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoNaskhArabic-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
        "C:\\Windows\\Fonts\\arial.ttf",
        "C:\\Windows\\Fonts\\tahoma.ttf",
        "/System/Library/Fonts/GeezaPro.ttc",
    ];
    for p in preferred_paths {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }

    // Shallow recursive fallback search under `/usr/share/fonts`
    let root = PathBuf::from("/usr/share/fonts");
    if root.is_dir() {
        return search_font_dir(&root, 0);
    }
    None
}

fn search_font_dir(dir: &PathBuf, depth: usize) -> Option<PathBuf> {
    if depth > 4 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut fallback_dejavu = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = search_font_dir(&path, depth + 1) {
                return Some(found);
            }
        } else if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".ttf") || lower.ends_with(".otf") {
                if lower.contains("arabic") && lower.contains("regular") {
                    return Some(path);
                }
                if lower == "dejavusans.ttf" && fallback_dejavu.is_none() {
                    fallback_dejavu = Some(path);
                }
            }
        }
    }
    fallback_dejavu
}

/// Converts a `coree_core::Color` into an `egui::Color32` (unmultiplied sRGB).
#[must_use]
pub fn core_color_to_egui(c: &Color) -> Color32 {
    let [r, g, b, a] = c.to_rgba8();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// Converts an `egui::Color32` into a `coree_core::Color::Rgba`.
#[must_use]
pub fn egui_color_to_core(c: Color32) -> Color {
    Color::rgba8(c.r(), c.g(), c.b(), c.a())
}
