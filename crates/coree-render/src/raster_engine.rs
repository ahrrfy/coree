// SPDX-License-Identifier: MPL-2.0

//! Raster Studio Engine: brushes, eraser, clone stamp, smudge,
//! selection masks (Rectangle, Lasso, Magic Wand, Cutout background removal),
//! all 19 non-destructive [`RasterFilter`] implementations, resampling, and
//! vector-to-bitmap conversion.

use crate::backend::{validate_surface_dimensions, CanvasBackend, TinySkiaBackend};
use crate::error::{RenderError, RenderResult};
use crate::renderer::{RenderOptions, SceneRenderer};
use crate::viewport::{ViewMode, Viewport};
use coree_core::math::{sanitize_f32, MM_PER_INCH};
use coree_core::{Color, Document, ObjectId, Point2D, RasterAsset, RasterFilter, Rect2D};
use image::{imageops::FilterType, RgbaImage};
use rayon::prelude::*;
use std::collections::VecDeque;

/// Maximum brush/eraser/clone/smudge radius in pixels to prevent OOM or integer overflow.
const MAX_BRUSH_RADIUS_PX: f32 = 512.0;

/// Maximum interpolated stroke dabs emitted per call to prevent OOM on extreme coordinates.
const MAX_STROKE_DABS: usize = 16_384;

/// Validates that a [`RasterAsset`] has safe non-zero dimensions matching `rgba_bytes.len()`.
#[inline]
fn is_valid_raster_asset(asset: &RasterAsset) -> bool {
    if validate_surface_dimensions(asset.width, asset.height).is_err() {
        return false;
    }
    (asset.width as usize)
        .checked_mul(asset.height as usize)
        .and_then(|px| px.checked_mul(4))
        == Some(asset.rgba_bytes.len())
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

/// Applies Cutout background removal: feathers `foreground_mask` by `feather_px` and
/// multiplies `asset`'s alpha channel so background pixels become smoothly transparent.
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
    let mut mask = foreground_mask.clone();
    if feather_px > 0.0 {
        mask.feather(feather_px);
    }
    for (idx, px) in asset
        .rgba_bytes
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        let m = u16::from(mask.alpha[idx]);
        px[3] = ((u16::from(px[3]) * m + 127) / 255) as u8;
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
    let r = sanitize_f32(radius_px, 4.0).clamp(0.5, MAX_BRUSH_RADIUS_PX);
    let hard = sanitize_f32(hardness, 0.8).clamp(0.0, 1.0);
    let op = sanitize_f32(opacity, 1.0).clamp(0.0, 1.0);
    if points.is_empty() || op <= 1e-6 || !is_valid_raster_asset(asset) {
        return;
    }
    let [sr, sg, sb, sa] = color.to_rgba_f32();
    let base_alpha = sa * op;
    let dabs = sample_stroke_dabs(points, (r * 0.25).max(1.0));

    for dab in dabs {
        let (x0, y0, x1, y1) = dab_bounds(dab, r, asset.width, asset.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = ((f64::from(x) + 0.5 - dab.x).hypot(f64::from(y) + 0.5 - dab.y)) as f32;
                let falloff = brush_hardness_falloff(d, r, hard);
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

/// Erases pixel alpha smoothly along `points` onto `asset`.
pub fn erase_stroke(asset: &mut RasterAsset, points: &[Point2D], radius_px: f32, hardness: f32) {
    let r = sanitize_f32(radius_px, 4.0).clamp(0.5, MAX_BRUSH_RADIUS_PX);
    let hard = sanitize_f32(hardness, 0.8).clamp(0.0, 1.0);
    if points.is_empty() || !is_valid_raster_asset(asset) {
        return;
    }
    let dabs = sample_stroke_dabs(points, (r * 0.25).max(1.0));
    for dab in dabs {
        let (x0, y0, x1, y1) = dab_bounds(dab, r, asset.width, asset.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = ((f64::from(x) + 0.5 - dab.x).hypot(f64::from(y) + 0.5 - dab.y)) as f32;
                let falloff = brush_hardness_falloff(d, r, hard);
                if falloff <= 1e-5 {
                    continue;
                }
                let idx = ((y as usize) * (asset.width as usize) + (x as usize)) * 4;
                let cur_a = f32::from(asset.rgba_bytes[idx + 3]) / 255.0;
                let new_a = (cur_a * (1.0 - falloff)).clamp(0.0, 1.0);
                asset.rgba_bytes[idx + 3] = (new_a * 255.0).round() as u8;
            }
        }
    }
}

/// Clones pixels sampled relative to `src_origin` along `dst_points` onto `asset`.
pub fn clone_stamp_stroke(
    asset: &mut RasterAsset,
    src_origin: Point2D,
    dst_points: &[Point2D],
    radius_px: f32,
    hardness: f32,
) {
    let r = sanitize_f32(radius_px, 4.0).clamp(0.5, MAX_BRUSH_RADIUS_PX);
    let hard = sanitize_f32(hardness, 0.8).clamp(0.0, 1.0);
    if !src_origin.is_finite() || dst_points.is_empty() || !is_valid_raster_asset(asset) {
        return;
    }
    let dabs = sample_stroke_dabs(dst_points, (r * 0.25).max(1.0));
    let Some(first_dab) = dabs.first().copied() else {
        return;
    };
    let snapshot = asset.rgba_bytes.clone();
    let offset = src_origin - first_dab;
    let w = asset.width;
    let h = asset.height;

    for dab in dabs {
        let (x0, y0, x1, y1) = dab_bounds(dab, r, w, h);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = ((f64::from(x) + 0.5 - dab.x).hypot(f64::from(y) + 0.5 - dab.y)) as f32;
                let falloff = brush_hardness_falloff(d, r, hard);
                if falloff <= 1e-5 {
                    continue;
                }
                let sx = (f64::from(x) + offset.x).round() as i64;
                let sy = (f64::from(y) + offset.y).round() as i64;
                if sx < 0 || sy < 0 || sx >= i64::from(w) || sy >= i64::from(h) {
                    continue;
                }
                let sidx = ((sy as usize) * (w as usize) + (sx as usize)) * 4;
                let sr = f32::from(snapshot[sidx]) / 255.0;
                let sg = f32::from(snapshot[sidx + 1]) / 255.0;
                let sb = f32::from(snapshot[sidx + 2]) / 255.0;
                let sa = (f32::from(snapshot[sidx + 3]) / 255.0) * falloff;
                if sa > 1e-5 {
                    blend_pixel_over(asset, x, y, sr, sg, sb, sa);
                }
            }
        }
    }
}

