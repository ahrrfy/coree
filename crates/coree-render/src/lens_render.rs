// SPDX-License-Identifier: MPL-2.0

//! Complete 11-Lens non-destructive optical & color-filtering renderer.
//!
//! Supports all 11 optical & color lens types (`Brighten`, `ColorAdd`, `ColorLimit`,
//! `CustomColorMap`, `FishEye`, `HeatMap`, `Invert`, `Magnify`, `TintedGrayscale`,
//! `Transparency`, `Wireframe`) plus `frozen` snapshots, custom `viewpoint` offsets,
//! and `remove_face` blank-page background exclusion.

use crate::backend::{
    validate_surface_dimensions, BackendPaint, CanvasBackend, FillRule, PixelRect, RgbaBuffer,
    TinySkiaBackend,
};
use crate::effects_render::{apply_geometry_effects, symmetry_effect_transforms};
use crate::renderer::{render_page_surface, RenderOptions, SceneRenderer};
use crate::viewport::ViewMode;
use coree_core::math::sanitize_f32;
use coree_core::{
    Affine2D, BezierPath, Color, Document, LensConfig, LensKind, NodePayload, ObjectId, Page,
    Point2D, Rect2D, SceneObject,
};

/// Maximum single-axis dimension for a cached frozen lens snapshot buffer.
const MAX_FROZEN_SNAPSHOT_DIM: f64 = 2048.0;

/// Maximum single-axis dimension for an offscreen lens sample backdrop.
const MAX_BACKDROP_DIM: i64 = 4096;

/// Frozen lens snapshot stored in [`SceneRenderer`] when `LensConfig::frozen == true`.
#[derive(Debug, Clone)]
pub struct FrozenLensSnapshot {
    pub buffer: RgbaBuffer,
}

/// Sampleable backdrop buffer positioned at `(origin_x, origin_y)` in target pixel space.
struct SampleBackdrop {
    origin_x: f64,
    origin_y: f64,
    buffer: RgbaBuffer,
}

impl SampleBackdrop {
    fn sample_bilinear(&self, target_x: f64, target_y: f64) -> [u8; 4] {
        self.buffer
            .sample_bilinear(target_x - self.origin_x, target_y - self.origin_y)
    }

    fn get_pixel_at_target(&self, target_x: i64, target_y: i64) -> [u8; 4] {
        let lx = target_x - (self.origin_x as i64);
        let ly = target_y - (self.origin_y as i64);
        if lx >= 0
            && ly >= 0
            && lx < i64::from(self.buffer.width)
            && ly < i64::from(self.buffer.height)
        {
            self.buffer
                .get_pixel(lx as u32, ly as u32)
                .unwrap_or([0, 0, 0, 0])
        } else {
            [0, 0, 0, 0]
        }
    }
}

