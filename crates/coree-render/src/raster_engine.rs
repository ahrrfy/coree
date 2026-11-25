// SPDX-License-Identifier: MIT OR Apache-2.0

//! Raster Studio Engine: brushes, eraser, clone stamp, smudge,
//! selection masks (Rectangle, Lasso, Magic Wand, Cutout background removal),
//! all 19 non-destructive [`RasterFilter`] implementations, resampling, and
//! vector-to-bitmap conversion.

use crate::backend::{validate_surface_dimensions, CanvasBackend, TinySkiaBackend};
use crate::error::{RenderError, RenderResult};
use crate::renderer::{RenderOptions, SceneRenderer};
use crate::viewport::{ViewMode, Viewport};
use coree_core::math::{sanitize_f32, sanitize_f64, MM_PER_INCH};
use coree_core::{
    Color, ColorMode, CowTile, Document, IccColorPipeline, ObjectId, PixelDepth, Point2D,
    PressureCurve, RasterAsset, RasterFilter, Rect2D, StylusSample, RASTER_TILE_DIM,
};
use image::{imageops::FilterType, ImageBuffer, Rgba, RgbaImage};
use rayon::prelude::*;
use std::collections::VecDeque;

/// Maximum brush/eraser/clone/smudge radius in pixels to prevent OOM or integer overflow.
const MAX_BRUSH_RADIUS_PX: f32 = 512.0;

/// Maximum interpolated stroke dabs emitted per call to prevent OOM on extreme coordinates.
const MAX_STROKE_DABS: usize = 16_384;

/// Validates that a [`RasterAsset`] has safe non-zero dimensions and valid CoW tile buffers.
#[inline]
fn is_valid_raster_asset(asset: &RasterAsset) -> bool {
    validate_surface_dimensions(asset.width, asset.height).is_ok() && asset.has_valid_layout()
}

/// 8-bit single-channel raster selection mask (`0` = unselected, `255` = fully selected).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterMask {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

impl RasterMask {
    /// Creates an empty (all-zero) selection mask.
    #[must_use]
    pub fn empty(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            alpha: vec![0u8; (width as usize).saturating_mul(height as usize)],
        }
    }

    /// Creates a fully selected (all-255) mask.
    #[must_use]
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            alpha: vec![255u8; (width as usize).saturating_mul(height as usize)],
        }
    }

    /// Reads mask coverage (`0..=255`) at `(x, y)`.
    #[inline]
    #[must_use]
    pub fn get(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.alpha[(y as usize) * (self.width as usize) + (x as usize)]
    }

    /// Writes mask coverage at `(x, y)`.
    #[inline]
    pub fn set(&mut self, x: u32, y: u32, value: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        self.alpha[(y as usize) * (self.width as usize) + (x as usize)] = value;
    }

    /// Counts how many pixels have coverage > 0.
    #[must_use]
    pub fn selected_pixel_count(&self) -> usize {
        self.alpha.iter().filter(|&&a| a > 0).count()
    }

    /// Rectangular selection mask in pixel coordinates.
    #[must_use]
    pub fn from_rectangle(width: u32, height: u32, rect: Rect2D) -> Self {
        let mut mask = Self::empty(width, height);
        if rect.is_empty() || width == 0 || height == 0 {
            return mask;
        }
        let x0 = (rect.min_x.floor() as i64).clamp(0, i64::from(width)) as u32;
        let x1 = (rect.max_x.ceil() as i64).clamp(0, i64::from(width)) as u32;
        let y0 = (rect.min_y.floor() as i64).clamp(0, i64::from(height)) as u32;
        let y1 = (rect.max_y.ceil() as i64).clamp(0, i64::from(height)) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                mask.set(x, y, 255);
            }
        }
        mask
    }

    /// Elliptical selection mask inscribed in `rect` in pixel coordinates.
    #[must_use]
    pub fn from_ellipse(width: u32, height: u32, rect: Rect2D) -> Self {
        let mut mask = Self::empty(width, height);
        if rect.is_empty() || width == 0 || height == 0 {
            return mask;
        }
        let cx = rect.center().x;
        let cy = rect.center().y;
        let rx = (rect.width() * 0.5).max(1e-4);
        let ry = (rect.height() * 0.5).max(1e-4);
        let x0 = (rect.min_x.floor() as i64).clamp(0, i64::from(width)) as u32;
        let x1 = (rect.max_x.ceil() as i64).clamp(0, i64::from(width)) as u32;
        let y0 = (rect.min_y.floor() as i64).clamp(0, i64::from(height)) as u32;
        let y1 = (rect.max_y.ceil() as i64).clamp(0, i64::from(height)) as u32;
        for y in y0..y1 {
            let ny = (f64::from(y) + 0.5 - cy) / ry;
            let ny2 = ny * ny;
            for x in x0..x1 {
                let nx = (f64::from(x) + 0.5 - cx) / rx;
                let r2 = nx * nx + ny2;
                if r2 <= 1.0 {
                    let edge = ((1.0 - r2.sqrt()) * rx.min(ry)).clamp(0.0, 1.0);
                    let a = (edge * 255.0).round().clamp(1.0, 255.0) as u8;
                    mask.set(x, y, a);
                }
            }
        }
        mask
    }

    /// Freehand polygon lasso selection mask using even-odd ray-casting scanlines.
    #[must_use]
    pub fn from_lasso(width: u32, height: u32, polygon: &[Point2D]) -> Self {
        let mut mask = Self::empty(width, height);
        let clean: Vec<Point2D> = polygon.iter().copied().filter(|p| p.is_finite()).collect();
        if clean.len() < 3 || width == 0 || height == 0 {
            return mask;
        }
        let n = clean.len();
        for y in 0..height {
            let py = f64::from(y) + 0.5;
            let mut intersections = Vec::new();
            for i in 0..n {
                let a = clean[i];
                let b = clean[(i + 1) % n];
                if (a.y <= py && b.y > py) || (b.y <= py && a.y > py) {
                    let denom = b.y - a.y;
                    if denom.abs() > 1e-12 {
                        let x = a.x + ((py - a.y) / denom) * (b.x - a.x);
                        if x.is_finite() {
                            intersections.push(x);
                        }
                    }
                }
            }
            intersections.sort_by(|a, b| a.total_cmp(b));
            for pair in intersections.as_chunks::<2>().0 {
                let x0 = (pair[0].floor() as i64).clamp(0, i64::from(width)) as u32;
                let x1 = (pair[1].ceil() as i64).clamp(0, i64::from(width)) as u32;
                for x in x0..x1 {
                    mask.set(x, y, 255);
                }
            }
        }
        mask
    }

    /// Magic Wand selection mask (`tolerance` in `0.0..=255.0` Euclidean RGB distance).
    ///
    /// When `contiguous` is `true`, performs 4-connected BFS flood fill from `(seed_x, seed_y)`.
    /// When `false`, selects all matching pixels across the entire image.
    #[must_use]
    pub fn from_magic_wand(
        asset: &RasterAsset,
        seed_x: u32,
        seed_y: u32,
        tolerance: f32,
        contiguous: bool,
    ) -> Self {
        if !is_valid_raster_asset(asset) || seed_x >= asset.width || seed_y >= asset.height {
            return Self::empty(0, 0);
        }
        let mut mask = Self::empty(asset.width, asset.height);
        let tol = sanitize_f32(tolerance, 16.0).max(0.0);
        let tol_sq = tol * tol * 3.0; // scale to 3-channel Euclidean squared distance
        let seed_px = asset_pixel(asset, seed_x, seed_y);

        let matches = |px: [u8; 4]| -> bool {
            let dr = f32::from(px[0]) - f32::from(seed_px[0]);
            let dg = f32::from(px[1]) - f32::from(seed_px[1]);
            let db = f32::from(px[2]) - f32::from(seed_px[2]);
            let da = f32::from(px[3]) - f32::from(seed_px[3]);
            dr * dr + dg * dg + db * db <= tol_sq && da.abs() <= tol.max(32.0)
        };

        if !contiguous {
            for y in 0..asset.height {
                for x in 0..asset.width {
                    if matches(asset_pixel(asset, x, y)) {
                        mask.set(x, y, 255);
                    }
                }
            }
            return mask;
        }

        let mut visited = vec![false; (asset.width as usize) * (asset.height as usize)];
        let mut queue = VecDeque::new();
        let start_idx = (seed_y as usize) * (asset.width as usize) + (seed_x as usize);
        visited[start_idx] = true;
        queue.push_back((seed_x, seed_y));

        while let Some((x, y)) = queue.pop_front() {
            if !matches(asset_pixel(asset, x, y)) {
                continue;
            }
            mask.set(x, y, 255);
            let neighbors = [
                x.checked_sub(1).map(|nx| (nx, y)),
                (x + 1 < asset.width).then_some((x + 1, y)),
                y.checked_sub(1).map(|ny| (x, ny)),
                (y + 1 < asset.height).then_some((x, y + 1)),
            ];
            for nb in neighbors.into_iter().flatten() {
                let nidx = (nb.1 as usize) * (asset.width as usize) + (nb.0 as usize);
                if !visited[nidx] {
                    visited[nidx] = true;
                    queue.push_back(nb);
                }
            }
        }
        mask
    }

    /// Inverts all selection mask values (`v -> 255 - v`).
    pub fn invert(&mut self) {
        for v in &mut self.alpha {
            *v = 255 - *v;
        }
    }

    /// Feathers the selection mask edges with a separable Gaussian blur of radius `radius_px`.
    pub fn feather(&mut self, radius_px: f32) {
        let r = sanitize_f32(radius_px, 0.0).max(0.0);
        if r <= 0.1 || self.width == 0 || self.height == 0 {
            return;
        }
        let kernel = build_gaussian_kernel(r);
        let half = (kernel.len() / 2) as i32;
        let w = self.width as usize;
        let h = self.height as usize;

        let mut temp = vec![0.0_f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0_f32;
                for (ki, &kw) in kernel.iter().enumerate() {
                    let sx = (x as i32 + ki as i32 - half).clamp(0, w as i32 - 1) as usize;
                    acc += f32::from(self.alpha[y * w + sx]) * kw;
                }
                temp[y * w + x] = acc;
            }
        }
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0_f32;
                for (ki, &kw) in kernel.iter().enumerate() {
                    let sy = (y as i32 + ki as i32 - half).clamp(0, h as i32 - 1) as usize;
                    acc += temp[sy * w + x] * kw;
                }
                self.alpha[y * w + x] = acc.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// Cutout Lab entry point for background removal via feathered foreground/background alpha matting.
pub fn cutout_lab(asset: &mut RasterAsset, foreground_mask: &RasterMask, feather_px: f32) {
    apply_cutout_background_removal(asset, foreground_mask, feather_px);
}

/// Applies Cutout background removal: feathers `foreground_mask`, performs local
/// Foreground/Background Alpha Matting color estimation on boundary transition bands, and
/// multiplies `asset`'s alpha channel so backgrounds become cleanly transparent while
/// preserving CoW `CowTile` sharing and 16-bit depth.
pub fn apply_cutout_background_removal(
    asset: &mut RasterAsset,
    foreground_mask: &RasterMask,
    feather_px: f32,
) {
    if !is_valid_raster_asset(asset)
        || asset.width != foreground_mask.width
        || asset.height != foreground_mask.height
        || foreground_mask.alpha.len() != (asset.width as usize) * (asset.height as usize)
    {
        return;
    }
    let mut cutout = foreground_mask.clone();
    if feather_px > 0.0 {
        cutout.feather(feather_px);
    }
    let mask = cutout;
    let w = asset.width;
    let h = asset.height;
    let snapshot = asset.clone();
    for y in 0..h {
        for x in 0..w {
            let m = f32::from(mask.get(x, y)) / 255.0;
            if m >= 0.999 {
                continue;
            }
            let Some(mut px) = snapshot.read_pixel_f32(x, y) else {
                continue;
            };
            let eff_m = if m > 0.02 && m < 0.98 {
                // Local color-estimation alpha matting in the trimap unknown band
                let mut fg_sum = [0.0_f32; 3];
                let mut fg_cnt = 0.0_f32;
                let mut bg_sum = [0.0_f32; 3];
                let mut bg_cnt = 0.0_f32;
                let search_r = (feather_px.ceil() as u32 + 2).clamp(2, 8);
                let x0 = x.saturating_sub(search_r);
                let y0 = y.saturating_sub(search_r);
                let x1 = (x + search_r + 1).min(w);
                let y1 = (y + search_r + 1).min(h);
                for ny in y0..y1 {
                    for nx in x0..x1 {
                        let nm = mask.get(nx, ny);
                        if nm >= 245 {
                            if let Some(p) = snapshot.read_pixel_f32(nx, ny) {
                                fg_sum[0] += p[0];
                                fg_sum[1] += p[1];
                                fg_sum[2] += p[2];
                                fg_cnt += 1.0;
                            }
                        } else if nm <= 10 {
                            if let Some(p) = snapshot.read_pixel_f32(nx, ny) {
                                bg_sum[0] += p[0];
                                bg_sum[1] += p[1];
                                bg_sum[2] += p[2];
                                bg_cnt += 1.0;
                            }
                        }
                    }
                }
                if fg_cnt > 0.0 && bg_cnt > 0.0 {
                    let fg = [fg_sum[0] / fg_cnt, fg_sum[1] / fg_cnt, fg_sum[2] / fg_cnt];
                    let bg = [bg_sum[0] / bg_cnt, bg_sum[1] / bg_cnt, bg_sum[2] / bg_cnt];
                    let d = [fg[0] - bg[0], fg[1] - bg[1], fg[2] - bg[2]];
                    let denom = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                    if denom > 1e-4 {
                        let num = (px[0] - bg[0]) * d[0]
                            + (px[1] - bg[1]) * d[1]
                            + (px[2] - bg[2]) * d[2];
                        let matte_a = (num / denom).clamp(0.0, 1.0);
                        0.5 * m + 0.5 * matte_a
                    } else {
                        m
                    }
                } else {
                    m
                }
            } else {
                m
            };
            px[3] = (px[3] * eff_m).clamp(0.0, 1.0);
            asset.write_pixel_f32(x, y, px);
        }
    }
}

