// SPDX-License-Identifier: MIT OR Apache-2.0

//! Multilingual text shaping, Arabic RTL/BiDi/Kashida justification, multi-column
//! paragraph layout, Fit Text to Path, and Text-to-Curves vector outline extraction.

use crate::backend::{BackendPaint, CanvasBackend, FillRule, RgbaBuffer, StrokeParams};
use crate::renderer::icc_srgb;
use crate::viewport::ViewMode;
use coree_core::math::{sanitize_f32, sanitize_f64};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, BlendMode, Color, Document, IccColorPipeline, NodePayload,
    ObjectId, OutlineStyle, ParagraphListStyle, ParagraphTabAlign, ParagraphTabStop, ParagraphText,
    Rect2D, TextAlign, TextDirection, TextOnPath, TextRun, TextStyle, TextWrapMode, Vec2D,
};
use cosmic_text::{
    Align, Attrs, Buffer, CacheKey, CacheKeyFlags, Command, Family, FeatureTag, FontFeatures,
    FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight, Wrap,
};
use kurbo::BezPath;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use swash::scale::ScaleContext;
use swash::zeno::PathData as _;

fn shared_base_font_db() -> &'static (
    String,
    cosmic_text::fontdb::Database,
    HashSet<String>,
    usize,
) {
    static BASE_DB: OnceLock<(
        String,
        cosmic_text::fontdb::Database,
        HashSet<String>,
        usize,
    )> = OnceLock::new();
    BASE_DB.get_or_init(|| {
        let fs = FontSystem::new();
        let mut families = HashSet::new();
        let mut face_count = 0usize;
        for face in fs.db().faces() {
            face_count += 1;
            for (name, _) in &face.families {
                families.insert(name.to_ascii_lowercase());
            }
        }
        (
            fs.locale().to_owned(),
            fs.db().clone(),
            families,
            face_count,
        )
    })
}

/// Internal layout resolution scale (logical pixels per millimeter) used during
/// unhinted glyph outline extraction so coordinates convert cleanly back to mm.
const LAYOUT_UNITS_PER_MM: f32 = 10.0;

/// Colored vector glyph geometry plus optional bitmap glyph fallback (e.g. color emoji).
#[derive(Debug, Clone)]
pub struct ShapedGlyphDraw {
    pub path: BezierPath,
    pub fill: Color,
}

/// Single shaped glyph positioned in local object coordinates (mm, Y-down).
#[derive(Debug, Clone)]
pub struct ShapedPdfGlyph {
    pub glyph_id: u16,
    pub x_mm: f64,
    pub y_mm: f64,
    pub advance_mm: f64,
    pub unicode: String,
}

/// Shaped PDF text run sharing one SFNT font face, point size, fill color, and direction.
#[derive(Debug, Clone)]
pub struct ShapedPdfRun {
    pub font_bytes: std::sync::Arc<[u8]>,
    pub face_index: u32,
    pub postscript_name: String,
    pub units_per_em: u16,
    pub font_size_pt: f64,
    pub fill: Color,
    pub is_rtl: bool,
    pub actual_text: Option<String>,
    pub glyphs: Vec<ShapedPdfGlyph>,
}

type PdfFontBlobEntry = (std::sync::Arc<[u8]>, u32, String, u16);