/// Renders an object bearing `lens: Some(lens_cfg)` by sampling the underlying
/// rendered backdrop (or re-rendering underlying objects for `Wireframe`), applying
/// the lens transformation, honoring `viewpoint`, `frozen`, and `remove_face`,
/// and compositing clipped to the lens shape.
///
/// `lens_to_surface` maps `lens_local_path` (in the lens object's own local coordinates)
/// into `backend` surface pixels (including any ancestor `Group` or `Symmetry` transforms).
/// `page_to_surface` maps page millimeter coordinates into `backend` surface pixels and is
/// used exclusively for `underlying_ids`, `remove_face`, and `lens.viewpoint`.
#[allow(clippy::too_many_arguments)]
pub fn render_lens_object(
    renderer: &mut SceneRenderer,
    doc: &Document,
    page: Option<&Page>,
    obj: &SceneObject,
    lens: &LensConfig,
    lens_local_path: &BezierPath,
    lens_to_surface: Affine2D,
    page_to_surface: Affine2D,
    view_mode: ViewMode,
    options: &RenderOptions,
    backend: &mut dyn CanvasBackend,
    underlying_ids: &[ObjectId],
    depth: usize,
) {
    if !lens_to_surface.is_finite() || !page_to_surface.is_finite() {
        return;
    }
    let Some(local_bounds) = lens_local_path.bounding_box() else {
        return;
    };
    if local_bounds.width() <= 1e-9 || local_bounds.height() <= 1e-9 {
        return;
    }
    let lens_screen_bounds = lens_to_surface.transform_rect(local_bounds);
    if !lens_screen_bounds.is_finite()
        || lens_screen_bounds.is_empty()
        || lens_screen_bounds.width() <= 1e-6
        || lens_screen_bounds.height() <= 1e-6
    {
        return;
    }

    // Overflow-safe destination rect clamped to current surface (full frame or 256×256 tile)
    let Some(lens_rect) =
        PixelRect::from_f64_rect_clamped(lens_screen_bounds, 0, backend.width(), backend.height())
    else {
        return;
    };

    if lens.frozen {
        let full_x0 = lens_screen_bounds.min_x.floor();
        let full_y0 = lens_screen_bounds.min_y.floor();
        let full_x1 = lens_screen_bounds.max_x.ceil();
        let full_y1 = lens_screen_bounds.max_y.ceil();
        let full_w_f = (full_x1 - full_x0).max(1.0);
        let full_h_f = (full_y1 - full_y0).max(1.0);

        let frozen_buf = if let Some(cached) = renderer.get_frozen_lens(obj.id) {
            cached.buffer.clone()
        } else {
            // Capture the COMPLETE unclipped lens image regardless of which tile renders first!
            let snap_scale = (MAX_FROZEN_SNAPSHOT_DIM / full_w_f.max(full_h_f)).min(1.0);
            let snap_w = (full_w_f * snap_scale)
                .ceil()
                .clamp(1.0, MAX_FROZEN_SNAPSHOT_DIM) as u32;
            let snap_h = (full_h_f * snap_scale)
                .ceil()
                .clamp(1.0, MAX_FROZEN_SNAPSHOT_DIM) as u32;

            let to_snap = Affine2D::scale(snap_scale, snap_scale)
                .compose(&Affine2D::translate(-full_x0, -full_y0));
            let snap_page_xf = to_snap.compose(&page_to_surface);
            let snap_lens_xf = to_snap.compose(&lens_to_surface);
            let snap_lens_bounds = snap_lens_xf.transform_rect(local_bounds);
            let snap_rect = PixelRect::new(0, 0, snap_w, snap_h);

            let backend_ref = if (snap_scale - 1.0).abs() < 1e-9 {
                Some((backend as &dyn CanvasBackend, full_x0, full_y0))
            } else {
                None
            };

            let fresh = compute_lens_buffer(
                renderer,
                doc,
                page,
                lens,
                snap_lens_bounds,
                snap_rect,
                snap_page_xf,
                view_mode,
                options,
                backend_ref,
                underlying_ids,
                depth,
            );
            renderer.store_frozen_lens(
                obj.id,
                FrozenLensSnapshot {
                    buffer: fresh.clone(),
                },
            );
            fresh
        };

        if !frozen_buf.is_empty() {
            let sx = full_w_f / f64::from(frozen_buf.width.max(1));
            let sy = full_h_f / f64::from(frozen_buf.height.max(1));
            let is_exact_1to1 = (sx - 1.0).abs() < 1e-6 && (sy - 1.0).abs() < 1e-6;
            let xf = Affine2D::translate(full_x0, full_y0).compose(&Affine2D::scale(sx, sy));
            backend.push_clip(lens_local_path, FillRule::Winding, lens_to_surface);
            backend.draw_pixmap(&frozen_buf, xf, obj.opacity, obj.blend_mode, !is_exact_1to1);
            backend.pop_clip();
        }
        return;
    }

    renderer.remove_frozen_lens(obj.id);
    let rendered_buf = compute_lens_buffer(
        renderer,
        doc,
        page,
        lens,
        lens_screen_bounds,
        lens_rect,
        page_to_surface,
        view_mode,
        options,
        Some((backend as &dyn CanvasBackend, 0.0, 0.0)),
        underlying_ids,
        depth,
    );

    // Composite lens pixels strictly clipped to the lens contour
    backend.push_clip(lens_local_path, FillRule::Winding, lens_to_surface);
    backend.composite_region(lens_rect, &rendered_buf, obj.opacity, obj.blend_mode);
    backend.pop_clip();
}

