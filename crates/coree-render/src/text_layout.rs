// SPDX-License-Identifier: MIT OR Apache-2.0

//! Multilingual text shaping, Arabic RTL/BiDi/Kashida justification, multi-column
//! paragraph layout, Fit Text to Path, and Text-to-Curves vector outline extraction.

use crate::backend::{BackendPaint, CanvasBackend, FillRule, RgbaBuffer, StrokeParams};
use crate::viewport::ViewMode;
use coree_core::math::{sanitize_f32, sanitize_f64};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, BlendMode, Color, OutlineStyle, ParagraphText, Rect2D,
    TextAlign, TextDirection, TextOnPath, TextRun, TextStyle, Vec2D,
};
use cosmic_text::{
    Align, Attrs, Buffer, CacheKey, CacheKeyFlags, Command, Family, FontSystem, Metrics, Shaping,
    Style, SwashCache, SwashContent, Weight, Wrap,
};
use kurbo::BezPath;
use std::sync::{Mutex, OnceLock};

/// Internal layout resolution scale (logical pixels per millimeter) used during
/// unhinted glyph outline extraction so coordinates convert cleanly back to mm.
const LAYOUT_UNITS_PER_MM: f32 = 10.0;

/// Colored vector glyph geometry plus optional bitmap glyph fallback (e.g. color emoji).
#[derive(Debug, Clone)]
pub struct ShapedGlyphDraw {
    pub path: BezierPath,
    pub fill: Color,
}