/// Stylus dab input point carrying normalized pressure and pen tilt/twist angles (`IMP-03`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StylusPoint {
    pub pos: Point2D,
    pub pressure: f32,
    pub tilt_x_deg: f32,
    pub tilt_y_deg: f32,
    pub twist_deg: f32,
}

impl StylusPoint {
    /// Creates a neutral full-pressure circular dab point (for mouse / legacy callers).
    #[must_use]
    pub const fn uniform(pos: Point2D) -> Self {
        Self {
            pos,
            pressure: 1.0,
            tilt_x_deg: 0.0,
            tilt_y_deg: 0.0,
            twist_deg: 0.0,
        }
    }

    /// Converts a [`StylusSample`] through `curve` at raster coordinate `pos`.
    #[must_use]
    pub fn from_sample(pos: Point2D, sample: StylusSample, curve: &PressureCurve) -> Self {
        Self {
            pos,
            pressure: sample.mapped_pressure(curve) as f32,
            tilt_x_deg: sample.tilt_deg[0],
            tilt_y_deg: sample.tilt_deg[1],
            twist_deg: sample.twist_deg,
        }
    }
}

/// Paints a soft/hard brush stroke along `points` (in pixel coordinates) onto `asset`.
pub fn paint_brush_stroke(
    asset: &mut RasterAsset,
    points: &[Point2D],
    radius_px: f32,
    hardness: f32,
    color: Color,
    opacity: f32,
) {
    paint_brush_stroke_masked(asset, points, radius_px, hardness, color, opacity, None);
}

/// Paints a brush stroke optionally restricted by a [`RasterMask`].
pub fn paint_brush_stroke_masked(
    asset: &mut RasterAsset,
    points: &[Point2D],
    radius_px: f32,
    hardness: f32,
    color: Color,
    opacity: f32,
    mask: Option<&RasterMask>,
) {
    let sp: Vec<StylusPoint> = points.iter().copied().map(StylusPoint::uniform).collect();
    paint_brush_stroke_stylus(asset, &sp, radius_px, hardness, color, opacity, mask);
}

/// Paints a pressure- & tilt/twist-sensitive brush stroke optionally restricted by a [`RasterMask`].
pub fn paint_brush_stroke_stylus(
    asset: &mut RasterAsset,
    points: &[StylusPoint],
    radius_px: f32,
    hardness: f32,
    color: Color,
    opacity: f32,
    mask: Option<&RasterMask>,
) {
    let r = sanitize_f32(radius_px, 4.0).clamp(0.5, MAX_BRUSH_RADIUS_PX);
    let hard = sanitize_f32(hardness, 0.8).clamp(0.0, 1.0);
    let op = sanitize_f32(opacity, 1.0).clamp(0.0, 1.0);
    if points.is_empty() || op <= 1e-6 || !is_valid_raster_asset(asset) {
        return;
    }
    let [sr, sg, sb, sa] = color.to_rgba_f32();
    let dabs = sample_stylus_dabs(points, (r * 0.25).max(1.0));

    for dab in dabs {
        let p = sanitize_f32(dab.pressure, 1.0).clamp(0.0, 1.0);
        let dab_r = (r * (0.25 + 0.75 * p)).clamp(0.5, MAX_BRUSH_RADIUS_PX);
        let base_alpha = sa * op * (0.2 + 0.8 * p);
        let (x0, y0, x1, y1) = dab_bounds_stylus(&dab, dab_r, asset.width, asset.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = dab_elliptical_dist(f64::from(x) + 0.5, f64::from(y) + 0.5, &dab);
                let falloff = brush_hardness_falloff(d, dab_r, hard);
                if falloff <= 1e-5 {
                    continue;
                }
                let mask_f = mask.map_or(1.0, |m| f32::from(m.get(x, y)) / 255.0);
                let eff_a = (base_alpha * falloff * mask_f).clamp(0.0, 1.0);
                if eff_a <= 1e-5 {
                    continue;
                }
                blend_pixel_over(asset, x, y, sr, sg, sb, eff_a);
            }
        }
    }
}

/// Erases pixel alpha smoothly along `points` onto `asset` (CoW-tile aware).
pub fn erase_stroke(asset: &mut RasterAsset, points: &[Point2D], radius_px: f32, hardness: f32) {
    let sp: Vec<StylusPoint> = points.iter().copied().map(StylusPoint::uniform).collect();
    erase_stroke_stylus(asset, &sp, radius_px, hardness);
}

/// Erases pixel alpha along pressure- & tilt-sensitive [`StylusPoint`]s onto `asset`.
pub fn erase_stroke_stylus(
    asset: &mut RasterAsset,
    points: &[StylusPoint],
    radius_px: f32,
    hardness: f32,
) {
    erase_stroke_stylus_masked(asset, points, radius_px, hardness, None);
}

/// Erases pixel alpha along pressure- & tilt-sensitive [`StylusPoint`]s constrained natively to `mask`.
pub fn erase_stroke_stylus_masked(
    asset: &mut RasterAsset,
    points: &[StylusPoint],
    radius_px: f32,
    hardness: f32,
    mask: Option<&RasterMask>,
) {
    let r = sanitize_f32(radius_px, 4.0).clamp(0.5, MAX_BRUSH_RADIUS_PX);
    let hard = sanitize_f32(hardness, 0.8).clamp(0.0, 1.0);
    if points.is_empty() || !is_valid_raster_asset(asset) {
        return;
    }
    let dabs = sample_stylus_dabs(points, (r * 0.25).max(1.0));
    for dab in dabs {
        let p = sanitize_f32(dab.pressure, 1.0).clamp(0.0, 1.0);
        let dab_r = (r * (0.25 + 0.75 * p)).clamp(0.5, MAX_BRUSH_RADIUS_PX);
        let op_scale = 0.2 + 0.8 * p;
        let (x0, y0, x1, y1) = dab_bounds_stylus(&dab, dab_r, asset.width, asset.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let mask_f = mask.map_or(1.0, |m| f32::from(m.get(x, y)) / 255.0);
                if mask_f <= 1e-5 {
                    continue;
                }
                let d = dab_elliptical_dist(f64::from(x) + 0.5, f64::from(y) + 0.5, &dab);
                let falloff = brush_hardness_falloff(d, dab_r, hard) * op_scale * mask_f;
                if falloff <= 1e-5 {
                    continue;
                }
                if let Some(mut px) = asset.read_pixel_f32(x, y) {
                    px[3] = (px[3] * (1.0 - falloff)).clamp(0.0, 1.0);
                    asset.write_pixel_f32(x, y, px);
                }
            }
        }
    }
}

/// Clones pixels sampled relative to `src_origin` along `dst_points` onto `asset` (CoW-tile + 16-bit).
pub fn clone_stamp_stroke(
    asset: &mut RasterAsset,
    src_origin: Point2D,
    dst_points: &[Point2D],
    radius_px: f32,
    hardness: f32,
) {
    let sp: Vec<StylusPoint> = dst_points
        .iter()
        .copied()
        .map(StylusPoint::uniform)
        .collect();
    clone_stamp_stroke_stylus(asset, src_origin, &sp, radius_px, hardness);
}

/// Pressure- & tilt-sensitive Clone Stamp brush along [`StylusPoint`]s onto `asset`.
pub fn clone_stamp_stroke_stylus(
    asset: &mut RasterAsset,
    src_origin: Point2D,
    dst_points: &[StylusPoint],
    radius_px: f32,
    hardness: f32,
) {
    clone_stamp_stroke_stylus_masked(asset, src_origin, dst_points, radius_px, hardness, None);
}

/// Pressure- & tilt-sensitive Clone Stamp brush constrained natively to `mask` (CoW-tile safe).
pub fn clone_stamp_stroke_stylus_masked(
    asset: &mut RasterAsset,
    src_origin: Point2D,
    dst_points: &[StylusPoint],
    radius_px: f32,
    hardness: f32,
    mask: Option<&RasterMask>,
) {
    let r = sanitize_f32(radius_px, 4.0).clamp(0.5, MAX_BRUSH_RADIUS_PX);
    let hard = sanitize_f32(hardness, 0.8).clamp(0.0, 1.0);
    if !src_origin.is_finite() || dst_points.is_empty() || !is_valid_raster_asset(asset) {
        return;
    }
    let dabs = sample_stylus_dabs(dst_points, (r * 0.25).max(1.0));
    let Some(first_dab) = dabs.first().copied() else {
        return;
    };
    let snapshot = asset.clone();
    let offset = src_origin - first_dab.pos;
    let w = asset.width;
    let h = asset.height;

    for dab in dabs {
        let p = sanitize_f32(dab.pressure, 1.0).clamp(0.0, 1.0);
        let dab_r = (r * (0.25 + 0.75 * p)).clamp(0.5, MAX_BRUSH_RADIUS_PX);
        let op_scale = 0.2 + 0.8 * p;
        let (x0, y0, x1, y1) = dab_bounds_stylus(&dab, dab_r, w, h);
        for y in y0..y1 {
            for x in x0..x1 {
                let mask_f = mask.map_or(1.0, |m| f32::from(m.get(x, y)) / 255.0);
                if mask_f <= 1e-5 {
                    continue;
                }
                let d = dab_elliptical_dist(f64::from(x) + 0.5, f64::from(y) + 0.5, &dab);
                let falloff = brush_hardness_falloff(d, dab_r, hard) * op_scale * mask_f;
                if falloff <= 1e-5 {
                    continue;
                }
                let sx = (f64::from(x) + offset.x).round() as i64;
                let sy = (f64::from(y) + offset.y).round() as i64;
                if sx < 0 || sy < 0 || sx >= i64::from(w) || sy >= i64::from(h) {
                    continue;
                }
                if let Some(spx) = snapshot.read_pixel_f32(sx as u32, sy as u32) {
                    let sa = spx[3] * falloff;
                    if sa > 1e-5 {
                        blend_pixel_over(asset, x, y, spx[0], spx[1], spx[2], sa);
                    }
                }
            }
        }
    }
}

/// Smudges existing pixel colors along `points` carrying a paint pickup buffer (CoW-tile + 16-bit).
pub fn smudge_stroke(asset: &mut RasterAsset, points: &[Point2D], radius_px: f32, strength: f32) {
    let sp: Vec<StylusPoint> = points.iter().copied().map(StylusPoint::uniform).collect();
    smudge_stroke_stylus(asset, &sp, radius_px, strength);
}

/// Pressure-sensitive Smudge brush along [`StylusPoint`]s onto `asset`.
pub fn smudge_stroke_stylus(
    asset: &mut RasterAsset,
    points: &[StylusPoint],
    radius_px: f32,
    strength: f32,
) {
    smudge_stroke_stylus_masked(asset, points, radius_px, strength, None);
}

/// Pressure-sensitive Smudge brush constrained natively to `mask` (samples & writes only inside mask).
pub fn smudge_stroke_stylus_masked(
    asset: &mut RasterAsset,
    points: &[StylusPoint],
    radius_px: f32,
    strength: f32,
    mask: Option<&RasterMask>,
) {
    let r = sanitize_f32(radius_px, 4.0).clamp(0.5, 128.0);
    let str_f = sanitize_f32(strength, 0.5).clamp(0.0, 1.0);
    if points.len() < 2 || str_f <= 1e-5 || !is_valid_raster_asset(asset) {
        return;
    }
    let dabs = sample_stylus_dabs(points, (r * 0.25).max(1.0));
    if dabs.len() < 2 {
        return;
    }
    let dab_aspect_max = dabs
        .iter()
        .map(|dab| {
            let tx = sanitize_f32(dab.tilt_x_deg, 0.0).clamp(-85.0, 85.0);
            let ty = sanitize_f32(dab.tilt_y_deg, 0.0).clamp(-85.0, 85.0);
            let tilt_mag = tx.hypot(ty).clamp(0.0, 85.0);
            1.0 + tilt_mag / 60.0
        })
        .fold(1.0_f32, f32::max);
    let rad_i = (r * dab_aspect_max).ceil() as i32;
    let rad_i64 = i64::from(rad_i);
    let side = (rad_i * 2 + 1) as usize;
    let mut pickup = vec![[0.0_f32; 4]; side * side];
    let mut pickup_valid = vec![false; side * side];
    let w_i64 = i64::from(asset.width);
    let h_i64 = i64::from(asset.height);

    let seed = dabs[0].pos;
    let seed_x = seed.x.round().clamp(-1e9, 1e9) as i64;
    let seed_y = seed.y.round().clamp(-1e9, 1e9) as i64;
    for dy in -rad_i..=rad_i {
        for dx in -rad_i..=rad_i {
            let sx = (seed_x + i64::from(dx)).clamp(0, w_i64 - 1) as u32;
            let sy = (seed_y + i64::from(dy)).clamp(0, h_i64 - 1) as u32;
            let k = ((dy + rad_i) as usize) * side + ((dx + rad_i) as usize);
            if mask.is_some_and(|m| m.get(sx, sy) == 0) {
                continue;
            }
            let px = asset.read_pixel_f32(sx, sy).unwrap_or([0.0; 4]);
            pickup[k] = px;
            pickup_valid[k] = true;
        }
    }

    for dab in dabs.iter().skip(1) {
        let p = sanitize_f32(dab.pressure, 1.0).clamp(0.0, 1.0);
        let dab_r = (r * (0.25 + 0.75 * p)).clamp(0.5, 128.0);
        let eff_str = str_f * (0.2 + 0.8 * p);
        let cx = dab.pos.x.round().clamp(-1e9, 1e9) as i64;
        let cy = dab.pos.y.round().clamp(-1e9, 1e9) as i64;
        let (x0, y0, x1, y1) = dab_bounds_stylus(dab, dab_r, asset.width, asset.height);
        for uy in y0..y1 {
            let dy = (i64::from(uy) - cy).clamp(-rad_i64, rad_i64) as i32;
            for ux in x0..x1 {
                let dx = (i64::from(ux) - cx).clamp(-rad_i64, rad_i64) as i32;
                let mask_f = mask.map_or(1.0, |m| f32::from(m.get(ux, uy)) / 255.0);
                if mask_f <= 1e-5 {
                    continue;
                }
                let dist = dab_elliptical_dist(f64::from(ux) + 0.5, f64::from(uy) + 0.5, dab);
                let w = brush_hardness_falloff(dist, dab_r, 0.25) * eff_str * mask_f;
                if w <= 1e-5 {
                    continue;
                }
                let k = ((dy + rad_i) as usize) * side + ((dx + rad_i) as usize);
                let cur = asset.read_pixel_f32(ux, uy).unwrap_or([0.0; 4]);
                if !pickup_valid[k] {
                    pickup[k] = cur;
                    pickup_valid[k] = true;
                    continue;
                }
                let mut next = [0.0_f32; 4];
                for c in 0..4 {
                    next[c] = (cur[c] * (1.0 - w) + pickup[k][c] * w).clamp(0.0, 1.0);
                    pickup[k][c] = pickup[k][c] * eff_str + cur[c] * (1.0 - eff_str);
                }
                asset.write_pixel_f32(ux, uy, next);
            }
        }
    }
}