#[allow(clippy::too_many_arguments)]
fn compute_lens_buffer(
    renderer: &mut SceneRenderer,
    doc: &Document,
    page: Option<&Page>,
    lens: &LensConfig,
    lens_screen_bounds: Rect2D,
    target_rect: PixelRect,
    page_to_target: Affine2D,
    view_mode: ViewMode,
    options: &RenderOptions,
    backend_ref: Option<(&dyn CanvasBackend, f64, f64)>,
    underlying_ids: &[ObjectId],
    depth: usize,
) -> RgbaBuffer {
    let w = target_rect.width;
    let h = target_rect.height;
    if w == 0 || h == 0 {
        return RgbaBuffer::new_transparent(0, 0);
    }

    // Use UNCLIPPED lens screen bounds for center & radial extent so lenses crossing
    // 256×256 tile edges have identical geometry and radii on every tile.
    let lens_center_px = lens_screen_bounds.center();
    let rx = (lens_screen_bounds.width() * 0.5).max(1.0);
    let ry = (lens_screen_bounds.height() * 0.5).max(1.0);

    // `viewpoint` is expressed in page millimeter coordinates, NEVER parent-group local space.
    let sample_center_px = lens
        .viewpoint
        .filter(|p| p.is_finite())
        .map_or(lens_center_px, |vp| page_to_target.transform_point(vp));
    let shift_x = sample_center_px.x - lens_center_px.x;
    let shift_y = sample_center_px.y - lens_center_px.y;

    // Optional `remove_face` coverage mask of underlying artwork (excluding blank page background!).
    // For geometric lenses (`FishEye` / `Magnify`), warp-sample the silhouette mask using the exact
    // same coordinate mapping as the backdrop so magnified/warped artwork is never erroneously clipped!
    let kind = lens.kind.clamped();
    let (mut out, face_mask): (RgbaBuffer, Option<Vec<u8>>) = match &kind {
        LensKind::Wireframe {
            override_outline,
            override_fill,
        } => {
            let buf = render_wireframe_lens(
                renderer,
                doc,
                page,
                underlying_ids,
                target_rect,
                page_to_target,
                shift_x,
                shift_y,
                override_outline.as_ref(),
                override_fill.as_ref(),
                options,
                depth,
            );
            let mask = lens.remove_face.then(|| {
                build_underlying_coverage_mask(
                    doc,
                    underlying_ids,
                    target_rect,
                    page_to_target,
                    shift_x,
                    shift_y,
                )
            });
            (buf, mask)
        }
        LensKind::FishEye { rate_pct } => {
            let strength = f64::from(sanitize_f32(*rate_pct, 100.0).clamp(-1000.0, 1000.0)) / 100.0;
            let inv_exp = if strength >= 0.0 {
                1.0 + strength
            } else {
                1.0 / (1.0 + strength.abs())
            };
            let (sx0, sy0, sx1, sy1) = fisheye_sample_bounds(
                target_rect,
                lens_center_px,
                sample_center_px,
                rx,
                ry,
                inv_exp,
            );
            let backdrop = obtain_sample_backdrop(
                renderer,
                doc,
                page,
                underlying_ids,
                page_to_target,
                view_mode,
                options,
                backend_ref,
                sx0,
                sy0,
                sx1,
                sy1,
                depth,
            );
            let buf = render_fisheye_lens(
                &backdrop,
                target_rect,
                lens_center_px,
                sample_center_px,
                rx,
                ry,
                inv_exp,
            );
            let mask = lens.remove_face.then(|| {
                let cov_backdrop = build_underlying_coverage_backdrop(
                    doc,
                    underlying_ids,
                    page_to_target,
                    sx0,
                    sy0,
                    sx1,
                    sy1,
                );
                let mut warped = Vec::with_capacity((w as usize) * (h as usize));
                for ly in 0..h {
                    let dst_y = f64::from(target_rect.y) + f64::from(ly) + 0.5;
                    for lx in 0..w {
                        let dst_x = f64::from(target_rect.x) + f64::from(lx) + 0.5;
                        let (sx, sy) = fisheye_map_point(
                            dst_x,
                            dst_y,
                            lens_center_px,
                            sample_center_px,
                            rx,
                            ry,
                            inv_exp,
                        );
                        warped.push(cov_backdrop.sample_bilinear(sx - 0.5, sy - 0.5)[3]);
                    }
                }
                warped
            });
            (buf, mask)
        }
        LensKind::Magnify { amount } => {
            let mag = f64::from(sanitize_f32(*amount, 2.0).clamp(1.0, 100.0));
            let inv_mag = 1.0 / mag;
            let min_sx = sample_center_px.x
                + (f64::from(target_rect.x) + 0.5 - lens_center_px.x) * inv_mag
                - 0.5;
            let max_sx = sample_center_px.x
                + (f64::from(target_rect.x) + f64::from(w) - 0.5 - lens_center_px.x) * inv_mag
                - 0.5;
            let min_sy = sample_center_px.y
                + (f64::from(target_rect.y) + 0.5 - lens_center_px.y) * inv_mag
                - 0.5;
            let max_sy = sample_center_px.y
                + (f64::from(target_rect.y) + f64::from(h) - 0.5 - lens_center_px.y) * inv_mag
                - 0.5;
            let sx0 = safe_floor_i64(min_sx.min(max_sx)) - 2;
            let sx1 = safe_ceil_i64(min_sx.max(max_sx)) + 3;
            let sy0 = safe_floor_i64(min_sy.min(max_sy)) - 2;
            let sy1 = safe_ceil_i64(min_sy.max(max_sy)) + 3;

            let backdrop = obtain_sample_backdrop(
                renderer,
                doc,
                page,
                underlying_ids,
                page_to_target,
                view_mode,
                options,
                backend_ref,
                sx0,
                sy0,
                sx1,
                sy1,
                depth,
            );
            let buf = render_magnify_lens(
                &backdrop,
                target_rect,
                lens_center_px,
                sample_center_px,
                inv_mag,
            );
            let mask = lens.remove_face.then(|| {
                let cov_backdrop = build_underlying_coverage_backdrop(
                    doc,
                    underlying_ids,
                    page_to_target,
                    sx0,
                    sy0,
                    sx1,
                    sy1,
                );
                let mut warped = Vec::with_capacity((w as usize) * (h as usize));
                for ly in 0..h {
                    let dst_y = f64::from(target_rect.y) + f64::from(ly) + 0.5;
                    let sy = sample_center_px.y + (dst_y - lens_center_px.y) * inv_mag;
                    for lx in 0..w {
                        let dst_x = f64::from(target_rect.x) + f64::from(lx) + 0.5;
                        let sx = sample_center_px.x + (dst_x - lens_center_px.x) * inv_mag;
                        warped.push(cov_backdrop.sample_bilinear(sx - 0.5, sy - 0.5)[3]);
                    }
                }
                warped
            });
            (buf, mask)
        }
        _ => {
            // Color-transform lenses: Brighten, ColorAdd, ColorLimit, CustomColorMap,
            // HeatMap, Invert, TintedGrayscale, Transparency
            let has_shift = shift_x.abs() > 0.5 || shift_y.abs() > 0.5;
            let (sx0, sy0, sx1, sy1) = if has_shift {
                (
                    safe_floor_i64(f64::from(target_rect.x) + shift_x) - 2,
                    safe_floor_i64(f64::from(target_rect.y) + shift_y) - 2,
                    safe_ceil_i64(f64::from(target_rect.x) + f64::from(w) + shift_x) + 3,
                    safe_ceil_i64(f64::from(target_rect.y) + f64::from(h) + shift_y) + 3,
                )
            } else {
                (
                    i64::from(target_rect.x),
                    i64::from(target_rect.y),
                    i64::from(target_rect.x) + i64::from(w),
                    i64::from(target_rect.y) + i64::from(h),
                )
            };

            let backdrop = obtain_sample_backdrop(
                renderer,
                doc,
                page,
                underlying_ids,
                page_to_target,
                view_mode,
                options,
                backend_ref,
                sx0,
                sy0,
                sx1,
                sy1,
                depth,
            );

            let mut buf = RgbaBuffer::new_transparent(w, h);
            for ly in 0..h {
                for lx in 0..w {
                    let src_px = if has_shift {
                        let sx = f64::from(target_rect.x) + f64::from(lx) + shift_x;
                        let sy = f64::from(target_rect.y) + f64::from(ly) + shift_y;
                        backdrop.sample_bilinear(sx, sy)
                    } else {
                        backdrop.get_pixel_at_target(
                            i64::from(target_rect.x) + i64::from(lx),
                            i64::from(target_rect.y) + i64::from(ly),
                        )
                    };
                    let mapped = apply_color_lens_pixel(&kind, src_px);
                    buf.set_pixel(lx, ly, mapped);
                }
            }
            let mask = lens.remove_face.then(|| {
                build_underlying_coverage_mask(
                    doc,
                    underlying_ids,
                    target_rect,
                    page_to_target,
                    shift_x,
                    shift_y,
                )
            });
            (buf, mask)
        }
    };

    // Apply `remove_face` mask: zero out alpha where no underlying object sits underneath
    if let Some(mask) = face_mask {
        for (i, px) in out.rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let cov = mask.get(i).copied().unwrap_or(0);
            if cov == 0 {
                px[3] = 0;
            } else if cov < 255 {
                px[3] = ((u16::from(px[3]) * u16::from(cov) + 127) / 255) as u8;
            }
        }
    }

    out
}