/// Persistent typography engine holding `cosmic-text`'s `FontSystem` and `SwashCache`.
pub struct TextEngine {
    font_system: FontSystem,
    swash_cache: SwashCache,
    arabic_fallback_family: Option<String>,
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEngine {
    /// Initializes the text engine and discovers installed system fonts.
    #[must_use]
    pub fn new() -> Self {
        let font_system = FontSystem::new();
        let arabic_fallback_family = detect_arabic_font_family(&font_system);
        Self {
            font_system,
            swash_cache: SwashCache::new(),
            arabic_fallback_family,
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
        let pieces = self.layout_artistic_pieces(text);
        draw_text_pieces(
            backend, &pieces, transform, outline, opacity, blend_mode, view_mode, px_per_mm,
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
        if view_mode != ViewMode::Wireframe {
            backend.push_clip(&frame_path, FillRule::Winding, transform);
        }
        let pieces = self.layout_paragraph_pieces(para);
        draw_text_pieces(
            backend, &pieces, transform, outline, opacity, blend_mode, view_mode, px_per_mm,
        );
        if view_mode != ViewMode::Wireframe {
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
        let pieces = self.layout_text_on_path_pieces(top, host_path);
        draw_text_pieces(
            backend, &pieces, transform, outline, opacity, blend_mode, view_mode, px_per_mm,
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
        self.font_system.db().faces().any(|face| {
            face.families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(family))
        })
    }

    #[must_use]
    pub fn layout_artistic_pieces(&mut self, text: &ArtisticText) -> Vec<ShapedGlyphDraw> {
        if text.runs.is_empty() {
            return Vec::new();
        }
        let direction = text.effective_direction();
        let processed_runs = preprocess_runs_for_kashida(&text.runs, text.align, None);
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

        let default_family = self.resolve_family(&default_style.font_family, has_arabic);
        let default_attrs = build_attrs(&default_style, default_family, 0);
        let spans: Vec<(&str, Attrs<'_>)> = processed_runs
            .iter()
            .enumerate()
            .map(|(idx, run)| {
                let run_ar = has_arabic || contains_arabic(&run.text);
                let fam = self.resolve_family(&run.style.font_family, run_ar);
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

        self.extract_buffer_glyph_paths(&buffer, &processed_runs, x_shift, 0.0, None)
    }

    #[must_use]
    pub fn layout_paragraph_pieces(&mut self, para: &ParagraphText) -> Vec<ShapedGlyphDraw> {
        if para.runs.is_empty() {
            return Vec::new();
        }
        let direction = para.effective_direction();
        let cols = para.columns.clamp(1, 16) as usize;
        let frame_w_mm = sanitize_f64(para.frame_size.x, 80.0).max(2.0);
        let frame_h_mm = sanitize_f64(para.frame_size.y, 40.0).max(2.0);
        let gutter_mm = sanitize_f64(para.column_gutter_mm, 5.0).max(0.0);
        let total_gutter = gutter_mm * (cols.saturating_sub(1) as f64);
        let col_w_mm = ((frame_w_mm - total_gutter) / (cols as f64)).max(2.0);
        let col_w_px = (col_w_mm as f32) * LAYOUT_UNITS_PER_MM;
        let frame_h_px = (frame_h_mm as f32) * LAYOUT_UNITS_PER_MM;

        let processed_runs = preprocess_runs_for_kashida(&para.runs, para.align, Some(col_w_mm));
        let default_style = processed_runs
            .first()
            .map_or_else(TextStyle::default, |r| r.style.clone());
        let font_px = (default_style.font_size_mm() as f32 * LAYOUT_UNITS_PER_MM).max(4.0);
        let line_ratio = sanitize_f64(default_style.line_height_ratio, 1.2).clamp(0.6, 4.0) as f32;
        let line_h_px = font_px * line_ratio;
        let metrics = Metrics::new(font_px, line_h_px);

        let full_text: String = processed_runs.iter().map(|r| r.text.as_str()).collect();
        let has_arabic = contains_arabic(&full_text);
        let cosmic_align = map_align(para.align, direction);

        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        {
            let mut b = buffer.borrow_with(&mut self.font_system);
            b.set_wrap(Wrap::WordOrGlyph);
            b.set_size(Some(col_w_px), None);
        }

        let default_family = self.resolve_family(&default_style.font_family, has_arabic);
        let default_attrs = build_attrs(&default_style, default_family, 0);
        let spans: Vec<(&str, Attrs<'_>)> = processed_runs
            .iter()
            .enumerate()
            .map(|(idx, run)| {
                let run_ar = has_arabic || contains_arabic(&run.text);
                let fam = self.resolve_family(&run.style.font_family, run_ar);
                (run.text.as_str(), build_attrs(&run.style, fam, idx))
            })
            .collect();

        buffer.set_rich_text(spans, &default_attrs, Shaping::Advanced, Some(cosmic_align));
        buffer.shape_until_scroll(&mut self.font_system, false);

        // Flow laid-out visual lines across columns (right-to-left column order for RTL!)
        let mut pieces = Vec::new();
        let mut current_col = 0usize;
        let mut col_y_px = 0.0_f32;
        let is_rtl = direction == TextDirection::Rtl;

        let line_snapshots: Vec<(f32, f32, Vec<GlyphSnapshot>)> = buffer
            .layout_runs()
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
                    })
                    .collect();
                (run.line_y - run.line_top, run.line_height, glyphs)
            })
            .collect();

        for (baseline_offset_px, lh_px, glyphs) in line_snapshots {
            if col_y_px + lh_px > frame_h_px && col_y_px > 0.0 {
                current_col += 1;
                col_y_px = 0.0;
            }
            if current_col >= cols {
                break;
            }
            let visual_col = if is_rtl {
                cols - 1 - current_col
            } else {
                current_col
            };
            let col_x_px =
                ((visual_col as f64) * (col_w_mm + gutter_mm)) as f32 * LAYOUT_UNITS_PER_MM;
            let baseline_y_px = col_y_px + baseline_offset_px;

            let mut extra_spacing_px = 0.0_f32;
            let mut seen_base = false;
            for g in &glyphs {
                let run_style = processed_runs
                    .get(g.metadata)
                    .map(|r| &r.style)
                    .unwrap_or(&default_style);
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
                if let Some(path) = self.glyph_outline_mm(
                    g,
                    col_x_px + g.x + extra_spacing_px,
                    baseline_y_px + g.y,
                    run_style,
                    g.w,
                ) {
                    pieces.push(ShapedGlyphDraw {
                        path,
                        fill: run_style.fill.clone(),
                    });
                }
            }
            col_y_px += lh_px;
        }

        pieces
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
        let processed_runs = preprocess_runs_for_kashida(&top.text.runs, top.text.align, None);
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

        let default_family = self.resolve_family(&default_style.font_family, has_arabic);
        let default_attrs = build_attrs(&default_style, default_family, 0);
        let spans: Vec<(&str, Attrs<'_>)> = processed_runs
            .iter()
            .enumerate()
            .map(|(idx, run)| {
                let run_ar = has_arabic || contains_arabic(&run.text);
                let fam = self.resolve_family(&run.style.font_family, run_ar);
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

        let snapshots: Vec<GlyphSnapshot> = buffer
            .layout_runs()
            .take(1)
            .flat_map(|run| {
                run.glyphs.iter().map(|g| GlyphSnapshot {
                    font_id: g.font_id,
                    glyph_id: g.glyph_id,
                    font_size: g.font_size,
                    font_weight: g.font_weight,
                    x: g.x + g.font_size * g.x_offset,
                    y: g.y - g.font_size * g.y_offset,
                    w: g.w,
                    metadata: g.metadata,
                    flags: g.cache_key_flags | CacheKeyFlags::DISABLE_HINTING,
                })
            })
            .collect();

        if snapshots.is_empty() {
            return Vec::new();
        }

        let min_x = snapshots.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
        let max_x = snapshots
            .iter()
            .map(|g| g.x + g.w)
            .fold(f32::NEG_INFINITY, f32::max);
        let text_width_mm = f64::from((max_x - min_x).max(0.0) / LAYOUT_UNITS_PER_MM);
        let offset_mm = sanitize_f64(top.offset_mm, 0.0);
        let dist_mm = sanitize_f64(top.distance_mm, 0.0);
        let is_closed_host = host_path.subpaths.first().is_some_and(|sp| sp.closed);
        let sx = if top.flip_horizontal { -1.0 } else { 1.0 };
        let sy = if top.flip_vertical { -1.0 } else { 1.0 };

        // Helper mapping an arc-length distance onto `(anchor_mm, tangent_angle_rad)` without
        // piling glyphs onto endpoints (wraps closed loops; linearly extrapolates tiny open overhangs).
        let sample_frame = |along_mm: f64, half_w_mm: f64| -> Option<(coree_core::Point2D, f64)> {
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

        // Track active base glyph frame `(base_center_x_px, anchor_mm, angle_rad)` so zero-width
        // combining marks (Arabic tashkeel / diacritics) remain locked to their base letter
        // around tight curves and circular paths!
        let mut active_base: Option<(f32, coree_core::Point2D, f64)> = None;
        let mut pieces = Vec::with_capacity(snapshots.len());

        for (idx, g) in snapshots.iter().enumerate() {
            let run_style = processed_runs
                .get(g.metadata)
                .map(|r| &r.style)
                .unwrap_or(&default_style);
            let is_combining_mark = g.w <= 1e-3;

            let (glyph_origin_x_px, anchor, angle) = if is_combining_mark {
                // Attach to active preceding base glyph, or peek forward to next base glyph if leading
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

        pieces
    }

    fn extract_buffer_glyph_paths(
        &mut self,
        buffer: &Buffer,
        runs: &[TextRun],
        x_shift_px: f32,
        y_shift_px: f32,
        max_height_px: Option<f32>,
    ) -> Vec<ShapedGlyphDraw> {
        let default_style = runs
            .first()
            .map_or_else(TextStyle::default, |r| r.style.clone());
        let snapshots: Vec<(f32, Vec<GlyphSnapshot>)> = buffer
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
                    })
                    .collect();
                (run.line_y, glyphs)
            })
            .collect();

        let mut pieces = Vec::new();
        for (line_y, glyphs) in snapshots {
            let mut extra_spacing_px = 0.0_f32;
            let mut seen_base = false;
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
        let (cache_key, _, _) = CacheKey::new(
            g.font_id,
            g.glyph_id,
            g.font_size,
            (0.0, 0.0),
            g.font_weight,
            g.flags,
        );
        let cmds = self
            .swash_cache
            .get_outline_commands(&mut self.font_system, cache_key)?;
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
        let fam = self.resolve_family(&first.style.font_family, has_arabic);
        let attrs = build_attrs(&first.style, fam, 0);
        buffer.set_text(
            &full_text,
            &attrs,
            Shaping::Advanced,
            Some(map_align(align, direction)),
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let [fr, fg, fb, fa] = first.style.fill.to_rgba8();
        let inv_scale = 1.0 / f64::from(LAYOUT_UNITS_PER_MM);

        let glyphs: Vec<(CacheKey, i32, i32)> = buffer
            .layout_runs()
            .flat_map(|run| {
                let ly = run.line_y;
                run.glyphs.iter().map(move |g| {
                    let phys = g.physical((0.0, ly), 1.0);
                    (phys.cache_key, phys.x, phys.y)
                })
            })
            .collect();

        for (cache_key, px_x, px_y) in glyphs {
            let Some(img) = self.swash_cache.get_image(&mut self.font_system, cache_key) else {
                continue;
            };
            let w = img.placement.width;
            let h = img.placement.height;
            if w == 0 || h == 0 {
                continue;
            }
            let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
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
                SwashContent::SubpixelMask => {}
            }
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
}

#[allow(clippy::too_many_arguments)]
fn draw_text_pieces(
    backend: &mut dyn CanvasBackend,
    pieces: &[ShapedGlyphDraw],
    transform: Affine2D,
    outline: &OutlineStyle,
    opacity: f32,
    blend_mode: BlendMode,
    view_mode: ViewMode,
    px_per_mm: f64,
) {
    let anti_alias = view_mode != ViewMode::Pixels;
    let op = sanitize_f32(opacity, 1.0).clamp(0.0, 1.0);
    if view_mode == ViewMode::Wireframe {
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
    let stroke_params =
        StrokeParams::from_outline(outline, stroke_width, op, blend_mode, anti_alias);

    for piece in pieces {
        if outline.behind_fill && outline.is_visible() {
            backend.stroke_path(&piece.path, &stroke_params, transform);
        }
        let paint = BackendPaint {
            shader: crate::backend::PaintShader::Solid(piece.fill.clone()),
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
    let mut attrs = Attrs::new()
        .family(family)
        .weight(if style.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        })
        .style(if style.italic {
            Style::Italic
        } else {
            Style::Normal
        })
        .metadata(run_idx);

    let font_px = (style.font_size_mm() as f32 * LAYOUT_UNITS_PER_MM).max(4.0);
    let line_ratio = sanitize_f64(style.line_height_ratio, 1.2).clamp(0.6, 4.0) as f32;
    attrs = attrs.metrics(Metrics::new(font_px, font_px * line_ratio));
    attrs
}

fn map_align(align: TextAlign, direction: TextDirection) -> Align {
    match align {
        TextAlign::Left => Align::Left,
        TextAlign::Center => Align::Center,
        TextAlign::Right => Align::Right,
        TextAlign::Justify => Align::Justified,
        TextAlign::ForceJustifyKashida => {
            if direction == TextDirection::Rtl {
                Align::Right
            } else {
                Align::Justified
            }
        }
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

fn preprocess_runs_for_kashida(
    runs: &[TextRun],
    block_align: TextAlign,
    col_width_mm: Option<f64>,
) -> Vec<TextRun> {
    runs.iter()
        .map(|run| {
            let stretch = sanitize_f32(run.style.kashida_stretch, 0.0).clamp(0.0, 2.0);
            let want_kashida = block_align == TextAlign::ForceJustifyKashida
                || run.style.align == TextAlign::ForceJustifyKashida
                || stretch > 0.01;
            if !want_kashida || !contains_arabic(&run.text) {
                return run.clone();
            }
            let tatweels = if stretch > 0.01 {
                (stretch * 2.5).round().clamp(1.0, 5.0) as usize
            } else if let Some(cw) = col_width_mm {
                let char_w = (run.style.font_size_mm() * 0.45).max(1.0);
                let words = run.text.split_whitespace().count().max(1);
                let raw_w = (run.text.chars().count() as f64) * char_w;
                if cw > raw_w {
                    let gap_per_word = (cw - raw_w) / (words as f64);
                    (gap_per_word / (char_w * 0.8)).round().clamp(1.0, 4.0) as usize
                } else {
                    1
                }
            } else {
                2
            };
            let mut cloned = run.clone();
            cloned.text = apply_kashida_elongation(&run.text, tatweels);
            cloned
        })
        .collect()
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

    // Resolve matching font face IDs to inspect actual cmap glyph support
    let trimmed_fam = font_family.map(str::trim).filter(|s| !s.is_empty());
    let candidate_ids: Vec<cosmic_text::fontdb::ID> = if let Some(fam) = trimmed_fam {
        let exact: Vec<_> = font_system
            .db()
            .faces()
            .filter(|f| {
                f.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(fam))
            })
            .map(|f| f.id)
            .take(8)
            .collect();
        if exact.is_empty() {
            // Fall back to checking a small set of general system fonts if requested family isn't installed
            font_system.db().faces().map(|f| f.id).take(16).collect()
        } else {
            exact
        }
    } else {
        // Sample representative system faces (preferring regular faces across Latin/Arabic/Symbol fonts)
        font_system
            .db()
            .faces()
            .filter(|f| f.weight == Weight::NORMAL)
            .map(|f| f.id)
            .take(32)
            .collect()
    };

    let loaded_fonts: Vec<_> = candidate_ids
        .into_iter()
        .filter_map(|id| font_system.get_font(id, Weight::NORMAL))
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