/// Tonal luminance band targeted by [`apply_dodge_burn_stroke`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ToneRange {
    Shadows,
    #[default]
    Midtones,
    Highlights,
}

/// Photographic exposure operation for [`apply_dodge_burn_stroke`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DodgeBurnMode {
    /// Lightens pixels within the targeted [`ToneRange`].
    #[default]
    Dodge,
    /// Darkens pixels within the targeted [`ToneRange`].
    Burn,
}

/// Seamless Poisson Healing Brush: solves a discrete 2D Poisson PDE (`poisson` multigrid /
/// red-black SOR on RGB gradient fields) over the stroke mask so source texture blends seamlessly
/// onto the destination boundary tones without visible seams (CoW-tile + 16-bit safe).
pub fn apply_healing_stroke(
    asset: &mut RasterAsset,
    src_origin: Point2D,
    dst_points: &[Point2D],
    radius_px: f32,
    hardness: f32,
) {
    let sp: Vec<StylusPoint> = dst_points
        .iter()
        .copied()
        .map(StylusPoint::uniform)
        .collect();
    apply_healing_stroke_stylus_masked(asset, src_origin, &sp, radius_px, hardness, None);
}

/// Seamless Poisson Healing Brush along pressure- & tilt-sensitive [`StylusPoint`]s.
pub fn apply_healing_stroke_stylus(
    asset: &mut RasterAsset,
    src_origin: Point2D,
    dst_points: &[StylusPoint],
    radius_px: f32,
    hardness: f32,
) {
    apply_healing_stroke_stylus_masked(asset, src_origin, dst_points, radius_px, hardness, None);
}

/// Seamless Poisson Healing Brush constrained natively to `mask` (treating masked-out pixels
/// as fixed Dirichlet boundary values and using V-cycle multigrid initialization for `r >= 10`).
pub fn apply_healing_stroke_stylus_masked(
    asset: &mut RasterAsset,
    src_origin: Point2D,
    dst_points: &[StylusPoint],
    radius_px: f32,
    hardness: f32,
    mask: Option<&RasterMask>,
) {
    let r = sanitize_f32(radius_px, 8.0).clamp(1.0, MAX_BRUSH_RADIUS_PX);
    let hard = sanitize_f32(hardness, 0.5).clamp(0.0, 1.0);
    if !src_origin.is_finite() || dst_points.is_empty() || !is_valid_raster_asset(asset) {
        return;
    }
    let dabs = sample_stylus_dabs(dst_points, (r * 0.3).max(1.0));
    let Some(first_dab) = dabs.first().copied() else {
        return;
    };
    let snapshot = asset.clone();
    let offset = src_origin - first_dab.pos;
    let w = asset.width;
    let h = asset.height;
    let w_i64 = i64::from(w);
    let h_i64 = i64::from(h);

    let mut bx0 = w;
    let mut by0 = h;
    let mut bx1 = 0u32;
    let mut by1 = 0u32;
    for dab in &dabs {
        let p = sanitize_f32(dab.pressure, 1.0).clamp(0.0, 1.0);
        let dab_r = (r * (0.25 + 0.75 * p)).clamp(1.0, MAX_BRUSH_RADIUS_PX);
        let (x0, y0, x1, y1) = dab_bounds_stylus(dab, dab_r + 2.0, w, h);
        if x0 < x1 && y0 < y1 {
            bx0 = bx0.min(x0);
            by0 = by0.min(y0);
            bx1 = bx1.max(x1);
            by1 = by1.max(y1);
        }
    }
    if bx0 >= bx1 || by0 >= by1 {
        return;
    }

    let bw = (bx1 - bx0) as usize;
    let bh = (by1 - by0) as usize;
    let mut falloff_mask = vec![0.0_f32; bw * bh];
    for dab in &dabs {
        let p = sanitize_f32(dab.pressure, 1.0).clamp(0.0, 1.0);
        let dab_r = (r * (0.25 + 0.75 * p)).clamp(1.0, MAX_BRUSH_RADIUS_PX);
        let (x0, y0, x1, y1) = dab_bounds_stylus(dab, dab_r, w, h);
        for y in y0..y1 {
            for x in x0..x1 {
                let mask_f = mask.map_or(1.0, |m| f32::from(m.get(x, y)) / 255.0);
                if mask_f <= 1e-5 {
                    continue;
                }
                let d = dab_elliptical_dist(f64::from(x) + 0.5, f64::from(y) + 0.5, dab);
                let f = brush_hardness_falloff(d, dab_r, hard) * mask_f;
                if f > 1e-5 {
                    let k = ((y - by0) as usize) * bw + ((x - bx0) as usize);
                    if f > falloff_mask[k] {
                        falloff_mask[k] = f;
                    }
                }
            }
        }
    }

    let mut poisson = vec![[0.0_f32; 3]; bw * bh];
    let mut src_px_grid = vec![[0.0_f32; 3]; bw * bh];
    let mut src_alpha_grid = vec![0.0_f32; bw * bh];
    for ly in 0..bh {
        let y = by0 + ly as u32;
        for lx in 0..bw {
            let x = bx0 + lx as u32;
            let k = ly * bw + lx;
            if let Some(dpx) = snapshot.read_pixel_f32(x, y) {
                poisson[k] = [dpx[0], dpx[1], dpx[2]];
            }
            let sx = (f64::from(x) + offset.x).round() as i64;
            let sy = (f64::from(y) + offset.y).round() as i64;
            if sx >= 0 && sy >= 0 && sx < w_i64 && sy < h_i64 {
                if let Some(spx) = snapshot.read_pixel_f32(sx as u32, sy as u32) {
                    src_px_grid[k] = [spx[0], spx[1], spx[2]];
                    src_alpha_grid[k] = spx[3];
                }
            } else {
                src_px_grid[k] = poisson[k];
            }
        }
    }

    let mut rhs = vec![[0.0_f32; 3]; bw * bh];
    let mut active = vec![false; bw * bh];
    for ly in 1..(bh.saturating_sub(1)) {
        for lx in 1..(bw.saturating_sub(1)) {
            let k = ly * bw + lx;
            if falloff_mask[k] <= 1e-5 || src_alpha_grid[k] <= 1e-4 {
                continue;
            }
            active[k] = true;
            let kl = k - 1;
            let kr = k + 1;
            let ku = k - bw;
            let kd = k + bw;
            for ch in 0..3 {
                rhs[k][ch] = 4.0 * src_px_grid[k][ch]
                    - src_px_grid[kl][ch]
                    - src_px_grid[kr][ch]
                    - src_px_grid[ku][ch]
                    - src_px_grid[kd][ch];
            }
        }
    }

    solve_poisson_vcycle(&mut poisson, &rhs, &active, bw, bh, 0);

    for ly in 0..bh {
        let y = by0 + ly as u32;
        for lx in 0..bw {
            let x = bx0 + lx as u32;
            let k = ly * bw + lx;
            let eff_a = (falloff_mask[k] * src_alpha_grid[k]).clamp(0.0, 1.0);
            if eff_a > 1e-5 {
                blend_pixel_over(
                    asset,
                    x,
                    y,
                    poisson[k][0].clamp(0.0, 1.0),
                    poisson[k][1].clamp(0.0, 1.0),
                    poisson[k][2].clamp(0.0, 1.0),
                    eff_a,
                );
            }
        }
    }
}

fn solve_poisson_vcycle(
    u: &mut [[f32; 3]],
    rhs: &[[f32; 3]],
    active: &[bool],
    bw: usize,
    bh: usize,
    depth: usize,
) {
    let sor_sweeps = if depth == 0 && bw.max(bh) < 20 {
        24
    } else {
        12
    };
    let omega = 1.72_f32;
    for _ in 0..sor_sweeps {
        for parity in 0..2 {
            for ly in 1..(bh.saturating_sub(1)) {
                for lx in 1..(bw.saturating_sub(1)) {
                    if (lx + ly) % 2 != parity {
                        continue;
                    }
                    let k = ly * bw + lx;
                    if !active[k] {
                        continue;
                    }
                    let kl = k - 1;
                    let kr = k + 1;
                    let ku = k - bw;
                    let kd = k + bw;
                    for ch in 0..3 {
                        let gs =
                            0.25 * (u[kl][ch] + u[kr][ch] + u[ku][ch] + u[kd][ch] + rhs[k][ch]);
                        u[k][ch] += omega * (gs - u[k][ch]);
                    }
                }
            }
        }
    }

    if bw.max(bh) >= 20 && bw >= 6 && bh >= 6 && depth < 4 {
        let cw = bw.div_ceil(2);
        let ch_dim = bh.div_ceil(2);
        let mut cu = vec![[0.0_f32; 3]; cw * ch_dim];
        let mut crhs = vec![[0.0_f32; 3]; cw * ch_dim];
        let mut cactive = vec![false; cw * ch_dim];

        for cy in 1..(ch_dim.saturating_sub(1)) {
            let fy = (cy * 2).min(bh - 2);
            for cx in 1..(cw.saturating_sub(1)) {
                let fx = (cx * 2).min(bw - 2);
                let fk = fy * bw + fx;
                if !active[fk] {
                    continue;
                }
                let ck = cy * cw + cx;
                cactive[ck] = true;
                for ch in 0..3 {
                    let res = rhs[fk][ch]
                        - (4.0 * u[fk][ch]
                            - u[fk - 1][ch]
                            - u[fk + 1][ch]
                            - u[fk - bw][ch]
                            - u[fk + bw][ch]);
                    crhs[ck][ch] = 4.0 * res;
                }
            }
        }

        solve_poisson_vcycle(&mut cu, &crhs, &cactive, cw, ch_dim, depth + 1);

        for ly in 1..(bh.saturating_sub(1)) {
            let cy0 = (ly / 2).min(ch_dim - 1);
            let cy1 = (cy0 + 1).min(ch_dim - 1);
            let fy_w = if ly % 2 == 1 { 0.5_f32 } else { 0.0_f32 };
            for lx in 1..(bw.saturating_sub(1)) {
                let fk = ly * bw + lx;
                if !active[fk] {
                    continue;
                }
                let cx0 = (lx / 2).min(cw - 1);
                let cx1 = (cx0 + 1).min(cw - 1);
                let fx_w = if lx % 2 == 1 { 0.5_f32 } else { 0.0_f32 };
                for ch in 0..3 {
                    let top = cu[cy0 * cw + cx0][ch] * (1.0 - fx_w) + cu[cy0 * cw + cx1][ch] * fx_w;
                    let bot = cu[cy1 * cw + cx0][ch] * (1.0 - fx_w) + cu[cy1 * cw + cx1][ch] * fx_w;
                    u[fk][ch] += top * (1.0 - fy_w) + bot * fy_w;
                }
            }
        }

        for _ in 0..12 {
            for parity in 0..2 {
                for ly in 1..(bh.saturating_sub(1)) {
                    for lx in 1..(bw.saturating_sub(1)) {
                        if (lx + ly) % 2 != parity {
                            continue;
                        }
                        let k = ly * bw + lx;
                        if !active[k] {
                            continue;
                        }
                        let kl = k - 1;
                        let kr = k + 1;
                        let ku = k - bw;
                        let kd = k + bw;
                        for ch in 0..3 {
                            let gs =
                                0.25 * (u[kl][ch] + u[kr][ch] + u[ku][ch] + u[kd][ch] + rhs[k][ch]);
                            u[k][ch] += omega * (gs - u[k][ch]);
                        }
                    }
                }
            }
        }
    }

    if depth == 0 {
        for px in u.iter_mut() {
            px[0] = px[0].clamp(0.0, 1.0);
            px[1] = px[1].clamp(0.0, 1.0);
            px[2] = px[2].clamp(0.0, 1.0);
        }
    }
}

/// Pressure- & tilt-sensitive Dodge or Burn stroke along [`StylusPoint`]s.
pub fn apply_dodge_burn_stroke_stylus(
    asset: &mut RasterAsset,
    points: &[StylusPoint],
    radius_px: f32,
    hardness: f32,
    mode: DodgeBurnMode,
    range: ToneRange,
    exposure: f32,
) {
    apply_dodge_burn_stroke_stylus_masked(
        asset, points, radius_px, hardness, mode, range, exposure, None,
    );
}