/// Obtains a backdrop buffer covering `[sx0..sx1, sy0..sy1]` in target pixel space.
///
/// Uses a zero-re-render `backend.read_region` fast path whenever the requested sample
/// window lies completely inside the current surface, and seamlessly renders an unclipped
/// offscreen backdrop from `page` + `underlying_ids` whenever the sample window crosses a
/// `256×256` tile boundary, extends offscreen via `viewpoint`, or captures a frozen snapshot.
#[allow(clippy::too_many_arguments)]
fn obtain_sample_backdrop(
    renderer: &mut SceneRenderer,
    doc: &Document,
    page: Option<&Page>,
    underlying_ids: &[ObjectId],
    page_to_target: Affine2D,
    view_mode: ViewMode,
    options: &RenderOptions,
    backend_ref: Option<(&dyn CanvasBackend, f64, f64)>,
    sx0: i64,
    sy0: i64,
    sx1: i64,
    sy1: i64,
    depth: usize,
) -> SampleBackdrop {
    if sx1 <= sx0 || sy1 <= sy0 {
        return SampleBackdrop {
            origin_x: sx0 as f64,
            origin_y: sy0 as f64,
            buffer: RgbaBuffer::new_transparent(0, 0),
        };
    }

    // Fast path: sample window fits strictly within the already-rendered backend surface
    if let Some((backend, bx_off, by_off)) = backend_ref {
        if bx_off.is_finite()
            && by_off.is_finite()
            && bx_off.fract() == 0.0
            && by_off.fract() == 0.0
        {
            let b_off_x = bx_off as i64;
            let b_off_y = by_off as i64;
            let bsx0 = sx0.saturating_add(b_off_x);
            let bsy0 = sy0.saturating_add(b_off_y);
            let bsx1 = sx1.saturating_add(b_off_x);
            let bsy1 = sy1.saturating_add(b_off_y);
            if bsx0 >= 0
                && bsy0 >= 0
                && bsx1 <= i64::from(backend.width())
                && bsy1 <= i64::from(backend.height())
            {
                let rect = PixelRect::new(
                    bsx0 as i32,
                    bsy0 as i32,
                    (bsx1 - bsx0) as u32,
                    (bsy1 - bsy0) as u32,
                );
                return SampleBackdrop {
                    origin_x: sx0 as f64,
                    origin_y: sy0 as f64,
                    buffer: backend.read_region(rect),
                };
            }
        }
    }

    // Offscreen unclipped backdrop path for cross-tile / offscreen-viewpoint / frozen capture
    let bw_i64 = sx1.saturating_sub(sx0);
    let bh_i64 = sy1.saturating_sub(sy0);
    if bw_i64 <= 0
        || bh_i64 <= 0
        || bw_i64 > MAX_BACKDROP_DIM
        || bh_i64 > MAX_BACKDROP_DIM
        || validate_surface_dimensions(bw_i64 as u32, bh_i64 as u32).is_err()
    {
        if let Some((backend, bx_off, by_off)) = backend_ref {
            return SampleBackdrop {
                origin_x: -bx_off,
                origin_y: -by_off,
                buffer: backend.snapshot_rgba(),
            };
        }
        return SampleBackdrop {
            origin_x: sx0 as f64,
            origin_y: sy0 as f64,
            buffer: RgbaBuffer::new_filled(1, 1, options.page_bg.to_rgba8()),
        };
    }

    let bw = bw_i64 as u32;
    let bh = bh_i64 as u32;
    let Ok(mut sub) = TinySkiaBackend::new(bw, bh) else {
        return SampleBackdrop {
            origin_x: sx0 as f64,
            origin_y: sy0 as f64,
            buffer: RgbaBuffer::new_filled(1, 1, options.page_bg.to_rgba8()),
        };
    };

    let local_page_xf = Affine2D::translate(-(sx0 as f64), -(sy0 as f64)).compose(&page_to_target);
    let px_per_mm = estimate_scale_xf(&local_page_xf);

    sub.clear(options.canvas_bg.clone());
    if let Some(p) = page {
        render_page_surface(p, local_page_xf, px_per_mm, options, &mut sub);
    } else if options.page_bg.alpha() > 1e-6 {
        sub.clear(options.page_bg.clone());
    }

    for (idx, &oid) in underlying_ids.iter().enumerate() {
        renderer.render_object_by_id(
            doc,
            page,
            oid,
            local_page_xf,
            local_page_xf,
            view_mode,
            options,
            &mut sub,
            &underlying_ids[..idx],
            depth + 1,
        );
    }

    SampleBackdrop {
        origin_x: sx0 as f64,
        origin_y: sy0 as f64,
        buffer: sub.into_rgba(),
    }
}