/// Smudges existing pixel colors along `points` carrying a paint pickup buffer.
pub fn smudge_stroke(asset: &mut RasterAsset, points: &[Point2D], radius_px: f32, strength: f32) {
    let r = sanitize_f32(radius_px, 4.0).clamp(0.5, 128.0);
    let str_f = sanitize_f32(strength, 0.5).clamp(0.0, 1.0);
    if points.len() < 2 || str_f <= 1e-5 || !is_valid_raster_asset(asset) {
        return;
    }
    let dabs = sample_stroke_dabs(points, (r * 0.25).max(1.0));
    if dabs.len() < 2 {
        return;
    }
    let rad_i = r.ceil() as i32;
    let rad_i64 = i64::from(rad_i);
    let side = (rad_i * 2 + 1) as usize;
    let mut pickup = vec![[0.0_f32; 4]; side * side];
    let w_i64 = i64::from(asset.width);
    let h_i64 = i64::from(asset.height);

    // Seed pickup from first dab position
    let seed = dabs[0];
    let seed_x = seed.x.round().clamp(-1e9, 1e9) as i64;
    let seed_y = seed.y.round().clamp(-1e9, 1e9) as i64;
    for dy in -rad_i..=rad_i {
        for dx in -rad_i..=rad_i {
            let sx = (seed_x + i64::from(dx)).clamp(0, w_i64 - 1) as u32;
            let sy = (seed_y + i64::from(dy)).clamp(0, h_i64 - 1) as u32;
            let px = asset_pixel(asset, sx, sy);
            let k = ((dy + rad_i) as usize) * side + ((dx + rad_i) as usize);
            pickup[k] = [
                f32::from(px[0]) / 255.0,
                f32::from(px[1]) / 255.0,
                f32::from(px[2]) / 255.0,
                f32::from(px[3]) / 255.0,
            ];
        }
    }

    for dab in dabs.iter().skip(1) {
        let cx = dab.x.round().clamp(-1e9, 1e9) as i64;
        let cy = dab.y.round().clamp(-1e9, 1e9) as i64;
        if cx + rad_i64 < 0 || cx - rad_i64 >= w_i64 || cy + rad_i64 < 0 || cy - rad_i64 >= h_i64 {
            continue;
        }
        for dy in -rad_i..=rad_i {
            let py = cy + i64::from(dy);
            if py < 0 || py >= h_i64 {
                continue;
            }
            for dx in -rad_i..=rad_i {
                let px_i = cx + i64::from(dx);
                if px_i < 0 || px_i >= w_i64 {
                    continue;
                }
                let dist = ((dx * dx + dy * dy) as f32).sqrt();
                let w = brush_hardness_falloff(dist, r, 0.25) * str_f;
                if w <= 1e-5 {
                    continue;
                }
                let k = ((dy + rad_i) as usize) * side + ((dx + rad_i) as usize);
                let idx = ((py as usize) * (asset.width as usize) + (px_i as usize)) * 4;
                let cur = [
                    f32::from(asset.rgba_bytes[idx]) / 255.0,
                    f32::from(asset.rgba_bytes[idx + 1]) / 255.0,
                    f32::from(asset.rgba_bytes[idx + 2]) / 255.0,
                    f32::from(asset.rgba_bytes[idx + 3]) / 255.0,
                ];
                let mut next = [0.0_f32; 4];
                for c in 0..4 {
                    next[c] = cur[c] * (1.0 - w) + pickup[k][c] * w;
                    asset.rgba_bytes[idx + c] = (next[c] * 255.0).round().clamp(0.0, 255.0) as u8;
                    pickup[k][c] = pickup[k][c] * str_f + cur[c] * (1.0 - str_f);
                }
            }
        }
    }
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

/// Applies a single [`RasterFilter`] (all 19 Raster Studio adjustments & convolutions implemented).
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
            map_pixels_parallel(&mut out.rgba_bytes, |px| {
                for ch in &mut px[0..3] {
                    let v = f32::from(*ch) / 255.0;
                    let adj = ((v - 0.5) * factor + 0.5 + b).clamp(0.0, 1.0);
                    *ch = (adj * 255.0).round() as u8;
                }
            });
        }
        RasterFilter::ToneCurve { points } => {
            let lut = build_tone_curve_lut(points);
            map_pixels_parallel(&mut out.rgba_bytes, |px| {
                px[0] = lut[px[0] as usize];
                px[1] = lut[px[1] as usize];
                px[2] = lut[px[2] as usize];
            });
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
            map_pixels_parallel(&mut out.rgba_bytes, |px| {
                for ch in &mut px[0..3] {
                    let v = ((f32::from(*ch) / 255.0 - ib) / span).clamp(0.0, 1.0);
                    let g = v.powf(inv_gamma);
                    let mapped = (ob + g * (ow - ob)).clamp(0.0, 1.0);
                    *ch = (mapped * 255.0).round() as u8;
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
            map_pixels_parallel(&mut out.rgba_bytes, |px| {
                let col = Color::rgba8(px[0], px[1], px[2], px[3]);
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
                let [r, g, b, _] = Color::hsl(nh, ns, nl).to_rgba8();
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
            map_pixels_parallel(&mut out.rgba_bytes, |px| {
                let r = (f32::from(px[0]) / 255.0 + cr * 0.5).clamp(0.0, 1.0);
                let g = (f32::from(px[1]) / 255.0 + mg * 0.5).clamp(0.0, 1.0);
                let b = (f32::from(px[2]) / 255.0 + yb * 0.5).clamp(0.0, 1.0);
                px[0] = (r * 255.0).round() as u8;
                px[1] = (g * 255.0).round() as u8;
                px[2] = (b * 255.0).round() as u8;
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
            map_pixels_parallel(&mut out.rgba_bytes, |px| {
                let r = f32::from(px[0]) / 255.0;
                let g = f32::from(px[1]) / 255.0;
                let b = f32::from(px[2]) / 255.0;
                let dist = ((r - tr).powi(2) + (g - tg).powi(2) + (b - tb).powi(2)).sqrt();
                if dist <= tol {
                    let blend = (1.0 - dist / tol).clamp(0.0, 1.0);
                    px[0] = ((r + (rr - r) * blend) * 255.0).round().clamp(0.0, 255.0) as u8;
                    px[1] = ((g + (rg - g) * blend) * 255.0).round().clamp(0.0, 255.0) as u8;
                    px[2] = ((b + (rb - b) * blend) * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            });
        }
        RasterFilter::Gamma { gamma } => {
            let g = sanitize_f32(*gamma, 1.0).clamp(0.05, 10.0);
            let inv_g = 1.0 / g;
            let mut lut = [0u8; 256];
            for (i, slot) in lut.iter_mut().enumerate() {
                *slot = ((i as f32 / 255.0).powf(inv_g) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
            map_pixels_parallel(&mut out.rgba_bytes, |px| {
                px[0] = lut[px[0] as usize];
                px[1] = lut[px[1] as usize];
                px[2] = lut[px[2] as usize];
            });
        }
        RasterFilter::Desaturate => {
            map_pixels_parallel(&mut out.rgba_bytes, |px| {
                let l = (0.2126 * f32::from(px[0])
                    + 0.7152 * f32::from(px[1])
                    + 0.0722 * f32::from(px[2]))
                .round()
                .clamp(0.0, 255.0) as u8;
                px[0] = l;
                px[1] = l;
                px[2] = l;
            });
        }
        RasterFilter::Invert => {
            map_pixels_parallel(&mut out.rgba_bytes, |px| {
                px[0] = 255 - px[0];
                px[1] = 255 - px[1];
                px[2] = 255 - px[2];
            });
        }
        RasterFilter::Posterize { levels } => {
            let lv = f32::from((*levels).max(2) - 1);
            map_pixels_parallel(&mut out.rgba_bytes, |px| {
                for ch in &mut px[0..3] {
                    let q = ((f32::from(*ch) / 255.0) * lv).round() / lv;
                    *ch = (q * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            });
        }
        RasterFilter::Threshold { cutoff } => {
            let cut = *cutoff;
            map_pixels_parallel(&mut out.rgba_bytes, |px| {
                let l = (0.2126 * f32::from(px[0])
                    + 0.7152 * f32::from(px[1])
                    + 0.0722 * f32::from(px[2]))
                .round() as u8;
                let v = if l >= cut { 255 } else { 0 };
                px[0] = v;
                px[1] = v;
                px[2] = v;
            });
        }
        RasterFilter::GaussianBlur { radius_px } => {
            out.rgba_bytes = gaussian_blur_rgba(
                &asset.rgba_bytes,
                asset.width,
                asset.height,
                sanitize_f32(*radius_px, 0.0),
            );
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
                let src = &asset.rgba_bytes;
                let row_stride = asset.width as usize * 4;
                out.rgba_bytes
                    .par_chunks_mut(row_stride)
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
                                    acc[c] += f32::from(src[idx + c]);
                                }
                            }
                            let inv = 1.0 / (samples as f32);
                            let dst = x * 4;
                            for c in 0..4 {
                                row[dst + c] = (acc[c] * inv).round().clamp(0.0, 255.0) as u8;
                            }
                        }
                    });
            }
        }
        RasterFilter::UnsharpMask {
            amount,
            radius_px,
            threshold,
        } => {
            let amt = sanitize_f32(*amount, 0.0).clamp(0.0, 5.0);
            let blurred = gaussian_blur_rgba(
                &asset.rgba_bytes,
                asset.width,
                asset.height,
                sanitize_f32(*radius_px, 1.5),
            );
            let thresh = f32::from(*threshold);
            for (dst, (orig, blur)) in out.rgba_bytes.as_chunks_mut::<4>().0.iter_mut().zip(
                asset
                    .rgba_bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(blurred.as_chunks::<4>().0.iter()),
            ) {
                for c in 0..3 {
                    let diff = f32::from(orig[c]) - f32::from(blur[c]);
                    if diff.abs() >= thresh {
                        dst[c] = (f32::from(orig[c]) + diff * amt).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
        RasterFilter::Sharpen { amount } => {
            let a = sanitize_f32(*amount, 0.5).clamp(0.0, 5.0);
            let kernel = [0.0, -a, 0.0, -a, 1.0 + 4.0 * a, -a, 0.0, -a, 0.0];
            out.rgba_bytes =
                convolve_3x3(&asset.rgba_bytes, asset.width, asset.height, &kernel, 0.0);
        }
        RasterFilter::Noise {
            amount,
            monochrome,
            seed,
        } => {
            let amt = sanitize_f32(*amount, 0.1).clamp(0.0, 1.0) * 255.0;
            let mono = *monochrome;
            let base_seed = *seed;
            let w = asset.width as usize;
            out.rgba_bytes
                .par_chunks_mut(w * 4)
                .enumerate()
                .for_each(|(y, row)| {
                    for x in 0..w {
                        let h0 = splitmix64(
                            base_seed
                                ^ (y as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
                                ^ (x as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9),
                        );
                        let n0 = ((h0 as f32 / u64::MAX as f32) * 2.0 - 1.0) * amt;
                        let o = x * 4;
                        if mono {
                            for c in 0..3 {
                                row[o + c] =
                                    (f32::from(row[o + c]) + n0).round().clamp(0.0, 255.0) as u8;
                            }
                        } else {
                            let h1 = splitmix64(h0);
                            let h2 = splitmix64(h1);
                            let n1 = ((h1 as f32 / u64::MAX as f32) * 2.0 - 1.0) * amt;
                            let n2 = ((h2 as f32 / u64::MAX as f32) * 2.0 - 1.0) * amt;
                            row[o] = (f32::from(row[o]) + n0).round().clamp(0.0, 255.0) as u8;
                            row[o + 1] =
                                (f32::from(row[o + 1]) + n1).round().clamp(0.0, 255.0) as u8;
                            row[o + 2] =
                                (f32::from(row[o + 2]) + n2).round().clamp(0.0, 255.0) as u8;
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
            let w = asset.width as usize;
            out.rgba_bytes
                .par_chunks_mut(w * 4)
                .enumerate()
                .for_each(|(y, row)| {
                    let dy = (y as f32 + 0.5 - cy) / max_r;
                    for x in 0..w {
                        let dx = (x as f32 + 0.5 - cx) / max_r;
                        let dist = dx.hypot(dy);
                        let t = ((dist - rad) / feath).clamp(0.0, 1.0);
                        let smooth = t * t * (3.0 - 2.0 * t);
                        let factor = (1.0_f32 - str_f * smooth).clamp(0.0, 1.0);
                        let o = x * 4;
                        for c in 0..3 {
                            row[o + c] = (f32::from(row[o + c]) * factor).round() as u8;
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
            out.rgba_bytes =
                convolve_3x3(&asset.rgba_bytes, asset.width, asset.height, &kernel, 128.0);
        }
        RasterFilter::EdgeDetect { sensitivity } => {
            let sens = sanitize_f32(*sensitivity, 1.0).clamp(0.1, 10.0);
            let w = asset.width as usize;
            let h = asset.height as usize;
            let src = &asset.rgba_bytes;
            let luma = |x: i32, y: i32| -> f32 {
                let cx = x.clamp(0, w as i32 - 1) as usize;
                let cy = y.clamp(0, h as i32 - 1) as usize;
                let idx = (cy * w + cx) * 4;
                0.2126 * f32::from(src[idx])
                    + 0.7152 * f32::from(src[idx + 1])
                    + 0.0722 * f32::from(src[idx + 2])
            };
            out.rgba_bytes
                .par_chunks_mut(w * 4)
                .enumerate()
                .for_each(|(y, row)| {
                    let yi = y as i32;
                    for x in 0..w {
                        let xi = x as i32;
                        let gx =
                            -luma(xi - 1, yi - 1) - 2.0 * luma(xi - 1, yi) - luma(xi - 1, yi + 1)
                                + luma(xi + 1, yi - 1)
                                + 2.0 * luma(xi + 1, yi)
                                + luma(xi + 1, yi + 1);
                        let gy =
                            -luma(xi - 1, yi - 1) - 2.0 * luma(xi, yi - 1) - luma(xi + 1, yi - 1)
                                + luma(xi - 1, yi + 1)
                                + 2.0 * luma(xi, yi + 1)
                                + luma(xi + 1, yi + 1);
                        let mag = (gx.hypot(gy) * sens).round().clamp(0.0, 255.0) as u8;
                        let o = x * 4;
                        row[o] = mag;
                        row[o + 1] = mag;
                        row[o + 2] = mag;
                    }
                });
        }
    }

    out
}

/// Resamples a [`RasterAsset`] to `(new_w, new_h)` using high-quality Catmull-Rom bicubic filtering.
pub fn resample(asset: &RasterAsset, new_w: u32, new_h: u32) -> RenderResult<RasterAsset> {
    validate_surface_dimensions(new_w, new_h)?;
    if !is_valid_raster_asset(asset) {
        return Err(RenderError::Raster(
            "source raster dimensions or buffer length invalid".into(),
        ));
    }
    let img = RgbaImage::from_raw(asset.width, asset.height, asset.rgba_bytes.clone())
        .ok_or_else(|| RenderError::Raster("source raster buffer length mismatch".into()))?;
    let resized = image::imageops::resize(&img, new_w, new_h, FilterType::CatmullRom);
    let mut out = RasterAsset::new(
        new_w,
        new_h,
        resized.into_raw(),
        asset.source_dpi,
        asset.color_mode,
    )?;
    out.id = asset.id;
    Ok(out)
}

/// Rasterizes a single scene object (with its full fills, outlines, effects, and text) into a [`RasterAsset`] at `dpi`.
pub fn convert_object_to_bitmap(
    doc: &Document,
    obj_id: ObjectId,
    dpi: f32,
) -> RenderResult<RasterAsset> {
    if !doc.objects.contains_key(&obj_id) {
        return Err(RenderError::InvalidParameter(format!(
            "object {obj_id} not found"
        )));
    }
    let safe_dpi = sanitize_f32(dpi, 300.0).clamp(1.0, 2400.0);
    let bounds = doc.object_bounds(obj_id).ok_or_else(|| {
        RenderError::InvalidParameter(format!("object {obj_id} has empty or non-finite bounds"))
    })?;
    // Pad slightly for strokes/shadows
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
    let asset = RasterAsset::new(width, height, rgba_buf.rgba, safe_dpi, doc.color_mode)?;
    Ok(asset)
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
                let a = f32::from(src[sidx + 3]) / 255.0;
                acc[0] += (f32::from(src[sidx]) * a) * kw;
                acc[1] += (f32::from(src[sidx + 1]) * a) * kw;
                acc[2] += (f32::from(src[sidx + 2]) * a) * kw;
                acc[3] += f32::from(src[sidx + 3]) * kw;
            }
            let o = x * 4;
            row[o..o + 4].copy_from_slice(&acc);
        }
    });

    // Vertical pass + unpremultiply
    let mut dst = vec![0u8; w * h * 4];
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
            let a_u8 = acc[3].round().clamp(0.0, 255.0) as u8;
            let o = x * 4;
            if a_u8 == 0 {
                row[o..o + 4].copy_from_slice(&[0, 0, 0, 0]);
            } else {
                let af = acc[3] / 255.0;
                row[o] = (acc[0] / af).round().clamp(0.0, 255.0) as u8;
                row[o + 1] = (acc[1] / af).round().clamp(0.0, 255.0) as u8;
                row[o + 2] = (acc[2] / af).round().clamp(0.0, 255.0) as u8;
                row[o + 3] = a_u8;
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

fn convolve_3x3(src: &[u8], width: u32, height: u32, kernel: &[f32; 9], bias: f32) -> Vec<u8> {
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
                    acc[0] += f32::from(src[sidx]) * kw;
                    acc[1] += f32::from(src[sidx + 1]) * kw;
                    acc[2] += f32::from(src[sidx + 2]) * kw;
                }
            }
            let o = x * 4;
            row[o] = acc[0].round().clamp(0.0, 255.0) as u8;
            row[o + 1] = acc[1].round().clamp(0.0, 255.0) as u8;
            row[o + 2] = acc[2].round().clamp(0.0, 255.0) as u8;
        }
    });
    dst
}

fn build_tone_curve_lut(points: &[(f32, f32)]) -> [u8; 256] {
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

    let mut lut = [0u8; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let x = i as f32 / 255.0;
        let mut y = x;
        for win in sorted.windows(2) {
            let (x0, y0) = win[0];
            let (x1, y1) = win[1];
            if x >= x0 && x <= x1 {
                let span = x1 - x0;
                y = if span <= 1e-6 {
                    y1
                } else {
                    let t = (x - x0) / span;
                    // Smooth Hermite interpolation between tone control knots
                    let s = t * t * (3.0 - 2.0 * t);
                    y0 + (y1 - y0) * s
                };
                break;
            }
        }
        *slot = (y * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    lut
}

fn map_pixels_parallel<F: Fn(&mut [u8]) + Sync + Send>(rgba: &mut [u8], f: F) {
    rgba.par_chunks_exact_mut(4).for_each(f);
}

#[inline]
fn asset_pixel(asset: &RasterAsset, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y as usize) * (asset.width as usize) + (x as usize)) * 4;
    [
        asset.rgba_bytes[idx],
        asset.rgba_bytes[idx + 1],
        asset.rgba_bytes[idx + 2],
        asset.rgba_bytes[idx + 3],
    ]
}

fn blend_pixel_over(asset: &mut RasterAsset, x: u32, y: u32, sr: f32, sg: f32, sb: f32, sa: f32) {
    let idx = ((y as usize) * (asset.width as usize) + (x as usize)) * 4;
    let dr = f32::from(asset.rgba_bytes[idx]) / 255.0;
    let dg = f32::from(asset.rgba_bytes[idx + 1]) / 255.0;
    let db = f32::from(asset.rgba_bytes[idx + 2]) / 255.0;
    let da = f32::from(asset.rgba_bytes[idx + 3]) / 255.0;

    let out_a = sa + da * (1.0 - sa);
    if out_a <= 1e-6 {
        asset.rgba_bytes[idx..idx + 4].copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    let out_r = (sr * sa + dr * da * (1.0 - sa)) / out_a;
    let out_g = (sg * sa + dg * da * (1.0 - sa)) / out_a;
    let out_b = (sb * sa + db * da * (1.0 - sa)) / out_a;

    asset.rgba_bytes[idx] = (out_r * 255.0).round().clamp(0.0, 255.0) as u8;
    asset.rgba_bytes[idx + 1] = (out_g * 255.0).round().clamp(0.0, 255.0) as u8;
    asset.rgba_bytes[idx + 2] = (out_b * 255.0).round().clamp(0.0, 255.0) as u8;
    asset.rgba_bytes[idx + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
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

fn sample_stroke_dabs(points: &[Point2D], step: f32) -> Vec<Point2D> {
    let finite: Vec<Point2D> = points.iter().copied().filter(|p| p.is_finite()).collect();
    if finite.is_empty() {
        return Vec::new();
    }
    let step_f64 = f64::from(step.max(0.5));
    let mut dabs = Vec::new();
    dabs.push(finite[0]);
    for win in finite.windows(2) {
        if dabs.len() >= MAX_STROKE_DABS {
            break;
        }
        let a = win[0];
        let b = win[1];
        let dist = a.distance_to(b);
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
            dabs.push(a.lerp(b, t));
        }
    }
    dabs
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