/// Pressure- & tilt-sensitive Dodge or Burn stroke constrained natively to `mask`.
#[allow(clippy::too_many_arguments)]
pub fn apply_dodge_burn_stroke_stylus_masked(
    asset: &mut RasterAsset,
    points: &[StylusPoint],
    radius_px: f32,
    hardness: f32,
    mode: DodgeBurnMode,
    range: ToneRange,
    exposure: f32,
    mask: Option<&RasterMask>,
) {
    let r = sanitize_f32(radius_px, 8.0).clamp(0.5, MAX_BRUSH_RADIUS_PX);
    let hard = sanitize_f32(hardness, 0.4).clamp(0.0, 1.0);
    let exp = sanitize_f32(exposure, 0.35).clamp(0.0, 1.0);
    if points.is_empty() || exp <= 1e-5 || !is_valid_raster_asset(asset) {
        return;
    }
    let dabs = sample_stylus_dabs(points, (r * 0.25).max(1.0));
    if dabs.is_empty() {
        return;
    }

    let mut bx0 = asset.width;
    let mut by0 = asset.height;
    let mut bx1 = 0u32;
    let mut by1 = 0u32;
    for dab in &dabs {
        let p = sanitize_f32(dab.pressure, 1.0).clamp(0.0, 1.0);
        let dab_r = (r * (0.25 + 0.75 * p)).clamp(0.5, MAX_BRUSH_RADIUS_PX);
        let (x0, y0, x1, y1) = dab_bounds_stylus(dab, dab_r, asset.width, asset.height);
        if x0 < x1 && y0 < y1 {
            bx0 = bx0.min(x0);
            by0 = by0.min(y0);
            bx1 = bx1.max(x1);
            by1 = by1.max(y1);
        }
    }
    if bx0 >= bx1 || by0 >= by1 {
        return;
    }

    let bw = (bx1 - bx0) as usize;
    let bh = (by1 - by0) as usize;
    let mut coverage = vec![0.0_f32; bw.saturating_mul(bh)];

    for dab in dabs {
        let p = sanitize_f32(dab.pressure, 1.0).clamp(0.0, 1.0);
        let dab_r = (r * (0.25 + 0.75 * p)).clamp(0.5, MAX_BRUSH_RADIUS_PX);
        let op_scale = 0.2 + 0.8 * p;
        let (x0, y0, x1, y1) = dab_bounds_stylus(&dab, dab_r, asset.width, asset.height);
        for y in y0..y1 {
            let row_off = ((y - by0) as usize) * bw;
            for x in x0..x1 {
                let mask_f = mask.map_or(1.0, |m| f32::from(m.get(x, y)) / 255.0);
                if mask_f <= 1e-5 {
                    continue;
                }
                let d = dab_elliptical_dist(f64::from(x) + 0.5, f64::from(y) + 0.5, &dab);
                let falloff = brush_hardness_falloff(d, dab_r, hard) * op_scale * mask_f;
                if falloff > 1e-5 {
                    let slot = &mut coverage[row_off + ((x - bx0) as usize)];
                    if falloff > *slot {
                        *slot = falloff;
                    }
                }
            }
        }
    }

    for ly in 0..bh {
        let y = by0 + ly as u32;
        let cov_row = ly * bw;
        for lx in 0..bw {
            let cov = coverage[cov_row + lx];
            if cov <= 1e-5 {
                continue;
            }
            let x = bx0 + lx as u32;
            let Some(mut px) = asset.read_pixel_f32(x, y) else {
                continue;
            };
            if px[3] <= 1e-5 {
                continue;
            }
            let luma = (0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2]).clamp(0.0, 1.0);
            let tone_weight = match range {
                ToneRange::Shadows => (1.0 - luma) * (1.0 - luma),
                ToneRange::Midtones => 4.0 * luma * (1.0 - luma),
                ToneRange::Highlights => luma * luma,
            };
            let strength = (exp * cov * tone_weight * 0.70).clamp(0.0, 0.95);
            if strength <= 1e-5 {
                continue;
            }
            let adjust = |ch: f32| -> f32 {
                match mode {
                    DodgeBurnMode::Dodge => ch + (1.0 - ch) * strength,
                    DodgeBurnMode::Burn => ch * (1.0 - strength),
                }
                .clamp(0.0, 1.0)
            };
            px[0] = adjust(px[0]);
            px[1] = adjust(px[1]);
            px[2] = adjust(px[2]);
            asset.write_pixel_f32(x, y, px);
        }
    }
}

/// Applies photographic Dodge (lighten) or Burn (darken) exposure adjustment along `points`,
/// smoothly weighted to the chosen [`ToneRange`] (`Shadows`, `Midtones`, or `Highlights`).
pub fn apply_dodge_burn_stroke(
    asset: &mut RasterAsset,
    points: &[Point2D],
    radius_px: f32,
    hardness: f32,
    mode: DodgeBurnMode,
    range: ToneRange,
    exposure: f32,
) {
    let sp: Vec<StylusPoint> = points.iter().copied().map(StylusPoint::uniform).collect();
    apply_dodge_burn_stroke_stylus_masked(
        asset, &sp, radius_px, hardness, mode, range, exposure, None,
    );
}

/// Contiguous raster paint-bucket flood fill using an iterative scanline-span BFS queue
/// (zero recursion, safe on arbitrarily large images) with per-channel tolerance `0..=255`.
///
/// Returns the exact number of pixels modified.
pub fn raster_flood_fill(
    asset: &mut RasterAsset,
    seed_x: u32,
    seed_y: u32,
    fill_color: Color,
    tolerance: u8,
) -> usize {
    raster_flood_fill_masked(asset, seed_x, seed_y, fill_color, tolerance, None)
}

/// Contiguous raster paint-bucket flood fill constrained natively to `mask` so traversal
/// never leaks across mask boundaries and never dirties CoW tiles outside the mask.
pub fn raster_flood_fill_masked(
    asset: &mut RasterAsset,
    seed_x: u32,
    seed_y: u32,
    fill_color: Color,
    tolerance: u8,
    mask: Option<&RasterMask>,
) -> usize {
    if !is_valid_raster_asset(asset) || seed_x >= asset.width || seed_y >= asset.height {
        return 0;
    }
    if mask.is_some_and(|m| m.get(seed_x, seed_y) == 0) {
        return 0;
    }
    let w = asset.width as usize;
    let h = asset.height as usize;
    let rgba8 = asset.to_rgba8_vec();
    let seed_px = asset_pixel_from_flat(&rgba8, asset.width, seed_x, seed_y);
    let fill_px = fill_color.to_rgba8();
    if tolerance == 0 && seed_px == fill_px {
        return 0;
    }

    let tol = i32::from(tolerance);
    let matches_seed = |bytes: &[u8], x: usize, y: usize| -> bool {
        if mask.is_some_and(|m| m.get(x as u32, y as u32) == 0) {
            return false;
        }
        let idx = (y * w + x) * 4;
        (i32::from(bytes[idx]) - i32::from(seed_px[0])).abs() <= tol
            && (i32::from(bytes[idx + 1]) - i32::from(seed_px[1])).abs() <= tol
            && (i32::from(bytes[idx + 2]) - i32::from(seed_px[2])).abs() <= tol
            && (i32::from(bytes[idx + 3]) - i32::from(seed_px[3])).abs() <= tol
    };

    let mut visited = vec![false; w * h];
    let mut queue: VecDeque<(usize, usize)> = VecDeque::new();
    queue.push_back((seed_x as usize, seed_y as usize));
    let mut filled_count = 0usize;

    let [fr, fg, fb, fa] = fill_color.to_rgba_f32();
    let direct_write = fill_px[3] == 255 || fill_px[3] == 0;

    while let Some((sx, sy)) = queue.pop_front() {
        let row_base = sy * w;
        if visited[row_base + sx] || !matches_seed(&rgba8, sx, sy) {
            continue;
        }

        let mut lx = sx;
        while lx > 0 && !visited[row_base + lx - 1] && matches_seed(&rgba8, lx - 1, sy) {
            lx -= 1;
        }
        let mut rx = sx;
        while rx + 1 < w && !visited[row_base + rx + 1] && matches_seed(&rgba8, rx + 1, sy) {
            rx += 1;
        }

        let mut above_in_run = false;
        let mut below_in_run = false;
        for x in lx..=rx {
            visited[row_base + x] = true;
            let mask_f = mask.map_or(1.0, |m| f32::from(m.get(x as u32, sy as u32)) / 255.0);
            if direct_write && mask_f >= 0.999 {
                asset.write_pixel_f32(x as u32, sy as u32, [fr, fg, fb, fa]);
            } else {
                blend_pixel_over(asset, x as u32, sy as u32, fr, fg, fb, fa * mask_f);
            }
            filled_count += 1;

            if sy > 0 {
                let ay = sy - 1;
                let a_match = !visited[ay * w + x] && matches_seed(&rgba8, x, ay);
                if a_match && !above_in_run {
                    queue.push_back((x, ay));
                    above_in_run = true;
                } else if !a_match {
                    above_in_run = false;
                }
            }
            if sy + 1 < h {
                let by = sy + 1;
                let b_match = !visited[by * w + x] && matches_seed(&rgba8, x, by);
                if b_match && !below_in_run {
                    queue.push_back((x, by));
                    below_in_run = true;
                } else if !b_match {
                    below_in_run = false;
                }
            }
        }
    }

    filled_count
}

/// Applies a stack of non-destructive [`RasterFilter`]s sequentially, returning a new [`RasterAsset`].
#[must_use]
pub fn apply_raster_filters(asset: &RasterAsset, filters: &[RasterFilter]) -> RasterAsset {
    let mut current = asset.clone();
    for filter in filters {
        current = apply_raster_filter(&current, filter);
    }
    current
}