/// Evaluates all color-space lens variants accurately on a single RGBA8 pixel.
fn apply_color_lens_pixel(kind: &LensKind, px: [u8; 4]) -> [u8; 4] {
    match kind {
        LensKind::HeatMap {
            palette_rotation_deg,
        } => {
            // 5-stop thermal palette (deep-blue -> cyan -> green -> yellow -> red)
            // with smooth ramp rotation by `palette_rotation_deg`.
            let col = Color::rgba8(px[0], px[1], px[2], px[3]);
            let lum = col.luminance().clamp(0.0, 1.0);
            let rot_frac = sanitize_f32(*palette_rotation_deg, 0.0).rem_euclid(360.0) / 360.0;
            let t = (lum + rot_frac).rem_euclid(1.0);
            let thermal = sample_heatmap_5stop(t);
            [thermal[0], thermal[1], thermal[2], px[3]]
        }
        other => {
            let col = Color::rgba8(px[0], px[1], px[2], px[3]);
            other.apply_color(&col).to_rgba8()
        }
    }
}

/// 5-stop thermal infrared palette: Deep Blue -> Cyan -> Green -> Yellow -> Red.
fn sample_heatmap_5stop(t: f32) -> [u8; 3] {
    const STOPS: [[f32; 3]; 5] = [
        [0.05, 0.05, 0.55], // 0.00: cold deep blue
        [0.00, 0.80, 0.90], // 0.25: cool cyan
        [0.10, 0.85, 0.20], // 0.50: warm green
        [1.00, 0.92, 0.10], // 0.75: hot yellow
        [0.95, 0.12, 0.08], // 1.00: blazing red
    ];
    let scaled = t.clamp(0.0, 1.0) * 4.0;
    let idx = (scaled.floor() as usize).min(3);
    let frac = scaled - idx as f32;
    let a = STOPS[idx];
    let b = STOPS[idx + 1];
    [
        ((a[0] + (b[0] - a[0]) * frac) * 255.0).round() as u8,
        ((a[1] + (b[1] - a[1]) * frac) * 255.0).round() as u8,
        ((a[2] + (b[2] - a[2]) * frac) * 255.0).round() as u8,
    ]
}