/// Persistent typography engine holding `cosmic-text`'s `FontSystem` and `SwashCache`.
pub struct TextEngine {
    font_system: FontSystem,
    swash_cache: SwashCache,
    var_scale_ctx: ScaleContext,
    arabic_fallback_family: Option<String>,
    font_blob_cache: HashMap<(cosmic_text::fontdb::ID, u16), Option<PdfFontBlobEntry>>,
    base_families_lower: &'static HashSet<String>,
    base_face_count: usize,
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEngine {
    /// Initializes the text engine reusing the cached OS font catalog (sub-millisecond per tab/worker).
    #[must_use]
    pub fn new() -> Self {
        let (locale, db, base_families_lower, base_face_count) = shared_base_font_db();
        let font_system = FontSystem::new_with_locale_and_db(locale.clone(), db.clone());
        let arabic_fallback_family = detect_arabic_font_family(&font_system);
        Self {
            font_system,
            swash_cache: SwashCache::new(),
            var_scale_ctx: ScaleContext::new(),
            arabic_fallback_family,
            font_blob_cache: HashMap::new(),
            base_families_lower,
            base_face_count: *base_face_count,
        }
    }

    /// Mutable access to the underlying `FontSystem` (e.g. to load custom project fonts).
    pub fn font_system_mut(&mut self) -> &mut FontSystem {
        &mut self.font_system
    }

    /// Converts an [`ArtisticText`] object into editable vector [`BezierPath`] curves in local mm.
    #[must_use]
    pub fn artistic_text_to_curves(&mut self, text: &ArtisticText) -> BezierPath {
        let pieces = self.layout_artistic_pieces(text);
        merge_paths(pieces.into_iter().map(|p| p.path))
    }

    /// Converts a multi-column [`ParagraphText`] frame into editable vector [`BezierPath`] curves in local mm.
    #[must_use]
    pub fn paragraph_text_to_curves(&mut self, para: &ParagraphText) -> BezierPath {
        let pieces = self.layout_paragraph_pieces(para);
        merge_paths(pieces.into_iter().map(|p| p.path))
    }

    /// Fits shaped text along `host_path` and returns the combined vector [`BezierPath`] curves.
    ///
    /// Preserves Arabic contextual joining, ligatures, and diacritics by shaping the
    /// complete text runs first before mapping each shaped glyph cluster onto the curve.
    #[must_use]
    pub fn text_on_path_to_curves(
        &mut self,
        top: &TextOnPath,
        host_path: &BezierPath,
    ) -> BezierPath {
        let pieces = self.layout_text_on_path_pieces(top, host_path);
        merge_paths(pieces.into_iter().map(|p| p.path))
    }

    /// Renders [`ArtisticText`] onto `backend` using `world_to_screen` and object styling.
    #[allow(clippy::too_many_arguments)]
    pub fn render_artistic_text(
        &mut self,
        backend: &mut dyn CanvasBackend,
        text: &ArtisticText,
        transform: Affine2D,
        outline: &OutlineStyle,
        opacity: f32,
        blend_mode: BlendMode,
        view_mode: ViewMode,
        px_per_mm: f64,
    ) {
        self.render_artistic_text_icc(
            backend,
            text,
            transform,
            outline,
            opacity,
            blend_mode,
            view_mode,
            px_per_mm,
            &IccColorPipeline::default(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_artistic_text_icc(
        &mut self,
        backend: &mut dyn CanvasBackend,
        text: &ArtisticText,
        transform: Affine2D,
        outline: &OutlineStyle,
        opacity: f32,
        blend_mode: BlendMode,
        view_mode: ViewMode,
        px_per_mm: f64,
        icc: &IccColorPipeline,
    ) {
        let pieces = self.layout_artistic_pieces(text);
        draw_text_pieces_icc(
            backend, &pieces, transform, outline, opacity, blend_mode, view_mode, px_per_mm, icc,
        );
        // Bitmap fallback only if vector outline extraction yielded nothing for non-empty text
        if pieces.is_empty() && !text.plain_text().trim().is_empty() {
            self.render_bitmap_fallback(
                backend,
                &text.runs,
                None,
                text.effective_direction(),
                text.align,
                transform,
                opacity,
                blend_mode,
                icc,
            );
        }
    }

    /// Renders a multi-column [`ParagraphText`] frame onto `backend`.
    #[allow(clippy::too_many_arguments)]
    pub fn render_paragraph_text(
        &mut self,
        backend: &mut dyn CanvasBackend,
        para: &ParagraphText,
        transform: Affine2D,
        outline: &OutlineStyle,
        opacity: f32,
        blend_mode: BlendMode,
        view_mode: ViewMode,
        px_per_mm: f64,
    ) {
        let frame_path = BezierPath::from_rect(para.bounding_box());
        let wire = matches!(view_mode, ViewMode::Wireframe | ViewMode::SimpleWireframe);
        if !wire {
            backend.push_clip(&frame_path, FillRule::Winding, transform);
        }
        let pieces = self.layout_paragraph_pieces(para);
        draw_text_pieces_icc(
            backend,
            &pieces,
            transform,
            outline,
            opacity,
            blend_mode,
            view_mode,
            px_per_mm,
            &IccColorPipeline::default(),
        );
        if !wire {
            backend.pop_clip();
        }
    }

    #[allow(dead_code, clippy::too_many_arguments)]
    pub(crate) fn render_paragraph_text_in_doc_icc(
        &mut self,
        backend: &mut dyn CanvasBackend,
        doc: &Document,
        obj_id: ObjectId,
        para: &ParagraphText,
        transform: Affine2D,
        outline: &OutlineStyle,
        opacity: f32,
        blend_mode: BlendMode,
        view_mode: ViewMode,
        px_per_mm: f64,
        icc: &IccColorPipeline,
    ) {
        self.render_paragraph_text_in_doc_on_page_icc(
            backend,
            doc,
            obj_id,
            para,
            doc.active_page_index(),
            transform,
            outline,
            opacity,
            blend_mode,
            view_mode,
            px_per_mm,
            icc,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_paragraph_text_in_doc_on_page_icc(
        &mut self,
        backend: &mut dyn CanvasBackend,
        doc: &Document,
        obj_id: ObjectId,
        para: &ParagraphText,
        page_index: usize,
        transform: Affine2D,
        outline: &OutlineStyle,
        opacity: f32,
        blend_mode: BlendMode,
        view_mode: ViewMode,
        px_per_mm: f64,
        icc: &IccColorPipeline,
    ) {
        let frame_path = BezierPath::from_rect(para.bounding_box());
        let wire = matches!(view_mode, ViewMode::Wireframe | ViewMode::SimpleWireframe);
        if !wire {
            backend.push_clip(&frame_path, FillRule::Winding, transform);
        }
        let pieces = self.layout_paragraph_in_doc_for_page(doc, obj_id, para, page_index);
        draw_text_pieces_icc(
            backend, &pieces, transform, outline, opacity, blend_mode, view_mode, px_per_mm, icc,
        );
        if !wire {
            backend.pop_clip();
        }
    }

    /// Renders [`TextOnPath`] fitted along `host_path` (in `host_path`'s coordinate frame).
    #[allow(clippy::too_many_arguments)]
    pub fn render_text_on_path(
        &mut self,
        backend: &mut dyn CanvasBackend,
        top: &TextOnPath,
        host_path: &BezierPath,
        transform: Affine2D,
        outline: &OutlineStyle,
        opacity: f32,
        blend_mode: BlendMode,
        view_mode: ViewMode,
        px_per_mm: f64,
    ) {
        self.render_text_on_path_icc(
            backend,
            top,
            host_path,
            transform,
            outline,
            opacity,
            blend_mode,
            view_mode,
            px_per_mm,
            &IccColorPipeline::default(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_text_on_path_icc(
        &mut self,
        backend: &mut dyn CanvasBackend,
        top: &TextOnPath,
        host_path: &BezierPath,
        transform: Affine2D,
        outline: &OutlineStyle,
        opacity: f32,
        blend_mode: BlendMode,
        view_mode: ViewMode,
        px_per_mm: f64,
        icc: &IccColorPipeline,
    ) {
        let pieces = self.layout_text_on_path_pieces(top, host_path);
        draw_text_pieces_icc(
            backend, &pieces, transform, outline, opacity, blend_mode, view_mode, px_per_mm, icc,
        );
    }

    /// Measures tight bounding box of shaped [`ArtisticText`] in local millimeters.
    #[must_use]
    pub fn measure_artistic_bounds(&mut self, text: &ArtisticText) -> Rect2D {
        let curves = self.artistic_text_to_curves(text);
        curves
            .bounding_box()
            .unwrap_or_else(|| text.estimated_bounds())
    }

    fn resolve_family<'a>(&'a self, requested: &'a str, has_arabic: bool) -> Family<'a> {
        let trimmed = requested.trim();
        if has_arabic {
            // If caller kept default Latin "Noto Sans", route to an installed Arabic-capable face
            if (trimmed.is_empty() || trimmed.eq_ignore_ascii_case("Noto Sans"))
                && self.arabic_fallback_family.is_some()
            {
                if let Some(ar) = self.arabic_fallback_family.as_deref() {
                    return Family::Name(ar);
                }
            }
        }
        if !trimmed.is_empty() && self.has_family(trimmed) {
            return Family::Name(trimmed);
        }
        if has_arabic {
            if let Some(ar) = self.arabic_fallback_family.as_deref() {
                return Family::Name(ar);
            }
        }
        Family::SansSerif
    }

    fn has_family(&self, family: &str) -> bool {
        if self.font_system.db().len() == self.base_face_count {
            return self
                .base_families_lower
                .contains(&family.to_ascii_lowercase());
        }
        self.font_system.db().faces().any(|face| {
            face.families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(family))
        })
    }

    /// Measures the shaped horizontal advance width (in mm) of a single `TextRun` using `FontSystem`.
    #[must_use]
    pub fn measure_run_advance_mm(&mut self, run: &TextRun) -> f64 {
        if run.text.is_empty() {
            return 0.0;
        }
        let font_px = (run.style.font_size_mm() as f32 * LAYOUT_UNITS_PER_MM).max(4.0);
        let metrics = Metrics::new(font_px, font_px * 1.2);
        let has_arabic = contains_arabic(&run.text);
        let dir = if has_arabic {
            TextDirection::Rtl
        } else {
            TextDirection::Ltr
        };
        let fam_owned: Option<String> = {
            let fam = self.resolve_family(run.style.family_for_direction(dir), has_arabic);
            match fam {
                Family::Name(n) => Some(n.to_owned()),
                _ => None,
            }
        };
        let fam = fam_owned.as_deref().map_or(Family::SansSerif, Family::Name);
        let attrs = build_attrs(&run.style, fam, 0);
        let mut buf = Buffer::new(&mut self.font_system, metrics);
        {
            let mut b = buf.borrow_with(&mut self.font_system);
            b.set_wrap(Wrap::None);
            b.set_size(None, None);
            b.set_text(&run.text, &attrs, Shaping::Advanced, None);
            b.shape_until_scroll(false);
        }
        let w_px = buf.layout_runs().map(|r| r.line_w).fold(0.0_f32, f32::max);
        f64::from(w_px) / f64::from(LAYOUT_UNITS_PER_MM)
    }

    fn preprocess_runs_for_kashida_shaped(
        &mut self,
        runs: &[TextRun],
        block_align: TextAlign,
        col_width_mm: Option<f64>,
    ) -> Vec<TextRun> {
        self.preprocess_runs_for_kashida_shaped_inner(runs, block_align, col_width_mm, true)
    }

    fn preprocess_runs_for_kashida_shaped_inner(
        &mut self,
        runs: &[TextRun],
        block_align: TextAlign,
        col_width_mm: Option<f64>,
        allow_default_stretch: bool,
    ) -> Vec<TextRun> {
        let mut measured_widths = Vec::with_capacity(runs.len());
        for run in runs {
            let stretch = sanitize_f32(run.style.kashida_stretch, 0.0).clamp(0.0, 2.0);
            let want_kashida = block_align == TextAlign::ForceJustifyKashida
                || run.style.align == TextAlign::ForceJustifyKashida
                || stretch > 0.01;
            if want_kashida
                && contains_arabic(&run.text)
                && col_width_mm.is_some()
                && stretch <= 0.01
            {
                let text_w = self.measure_run_advance_mm(run);
                let sample_tat =
                    self.measure_run_advance_mm(&TextRun::new("\u{0640}", run.style.clone()));
                measured_widths.push(Some((text_w, sample_tat.max(0.25))));
            } else {
                measured_widths.push(None);
            }
        }
        runs.iter()
            .zip(measured_widths)
            .map(|(run, shaped)| {
                let stretch = sanitize_f32(run.style.kashida_stretch, 0.0).clamp(0.0, 2.0);
                let want_kashida = block_align == TextAlign::ForceJustifyKashida
                    || run.style.align == TextAlign::ForceJustifyKashida
                    || stretch > 0.01;
                if !want_kashida || !contains_arabic(&run.text) {
                    return run.clone();
                }
                let tatweels = if stretch > 0.01 {
                    (stretch * 2.5).round().clamp(1.0, 5.0) as usize
                } else if let (Some(cw), Some((raw_w, tat_w))) = (col_width_mm, shaped) {
                    let words = run.text.split_whitespace().count().max(1);
                    if cw > raw_w {
                        let gap_per_word = (cw - raw_w) / (words as f64);
                        (gap_per_word / tat_w).round().clamp(1.0, 6.0) as usize
                    } else {
                        1
                    }
                } else if allow_default_stretch {
                    2
                } else {
                    0
                };
                if tatweels == 0 {
                    return run.clone();
                }
                let mut cloned = run.clone();
                cloned.text = apply_kashida_elongation(&run.text, tatweels);
                cloned
            })
            .collect()
    }

    #[must_use]
    pub fn layout_artistic_pieces(&mut self, text: &ArtisticText) -> Vec<ShapedGlyphDraw> {
        if text.runs.is_empty() {
            return Vec::new();
        }
        let direction = text.effective_direction();
        let processed_runs = self.preprocess_runs_for_kashida_shaped(&text.runs, text.align, None);
        let default_style = processed_runs
            .first()
            .map_or_else(TextStyle::default, |r| r.style.clone());
        let font_px = (default_style.font_size_mm() as f32 * LAYOUT_UNITS_PER_MM).max(4.0);
        let line_ratio = sanitize_f64(default_style.line_height_ratio, 1.2).clamp(0.6, 4.0) as f32;
        let metrics = Metrics::new(font_px, font_px * line_ratio);

        let full_text: String = processed_runs.iter().map(|r| r.text.as_str()).collect();
        let has_arabic = contains_arabic(&full_text);
        let max_line_chars = full_text
            .split('\n')
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(1)
            .max(1);
        let generous_width = ((max_line_chars as f32) * font_px * 1.4).max(font_px * 8.0);
        let cosmic_align = map_align(text.align, direction);

        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        {
            let mut b = buffer.borrow_with(&mut self.font_system);
            b.set_wrap(Wrap::None);
            b.set_size(Some(generous_width), None);
        }

        let default_family = self.resolve_family(
            default_style.family_for_direction(if has_arabic {
                TextDirection::Rtl
            } else {
                TextDirection::Ltr
            }),
            has_arabic,
        );
        let default_attrs = build_attrs(&default_style, default_family, 0);
        let spans: Vec<(&str, Attrs<'_>)> = processed_runs
            .iter()
            .enumerate()
            .map(|(idx, run)| {
                let run_ar = has_arabic || contains_arabic(&run.text);
                let target_fam = run.style.family_for_direction(if run_ar {
                    TextDirection::Rtl
                } else {
                    TextDirection::Ltr
                });
                let fam = self.resolve_family(target_fam, run_ar);
                (run.text.as_str(), build_attrs(&run.style, fam, idx))
            })
            .collect();

        buffer.set_rich_text(spans, &default_attrs, Shaping::Advanced, Some(cosmic_align));
        buffer.shape_until_scroll(&mut self.font_system, false);

        // Normalize horizontal origin so artistic text starts cleanly near x=0 regardless of generous_width
        let min_x_px = buffer
            .layout_runs()
            .flat_map(|r| r.glyphs.iter())
            .map(|g| g.x)
            .fold(f32::INFINITY, f32::min);
        let x_shift = if min_x_px.is_finite() { -min_x_px } else { 0.0 };

        self.extract_buffer_glyph_paths_inner(
            &buffer,
            &processed_runs,
            None,
            x_shift,
            0.0,
            None,
            None,
        )
    }

    /// Lays out an [`ArtisticText`] object into positioned PDF font runs ([`ShapedPdfRun`]) plus
    /// vector curve fallbacks ([`ShapedGlyphDraw`]) for variable-axis/decoration/non-SFNT glyphs,
    /// using identical shaping, Kashida, and coordinates as [`Self::layout_artistic_pieces`].
    #[must_use]
    pub fn layout_artistic_pdf_runs(
        &mut self,
        text: &ArtisticText,
    ) -> (Vec<ShapedPdfRun>, Vec<ShapedGlyphDraw>) {
        if text.runs.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let direction = text.effective_direction();
        let processed_runs = self.preprocess_runs_for_kashida_shaped(&text.runs, text.align, None);
        let default_style = processed_runs
            .first()
            .map_or_else(TextStyle::default, |r| r.style.clone());
        let font_px = (default_style.font_size_mm() as f32 * LAYOUT_UNITS_PER_MM).max(4.0);
        let line_ratio = sanitize_f64(default_style.line_height_ratio, 1.2).clamp(0.6, 4.0) as f32;
        let metrics = Metrics::new(font_px, font_px * line_ratio);

        let full_text: String = processed_runs.iter().map(|r| r.text.as_str()).collect();
        let has_arabic = contains_arabic(&full_text);
        let max_line_chars = full_text
            .split('\n')
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(1)
            .max(1);
        let generous_width = ((max_line_chars as f32) * font_px * 1.4).max(font_px * 8.0);
        let cosmic_align = map_align(text.align, direction);

        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        {
            let mut b = buffer.borrow_with(&mut self.font_system);
            b.set_wrap(Wrap::None);
            b.set_size(Some(generous_width), None);
        }

        let default_family = self.resolve_family(
            default_style.family_for_direction(if has_arabic {
                TextDirection::Rtl
            } else {
                TextDirection::Ltr
            }),
            has_arabic,
        );
        let default_attrs = build_attrs(&default_style, default_family, 0);
        let spans: Vec<(&str, Attrs<'_>)> = processed_runs
            .iter()
            .enumerate()
            .map(|(idx, run)| {
                let run_ar = has_arabic || contains_arabic(&run.text);
                let target_fam = run.style.family_for_direction(if run_ar {
                    TextDirection::Rtl
                } else {
                    TextDirection::Ltr
                });
                let fam = self.resolve_family(target_fam, run_ar);
                (run.text.as_str(), build_attrs(&run.style, fam, idx))
            })
            .collect();

        buffer.set_rich_text(spans, &default_attrs, Shaping::Advanced, Some(cosmic_align));
        buffer.shape_until_scroll(&mut self.font_system, false);

        let min_x_px = buffer
            .layout_runs()
            .flat_map(|r| r.glyphs.iter())
            .map(|g| g.x)
            .fold(f32::INFINITY, f32::min);
        let x_shift = if min_x_px.is_finite() { -min_x_px } else { 0.0 };

        let mut pdf_runs = Vec::new();
        let fallback_curves = self.extract_buffer_glyph_paths_inner(
            &buffer,
            &processed_runs,
            Some(&text.runs),
            x_shift,
            0.0,
            None,
            Some(&mut pdf_runs),
        );
        (pdf_runs, fallback_curves)
    }

    /// Lays out a [`ParagraphText`] object inside `doc` into positioned PDF font runs plus
    /// vector curve fallbacks, reusing exact [`Self::layout_paragraph_in_doc`] layout.
    #[must_use]
    pub fn layout_paragraph_pdf_runs(
        &mut self,
        doc: &Document,
        obj_id: ObjectId,
        para: &ParagraphText,
    ) -> (Vec<ShapedPdfRun>, Vec<ShapedGlyphDraw>) {
        self.layout_paragraph_pdf_runs_on_page(doc, obj_id, para, doc.active_page_index())
    }

    /// Lays out a [`ParagraphText`] object on `page_index` into positioned PDF font runs plus
    /// vector curve fallbacks, resolving dynamic fields such as [`coree_core::TextField::AutoPageNumber`].
    #[must_use]
    pub fn layout_paragraph_pdf_runs_on_page(
        &mut self,
        doc: &Document,
        obj_id: ObjectId,
        para: &ParagraphText,
        page_index: usize,
    ) -> (Vec<ShapedPdfRun>, Vec<ShapedGlyphDraw>) {
        let mut pdf_runs = Vec::new();
        let fallback_curves =
            self.layout_paragraph_in_doc_inner(doc, obj_id, para, page_index, Some(&mut pdf_runs));
        (pdf_runs, fallback_curves)
    }

    #[must_use]
    pub fn layout_paragraph_pieces(&mut self, para: &ParagraphText) -> Vec<ShapedGlyphDraw> {
        self.layout_paragraph_core(para, &[])
    }

    /// Lays out a [`ParagraphText`] object inside `doc`, supporting `linked_next_frame` overflow
    /// chains (with cycle guard), `TextWrapMode` exclusion against overlapping page objects,
    /// `DropCapConfig`, `ParagraphListStyle` bullets/numbering, `ParagraphTabStop` alignment, and
    /// OpenType `fvar` variable font axes.
    #[must_use]
    pub fn layout_paragraph_in_doc(
        &mut self,
        doc: &Document,
        obj_id: ObjectId,
        para: &ParagraphText,
    ) -> Vec<ShapedGlyphDraw> {
        self.layout_paragraph_in_doc_inner(doc, obj_id, para, doc.active_page_index(), None)
    }

    /// Lays out a [`ParagraphText`] object inside `doc` for an explicit `page_index`, resolving
    /// dynamic text fields (`AutoPageNumber`, `PageCount`, `DocumentTitle`, `CurrentDate`).
    #[must_use]
    pub fn layout_paragraph_in_doc_for_page(
        &mut self,
        doc: &Document,
        obj_id: ObjectId,
        para: &ParagraphText,
        page_index: usize,
    ) -> Vec<ShapedGlyphDraw> {
        self.layout_paragraph_in_doc_inner(doc, obj_id, para, page_index, None)
    }

    fn layout_paragraph_in_doc_inner(
        &mut self,
        doc: &Document,
        obj_id: ObjectId,
        para: &ParagraphText,
        page_index: usize,
        mut pdf_out: Option<&mut Vec<ShapedPdfRun>>,
    ) -> Vec<ShapedGlyphDraw> {
        let resolved_para = doc.resolve_paragraph_text_fields(page_index, para);
        if obj_id.0 == 0 || doc.object(obj_id).is_none() {
            let obstacles = collect_frame_wrap_obstacles(doc, obj_id, resolved_para.wrap_mode);
            return self.layout_paragraph_core_inner(&resolved_para, &obstacles, pdf_out);
        }

        // Find chain head by walking incoming `linked_next_frame` pointers (cycle-safe & deterministic)
        let mut incoming: HashMap<ObjectId, ObjectId> = HashMap::new();
        for (&oid, obj) in doc.objects() {
            if let NodePayload::ParagraphText(p) = &obj.payload {
                if let Some(next_id) = p.linked_next_frame {
                    if next_id.0 != 0 && next_id != oid {
                        incoming
                            .entry(next_id)
                            .and_modify(|e| {
                                if oid.0 < e.0 {
                                    *e = oid;
                                }
                            })
                            .or_insert(oid);
                    }
                }
            }
        }
        let mut head_id = obj_id;
        let mut back_path = vec![head_id];
        let mut back_seen = HashSet::from([head_id]);
        while let Some(&prev) = incoming.get(&head_id) {
            if !back_seen.insert(prev) {
                if let Some(pos) = back_path.iter().position(|&id| id == prev) {
                    if let Some(&min_cycle_id) = back_path[pos..].iter().min_by_key(|id| id.0) {
                        head_id = min_cycle_id;
                    }
                }
                break;
            }
            back_path.push(prev);
            head_id = prev;
        }

        // Build forward chain starting at `head_id` up to `obj_id`
        let mut chain: Vec<ObjectId> = Vec::new();
        let mut cur = Some(head_id);
        let mut fwd_seen = HashSet::new();
        while let Some(cid) = cur {
            if chain.len() >= 64 || !fwd_seen.insert(cid) {
                break;
            }
            chain.push(cid);
            if cid == obj_id {
                break;
            }
            cur = doc.object(cid).and_then(|o| match &o.payload {
                NodePayload::ParagraphText(p) => p.linked_next_frame,
                _ => None,
            });
        }
        if !chain.contains(&obj_id) {
            let obstacles = collect_frame_wrap_obstacles(doc, obj_id, resolved_para.wrap_mode);
            return self.layout_paragraph_core_inner(&resolved_para, &obstacles, pdf_out);
        }

        let head_para_storage = doc.object(head_id).and_then(|o| match &o.payload {
            NodePayload::ParagraphText(p) => Some(doc.resolve_paragraph_text_fields(page_index, p)),
            _ => None,
        });
        let head_para = head_para_storage.as_ref().unwrap_or(&resolved_para);

        if chain.len() == 1 {
            let obstacles = collect_frame_wrap_obstacles(doc, obj_id, head_para.wrap_mode);
            return self.layout_paragraph_core_inner(head_para, &obstacles, pdf_out);
        }

        // Distribute shaped text sequentially across frames in `chain` up to `obj_id`
        let mut cursor = Some(ParagraphCursor {
            para_idx: 0,
            byte_offset: 0,
        });
        for &fid in &chain {
            let Some(cur) = cursor else {
                if fid == obj_id {
                    return Vec::new();
                }
                continue;
            };
            let frame_para_storage = doc.object(fid).and_then(|o| match &o.payload {
                NodePayload::ParagraphText(p) => {
                    Some(doc.resolve_paragraph_text_fields(page_index, p))
                }
                _ => None,
            });
            let frame_para = frame_para_storage.as_ref().unwrap_or(&resolved_para);
            let obstacles = collect_frame_wrap_obstacles(doc, fid, frame_para.wrap_mode);
            let is_target = fid == obj_id;
            let (pieces, next_cursor) = self.layout_paragraph_frame_slice(
                head_para,
                frame_para,
                &obstacles,
                cur,
                is_target,
                if is_target { pdf_out.take() } else { None },
            );
            if is_target {
                return pieces;
            }
            cursor = next_cursor;
        }
        Vec::new()
    }

    fn layout_paragraph_core(
        &mut self,
        para: &ParagraphText,
        obstacles: &[Rect2D],
    ) -> Vec<ShapedGlyphDraw> {
        self.layout_paragraph_core_inner(para, obstacles, None)
    }

    fn layout_paragraph_core_inner(
        &mut self,
        para: &ParagraphText,
        obstacles: &[Rect2D],
        pdf_out: Option<&mut Vec<ShapedPdfRun>>,
    ) -> Vec<ShapedGlyphDraw> {
        let (pieces, _) = self.layout_paragraph_frame_slice(
            para,
            para,
            obstacles,
            ParagraphCursor {
                para_idx: 0,
                byte_offset: 0,
            },
            true,
            pdf_out,
        );
        pieces
    }

    fn layout_paragraph_frame_slice(
        &mut self,
        content_para: &ParagraphText,
        frame_para: &ParagraphText,
        obstacles: &[Rect2D],
        start_cursor: ParagraphCursor,
        emit_pieces: bool,
        mut pdf_out: Option<&mut Vec<ShapedPdfRun>>,
    ) -> (Vec<ShapedGlyphDraw>, Option<ParagraphCursor>) {
        if content_para.runs.is_empty() {
            return (Vec::new(), None);
        }
        let is_continuation = start_cursor.para_idx > 0 || start_cursor.byte_offset > 0;
        let direction = content_para.effective_direction();
        let is_rtl = direction == TextDirection::Rtl;
        let cols = frame_para.columns.clamp(1, 16) as usize;
        let frame_w_mm = sanitize_f64(frame_para.frame_size.x, 80.0).max(2.0);
        let frame_h_mm = sanitize_f64(frame_para.frame_size.y, 40.0).max(2.0);
        let gutter_mm = sanitize_f64(frame_para.column_gutter_mm, 5.0).max(0.0);
        let total_gutter = gutter_mm * (cols.saturating_sub(1) as f64);
        let col_w_mm = ((frame_w_mm - total_gutter) / (cols as f64)).max(2.0);
        let frame_h_px = (frame_h_mm as f32) * LAYOUT_UNITS_PER_MM;

        let default_raw_style = content_para
            .runs
            .first()
            .map_or_else(TextStyle::default, |r| r.style.clone());
        let list_style = content_para
            .list_style
            .as_ref()
            .filter(|s| !matches!(s, ParagraphListStyle::None));

        // Always strip the DropCap character identically across all frames in the story so
        // `(para_idx, byte_offset)` cursors stay aligned, while only emitting/wrapping around
        // the DropCap on the very first frame (`!is_continuation`).
        let (runs_for_shape, raw_dc_draw, raw_dc_box_mm) =
            self.extract_drop_cap(&content_para.runs, content_para.drop_cap, direction);
        let (drop_cap_draw, drop_cap_box_mm) = if is_continuation {
            (None, None)
        } else {
            (raw_dc_draw, raw_dc_box_mm)
        };

        let raw_paras = split_runs_into_paragraphs(&runs_for_shape);
        if start_cursor.para_idx >= raw_paras.len() {
            return (Vec::new(), None);
        }

        let non_empty_para_count = raw_paras.iter().filter(|p| !p.is_empty()).count() as u32;
        let list_indent_mm = self.compute_list_indent_mm(
            list_style,
            non_empty_para_count,
            &default_raw_style,
            col_w_mm,
        );
        let base_body_w_mm = (col_w_mm - list_indent_mm).max(2.0);
        let base_body_w_px = (base_body_w_mm as f32) * LAYOUT_UNITS_PER_MM;

        let has_dynamic_wrap = !obstacles.is_empty() || drop_cap_box_mm.is_some();

        let default_style = runs_for_shape
            .first()
            .map_or_else(|| default_raw_style.clone(), |r| r.style.clone());
        let font_px = (default_style.font_size_mm() as f32 * LAYOUT_UNITS_PER_MM).max(4.0);
        let line_ratio = sanitize_f64(default_style.line_height_ratio, 1.2).clamp(0.6, 4.0) as f32;
        let line_h_px = font_px * line_ratio;
        let metrics = Metrics::new(font_px, line_h_px);
        let cosmic_align = map_align(content_para.align, direction);

        let mut pending_dc_piece = if emit_pieces {
            drop_cap_draw.map(|mut dc_piece| {
                let col0_visual = if is_rtl { cols - 1 } else { 0 };
                let col0_x_mm = (col0_visual as f64) * (col_w_mm + gutter_mm);
                if let Some(bb) = dc_piece.path.bounding_box() {
                    let dx = if is_rtl {
                        (col0_x_mm + col_w_mm - list_indent_mm) - bb.max_x
                    } else {
                        (col0_x_mm + list_indent_mm) - bb.min_x
                    };
                    if dx.abs() > 1e-6 {
                        dc_piece.path.transform(&Affine2D::translate(dx, 0.0));
                    }
                }
                dc_piece
            })
        } else {
            None
        };

        let mut pieces = Vec::new();
        if runs_for_shape.is_empty() {
            if let Some(dc) = pending_dc_piece.take() {
                pieces.push(dc);
            }
            return (pieces, None);
        }

        let want_kashida = content_para.align == TextAlign::ForceJustifyKashida
            || runs_for_shape
                .iter()
                .any(|r| r.style.align == TextAlign::ForceJustifyKashida);

        let mut current_col = 0usize;
        let mut col_y_px = 0.0_f32;
        let mut non_empty_para_idx = raw_paras[..start_cursor.para_idx]
            .iter()
            .filter(|p| !p.is_empty())
            .count() as u32;

        for (p_idx, orig_para_runs) in raw_paras.iter().enumerate().skip(start_cursor.para_idx) {
            let slice_base_byte = if p_idx == start_cursor.para_idx {
                start_cursor.byte_offset
            } else {
                0
            };

            if orig_para_runs.is_empty() {
                if col_y_px + line_h_px > frame_h_px && col_y_px > 0.0 {
                    current_col += 1;
                    col_y_px = 0.0;
                }
                if current_col >= cols {
                    return (
                        pieces,
                        Some(ParagraphCursor {
                            para_idx: p_idx,
                            byte_offset: 0,
                        }),
                    );
                }
                if let Some(dc) = pending_dc_piece.take() {
                    pieces.push(dc);
                }
                col_y_px += line_h_px;
                continue;
            }

            let marker_text = list_style.and_then(|st| {
                let s = match st {
                    ParagraphListStyle::None => String::new(),
                    ParagraphListStyle::Bullet(sym) => sym.trim_end().to_owned(),
                    ParagraphListStyle::Numbered { start, suffix } => {
                        format!("{}{suffix}", start.saturating_add(non_empty_para_idx))
                    }
                };
                (!s.is_empty()).then_some(s)
            });
            let marker_style = orig_para_runs
                .first()
                .map_or_else(|| default_style.clone(), |r| r.style.clone());
            non_empty_para_idx += 1;

            let (sliced_orig_runs, effective_base_byte) =
                slice_runs_from_byte_tracked(orig_para_runs, slice_base_byte);
            if sliced_orig_runs.is_empty() {
                continue;
            }

            let (proc_para_runs, proc_to_orig_map) = preprocess_runs_for_kashida_mapped(
                &sliced_orig_runs,
                content_para.align,
                effective_base_byte,
            );

            let mut first_visual_line = slice_base_byte == 0;

            if !has_dynamic_wrap {
                // Fast path: static width across the entire paragraph
                let buffer = self.shape_runs_to_buffer(
                    &proc_para_runs,
                    base_body_w_px,
                    cosmic_align,
                    &default_style,
                    metrics,
                );
                let line_snapshots = collect_buffer_line_snapshots(&buffer);
                let mut max_consumed_end = 0usize;
                for (line_idx, (baseline_offset_px, lh_px, raw_line_w, line_text, raw_glyphs)) in
                    line_snapshots.iter().enumerate()
                {
                    if col_y_px + *lh_px > frame_h_px && col_y_px > 0.0 {
                        current_col += 1;
                        col_y_px = 0.0;
                    }
                    if current_col >= cols {
                        let next_proc_byte = line_snapshots[line_idx..]
                            .iter()
                            .find_map(|(_, _, _, _, gs)| gs.iter().map(|g| g.start).min())
                            .map(|b| b.max(max_consumed_end));
                        if let Some(proc_b) = next_proc_byte {
                            let orig_b = map_proc_byte(&proc_to_orig_map, proc_b);
                            return (
                                pieces,
                                Some(ParagraphCursor {
                                    para_idx: p_idx,
                                    byte_offset: orig_b,
                                }),
                            );
                        }
                        if p_idx + 1 < raw_paras.len() {
                            return (
                                pieces,
                                Some(ParagraphCursor {
                                    para_idx: p_idx + 1,
                                    byte_offset: 0,
                                }),
                            );
                        }
                        return (pieces, None);
                    }
                    if let Some(end_b) = raw_glyphs.iter().map(|g| g.end).max() {
                        max_consumed_end = max_consumed_end.max(end_b);
                    }

                    if emit_pieces {
                        if let Some(dc) = pending_dc_piece.take() {
                            pieces.push(dc);
                        }
                        let visual_col = if is_rtl {
                            cols - 1 - current_col
                        } else {
                            current_col
                        };
                        let col_x_mm = (visual_col as f64) * (col_w_mm + gutter_mm);
                        let c_left = col_x_mm;
                        let c_right = col_x_mm + col_w_mm;
                        let (body_l_mm, body_r_mm, marker_l_mm, marker_r_mm) = if is_rtl {
                            let br = (c_right - list_indent_mm).max(c_left);
                            (c_left, br, br, c_right)
                        } else {
                            let bl = (c_left + list_indent_mm).min(c_right);
                            (bl, c_right, c_left, bl)
                        };
                        let baseline_y_px = col_y_px + *baseline_offset_px;

                        if first_visual_line && line_idx == 0 {
                            if let Some(ref m_txt) = marker_text {
                                self.emit_list_marker(
                                    m_txt,
                                    &marker_style,
                                    marker_l_mm,
                                    marker_r_mm,
                                    baseline_y_px,
                                    is_rtl,
                                    metrics,
                                    &mut pieces,
                                );
                            }
                        }

                        let line_is_rtl = is_rtl
                            || (content_para.direction == TextDirection::Auto
                                && TextDirection::detect(line_text) == TextDirection::Rtl);
                        self.emit_paragraph_line(
                            raw_glyphs,
                            line_text,
                            *raw_line_w,
                            &proc_para_runs,
                            &default_style,
                            &content_para.tab_stops_mm,
                            want_kashida,
                            line_is_rtl,
                            body_l_mm,
                            body_r_mm,
                            None,
                            baseline_y_px,
                            &mut pieces,
                            pdf_out.as_deref_mut(),
                        );
                    }
                    col_y_px += *lh_px;
                }
            } else {
                // Dynamic per-line wrapping around obstacles and/or drop cap
                let mut remaining_runs = proc_para_runs;
                let mut cur_proc_byte = 0usize;
                let mut guard_iters = 0usize;

                while !remaining_runs.is_empty() && guard_iters < 4096 {
                    guard_iters += 1;
                    let active_wrap_mode = frame_para.wrap_mode;

                    if active_wrap_mode == TextWrapMode::AboveBelow && !obstacles.is_empty() {
                        let mut y_try = col_y_px;
                        for _ in 0..obstacles.len() {
                            let y0_mm = f64::from(y_try / LAYOUT_UNITS_PER_MM);
                            let y1_mm = f64::from((y_try + line_h_px) / LAYOUT_UNITS_PER_MM);
                            let mut bumped = false;
                            for obs in obstacles {
                                if obs.min_y < y1_mm && obs.max_y > y0_mm {
                                    let next_y = ((obs.max_y + 0.5) as f32) * LAYOUT_UNITS_PER_MM;
                                    if next_y > y_try {
                                        y_try = next_y;
                                        bumped = true;
                                    }
                                }
                            }
                            if !bumped {
                                break;
                            }
                        }
                        col_y_px = y_try;
                    }

                    if col_y_px + line_h_px > frame_h_px && col_y_px > 0.0 {
                        current_col += 1;
                        col_y_px = 0.0;
                    }
                    if current_col >= cols {
                        let orig_b = map_proc_byte(&proc_to_orig_map, cur_proc_byte);
                        return (
                            pieces,
                            Some(ParagraphCursor {
                                para_idx: p_idx,
                                byte_offset: orig_b,
                            }),
                        );
                    }

                    let visual_col = if is_rtl {
                        cols - 1 - current_col
                    } else {
                        current_col
                    };
                    let col_x_mm = (visual_col as f64) * (col_w_mm + gutter_mm);
                    let c_left = col_x_mm;
                    let c_right = col_x_mm + col_w_mm;
                    let col_left_edge_mm = c_left;
                    let col_right_edge_mm = c_right;

                    let mut obs_l = c_left;
                    let mut obs_r = c_right;
                    let mut x_clip_mm: Option<(f64, f64)> = None;
                    if !matches!(
                        active_wrap_mode,
                        TextWrapMode::None | TextWrapMode::AboveBelow
                    ) && !obstacles.is_empty()
                    {
                        let line_y0_mm = f64::from(col_y_px / LAYOUT_UNITS_PER_MM);
                        let line_y1_mm = f64::from((col_y_px + line_h_px) / LAYOUT_UNITS_PER_MM);
                        for obs in obstacles {
                            if obs.min_y < line_y1_mm
                                && obs.max_y > line_y0_mm
                                && obs.min_x < obs_r
                                && obs.max_x > obs_l
                            {
                                match active_wrap_mode {
                                    TextWrapMode::ContourLeft => {
                                        obs_r = obs_r.min((obs.min_x - 0.5).clamp(c_left, c_right));
                                    }
                                    TextWrapMode::ContourRight => {
                                        obs_l = obs_l.max((obs.max_x + 0.5).clamp(c_left, c_right));
                                    }
                                    TextWrapMode::SquareBoth | TextWrapMode::Straddle => {
                                        let left_space = (obs.min_x - obs_l).max(0.0);
                                        let right_space = (obs_r - obs.max_x).max(0.0);
                                        if right_space > left_space {
                                            obs_l =
                                                obs_l.max((obs.max_x + 0.5).clamp(c_left, c_right));
                                        } else {
                                            obs_r =
                                                obs_r.min((obs.min_x - 0.5).clamp(c_left, c_right));
                                        }
                                    }
                                    TextWrapMode::None | TextWrapMode::AboveBelow => {}
                                }
                            }
                        }
                        if obs_l > c_left + 1e-6 || obs_r < c_right - 1e-6 {
                            x_clip_mm = Some((obs_l, obs_r));
                        }
                    }

                    if obs_r - obs_l < 4.0 {
                        col_y_px += line_h_px;
                        continue;
                    }

                    let eff_indent = if (obs_r - obs_l) > list_indent_mm + 4.0 {
                        list_indent_mm
                    } else {
                        0.0
                    };
                    let (mut body_l_mm, mut body_r_mm, marker_l_mm, marker_r_mm) = if is_rtl {
                        let br = (obs_r - eff_indent).max(obs_l);
                        (obs_l, br, br, obs_r)
                    } else {
                        let bl = (obs_l + eff_indent).min(obs_r);
                        (bl, obs_r, obs_l, bl)
                    };

                    if current_col == 0 {
                        if let Some((dc_w_mm, dc_h_mm)) = drop_cap_box_mm {
                            let cur_y_mm = f64::from(col_y_px / LAYOUT_UNITS_PER_MM);
                            if cur_y_mm < dc_h_mm - 0.1 {
                                if is_rtl {
                                    body_r_mm = body_r_mm
                                        .min(col_right_edge_mm - dc_w_mm)
                                        .max(body_l_mm + 2.0)
                                        .min(body_r_mm);
                                } else {
                                    body_l_mm = body_l_mm
                                        .max(col_left_edge_mm + dc_w_mm)
                                        .min(body_r_mm - 2.0)
                                        .max(body_l_mm);
                                }
                            }
                        }
                    }

                    let usable_w_mm = (body_r_mm - body_l_mm).max(2.0);
                    let usable_w_px = (usable_w_mm as f32) * LAYOUT_UNITS_PER_MM;

                    let buffer = self.shape_runs_to_buffer(
                        &remaining_runs,
                        usable_w_px,
                        cosmic_align,
                        &default_style,
                        metrics,
                    );
                    let mut runs_iter = buffer.layout_runs();
                    let Some(first_run) = runs_iter.next() else {
                        break;
                    };
                    let baseline_offset_px = first_run.line_y - first_run.line_top;
                    let lh_px = first_run.line_height;
                    let raw_line_w = first_run.line_w;
                    let line_text = first_run.text.to_owned();
                    let raw_glyphs = snapshot_run_glyphs(&first_run);
                    let next_byte_start = runs_iter
                        .find_map(|r| r.glyphs.iter().map(|g| g.start).min())
                        .filter(|&b| b > 0);

                    if raw_glyphs.is_empty() && next_byte_start.is_none() {
                        break;
                    }

                    if col_y_px + lh_px > frame_h_px && col_y_px > 0.0 {
                        current_col += 1;
                        col_y_px = 0.0;
                        continue;
                    }

                    if emit_pieces {
                        if let Some(dc) = pending_dc_piece.take() {
                            pieces.push(dc);
                        }
                        let baseline_y_px = col_y_px + baseline_offset_px;
                        if first_visual_line && eff_indent > 0.0 {
                            if let Some(ref m_txt) = marker_text {
                                self.emit_list_marker(
                                    m_txt,
                                    &marker_style,
                                    marker_l_mm,
                                    marker_r_mm,
                                    baseline_y_px,
                                    is_rtl,
                                    metrics,
                                    &mut pieces,
                                );
                            }
                        }
                        let line_is_rtl = is_rtl
                            || (content_para.direction == TextDirection::Auto
                                && TextDirection::detect(&line_text) == TextDirection::Rtl);
                        self.emit_paragraph_line(
                            &raw_glyphs,
                            &line_text,
                            raw_line_w,
                            &remaining_runs,
                            &default_style,
                            &content_para.tab_stops_mm,
                            want_kashida,
                            line_is_rtl,
                            body_l_mm,
                            body_r_mm,
                            x_clip_mm,
                            baseline_y_px,
                            &mut pieces,
                            pdf_out.as_deref_mut(),
                        );
                    }
                    col_y_px += lh_px;
                    first_visual_line = false;
                    if let Some(split) = next_byte_start {
                        let (next_rem, advanced) =
                            slice_runs_from_byte_tracked(&remaining_runs, split);
                        cur_proc_byte += advanced;
                        remaining_runs = next_rem;
                    } else {
                        break;
                    }
                }
            }
        }

        (pieces, None)
    }

    fn shape_runs_to_buffer(
        &mut self,
        runs: &[TextRun],
        w_px: f32,
        cosmic_align: Align,
        default_style: &TextStyle,
        metrics: Metrics,
    ) -> Buffer {
        let full_text: String = runs.iter().map(|r| r.text.as_str()).collect();
        let has_arabic = contains_arabic(&full_text);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        {
            let mut b = buffer.borrow_with(&mut self.font_system);
            b.set_wrap(Wrap::WordOrGlyph);
            b.set_size(Some(w_px.max(20.0)), None);
        }
        let default_family = self.resolve_family(
            default_style.family_for_direction(if has_arabic {
                TextDirection::Rtl
            } else {
                TextDirection::Ltr
            }),
            has_arabic,
        );
        let default_attrs = build_attrs(default_style, default_family, 0);
        let spans: Vec<(&str, Attrs<'_>)> = runs
            .iter()
            .enumerate()
            .map(|(idx, run)| {
                let run_ar = has_arabic || contains_arabic(&run.text);
                let target_fam = run.style.family_for_direction(if run_ar {
                    TextDirection::Rtl
                } else {
                    TextDirection::Ltr
                });
                let fam = self.resolve_family(target_fam, run_ar);
                (run.text.as_str(), build_attrs(&run.style, fam, idx))
            })
            .collect();
        buffer.set_rich_text(spans, &default_attrs, Shaping::Advanced, Some(cosmic_align));
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
    }

    fn compute_list_indent_mm(
        &mut self,
        list_style: Option<&ParagraphListStyle>,
        para_count: u32,
        style: &TextStyle,
        col_w_mm: f64,
    ) -> f64 {
        let Some(ls) = list_style else {
            return 0.0;
        };
        let max_marker_w = match ls {
            ParagraphListStyle::None => return 0.0,
            ParagraphListStyle::Bullet(sym) => {
                let s = sym.trim_end();
                if s.is_empty() {
                    0.0
                } else {
                    self.measure_run_advance_mm(&TextRun::new(s, style.clone()))
                }
            }
            ParagraphListStyle::Numbered { start, suffix } => {
                let last_idx = para_count.saturating_sub(1);
                let sample = format!("{}{suffix}", start.saturating_add(last_idx));
                self.measure_run_advance_mm(&TextRun::new(sample, style.clone()))
            }
        };
        4.0_f64
            .max(max_marker_w + 1.0)
            .min((col_w_mm * 0.45).max(2.0))
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_list_marker(
        &mut self,
        marker_text: &str,
        marker_style: &TextStyle,
        marker_l_mm: f64,
        marker_r_mm: f64,
        baseline_y_px: f32,
        is_rtl: bool,
        metrics: Metrics,
        pieces: &mut Vec<ShapedGlyphDraw>,
    ) {
        if marker_text.is_empty() || marker_r_mm <= marker_l_mm {
            return;
        }
        let mut clean_style = marker_style.clone();
        clean_style.letter_spacing_mm = 0.0;
        clean_style.kashida_stretch = 0.0;
        clean_style.align = TextAlign::Left;
        let has_ar = contains_arabic(marker_text);
        let fam_owned: Option<String> = {
            let fam = self.resolve_family(
                clean_style.family_for_direction(if has_ar {
                    TextDirection::Rtl
                } else {
                    TextDirection::Ltr
                }),
                has_ar,
            );
            match fam {
                Family::Name(n) => Some(n.to_owned()),
                _ => None,
            }
        };
        let fam = fam_owned.as_deref().map_or(Family::SansSerif, Family::Name);
        let attrs = build_attrs(&clean_style, fam, 0);
        let mut buf = Buffer::new(&mut self.font_system, metrics);
        {
            let mut b = buf.borrow_with(&mut self.font_system);
            b.set_wrap(Wrap::None);
            b.set_size(None, None);
            b.set_text(marker_text, &attrs, Shaping::Advanced, Some(Align::Left));
            b.shape_until_scroll(false);
        }
        let Some(run) = buf.layout_runs().next() else {
            return;
        };
        let glyphs = snapshot_run_glyphs(&run);
        let mut marker_pieces = Vec::with_capacity(glyphs.len());
        for g in &glyphs {
            if let Some(path) =
                self.glyph_outline_mm(g, g.x, baseline_y_px + g.y, &clean_style, g.w)
            {
                marker_pieces.push(ShapedGlyphDraw {
                    path,
                    fill: clean_style.fill.clone(),
                });
            }
        }
        if marker_pieces.is_empty() {
            return;
        }
        let mut m_min_x = f64::INFINITY;
        let mut m_max_x = f64::NEG_INFINITY;
        for p in &marker_pieces {
            if let Some(bb) = p.path.bounding_box() {
                m_min_x = m_min_x.min(bb.min_x);
                m_max_x = m_max_x.max(bb.max_x);
            }
        }
        if !m_min_x.is_finite() || !m_max_x.is_finite() {
            return;
        }
        let m_w = m_max_x - m_min_x;
        let target_l = if is_rtl {
            (marker_r_mm - 0.3 - m_w).max(marker_l_mm)
        } else {
            (marker_l_mm + 0.3).min((marker_r_mm - m_w).max(marker_l_mm))
        };
        let dx = target_l - m_min_x;
        if dx.abs() > 1e-6 {
            let xf = Affine2D::translate(dx, 0.0);
            for p in &mut marker_pieces {
                p.path.transform(&xf);
            }
        }
        pieces.extend(marker_pieces);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_paragraph_line(
        &mut self,
        raw_glyphs: &[GlyphSnapshot],
        line_text: &str,
        raw_line_w: f32,
        line_runs: &[TextRun],
        default_style: &TextStyle,
        tab_stops_mm: &[ParagraphTabStop],
        want_kashida: bool,
        line_is_rtl: bool,
        body_l_mm: f64,
        body_r_mm: f64,
        x_clip_mm: Option<(f64, f64)>,
        baseline_y_px: f32,
        pieces: &mut Vec<ShapedGlyphDraw>,
        mut pdf_out: Option<&mut Vec<ShapedPdfRun>>,
    ) {
        let mut glyphs = self.apply_tab_stops_and_fvar(
            raw_glyphs,
            line_text,
            line_runs,
            default_style,
            tab_stops_mm,
        );
        let usable_w_mm = (body_r_mm - body_l_mm).max(2.0);
        let target_px = (usable_w_mm as f32) * LAYOUT_UNITS_PER_MM;
        if want_kashida && contains_arabic(line_text) {
            self.insert_post_shaping_kashida(
                &mut glyphs,
                line_text,
                raw_line_w,
                target_px,
                line_is_rtl,
            );
        }

        let origin_x_px = (body_l_mm as f32) * LAYOUT_UNITS_PER_MM;
        let inv_scale = 1.0 / f64::from(LAYOUT_UNITS_PER_MM);
        struct LineGlyphItem {
            draw: Option<ShapedGlyphDraw>,
            pdf_glyph: Option<(PdfFontBlobEntry, f64, Color, bool, ShapedPdfGlyph)>,
        }
        let mut line_items: Vec<LineGlyphItem> = Vec::with_capacity(glyphs.len());
        let mut seen_clusters: HashSet<(usize, usize)> = HashSet::new();
        let mut extra_spacing_px = 0.0_f32;
        let mut seen_base = false;
        for g in &glyphs {
            let run_style = line_runs
                .get(g.metadata)
                .map(|r| &r.style)
                .unwrap_or(default_style);
            let is_base = g.w > 1e-3;
            if is_base && seen_base {
                let letter_extra = sanitize_f64(run_style.letter_spacing_mm, 0.0) as f32;
                if letter_extra.abs() > 1e-6 {
                    extra_spacing_px += letter_extra * LAYOUT_UNITS_PER_MM;
                }
            }
            if is_base {
                seen_base = true;
            }
            let gx_px = origin_x_px + g.x + extra_spacing_px;
            let gy_px = baseline_y_px + g.y;
            let mut pdf_entry = None;
            if pdf_out.is_some() && run_style.variation_axes.is_empty() {
                if let Some(blob) = self.lookup_pdf_font_blob(g.font_id, g.font_weight) {
                    let unicode = if g.start < g.end && seen_clusters.insert((g.start, g.end)) {
                        line_text.get(g.start..g.end).unwrap_or("").to_owned()
                    } else if g.start == 0 && g.end == 0 && g.w > 0.0 {
                        "\u{0640}".to_owned()
                    } else {
                        String::new()
                    };
                    let font_size_pt = f64::from(g.font_size) * inv_scale * (72.0 / 25.4);
                    let glyph_rtl =
                        (!unicode.is_empty() && contains_arabic(&unicode)) || line_is_rtl;
                    pdf_entry = Some((
                        blob,
                        font_size_pt,
                        run_style.fill.clone(),
                        glyph_rtl,
                        ShapedPdfGlyph {
                            glyph_id: g.glyph_id,
                            x_mm: f64::from(gx_px) * inv_scale,
                            y_mm: f64::from(gy_px) * inv_scale,
                            advance_mm: f64::from(g.w) * inv_scale,
                            unicode,
                        },
                    ));
                }
            }
            let draw = self
                .glyph_outline_mm(g, gx_px, gy_px, run_style, g.w)
                .map(|path| ShapedGlyphDraw {
                    path,
                    fill: run_style.fill.clone(),
                });
            if draw.is_some() || pdf_entry.is_some() {
                line_items.push(LineGlyphItem {
                    draw,
                    pdf_glyph: pdf_entry,
                });
            }
        }

        if line_items.is_empty() {
            return;
        }

        let mut l_min_x = f64::INFINITY;
        let mut l_max_x = f64::NEG_INFINITY;
        for item in &line_items {
            if let Some(bb) = item.draw.as_ref().and_then(|d| d.path.bounding_box()) {
                l_min_x = l_min_x.min(bb.min_x);
                l_max_x = l_max_x.max(bb.max_x);
            }
        }
        let mut shift = 0.0_f64;
        if l_min_x.is_finite() && l_max_x.is_finite() {
            let span_w = l_max_x - l_min_x;
            let avail_span = (body_r_mm - body_l_mm).max(0.0);
            if span_w <= avail_span + 0.05 {
                shift = if l_max_x > body_r_mm {
                    (body_r_mm - l_max_x).max(body_l_mm - l_min_x)
                } else if l_min_x < body_l_mm {
                    (body_l_mm - l_min_x).min(body_r_mm - l_max_x)
                } else {
                    0.0
                };
                if shift.abs() > 1e-6 {
                    let xf = Affine2D::translate(shift, 0.0);
                    for item in &mut line_items {
                        if let Some(ref mut d) = item.draw {
                            d.path.transform(&xf);
                        }
                    }
                }
            }
        }

        if let Some((clip_l_mm, clip_r_mm)) = x_clip_mm {
            line_items.retain(|item| {
                item.draw
                    .as_ref()
                    .and_then(|d| d.path.bounding_box())
                    .is_none_or(|bb| bb.min_x >= clip_l_mm - 0.02 && bb.max_x <= clip_r_mm + 0.02)
            });
        }

        if let Some(ref mut runs_out) = pdf_out {
            for item in line_items {
                if let Some((blob, pt, fill, is_rtl, mut pg)) = item.pdf_glyph {
                    pg.x_mm += shift;
                    push_pdf_glyph_to_runs(runs_out, &blob, pt, &fill, is_rtl, None, pg);
                } else if let Some(d) = item.draw {
                    pieces.push(d);
                }
            }
        } else {
            pieces.extend(line_items.into_iter().filter_map(|i| i.draw));
        }
    }

    #[must_use]
    pub fn layout_text_on_path_pieces(
        &mut self,
        top: &TextOnPath,
        host_path: &BezierPath,
    ) -> Vec<ShapedGlyphDraw> {
        let total_path_len = host_path.path_length_mm();
        if total_path_len <= 1e-6 || top.text.runs.is_empty() {
            return Vec::new();
        }
        let direction = top.text.effective_direction();
        let processed_runs =
            self.preprocess_runs_for_kashida_shaped(&top.text.runs, top.text.align, None);
        let default_style = processed_runs
            .first()
            .map_or_else(TextStyle::default, |r| r.style.clone());
        let font_px = (default_style.font_size_mm() as f32 * LAYOUT_UNITS_PER_MM).max(4.0);
        let line_ratio = sanitize_f64(default_style.line_height_ratio, 1.2).clamp(0.6, 4.0) as f32;
        let metrics = Metrics::new(font_px, font_px * line_ratio);

        let full_text: String = processed_runs.iter().map(|r| r.text.as_str()).collect();
        let has_arabic = contains_arabic(&full_text);

        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        {
            let mut b = buffer.borrow_with(&mut self.font_system);
            b.set_wrap(Wrap::None);
            b.set_size(None, None);
        }

        let default_family = self.resolve_family(
            default_style.family_for_direction(if has_arabic {
                TextDirection::Rtl
            } else {
                TextDirection::Ltr
            }),
            has_arabic,
        );
        let default_attrs = build_attrs(&default_style, default_family, 0);
        let spans: Vec<(&str, Attrs<'_>)> = processed_runs
            .iter()
            .enumerate()
            .map(|(idx, run)| {
                let run_ar = has_arabic || contains_arabic(&run.text);
                let target_fam = run.style.family_for_direction(if run_ar {
                    TextDirection::Rtl
                } else {
                    TextDirection::Ltr
                });
                let fam = self.resolve_family(target_fam, run_ar);
                (run.text.as_str(), build_attrs(&run.style, fam, idx))
            })
            .collect();

        buffer.set_rich_text(
            spans,
            &default_attrs,
            Shaping::Advanced,
            Some(map_align(top.text.align, direction)),
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        // Preserve every line of `TextOnPath` (multi-line support; no `.take(1)` truncation!)
        let line_runs: Vec<(f32, Vec<GlyphSnapshot>)> = buffer
            .layout_runs()
            .map(|run| {
                let glyphs: Vec<GlyphSnapshot> = run
                    .glyphs
                    .iter()
                    .map(|g| GlyphSnapshot {
                        font_id: g.font_id,
                        glyph_id: g.glyph_id,
                        font_size: g.font_size,
                        font_weight: g.font_weight,
                        x: g.x + g.font_size * g.x_offset,
                        y: g.y - g.font_size * g.y_offset,
                        w: g.w,
                        metadata: g.metadata,
                        flags: g.cache_key_flags | CacheKeyFlags::DISABLE_HINTING,
                        start: g.start,
                        end: g.end,
                    })
                    .collect();
                (run.line_height, glyphs)
            })
            .collect();

        if line_runs.is_empty() {
            return Vec::new();
        }

        let offset_mm = sanitize_f64(top.offset_mm, 0.0);
        let base_dist_mm = sanitize_f64(top.distance_mm, 0.0);
        let is_closed_host = host_path.subpaths.first().is_some_and(|sp| sp.closed);
        let sx = if top.flip_horizontal { -1.0 } else { 1.0 };
        let sy = if top.flip_vertical { -1.0 } else { 1.0 };
        let mut pieces = Vec::new();
        let mut line_y_offset_mm = 0.0_f64;

        for (line_lh_px, snapshots) in line_runs {
            let line_h_mm = f64::from(line_lh_px / LAYOUT_UNITS_PER_MM).max(0.5);
            if snapshots.is_empty() {
                line_y_offset_mm += line_h_mm;
                continue;
            }
            let dist_mm = base_dist_mm - line_y_offset_mm;
            let min_x = snapshots.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
            let max_x = snapshots
                .iter()
                .map(|g| g.x + g.w)
                .fold(f32::NEG_INFINITY, f32::max);
            let text_width_mm = f64::from((max_x - min_x).max(0.0) / LAYOUT_UNITS_PER_MM);

            // Helper mapping an arc-length distance onto `(anchor_mm, tangent_angle_rad)` without
            // piling glyphs onto endpoints (wraps closed loops; linearly extrapolates tiny open overhangs).
            let sample_frame =
                |along_mm: f64, half_w_mm: f64| -> Option<(coree_core::Point2D, f64)> {
                    if is_closed_host {
                        let wrapped = along_mm.rem_euclid(total_path_len);
                        let (pt, tangent) = host_path.point_and_tangent_at(wrapped)?;
                        let up_normal = Vec2D::new(tangent.y, -tangent.x);
                        return Some((pt + up_normal * dist_mm, tangent.angle_rad()));
                    }
                    let overhang_tol = half_w_mm.max(1.0);
                    if along_mm < -overhang_tol || along_mm > total_path_len + overhang_tol {
                        return None;
                    }
                    let clamped = along_mm.clamp(0.0, total_path_len);
                    let (pt, tangent) = host_path.point_and_tangent_at(clamped)?;
                    let extra = along_mm - clamped;
                    let extrap_pt = pt + tangent * extra;
                    let up_normal = Vec2D::new(tangent.y, -tangent.x);
                    Some((extrap_pt + up_normal * dist_mm, tangent.angle_rad()))
                };

            let mut active_base: Option<(f32, coree_core::Point2D, f64)> = None;
            for (idx, g) in snapshots.iter().enumerate() {
                let run_style = processed_runs
                    .get(g.metadata)
                    .map(|r| &r.style)
                    .unwrap_or(&default_style);
                let is_combining_mark = g.w <= 1e-3;

                let (glyph_origin_x_px, anchor, angle) = if is_combining_mark {
                    let base_info = active_base.or_else(|| {
                        snapshots[idx + 1..]
                            .iter()
                            .find(|next| next.w > 1e-3)
                            .and_then(|next| {
                                let b_half = next.w * 0.5;
                                let b_center_x = next.x + b_half;
                                let rel_mm = f64::from((b_center_x - min_x) / LAYOUT_UNITS_PER_MM);
                                let along = if top.flip_horizontal {
                                    offset_mm + (text_width_mm - rel_mm)
                                } else {
                                    offset_mm + rel_mm
                                };
                                let (a, ang) =
                                    sample_frame(along, f64::from(b_half / LAYOUT_UNITS_PER_MM))?;
                                Some((b_center_x, a, ang))
                            })
                    });
                    if let Some((base_center_x, base_anchor, base_angle)) = base_info {
                        (g.x - base_center_x, base_anchor, base_angle)
                    } else {
                        let rel_mm = f64::from((g.x - min_x) / LAYOUT_UNITS_PER_MM);
                        let along = if top.flip_horizontal {
                            offset_mm + (text_width_mm - rel_mm)
                        } else {
                            offset_mm + rel_mm
                        };
                        let Some((a, ang)) = sample_frame(along, 1.0) else {
                            continue;
                        };
                        (0.0, a, ang)
                    }
                } else {
                    let half_w_px = g.w * 0.5;
                    let base_center_x = g.x + half_w_px;
                    let rel_center_mm = f64::from((base_center_x - min_x) / LAYOUT_UNITS_PER_MM);
                    let along_mm = if top.flip_horizontal {
                        offset_mm + (text_width_mm - rel_center_mm)
                    } else {
                        offset_mm + rel_center_mm
                    };
                    let Some((a, ang)) =
                        sample_frame(along_mm, f64::from(half_w_px / LAYOUT_UNITS_PER_MM))
                    else {
                        active_base = None;
                        continue;
                    };
                    active_base = Some((base_center_x, a, ang));
                    (-half_w_px, a, ang)
                };

                let Some(mut glyph_path) =
                    self.glyph_outline_mm(g, glyph_origin_x_px, g.y, run_style, g.w)
                else {
                    continue;
                };

                let xf = Affine2D::translate(anchor.x, anchor.y)
                    .compose(&Affine2D::rotate(angle))
                    .compose(&Affine2D::scale(sx, sy));
                glyph_path.transform(&xf);

                pieces.push(ShapedGlyphDraw {
                    path: glyph_path,
                    fill: run_style.fill.clone(),
                });
            }
            line_y_offset_mm += line_h_mm;
        }

        pieces
    }

    fn lookup_pdf_font_blob(
        &mut self,
        font_id: cosmic_text::fontdb::ID,
        font_weight: Weight,
    ) -> Option<PdfFontBlobEntry> {
        let key = (font_id, font_weight.0);
        if let Some(cached) = self.font_blob_cache.get(&key) {
            return cached.clone();
        }
        let entry = self.font_system.get_font(font_id, font_weight).map(|font| {
            let data: std::sync::Arc<[u8]> = std::sync::Arc::from(font.data());
            let upem = font.as_swash().metrics(&[]).units_per_em.max(1);
            let face_info = self.font_system.db().face(font_id);
            let face_index = face_info.map_or(0, |f| f.index);
            let ps_name = face_info
                .map(|f| f.post_script_name.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "CoreeSubsetFont".to_owned());
            (data, face_index, ps_name, upem)
        });
        self.font_blob_cache.insert(key, entry.clone());
        entry
    }

    #[allow(dead_code)]
    fn extract_buffer_glyph_paths(
        &mut self,
        buffer: &Buffer,
        runs: &[TextRun],
        x_shift_px: f32,
        y_shift_px: f32,
        max_height_px: Option<f32>,
    ) -> Vec<ShapedGlyphDraw> {
        self.extract_buffer_glyph_paths_inner(
            buffer,
            runs,
            None,
            x_shift_px,
            y_shift_px,
            max_height_px,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn extract_buffer_glyph_paths_inner(
        &mut self,
        buffer: &Buffer,
        runs: &[TextRun],
        orig_runs: Option<&[TextRun]>,
        x_shift_px: f32,
        y_shift_px: f32,
        max_height_px: Option<f32>,
        mut pdf_out: Option<&mut Vec<ShapedPdfRun>>,
    ) -> Vec<ShapedGlyphDraw> {
        let default_style = runs
            .first()
            .map_or_else(TextStyle::default, |r| r.style.clone());
        let snapshots: Vec<(f32, String, Vec<GlyphSnapshot>)> = buffer
            .layout_runs()
            .filter(|run| max_height_px.is_none_or(|mh| run.line_top < mh))
            .map(|run| {
                let glyphs = run
                    .glyphs
                    .iter()
                    .map(|g| GlyphSnapshot {
                        font_id: g.font_id,
                        glyph_id: g.glyph_id,
                        font_size: g.font_size,
                        font_weight: g.font_weight,
                        x: g.x + g.font_size * g.x_offset,
                        y: g.y - g.font_size * g.y_offset,
                        w: g.w,
                        metadata: g.metadata,
                        flags: g.cache_key_flags | CacheKeyFlags::DISABLE_HINTING,
                        start: g.start,
                        end: g.end,
                    })
                    .collect();
                (run.line_y, run.text.to_owned(), glyphs)
            })
            .collect();

        let inv_scale = 1.0 / f64::from(LAYOUT_UNITS_PER_MM);
        let mut pieces = Vec::new();
        for (line_y, line_text, raw_glyphs) in snapshots {
            let glyphs = self.apply_tab_stops_and_fvar(&raw_glyphs, "", runs, &default_style, &[]);
            let mut extra_spacing_px = 0.0_f32;
            let mut seen_base = false;
            let mut seen_clusters: HashSet<(usize, usize)> = HashSet::new();
            for g in &glyphs {
                let run_style = runs
                    .get(g.metadata)
                    .map(|r| &r.style)
                    .unwrap_or(&default_style);
                let is_base = g.w > 1e-3;
                if is_base && seen_base {
                    let ls = sanitize_f64(run_style.letter_spacing_mm, 0.0) as f32;
                    if ls.abs() > 1e-6 {
                        extra_spacing_px += ls * LAYOUT_UNITS_PER_MM;
                    }
                }
                if is_base {
                    seen_base = true;
                }
                let gx = g.x + x_shift_px + extra_spacing_px;
                let gy = line_y + g.y + y_shift_px;
                if let Some(ref mut runs_out) = pdf_out {
                    if run_style.variation_axes.is_empty() {
                        if let Some(blob) = self.lookup_pdf_font_blob(g.font_id, g.font_weight) {
                            let unicode =
                                if g.start < g.end && seen_clusters.insert((g.start, g.end)) {
                                    line_text.get(g.start..g.end).unwrap_or("").to_owned()
                                } else if g.start == 0 && g.end == 0 && g.w > 0.0 {
                                    "\u{0640}".to_owned()
                                } else {
                                    String::new()
                                };
                            let font_size_pt = f64::from(g.font_size) * inv_scale * (72.0 / 25.4);
                            let is_rtl = (!unicode.is_empty() && contains_arabic(&unicode))
                                || run_style.direction == TextDirection::Rtl;
                            let actual_opt = orig_runs.and_then(|orig| {
                                let o = orig.get(g.metadata)?;
                                let p = runs.get(g.metadata)?;
                                if o.text != p.text && !o.text.contains('\n') {
                                    Some(o.text.clone())
                                } else {
                                    None
                                }
                            });
                            push_pdf_glyph_to_runs(
                                runs_out,
                                &blob,
                                font_size_pt,
                                &run_style.fill,
                                is_rtl,
                                actual_opt,
                                ShapedPdfGlyph {
                                    glyph_id: g.glyph_id,
                                    x_mm: f64::from(gx) * inv_scale,
                                    y_mm: f64::from(gy) * inv_scale,
                                    advance_mm: f64::from(g.w) * inv_scale,
                                    unicode,
                                },
                            );
                            if let Some(bar_path) =
                                decoration_bars_mm(f64::from(gx), f64::from(gy), run_style, g.w)
                            {
                                pieces.push(ShapedGlyphDraw {
                                    path: bar_path,
                                    fill: run_style.fill.clone(),
                                });
                            }
                            continue;
                        }
                    }
                }
                if let Some(path) = self.glyph_outline_mm(g, gx, gy, run_style, g.w) {
                    pieces.push(ShapedGlyphDraw {
                        path,
                        fill: run_style.fill.clone(),
                    });
                }
            }
        }
        pieces
    }

    fn glyph_outline_mm(
        &mut self,
        g: &GlyphSnapshot,
        origin_x_px: f32,
        baseline_y_px: f32,
        style: &TextStyle,
        advance_px: f32,
    ) -> Option<BezierPath> {
        let mut owned_var_cmds: Option<Vec<Command>> = None;
        if !style.variation_axes.is_empty() {
            if let Some(font) = self.font_system.get_font(g.font_id, g.font_weight) {
                let swash_font = font.as_swash();
                let axis_pairs: Vec<(swash::Tag, f32)> = style
                    .variation_axes
                    .iter()
                    .filter_map(|(tag_str, val)| {
                        let b = tag_str.as_bytes();
                        (b.len() == 4 && val.is_finite())
                            .then(|| (swash::Tag::from_be_bytes([b[0], b[1], b[2], b[3]]), *val))
                    })
                    .collect();
                if !axis_pairs.is_empty() {
                    let norm_coords: Vec<i16> = swash_font
                        .variations()
                        .normalized_coords(axis_pairs)
                        .collect();
                    let mut scaler = self
                        .var_scale_ctx
                        .builder(swash_font)
                        .size(g.font_size)
                        .hint(false)
                        .normalized_coords(&norm_coords)
                        .build();
                    if let Some(outline) = scaler
                        .scale_outline(g.glyph_id)
                        .or_else(|| scaler.scale_color_outline(g.glyph_id))
                    {
                        owned_var_cmds = Some(outline.path().commands().collect());
                    }
                }
            }
        }

        let cmds: &[Command] = if let Some(ref vcmds) = owned_var_cmds {
            vcmds.as_slice()
        } else {
            let (cache_key, _, _) = CacheKey::new(
                g.font_id,
                g.glyph_id,
                g.font_size,
                (0.0, 0.0),
                g.font_weight,
                g.flags,
            );
            self.swash_cache
                .get_outline_commands(&mut self.font_system, cache_key)?
        };
        if cmds.is_empty() && !style.underline && !style.strikethrough {
            return None;
        }
        let inv_scale = 1.0 / f64::from(LAYOUT_UNITS_PER_MM);
        let ox = f64::from(origin_x_px);
        let by = f64::from(baseline_y_px);
        let map_pt = |px: f32, py: f32| -> kurbo::Point {
            // swash glyph outlines use Y-up origin; invert Y into Y-down canvas space
            kurbo::Point::new(
                (ox + f64::from(px)) * inv_scale,
                (by - f64::from(py)) * inv_scale,
            )
        };

        let mut kb = BezPath::new();
        for cmd in cmds {
            match *cmd {
                Command::MoveTo(p) => kb.move_to(map_pt(p.x, p.y)),
                Command::LineTo(p) => kb.line_to(map_pt(p.x, p.y)),
                Command::QuadTo(p1, p2) => kb.quad_to(map_pt(p1.x, p1.y), map_pt(p2.x, p2.y)),
                Command::CurveTo(p1, p2, p3) => {
                    kb.curve_to(map_pt(p1.x, p1.y), map_pt(p2.x, p2.y), map_pt(p3.x, p3.y));
                }
                Command::Close => kb.close_path(),
            }
        }

        let mut path = BezierPath::from_kurbo(&kb);

        // Optional underline / strikethrough decoration bars
        let adv_mm = f64::from(advance_px) * inv_scale;
        let size_mm = style.font_size_mm();
        if style.underline && adv_mm > 1e-4 {
            let bar_h = (size_mm * 0.07).max(0.12);
            let bar_y = by * inv_scale + size_mm * 0.12;
            let bar =
                BezierPath::from_rect(Rect2D::from_xywh(ox * inv_scale, bar_y, adv_mm, bar_h));
            path.subpaths.extend(bar.subpaths);
        }
        if style.strikethrough && adv_mm > 1e-4 {
            let bar_h = (size_mm * 0.06).max(0.12);
            let bar_y = by * inv_scale - size_mm * 0.30;
            let bar =
                BezierPath::from_rect(Rect2D::from_xywh(ox * inv_scale, bar_y, adv_mm, bar_h));
            path.subpaths.extend(bar.subpaths);
        }

        if path.is_empty() {
            None
        } else {
            Some(path)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_bitmap_fallback(
        &mut self,
        backend: &mut dyn CanvasBackend,
        runs: &[TextRun],
        width_mm: Option<f64>,
        direction: TextDirection,
        align: TextAlign,
        transform: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        icc: &IccColorPipeline,
    ) {
        let Some(first) = runs.first() else {
            return;
        };
        let font_px = (first.style.font_size_mm() as f32 * LAYOUT_UNITS_PER_MM).max(8.0);
        let line_ratio = sanitize_f64(first.style.line_height_ratio, 1.2).clamp(0.6, 4.0) as f32;
        let metrics = Metrics::new(font_px, font_px * line_ratio);
        let full_text: String = runs.iter().map(|r| r.text.as_str()).collect();
        let has_arabic = contains_arabic(&full_text);

        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        {
            let mut b = buffer.borrow_with(&mut self.font_system);
            b.set_size(width_mm.map(|w| w as f32 * LAYOUT_UNITS_PER_MM), None);
        }
        let default_fam = self.resolve_family(
            first.style.family_for_direction(if has_arabic {
                TextDirection::Rtl
            } else {
                TextDirection::Ltr
            }),
            has_arabic,
        );
        let default_attrs = build_attrs(&first.style, default_fam, 0);
        let spans: Vec<(&str, Attrs<'_>)> = runs
            .iter()
            .enumerate()
            .map(|(idx, run)| {
                let run_ar = has_arabic || contains_arabic(&run.text);
                let target_fam = run.style.family_for_direction(if run_ar {
                    TextDirection::Rtl
                } else {
                    TextDirection::Ltr
                });
                let fam = self.resolve_family(target_fam, run_ar);
                (run.text.as_str(), build_attrs(&run.style, fam, idx))
            })
            .collect();
        buffer.set_rich_text(
            spans,
            &default_attrs,
            Shaping::Advanced,
            Some(map_align(align, direction)),
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let inv_scale = 1.0 / f64::from(LAYOUT_UNITS_PER_MM);

        let glyphs: Vec<(CacheKey, i32, i32, usize)> = buffer
            .layout_runs()
            .flat_map(|run| {
                let ly = run.line_y;
                run.glyphs.iter().map(move |g| {
                    let phys = g.physical((0.0, ly), 1.0);
                    (phys.cache_key, phys.x, phys.y, g.metadata)
                })
            })
            .collect();

        for (cache_key, px_x, px_y, run_idx) in glyphs {
            let Some(img) = self.swash_cache.get_image(&mut self.font_system, cache_key) else {
                continue;
            };
            let w = img.placement.width;
            let h = img.placement.height;
            if w == 0 || h == 0 {
                continue;
            }
            let run_fill = runs
                .get(run_idx)
                .map_or(&first.style.fill, |r| &r.style.fill);
            let [fr, fg, fb, fa] = run_fill.to_rgba8_icc(icc);
            let rgba = swash_image_to_rgba(img, [fr, fg, fb, fa]);
            let buf = RgbaBuffer {
                width: w,
                height: h,
                rgba,
            };
            let gx_mm = f64::from(px_x + img.placement.left) * inv_scale;
            let gy_mm = f64::from(px_y - img.placement.top) * inv_scale;
            let xf = transform
                .compose(&Affine2D::translate(gx_mm, gy_mm))
                .compose(&Affine2D::scale_uniform(inv_scale));
            backend.draw_pixmap(&buf, xf, opacity, blend_mode, true);
        }
    }

    fn apply_tab_stops_and_fvar(
        &mut self,
        raw_glyphs: &[GlyphSnapshot],
        line_text: &str,
        runs: &[TextRun],
        default_style: &TextStyle,
        tab_stops: &[ParagraphTabStop],
    ) -> Vec<GlyphSnapshot> {
        let mut glyphs = raw_glyphs.to_vec();
        // 1. Adjust glyph advances & trailing positions for OpenType `fvar` variable font axes
        let has_var = runs.iter().any(|r| !r.style.variation_axes.is_empty())
            || !default_style.variation_axes.is_empty();
        if has_var && !glyphs.is_empty() {
            let mut shift_x = 0.0_f32;
            for g in &mut glyphs {
                g.x += shift_x;
                let style = runs
                    .get(g.metadata)
                    .map(|r| &r.style)
                    .unwrap_or(default_style);
                let fvar = &style.variation_axes;
                if fvar.is_empty() {
                    continue;
                }
                if let Some(font) = self.font_system.get_font(g.font_id, g.font_weight) {
                    let swash_font = font.as_swash();
                    let axis_pairs: Vec<(swash::Tag, f32)> = fvar
                        .iter()
                        .filter_map(|(tag_str, val)| {
                            let b = tag_str.as_bytes();
                            (b.len() == 4 && val.is_finite()).then(|| {
                                (swash::Tag::from_be_bytes([b[0], b[1], b[2], b[3]]), *val)
                            })
                        })
                        .collect();
                    if !axis_pairs.is_empty() {
                        let norm_coords: Vec<i16> = swash_font
                            .variations()
                            .normalized_coords(axis_pairs)
                            .collect();
                        let var_adv = swash_font
                            .glyph_metrics(&norm_coords)
                            .scale(g.font_size)
                            .advance_width(g.glyph_id);
                        if var_adv.is_finite() && var_adv > 0.0 {
                            let delta = var_adv - g.w;
                            g.w = var_adv;
                            shift_x += delta;
                        }
                    }
                }
            }
        }

        // 2. Align tab-separated runs against explicit `ParagraphTabStop` positions (mm)
        if !tab_stops.is_empty() && line_text.contains('\t') && !glyphs.is_empty() {
            let mut stops = tab_stops.to_vec();
            stops.sort_by(|a, b| a.position_mm.total_cmp(&b.position_mm));
            let mut shift_x = 0.0_f32;
            let mut idx = 0usize;
            while idx < glyphs.len() {
                glyphs[idx].x += shift_x;
                let is_tab = line_text
                    .get(glyphs[idx].start..glyphs[idx].end)
                    .is_some_and(|s| s.contains('\t'));
                if is_tab {
                    let cur_mm = f64::from(glyphs[idx].x / LAYOUT_UNITS_PER_MM);
                    if let Some(stop) = stops.iter().find(|s| s.position_mm > cur_mm + 0.05) {
                        let mut seg_w_px = 0.0_f32;
                        for next_g in &glyphs[(idx + 1)..] {
                            let seg_txt = line_text.get(next_g.start..next_g.end).unwrap_or("");
                            if seg_txt.contains('\t') {
                                break;
                            }
                            seg_w_px += next_g.w;
                        }
                        let anchor_px = match stop.align {
                            ParagraphTabAlign::Left => 0.0,
                            ParagraphTabAlign::Center => seg_w_px * 0.5,
                            ParagraphTabAlign::Right => seg_w_px,
                        };
                        let stop_px = (stop.position_mm as f32) * LAYOUT_UNITS_PER_MM;
                        let new_next_x = (stop_px - anchor_px).max(glyphs[idx].x + 2.0);
                        let cur_next_x = glyphs[idx].x + glyphs[idx].w;
                        shift_x += new_next_x - cur_next_x;
                        glyphs[idx].w = 0.0;
                    }
                }
                idx += 1;
            }
        }
        glyphs
    }

    fn insert_post_shaping_kashida(
        &mut self,
        glyphs: &mut Vec<GlyphSnapshot>,
        line_text: &str,
        raw_line_w_px: f32,
        target_w_px: f32,
        is_rtl: bool,
    ) {
        if glyphs.len() < 2 {
            return;
        }
        let orig_min_x = glyphs.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
        let orig_max_x = glyphs
            .iter()
            .map(|g| g.x + g.w)
            .fold(f32::NEG_INFINITY, f32::max);
        if !orig_min_x.is_finite() || !orig_max_x.is_finite() {
            return;
        }
        let actual_w = (orig_max_x - orig_min_x).max(raw_line_w_px);
        let slack = target_w_px - actual_w;
        if slack <= 2.0 {
            if orig_max_x > target_w_px && orig_min_x > 0.0 {
                let shift_left = (orig_max_x - target_w_px).min(orig_min_x);
                if shift_left > 1e-4 {
                    for g in glyphs.iter_mut() {
                        g.x -= shift_left;
                    }
                }
            }
            return;
        }
        let mut tatweel_info: Option<(cosmic_text::fontdb::ID, Weight, f32, u16, f32)> = None;
        for g in glyphs.iter() {
            if let Some(font) = self.font_system.get_font(g.font_id, g.font_weight) {
                let sf = font.as_swash();
                let tat_gid = sf.charmap().map('\u{0640}');
                if tat_gid != 0 {
                    let tat_w = sf
                        .glyph_metrics(&[])
                        .scale(g.font_size)
                        .advance_width(tat_gid)
                        .max(1.0);
                    tatweel_info = Some((g.font_id, g.font_weight, g.font_size, tat_gid, tat_w));
                    break;
                }
            }
        }
        let Some((fid, fweight, fsize, tat_gid, tat_w)) = tatweel_info else {
            return;
        };
        let max_fit_tatweels = (slack / tat_w).floor() as usize;
        if max_fit_tatweels == 0 {
            return;
        }
        let total_tatweels = max_fit_tatweels.min(24);
        let mut candidate_slots: Vec<usize> = glyphs
            .windows(2)
            .enumerate()
            .filter_map(|(i, w)| {
                if w[0].w <= 1.0 || w[1].w <= 1.0 {
                    return None;
                }
                let (prev_g, next_g) = if w[0].start <= w[1].start {
                    (&w[0], &w[1])
                } else {
                    (&w[1], &w[0])
                };
                if prev_g.end > next_g.start
                    || prev_g.start >= prev_g.end
                    || next_g.start >= next_g.end
                {
                    return None;
                }
                let prev_slice = line_text.get(prev_g.start..prev_g.end)?;
                let next_slice = line_text.get(next_g.start..next_g.end)?;
                let prev_bases: Vec<char> = prev_slice
                    .chars()
                    .filter(|&c| !is_arabic_diacritic(c))
                    .collect();
                let next_bases: Vec<char> = next_slice
                    .chars()
                    .filter(|&c| !is_arabic_diacritic(c))
                    .collect();
                if prev_bases.len() != 1 || next_bases.len() != 1 {
                    return None;
                }
                let prev_ch = prev_bases[0];
                let next_ch = next_bases[0];
                if !is_arabic_dual_joining(prev_ch) || !is_arabic_right_connecting(next_ch) {
                    return None;
                }
                if prev_ch == 'ل' && matches!(next_ch, 'ا' | 'أ' | 'إ' | 'آ' | 'ٱ') {
                    return None;
                }
                Some(i)
            })
            .take(6)
            .collect();
        if candidate_slots.is_empty() {
            return;
        }
        let per_slot = if total_tatweels >= candidate_slots.len() {
            (total_tatweels / candidate_slots.len()).clamp(1, 4)
        } else {
            candidate_slots.truncate(total_tatweels);
            1
        };
        let mut inserted_offset = 0usize;
        for &slot in &candidate_slots {
            let pos = slot + 1 + inserted_offset;
            let right_to_left_order = glyphs[pos - 1].x > glyphs[pos].x;
            let insert_y = glyphs[pos - 1].y;
            let meta = glyphs[pos - 1].metadata;
            let flags = glyphs[pos - 1].flags;
            let shift = tat_w * (per_slot as f32);
            if right_to_left_order {
                // `glyphs` is in RTL logical order (`pos - 1` is to the right of `pos`):
                // keep `glyphs[..pos]` anchored on the right and expand `glyphs[pos..]` leftward.
                let anchor_x = glyphs[pos - 1].x;
                for g in &mut glyphs[pos..] {
                    g.x -= shift;
                }
                for k in 0..per_slot {
                    glyphs.insert(
                        pos + k,
                        GlyphSnapshot {
                            font_id: fid,
                            glyph_id: tat_gid,
                            font_size: fsize,
                            font_weight: fweight,
                            x: anchor_x - tat_w * ((k + 1) as f32),
                            y: insert_y,
                            w: tat_w,
                            metadata: meta,
                            flags,
                            start: 0,
                            end: 0,
                        },
                    );
                }
            } else {
                let insert_x = glyphs[pos - 1].x + glyphs[pos - 1].w;
                for g in &mut glyphs[pos..] {
                    g.x += shift;
                }
                for k in 0..per_slot {
                    glyphs.insert(
                        pos + k,
                        GlyphSnapshot {
                            font_id: fid,
                            glyph_id: tat_gid,
                            font_size: fsize,
                            font_weight: fweight,
                            x: insert_x + tat_w * (k as f32),
                            y: insert_y,
                            w: tat_w,
                            metadata: meta,
                            flags,
                            start: 0,
                            end: 0,
                        },
                    );
                }
            }
            inserted_offset += per_slot;
        }

        // Clamp / anchor line horizontally within `[0 .. target_w_px]` so inserted tatweels
        // never push leading or trailing glyphs outside the paragraph frame clip box.
        let new_min_x = glyphs.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
        let new_max_x = glyphs
            .iter()
            .map(|g| g.x + g.w)
            .fold(f32::NEG_INFINITY, f32::max);
        if new_min_x < 0.0 {
            let shift_right = (-new_min_x).min((target_w_px - new_max_x).max(0.0));
            if shift_right > 1e-4 {
                for g in glyphs.iter_mut() {
                    g.x += shift_right;
                }
            }
        } else {
            let anchor_right = if is_rtl {
                orig_max_x.min(target_w_px)
            } else {
                target_w_px
            };
            let shift_left = (new_max_x - anchor_right).max(0.0).min(new_min_x.max(0.0));
            if shift_left > 1e-4 {
                for g in glyphs.iter_mut() {
                    g.x -= shift_left;
                }
            }
        }
    }

    fn extract_drop_cap(
        &mut self,
        runs: &[TextRun],
        drop_cap: Option<coree_core::DropCapConfig>,
        direction: TextDirection,
    ) -> (Vec<TextRun>, Option<ShapedGlyphDraw>, Option<(f64, f64)>) {
        type DropCap = coree_core::DropCapConfig;
        let Some(dc): Option<DropCap> = drop_cap.filter(|c| c.enabled && c.lines >= 1) else {
            return (runs.to_vec(), None, None);
        };
        let mut out_runs = Vec::with_capacity(runs.len());
        let mut extracted: Option<(char, TextStyle)> = None;
        for run in runs {
            if extracted.is_none() {
                if let Some((byte_idx, ch)) =
                    run.text.char_indices().find(|(_, c)| !c.is_whitespace())
                {
                    let mut rest = String::with_capacity(run.text.len());
                    rest.push_str(&run.text[..byte_idx]);
                    rest.push_str(&run.text[(byte_idx + ch.len_utf8())..]);
                    extracted = Some((ch, run.style.clone()));
                    if !rest.is_empty() {
                        let mut nr = run.clone();
                        nr.text = rest;
                        out_runs.push(nr);
                    }
                    continue;
                }
            }
            out_runs.push(run.clone());
        }
        let Some((ch, mut dc_style)) = extracted else {
            return (runs.to_vec(), None, None);
        };
        let dc_lines = f64::from(dc.lines.clamp(1, 10));
        let line_ratio = sanitize_f64(dc_style.line_height_ratio, 1.2).clamp(0.6, 4.0);
        let target_h_mm = dc_style.font_size_mm() * line_ratio * dc_lines;
        dc_style.font_size_pt = (target_h_mm * 0.72 / (25.4 / 72.0)).clamp(4.0, 400.0);
        dc_style.direction = direction;
        let at = ArtisticText::from_plain(ch.to_string(), dc_style);
        let dc_pieces = self.layout_artistic_pieces(&at);
        let dc_draw = if dc_pieces.is_empty() {
            None
        } else {
            let mut merged = BezierPath::new();
            let fill = dc_pieces[0].fill.clone();
            for p in dc_pieces {
                merged.subpaths.extend(p.path.subpaths);
            }
            Some(ShapedGlyphDraw { path: merged, fill })
        };
        let dc_w_mm = dc_draw
            .as_ref()
            .and_then(|d| d.path.bounding_box())
            .map_or(target_h_mm * 0.65, |b| b.width().max(1.0))
            + sanitize_f64(dc.gap_mm, 1.5).max(0.0);
        (out_runs, dc_draw, Some((dc_w_mm, target_h_mm)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParagraphCursor {
    para_idx: usize,
    byte_offset: usize,
}

fn decoration_bars_mm(
    ox_px: f64,
    by_px: f64,
    style: &TextStyle,
    advance_px: f32,
) -> Option<BezierPath> {
    if !style.underline && !style.strikethrough {
        return None;
    }
    let inv_scale = 1.0 / f64::from(LAYOUT_UNITS_PER_MM);
    let adv_mm = f64::from(advance_px) * inv_scale;
    if adv_mm <= 1e-4 {
        return None;
    }
    let size_mm = style.font_size_mm();
    let mut path = BezierPath::new();
    if style.underline {
        let bar_h = (size_mm * 0.07).max(0.12);
        let bar_y = by_px * inv_scale + size_mm * 0.12;
        let bar = BezierPath::from_rect(Rect2D::from_xywh(ox_px * inv_scale, bar_y, adv_mm, bar_h));
        path.subpaths.extend(bar.subpaths);
    }
    if style.strikethrough {
        let bar_h = (size_mm * 0.06).max(0.12);
        let bar_y = by_px * inv_scale - size_mm * 0.30;
        let bar = BezierPath::from_rect(Rect2D::from_xywh(ox_px * inv_scale, bar_y, adv_mm, bar_h));
        path.subpaths.extend(bar.subpaths);
    }
    (!path.is_empty()).then_some(path)
}

fn push_pdf_glyph_to_runs(
    runs_out: &mut Vec<ShapedPdfRun>,
    blob: &PdfFontBlobEntry,
    font_size_pt: f64,
    fill: &Color,
    is_rtl: bool,
    actual_text: Option<String>,
    glyph: ShapedPdfGlyph,
) {
    if let Some(last) = runs_out.last_mut() {
        if std::sync::Arc::ptr_eq(&last.font_bytes, &blob.0)
            && last.face_index == blob.1
            && (last.font_size_pt - font_size_pt).abs() < 1e-4
            && last.fill == *fill
            && last.is_rtl == is_rtl
            && last.actual_text == actual_text
        {
            last.glyphs.push(glyph);
            return;
        }
    }
    runs_out.push(ShapedPdfRun {
        font_bytes: blob.0.clone(),
        face_index: blob.1,
        postscript_name: blob.2.clone(),
        units_per_em: blob.3,
        font_size_pt,
        fill: fill.clone(),
        is_rtl,
        actual_text,
        glyphs: vec![glyph],
    });
}

#[derive(Debug, Clone)]
struct GlyphSnapshot {
    font_id: cosmic_text::fontdb::ID,
    glyph_id: u16,
    font_size: f32,
    font_weight: Weight,
    x: f32,
    y: f32,
    w: f32,
    metadata: usize,
    flags: CacheKeyFlags,
    start: usize,
    end: usize,
}

fn snapshot_run_glyphs(run: &cosmic_text::LayoutRun<'_>) -> Vec<GlyphSnapshot> {
    run.glyphs
        .iter()
        .map(|g| GlyphSnapshot {
            font_id: g.font_id,
            glyph_id: g.glyph_id,
            font_size: g.font_size,
            font_weight: g.font_weight,
            x: g.x + g.font_size * g.x_offset,
            y: g.y - g.font_size * g.y_offset,
            w: g.w,
            metadata: g.metadata,
            flags: g.cache_key_flags | CacheKeyFlags::DISABLE_HINTING,
            start: g.start,
            end: g.end,
        })
        .collect()
}

fn collect_buffer_line_snapshots(
    buffer: &Buffer,
) -> Vec<(f32, f32, f32, String, Vec<GlyphSnapshot>)> {
    buffer
        .layout_runs()
        .map(|run| {
            let glyphs = snapshot_run_glyphs(&run);
            (
                run.line_y - run.line_top,
                run.line_height,
                run.line_w,
                run.text.to_owned(),
                glyphs,
            )
        })
        .collect()
}

fn split_runs_into_paragraphs(runs: &[TextRun]) -> Vec<Vec<TextRun>> {
    let mut paras: Vec<Vec<TextRun>> = vec![Vec::new()];
    for run in runs {
        let mut start = 0usize;
        for (idx, ch) in run.text.char_indices() {
            if ch == '\n' {
                let seg = &run.text[start..idx];
                if !seg.is_empty() {
                    let mut nr = run.clone();
                    nr.text = seg.to_owned();
                    paras.last_mut().expect("at least one para").push(nr);
                }
                paras.push(Vec::new());
                start = idx + 1;
            }
        }
        let tail = &run.text[start..];
        if !tail.is_empty() {
            let mut nr = run.clone();
            nr.text = tail.to_owned();
            paras.last_mut().expect("at least one para").push(nr);
        }
    }
    paras
}

fn slice_runs_from_byte_tracked(runs: &[TextRun], mut skip_bytes: usize) -> (Vec<TextRun>, usize) {
    let mut out = Vec::with_capacity(runs.len());
    let mut actual_skipped = 0usize;
    for run in runs {
        if skip_bytes == 0 {
            out.push(run.clone());
            continue;
        }
        let len = run.text.len();
        if skip_bytes >= len {
            skip_bytes -= len;
            actual_skipped += len;
            continue;
        }
        let mut idx = skip_bytes;
        while idx < len && !run.text.is_char_boundary(idx) {
            idx += 1;
        }
        actual_skipped += idx;
        skip_bytes = 0;
        let rest = &run.text[idx..];
        if !rest.is_empty() {
            let mut nr = run.clone();
            nr.text = rest.to_owned();
            out.push(nr);
        }
    }
    (out, actual_skipped)
}

fn preprocess_runs_for_kashida_mapped(
    runs: &[TextRun],
    block_align: TextAlign,
    orig_base_byte: usize,
) -> (Vec<TextRun>, Vec<usize>) {
    let mut out_runs = Vec::with_capacity(runs.len());
    let mut proc_to_orig = Vec::new();
    let mut orig_cursor = orig_base_byte;

    for run in runs {
        let stretch = sanitize_f32(run.style.kashida_stretch, 0.0).clamp(0.0, 2.0);
        let want_kashida = block_align == TextAlign::ForceJustifyKashida
            || run.style.align == TextAlign::ForceJustifyKashida
            || stretch > 0.01;
        if want_kashida && stretch > 0.01 && contains_arabic(&run.text) {
            let tatweels = (stretch * 2.5).round().clamp(1.0, 5.0) as usize;
            let (elongated, local_map) = apply_kashida_elongation_mapped(&run.text, tatweels);
            // `local_map` has `elongated.len() + 1` entries; push entries for `0..elongated.len()`
            for &m in &local_map[..elongated.len()] {
                proc_to_orig.push(orig_cursor + m);
            }
            let mut cloned = run.clone();
            cloned.text = elongated;
            out_runs.push(cloned);
        } else {
            for b in 0..run.text.len() {
                proc_to_orig.push(orig_cursor + b);
            }
            out_runs.push(run.clone());
        }
        orig_cursor += run.text.len();
    }
    proc_to_orig.push(orig_cursor);
    (out_runs, proc_to_orig)
}

fn apply_kashida_elongation_mapped(text: &str, tatweels_per_word: usize) -> (String, Vec<usize>) {
    if tatweels_per_word == 0 || !contains_arabic(text) {
        let mut map: Vec<usize> = (0..=text.len()).collect();
        if map.is_empty() {
            map.push(0);
        }
        return (text.to_owned(), map);
    }
    let count = tatweels_per_word.min(6);
    let mut out = String::with_capacity(text.len() + count * 8);
    let mut map = Vec::with_capacity(text.len() + count * 8 + 1);
    let mut orig_pos = 0usize;
    for token in text.split_inclusive(|c: char| c.is_whitespace()) {
        let chars: Vec<char> = token.chars().collect();
        let best_pos = find_best_kashida_index(&chars);
        for (i, &ch) in chars.iter().enumerate() {
            let ch_len = ch.len_utf8();
            for b in 0..ch_len {
                map.push(orig_pos + b);
            }
            out.push(ch);
            orig_pos += ch_len;
            if Some(i) == best_pos {
                let tat_len = '\u{0640}'.len_utf8();
                for _ in 0..count {
                    for _ in 0..tat_len {
                        map.push(orig_pos);
                    }
                    out.push('\u{0640}');
                }
            }
        }
    }
    map.push(text.len());
    (out, map)
}

#[inline]
fn map_proc_byte(proc_to_orig: &[usize], proc_byte: usize) -> usize {
    if proc_to_orig.is_empty() {
        0
    } else {
        proc_to_orig[proc_byte.min(proc_to_orig.len() - 1)]
    }
}

fn collect_frame_wrap_obstacles(
    doc: &Document,
    self_id: ObjectId,
    wrap_mode: TextWrapMode,
) -> Vec<Rect2D> {
    if wrap_mode == TextWrapMode::None || self_id.0 == 0 {
        return Vec::new();
    }
    let Some(self_wb) = doc.world_object_bounds(self_id) else {
        return Vec::new();
    };
    let self_xf = doc
        .world_transform_of(self_id)
        .unwrap_or(Affine2D::IDENTITY);
    let Some(world_to_frame) = self_xf.inverse() else {
        return Vec::new();
    };
    let page_idx = crate::export_render::find_object_page_index(doc, self_id);
    let Some(page) = doc.pages().get(page_idx) else {
        return Vec::new();
    };
    let mut obstacles = Vec::new();
    for layer in &page.layers {
        if !layer.visible {
            continue;
        }
        for &oid in &layer.objects {
            if oid == self_id {
                continue;
            }
            let Some(other) = doc.object(oid) else {
                continue;
            };
            if !other.visible || matches!(other.payload, NodePayload::ParagraphText(_)) {
                continue;
            }
            if let Some(owb) = doc.world_object_bounds(oid) {
                if owb.intersects(self_wb) {
                    if let Some(local_obs) = world_to_frame.try_transform_rect(owb) {
                        obstacles.push(local_obs);
                    }
                }
            }
        }
    }
    obstacles
}

#[allow(clippy::too_many_arguments, dead_code)]
pub(crate) fn draw_text_pieces(
    backend: &mut dyn CanvasBackend,
    pieces: &[ShapedGlyphDraw],
    transform: Affine2D,
    outline: &OutlineStyle,
    opacity: f32,
    blend_mode: BlendMode,
    view_mode: ViewMode,
    px_per_mm: f64,
) {
    draw_text_pieces_icc(
        backend,
        pieces,
        transform,
        outline,
        opacity,
        blend_mode,
        view_mode,
        px_per_mm,
        &IccColorPipeline::default(),
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_text_pieces_icc(
    backend: &mut dyn CanvasBackend,
    pieces: &[ShapedGlyphDraw],
    transform: Affine2D,
    outline: &OutlineStyle,
    opacity: f32,
    blend_mode: BlendMode,
    view_mode: ViewMode,
    px_per_mm: f64,
    icc: &IccColorPipeline,
) {
    let anti_alias = view_mode != ViewMode::Pixels;
    let op = sanitize_f32(opacity, 1.0).clamp(0.0, 1.0);
    if matches!(view_mode, ViewMode::Wireframe | ViewMode::SimpleWireframe) {
        let wire = StrokeParams {
            color: Color::BLACK,
            width: (1.0 / px_per_mm.max(1e-3)) as f32,
            cap: coree_core::LineCap::Butt,
            join: coree_core::LineJoin::Miter,
            miter_limit: 4.0,
            dash_pattern: Vec::new(),
            dash_offset: 0.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: true,
        };
        for piece in pieces {
            backend.stroke_path(&piece.path, &wire, transform);
        }
        return;
    }

    let stroke_width = if outline.is_visible() {
        let w_mm = outline.effective_width_mm();
        if outline.is_hairline {
            w_mm.max(1.0 / px_per_mm.max(1e-3)) as f32
        } else {
            w_mm as f32
        }
    } else {
        0.0
    };
    let mut stroke_params =
        StrokeParams::from_outline(outline, stroke_width, op, blend_mode, anti_alias);
    stroke_params.color = icc_srgb(&outline.color, icc);

    for piece in pieces {
        if outline.behind_fill && outline.is_visible() {
            backend.stroke_path(&piece.path, &stroke_params, transform);
        }
        let paint = BackendPaint {
            shader: crate::backend::PaintShader::Solid(icc_srgb(&piece.fill, icc)),
            opacity: op,
            blend_mode,
            anti_alias,
        };
        backend.fill_path(&piece.path, &paint, FillRule::Winding, transform);
        if !outline.behind_fill && outline.is_visible() {
            backend.stroke_path(&piece.path, &stroke_params, transform);
        }
    }
}

fn build_attrs<'a>(style: &TextStyle, family: Family<'a>, run_idx: usize) -> Attrs<'a> {
    let mut ff = FontFeatures::new();
    if style.opentype_features.liga {
        ff.enable(FeatureTag::STANDARD_LIGATURES);
    } else {
        ff.disable(FeatureTag::STANDARD_LIGATURES);
    }
    if style.opentype_features.calt {
        ff.enable(FeatureTag::CONTEXTUAL_ALTERNATES);
    } else {
        ff.disable(FeatureTag::CONTEXTUAL_ALTERNATES);
    }
    if style.opentype_features.kern {
        ff.enable(FeatureTag::KERNING);
    } else {
        ff.disable(FeatureTag::KERNING);
    }
    if style.opentype_features.dlig {
        ff.enable(FeatureTag::DISCRETIONARY_LIGATURES);
    }
    if style.opentype_features.smcp {
        ff.enable(FeatureTag::SMALL_CAPS);
    }

    let mut attrs = Attrs::new()
        .family(family)
        .weight(Weight(style.effective_weight()))
        .style(if style.italic {
            Style::Italic
        } else {
            Style::Normal
        })
        .font_features(ff)
        .metadata(run_idx);

    let font_px = (style.font_size_mm() as f32 * LAYOUT_UNITS_PER_MM).max(4.0);
    let line_ratio = sanitize_f64(style.line_height_ratio, 1.2).clamp(0.6, 4.0) as f32;
    attrs = attrs.metrics(Metrics::new(font_px, font_px * line_ratio));
    attrs
}

fn map_align(align: TextAlign, _direction: TextDirection) -> Align {
    match align {
        TextAlign::Left => Align::Left,
        TextAlign::Center => Align::Center,
        TextAlign::Right => Align::Right,
        TextAlign::Justify | TextAlign::ForceJustifyKashida => Align::Justified,
    }
}

fn detect_arabic_font_family(font_system: &FontSystem) -> Option<String> {
    let preferred = [
        "Noto Sans Arabic",
        "Noto Naskh Arabic",
        "Noto Kufi Arabic",
        "Amiri",
        "Cairo",
        "DejaVu Sans",
        "Liberation Sans",
    ];
    for target in preferred {
        if font_system.db().faces().any(|f| {
            f.families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(target))
        }) {
            return Some(target.to_owned());
        }
    }
    None
}

/// Returns `true` if `text` contains any character from Arabic script Unicode blocks.
#[must_use]
pub fn contains_arabic(text: &str) -> bool {
    text.chars().any(is_arabic_letter_or_mark)
}

#[inline]
fn is_arabic_letter_or_mark(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF
    )
}

/// Returns `true` for Arabic letters that join to BOTH the preceding and following letter
/// (valid Kashida insertion hosts when followed by another Arabic letter).
#[inline]
fn is_arabic_dual_joining(ch: char) -> bool {
    matches!(
        ch,
        'ب' | 'ت'
            | 'ث'
            | 'ج'
            | 'ح'
            | 'خ'
            | 'س'
            | 'ش'
            | 'ص'
            | 'ض'
            | 'ط'
            | 'ظ'
            | 'ع'
            | 'غ'
            | 'ف'
            | 'ق'
            | 'ك'
            | 'ل'
            | 'م'
            | 'ن'
            | 'ه'
            | 'ي'
            | 'ئ'
            | 'پ'
            | 'چ'
            | 'گ'
            | 'ک'
            | 'ی'
    )
}

/// Returns `true` for Arabic letters that accept an incoming connection on their right side.
#[inline]
fn is_arabic_right_connecting(ch: char) -> bool {
    is_arabic_dual_joining(ch)
        || matches!(
            ch,
            'ا' | 'أ' | 'إ' | 'آ' | 'د' | 'ذ' | 'ر' | 'ز' | 'ژ' | 'و' | 'ؤ' | 'ة' | 'ى' | 'ٱ'
        )
}

/// Returns `true` for Arabic diacritic / tashkeel combining marks.
#[inline]
fn is_arabic_diacritic(ch: char) -> bool {
    matches!(ch as u32, 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06ED)
}

/// Inserts typographically valid Arabic Kashida (`\u{0640}` tatweel) elongations
/// into `text` without breaking right-joining-only letters or `لا` ligatures.
#[must_use]
pub fn apply_kashida_elongation(text: &str, tatweels_per_word: usize) -> String {
    if tatweels_per_word == 0 || !contains_arabic(text) {
        return text.to_owned();
    }
    let count = tatweels_per_word.min(6);
    let mut out = String::with_capacity(text.len() + count * 8);
    for token in text.split_inclusive(|c: char| c.is_whitespace()) {
        let chars: Vec<char> = token.chars().collect();
        let best_pos = find_best_kashida_index(&chars);
        for (i, &ch) in chars.iter().enumerate() {
            out.push(ch);
            if Some(i) == best_pos {
                for _ in 0..count {
                    out.push('\u{0640}');
                }
            }
        }
    }
    out
}

/// Finds the highest-priority valid Arabic Kashida insertion position within a single word.
fn find_best_kashida_index(chars: &[char]) -> Option<usize> {
    let mut best: Option<(u8, usize)> = None;
    for i in 0..chars.len() {
        let ch = chars[i];
        if !is_arabic_dual_joining(ch) {
            continue;
        }
        // Skip any combining diacritics attached to `ch` to find the next base letter
        let mut insert_after = i;
        while insert_after + 1 < chars.len() && is_arabic_diacritic(chars[insert_after + 1]) {
            insert_after += 1;
        }
        let next_idx = insert_after + 1;
        if next_idx >= chars.len() {
            continue;
        }
        let next_ch = chars[next_idx];
        if !is_arabic_right_connecting(next_ch) {
            continue;
        }
        // Never break a Lam-Alef (`لا` / `لأ` / `لإ` / `لآ` / `لٱ`) ligature
        if ch == 'ل' && matches!(next_ch, 'ا' | 'أ' | 'إ' | 'آ' | 'ٱ') {
            continue;
        }
        // Calligraphic priority: Seen/Sheen/Sad/Dad > medial-to-final > general dual-join
        let priority: u8 = if matches!(ch, 'س' | 'ش' | 'ص' | 'ض') {
            4
        } else if matches!(next_ch, 'م' | 'ن' | 'ل' | 'ه' | 'ة' | 'ي') {
            3
        } else if matches!(ch, 'ب' | 'ت' | 'ث' | 'ف' | 'ق' | 'ك') {
            2
        } else {
            1
        };
        if best.is_none_or(|(bp, _)| priority >= bp) {
            best = Some((priority, insert_after));
        }
    }
    best.map(|(_, idx)| idx)
}

fn swash_image_to_rgba(img: &cosmic_text::SwashImage, fill_rgba: [u8; 4]) -> Vec<u8> {
    let [fr, fg, fb, fa] = fill_rgba;
    let mut rgba = vec![0u8; (img.placement.width as usize) * (img.placement.height as usize) * 4];
    match img.content {
        SwashContent::Mask => {
            for (i, &cov) in img.data.iter().enumerate() {
                let a = ((u16::from(cov) * u16::from(fa) + 127) / 255) as u8;
                let o = i * 4;
                if o + 3 < rgba.len() {
                    rgba[o] = fr;
                    rgba[o + 1] = fg;
                    rgba[o + 2] = fb;
                    rgba[o + 3] = a;
                }
            }
        }
        SwashContent::Color => {
            let copy_len = rgba.len().min(img.data.len());
            rgba[..copy_len].copy_from_slice(&img.data[..copy_len]);
        }
        SwashContent::SubpixelMask => {
            for (i, sub) in img.data.as_chunks::<3>().0.iter().enumerate() {
                let avg_cov = (u16::from(sub[0]) + u16::from(sub[1]) + u16::from(sub[2])) / 3;
                let o = i * 4;
                if o + 3 < rgba.len() {
                    rgba[o] = fr;
                    rgba[o + 1] = fg;
                    rgba[o + 2] = fb;
                    rgba[o + 3] = ((avg_cov * u16::from(fa) + 127) / 255) as u8;
                }
            }
        }
    }
    rgba
}

fn merge_paths(iter: impl IntoIterator<Item = BezierPath>) -> BezierPath {
    let mut subpaths = Vec::new();
    for p in iter {
        subpaths.extend(p.subpaths);
    }
    BezierPath { subpaths }
}

/// Convenience one-shot conversion of [`ArtisticText`] into editable vector [`BezierPath`] curves.
#[must_use]
pub fn text_to_curves(text: &ArtisticText) -> BezierPath {
    let mut engine = TextEngine::new();
    engine.artistic_text_to_curves(text)
}

/// Convenience one-shot conversion of [`ParagraphText`] into editable vector [`BezierPath`] curves.
#[must_use]
pub fn paragraph_to_curves(para: &ParagraphText) -> BezierPath {
    let mut engine = TextEngine::new();
    engine.paragraph_text_to_curves(para)
}

/// Convenience one-shot conversion of [`TextOnPath`] along `host_path` into editable vector curves.
#[must_use]
pub fn text_on_path_to_curves(top: &TextOnPath, host_path: &BezierPath) -> BezierPath {
    let mut engine = TextEngine::new();
    engine.text_on_path_to_curves(top, host_path)
}

// =============================================================================
// Interactive Font Glyph Catalog / Glyphs Docker Listing API
// =============================================================================

/// Unicode script & symbol block filter for the interactive Glyphs Docker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GlyphBlockFilter {
    #[default]
    All,
    Arabic,
    ArabicPresentationForms,
    Latin,
    SymbolsAndDingbats,
    Arrows,
    Geometric,
    Math,
    Punctuation,
}

impl GlyphBlockFilter {
    /// All selectable filter tabs for the Glyphs Docker UI.
    pub const ALL_FILTERS: [Self; 9] = [
        Self::All,
        Self::Arabic,
        Self::ArabicPresentationForms,
        Self::Latin,
        Self::SymbolsAndDingbats,
        Self::Arrows,
        Self::Geometric,
        Self::Math,
        Self::Punctuation,
    ];

    /// Human-readable display label for UI dropdowns and filter chips.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All Blocks",
            Self::Arabic => "Arabic",
            Self::ArabicPresentationForms => "Arabic Presentation Forms",
            Self::Latin => "Latin",
            Self::SymbolsAndDingbats => "Symbols & Dingbats",
            Self::Arrows => "Arrows",
            Self::Geometric => "Geometric Shapes",
            Self::Math => "Math Operators",
            Self::Punctuation => "Punctuation",
        }
    }
}

/// Single selectable glyph entry returned by [`list_glyphs_for_block`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlyphCatalogEntry {
    /// Unicode scalar character to insert into text.
    pub ch: char,
    /// Numeric Unicode codepoint (`ch as u32`).
    pub codepoint: u32,
    /// Canonical uppercase hex identifier (e.g. `"U+0627"`).
    pub codepoint_hex: String,
    /// Human-readable glyph / Unicode character name.
    pub name: String,
    /// Specific block category this glyph belongs to.
    pub block: GlyphBlockFilter,
    /// Whether the queried font family (or an installed system font when `None`) has a cmap entry.
    pub supported_by_font: bool,
}

impl TextEngine {
    /// Lists Unicode glyphs matching `filter` and `search`, annotating `supported_by_font`
    /// against `font_family` (or any installed font if `None`) using this engine's `FontSystem`.
    #[must_use]
    pub fn list_glyphs_for_block(
        &mut self,
        font_family: Option<&str>,
        filter: GlyphBlockFilter,
        search: &str,
        max_items: usize,
    ) -> Vec<GlyphCatalogEntry> {
        list_glyphs_with_font_system(
            &mut self.font_system,
            font_family,
            filter,
            search,
            max_items,
        )
    }
}

/// Lists curated Unicode glyphs for the interactive Glyphs Docker filtered by `filter`,
/// optional case-insensitive `search` (matching glyph char, descriptive name, or hex codepoint),
/// and bounded to `max_items`, checking actual OpenType/TrueType cmap coverage in `font_family`.
///
/// Uses a process-wide cached `FontSystem` so live UI search stays sub-millisecond.
#[must_use]
pub fn list_glyphs_for_block(
    font_family: Option<&str>,
    filter: GlyphBlockFilter,
    search: &str,
    max_items: usize,
) -> Vec<GlyphCatalogEntry> {
    if max_items == 0 {
        return Vec::new();
    }
    static SHARED_FONT_SYSTEM: OnceLock<Mutex<FontSystem>> = OnceLock::new();
    let mutex = SHARED_FONT_SYSTEM.get_or_init(|| Mutex::new(FontSystem::new()));
    if let Ok(mut fs) = mutex.lock() {
        list_glyphs_with_font_system(&mut fs, font_family, filter, search, max_items)
    } else {
        let mut fallback = FontSystem::new();
        list_glyphs_with_font_system(&mut fallback, font_family, filter, search, max_items)
    }
}

fn list_glyphs_with_font_system(
    font_system: &mut FontSystem,
    font_family: Option<&str>,
    filter: GlyphBlockFilter,
    search: &str,
    max_items: usize,
) -> Vec<GlyphCatalogEntry> {
    if max_items == 0 {
        return Vec::new();
    }

    // Resolve matching font face IDs & actual face weights to inspect true cmap glyph coverage
    let trimmed_fam = font_family.map(str::trim).filter(|s| !s.is_empty());
    let candidate_faces: Vec<(cosmic_text::fontdb::ID, Weight)> = if let Some(fam) = trimmed_fam {
        let mut exact: Vec<_> = font_system
            .db()
            .faces()
            .filter(|f| {
                f.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(fam))
            })
            .map(|f| (f.id, f.weight))
            .collect();
        exact.sort_by_key(|(_, w)| (w.0 as i32 - 400).abs());
        exact.truncate(8);
        if exact.is_empty() {
            let mut general: Vec<_> = font_system.db().faces().map(|f| (f.id, f.weight)).collect();
            general.sort_by_key(|(_, w)| (w.0 as i32 - 400).abs());
            general.truncate(16);
            general
        } else {
            exact
        }
    } else {
        // Sample representative faces across all weights (preferring regular first without dropping thin/bold-only faces)
        let mut faces: Vec<_> = font_system.db().faces().map(|f| (f.id, f.weight)).collect();
        faces.sort_by_key(|(_, w)| (w.0 as i32 - 400).abs());
        faces.truncate(40);
        faces
    };

    let loaded_fonts: Vec<_> = candidate_faces
        .into_iter()
        .filter_map(|(id, weight)| font_system.get_font(id, weight))
        .collect();

    let is_supported = |ch: char| -> bool {
        loaded_fonts
            .iter()
            .any(|font| font.as_swash().charmap().map(ch) != 0)
    };

    let query = search.trim().to_ascii_lowercase();
    let query_raw = search.trim();
    let blocks: &[GlyphBlockFilter] = if filter == GlyphBlockFilter::All {
        &[
            GlyphBlockFilter::Arabic,
            GlyphBlockFilter::ArabicPresentationForms,
            GlyphBlockFilter::Latin,
            GlyphBlockFilter::SymbolsAndDingbats,
            GlyphBlockFilter::Arrows,
            GlyphBlockFilter::Geometric,
            GlyphBlockFilter::Math,
            GlyphBlockFilter::Punctuation,
        ]
    } else {
        std::slice::from_ref(&filter)
    };

    let mut out = Vec::with_capacity(max_items.min(256));
    let mut seen_cp = std::collections::HashSet::new();

    for &blk in blocks {
        for (ch, name) in block_catalog_entries(blk) {
            if out.len() >= max_items {
                return out;
            }
            let cp = ch as u32;
            if !seen_cp.insert(cp) {
                continue;
            }
            let hex = format!("U+{cp:04X}");
            if !query.is_empty() {
                let hex_lc = hex.to_ascii_lowercase();
                let name_lc = name.to_ascii_lowercase();
                let char_matches = query_raw.chars().count() == 1 && query_raw.starts_with(ch);
                if !char_matches
                    && !name_lc.contains(&query)
                    && !hex_lc.contains(&query)
                    && !format!("{cp:04x}").contains(&query)
                {
                    continue;
                }
            }
            out.push(GlyphCatalogEntry {
                ch,
                codepoint: cp,
                codepoint_hex: hex,
                name,
                block: blk,
                supported_by_font: is_supported(ch),
            });
        }
    }

    out
}

fn block_catalog_entries(block: GlyphBlockFilter) -> Vec<(char, String)> {
    let mut items: Vec<(char, String)> = Vec::new();
    match block {
        GlyphBlockFilter::All => {}
        GlyphBlockFilter::Arabic => {
            const NAMED: &[(char, &str)] = &[
                ('\u{0621}', "ARABIC LETTER HAMZA"),
                ('\u{0622}', "ARABIC LETTER ALEF WITH MADDA ABOVE"),
                ('\u{0623}', "ARABIC LETTER ALEF WITH HAMZA ABOVE"),
                ('\u{0624}', "ARABIC LETTER WAW WITH HAMZA ABOVE"),
                ('\u{0625}', "ARABIC LETTER ALEF WITH HAMZA BELOW"),
                ('\u{0626}', "ARABIC LETTER YEH WITH HAMZA ABOVE"),
                ('\u{0627}', "ARABIC LETTER ALEF"),
                ('\u{0628}', "ARABIC LETTER BEH"),
                ('\u{0629}', "ARABIC LETTER TEH MARBUTA"),
                ('\u{062A}', "ARABIC LETTER TEH"),
                ('\u{062B}', "ARABIC LETTER THEH"),
                ('\u{062C}', "ARABIC LETTER JEEM"),
                ('\u{062D}', "ARABIC LETTER HAH"),
                ('\u{062E}', "ARABIC LETTER KHAH"),
                ('\u{062F}', "ARABIC LETTER DAL"),
                ('\u{0630}', "ARABIC LETTER THAL"),
                ('\u{0631}', "ARABIC LETTER REH"),
                ('\u{0632}', "ARABIC LETTER ZAIN"),
                ('\u{0633}', "ARABIC LETTER SEEN"),
                ('\u{0634}', "ARABIC LETTER SHEEN"),
                ('\u{0635}', "ARABIC LETTER SAD"),
                ('\u{0636}', "ARABIC LETTER DAD"),
                ('\u{0637}', "ARABIC LETTER TAH"),
                ('\u{0638}', "ARABIC LETTER ZAH"),
                ('\u{0639}', "ARABIC LETTER AIN"),
                ('\u{063A}', "ARABIC LETTER GHAIN"),
                ('\u{0640}', "ARABIC TATWEEL (KASHIDA)"),
                ('\u{0641}', "ARABIC LETTER FEH"),
                ('\u{0642}', "ARABIC LETTER QAF"),
                ('\u{0643}', "ARABIC LETTER KAF"),
                ('\u{0644}', "ARABIC LETTER LAM"),
                ('\u{0645}', "ARABIC LETTER MEEM"),
                ('\u{0646}', "ARABIC LETTER NOON"),
                ('\u{0647}', "ARABIC LETTER HEH"),
                ('\u{0648}', "ARABIC LETTER WAW"),
                ('\u{0649}', "ARABIC LETTER ALEF MAKSURA"),
                ('\u{064A}', "ARABIC LETTER YEH"),
                ('\u{064B}', "ARABIC FATHATAN"),
                ('\u{064C}', "ARABIC DAMMATAN"),
                ('\u{064D}', "ARABIC KASRATAN"),
                ('\u{064E}', "ARABIC FATHA"),
                ('\u{064F}', "ARABIC DAMMA"),
                ('\u{0650}', "ARABIC KASRA"),
                ('\u{0651}', "ARABIC SHADDA"),
                ('\u{0652}', "ARABIC SUKUN"),
                ('\u{0653}', "ARABIC MADDAH ABOVE"),
                ('\u{0654}', "ARABIC HAMZA ABOVE"),
                ('\u{0655}', "ARABIC HAMZA BELOW"),
                ('\u{0660}', "ARABIC-INDIC DIGIT ZERO"),
                ('\u{0661}', "ARABIC-INDIC DIGIT ONE"),
                ('\u{0662}', "ARABIC-INDIC DIGIT TWO"),
                ('\u{0663}', "ARABIC-INDIC DIGIT THREE"),
                ('\u{0664}', "ARABIC-INDIC DIGIT FOUR"),
                ('\u{0665}', "ARABIC-INDIC DIGIT FIVE"),
                ('\u{0666}', "ARABIC-INDIC DIGIT SIX"),
                ('\u{0667}', "ARABIC-INDIC DIGIT SEVEN"),
                ('\u{0668}', "ARABIC-INDIC DIGIT EIGHT"),
                ('\u{0669}', "ARABIC-INDIC DIGIT NINE"),
                ('\u{066A}', "ARABIC PERCENT SIGN"),
                ('\u{066B}', "ARABIC DECIMAL SEPARATOR"),
                ('\u{066C}', "ARABIC THOUSANDS SEPARATOR"),
                ('\u{066D}', "ARABIC FIVE POINTED STAR"),
                ('\u{060C}', "ARABIC COMMA"),
                ('\u{061B}', "ARABIC SEMICOLON"),
                ('\u{061F}', "ARABIC QUESTION MARK"),
                ('\u{067E}', "ARABIC LETTER PEH"),
                ('\u{0686}', "ARABIC LETTER TCHEH"),
                ('\u{0698}', "ARABIC LETTER JEH"),
                ('\u{06A9}', "ARABIC LETTER KEHEH"),
                ('\u{06AF}', "ARABIC LETTER GAF"),
                ('\u{06CC}', "ARABIC LETTER FARSI YEH"),
                ('\u{06DE}', "ARABIC START OF RUB EL HIZB"),
                ('\u{06E9}', "ARABIC PLACE OF SAJDAH"),
            ];
            for &(c, n) in NAMED {
                items.push((c, n.to_owned()));
            }
        }
        GlyphBlockFilter::ArabicPresentationForms => {
            const NAMED: &[(char, &str)] = &[
                ('\u{FDF2}', "ARABIC LIGATURE ALLAH ISOLATED FORM"),
                ('\u{FDFA}', "ARABIC LIGATURE SALLALLAHOU ALAYHE WASALLAM"),
                ('\u{FDFB}', "ARABIC LIGATURE JALLAJALALOUHOU"),
                ('\u{FDFC}', "RIAL SIGN"),
                ('\u{FDFD}', "ARABIC LIGATURE BISMILLAH AR-RAHMAN AR-RAHEEM"),
                ('\u{FD3E}', "ORNATE LEFT PARENTHESIS"),
                ('\u{FD3F}', "ORNATE RIGHT PARENTHESIS"),
                ('\u{FEFB}', "ARABIC LIGATURE LAM WITH ALEF ISOLATED FORM"),
                ('\u{FEFC}', "ARABIC LIGATURE LAM WITH ALEF FINAL FORM"),
                (
                    '\u{FEF5}',
                    "ARABIC LIGATURE LAM WITH ALEF WITH MADDA ABOVE ISOLATED FORM",
                ),
                (
                    '\u{FEF7}',
                    "ARABIC LIGATURE LAM WITH ALEF WITH HAMZA ABOVE ISOLATED FORM",
                ),
                (
                    '\u{FEF9}',
                    "ARABIC LIGATURE LAM WITH ALEF WITH HAMZA BELOW ISOLATED FORM",
                ),
            ];
            for &(c, n) in NAMED {
                items.push((c, n.to_owned()));
            }
            for cp in 0xFE80u32..=0xFEFCu32 {
                if let Some(ch) = char::from_u32(cp) {
                    if !NAMED.iter().any(|&(nc, _)| nc == ch) {
                        items.push((ch, format!("ARABIC PRESENTATION FORM U+{cp:04X}")));
                    }
                }
            }
        }
        GlyphBlockFilter::Latin => {
            for cp in 0x0041u32..=0x005Au32 {
                if let Some(ch) = char::from_u32(cp) {
                    items.push((ch, format!("LATIN CAPITAL LETTER {ch}")));
                }
            }
            for cp in 0x0061u32..=0x007Au32 {
                if let Some(ch) = char::from_u32(cp) {
                    items.push((ch, format!("LATIN SMALL LETTER {ch}")));
                }
            }
            for cp in 0x0030u32..=0x0039u32 {
                if let Some(ch) = char::from_u32(cp) {
                    items.push((ch, format!("DIGIT {ch}")));
                }
            }
            for cp in 0x00C0u32..=0x00FFu32 {
                if cp == 0x00D7 || cp == 0x00F7 {
                    continue;
                }
                if let Some(ch) = char::from_u32(cp) {
                    items.push((ch, format!("LATIN SUPPLEMENT LETTER {ch} (U+{cp:04X})")));
                }
            }
            const EXTRA: &[(char, &str)] = &[
                ('\u{0152}', "LATIN CAPITAL LIGATURE OE"),
                ('\u{0153}', "LATIN SMALL LIGATURE OE"),
                ('\u{0160}', "LATIN CAPITAL LETTER S WITH CARON"),
                ('\u{0161}', "LATIN SMALL LETTER S WITH CARON"),
                ('\u{0178}', "LATIN CAPITAL LETTER Y WITH DIAERESIS"),
                ('\u{017D}', "LATIN CAPITAL LETTER Z WITH CARON"),
                ('\u{017E}', "LATIN SMALL LETTER Z WITH CARON"),
            ];
            for &(c, n) in EXTRA {
                items.push((c, n.to_owned()));
            }
        }
        GlyphBlockFilter::SymbolsAndDingbats => {
            const NAMED: &[(char, &str)] = &[
                ('\u{00A9}', "COPYRIGHT SIGN"),
                ('\u{00AE}', "REGISTERED SIGN"),
                ('\u{2122}', "TRADE MARK SIGN"),
                ('\u{00A7}', "SECTION SIGN"),
                ('\u{00B6}', "PILCROW SIGN"),
                ('\u{00B0}', "DEGREE SIGN"),
                ('\u{20AC}', "EURO SIGN"),
                ('\u{00A3}', "POUND SIGN"),
                ('\u{00A5}', "YEN SIGN"),
                ('\u{00A2}', "CENT SIGN"),
                ('\u{20B9}', "INDIAN RUPEE SIGN"),
                ('\u{20BA}', "TURKISH LIRA SIGN"),
                ('\u{2600}', "BLACK SUN WITH RAYS"),
                ('\u{2601}', "CLOUD"),
                ('\u{2605}', "BLACK STAR"),
                ('\u{2606}', "WHITE STAR"),
                ('\u{260E}', "BLACK TELEPHONE"),
                ('\u{2611}', "BALLOT BOX WITH CHECK"),
                ('\u{2622}', "RADIOACTIVE SIGN"),
                ('\u{262E}', "PEACE SYMBOL"),
                ('\u{262F}', "YIN YANG"),
                ('\u{263A}', "WHITE SMILING FACE"),
                ('\u{2660}', "BLACK SPADE SUIT"),
                ('\u{2663}', "BLACK CLUB SUIT"),
                ('\u{2665}', "BLACK HEART SUIT"),
                ('\u{2666}', "BLACK DIAMOND SUIT"),
                ('\u{266A}', "EIGHTH NOTE"),
                ('\u{266B}', "BEAMED EIGHTH NOTES"),
                ('\u{2699}', "GEAR"),
                ('\u{26A0}', "WARNING SIGN"),
                ('\u{26A1}', "HIGH VOLTAGE SIGN"),
                ('\u{2702}', "BLACK SCISSORS"),
                ('\u{2708}', "AIRPLANE"),
                ('\u{2709}', "ENVELOPE"),
                ('\u{2713}', "CHECK MARK"),
                ('\u{2714}', "HEAVY CHECK MARK"),
                ('\u{2715}', "MULTIPLICATION X"),
                ('\u{2716}', "HEAVY MULTIPLICATION X"),
                ('\u{271A}', "HEAVY GREEK CROSS"),
                ('\u{2726}', "BLACK FOUR POINTED STAR"),
                ('\u{2727}', "WHITE FOUR POINTED STAR"),
                ('\u{272F}', "PINWHEEL STAR"),
                ('\u{2736}', "SIX POINTED BLACK STAR"),
                ('\u{273F}', "BLACK FLORETTE"),
                ('\u{2740}', "WHITE FLORETTE"),
                ('\u{2744}', "SNOWFLAKE"),
                ('\u{2756}', "BLACK DIAMOND MINUS WHITE X"),
                ('\u{2764}', "HEAVY BLACK HEART"),
            ];
            for &(c, n) in NAMED {
                items.push((c, n.to_owned()));
            }
        }
        GlyphBlockFilter::Arrows => {
            const NAMED: &[(char, &str)] = &[
                ('\u{2190}', "LEFTWARDS ARROW"),
                ('\u{2191}', "UPWARDS ARROW"),
                ('\u{2192}', "RIGHTWARDS ARROW"),
                ('\u{2193}', "DOWNWARDS ARROW"),
                ('\u{2194}', "LEFT RIGHT ARROW"),
                ('\u{2195}', "UP DOWN ARROW"),
                ('\u{2196}', "NORTH WEST ARROW"),
                ('\u{2197}', "NORTH EAST ARROW"),
                ('\u{2198}', "SOUTH EAST ARROW"),
                ('\u{2199}', "SOUTH WEST ARROW"),
                ('\u{21A9}', "LEFTWARDS ARROW WITH HOOK"),
                ('\u{21AA}', "RIGHTWARDS ARROW WITH HOOK"),
                ('\u{21BA}', "ANTICLOCKWISE OPEN CIRCLE ARROW"),
                ('\u{21BB}', "CLOCKWISE OPEN CIRCLE ARROW"),
                ('\u{21C4}', "RIGHTWARDS ARROW OVER LEFTWARDS ARROW"),
                ('\u{21D0}', "LEFTWARDS DOUBLE ARROW"),
                ('\u{21D1}', "UPWARDS DOUBLE ARROW"),
                ('\u{21D2}', "RIGHTWARDS DOUBLE ARROW"),
                ('\u{21D3}', "DOWNWARDS DOUBLE ARROW"),
                ('\u{21D4}', "LEFT RIGHT DOUBLE ARROW"),
                ('\u{21D5}', "UP DOWN DOUBLE ARROW"),
                ('\u{21E6}', "LEFTWARDS WHITE ARROW"),
                ('\u{21E7}', "UPWARDS WHITE ARROW"),
                ('\u{21E8}', "RIGHTWARDS WHITE ARROW"),
                ('\u{21E9}', "DOWNWARDS WHITE ARROW"),
                ('\u{2794}', "HEAVY WIDE-HEADED RIGHTWARDS ARROW"),
                ('\u{279C}', "HEAVY ROUND-TIPPED RIGHTWARDS ARROW"),
                ('\u{27A1}', "BLACK RIGHTWARDS ARROW"),
            ];
            for &(c, n) in NAMED {
                items.push((c, n.to_owned()));
            }
        }
        GlyphBlockFilter::Geometric => {
            const NAMED: &[(char, &str)] = &[
                ('\u{25A0}', "BLACK SQUARE"),
                ('\u{25A1}', "WHITE SQUARE"),
                ('\u{25A2}', "WHITE SQUARE WITH ROUNDED CORNERS"),
                ('\u{25A3}', "WHITE SQUARE CONTAINING BLACK SMALL SQUARE"),
                ('\u{25AA}', "BLACK SMALL SQUARE"),
                ('\u{25AB}', "WHITE SMALL SQUARE"),
                ('\u{25AC}', "BLACK RECTANGLE"),
                ('\u{25AD}', "WHITE RECTANGLE"),
                ('\u{25B2}', "BLACK UP-POINTING TRIANGLE"),
                ('\u{25B3}', "WHITE UP-POINTING TRIANGLE"),
                ('\u{25B6}', "BLACK RIGHT-POINTING TRIANGLE"),
                ('\u{25B7}', "WHITE RIGHT-POINTING TRIANGLE"),
                ('\u{25BC}', "BLACK DOWN-POINTING TRIANGLE"),
                ('\u{25BD}', "WHITE DOWN-POINTING TRIANGLE"),
                ('\u{25C0}', "BLACK LEFT-POINTING TRIANGLE"),
                ('\u{25C1}', "WHITE LEFT-POINTING TRIANGLE"),
                ('\u{25C6}', "BLACK DIAMOND"),
                ('\u{25C7}', "WHITE DIAMOND"),
                ('\u{25C8}', "WHITE DIAMOND CONTAINING BLACK SMALL DIAMOND"),
                ('\u{25CA}', "LOZENGE"),
                ('\u{25CB}', "WHITE CIRCLE"),
                ('\u{25CC}', "DOTTED CIRCLE"),
                ('\u{25CE}', "BULLSEYE"),
                ('\u{25CF}', "BLACK CIRCLE"),
                ('\u{25D0}', "CIRCLE WITH LEFT HALF BLACK"),
                ('\u{25D1}', "CIRCLE WITH RIGHT HALF BLACK"),
                ('\u{25D2}', "CIRCLE WITH LOWER HALF BLACK"),
                ('\u{25D3}', "CIRCLE WITH UPPER HALF BLACK"),
                ('\u{25E6}', "WHITE BULLET"),
                ('\u{25EF}', "LARGE CIRCLE"),
            ];
            for &(c, n) in NAMED {
                items.push((c, n.to_owned()));
            }
        }
        GlyphBlockFilter::Math => {
            const NAMED: &[(char, &str)] = &[
                ('\u{00B1}', "PLUS-MINUS SIGN"),
                ('\u{00D7}', "MULTIPLICATION SIGN"),
                ('\u{00F7}', "DIVISION SIGN"),
                ('\u{00AC}', "NOT SIGN"),
                ('\u{00BC}', "VULGAR FRACTION ONE QUARTER"),
                ('\u{00BD}', "VULGAR FRACTION ONE HALF"),
                ('\u{00BE}', "VULGAR FRACTION THREE QUARTERS"),
                ('\u{2200}', "FOR ALL"),
                ('\u{2202}', "PARTIAL DIFFERENTIAL"),
                ('\u{2203}', "THERE EXISTS"),
                ('\u{2205}', "EMPTY SET"),
                ('\u{2206}', "INCREMENT"),
                ('\u{2207}', "NABLA"),
                ('\u{2208}', "ELEMENT OF"),
                ('\u{2209}', "NOT AN ELEMENT OF"),
                ('\u{220F}', "N-ARY PRODUCT"),
                ('\u{2211}', "N-ARY SUMMATION"),
                ('\u{2212}', "MINUS SIGN"),
                ('\u{221A}', "SQUARE ROOT"),
                ('\u{221E}', "INFINITY"),
                ('\u{2220}', "ANGLE"),
                ('\u{2227}', "LOGICAL AND"),
                ('\u{2228}', "LOGICAL OR"),
                ('\u{2229}', "INTERSECTION"),
                ('\u{222A}', "UNION"),
                ('\u{222B}', "INTEGRAL"),
                ('\u{2248}', "ALMOST EQUAL TO"),
                ('\u{2260}', "NOT EQUAL TO"),
                ('\u{2261}', "IDENTICAL TO"),
                ('\u{2264}', "LESS-THAN OR EQUAL TO"),
                ('\u{2265}', "GREATER-THAN OR EQUAL TO"),
                ('\u{2282}', "SUBSET OF"),
                ('\u{2283}', "SUPERSET OF"),
                ('\u{2286}', "SUBSET OF OR EQUAL TO"),
                ('\u{2287}', "SUPERSET OF OR EQUAL TO"),
                ('\u{2295}', "CIRCLED PLUS"),
                ('\u{2297}', "CIRCLED TIMES"),
                ('\u{22A5}', "UP TACK (PERPENDICULAR)"),
                ('\u{22C5}', "DOT OPERATOR"),
            ];
            for &(c, n) in NAMED {
                items.push((c, n.to_owned()));
            }
        }
        GlyphBlockFilter::Punctuation => {
            const NAMED: &[(char, &str)] = &[
                ('!', "EXCLAMATION MARK"),
                ('"', "QUOTATION MARK"),
                ('#', "NUMBER SIGN"),
                ('$', "DOLLAR SIGN"),
                ('%', "PERCENT SIGN"),
                ('&', "AMPERSAND"),
                ('\'', "APOSTROPHE"),
                ('(', "LEFT PARENTHESIS"),
                (')', "RIGHT PARENTHESIS"),
                ('*', "ASTERISK"),
                (',', "COMMA"),
                ('-', "HYPHEN-MINUS"),
                ('.', "FULL STOP"),
                ('/', "SOLIDUS"),
                (':', "COLON"),
                (';', "SEMICOLON"),
                ('?', "QUESTION MARK"),
                ('@', "COMMERCIAL AT"),
                ('[', "LEFT SQUARE BRACKET"),
                (']', "RIGHT SQUARE BRACKET"),
                ('{', "LEFT CURLY BRACKET"),
                ('}', "RIGHT CURLY BRACKET"),
                ('\u{00AB}', "LEFT-POINTING DOUBLE ANGLE QUOTATION MARK"),
                ('\u{00BB}', "RIGHT-POINTING DOUBLE ANGLE QUOTATION MARK"),
                ('\u{060C}', "ARABIC COMMA"),
                ('\u{061B}', "ARABIC SEMICOLON"),
                ('\u{061F}', "ARABIC QUESTION MARK"),
                ('\u{2013}', "EN DASH"),
                ('\u{2014}', "EM DASH"),
                ('\u{2018}', "LEFT SINGLE QUOTATION MARK"),
                ('\u{2019}', "RIGHT SINGLE QUOTATION MARK"),
                ('\u{201C}', "LEFT DOUBLE QUOTATION MARK"),
                ('\u{201D}', "RIGHT DOUBLE QUOTATION MARK"),
                ('\u{2020}', "DAGGER"),
                ('\u{2021}', "DOUBLE DAGGER"),
                ('\u{2022}', "BULLET"),
                ('\u{2026}', "HORIZONTAL ELLIPSIS"),
                ('\u{2030}', "PER MILLE SIGN"),
                ('\u{2032}', "PRIME"),
                ('\u{2033}', "DOUBLE PRIME"),
                ('\u{2039}', "SINGLE LEFT-POINTING ANGLE QUOTATION MARK"),
                ('\u{203A}', "SINGLE RIGHT-POINTING ANGLE QUOTATION MARK"),
                ('\u{203D}', "INTERROBANG"),
            ];
            for &(c, n) in NAMED {
                items.push((c, n.to_owned()));
            }
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        CornerStyle, DropCapConfig, NodePayload, ParagraphListStyle, ParagraphTabAlign,
        ParagraphTabStop, ParametricShape, SceneObject, TextWrapMode,
    };

    #[test]
    fn w3a_linked_frames_wrap_dropcap_lists_tabs_and_post_shaping_kashida() {
        let mut doc = Document::new_a4("Typo");
        let obs = SceneObject::new(
            ObjectId(1),
            "Obstacle",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 15.0,
                height: 15.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_transform(Affine2D::translate(10.0, 10.0));
        doc.add_object(obs, None).unwrap();

        let p2_id = ObjectId(3);
        let mut p1 = ParagraphText::new(
            "First\tCol\nSecond line overflowing into linked frame quickly enough to wrap across lines",
            Vec2D::new(60.0, 18.0),
            TextStyle {
                font_size_pt: 12.0,
                ..TextStyle::default()
            },
        );
        p1.wrap_mode = TextWrapMode::SquareBoth;
        p1.drop_cap = Some(DropCapConfig {
            enabled: true,
            lines: 2,
            gap_mm: 1.5,
        });
        p1.list_style = Some(ParagraphListStyle::Numbered {
            start: 1,
            suffix: ".".into(),
        });
        p1.tab_stops_mm = vec![ParagraphTabStop {
            position_mm: 35.0,
            align: ParagraphTabAlign::Right,
        }];
        p1.linked_next_frame = Some(p2_id);

        let p2 = ParagraphText::new("", Vec2D::new(60.0, 40.0), TextStyle::default());

        let f1_id = doc
            .add_object(
                SceneObject::new(ObjectId(2), "F1", NodePayload::ParagraphText(p1.clone()))
                    .with_transform(Affine2D::translate(5.0, 5.0)),
                None,
            )
            .unwrap();
        doc.add_object(
            SceneObject::new(p2_id, "F2", NodePayload::ParagraphText(p2))
                .with_transform(Affine2D::translate(80.0, 5.0)),
            None,
        )
        .unwrap();

        let mut te = TextEngine::new();
        let pieces_f1 = te.layout_paragraph_in_doc(&doc, f1_id, &p1);
        let pieces_f2 = te.layout_paragraph_in_doc(
            &doc,
            p2_id,
            match &doc.object(p2_id).unwrap().payload {
                NodePayload::ParagraphText(pt) => pt,
                _ => unreachable!(),
            },
        );
        assert!(!pieces_f1.is_empty());
        assert!(!pieces_f2.is_empty(), "overflow should flow into F2");

        // Post-shaping Arabic Kashida widens shaped line toward target width
        let mut arabic = ArtisticText::from_plain("مرحبا بالعالم", TextStyle::default());
        arabic.align = TextAlign::ForceJustifyKashida;
        arabic.direction = TextDirection::Rtl;
        let base_w = te
            .layout_artistic_pieces(&ArtisticText::from_plain(
                "مرحبا بالعالم",
                TextStyle::default(),
            ))
            .iter()
            .filter_map(|p| p.path.bounding_box())
            .fold(0.0_f64, |acc, bb| acc.max(bb.max_x));
        let kashida_pieces = te.layout_artistic_pieces(&arabic);
        assert!(!kashida_pieces.is_empty() && base_w > 0.0);
    }

    #[test]
    fn post_shaping_kashida_respects_arabic_joining_and_skips_spaces_and_right_joiners() {
        let mut te = TextEngine::new();
        // Mix spaces, right-joining-only chars ('ا','د','ر','و'), Lam-Alef ('لا'), and dual-joiners ('س','م','ح','ت')
        let line_text = "سمح دار ورد لا ا م";
        let metrics = Metrics::new(24.0, 28.8);
        let mut buffer = Buffer::new(&mut te.font_system, metrics);
        {
            let mut b = buffer.borrow_with(&mut te.font_system);
            b.set_wrap(Wrap::None);
            b.set_size(None, None);
        }
        let fam = te.resolve_family("Noto Sans Arabic", true);
        let attrs = build_attrs(&TextStyle::default(), fam, 0);
        buffer.set_text(line_text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut te.font_system, false);

        let (raw_w, mut glyphs): (f32, Vec<GlyphSnapshot>) = buffer
            .layout_runs()
            .next()
            .map(|run| {
                let gs = run
                    .glyphs
                    .iter()
                    .map(|g| GlyphSnapshot {
                        font_id: g.font_id,
                        glyph_id: g.glyph_id,
                        font_size: g.font_size,
                        font_weight: g.font_weight,
                        x: g.x + g.font_size * g.x_offset,
                        y: g.y - g.font_size * g.y_offset,
                        w: g.w,
                        metadata: g.metadata,
                        flags: g.cache_key_flags | CacheKeyFlags::DISABLE_HINTING,
                        start: g.start,
                        end: g.end,
                    })
                    .collect();
                (run.line_w, gs)
            })
            .expect("shaped Arabic run");

        // Find the tatweel glyph id used by the font
        let tat_gid = glyphs.iter().find_map(|g| {
            te.font_system
                .get_font(g.font_id, g.font_weight)
                .map(|f| f.as_swash().charmap().map('\u{0640}'))
                .filter(|&gid| gid != 0)
        });

        te.insert_post_shaping_kashida(&mut glyphs, line_text, raw_w, raw_w + 180.0, true);

        if let Some(tat) = tat_gid {
            for (idx, g) in glyphs.iter().enumerate() {
                if g.glyph_id == tat && g.start == 0 && g.end == 0 {
                    // Inserted tatweel must have a valid dual-joining logical predecessor
                    // and right-connecting logical successor (never adjacent to space, ا, د, ر, و).
                    assert!(idx > 0 && idx + 1 < glyphs.len());
                    let left_g = &glyphs[idx - 1];
                    let right_g = &glyphs[idx + 1];
                    let (prev_g, next_g) = if left_g.start <= right_g.start {
                        (left_g, right_g)
                    } else {
                        (right_g, left_g)
                    };
                    let prev_slice = &line_text[prev_g.start..prev_g.end];
                    let next_slice = &line_text[next_g.start..next_g.end];
                    assert!(
                        !prev_slice.contains(' ') && !next_slice.contains(' '),
                        "tatweel must never be inserted next to whitespace: prev={prev_slice:?}, next={next_slice:?}"
                    );
                    for non_left in ['ا', 'د', 'ر', 'و'] {
                        assert!(
                            !prev_slice.contains(non_left),
                            "tatweel must never follow right-joining-only char {non_left}: prev={prev_slice:?}"
                        );
                    }
                }
            }
        }

        // Pure non-joinable line ("دار ورد ا") must receive ZERO inserted tatweels!
        let non_join_text = "دار ورد ا";
        let mut buf2 = Buffer::new(&mut te.font_system, metrics);
        {
            let mut b = buf2.borrow_with(&mut te.font_system);
            b.set_wrap(Wrap::None);
            b.set_size(None, None);
        }
        let fam2 = te.resolve_family("Noto Sans Arabic", true);
        let attrs2 = build_attrs(&TextStyle::default(), fam2, 0);
        buf2.set_text(non_join_text, &attrs2, Shaping::Advanced, None);
        buf2.shape_until_scroll(&mut te.font_system, false);
        let (w2, mut gs2): (f32, Vec<GlyphSnapshot>) = buf2
            .layout_runs()
            .next()
            .map(|run| {
                let gs = run
                    .glyphs
                    .iter()
                    .map(|g| GlyphSnapshot {
                        font_id: g.font_id,
                        glyph_id: g.glyph_id,
                        font_size: g.font_size,
                        font_weight: g.font_weight,
                        x: g.x + g.font_size * g.x_offset,
                        y: g.y - g.font_size * g.y_offset,
                        w: g.w,
                        metadata: g.metadata,
                        flags: g.cache_key_flags | CacheKeyFlags::DISABLE_HINTING,
                        start: g.start,
                        end: g.end,
                    })
                    .collect();
                (run.line_w, gs)
            })
            .expect("non-joinable Arabic run");
        let before_len = gs2.len();
        te.insert_post_shaping_kashida(&mut gs2, non_join_text, w2, w2 + 180.0, true);
        assert_eq!(
            gs2.len(),
            before_len,
            "right-joining-only Arabic words + spaces must not receive post-shaping tatweels"
        );
    }

    #[test]
    fn drop_cap_linked_next_frame_consistent_when_skip_lines_gt_zero() {
        let mut doc = coree_core::Document::new_a4("LinkedDropCap");
        let p2_id = coree_core::ObjectId(3);
        let mut p1 = ParagraphText::new(
            "Alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron",
            Vec2D::new(45.0, 14.0),
            TextStyle {
                font_size_pt: 12.0,
                ..TextStyle::default()
            },
        );
        p1.drop_cap = Some(DropCapConfig {
            enabled: true,
            lines: 2,
            gap_mm: 1.5,
        });
        p1.linked_next_frame = Some(p2_id);

        let p2 = ParagraphText::new("", Vec2D::new(45.0, 40.0), TextStyle::default());
        let f1_id = doc
            .add_object(
                coree_core::SceneObject::new(
                    coree_core::ObjectId(2),
                    "F1",
                    coree_core::NodePayload::ParagraphText(p1.clone()),
                ),
                None,
            )
            .unwrap();
        doc.add_object(
            coree_core::SceneObject::new(p2_id, "F2", coree_core::NodePayload::ParagraphText(p2)),
            None,
        )
        .unwrap();

        let mut te = TextEngine::new();
        let (f1_pieces, f1_next) = te.layout_paragraph_frame_slice(
            &p1,
            &p1,
            &[],
            ParagraphCursor {
                para_idx: 0,
                byte_offset: 0,
            },
            true,
            None,
        );
        let next_cur = f1_next.expect("F1 should overflow into F2");
        assert!(next_cur.para_idx > 0 || next_cur.byte_offset > 0);
        let (f2_pieces, f2_next) = te.layout_paragraph_frame_slice(
            &p1,
            match &doc.object(p2_id).unwrap().payload {
                coree_core::NodePayload::ParagraphText(pt) => pt,
                _ => unreachable!(),
            },
            &[],
            next_cur,
            true,
            None,
        );
        assert!(f2_next.is_none(), "F2 should consume remaining text");
        // Frame 1 has the drop-cap 'A' piece at the start; Frame 2 must NOT duplicate 'A'
        // and must shape the exact same `runs_for_shape` slice starting at `f1_consumed`.
        assert!(f1_pieces.len() > 1);
        assert!(!f2_pieces.is_empty());
        let f1_dc_bounds = f1_pieces[0].path.bounding_box().unwrap();
        let f2_first_bounds = f2_pieces[0].path.bounding_box().unwrap();
        assert!(
            f1_dc_bounds.height() > f2_first_bounds.height() * 1.3,
            "drop cap in Frame 1 ({:.2}mm) must be taller than Frame 2 body glyphs ({:.2}mm)",
            f1_dc_bounds.height(),
            f2_first_bounds.height()
        );
        // Also ensure layout_paragraph_in_doc matches for F2 without repeating drop cap
        let doc_f2 = te.layout_paragraph_in_doc(
            &doc,
            p2_id,
            match &doc.object(p2_id).unwrap().payload {
                coree_core::NodePayload::ParagraphText(pt) => pt,
                _ => unreachable!(),
            },
        );
        assert_eq!(doc_f2.len(), f2_pieces.len());
        let _ = f1_id;
    }

    #[test]
    fn subpixel_mask_bitmap_fallback_composites_rgb_coverage_to_rgba() {
        let img = cosmic_text::SwashImage {
            content: SwashContent::SubpixelMask,
            placement: cosmic_text::Placement {
                left: 0,
                top: 0,
                width: 2,
                height: 1,
            },
            data: vec![
                255, 0, 0, // pixel 0: average coverage = 85
                150, 150, 150, // pixel 1: average coverage = 150
            ],
            ..Default::default()
        };
        let rgba = swash_image_to_rgba(&img, [20, 40, 200, 255]);
        assert_eq!(rgba, vec![20, 40, 200, 85, 20, 40, 200, 150]);
    }

    #[test]
    fn rtl_paragraph_kashida_stays_anchored_within_frame_without_right_overflow() {
        let mut te = TextEngine::new();
        let cases = [
            ("تصميم لا دار ورد سمح ب", 14.0_f64, 80.0_f64, 0.0_f32),
            ("تصميم", 24.0_f64, 100.0_f64, 0.0_f32),
            ("تصميم", 24.0_f64, 100.0_f64, 1.2_f32),
        ];
        for (txt, font_pt, frame_w, stretch) in cases {
            let mut para = ParagraphText::new(
                txt,
                Vec2D::new(frame_w, 30.0),
                TextStyle {
                    font_family: "Noto Sans Arabic".to_owned(),
                    bidi_font_family: Some("Noto Sans Arabic".to_owned()),
                    font_size_pt: font_pt,
                    align: TextAlign::ForceJustifyKashida,
                    kashida_stretch: stretch,
                    ..TextStyle::default()
                },
            );
            para.align = TextAlign::ForceJustifyKashida;
            para.direction = TextDirection::Rtl;
            let pieces = te.layout_paragraph_pieces(&para);
            assert!(
                !pieces.is_empty(),
                "RTL paragraph kashida must produce shaped pieces for {txt:?}"
            );
            let mut min_x = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            for p in &pieces {
                if let Some(bb) = p.path.bounding_box() {
                    min_x = min_x.min(bb.min_x);
                    max_x = max_x.max(bb.max_x);
                }
            }
            let mut right_para = para.clone();
            right_para.align = TextAlign::Right;
            for r in &mut right_para.runs {
                r.style.align = TextAlign::Right;
                r.style.kashida_stretch = 0.0;
            }
            let right_pieces = te.layout_paragraph_pieces(&right_para);
            let right_min_x = right_pieces
                .iter()
                .filter_map(|p| p.path.bounding_box())
                .map(|b| b.min_x)
                .fold(f64::INFINITY, f64::min);
            let right_max_x = right_pieces
                .iter()
                .filter_map(|p| p.path.bounding_box())
                .map(|b| b.max_x)
                .fold(f64::NEG_INFINITY, f64::max);

            assert!(
                min_x >= -0.25 && max_x <= frame_w + 0.25,
                "RTL ForceJustifyKashida ({txt:?}, pt={font_pt}, stretch={stretch}) overflowed [0..{frame_w}]: min_x={min_x:.2}, max_x={max_x:.2}, right=({right_min_x:.2}..{right_max_x:.2})"
            );
            assert!(
                (max_x - right_max_x).abs() <= 0.6,
                "RTL ForceJustifyKashida right edge ({max_x:.2}) must stay anchored to Right alignment ({right_max_x:.2}) for {txt:?} (min_x={min_x:.2}, right_min_x={right_min_x:.2})"
            );
            if txt == "تصميم" {
                assert!(
                    min_x < right_min_x - 1.0,
                    "RTL ForceJustifyKashida must elongate leftward ({min_x:.2} < {right_min_x:.2}) for {txt:?}"
                );
            }
        }
    }

    #[test]
    fn rtl_and_ltr_bulleted_and_numbered_lists_stay_within_frame_across_all_alignments() {
        let mut te = TextEngine::new();
        let frame_w = 80.0_f64;
        let rtl_texts = [
            ("short", "تصميم جميل\nسطر ثاني"),
            (
                "long_kashida",
                "تصميم احترافي للطباعة العربية الحديثة كشيدة",
            ),
        ];
        let rtl_list_styles = [
            ParagraphListStyle::Bullet("•".to_owned()),
            ParagraphListStyle::Numbered {
                start: 1,
                suffix: ".".to_owned(),
            },
        ];
        let alignments = [
            TextAlign::Right,
            TextAlign::Left,
            TextAlign::Center,
            TextAlign::Justify,
            TextAlign::ForceJustifyKashida,
        ];
        let directions = [TextDirection::Rtl, TextDirection::Auto];

        for (label, txt) in rtl_texts {
            for ls in &rtl_list_styles {
                for &dir in &directions {
                    for &align in &alignments {
                        let mut para = ParagraphText::new(
                            txt,
                            Vec2D::new(frame_w, 40.0),
                            TextStyle {
                                font_family: "Noto Sans Arabic".to_owned(),
                                bidi_font_family: Some("Noto Sans Arabic".to_owned()),
                                font_size_pt: 14.0,
                                align,
                                direction: dir,
                                ..TextStyle::default()
                            },
                        );
                        para.align = align;
                        para.direction = dir;
                        para.list_style = Some(ls.clone());

                        let pieces = te.layout_paragraph_pieces(&para);
                        assert!(
                            !pieces.is_empty(),
                            "RTL list ({label}, {ls:?}, {dir:?}, {align:?}) produced 0 pieces"
                        );
                        let mut min_x = f64::INFINITY;
                        let mut max_x = f64::NEG_INFINITY;
                        let mut has_right_gutter_marker = false;
                        for p in &pieces {
                            if let Some(bb) = p.path.bounding_box() {
                                min_x = min_x.min(bb.min_x);
                                max_x = max_x.max(bb.max_x);
                                if bb.min_x >= frame_w - 4.05 && bb.max_x <= frame_w + 0.05 {
                                    has_right_gutter_marker = true;
                                }
                            }
                        }
                        assert!(
                            min_x >= -0.05 && max_x <= frame_w + 0.05,
                            "RTL list ({label}, {ls:?}, {dir:?}, {align:?}) overflowed [0..{frame_w}]: min_x={min_x:.2}, max_x={max_x:.2}"
                        );
                        assert!(
                            has_right_gutter_marker,
                            "RTL list ({label}, {ls:?}, {dir:?}, {align:?}) missing bullet/number marker in right gutter [{:.2}..{frame_w}]: min_x={min_x:.2}, max_x={max_x:.2}",
                            frame_w - 4.0
                        );
                    }
                }
            }
        }

        // LTR lists must indent body right by +list_indent_mm and keep markers in [0..4.0mm]
        for ls in &rtl_list_styles {
            for &align in &[
                TextAlign::Left,
                TextAlign::Right,
                TextAlign::Center,
                TextAlign::Justify,
            ] {
                let mut para = ParagraphText::new(
                    "First line of bulleted LTR list\nSecond line wrapping cleanly inside frame",
                    Vec2D::new(frame_w, 40.0),
                    TextStyle {
                        font_size_pt: 14.0,
                        align,
                        direction: TextDirection::Ltr,
                        ..TextStyle::default()
                    },
                );
                para.align = align;
                para.direction = TextDirection::Ltr;
                para.list_style = Some(ls.clone());

                let pieces = te.layout_paragraph_pieces(&para);
                assert!(!pieces.is_empty());
                let mut min_x = f64::INFINITY;
                let mut max_x = f64::NEG_INFINITY;
                let mut has_left_gutter_marker = false;
                for p in &pieces {
                    if let Some(bb) = p.path.bounding_box() {
                        min_x = min_x.min(bb.min_x);
                        max_x = max_x.max(bb.max_x);
                        if bb.min_x >= -0.05 && bb.max_x <= 4.05 {
                            has_left_gutter_marker = true;
                        }
                    }
                }
                assert!(
                    min_x >= -0.05 && max_x <= frame_w + 0.05,
                    "LTR list ({ls:?}, {align:?}) overflowed [0..{frame_w}]: min_x={min_x:.2}, max_x={max_x:.2}"
                );
                assert!(
                    has_left_gutter_marker,
                    "LTR list ({ls:?}, {align:?}) missing marker in left gutter [0..4.0]"
                );
            }
        }
    }

    #[test]
    fn contour_left_right_both_above_below_wrap_rtl_and_ltr_without_dropping_or_intruding() {
        let mut te = TextEngine::new();
        let mut make_doc_with_obstacle = |obs_x: f64,
                                          obs_y: f64,
                                          obs_w: f64,
                                          obs_h: f64,
                                          txt: &str,
                                          dir: TextDirection,
                                          align: TextAlign,
                                          wrap_mode: TextWrapMode|
         -> Vec<ShapedGlyphDraw> {
            let mut doc = Document::new_a4("WrapTest");
            let mut para = ParagraphText::new(
                txt,
                Vec2D::new(80.0, 40.0),
                TextStyle {
                    font_family: if dir == TextDirection::Rtl {
                        "Noto Sans Arabic".to_owned()
                    } else {
                        "Noto Sans".to_owned()
                    },
                    bidi_font_family: Some("Noto Sans Arabic".to_owned()),
                    font_size_pt: 14.0,
                    align,
                    direction: dir,
                    ..TextStyle::default()
                },
            );
            para.align = align;
            para.direction = dir;
            para.wrap_mode = wrap_mode;

            let p_id = doc
                .add_object(
                    SceneObject::new(
                        ObjectId(1),
                        "Para",
                        NodePayload::ParagraphText(para.clone()),
                    ),
                    None,
                )
                .unwrap();
            let obs = SceneObject::new(
                ObjectId(2),
                "Obstacle",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: obs_w,
                    height: obs_h,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_transform(Affine2D::translate(obs_x, obs_y));
            doc.add_object(obs, None).unwrap();

            te.layout_paragraph_in_doc(&doc, p_id, &para)
        };

        let bounds_of = |pieces: &[ShapedGlyphDraw]| -> (f64, f64, f64, f64) {
            let mut min_x = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_y = f64::NEG_INFINITY;
            for p in pieces {
                if let Some(bb) = p.path.bounding_box() {
                    min_x = min_x.min(bb.min_x);
                    max_x = max_x.max(bb.max_x);
                    min_y = min_y.min(bb.min_y);
                    max_y = max_y.max(bb.max_y);
                }
            }
            (min_x, max_x, min_y, max_y)
        };

        // 1. ContourLeft with obstacle [50..75mm, 0..20mm] -> usable [0.0 .. 49.5mm]
        for (label, txt, dir, align) in [
            (
                "RTL short Right",
                "تصميم",
                TextDirection::Rtl,
                TextAlign::Right,
            ),
            (
                "RTL long Kashida",
                "تصميم احترافي للطباعة العربية",
                TextDirection::Rtl,
                TextAlign::ForceJustifyKashida,
            ),
            ("LTR Right", "Hello", TextDirection::Ltr, TextAlign::Right),
            (
                "LTR Left",
                "Hello world wrapping around obstacle",
                TextDirection::Ltr,
                TextAlign::Left,
            ),
        ] {
            let pieces = make_doc_with_obstacle(
                50.0,
                0.0,
                25.0,
                20.0,
                txt,
                dir,
                align,
                TextWrapMode::ContourLeft,
            );
            assert!(
                !pieces.is_empty(),
                "ContourLeft ({label}) must produce non-empty pieces"
            );
            let (min_x, max_x, _, _) = bounds_of(&pieces);
            assert!(
                min_x >= -0.05 && max_x <= 49.55,
                "ContourLeft ({label}) intruded obstacle [50..75]: min_x={min_x:.2}, max_x={max_x:.2}"
            );
        }

        // 2. ContourRight with obstacle [5..30mm, 0..20mm] -> usable [30.5 .. 80.0mm]
        for (label, txt, dir, align) in [
            (
                "RTL short Right",
                "تصميم",
                TextDirection::Rtl,
                TextAlign::Right,
            ),
            (
                "RTL short Left",
                "تصميم",
                TextDirection::Rtl,
                TextAlign::Left,
            ),
            (
                "RTL long Kashida",
                "تصميم احترافي للطباعة العربية",
                TextDirection::Rtl,
                TextAlign::ForceJustifyKashida,
            ),
            ("LTR Left", "Hello", TextDirection::Ltr, TextAlign::Left),
        ] {
            let pieces = make_doc_with_obstacle(
                5.0,
                0.0,
                25.0,
                20.0,
                txt,
                dir,
                align,
                TextWrapMode::ContourRight,
            );
            assert!(
                !pieces.is_empty(),
                "ContourRight ({label}) must produce non-empty pieces"
            );
            let (min_x, max_x, _, _) = bounds_of(&pieces);
            assert!(
                min_x >= 30.45 && max_x <= 80.05,
                "ContourRight ({label}) intruded obstacle [5..30] or frame [80]: min_x={min_x:.2}, max_x={max_x:.2}"
            );
        }

        // 3. SquareBoth (obstacle at [5..25mm], right_space=55 > left_space=5 -> usable [25.5..80mm])
        let sq_pieces = make_doc_with_obstacle(
            5.0,
            0.0,
            20.0,
            20.0,
            "تصميم احترافي للطباعة العربية",
            TextDirection::Rtl,
            TextAlign::ForceJustifyKashida,
            TextWrapMode::SquareBoth,
        );
        assert!(!sq_pieces.is_empty());
        let (sq_min_x, sq_max_x, _, _) = bounds_of(&sq_pieces);
        assert!(
            sq_min_x >= 25.45 && sq_max_x <= 80.05,
            "SquareBoth RTL intruded obstacle: min_x={sq_min_x:.2}, max_x={sq_max_x:.2}"
        );

        // 4. AboveBelow (obstacle at y=[0..8mm] -> text pushed below y=8.5mm)
        let ab_pieces = make_doc_with_obstacle(
            0.0,
            0.0,
            80.0,
            8.0,
            "تصميم جميل",
            TextDirection::Rtl,
            TextAlign::Right,
            TextWrapMode::AboveBelow,
        );
        assert!(!ab_pieces.is_empty());
        let (_, _, ab_min_y, _) = bounds_of(&ab_pieces);
        assert!(
            ab_min_y >= 8.0,
            "AboveBelow RTL did not clear obstacle y=[0..8]: min_y={ab_min_y:.2}"
        );
    }

    #[test]
    fn rtl_drop_cap_anchors_right_without_overlapping_kashida_body() {
        let mut te = TextEngine::new();
        let frame_w = 80.0_f64;
        let mut para = ParagraphText::new(
            "تصميم احترافي للطباعة العربية الحديثة كشيدة مع استمرار السطر الثاني والثالث",
            Vec2D::new(frame_w, 40.0),
            TextStyle {
                font_family: "Noto Sans Arabic".to_owned(),
                bidi_font_family: Some("Noto Sans Arabic".to_owned()),
                font_size_pt: 14.0,
                align: TextAlign::ForceJustifyKashida,
                direction: TextDirection::Rtl,
                ..TextStyle::default()
            },
        );
        para.align = TextAlign::ForceJustifyKashida;
        para.direction = TextDirection::Rtl;
        para.drop_cap = Some(DropCapConfig {
            enabled: true,
            lines: 2,
            gap_mm: 1.5,
        });

        let pieces = te.layout_paragraph_pieces(&para);
        assert!(
            pieces.len() > 2,
            "RTL DropCap + body must emit multiple pieces"
        );
        let dc_bb = pieces[0].path.bounding_box().unwrap();
        assert!(
            dc_bb.max_x >= frame_w - 0.5 && dc_bb.max_x <= frame_w + 0.05,
            "RTL DropCap must anchor on right edge ({frame_w}mm): dc_bb={dc_bb:?}"
        );
        let mut body_min_x = f64::INFINITY;
        let mut body_in_dc_band_max_x = f64::NEG_INFINITY;
        for p in &pieces[1..] {
            if let Some(bb) = p.path.bounding_box() {
                body_min_x = body_min_x.min(bb.min_x);
                if bb.min_y < dc_bb.max_y - 0.5 {
                    body_in_dc_band_max_x = body_in_dc_band_max_x.max(bb.max_x);
                }
            }
        }
        assert!(
            body_min_x >= -0.05,
            "RTL DropCap body min_x overflowed left: {body_min_x:.2}"
        );
        assert!(
            body_in_dc_band_max_x <= dc_bb.min_x - 0.5,
            "RTL DropCap body ({body_in_dc_band_max_x:.2}) overlapped DropCap ({:.2}..{:.2})",
            dc_bb.min_x,
            dc_bb.max_x
        );
    }

    #[test]
    fn linked_frames_differing_widths_conserve_exact_glyphs() {
        let mut te = TextEngine::new();
        let txt =
            "Alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron";
        let style = TextStyle {
            font_family: "Noto Sans".to_owned(),
            font_size_pt: 12.0,
            ..TextStyle::default()
        };
        let tall_para = ParagraphText::new(txt, Vec2D::new(80.0, 100.0), style.clone());
        let tall = te.layout_paragraph_pieces(&tall_para);
        assert_eq!(tall.len(), 66);

        let mut run_chain = |sizes: &[(f64, f64)]| -> Vec<Vec<ShapedGlyphDraw>> {
            let mut doc = Document::new_a4("DiffWidth");
            let ids: Vec<ObjectId> = (0..sizes.len()).map(|i| ObjectId((i + 1) as u64)).collect();
            for (idx, &(w, h)) in sizes.iter().enumerate() {
                let mut p = if idx == 0 {
                    ParagraphText::new(txt, Vec2D::new(w, h), style.clone())
                } else {
                    ParagraphText::new("", Vec2D::new(w, h), style.clone())
                };
                if idx + 1 < ids.len() {
                    p.linked_next_frame = Some(ids[idx + 1]);
                }
                doc.add_object(
                    SceneObject::new(
                        ids[idx],
                        format!("F{}", idx + 1),
                        NodePayload::ParagraphText(p),
                    )
                    .with_transform(Affine2D::translate((idx as f64) * 90.0, 10.0)),
                    None,
                )
                .unwrap();
            }
            ids.iter()
                .map(|&id| {
                    let pt = match &doc.object(id).unwrap().payload {
                        NodePayload::ParagraphText(p) => p.clone(),
                        _ => unreachable!(),
                    };
                    te.layout_paragraph_in_doc(&doc, id, &pt)
                })
                .collect()
        };

        // 1. Narrow -> Wide (35x12mm -> 80x40mm)
        let nw = run_chain(&[(35.0, 12.0), (80.0, 40.0)]);
        assert!(!nw[0].is_empty() && !nw[1].is_empty());
        assert_eq!(
            nw[0].len() + nw[1].len(),
            tall.len(),
            "Narrow->Wide must conserve all {} glyphs (f1={}, f2={})",
            tall.len(),
            nw[0].len(),
            nw[1].len()
        );

        // 2. Wide -> Narrow (80x8mm -> 35x60mm)
        let wn = run_chain(&[(80.0, 8.0), (35.0, 60.0)]);
        assert!(!wn[0].is_empty() && !wn[1].is_empty());
        assert_eq!(
            wn[0].len() + wn[1].len(),
            tall.len(),
            "Wide->Narrow must conserve all {} glyphs without duplication (f1={}, f2={})",
            tall.len(),
            wn[0].len(),
            wn[1].len()
        );

        // 3. Three-frame differing-width chain (35x8mm -> 50x8mm -> 80x40mm)
        let chain3 = run_chain(&[(35.0, 8.0), (50.0, 8.0), (80.0, 40.0)]);
        assert!(
            !chain3[0].is_empty() && !chain3[1].is_empty() && !chain3[2].is_empty(),
            "All 3 frames should receive glyphs: ({}, {}, {})",
            chain3[0].len(),
            chain3[1].len(),
            chain3[2].len()
        );
        assert_eq!(
            chain3[0].len() + chain3[1].len() + chain3[2].len(),
            tall.len(),
            "3-frame differing-width chain must conserve all glyphs"
        );

        // 4. Rich multi-run color preservation across split boundary
        let red = Color::rgba8(200, 20, 20, 255);
        let blue = Color::rgba8(20, 40, 220, 255);
        let mut s_red = style.clone();
        s_red.fill = red.clone();
        let mut s_blue = style;
        s_blue.fill = blue.clone();
        let rich_runs = vec![
            TextRun::new("Alpha beta gamma delta epsilon ", s_red),
            TextRun::new("zeta eta theta iota kappa lambda mu nu xi omicron", s_blue),
        ];
        let mut rich_doc = Document::new_a4("RichSplit");
        let mut rf1 = ParagraphText::new("", Vec2D::new(35.0, 12.0), TextStyle::default());
        rf1.runs = rich_runs;
        rf1.linked_next_frame = Some(ObjectId(2));
        let rf2 = ParagraphText::new("", Vec2D::new(80.0, 40.0), TextStyle::default());
        rich_doc
            .add_object(
                SceneObject::new(ObjectId(1), "RF1", NodePayload::ParagraphText(rf1.clone())),
                None,
            )
            .unwrap();
        rich_doc
            .add_object(
                SceneObject::new(ObjectId(2), "RF2", NodePayload::ParagraphText(rf2.clone()))
                    .with_transform(Affine2D::translate(50.0, 0.0)),
                None,
            )
            .unwrap();
        let r1 = te.layout_paragraph_in_doc(&rich_doc, ObjectId(1), &rf1);
        let r2 = te.layout_paragraph_in_doc(&rich_doc, ObjectId(2), &rf2);
        assert_eq!(r1.len() + r2.len(), tall.len());
        assert!(r1.iter().any(|p| p.fill == red));
        assert!(r2.iter().any(|p| p.fill == blue));
    }

    #[test]
    fn linked_frames_with_frame1_obstacle_conserve_glyphs_ltr_and_rtl() {
        let mut te = TextEngine::new();
        let add_obstacle = |doc: &mut Document, id: u64, x: f64, y: f64, w: f64, h: f64| {
            doc.add_object(
                SceneObject::new(
                    ObjectId(id),
                    "Obs",
                    NodePayload::Shape(ParametricShape::Rectangle {
                        width: w,
                        height: h,
                        corner_radii: [0.0; 4],
                        corner_style: CornerStyle::Rounded,
                        lock_corners: true,
                    }),
                )
                .with_transform(Affine2D::translate(x, y)),
                None,
            )
            .unwrap();
        };

        // 1. LTR ContourLeft obstacle in F1 (60x15mm, obs [25..60, 0..15]) -> F2 (60x60mm)
        let ltr_txt =
            "Alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron";
        let ltr_style = TextStyle {
            font_family: "Noto Sans".to_owned(),
            font_size_pt: 12.0,
            ..TextStyle::default()
        };
        let mut ltr_doc = Document::new_a4("LtrObs");
        add_obstacle(&mut ltr_doc, 10, 25.0, 0.0, 35.0, 15.0);
        let mut lf1 = ParagraphText::new(ltr_txt, Vec2D::new(60.0, 15.0), ltr_style.clone());
        lf1.wrap_mode = TextWrapMode::ContourLeft;
        lf1.linked_next_frame = Some(ObjectId(2));
        let lf2 = ParagraphText::new("", Vec2D::new(60.0, 60.0), ltr_style.clone());
        ltr_doc
            .add_object(
                SceneObject::new(ObjectId(1), "F1", NodePayload::ParagraphText(lf1.clone())),
                None,
            )
            .unwrap();
        ltr_doc
            .add_object(
                SceneObject::new(ObjectId(2), "F2", NodePayload::ParagraphText(lf2.clone()))
                    .with_transform(Affine2D::translate(80.0, 0.0)),
                None,
            )
            .unwrap();

        let lf1_pieces = te.layout_paragraph_in_doc(&ltr_doc, ObjectId(1), &lf1);
        let lf2_pieces = te.layout_paragraph_in_doc(&ltr_doc, ObjectId(2), &lf2);
        let tall_ltr = te.layout_paragraph_pieces(&ParagraphText::new(
            ltr_txt,
            Vec2D::new(60.0, 80.0),
            ltr_style,
        ));
        assert!(!lf1_pieces.is_empty() && !lf2_pieces.is_empty());
        assert_eq!(lf1_pieces.len() + lf2_pieces.len(), tall_ltr.len());
        let lf1_max_x = lf1_pieces
            .iter()
            .filter_map(|p| p.path.bounding_box())
            .map(|b| b.max_x)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            lf1_max_x <= 24.55,
            "LTR F1 glyphs intruded ContourLeft obstacle [25..60]: max_x={lf1_max_x:.2}"
        );

        // 2. RTL ContourLeft obstacle in F1 (60x15mm, obs [25..60, 0..15]) -> F2 (60x60mm)
        // Test both Right and ForceJustifyKashida (plus stretch > 0)
        let ar_txt =
            "تصميم احترافي للطباعة العربية الحديثة كشيدة مع استمرار النص الكامل عبر الإطار التالي";
        for (align, stretch) in [
            (TextAlign::Right, 0.0_f32),
            (TextAlign::ForceJustifyKashida, 0.0_f32),
            (TextAlign::ForceJustifyKashida, 0.8_f32),
        ] {
            let mut ar_style = TextStyle {
                font_family: "Noto Sans Arabic".to_owned(),
                bidi_font_family: Some("Noto Sans Arabic".to_owned()),
                font_size_pt: 12.0,
                align,
                direction: TextDirection::Rtl,
                kashida_stretch: stretch,
                ..TextStyle::default()
            };

            let mut ar_doc = Document::new_a4("RtlObs");
            add_obstacle(&mut ar_doc, 10, 25.0, 0.0, 35.0, 15.0);
            let mut af1 = ParagraphText::new(ar_txt, Vec2D::new(60.0, 15.0), ar_style.clone());
            af1.align = align;
            af1.direction = TextDirection::Rtl;
            af1.wrap_mode = TextWrapMode::ContourLeft;
            af1.linked_next_frame = Some(ObjectId(2));
            let mut af2 = ParagraphText::new("", Vec2D::new(60.0, 60.0), ar_style.clone());
            af2.align = align;
            af2.direction = TextDirection::Rtl;
            ar_doc
                .add_object(
                    SceneObject::new(ObjectId(1), "AF1", NodePayload::ParagraphText(af1.clone())),
                    None,
                )
                .unwrap();
            ar_doc
                .add_object(
                    SceneObject::new(ObjectId(2), "AF2", NodePayload::ParagraphText(af2.clone()))
                        .with_transform(Affine2D::translate(80.0, 0.0)),
                    None,
                )
                .unwrap();

            let af1_pieces = te.layout_paragraph_in_doc(&ar_doc, ObjectId(1), &af1);
            let af2_pieces = te.layout_paragraph_in_doc(&ar_doc, ObjectId(2), &af2);
            assert!(
                !af1_pieces.is_empty() && !af2_pieces.is_empty(),
                "RTL ({align:?}, stretch={stretch}) must emit pieces in both F1 and F2"
            );

            let af1_max_x = af1_pieces
                .iter()
                .filter_map(|p| p.path.bounding_box())
                .map(|b| b.max_x)
                .fold(f64::NEG_INFINITY, f64::max);
            assert!(
                af1_max_x <= 24.55,
                "RTL ({align:?}, stretch={stretch}) F1 intruded obstacle [25..60]: max_x={af1_max_x:.2}"
            );
            let af2_max_x = af2_pieces
                .iter()
                .filter_map(|p| p.path.bounding_box())
                .map(|b| b.max_x)
                .fold(f64::NEG_INFINITY, f64::max);
            let af2_min_x = af2_pieces
                .iter()
                .filter_map(|p| p.path.bounding_box())
                .map(|b| b.min_x)
                .fold(f64::INFINITY, f64::min);
            assert!(
                af2_min_x >= -0.05 && af2_max_x <= 60.05,
                "RTL ({align:?}, stretch={stretch}) F2 overflowed [0..60]: ({af2_min_x:.2}..{af2_max_x:.2})"
            );

            if align == TextAlign::Right {
                ar_style.align = TextAlign::Right;
                let mut tall_ar = ParagraphText::new(ar_txt, Vec2D::new(60.0, 80.0), ar_style);
                tall_ar.align = TextAlign::Right;
                tall_ar.direction = TextDirection::Rtl;
                let tall_right = te.layout_paragraph_pieces(&tall_ar);
                assert_eq!(
                    af1_pieces.len() + af2_pieces.len(),
                    tall_right.len(),
                    "RTL Right + F1 obstacle must conserve exact glyph count (f1={}, f2={}, tall={})",
                    af1_pieces.len(),
                    af2_pieces.len(),
                    tall_right.len()
                );
            } else {
                // With ForceJustifyKashida + obstacle matching F1 height (0..10mm),
                // tall and F1+F2 have identical line widths and exact glyph count parity
                let mut exact_doc = Document::new_a4("RtlObsExact");
                add_obstacle(&mut exact_doc, 10, 25.0, 0.0, 35.0, 10.0);
                let mut tall_obs_p = af1.clone();
                tall_obs_p.frame_size = Vec2D::new(60.0, 80.0);
                tall_obs_p.linked_next_frame = None;
                exact_doc
                    .add_object(
                        SceneObject::new(
                            ObjectId(1),
                            "TallObs",
                            NodePayload::ParagraphText(tall_obs_p.clone()),
                        ),
                        None,
                    )
                    .unwrap();
                let tall_obs_pieces =
                    te.layout_paragraph_in_doc(&exact_doc, ObjectId(1), &tall_obs_p);

                let mut split_doc = Document::new_a4("RtlObsSplit");
                add_obstacle(&mut split_doc, 10, 25.0, 0.0, 35.0, 10.0);
                let mut sf1 = af1.clone();
                sf1.frame_size = Vec2D::new(60.0, 10.5);
                sf1.linked_next_frame = Some(ObjectId(2));
                split_doc
                    .add_object(
                        SceneObject::new(
                            ObjectId(1),
                            "SF1",
                            NodePayload::ParagraphText(sf1.clone()),
                        ),
                        None,
                    )
                    .unwrap();
                split_doc
                    .add_object(
                        SceneObject::new(
                            ObjectId(2),
                            "SF2",
                            NodePayload::ParagraphText(af2.clone()),
                        )
                        .with_transform(Affine2D::translate(80.0, 0.0)),
                        None,
                    )
                    .unwrap();
                let sf1_pieces = te.layout_paragraph_in_doc(&split_doc, ObjectId(1), &sf1);
                let sf2_pieces = te.layout_paragraph_in_doc(&split_doc, ObjectId(2), &af2);
                assert_eq!(
                    sf1_pieces.len() + sf2_pieces.len(),
                    tall_obs_pieces.len(),
                    "RTL Kashida ({stretch}) + F1 obstacle must conserve exact glyph+tatweel count"
                );
            }
        }
    }

    #[test]
    fn linked_frames_multi_paragraph_dropcap_and_lists_across_differing_widths() {
        let mut te = TextEngine::new();
        let multi_txt = "Alpha beta gamma delta epsilon zeta eta theta\nIota kappa lambda mu nu xi omicron pi\nRho sigma tau";
        let style = TextStyle {
            font_family: "Noto Sans".to_owned(),
            font_size_pt: 12.0,
            ..TextStyle::default()
        };

        // 1. Mid-paragraph overflow with DropCap + Numbered list across differing widths (35x14mm -> 75x50mm)
        let mut tall_p = ParagraphText::new(multi_txt, Vec2D::new(75.0, 80.0), style.clone());
        tall_p.drop_cap = Some(DropCapConfig {
            enabled: true,
            lines: 2,
            gap_mm: 1.5,
        });
        tall_p.list_style = Some(ParagraphListStyle::Numbered {
            start: 1,
            suffix: ".".to_owned(),
        });
        let tall_pieces = te.layout_paragraph_pieces(&tall_p);

        let mut doc = Document::new_a4("MultiParaDropList");
        let mut f1 = tall_p.clone();
        f1.frame_size = Vec2D::new(35.0, 14.0);
        f1.linked_next_frame = Some(ObjectId(2));
        let f2 = ParagraphText::new("", Vec2D::new(75.0, 50.0), style.clone());
        doc.add_object(
            SceneObject::new(ObjectId(1), "F1", NodePayload::ParagraphText(f1.clone())),
            None,
        )
        .unwrap();
        doc.add_object(
            SceneObject::new(ObjectId(2), "F2", NodePayload::ParagraphText(f2.clone()))
                .with_transform(Affine2D::translate(50.0, 0.0)),
            None,
        )
        .unwrap();

        let p1 = te.layout_paragraph_in_doc(&doc, ObjectId(1), &f1);
        let p2 = te.layout_paragraph_in_doc(&doc, ObjectId(2), &f2);
        assert!(!p1.is_empty() && !p2.is_empty());
        assert_eq!(
            p1.len() + p2.len(),
            tall_pieces.len(),
            "DropCap + Numbered multi-para overflow across differing widths must conserve exact piece count (f1={}, f2={}, tall={})",
            p1.len(),
            p2.len(),
            tall_pieces.len()
        );

        // DropCap 'A' must ONLY be in F1 (taller than body glyphs), never duplicated in F2
        let f1_dc_h = p1[0].path.bounding_box().unwrap().height();
        let f2_max_h = p2
            .iter()
            .filter_map(|p| p.path.bounding_box())
            .map(|b| b.height())
            .fold(0.0_f64, f64::max);
        assert!(
            f1_dc_h > f2_max_h * 1.3,
            "DropCap must NOT be duplicated in F2 (f1_dc_h={f1_dc_h:.2}, f2_max_h={f2_max_h:.2})"
        );

        // Mid-paragraph continuation at top of F2 (y < 4.5mm) must NOT have a duplicate gutter marker;
        // subsequent paragraphs in F2 (y > 4.5mm) MUST have their markers ("2.", "3.") inside [0..4mm]
        let f2_top_gutter = p2.iter().any(|p| {
            p.path
                .bounding_box()
                .is_some_and(|b| b.min_y < 4.5 && b.max_x < 4.05)
        });
        assert!(
            !f2_top_gutter,
            "Mid-paragraph continuation in F2 must NOT emit a duplicate list marker at top of F2"
        );
        let f2_new_para_markers = p2
            .iter()
            .filter(|p| {
                p.path
                    .bounding_box()
                    .is_some_and(|b| b.min_y >= 4.5 && b.max_x < 4.05)
            })
            .count();
        assert!(
            f2_new_para_markers >= 4,
            "Paragraphs 2 and 3 starting in F2 must keep their '2.' and '3.' gutter markers (got {f2_new_para_markers} pieces)"
        );

        // 2. Paragraph-boundary overflow with Bullet list across differing widths (60x6mm -> 40x40mm)
        let bullet_txt = "First short item\nSecond item in F2";
        let mut btall = ParagraphText::new(bullet_txt, Vec2D::new(40.0, 40.0), style.clone());
        btall.list_style = Some(ParagraphListStyle::Bullet("•".to_owned()));
        let btall_pieces = te.layout_paragraph_pieces(&btall);

        let mut bdoc = Document::new_a4("BulletBoundary");
        let mut bf1 = btall.clone();
        bf1.frame_size = Vec2D::new(60.0, 6.0);
        bf1.linked_next_frame = Some(ObjectId(2));
        let bf2 = ParagraphText::new("", Vec2D::new(40.0, 40.0), style);
        bdoc.add_object(
            SceneObject::new(ObjectId(1), "BF1", NodePayload::ParagraphText(bf1.clone())),
            None,
        )
        .unwrap();
        bdoc.add_object(
            SceneObject::new(ObjectId(2), "BF2", NodePayload::ParagraphText(bf2.clone()))
                .with_transform(Affine2D::translate(70.0, 0.0)),
            None,
        )
        .unwrap();
        let bp1 = te.layout_paragraph_in_doc(&bdoc, ObjectId(1), &bf1);
        let bp2 = te.layout_paragraph_in_doc(&bdoc, ObjectId(2), &bf2);
        assert_eq!(bp1.len() + bp2.len(), btall_pieces.len());
        assert!(
            bp2.iter()
                .any(|p| p.path.bounding_box().is_some_and(|b| b.max_x < 4.05)),
            "New paragraph starting at top of F2 must emit its bullet marker in left gutter"
        );
    }
}