/// Applies a single [`RasterFilter`] (all 20 Raster Studio adjustments & convolutions implemented)
/// in either 8-bit or 16-bit precision across Copy-on-Write [`CowTile`] buffers.
#[must_use]
pub fn apply_raster_filter(asset: &RasterAsset, filter: &RasterFilter) -> RasterAsset {
    let mut out = asset.clone();
    if !is_valid_raster_asset(asset) {
        return out;
    }

    match filter {
        RasterFilter::BrightnessContrast {
            brightness,
            contrast,
        } => {
            let b = sanitize_f32(*brightness, 0.0).clamp(-1.0, 1.0);
            let c = sanitize_f32(*contrast, 0.0).clamp(-1.0, 1.0);
            let factor = if c >= 0.0 {
                1.0 / (1.0 - c.min(0.99))
            } else {
                1.0 + c
            };
            map_tiles_parallel_f32(&mut out, |px| {
                for ch in &mut px[0..3] {
                    *ch = ((*ch - 0.5) * factor + 0.5 + b).clamp(0.0, 1.0);
                }
            });
        }
        RasterFilter::ToneCurve { points } => {
            let knots = prepare_tone_curve_knots(points);
            match out.pixel_depth {
                PixelDepth::Bit8 => {
                    let lut = build_tone_curve_lut_from_knots(&knots);
                    out.tiles.par_iter_mut().for_each(|tile| {
                        let data = tile.make_mut();
                        for px in data.as_chunks_mut::<4>().0 {
                            px[0] = lut[px[0] as usize];
                            px[1] = lut[px[1] as usize];
                            px[2] = lut[px[2] as usize];
                        }
                    });
                }
                PixelDepth::Bit16 => {
                    let lut = build_tone_curve_lut16_from_knots(&knots);
                    out.tiles.par_iter_mut().for_each(|tile| {
                        let data = tile.make_mut();
                        for px in data.as_chunks_mut::<8>().0 {
                            for c in 0..3 {
                                let v = u16::from_le_bytes([px[c * 2], px[c * 2 + 1]]);
                                let mapped = lut[v as usize].to_le_bytes();
                                px[c * 2] = mapped[0];
                                px[c * 2 + 1] = mapped[1];
                            }
                        }
                    });
                }
            }
        }
        RasterFilter::Levels {
            in_black,
            in_gamma,
            in_white,
            out_black,
            out_white,
        } => {
            let ib = sanitize_f32(*in_black, 0.0).clamp(0.0, 0.99);
            let iw = sanitize_f32(*in_white, 1.0).clamp(ib + 0.01, 1.0);
            let gamma = sanitize_f32(*in_gamma, 1.0).clamp(0.05, 10.0);
            let inv_gamma = 1.0 / gamma;
            let ob = sanitize_f32(*out_black, 0.0).clamp(0.0, 1.0);
            let ow = sanitize_f32(*out_white, 1.0).clamp(0.0, 1.0);
            let span = (iw - ib).max(1e-5);
            map_tiles_parallel_f32(&mut out, |px| {
                for ch in &mut px[0..3] {
                    let v = ((*ch - ib) / span).clamp(0.0, 1.0);
                    let g = v.powf(inv_gamma);
                    *ch = (ob + g * (ow - ob)).clamp(0.0, 1.0);
                }
            });
        }
        RasterFilter::Hsl {
            hue_deg,
            saturation,
            lightness,
        } => {
            let dh = sanitize_f32(*hue_deg, 0.0);
            let ds = sanitize_f32(*saturation, 0.0).clamp(-1.0, 1.0);
            let dl = sanitize_f32(*lightness, 0.0).clamp(-1.0, 1.0);
            map_tiles_parallel_f32(&mut out, |px| {
                let col = Color::rgba(px[0], px[1], px[2], px[3]);
                let [h, s, l] = col.to_hsl();
                let nh = (h + dh).rem_euclid(360.0);
                let ns = if ds >= 0.0 {
                    s + (1.0 - s) * ds
                } else {
                    s * (1.0 + ds)
                }
                .clamp(0.0, 1.0);
                let nl = if dl >= 0.0 {
                    l + (1.0 - l) * dl
                } else {
                    l * (1.0 + dl)
                }
                .clamp(0.0, 1.0);
                let [r, g, b, _] = Color::hsl(nh, ns, nl).to_rgba_f32();
                px[0] = r;
                px[1] = g;
                px[2] = b;
            });
        }
        RasterFilter::ColorBalance {
            cyan_red,
            magenta_green,
            yellow_blue,
        } => {
            let cr = sanitize_f32(*cyan_red, 0.0).clamp(-1.0, 1.0);
            let mg = sanitize_f32(*magenta_green, 0.0).clamp(-1.0, 1.0);
            let yb = sanitize_f32(*yellow_blue, 0.0).clamp(-1.0, 1.0);
            map_tiles_parallel_f32(&mut out, |px| {
                px[0] = (px[0] + cr * 0.5).clamp(0.0, 1.0);
                px[1] = (px[1] + mg * 0.5).clamp(0.0, 1.0);
                px[2] = (px[2] + yb * 0.5).clamp(0.0, 1.0);
            });
        }
        RasterFilter::ReplaceColor {
            target,
            replacement,
            tolerance,
        } => {
            let [tr, tg, tb, _] = target.to_rgba_f32();
            let [rr, rg, rb, _] = replacement.to_rgba_f32();
            let tol = sanitize_f32(*tolerance, 0.15).clamp(0.001, 2.0);
            map_tiles_parallel_f32(&mut out, |px| {
                let dist =
                    ((px[0] - tr).powi(2) + (px[1] - tg).powi(2) + (px[2] - tb).powi(2)).sqrt();
                if dist <= tol {
                    let blend = (1.0 - dist / tol).clamp(0.0, 1.0);
                    px[0] = (px[0] + (rr - px[0]) * blend).clamp(0.0, 1.0);
                    px[1] = (px[1] + (rg - px[1]) * blend).clamp(0.0, 1.0);
                    px[2] = (px[2] + (rb - px[2]) * blend).clamp(0.0, 1.0);
                }
            });
        }
        RasterFilter::Gamma { gamma } => {
            let g = sanitize_f32(*gamma, 1.0).clamp(0.05, 10.0);
            let inv_g = 1.0 / g;
            match out.pixel_depth {
                PixelDepth::Bit8 => {
                    let mut lut = [0u8; 256];
                    for (i, slot) in lut.iter_mut().enumerate() {
                        *slot = ((i as f32 / 255.0).powf(inv_g) * 255.0)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                    }
                    out.tiles.par_iter_mut().for_each(|tile| {
                        let data = tile.make_mut();
                        for px in data.as_chunks_mut::<4>().0 {
                            px[0] = lut[px[0] as usize];
                            px[1] = lut[px[1] as usize];
                            px[2] = lut[px[2] as usize];
                        }
                    });
                }
                PixelDepth::Bit16 => {
                    map_tiles_parallel_f32(&mut out, |px| {
                        px[0] = px[0].clamp(0.0, 1.0).powf(inv_g).clamp(0.0, 1.0);
                        px[1] = px[1].clamp(0.0, 1.0).powf(inv_g).clamp(0.0, 1.0);
                        px[2] = px[2].clamp(0.0, 1.0).powf(inv_g).clamp(0.0, 1.0);
                    });
                }
            }
        }
        RasterFilter::Desaturate => {
            map_tiles_parallel_f32(&mut out, |px| {
                let l = (0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2]).clamp(0.0, 1.0);
                px[0] = l;
                px[1] = l;
                px[2] = l;
            });
        }
        RasterFilter::Invert => {
            map_tiles_parallel_f32(&mut out, |px| {
                px[0] = (1.0 - px[0]).clamp(0.0, 1.0);
                px[1] = (1.0 - px[1]).clamp(0.0, 1.0);
                px[2] = (1.0 - px[2]).clamp(0.0, 1.0);
            });
        }
        RasterFilter::Posterize { levels } => {
            let lv = f32::from((*levels).max(2) - 1);
            map_tiles_parallel_f32(&mut out, |px| {
                for ch in &mut px[0..3] {
                    *ch = ((*ch * lv).round() / lv).clamp(0.0, 1.0);
                }
            });
        }
        RasterFilter::Threshold { cutoff } => {
            let cut = f32::from(*cutoff) / 255.0;
            map_tiles_parallel_f32(&mut out, |px| {
                let l = 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2];
                let v = if l >= cut { 1.0 } else { 0.0 };
                px[0] = v;
                px[1] = v;
                px[2] = v;
            });
        }
        RasterFilter::ChannelMixer {
            red,
            green,
            blue,
            monochrome,
        } => {
            let r_w = [
                sanitize_f32(red[0], 1.0).clamp(-2.0, 2.0),
                sanitize_f32(red[1], 0.0).clamp(-2.0, 2.0),
                sanitize_f32(red[2], 0.0).clamp(-2.0, 2.0),
            ];
            let g_w = [
                sanitize_f32(green[0], 0.0).clamp(-2.0, 2.0),
                sanitize_f32(green[1], 1.0).clamp(-2.0, 2.0),
                sanitize_f32(green[2], 0.0).clamp(-2.0, 2.0),
            ];
            let b_w = [
                sanitize_f32(blue[0], 0.0).clamp(-2.0, 2.0),
                sanitize_f32(blue[1], 0.0).clamp(-2.0, 2.0),
                sanitize_f32(blue[2], 1.0).clamp(-2.0, 2.0),
            ];
            let mono = *monochrome;
            map_tiles_parallel_f32(&mut out, |px| {
                let (ir, ig, ib) = (px[0], px[1], px[2]);
                if mono {
                    let gray = (ir * r_w[0] + ig * r_w[1] + ib * r_w[2]).clamp(0.0, 1.0);
                    px[0] = gray;
                    px[1] = gray;
                    px[2] = gray;
                } else {
                    px[0] = (ir * r_w[0] + ig * r_w[1] + ib * r_w[2]).clamp(0.0, 1.0);
                    px[1] = (ir * g_w[0] + ig * g_w[1] + ib * g_w[2]).clamp(0.0, 1.0);
                    px[2] = (ir * b_w[0] + ig * b_w[1] + ib * b_w[2]).clamp(0.0, 1.0);
                }
            });
        }
        RasterFilter::GaussianBlur { radius_px } => {
            let src = flatten_asset_f32(asset);
            let blurred = gaussian_blur_f32(
                &src,
                asset.width,
                asset.height,
                sanitize_f32(*radius_px, 0.0),
            );
            return rebuild_asset_from_f32(asset, &blurred);
        }
        RasterFilter::MotionBlur {
            distance_px,
            angle_deg,
        } => {
            let dist = sanitize_f32(*distance_px, 0.0).clamp(0.0, 128.0);
            if dist > 0.25 {
                let rad = sanitize_f32(*angle_deg, 0.0).to_radians();
                let (sin_a, cos_a) = rad.sin_cos();
                let samples = (dist.ceil() as i32 * 2 + 1).clamp(3, 65);
                let half = samples / 2;
                let w = asset.width as i32;
                let h = asset.height as i32;
                let src = flatten_asset_f32(asset);
                let mut dst = vec![0.0_f32; src.len()];
                let row_stride = asset.width as usize * 4;
                dst.par_chunks_mut(row_stride)
                    .enumerate()
                    .for_each(|(y, row)| {
                        for x in 0..asset.width as usize {
                            let mut acc = [0.0_f32; 4];
                            for s in -half..=half {
                                let t = (s as f32 / half.max(1) as f32) * (dist * 0.5);
                                let sx = (x as f32 + t * cos_a).round().clamp(0.0, (w - 1) as f32)
                                    as usize;
                                let sy = (y as f32 + t * sin_a).round().clamp(0.0, (h - 1) as f32)
                                    as usize;
                                let idx = (sy * asset.width as usize + sx) * 4;
                                for c in 0..4 {
                                    acc[c] += src[idx + c];
                                }
                            }
                            let inv = 1.0 / (samples as f32);
                            let o = x * 4;
                            for c in 0..4 {
                                row[o + c] = (acc[c] * inv).clamp(0.0, 1.0);
                            }
                        }
                    });
                return rebuild_asset_from_f32(asset, &dst);
            }
        }
        RasterFilter::UnsharpMask {
            amount,
            radius_px,
            threshold,
        } => {
            let amt = sanitize_f32(*amount, 0.0).clamp(0.0, 5.0);
            let orig = flatten_asset_f32(asset);
            let blurred = gaussian_blur_f32(
                &orig,
                asset.width,
                asset.height,
                sanitize_f32(*radius_px, 1.5),
            );
            let thresh = f32::from(*threshold) / 255.0;
            let mut dst = orig.clone();
            for ((d, o), b) in dst
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(orig.as_chunks::<4>().0.iter())
                .zip(blurred.as_chunks::<4>().0.iter())
            {
                for c in 0..3 {
                    let diff = o[c] - b[c];
                    if diff.abs() >= thresh {
                        d[c] = (o[c] + diff * amt).clamp(0.0, 1.0);
                    }
                }
            }
            return rebuild_asset_from_f32(asset, &dst);
        }
        RasterFilter::Sharpen { amount } => {
            let a = sanitize_f32(*amount, 0.5).clamp(0.0, 5.0);
            let kernel = [0.0, -a, 0.0, -a, 1.0 + 4.0 * a, -a, 0.0, -a, 0.0];
            let src = flatten_asset_f32(asset);
            let convolved = convolve_3x3_f32(&src, asset.width, asset.height, &kernel, 0.0);
            return rebuild_asset_from_f32(asset, &convolved);
        }
        RasterFilter::Noise {
            amount,
            monochrome,
            seed,
        } => {
            let amt = sanitize_f32(*amount, 0.1).clamp(0.0, 1.0);
            let mono = *monochrome;
            let base_seed = *seed;
            let tiles_x = out.tiles_x.max(1);
            let depth = out.pixel_depth;
            out.tiles
                .par_iter_mut()
                .enumerate()
                .for_each(|(t_idx, tile)| {
                    let tx = (t_idx as u32) % tiles_x;
                    let ty = (t_idx as u32) / tiles_x;
                    let x0 = tx * RASTER_TILE_DIM;
                    let y0 = ty * RASTER_TILE_DIM;
                    let tw = tile.width as usize;
                    let th = tile.height as usize;
                    let data = tile.make_mut();
                    for ly in 0..th {
                        let y = y0 as u64 + ly as u64;
                        for lx in 0..tw {
                            let x = x0 as u64 + lx as u64;
                            let h0 = splitmix64(
                                base_seed
                                    ^ y.wrapping_mul(0x9e37_79b9_7f4a_7c15)
                                    ^ x.wrapping_mul(0xbf58_476d_1ce4_e5b9),
                            );
                            let n0 = ((h0 as f32 / u64::MAX as f32) * 2.0 - 1.0) * amt;
                            let (n1, n2) = if mono {
                                (n0, n0)
                            } else {
                                let h1 = splitmix64(h0);
                                let h2 = splitmix64(h1);
                                (
                                    ((h1 as f32 / u64::MAX as f32) * 2.0 - 1.0) * amt,
                                    ((h2 as f32 / u64::MAX as f32) * 2.0 - 1.0) * amt,
                                )
                            };
                            let idx_px = ly * tw + lx;
                            apply_tile_pixel_f32(data, depth, idx_px, |px| {
                                px[0] = (px[0] + n0).clamp(0.0, 1.0);
                                px[1] = (px[1] + n1).clamp(0.0, 1.0);
                                px[2] = (px[2] + n2).clamp(0.0, 1.0);
                            });
                        }
                    }
                });
        }
        RasterFilter::Vignette {
            strength,
            radius,
            feather,
        } => {
            let str_f = sanitize_f32(*strength, 0.5).clamp(0.0, 1.0);
            let rad = sanitize_f32(*radius, 0.6).clamp(0.05, 1.5);
            let feath = sanitize_f32(*feather, 0.4).clamp(0.01, 1.5);
            let cx = (asset.width as f32) * 0.5;
            let cy = (asset.height as f32) * 0.5;
            let max_r = cx.hypot(cy).max(1.0);
            let tiles_x = out.tiles_x.max(1);
            let depth = out.pixel_depth;
            out.tiles
                .par_iter_mut()
                .enumerate()
                .for_each(|(t_idx, tile)| {
                    let tx = (t_idx as u32) % tiles_x;
                    let ty = (t_idx as u32) / tiles_x;
                    let x0 = tx * RASTER_TILE_DIM;
                    let y0 = ty * RASTER_TILE_DIM;
                    let tw = tile.width as usize;
                    let th = tile.height as usize;
                    let data = tile.make_mut();
                    for ly in 0..th {
                        let dy = ((y0 + ly as u32) as f32 + 0.5 - cy) / max_r;
                        for lx in 0..tw {
                            let dx = ((x0 + lx as u32) as f32 + 0.5 - cx) / max_r;
                            let dist = dx.hypot(dy);
                            let t = ((dist - rad) / feath).clamp(0.0, 1.0);
                            let smooth = t * t * (3.0 - 2.0 * t);
                            let factor = (1.0_f32 - str_f * smooth).clamp(0.0, 1.0);
                            let idx_px = ly * tw + lx;
                            apply_tile_pixel_f32(data, depth, idx_px, |px| {
                                px[0] *= factor;
                                px[1] *= factor;
                                px[2] *= factor;
                            });
                        }
                    }
                });
        }
        RasterFilter::Emboss { angle_deg, depth } => {
            let d = sanitize_f32(*depth, 1.0).clamp(0.1, 5.0);
            let rad = sanitize_f32(*angle_deg, 45.0).to_radians();
            let (s, c) = rad.sin_cos();
            let kernel = [
                (-c - s) * d,
                -s * d,
                (c - s) * d,
                -c * d,
                1.0,
                c * d,
                (-c + s) * d,
                s * d,
                (c + s) * d,
            ];
            let src = flatten_asset_f32(asset);
            let convolved =
                convolve_3x3_f32(&src, asset.width, asset.height, &kernel, 128.0 / 255.0);
            return rebuild_asset_from_f32(asset, &convolved);
        }
        RasterFilter::EdgeDetect { sensitivity } => {
            let sens = sanitize_f32(*sensitivity, 1.0).clamp(0.1, 10.0);
            let w = asset.width as usize;
            let h = asset.height as usize;
            let src = flatten_asset_f32(asset);
            let mut dst = src.clone();
            let luma = |x: i32, y: i32| -> f32 {
                let cx = x.clamp(0, w as i32 - 1) as usize;
                let cy = y.clamp(0, h as i32 - 1) as usize;
                let idx = (cy * w + cx) * 4;
                0.2126 * src[idx] + 0.7152 * src[idx + 1] + 0.0722 * src[idx + 2]
            };
            dst.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                let yi = y as i32;
                for x in 0..w {
                    let xi = x as i32;
                    let gx = -luma(xi - 1, yi - 1) - 2.0 * luma(xi - 1, yi) - luma(xi - 1, yi + 1)
                        + luma(xi + 1, yi - 1)
                        + 2.0 * luma(xi + 1, yi)
                        + luma(xi + 1, yi + 1);
                    let gy = -luma(xi - 1, yi - 1) - 2.0 * luma(xi, yi - 1) - luma(xi + 1, yi - 1)
                        + luma(xi - 1, yi + 1)
                        + 2.0 * luma(xi, yi + 1)
                        + luma(xi + 1, yi + 1);
                    let mag = (gx.hypot(gy) * sens).clamp(0.0, 1.0);
                    let o = x * 4;
                    row[o] = mag;
                    row[o + 1] = mag;
                    row[o + 2] = mag;
                }
            });
            return rebuild_asset_from_f32(asset, &dst);
        }
    }

    out
}