#[inline]
fn fisheye_map_point(
    dst_x: f64,
    dst_y: f64,
    lens_center_px: Point2D,
    sample_center_px: Point2D,
    rx: f64,
    ry: f64,
    inv_exp: f64,
) -> (f64, f64) {
    let nx = (dst_x - lens_center_px.x) / rx;
    let ny = (dst_y - lens_center_px.y) / ry;
    let r = nx.hypot(ny);
    if r > 1e-9 && r < 1.0 {
        let r_src = r.powf(inv_exp);
        let scale = r_src / r;
        (
            sample_center_px.x + nx * scale * rx,
            sample_center_px.y + ny * scale * ry,
        )
    } else {
        (
            sample_center_px.x + (dst_x - lens_center_px.x),
            sample_center_px.y + (dst_y - lens_center_px.y),
        )
    }
}

/// Computes the exact sample-space pixel bounding box required to evaluate FishEye
/// over `target_rect`, checking perimeter edges and axis cross-sections through `lens_center_px`.
fn fisheye_sample_bounds(
    target_rect: PixelRect,
    lens_center_px: Point2D,
    sample_center_px: Point2D,
    rx: f64,
    ry: f64,
    inv_exp: f64,
) -> (i64, i64, i64, i64) {
    let mut min_sx = f64::INFINITY;
    let mut max_sx = f64::NEG_INFINITY;
    let mut min_sy = f64::INFINITY;
    let mut max_sy = f64::NEG_INFINITY;

    let mut include = |dst_x: f64, dst_y: f64| {
        let (sx, sy) = fisheye_map_point(
            dst_x,
            dst_y,
            lens_center_px,
            sample_center_px,
            rx,
            ry,
            inv_exp,
        );
        if sx.is_finite() && sy.is_finite() {
            min_sx = min_sx.min(sx - 0.5);
            max_sx = max_sx.max(sx - 0.5);
            min_sy = min_sy.min(sy - 0.5);
            max_sy = max_sy.max(sy - 0.5);
        }
    };

    let x_start = f64::from(target_rect.x) + 0.5;
    let x_end = f64::from(target_rect.x) + f64::from(target_rect.width) - 0.5;
    let y_start = f64::from(target_rect.y) + 0.5;
    let y_end = f64::from(target_rect.y) + f64::from(target_rect.height) - 0.5;

    for lx in 0..target_rect.width {
        let dx = f64::from(target_rect.x) + f64::from(lx) + 0.5;
        include(dx, y_start);
        include(dx, y_end);
        if lens_center_px.y >= y_start && lens_center_px.y <= y_end {
            include(dx, lens_center_px.y);
        }
    }
    for ly in 0..target_rect.height {
        let dy = f64::from(target_rect.y) + f64::from(ly) + 0.5;
        include(x_start, dy);
        include(x_end, dy);
        if lens_center_px.x >= x_start && lens_center_px.x <= x_end {
            include(lens_center_px.x, dy);
        }
    }

    if !min_sx.is_finite() || !max_sx.is_finite() || !min_sy.is_finite() || !max_sy.is_finite() {
        return (0, 0, 0, 0);
    }

    (
        safe_floor_i64(min_sx) - 2,
        safe_floor_i64(min_sy) - 2,
        safe_ceil_i64(max_sx) + 3,
        safe_ceil_i64(max_sy) + 3,
    )
}

/// True radial barrel (`rate_pct > 0`) / pincushion (`rate_pct < 0`) geometric warp.
fn render_fisheye_lens(
    backdrop: &SampleBackdrop,
    target_rect: PixelRect,
    lens_center_px: Point2D,
    sample_center_px: Point2D,
    rx: f64,
    ry: f64,
    inv_exp: f64,
) -> RgbaBuffer {
    let w = target_rect.width;
    let h = target_rect.height;
    let mut out = RgbaBuffer::new_transparent(w, h);

    for ly in 0..h {
        let dst_y = f64::from(target_rect.y) + f64::from(ly) + 0.5;
        for lx in 0..w {
            let dst_x = f64::from(target_rect.x) + f64::from(lx) + 0.5;
            let (sx, sy) = fisheye_map_point(
                dst_x,
                dst_y,
                lens_center_px,
                sample_center_px,
                rx,
                ry,
                inv_exp,
            );
            let px = backdrop.sample_bilinear(sx - 0.5, sy - 0.5);
            out.set_pixel(lx, ly, px);
        }
    }
    out
}

/// True optical magnification lens (`amount` in `1.0..=100.0`) sampling a zoomed sub-region.
fn render_magnify_lens(
    backdrop: &SampleBackdrop,
    target_rect: PixelRect,
    lens_center_px: Point2D,
    sample_center_px: Point2D,
    inv_mag: f64,
) -> RgbaBuffer {
    let w = target_rect.width;
    let h = target_rect.height;
    let mut out = RgbaBuffer::new_transparent(w, h);

    for ly in 0..h {
        let dst_y = f64::from(target_rect.y) + f64::from(ly) + 0.5;
        let sy = sample_center_px.y + (dst_y - lens_center_px.y) * inv_mag;
        for lx in 0..w {
            let dst_x = f64::from(target_rect.x) + f64::from(lx) + 0.5;
            let sx = sample_center_px.x + (dst_x - lens_center_px.x) * inv_mag;
            let px = backdrop.sample_bilinear(sx - 0.5, sy - 0.5);
            out.set_pixel(lx, ly, px);
        }
    }
    out
}

/// Wireframe Lens: re-renders all underlying vector objects in Wireframe mode with optional
/// override outline & fill colors, respecting custom `viewpoint` offset.
#[allow(clippy::too_many_arguments)]
fn render_wireframe_lens(
    renderer: &mut SceneRenderer,
    doc: &Document,
    page: Option<&Page>,
    underlying_ids: &[ObjectId],
    target_rect: PixelRect,
    page_to_target: Affine2D,
    shift_x: f64,
    shift_y: f64,
    override_outline: Option<&Color>,
    override_fill: Option<&Color>,
    options: &RenderOptions,
    depth: usize,
) -> RgbaBuffer {
    let Ok(mut sub_backend) = TinySkiaBackend::new(target_rect.width, target_rect.height) else {
        return RgbaBuffer::new_transparent(target_rect.width, target_rect.height);
    };
    let bg = override_fill
        .cloned()
        .unwrap_or_else(|| options.page_bg.clone());
    sub_backend.clear(bg);

    let mut wire_opts = options.clone();
    if let Some(ov_out) = override_outline {
        wire_opts.wireframe_stroke_color = ov_out.clone();
    }

    let local_xf = Affine2D::translate(
        -f64::from(target_rect.x) - shift_x,
        -f64::from(target_rect.y) - shift_y,
    )
    .compose(&page_to_target);

    for &oid in underlying_ids {
        renderer.render_object_by_id(
            doc,
            page,
            oid,
            local_xf,
            local_xf,
            ViewMode::Wireframe,
            &wire_opts,
            &mut sub_backend,
            &[],
            depth + 1,
        );
    }

    sub_backend.into_rgba()
}

/// Rasterizes the silhouette coverage of all underlying objects (strictly below the lens)
/// into a 1-channel coverage mask so `LensConfig::remove_face` hides empty page background.
fn build_underlying_coverage_mask(
    doc: &Document,
    underlying_ids: &[ObjectId],
    target_rect: PixelRect,
    page_to_target: Affine2D,
    shift_x: f64,
    shift_y: f64,
) -> Vec<u8> {
    let w = target_rect.width;
    let h = target_rect.height;
    let Ok(mut mask_backend) = TinySkiaBackend::new(w, h) else {
        return vec![255u8; (w as usize) * (h as usize)];
    };
    mask_backend.clear(Color::TRANSPARENT);
    let local_xf = Affine2D::translate(
        -f64::from(target_rect.x) - shift_x,
        -f64::from(target_rect.y) - shift_y,
    )
    .compose(&page_to_target);
    let solid = BackendPaint::solid(Color::WHITE);

    for &oid in underlying_ids {
        rasterize_object_silhouette(doc, oid, local_xf, &solid, &mut mask_backend, 0);
    }

    let buf = mask_backend.into_rgba();
    buf.rgba.as_chunks::<4>().0.iter().map(|px| px[3]).collect()
}