/// Resampling kernel algorithm selector for bitmap scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResampleAlgorithm {
    Nearest,
    Bilinear,
    #[default]
    Bicubic,
    Lanczos3,
}

impl ResampleAlgorithm {
    const fn to_image_filter(self) -> FilterType {
        let lanczos = FilterType::Lanczos3;
        match self {
            Self::Nearest => FilterType::Nearest,
            Self::Bilinear => FilterType::Triangle,
            Self::Bicubic => FilterType::CatmullRom,
            Self::Lanczos3 => lanczos,
        }
    }
}

/// Resamples a [`RasterAsset`] to `(new_w, new_h)` using high-quality Catmull-Rom bicubic filtering.
pub fn resample(asset: &RasterAsset, new_w: u32, new_h: u32) -> RenderResult<RasterAsset> {
    resample_with_algorithm(asset, new_w, new_h, ResampleAlgorithm::Bicubic)
}

/// Resamples a [`RasterAsset`] to `(new_w, new_h)` with the chosen [`ResampleAlgorithm`],
/// preserving both 8-bit and 16-bit channel precision.
pub fn resample_with_algorithm(
    asset: &RasterAsset,
    new_w: u32,
    new_h: u32,
    algorithm: ResampleAlgorithm,
) -> RenderResult<RasterAsset> {
    validate_surface_dimensions(new_w, new_h)?;
    if !is_valid_raster_asset(asset) {
        return Err(RenderError::Raster(
            "source raster dimensions or buffer length invalid".into(),
        ));
    }
    let filter = algorithm.to_image_filter();
    let mut out = match asset.pixel_depth {
        PixelDepth::Bit8 => {
            let img = RgbaImage::from_raw(asset.width, asset.height, asset.to_rgba8_vec())
                .ok_or_else(|| {
                    RenderError::Raster("source raster buffer length mismatch".into())
                })?;
            let resized = image::imageops::resize(&img, new_w, new_h, filter);
            RasterAsset::new(
                new_w,
                new_h,
                resized.into_raw(),
                asset.source_dpi,
                asset.color_mode,
            )?
        }
        PixelDepth::Bit16 => {
            let img = ImageBuffer::<Rgba<u16>, Vec<u16>>::from_raw(
                asset.width,
                asset.height,
                asset.to_rgba16_vec(),
            )
            .ok_or_else(|| RenderError::Raster("source 16-bit raster length mismatch".into()))?;
            let resized = image::imageops::resize(&img, new_w, new_h, filter);
            RasterAsset::new_16bit(
                new_w,
                new_h,
                resized.into_raw(),
                asset.source_dpi,
                asset.color_mode,
            )?
        }
    };
    out.id = asset.id;
    Ok(out)
}

/// Per-channel luminance & RGB histogram computed from a [`RasterAsset`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterHistogram {
    pub bins: usize,
    pub red: Vec<u32>,
    pub green: Vec<u32>,
    pub blue: Vec<u32>,
    pub luma: Vec<u32>,
    pub total_pixels: u64,
}

/// Computes an `N`-bin RGB + BT.709 luminance histogram across all CoW tiles in `asset`.
#[must_use]
pub fn compute_histogram(asset: &RasterAsset, bins: usize) -> RasterHistogram {
    let n = bins.clamp(2, 65_536);
    let mut hist = RasterHistogram {
        bins: n,
        red: vec![0; n],
        green: vec![0; n],
        blue: vec![0; n],
        luma: vec![0; n],
        total_pixels: 0,
    };
    if !is_valid_raster_asset(asset) {
        return hist;
    }
    let scale = (n - 1) as f32;
    let bin_idx = |v: f32| -> usize { (v.clamp(0.0, 1.0) * scale).round() as usize };

    for tile in &asset.tiles {
        let data = tile.as_slice();
        match asset.pixel_depth {
            PixelDepth::Bit8 => {
                for px in data.as_chunks::<4>().0 {
                    let r = f32::from(px[0]) / 255.0;
                    let g = f32::from(px[1]) / 255.0;
                    let b = f32::from(px[2]) / 255.0;
                    let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                    hist.red[bin_idx(r)] += 1;
                    hist.green[bin_idx(g)] += 1;
                    hist.blue[bin_idx(b)] += 1;
                    hist.luma[bin_idx(l)] += 1;
                    hist.total_pixels += 1;
                }
            }
            PixelDepth::Bit16 => {
                for px in data.as_chunks::<8>().0 {
                    let r = f32::from(u16::from_le_bytes([px[0], px[1]])) / 65_535.0;
                    let g = f32::from(u16::from_le_bytes([px[2], px[3]])) / 65_535.0;
                    let b = f32::from(u16::from_le_bytes([px[4], px[5]])) / 65_535.0;
                    let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                    hist.red[bin_idx(r)] += 1;
                    hist.green[bin_idx(g)] += 1;
                    hist.blue[bin_idx(b)] += 1;
                    hist.luma[bin_idx(l)] += 1;
                    hist.total_pixels += 1;
                }
            }
        }
    }
    hist
}

/// Converts a [`RasterAsset`] between 8-bit (`Bit8`) and 16-bit (`Bit16`) channel depth.
#[must_use]
pub fn convert_raster_depth(asset: &RasterAsset, target: PixelDepth) -> RasterAsset {
    if asset.pixel_depth == target || !is_valid_raster_asset(asset) {
        return asset.clone();
    }
    let converted = match target {
        PixelDepth::Bit8 => {
            let rgba8 = asset.to_rgba8_vec();
            RasterAsset::new(
                asset.width,
                asset.height,
                rgba8,
                asset.source_dpi,
                asset.color_mode,
            )
        }
        PixelDepth::Bit16 => {
            let rgba16 = asset.to_rgba16_vec();
            RasterAsset::new_16bit(
                asset.width,
                asset.height,
                rgba16,
                asset.source_dpi,
                asset.color_mode,
            )
        }
    };
    match converted {
        Ok(mut out) => {
            out.id = asset.id;
            out
        }
        Err(_) => asset.clone(),
    }
}

/// Converts a [`RasterAsset`] between `ColorMode::Rgb` and `ColorMode::Cmyk`, optionally gamut-mapping
/// through an active [`IccColorPipeline`] while preserving 8-bit/16-bit channel precision.
#[must_use]
pub fn convert_raster_color_mode(
    asset: &RasterAsset,
    target: ColorMode,
    icc: Option<&IccColorPipeline>,
) -> RasterAsset {
    if !is_valid_raster_asset(asset) {
        return asset.clone();
    }
    let mut out = asset.clone();
    if out.color_mode == target {
        return out;
    }
    out.color_mode = target;
    let default_icc = IccColorPipeline::default();
    let pipeline = icc.unwrap_or(&default_icc);
    if target == ColorMode::Cmyk {
        map_tiles_parallel_f32(&mut out, |px| {
            let [c, m, y, k] = pipeline.srgb_to_cmyk([px[0], px[1], px[2]]);
            let cmyk_col = Color::cmyk(c, m, y, k);
            let mapped = cmyk_col.to_rgba_f32_icc(pipeline);
            px[0] = mapped[0];
            px[1] = mapped[1];
            px[2] = mapped[2];
        });
    }
    out
}

/// Converts a [`RasterAsset`] to grayscale (using BT.709 / Gray ICC luminance), preserving depth.
#[must_use]
pub fn convert_raster_grayscale(
    asset: &RasterAsset,
    icc: Option<&IccColorPipeline>,
) -> RasterAsset {
    let mut out = asset.clone();
    if !is_valid_raster_asset(asset) {
        return out;
    }
    map_tiles_parallel_f32(&mut out, |px| {
        let l = (0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2]).clamp(0.0, 1.0);
        let mapped = if let Some(p) = icc {
            p.gray_to_srgb(l)
        } else {
            [l, l, l]
        };
        px[0] = mapped[0];
        px[1] = mapped[1];
        px[2] = mapped[2];
    });
    out
}

fn convert_object_to_bitmap_with_bounds_inner(
    doc: &Document,
    obj_id: ObjectId,
    dpi: f64,
) -> RenderResult<(RasterAsset, Rect2D)> {
    let _obj = doc
        .object(obj_id)
        .ok_or_else(|| RenderError::InvalidParameter(format!("object {obj_id} not found")))?;
    let safe_dpi = sanitize_f64(dpi, 300.0).clamp(1.0, 2400.0) as f32;
    let bounds = doc
        .visual_world_bounds(obj_id)
        .or_else(|| doc.world_object_bounds(obj_id))
        .ok_or_else(|| {
            RenderError::InvalidParameter(format!("object {obj_id} has empty or non-finite bounds"))
        })?;
    // `visual_world_bounds` already covers nested Group/SymbolInstance children, Symmetry,
    // Blend, Extrude25D, Envelope/Perspective, effect overhangs, and spatial padding.
    // Expand by 4.0 mm anti-aliasing / miter bleed margin.
    let padded = bounds.expand(4.0);
    let px_per_mm = f64::from(safe_dpi) / MM_PER_INCH;
    let width = (padded.width() * px_per_mm).ceil().max(1.0) as u32;
    let height = (padded.height() * px_per_mm).ceil().max(1.0) as u32;

    let mut viewport = Viewport::for_export(width, height, f64::from(safe_dpi));
    viewport.pan_mm = padded.min();
    let mut backend = TinySkiaBackend::new(width, height)?;
    backend.clear(Color::TRANSPARENT);

    let mut renderer = SceneRenderer::new();
    let mut opts = RenderOptions::export();
    opts.canvas_bg = Color::TRANSPARENT;
    opts.page_bg = Color::TRANSPARENT;
    renderer.render_single_object(
        doc,
        obj_id,
        &viewport,
        ViewMode::Enhanced,
        &opts,
        &mut backend,
    );

    let rgba_buf = backend.into_rgba();
    let asset = RasterAsset::new(width, height, rgba_buf.rgba, safe_dpi, doc.color_mode())?;
    Ok((asset, padded))
}

/// Rasterizes a single scene object (with its full fills, outlines, effects, and text) into a
/// [`RasterAsset`] at `dpi`, returning both the raster asset and the exact padded world-space
/// [`Rect2D`] covered by its pixels.
pub fn convert_object_to_bitmap_with_bounds(
    doc: &Document,
    obj_id: ObjectId,
    dpi: f64,
) -> Result<(RasterAsset, Rect2D), String> {
    convert_object_to_bitmap_with_bounds_inner(doc, obj_id, dpi).map_err(|e| e.to_string())
}

/// Rasterizes a single scene object (with its full fills, outlines, effects, and text) into a [`RasterAsset`] at `dpi`.
pub fn convert_object_to_bitmap(
    doc: &Document,
    obj_id: ObjectId,
    dpi: f32,
) -> RenderResult<RasterAsset> {
    convert_object_to_bitmap_with_bounds_inner(doc, obj_id, f64::from(dpi)).map(|(asset, _)| asset)
}

#[inline]
fn apply_tile_pixel_f32<F: FnMut(&mut [f32; 4])>(
    data: &mut [u8],
    depth: PixelDepth,
    px_idx: usize,
    mut f: F,
) {
    match depth {
        PixelDepth::Bit8 => {
            let o = px_idx * 4;
            let mut px = [
                f32::from(data[o]) / 255.0,
                f32::from(data[o + 1]) / 255.0,
                f32::from(data[o + 2]) / 255.0,
                f32::from(data[o + 3]) / 255.0,
            ];
            f(&mut px);
            for c in 0..4 {
                data[o + c] = (px[c].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        PixelDepth::Bit16 => {
            let o = px_idx * 8;
            let mut px = [
                f32::from(u16::from_le_bytes([data[o], data[o + 1]])) / 65_535.0,
                f32::from(u16::from_le_bytes([data[o + 2], data[o + 3]])) / 65_535.0,
                f32::from(u16::from_le_bytes([data[o + 4], data[o + 5]])) / 65_535.0,
                f32::from(u16::from_le_bytes([data[o + 6], data[o + 7]])) / 65_535.0,
            ];
            f(&mut px);
            for c in 0..4 {
                let u = (px[c].clamp(0.0, 1.0) * 65_535.0).round() as u16;
                let le = u.to_le_bytes();
                data[o + c * 2] = le[0];
                data[o + c * 2 + 1] = le[1];
            }
        }
    }
}

fn map_tiles_parallel_f32<F: Fn(&mut [f32; 4]) + Sync + Send>(asset: &mut RasterAsset, f: F) {
    let depth = asset.pixel_depth;
    asset.tiles.par_iter_mut().for_each(|cow: &mut CowTile| {
        let data = cow.make_mut();
        match depth {
            PixelDepth::Bit8 => {
                for px_bytes in data.as_chunks_mut::<4>().0 {
                    let mut px = [
                        f32::from(px_bytes[0]) / 255.0,
                        f32::from(px_bytes[1]) / 255.0,
                        f32::from(px_bytes[2]) / 255.0,
                        f32::from(px_bytes[3]) / 255.0,
                    ];
                    f(&mut px);
                    for c in 0..4 {
                        px_bytes[c] = (px[c].clamp(0.0, 1.0) * 255.0).round() as u8;
                    }
                }
            }
            PixelDepth::Bit16 => {
                for px_bytes in data.as_chunks_mut::<8>().0 {
                    let mut px = [
                        f32::from(u16::from_le_bytes([px_bytes[0], px_bytes[1]])) / 65_535.0,
                        f32::from(u16::from_le_bytes([px_bytes[2], px_bytes[3]])) / 65_535.0,
                        f32::from(u16::from_le_bytes([px_bytes[4], px_bytes[5]])) / 65_535.0,
                        f32::from(u16::from_le_bytes([px_bytes[6], px_bytes[7]])) / 65_535.0,
                    ];
                    f(&mut px);
                    for c in 0..4 {
                        let u = (px[c].clamp(0.0, 1.0) * 65_535.0).round() as u16;
                        let le = u.to_le_bytes();
                        px_bytes[c * 2] = le[0];
                        px_bytes[c * 2 + 1] = le[1];
                    }
                }
            }
        }
    });
}

fn flatten_asset_f32(asset: &RasterAsset) -> Vec<f32> {
    let w = asset.width as usize;
    let h = asset.height as usize;
    let mut out = vec![0.0_f32; w * h * 4];
    match asset.pixel_depth {
        PixelDepth::Bit8 => {
            for (d, s) in out.iter_mut().zip(asset.to_rgba8_vec()) {
                *d = f32::from(s) / 255.0;
            }
        }
        PixelDepth::Bit16 => {
            for (d, s) in out.iter_mut().zip(asset.to_rgba16_vec()) {
                *d = f32::from(s) / 65_535.0;
            }
        }
    }
    out
}

fn rebuild_asset_from_f32(template: &RasterAsset, rgba: &[f32]) -> RasterAsset {
    let mut out = match template.pixel_depth {
        PixelDepth::Bit8 => {
            let bytes: Vec<u8> = rgba
                .iter()
                .map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                .collect();
            RasterAsset::new(
                template.width,
                template.height,
                bytes,
                template.source_dpi,
                template.color_mode,
            )
        }
        PixelDepth::Bit16 => {
            let words: Vec<u16> = rgba
                .iter()
                .map(|&v| (v.clamp(0.0, 1.0) * 65_535.0).round() as u16)
                .collect();
            RasterAsset::new_16bit(
                template.width,
                template.height,
                words,
                template.source_dpi,
                template.color_mode,
            )
        }
    }
    .unwrap_or_else(|_| template.clone());
    out.id = template.id;
    out
}

/// Fast separable Gaussian blur on a straight RGBA8 buffer (also shared by DropShadow!).
#[must_use]
pub fn gaussian_blur_rgba(src: &[u8], width: u32, height: u32, radius_px: f32) -> Vec<u8> {
    let r = sanitize_f32(radius_px, 0.0).clamp(0.0, 128.0);
    let w = width as usize;
    let h = height as usize;
    if r <= 0.1 || w == 0 || h == 0 || src.len() != w * h * 4 {
        return src.to_vec();
    }
    let f32_src: Vec<f32> = src.iter().map(|&b| f32::from(b) / 255.0).collect();
    let f32_dst = gaussian_blur_f32(&f32_src, width, height, r);
    f32_dst
        .into_iter()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect()
}

fn gaussian_blur_f32(src: &[f32], width: u32, height: u32, radius_px: f32) -> Vec<f32> {
    let r = sanitize_f32(radius_px, 0.0).clamp(0.0, 128.0);
    let w = width as usize;
    let h = height as usize;
    if r <= 0.1 || w == 0 || h == 0 || src.len() != w * h * 4 {
        return src.to_vec();
    }
    let kernel = build_gaussian_kernel(r);
    let half = (kernel.len() / 2) as i32;

    // Horizontal pass (premultiplied to prevent dark color fringing on transparent edges)
    let mut temp = vec![0.0_f32; w * h * 4];
    temp.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let mut acc = [0.0_f32; 4];
            for (ki, &kw) in kernel.iter().enumerate() {
                let sx = (x as i32 + ki as i32 - half).clamp(0, w as i32 - 1) as usize;
                let sidx = (y * w + sx) * 4;
                let a = src[sidx + 3].clamp(0.0, 1.0);
                acc[0] += src[sidx] * a * kw;
                acc[1] += src[sidx + 1] * a * kw;
                acc[2] += src[sidx + 2] * a * kw;
                acc[3] += a * kw;
            }
            let o = x * 4;
            row[o..o + 4].copy_from_slice(&acc);
        }
    });

    // Vertical pass + unpremultiply
    let mut dst = vec![0.0_f32; w * h * 4];
    dst.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let mut acc = [0.0_f32; 4];
            for (ki, &kw) in kernel.iter().enumerate() {
                let sy = (y as i32 + ki as i32 - half).clamp(0, h as i32 - 1) as usize;
                let tidx = (sy * w + x) * 4;
                for c in 0..4 {
                    acc[c] += temp[tidx + c] * kw;
                }
            }
            let af = acc[3].clamp(0.0, 1.0);
            let o = x * 4;
            if af <= 1e-6 {
                row[o..o + 4].copy_from_slice(&[0.0, 0.0, 0.0, 0.0]);
            } else {
                row[o] = (acc[0] / af).clamp(0.0, 1.0);
                row[o + 1] = (acc[1] / af).clamp(0.0, 1.0);
                row[o + 2] = (acc[2] / af).clamp(0.0, 1.0);
                row[o + 3] = af;
            }
        }
    });
    dst
}

pub(crate) fn build_gaussian_kernel(radius_px: f32) -> Vec<f32> {
    let sigma = (radius_px * 0.5).max(0.35);
    let half = (radius_px.ceil() as usize).clamp(1, 128);
    let len = half * 2 + 1;
    let mut k = Vec::with_capacity(len);
    let two_sigma_sq = 2.0 * sigma * sigma;
    let mut sum = 0.0_f32;
    for i in 0..len {
        let x = i as f32 - half as f32;
        let w = (-(x * x) / two_sigma_sq).exp();
        k.push(w);
        sum += w;
    }
    if sum > 0.0 {
        for w in &mut k {
            *w /= sum;
        }
    }
    k
}

fn convolve_3x3_f32(
    src: &[f32],
    width: u32,
    height: u32,
    kernel: &[f32; 9],
    bias: f32,
) -> Vec<f32> {
    let w = width as usize;
    let h = height as usize;
    let mut dst = src.to_vec();
    dst.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let mut acc = [bias; 3];
            for ky in -1_i32..=1 {
                let sy = (y as i32 + ky).clamp(0, h as i32 - 1) as usize;
                for kx in -1_i32..=1 {
                    let sx = (x as i32 + kx).clamp(0, w as i32 - 1) as usize;
                    let kw = kernel[((ky + 1) * 3 + (kx + 1)) as usize];
                    let sidx = (sy * w + sx) * 4;
                    acc[0] += src[sidx] * kw;
                    acc[1] += src[sidx + 1] * kw;
                    acc[2] += src[sidx + 2] * kw;
                }
            }
            let o = x * 4;
            row[o] = acc[0].clamp(0.0, 1.0);
            row[o + 1] = acc[1].clamp(0.0, 1.0);
            row[o + 2] = acc[2].clamp(0.0, 1.0);
        }
    });
    dst
}

fn prepare_tone_curve_knots(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut sorted: Vec<(f32, f32)> = points
        .iter()
        .map(|&(x, y)| {
            (
                sanitize_f32(x, 0.0).clamp(0.0, 1.0),
                sanitize_f32(y, 0.0).clamp(0.0, 1.0),
            )
        })
        .collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    if sorted.is_empty() {
        sorted.push((0.0, 0.0));
        sorted.push((1.0, 1.0));
    } else {
        if sorted[0].0 > 0.0 {
            sorted.insert(0, (0.0, sorted[0].1));
        }
        if sorted.last().is_some_and(|p| p.0 < 1.0) {
            let last_y = sorted.last().map_or(1.0, |p| p.1);
            sorted.push((1.0, last_y));
        }
    }
    sorted
}