/// Builds a sampleable silhouette coverage backdrop over `[sx0..sx1, sy0..sy1]` for warped `remove_face`.
fn build_underlying_coverage_backdrop(
    doc: &Document,
    underlying_ids: &[ObjectId],
    page_to_target: Affine2D,
    sx0: i64,
    sy0: i64,
    sx1: i64,
    sy1: i64,
) -> SampleBackdrop {
    let bw_i64 = sx1.saturating_sub(sx0);
    let bh_i64 = sy1.saturating_sub(sy0);
    if bw_i64 <= 0
        || bh_i64 <= 0
        || bw_i64 > MAX_BACKDROP_DIM
        || bh_i64 > MAX_BACKDROP_DIM
        || validate_surface_dimensions(bw_i64 as u32, bh_i64 as u32).is_err()
    {
        return SampleBackdrop {
            origin_x: sx0 as f64,
            origin_y: sy0 as f64,
            buffer: RgbaBuffer::new_filled(1, 1, [255, 255, 255, 255]),
        };
    }
    let Ok(mut mask_backend) = TinySkiaBackend::new(bw_i64 as u32, bh_i64 as u32) else {
        return SampleBackdrop {
            origin_x: sx0 as f64,
            origin_y: sy0 as f64,
            buffer: RgbaBuffer::new_filled(1, 1, [255, 255, 255, 255]),
        };
    };
    mask_backend.clear(Color::TRANSPARENT);
    let local_xf = Affine2D::translate(-(sx0 as f64), -(sy0 as f64)).compose(&page_to_target);
    let solid = BackendPaint::solid(Color::WHITE);
    for &oid in underlying_ids {
        rasterize_object_silhouette(doc, oid, local_xf, &solid, &mut mask_backend, 0);
    }
    SampleBackdrop {
        origin_x: sx0 as f64,
        origin_y: sy0 as f64,
        buffer: mask_backend.into_rgba(),
    }
}

fn rasterize_object_silhouette(
    doc: &Document,
    id: ObjectId,
    parent_xf: Affine2D,
    paint: &BackendPaint,
    backend: &mut dyn CanvasBackend,
    depth: usize,
) {
    if depth > 64 {
        return;
    }
    let Some(obj) = doc.objects.get(&id) else {
        return;
    };
    if !obj.visible {
        return;
    }
    let sym_transforms = symmetry_effect_transforms(&obj.effects);
    for sym_xf in sym_transforms {
        let world_xf = parent_xf.compose(&sym_xf).compose(&obj.transform);
        match &obj.payload {
            NodePayload::Path(p) => {
                let deformed = apply_geometry_effects(p, &obj.effects);
                backend.fill_path(&deformed, paint, FillRule::Winding, world_xf);
            }
            NodePayload::Shape(s) => {
                let deformed = apply_geometry_effects(&s.to_bezier_path(), &obj.effects);
                backend.fill_path(&deformed, paint, FillRule::Winding, world_xf);
            }
            NodePayload::PowerClip { container, .. } => {
                let deformed = apply_geometry_effects(container, &obj.effects);
                backend.fill_path(&deformed, paint, FillRule::Winding, world_xf);
            }
            NodePayload::Table(t) => {
                let rect = BezierPath::from_rect(t.to_bounding_box());
                backend.fill_path(&rect, paint, FillRule::Winding, world_xf);
            }
            NodePayload::Raster(r) => {
                let rect = BezierPath::from_rect(r.crop_rect.unwrap_or_else(|| {
                    coree_core::Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm)
                }));
                backend.fill_path(&rect, paint, FillRule::Winding, world_xf);
            }
            NodePayload::ArtisticText(t) => {
                let rect = BezierPath::from_rect(t.estimated_bounds());
                backend.fill_path(&rect, paint, FillRule::Winding, world_xf);
            }
            NodePayload::ParagraphText(t) => {
                let rect = BezierPath::from_rect(t.bounding_box());
                backend.fill_path(&rect, paint, FillRule::Winding, world_xf);
            }
            NodePayload::Group(children) => {
                for &cid in children {
                    rasterize_object_silhouette(doc, cid, world_xf, paint, backend, depth + 1);
                }
            }
            NodePayload::SymbolInstance(sym_id) => {
                if let Some(sym) = doc.symbols.get(sym_id) {
                    for sub in &sym.objects {
                        if !sub.visible {
                            continue;
                        }
                        let sub_xf = world_xf.compose(&sub.transform);
                        match &sub.payload {
                            NodePayload::Path(p) => {
                                let deformed = apply_geometry_effects(p, &sub.effects);
                                backend.fill_path(&deformed, paint, FillRule::Winding, sub_xf);
                            }
                            NodePayload::Shape(s) => {
                                let deformed =
                                    apply_geometry_effects(&s.to_bezier_path(), &sub.effects);
                                backend.fill_path(&deformed, paint, FillRule::Winding, sub_xf);
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[inline]
fn safe_floor_i64(v: f64) -> i64 {
    if !v.is_finite() {
        0
    } else {
        v.floor().clamp(-1_000_000_000.0, 1_000_000_000.0) as i64
    }
}

#[inline]
fn safe_ceil_i64(v: f64) -> i64 {
    if !v.is_finite() {
        0
    } else {
        v.ceil().clamp(-1_000_000_000.0, 1_000_000_000.0) as i64
    }
}

#[inline]
fn estimate_scale_xf(xf: &Affine2D) -> f64 {
    let sx = xf.m[0].hypot(xf.m[1]);
    let sy = xf.m[2].hypot(xf.m[3]);
    (0.5 * (sx + sy)).max(1e-4)
}