/// Monotonic Fritsch-Carlson cubic Hermite spline evaluation over sorted `(x, y)` knots.
#[inline]
fn eval_tone_curve_hermite(sorted: &[(f32, f32)], x: f32) -> f32 {
    if sorted.is_empty() {
        return x.clamp(0.0, 1.0);
    }
    if sorted.len() == 1 {
        return sorted[0].1.clamp(0.0, 1.0);
    }
    // Deduplicate knots sharing identical x within 1e-5
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(sorted.len());
    for &p in sorted {
        if let Some(last) = pts.last_mut() {
            if (p.0 - last.0).abs() <= 1e-5 {
                last.1 = p.1;
                continue;
            }
        }
        pts.push(p);
    }
    let n = pts.len();
    if n < 2 {
        return pts[0].1.clamp(0.0, 1.0);
    }
    let mut h = Vec::with_capacity(n - 1);
    let mut d = Vec::with_capacity(n - 1);
    for k in 0..(n - 1) {
        let hk = (pts[k + 1].0 - pts[k].0).max(1e-6);
        h.push(hk);
        d.push((pts[k + 1].1 - pts[k].1) / hk);
    }
    let mut m = vec![0.0_f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for k in 1..(n - 1) {
        if d[k - 1] * d[k] <= 0.0 {
            m[k] = 0.0;
        } else {
            m[k] = 0.5 * (d[k - 1] + d[k]);
        }
    }
    for k in 0..(n - 1) {
        let dk = d[k];
        if dk.abs() <= 1e-6 {
            m[k] = 0.0;
            m[k + 1] = 0.0;
        } else {
            let a = m[k] / dk;
            let b = m[k + 1] / dk;
            let s = a * a + b * b;
            if s > 9.0 {
                let tau = 3.0 / s.sqrt();
                m[k] = tau * a * dk;
                m[k + 1] = tau * b * dk;
            }
        }
    }

    let xc = x.clamp(pts[0].0, pts[n - 1].0);
    for k in 0..(n - 1) {
        let (x0, y0) = pts[k];
        let (x1, y1) = pts[k + 1];
        if xc >= x0 && xc <= x1 {
            let span = h[k];
            let t = ((xc - x0) / span).clamp(0.0, 1.0);
            let t2 = t * t;
            let t3 = t2 * t;
            let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
            let h10 = t3 - 2.0 * t2 + t;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = t3 - t2;
            return (h00 * y0 + h10 * span * m[k] + h01 * y1 + h11 * span * m[k + 1])
                .clamp(0.0, 1.0);
        }
    }
    xc.clamp(0.0, 1.0)
}

fn build_tone_curve_lut_from_knots(sorted: &[(f32, f32)]) -> [u8; 256] {
    let mut lut = [0u8; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let y = eval_tone_curve_hermite(sorted, i as f32 / 255.0);
        *slot = (y * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    lut
}

fn build_tone_curve_lut16_from_knots(sorted: &[(f32, f32)]) -> Vec<u16> {
    let mut lut = vec![0u16; 65_536];
    for (i, slot) in lut.iter_mut().enumerate() {
        let y = eval_tone_curve_hermite(sorted, i as f32 / 65_535.0);
        *slot = (y * 65_535.0).round().clamp(0.0, 65_535.0) as u16;
    }
    lut
}

#[inline]
fn asset_pixel(asset: &RasterAsset, x: u32, y: u32) -> [u8; 4] {
    asset.read_pixel_rgba8(x, y).unwrap_or([0, 0, 0, 0])
}

#[inline]
fn asset_pixel_from_flat(bytes: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
    if idx + 3 < bytes.len() {
        [bytes[idx], bytes[idx + 1], bytes[idx + 2], bytes[idx + 3]]
    } else {
        [0, 0, 0, 0]
    }
}

fn blend_pixel_over(asset: &mut RasterAsset, x: u32, y: u32, sr: f32, sg: f32, sb: f32, sa: f32) {
    let Some([dr, dg, db, da]) = asset.read_pixel_f32(x, y) else {
        return;
    };

    let out_a = sa + da * (1.0 - sa);
    if out_a <= 1e-6 {
        asset.write_pixel_f32(x, y, [0.0, 0.0, 0.0, 0.0]);
        return;
    }
    let out_r = (sr * sa + dr * da * (1.0 - sa)) / out_a;
    let out_g = (sg * sa + dg * da * (1.0 - sa)) / out_a;
    let out_b = (sb * sa + db * da * (1.0 - sa)) / out_a;

    asset.write_pixel_f32(
        x,
        y,
        [
            out_r.clamp(0.0, 1.0),
            out_g.clamp(0.0, 1.0),
            out_b.clamp(0.0, 1.0),
            out_a.clamp(0.0, 1.0),
        ],
    );
}

fn brush_hardness_falloff(dist: f32, radius: f32, hardness: f32) -> f32 {
    if dist >= radius {
        return 0.0;
    }
    let norm = (dist / radius).clamp(0.0, 1.0);
    if norm <= hardness {
        1.0
    } else {
        let t = (1.0 - (norm - hardness) / (1.0 - hardness).max(1e-5)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
}

fn dab_elliptical_dist(px_x: f64, px_y: f64, dab: &StylusPoint) -> f32 {
    let dx = (px_x - dab.pos.x) as f32;
    let dy = (px_y - dab.pos.y) as f32;
    let tx = sanitize_f32(dab.tilt_x_deg, 0.0).clamp(-85.0, 85.0);
    let ty = sanitize_f32(dab.tilt_y_deg, 0.0).clamp(-85.0, 85.0);
    let tilt_mag = tx.hypot(ty).clamp(0.0, 85.0);
    let twist = sanitize_f32(dab.twist_deg, 0.0);
    if tilt_mag <= 1.0 && twist.abs() <= 1e-3 {
        return dx.hypot(dy);
    }
    let angle = ty.atan2(tx) + twist.to_radians();
    let (s, c) = angle.sin_cos();
    let lx = dx * c + dy * s;
    let ly = -dx * s + dy * c;
    let aspect = 1.0 + tilt_mag / 60.0;
    (lx / aspect).hypot(ly)
}

fn sample_stylus_dabs(points: &[StylusPoint], step: f32) -> Vec<StylusPoint> {
    let finite: Vec<StylusPoint> = points
        .iter()
        .copied()
        .filter(|p| p.pos.is_finite())
        .collect();
    if finite.is_empty() {
        return Vec::new();
    }
    let step_f64 = f64::from(step.max(0.5));
    let mut dabs = Vec::with_capacity(finite.len());
    dabs.push(finite[0]);
    for win in finite.windows(2) {
        if dabs.len() >= MAX_STROKE_DABS {
            break;
        }
        let a = win[0];
        let b = win[1];
        let dist = a.pos.distance_to(b.pos);
        if !dist.is_finite() || dist <= 1e-6 {
            continue;
        }
        let remaining = MAX_STROKE_DABS.saturating_sub(dabs.len());
        let raw_steps = (dist / step_f64).ceil();
        let steps = if !raw_steps.is_finite() || raw_steps >= remaining as f64 {
            remaining.max(1)
        } else {
            (raw_steps as usize).max(1)
        };
        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let tf = t as f32;
            dabs.push(StylusPoint {
                pos: a.pos.lerp(b.pos, t),
                pressure: a.pressure + (b.pressure - a.pressure) * tf,
                tilt_x_deg: a.tilt_x_deg + (b.tilt_x_deg - a.tilt_x_deg) * tf,
                tilt_y_deg: a.tilt_y_deg + (b.tilt_y_deg - a.tilt_y_deg) * tf,
                twist_deg: a.twist_deg + (b.twist_deg - a.twist_deg) * tf,
            });
        }
    }
    dabs
}

fn dab_bounds_stylus(dab: &StylusPoint, r: f32, width: u32, height: u32) -> (u32, u32, u32, u32) {
    let tx = sanitize_f32(dab.tilt_x_deg, 0.0).clamp(-85.0, 85.0);
    let ty = sanitize_f32(dab.tilt_y_deg, 0.0).clamp(-85.0, 85.0);
    let tilt_mag = tx.hypot(ty).clamp(0.0, 85.0);
    let aspect = 1.0 + tilt_mag / 60.0;
    dab_bounds(dab.pos, r * aspect, width, height)
}

fn dab_bounds(dab: Point2D, r: f32, width: u32, height: u32) -> (u32, u32, u32, u32) {
    let rf = f64::from(r);
    let x0 = ((dab.x - rf).floor() as i64).clamp(0, i64::from(width)) as u32;
    let y0 = ((dab.y - rf).floor() as i64).clamp(0, i64::from(height)) as u32;
    let x1 = ((dab.x + rf).ceil() as i64).clamp(0, i64::from(width)) as u32;
    let y1 = ((dab.y + rf).ceil() as i64).clamp(0, i64::from(height)) as u32;
    (x0, y0, x1, y1)
}

#[inline]
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cow_tile_sharing_only_clones_touched_tile() {
        let orig =
            RasterAsset::new(512, 512, vec![120u8; 512 * 512 * 4], 300.0, ColorMode::Rgb).unwrap();
        let before = orig.shared_tile_ptrs();
        assert_eq!(before.len(), 4);

        let mut edited = orig.clone();
        assert_eq!(edited.shared_tile_ptrs(), before);

        paint_brush_stroke(
            &mut edited,
            &[Point2D::new(20.0, 20.0)],
            4.0,
            1.0,
            Color::rgb(1.0, 0.0, 0.0),
            1.0,
        );
        let after = edited.shared_tile_ptrs();
        assert_ne!(after[0], before[0], "tile (0,0) must CoW copy");
        assert_eq!(after[1], before[1], "untouched tile (1,0) must stay shared");
        assert_eq!(after[2], before[2], "untouched tile (0,1) must stay shared");
        assert_eq!(after[3], before[3], "untouched tile (1,1) must stay shared");
    }

    #[test]
    fn raster_16bit_channel_mixer_lanczos_resample_and_histogram() {
        let mut pixels = Vec::with_capacity(64 * 64 * 4);
        for y in 0..64u16 {
            for x in 0..64u16 {
                pixels.push(x * 1000);
                pixels.push(y * 1000);
                pixels.push(32768);
                pixels.push(65535);
            }
        }
        let a16 = RasterAsset::new_16bit(64, 64, pixels, 300.0, ColorMode::Rgb).unwrap();
        assert_eq!(a16.pixel_depth(), PixelDepth::Bit16);

        let mixed = apply_raster_filter(
            &a16,
            &RasterFilter::ChannelMixer {
                red: [0.0, 1.0, 0.0],
                green: [1.0, 0.0, 0.0],
                blue: [0.0, 0.0, 1.0],
                monochrome: false,
            },
        );
        assert_eq!(mixed.pixel_depth(), PixelDepth::Bit16);
        let px0 = mixed.read_pixel_u16(5, 10).unwrap();
        assert_eq!(px0[0], 10_000);
        assert_eq!(px0[1], 5_000);

        let curved = apply_raster_filter(
            &a16,
            &RasterFilter::ToneCurve {
                points: vec![(0.0, 0.1), (0.5, 0.7), (1.0, 0.9)],
            },
        );
        assert_eq!(curved.pixel_depth(), PixelDepth::Bit16);
        assert!(curved.read_pixel_u16(0, 0).unwrap()[0] > 6000);

        let resized = resample_with_algorithm(&a16, 32, 32, ResampleAlgorithm::Lanczos3).unwrap();
        assert_eq!(resized.pixel_depth(), PixelDepth::Bit16);
        assert_eq!((resized.width, resized.height), (32, 32));

        let hist = compute_histogram(&a16, 256);
        assert_eq!(hist.bins, 256);
        assert_eq!(hist.total_pixels, 64 * 64);
        assert_eq!(hist.red.iter().map(|&v| u64::from(v)).sum::<u64>(), 64 * 64);

        let a8 = convert_raster_depth(&a16, PixelDepth::Bit8);
        assert_eq!(a8.pixel_depth(), PixelDepth::Bit8);
        let cmyk = convert_raster_color_mode(&a16, ColorMode::Cmyk, None);
        assert_eq!(cmyk.color_mode, ColorMode::Cmyk);
        assert_eq!(cmyk.pixel_depth(), PixelDepth::Bit16);
    }

    #[test]
    fn ellipse_mask_poisson_healing_and_cutout_alpha_matting() {
        let mask = RasterMask::from_ellipse(40, 40, Rect2D::new(10.0, 12.0, 30.0, 28.0));
        assert_eq!(mask.get(20, 20), 255);
        assert_eq!(mask.get(0, 0), 0);

        // Red/blue scene with central foreground region
        let mut img =
            RasterAsset::new(32, 32, vec![0u8; 32 * 32 * 4], 96.0, ColorMode::Rgb).unwrap();
        let mut fg_mask = RasterMask::empty(32, 32);
        for y in 0..32u32 {
            for x in 0..32u32 {
                let fg = (10..=21).contains(&x) && (10..=21).contains(&y);
                if fg {
                    img.write_pixel_f32(x, y, [0.95, 0.2, 0.2, 1.0]);
                    fg_mask.set(x, y, 255);
                } else {
                    img.write_pixel_f32(x, y, [0.1, 0.1, 0.9, 1.0]);
                }
            }
        }
        apply_cutout_background_removal(&mut img, &fg_mask, 2.0);
        assert!(img.read_pixel_rgba8(16, 16).unwrap()[3] > 220);
        assert!(img.read_pixel_rgba8(2, 2).unwrap()[3] < 40);

        let mut heal =
            RasterAsset::new(32, 32, vec![180u8; 32 * 32 * 4], 96.0, ColorMode::Rgb).unwrap();
        for y in 0..32u32 {
            for x in 0..32u32 {
                heal.write_pixel_f32(x, y, [0.85, 0.35, 0.20, 1.0]);
            }
        }
        apply_healing_stroke(
            &mut heal,
            Point2D::new(6.0, 6.0),
            &[Point2D::new(20.0, 20.0)],
            4.0,
            0.8,
        );
        let px = heal.read_pixel_rgba8(20, 20).unwrap();
        assert!(px[0] > 180 && px[2] < 80);
    }

    #[test]
    fn convert_raster_color_mode_cmyk_uses_icc_pipeline() {
        use coree_core::{
            generate_minimal_cmyk_icc_v2, generate_minimal_srgb_icc_v2, BuiltinIccProfile,
            RenderingIntent,
        };

        let srgb_bytes = generate_minimal_srgb_icc_v2("TestSRGB");
        let cmyk_bytes =
            generate_minimal_cmyk_icc_v2("TestSWOP", BuiltinIccProfile::UsWebCoatedSwopCmyk);
        let icc = IccColorPipeline::from_document_profiles(
            Some(&srgb_bytes),
            Some(&cmyk_bytes),
            None,
            None,
            RenderingIntent::RelativeColorimetric,
            true,
        );

        // Saturated cyan-green pixel where ICC separation differs markedly from naive 1-r-k
        let src_px = [20u8, 220, 140, 255];
        let asset = RasterAsset::new(2, 2, src_px.repeat(4), 96.0, ColorMode::Rgb).unwrap();
        let converted = convert_raster_color_mode(&asset, ColorMode::Cmyk, Some(&icc));
        assert_eq!(converted.color_mode, ColorMode::Cmyk);

        let got = converted.read_pixel_rgba8(0, 0).unwrap();
        let src_f32 = [
            f32::from(src_px[0]) / 255.0,
            f32::from(src_px[1]) / 255.0,
            f32::from(src_px[2]) / 255.0,
        ];
        let [c, m, y, k] = icc.srgb_to_cmyk(src_f32);
        let expected_f32 = Color::cmyk(c, m, y, k).to_rgba_f32_icc(&icc);
        let expected = [
            (expected_f32[0] * 255.0).round() as u8,
            (expected_f32[1] * 255.0).round() as u8,
            (expected_f32[2] * 255.0).round() as u8,
            255,
        ];
        for ch in 0..4 {
            assert!(
                got[ch].abs_diff(expected[ch]) <= 1,
                "channel {ch}: got {got:?}, expected {expected:?}"
            );
        }

        // Verify naive Color::to_cmyk() + to_rgba_f32_icc(&icc) produces a measurably different result
        let [nc, nm, ny, nk] = Color::rgba(src_f32[0], src_f32[1], src_f32[2], 1.0).to_cmyk();
        let naive_f32 = Color::cmyk(nc, nm, ny, nk).to_rgba_f32_icc(&icc);
        let naive = [
            (naive_f32[0] * 255.0).round() as u8,
            (naive_f32[1] * 255.0).round() as u8,
            (naive_f32[2] * 255.0).round() as u8,
            255,
        ];
        assert_ne!(
            got, naive,
            "convert_raster_color_mode must route via pipeline.srgb_to_cmyk, not naive to_cmyk()"
        );
    }

    #[test]
    fn stylus_brush_stroke_modulates_radius_opacity_and_tilt() {
        let mut light =
            RasterAsset::new(128, 128, vec![0u8; 128 * 128 * 4], 96.0, ColorMode::Rgb).unwrap();
        let mut heavy = light.clone();

        paint_brush_stroke_stylus(
            &mut light,
            &[StylusPoint {
                pos: Point2D::new(64.0, 64.0),
                pressure: 0.2,
                tilt_x_deg: 0.0,
                tilt_y_deg: 0.0,
                twist_deg: 0.0,
            }],
            20.0,
            0.8,
            Color::rgba8(220, 40, 20, 255),
            1.0,
            None,
        );
        paint_brush_stroke_stylus(
            &mut heavy,
            &[StylusPoint {
                pos: Point2D::new(64.0, 64.0),
                pressure: 1.0,
                tilt_x_deg: 45.0,
                tilt_y_deg: 0.0,
                twist_deg: 0.0,
            }],
            20.0,
            0.8,
            Color::rgba8(220, 40, 20, 255),
            1.0,
            None,
        );

        let count_painted = |a: &RasterAsset| -> usize {
            a.to_rgba8_vec()
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| px[3] > 0)
                .count()
        };
        let light_px = count_painted(&light);
        let heavy_px = count_painted(&heavy);
        assert!(
            heavy_px > light_px * 2,
            "Higher pressure + tilt must cover significantly more pixels (light={light_px}, heavy={heavy_px})"
        );
        assert!(
            heavy.read_pixel_rgba8(64, 64).unwrap()[3] > light.read_pixel_rgba8(64, 64).unwrap()[3],
            "Higher pressure must yield higher center alpha"
        );
    }

    #[test]
    fn tone_curve_monotonic_hermite_preserves_identity_knots() {
        let knots = prepare_tone_curve_knots(&[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]);
        for i in 0..=10 {
            let x = i as f32 / 10.0;
            let y = eval_tone_curve_hermite(&knots, x);
            assert!(
                (y - x).abs() < 1e-4,
                "Expected monotonic Hermite identity at {x}, got {y}"
            );
        }
    }

    #[test]
    fn masked_brushes_and_flood_fill_preserve_cow_tiles() {
        let orig =
            RasterAsset::new(512, 512, vec![128u8; 512 * 512 * 4], 300.0, ColorMode::Rgb).unwrap();
        let before = orig.shared_tile_ptrs();
        // Mask covers only tile (0,0): [0..100, 0..100]
        let mask = RasterMask::from_rectangle(512, 512, Rect2D::from_xywh(0.0, 0.0, 100.0, 100.0));

        let mut erased = orig.clone();
        erase_stroke_stylus_masked(
            &mut erased,
            &[StylusPoint::uniform(Point2D::new(90.0, 90.0))],
            40.0,
            0.8,
            Some(&mask),
        );
        assert_eq!(erased.read_pixel_rgba8(150, 90).unwrap()[3], 128);
        assert_eq!(&erased.shared_tile_ptrs()[1..], &before[1..]);

        let mut filled = orig.clone();
        let cnt = raster_flood_fill_masked(
            &mut filled,
            50,
            50,
            Color::rgba8(20, 40, 200, 255),
            10,
            Some(&mask),
        );
        assert_eq!(cnt, 100 * 100);
        assert_eq!(&filled.shared_tile_ptrs()[1..], &before[1..]);
    }
}
