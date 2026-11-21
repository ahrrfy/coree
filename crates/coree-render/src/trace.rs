// SPDX-License-Identifier: MPL-2.0

//! Pure-Rust bitmap-to-vector tracing engine supporting:
//! - [`TraceMode::QuickTrace`]
//! - [`TraceMode::OutlineTrace`] with presets (`LineArt`, `Logo`, `DetailedLogo`, `Clipart`, etc.)
//! - [`TraceMode::CenterlineTrace`] via Zhang-Suen morphological skeletonization for CNC / laser / plotters!
//!
//! Pipeline: K-Means color quantization -> connected component pruning ->
//! Marching Squares contour extraction (or Zhang-Suen thinning) ->
//! Ramer-Douglas-Peucker simplification -> smooth Cubic Bézier curve fitting.

use crate::backend::validate_surface_dimensions;
use coree_core::math::{sanitize_f64, MM_PER_INCH};
use coree_core::{BezierPath, Color, NodeKind, PathNode, Point2D, RasterAsset, Subpath};
use std::collections::{HashMap, HashSet, VecDeque};

/// Preset tuning for [`TraceMode::OutlineTrace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OutlinePreset {
    LineArt,
    #[default]
    Logo,
    DetailedLogo,
    Clipart,
    LowQualityImage,
    HighQualityImage,
}

/// Vectorization strategy for bitmap tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TraceMode {
    #[default]
    QuickTrace,
    OutlineTrace(OutlinePreset),
    /// Morphological thinning / skeletonization producing single-stroke centerlines for CNC/laser/plotters.
    CenterlineTrace,
}

/// Configuration for [`trace_raster`].
#[derive(Debug, Clone, PartialEq)]
pub struct TraceConfig {
    pub mode: TraceMode,
    /// Maximum quantized color regions (`2..=64`).
    pub max_colors: usize,
    /// Curve smoothing strength (`0.0..=1.0`, higher produces smoother Bézier curves).
    pub smoothing: f64,
    /// Detail preservation (`0.0..=1.0`, higher keeps smaller regions and tighter RDP tolerance).
    pub detail: f64,
    /// When `true`, omits the dominant border background color from traced output.
    pub remove_background: bool,
    /// Sharp corner threshold in degrees (`0.0..=180.0`); corners sharper than this stay `NodeKind::Cusp`.
    pub corner_threshold: f64,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            mode: TraceMode::OutlineTrace(OutlinePreset::Logo),
            max_colors: 8,
            smoothing: 0.5,
            detail: 0.75,
            remove_background: true,
            corner_threshold: 60.0,
        }
    }
}

/// Traces `asset` into `(BezierPath, Color)` vector paths scaled into millimeters using `asset.source_dpi`.
#[must_use]
pub fn trace_raster(asset: &RasterAsset, config: &TraceConfig) -> Vec<(BezierPath, Color)> {
    let dpi = f64::from(asset.source_dpi.max(1.0));
    let width_mm = (f64::from(asset.width) / dpi) * MM_PER_INCH;
    let height_mm = (f64::from(asset.height) / dpi) * MM_PER_INCH;
    trace_raster_to_mm(asset, width_mm, height_mm, config)
}

/// Traces `asset` into `(BezierPath, Color)` vector paths mapped cleanly into `[0..width_mm, 0..height_mm]`.
#[must_use]
pub fn trace_raster_to_mm(
    asset: &RasterAsset,
    width_mm: f64,
    height_mm: f64,
    config: &TraceConfig,
) -> Vec<(BezierPath, Color)> {
    if asset.width < 2
        || asset.height < 2
        || validate_surface_dimensions(asset.width, asset.height).is_err()
    {
        return Vec::new();
    }
    let expected_bytes = (asset.width as usize)
        .checked_mul(asset.height as usize)
        .and_then(|n| n.checked_mul(4));
    if expected_bytes != Some(asset.rgba_bytes.len()) {
        return Vec::new();
    }
    let scale_x = sanitize_f64(width_mm, f64::from(asset.width)).max(1e-3) / f64::from(asset.width);
    let scale_y =
        sanitize_f64(height_mm, f64::from(asset.height)).max(1e-3) / f64::from(asset.height);

    match config.mode {
        TraceMode::CenterlineTrace => trace_centerline(asset, config, scale_x, scale_y),
        TraceMode::QuickTrace | TraceMode::OutlineTrace(_) => {
            trace_outline(asset, config, scale_x, scale_y)
        }
    }
}

fn effective_params(config: &TraceConfig) -> (usize, f64, f64, f64, usize) {
    let (preset_max_colors, preset_detail, preset_smooth) = match config.mode {
        TraceMode::QuickTrace => (16, 0.65, 0.5),
        TraceMode::OutlineTrace(OutlinePreset::LineArt) => (2, 0.85, 0.35),
        TraceMode::OutlineTrace(OutlinePreset::Logo) => (12, 0.75, 0.55),
        TraceMode::OutlineTrace(OutlinePreset::DetailedLogo) => (24, 0.9, 0.45),
        TraceMode::OutlineTrace(OutlinePreset::Clipart) => (16, 0.75, 0.6),
        TraceMode::OutlineTrace(OutlinePreset::LowQualityImage) => (12, 0.5, 0.75),
        TraceMode::OutlineTrace(OutlinePreset::HighQualityImage) => (64, 0.9, 0.5),
        TraceMode::CenterlineTrace => (2, 0.8, 0.4),
    };
    let k = config.max_colors.clamp(2, 64).min(preset_max_colors);
    let detail = (0.5 * sanitize_f64(config.detail, preset_detail) + 0.5 * preset_detail)
        .clamp(0.05_f64, 1.0);
    let smooth = (0.5 * sanitize_f64(config.smoothing, preset_smooth) + 0.5 * preset_smooth)
        .clamp(0.0_f64, 1.0);
    let corner_deg = sanitize_f64(config.corner_threshold, 60.0).clamp(5.0_f64, 175.0);
    let min_area_px = ((1.0 - detail) * 16.0).round() as usize + 1;
    (k, detail, smooth, corner_deg, min_area_px)
}

fn trace_outline(
    asset: &RasterAsset,
    config: &TraceConfig,
    scale_x: f64,
    scale_y: f64,
) -> Vec<(BezierPath, Color)> {
    let (k, detail, smooth, corner_deg, min_area_px) = effective_params(config);
    let w = asset.width as usize;
    let h = asset.height as usize;

    let (palette, labels) = quantize_kmeans(asset, k);
    if palette.is_empty() {
        return Vec::new();
    }

    let bg_label = if config.remove_background {
        detect_border_background_label(&labels, w, h)
    } else {
        None
    };

    // Pixel-space RDP tolerance based on detail & smoothing
    let rdp_eps_px = (0.4 + (1.0 - detail) * 2.2 + smooth * 0.6).clamp(0.35, 4.0);

    // Order color layers by pixel area (largest first so smaller details sit on top)
    let mut color_counts: Vec<(usize, usize)> = (0..palette.len())
        .map(|ci| {
            let cnt = labels.iter().filter(|&&l| l == Some(ci)).count();
            (ci, cnt)
        })
        .filter(|&(ci, cnt)| cnt >= min_area_px && Some(ci) != bg_label)
        .collect();
    color_counts.sort_by_key(|a| std::cmp::Reverse(a.1));

    let mut results = Vec::new();
    for (label_idx, _count) in color_counts {
        let mut mask = vec![false; w * h];
        for (i, &l) in labels.iter().enumerate() {
            if l == Some(label_idx) {
                mask[i] = true;
            }
        }
        prune_small_components(&mut mask, w, h, min_area_px);
        let loops = extract_marching_squares_contours(&mask, w, h);
        let mut subpaths = Vec::new();
        for poly in loops {
            if poly.len() < 3 {
                continue;
            }
            let simplified = rdp_simplify_closed(&poly, rdp_eps_px);
            if simplified.len() < 3 {
                continue;
            }
            let scaled: Vec<Point2D> = simplified
                .into_iter()
                .map(|p| Point2D::new(p.x * scale_x, p.y * scale_y))
                .collect();
            let sp = fit_smooth_subpath(&scaled, true, smooth, corner_deg);
            if sp.nodes.len() >= 3 {
                subpaths.push(sp);
            }
        }
        if !subpaths.is_empty() {
            let [r, g, b] = palette[label_idx];
            results.push((BezierPath { subpaths }, Color::rgba8(r, g, b, 255)));
        }
    }
    results
}

/// Centerline Trace using Zhang-Suen iterative morphological thinning/skeletonization.
fn trace_centerline(
    asset: &RasterAsset,
    config: &TraceConfig,
    scale_x: f64,
    scale_y: f64,
) -> Vec<(BezierPath, Color)> {
    let (_, detail, smooth, corner_deg, _) = effective_params(config);
    let w = asset.width as usize;
    let h = asset.height as usize;

    // Identify background RGB from border pixels
    let bg_rgb = estimate_border_rgb(asset);
    let mut fg_mask = vec![false; w * h];
    let mut sum_r = 0u64;
    let mut sum_g = 0u64;
    let mut sum_b = 0u64;
    let mut fg_count = 0u64;

    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let a = asset.rgba_bytes[idx + 3];
            if a < 128 {
                continue;
            }
            let r = asset.rgba_bytes[idx];
            let g = asset.rgba_bytes[idx + 1];
            let b = asset.rgba_bytes[idx + 2];
            let is_fg = if config.remove_background {
                let dr = f32::from(r) - f32::from(bg_rgb[0]);
                let dg = f32::from(g) - f32::from(bg_rgb[1]);
                let db = f32::from(b) - f32::from(bg_rgb[2]);
                dr * dr + dg * dg + db * db > 35.0 * 35.0
            } else {
                let luma = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
                luma < 160.0
            };
            if is_fg {
                fg_mask[y * w + x] = true;
                sum_r += u64::from(r);
                sum_g += u64::from(g);
                sum_b += u64::from(b);
                fg_count += 1;
            }
        }
    }

    if fg_count == 0 {
        return Vec::new();
    }

    let stroke_color = Color::rgba8(
        (sum_r / fg_count) as u8,
        (sum_g / fg_count) as u8,
        (sum_b / fg_count) as u8,
        255,
    );

    // Run Zhang-Suen morphological thinning until 1px skeleton converges
    let skeleton = zhang_suen_thin(&fg_mask, w, h);

    // Extract connected polyline chains from the 1-pixel skeleton
    let chains = extract_skeleton_chains(&skeleton, w, h);
    let rdp_eps_px = (0.5 + (1.0 - detail) * 2.0 + smooth * 0.5).clamp(0.4, 3.5);

    let mut subpaths = Vec::new();
    for (chain, closed) in chains {
        if chain.len() < 2 {
            continue;
        }
        let simplified = if closed && chain.len() >= 3 {
            rdp_simplify_closed(&chain, rdp_eps_px)
        } else {
            rdp_simplify_open(&chain, rdp_eps_px)
        };
        if simplified.len() < 2 {
            continue;
        }
        let scaled: Vec<Point2D> = simplified
            .into_iter()
            .map(|p| Point2D::new((p.x + 0.5) * scale_x, (p.y + 0.5) * scale_y))
            .collect();
        let sp = fit_smooth_subpath(&scaled, closed && scaled.len() >= 3, smooth, corner_deg);
        if sp.nodes.len() >= 2 {
            subpaths.push(sp);
        }
    }

    if subpaths.is_empty() {
        Vec::new()
    } else {
        vec![(BezierPath { subpaths }, stroke_color)]
    }
}

/// Zhang-Suen iterative morphological thinning producing an 8-connected 1-pixel skeleton.
/// Pads the input with a 1-pixel `false` border so strokes touching image edges thin cleanly.
fn zhang_suen_thin(input: &[bool], w: usize, h: usize) -> Vec<bool> {
    if w == 0 || h == 0 || input.len() != w.saturating_mul(h) {
        return Vec::new();
    }
    let pw = w + 2;
    let ph = h + 2;
    let mut img = vec![false; pw * ph];
    for y in 0..h {
        let src_off = y * w;
        let dst_off = (y + 1) * pw + 1;
        img[dst_off..dst_off + w].copy_from_slice(&input[src_off..src_off + w]);
    }

    let mut to_remove = Vec::new();
    for _iter in 0..256 {
        let mut changed = false;
        for step in 0..2 {
            to_remove.clear();
            for y in 1..=h {
                for x in 1..=w {
                    let idx = y * pw + x;
                    if !img[idx] {
                        continue;
                    }
                    // 8-neighbors clockwise from North: P2..P9
                    let p2 = img[(y - 1) * pw + x] as u8;
                    let p3 = img[(y - 1) * pw + (x + 1)] as u8;
                    let p4 = img[y * pw + (x + 1)] as u8;
                    let p5 = img[(y + 1) * pw + (x + 1)] as u8;
                    let p6 = img[(y + 1) * pw + x] as u8;
                    let p7 = img[(y + 1) * pw + (x - 1)] as u8;
                    let p8 = img[y * pw + (x - 1)] as u8;
                    let p9 = img[(y - 1) * pw + (x - 1)] as u8;

                    let b = p2 + p3 + p4 + p5 + p6 + p7 + p8 + p9;
                    if !(2..=6).contains(&b) {
                        continue;
                    }
                    let ring = [p2, p3, p4, p5, p6, p7, p8, p9, p2];
                    let a: u8 = ring
                        .windows(2)
                        .map(|pair| u8::from(pair[0] == 0 && pair[1] == 1))
                        .sum();
                    if a != 1 {
                        continue;
                    }
                    let cond = if step == 0 {
                        (p2 * p4 * p6 == 0) && (p4 * p6 * p8 == 0)
                    } else {
                        (p2 * p4 * p8 == 0) && (p2 * p6 * p8 == 0)
                    };
                    if cond {
                        to_remove.push(idx);
                    }
                }
            }
            if !to_remove.is_empty() {
                changed = true;
                for &idx in &to_remove {
                    img[idx] = false;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut out = vec![false; w * h];
    for y in 0..h {
        let src_off = (y + 1) * pw + 1;
        let dst_off = y * w;
        out[dst_off..dst_off + w].copy_from_slice(&img[src_off..src_off + w]);
    }
    out
}

/// Walks a 1-pixel 8-connected skeleton and extracts open and closed pixel chains.
fn extract_skeleton_chains(skeleton: &[bool], w: usize, h: usize) -> Vec<(Vec<Point2D>, bool)> {
    let mut adj: HashMap<(usize, usize), Vec<(usize, usize)>> = HashMap::new();
    for y in 0..h {
        for x in 0..w {
            if !skeleton[y * w + x] {
                continue;
            }
            let mut nbs = Vec::new();
            for dy in -1_i32..=1 {
                for dx in -1_i32..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx >= 0
                        && ny >= 0
                        && (nx as usize) < w
                        && (ny as usize) < h
                        && skeleton[(ny as usize) * w + (nx as usize)]
                    {
                        nbs.push((nx as usize, ny as usize));
                    }
                }
            }
            // Prefer 4-neighbors over diagonal shortcuts when both exist
            nbs.sort_by_key(|&(nx, ny)| {
                let manhattan = nx.abs_diff(x) + ny.abs_diff(y);
                (manhattan, ny, nx)
            });
            adj.insert((x, y), nbs);
        }
    }

    let mut visited_edges: HashSet<((usize, usize), (usize, usize))> = HashSet::new();
    let edge_key = |a: (usize, usize), b: (usize, usize)| {
        if a <= b {
            (a, b)
        } else {
            (b, a)
        }
    };

    let mut chains: Vec<(Vec<Point2D>, bool)> = Vec::new();

    // First walk starting from endpoints (degree 1) and branch junctions (degree >= 3)
    let mut seeds: Vec<(usize, usize)> = adj
        .iter()
        .filter(|(_, nbs)| nbs.len() != 2)
        .map(|(&pt, _)| pt)
        .collect();
    seeds.sort_unstable();

    for start in seeds {
        let Some(nbs) = adj.get(&start) else {
            continue;
        };
        for &first_next in nbs {
            if visited_edges.contains(&edge_key(start, first_next)) {
                continue;
            }
            let mut chain = vec![Point2D::new(start.0 as f64, start.1 as f64)];
            visited_edges.insert(edge_key(start, first_next));
            let mut cur = first_next;
            chain.push(Point2D::new(cur.0 as f64, cur.1 as f64));
            while let Some(cur_nbs) = adj.get(&cur) {
                if cur_nbs.len() != 2 {
                    break;
                }
                let next_opt = cur_nbs
                    .iter()
                    .copied()
                    .find(|&nb| !visited_edges.contains(&edge_key(cur, nb)));
                let Some(next) = next_opt else {
                    break;
                };
                visited_edges.insert(edge_key(cur, next));
                cur = next;
                chain.push(Point2D::new(cur.0 as f64, cur.1 as f64));
                if cur == start {
                    break;
                }
            }
            let closed = chain.len() >= 4
                && chain
                    .first()
                    .zip(chain.last())
                    .is_some_and(|(a, b)| a.distance_to(*b) <= 1.5);
            if closed {
                chain.pop();
            }
            chains.push((chain, closed));
        }
    }

    // Remaining unvisited edges form pure closed loops (all degree 2)
    let mut all_pts: Vec<(usize, usize)> = adj.keys().copied().collect();
    all_pts.sort_unstable();
    for start in all_pts {
        let Some(nbs) = adj.get(&start) else {
            continue;
        };
        for &first_next in nbs {
            if visited_edges.contains(&edge_key(start, first_next)) {
                continue;
            }
            let mut chain = vec![Point2D::new(start.0 as f64, start.1 as f64)];
            visited_edges.insert(edge_key(start, first_next));
            let mut cur = first_next;
            while cur != start {
                chain.push(Point2D::new(cur.0 as f64, cur.1 as f64));
                let Some(cur_nbs) = adj.get(&cur) else {
                    break;
                };
                let Some(next) = cur_nbs
                    .iter()
                    .copied()
                    .find(|&nb| !visited_edges.contains(&edge_key(cur, nb)))
                else {
                    break;
                };
                visited_edges.insert(edge_key(cur, next));
                cur = next;
            }
            if chain.len() >= 3 {
                chains.push((chain, true));
            }
        }
    }

    chains
}

/// Deterministic K-Means++ RGB color quantization ignoring transparent pixels (`alpha < 128`).
fn quantize_kmeans(asset: &RasterAsset, max_colors: usize) -> (Vec<[u8; 3]>, Vec<Option<usize>>) {
    let n_px = (asset.width as usize) * (asset.height as usize);
    let mut opaque_samples: Vec<[f32; 3]> = Vec::new();
    let stride = (n_px / 8192).max(1);

    for (i, px) in asset.rgba_bytes.as_chunks::<4>().0.iter().enumerate() {
        if i % stride == 0 && px[3] >= 128 {
            opaque_samples.push([f32::from(px[0]), f32::from(px[1]), f32::from(px[2])]);
        }
    }
    if opaque_samples.is_empty() {
        return (Vec::new(), vec![None; n_px]);
    }

    // Collect distinct colors first if fewer than max_colors
    let mut distinct: Vec<[u8; 3]> = Vec::new();
    for s in &opaque_samples {
        let rgb = [s[0] as u8, s[1] as u8, s[2] as u8];
        if !distinct.contains(&rgb) {
            distinct.push(rgb);
            if distinct.len() > max_colors {
                break;
            }
        }
    }

    let mut centroids: Vec<[f32; 3]> = if distinct.len() <= max_colors {
        distinct
            .into_iter()
            .map(|[r, g, b]| [f32::from(r), f32::from(g), f32::from(b)])
            .collect()
    } else {
        // Deterministic farthest-point K-Means++ initialization
        let mut init = vec![opaque_samples[0]];
        while init.len() < max_colors {
            let mut best_dist = -1.0_f32;
            let mut best_sample = opaque_samples[0];
            for &s in &opaque_samples {
                let min_d = init
                    .iter()
                    .map(|c| color_dist_sq(s, *c))
                    .fold(f32::INFINITY, f32::min);
                if min_d > best_dist {
                    best_dist = min_d;
                    best_sample = s;
                }
            }
            if best_dist <= 1.0 {
                break;
            }
            init.push(best_sample);
        }
        init
    };

    // Lloyd iterations on sampled pixels
    for _ in 0..10 {
        let mut sums = vec![[0.0_f64; 3]; centroids.len()];
        let mut counts = vec![0u64; centroids.len()];
        for &s in &opaque_samples {
            let best = nearest_centroid(s, &centroids);
            sums[best][0] += f64::from(s[0]);
            sums[best][1] += f64::from(s[1]);
            sums[best][2] += f64::from(s[2]);
            counts[best] += 1;
        }
        let mut moved = false;
        for i in 0..centroids.len() {
            if counts[i] > 0 {
                let nc = [
                    (sums[i][0] / counts[i] as f64) as f32,
                    (sums[i][1] / counts[i] as f64) as f32,
                    (sums[i][2] / counts[i] as f64) as f32,
                ];
                if color_dist_sq(nc, centroids[i]) > 0.25 {
                    moved = true;
                }
                centroids[i] = nc;
            }
        }
        if !moved {
            break;
        }
    }

    let palette: Vec<[u8; 3]> = centroids
        .iter()
        .map(|c| {
            [
                c[0].round().clamp(0.0, 255.0) as u8,
                c[1].round().clamp(0.0, 255.0) as u8,
                c[2].round().clamp(0.0, 255.0) as u8,
            ]
        })
        .collect();

    let mut labels = vec![None; n_px];
    for (i, px) in asset.rgba_bytes.as_chunks::<4>().0.iter().enumerate() {
        if px[3] >= 128 {
            let s = [f32::from(px[0]), f32::from(px[1]), f32::from(px[2])];
            labels[i] = Some(nearest_centroid(s, &centroids));
        }
    }

    (palette, labels)
}

#[inline]
fn color_dist_sq(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dr = a[0] - b[0];
    let dg = a[1] - b[1];
    let db = a[2] - b[2];
    dr * dr + dg * dg + db * db
}

#[inline]
fn nearest_centroid(sample: [f32; 3], centroids: &[[f32; 3]]) -> usize {
    let mut best_i = 0;
    let mut best_d = f32::INFINITY;
    for (i, &c) in centroids.iter().enumerate() {
        let d = color_dist_sq(sample, c);
        if d < best_d {
            best_d = d;
            best_i = i;
        }
    }
    best_i
}

fn detect_border_background_label(labels: &[Option<usize>], w: usize, h: usize) -> Option<usize> {
    let mut counts: HashMap<usize, usize> = HashMap::new();
    let mut total_border = 0usize;
    let mut visit = |x: usize, y: usize| {
        total_border += 1;
        if let Some(lbl) = labels[y * w + x] {
            *counts.entry(lbl).or_insert(0) += 1;
        }
    };
    for x in 0..w {
        visit(x, 0);
        if h > 1 {
            visit(x, h - 1);
        }
    }
    for y in 1..h.saturating_sub(1) {
        visit(0, y);
        if w > 1 {
            visit(w - 1, y);
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(_, c)| c)
        .filter(|&(_, c)| c * 4 >= total_border) // at least 25% of border pixels
        .map(|(lbl, _)| lbl)
}

/// Estimates the dominant background RGB color from opaque border pixels using
/// quantized mode voting so edge-touching foreground strokes cannot skew the average.
fn estimate_border_rgb(asset: &RasterAsset) -> [u8; 3] {
    let w = asset.width as usize;
    let h = asset.height as usize;
    // Bucket border pixels into 16x16x16 color bins: [count, sum_r, sum_g, sum_b]
    let mut buckets: HashMap<[u8; 3], [u64; 4]> = HashMap::new();
    let mut add = |x: usize, y: usize| {
        let idx = (y * w + x) * 4;
        if asset.rgba_bytes[idx + 3] >= 128 {
            let r = asset.rgba_bytes[idx];
            let g = asset.rgba_bytes[idx + 1];
            let b = asset.rgba_bytes[idx + 2];
            let key = [r >> 4, g >> 4, b >> 4];
            let entry = buckets.entry(key).or_insert([0; 4]);
            entry[0] += 1;
            entry[1] += u64::from(r);
            entry[2] += u64::from(g);
            entry[3] += u64::from(b);
        }
    };
    for x in 0..w {
        add(x, 0);
        if h > 1 {
            add(x, h - 1);
        }
    }
    for y in 1..h.saturating_sub(1) {
        add(0, y);
        if w > 1 {
            add(w - 1, y);
        }
    }
    if let Some((_, [cnt, sr, sg, sb])) = buckets.into_iter().max_by_key(|&(_, v)| v[0]) {
        [
            sr.checked_div(cnt).unwrap_or(255) as u8,
            sg.checked_div(cnt).unwrap_or(255) as u8,
            sb.checked_div(cnt).unwrap_or(255) as u8,
        ]
    } else {
        [255, 255, 255]
    }
}

fn prune_small_components(mask: &mut [bool], w: usize, h: usize, min_area: usize) {
    if min_area <= 1 {
        return;
    }
    let mut visited = vec![false; w * h];
    let mut queue = VecDeque::new();
    let mut comp = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let start = y * w + x;
            if !mask[start] || visited[start] {
                continue;
            }
            visited[start] = true;
            queue.push_back((x, y));
            comp.clear();
            while let Some((cx, cy)) = queue.pop_front() {
                comp.push(cy * w + cx);
                let nbs = [
                    cx.checked_sub(1).map(|nx| (nx, cy)),
                    (cx + 1 < w).then_some((cx + 1, cy)),
                    cy.checked_sub(1).map(|ny| (cx, ny)),
                    (cy + 1 < h).then_some((cx, cy + 1)),
                ];
                for (nx, ny) in nbs.into_iter().flatten() {
                    let ni = ny * w + nx;
                    if mask[ni] && !visited[ni] {
                        visited[ni] = true;
                        queue.push_back((nx, ny));
                    }
                }
            }
            if comp.len() < min_area {
                for &idx in &comp {
                    mask[idx] = false;
                }
            }
        }
    }
}

/// Exact Marching Squares contour extraction for a binary pixel mask `(w x h)`.
///
/// Operates on a 1-pixel zero-padded grid so shapes touching image edges always produce
/// clean closed boundary loops in `[0..w, 0..h]` pixel coordinates. Shared with `smart_fill.rs`.
#[must_use]
pub(crate) fn extract_marching_squares_contours(
    mask: &[bool],
    w: usize,
    h: usize,
) -> Vec<Vec<Point2D>> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    // Evaluate pixel occupancy with 1-pixel zero border around [0..w, 0..h]
    let sample = |gx: i32, gy: i32| -> bool {
        if gx < 0 || gy < 0 || gx >= w as i32 || gy >= h as i32 {
            false
        } else {
            mask[(gy as usize) * w + (gx as usize)]
        }
    };

    // Directed segments keyed by quantized integer half-pixel coordinates (2*x, 2*y)
    // Using half-pixel integer grid avoids all floating-point endpoint mismatch bugs!
    let mut edges: HashMap<(i32, i32), Vec<(i32, i32)>> = HashMap::new();
    let mut add_seg = |a: (i32, i32), b: (i32, i32)| {
        edges.entry(a).or_default().push(b);
    };

    // Cell (cx, cy) spans pixel centers from (cx, cy) to (cx+1, cy+1)
    // In 2x coords: pixel (gx, gy) center is (2*gx + 1, 2*gy + 1).
    // Cell corners: TL=(gx, gy), TR=(gx+1, gy), BR=(gx+1, gy+1), BL=(gx, gy+1)
    // Edge midpoints in 2x coords:
    //   Top    (N): (2*gx + 2, 2*gy + 1)
    //   Right  (E): (2*gx + 3, 2*gy + 2)
    //   Bottom (S): (2*gx + 2, 2*gy + 3)
    //   Left   (W): (2*gx + 1, 2*gy + 2)
    for gy in -1..(h as i32) {
        for gx in -1..(w as i32) {
            let tl = sample(gx, gy) as u8;
            let tr = sample(gx + 1, gy) as u8;
            let br = sample(gx + 1, gy + 1) as u8;
            let bl = sample(gx, gy + 1) as u8;
            let case = (tl << 3) | (tr << 2) | (br << 1) | bl;
            if case == 0 || case == 15 {
                continue;
            }
            let n = (2 * gx + 2, 2 * gy + 1);
            let e = (2 * gx + 3, 2 * gy + 2);
            let s = (2 * gx + 2, 2 * gy + 3);
            let west = (2 * gx + 1, 2 * gy + 2);

            // Consistent CW orientation around filled interior
            match case {
                1 => add_seg(s, west),
                2 => add_seg(e, s),
                3 => add_seg(e, west),
                4 => add_seg(n, e),
                5 => {
                    // Saddle: TL=0, TR=1, BR=0, BL=1
                    add_seg(n, west);
                    add_seg(s, e);
                }
                6 => add_seg(n, s),
                7 => add_seg(n, west),
                8 => add_seg(west, n),
                9 => add_seg(s, n),
                10 => {
                    // Saddle: TL=1, TR=0, BR=1, BL=0
                    add_seg(west, s);
                    add_seg(e, n);
                }
                11 => add_seg(e, n),
                12 => add_seg(west, e),
                13 => add_seg(s, e),
                14 => add_seg(west, s),
                _ => {}
            }
        }
    }

    // Chain directed half-pixel segments into closed loops
    let mut loops = Vec::new();
    let mut keys: Vec<(i32, i32)> = edges.keys().copied().collect();
    keys.sort_unstable();

    for start in keys {
        while let Some(next_first) = edges.get_mut(&start).and_then(Vec::pop) {
            let mut ring = vec![start];
            let mut cur = next_first;
            let mut guard = 0usize;
            while cur != start && guard < 200_000 {
                guard += 1;
                ring.push(cur);
                let Some(next) = edges.get_mut(&cur).and_then(Vec::pop) else {
                    break;
                };
                cur = next;
            }
            if ring.len() >= 3 {
                let pts: Vec<Point2D> = ring
                    .into_iter()
                    .map(|(x2, y2)| {
                        Point2D::new(
                            (f64::from(x2) * 0.5).clamp(0.0, w as f64),
                            (f64::from(y2) * 0.5).clamp(0.0, h as f64),
                        )
                    })
                    .collect();
                loops.push(pts);
            }
        }
    }
    loops
}

/// Iterative stack-based Ramer-Douglas-Peucker simplification for open polylines
/// (zero recursion stack-overflow risk even on very long traced contours).
#[must_use]
pub(crate) fn rdp_simplify_open(pts: &[Point2D], eps: f64) -> Vec<Point2D> {
    if pts.len() <= 2 {
        return pts.to_vec();
    }
    let n = pts.len();
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut stack = vec![(0usize, n - 1)];

    while let Some((start, end)) = stack.pop() {
        if end <= start + 1 {
            continue;
        }
        let a = pts[start];
        let b = pts[end];
        let mut max_d = 0.0_f64;
        let mut idx = start;
        for (i, &p) in pts.iter().enumerate().take(end).skip(start + 1) {
            let d = point_seg_dist(p, a, b);
            if d > max_d {
                max_d = d;
                idx = i;
            }
        }
        if max_d > eps && idx > start && idx < end {
            keep[idx] = true;
            stack.push((start, idx));
            stack.push((idx, end));
        }
    }

    pts.iter()
        .enumerate()
        .filter_map(|(i, &p)| keep[i].then_some(p))
        .collect()
}

/// Ramer-Douglas-Peucker simplification for closed polygon rings.
#[must_use]
pub(crate) fn rdp_simplify_closed(pts: &[Point2D], eps: f64) -> Vec<Point2D> {
    if pts.len() <= 3 {
        return pts.to_vec();
    }
    // Split ring at the two farthest vertices for rotation-invariant simplification
    let mut farthest_idx = 1;
    let mut farthest_d = 0.0;
    for (i, &p) in pts.iter().enumerate().skip(1) {
        let d = pts[0].distance_sq(p);
        if d > farthest_d {
            farthest_d = d;
            farthest_idx = i;
        }
    }
    let first_half = rdp_simplify_open(&pts[..=farthest_idx], eps);
    let mut second_slice = pts[farthest_idx..].to_vec();
    second_slice.push(pts[0]);
    let second_half = rdp_simplify_open(&second_slice, eps);

    let mut out = first_half;
    out.pop();
    out.extend(second_half);
    out.pop();
    if out.len() >= 3 {
        out
    } else {
        pts.to_vec()
    }
}

/// Fits a smooth cubic Bézier [`Subpath`] preserving sharp corners (`< corner_threshold_deg`)
/// as `NodeKind::Cusp` and giving smooth bends Catmull-Rom tangent handles scaled by `smoothing`.
#[must_use]
pub(crate) fn fit_smooth_subpath(
    pts: &[Point2D],
    closed: bool,
    smoothing: f64,
    corner_threshold_deg: f64,
) -> Subpath {
    let n = pts.len();
    if n < 2 {
        return Subpath::from_points(pts.iter().copied(), closed);
    }
    let smooth = smoothing.clamp(0.0, 1.0);
    if smooth <= 0.05 || n < 3 {
        return Subpath::from_points(pts.iter().copied(), closed);
    }
    let corner_rad = corner_threshold_deg.to_radians();
    let tension = 0.16 * smooth;

    let mut nodes = Vec::with_capacity(n);
    for i in 0..n {
        let curr = pts[i];
        if !closed && (i == 0 || i + 1 == n) {
            nodes.push(PathNode::corner(curr));
            continue;
        }
        let prev = pts[(i + n - 1) % n];
        let next = pts[(i + 1) % n];
        let v_in = (curr - prev).normalize_or_zero();
        let v_out = (next - curr).normalize_or_zero();
        let dot = v_in.dot(v_out).clamp(-1.0, 1.0);
        let turn_angle = dot.acos(); // 0 = straight, PI = hairline reversal

        // Preserve sharp corners (e.g., 90° rectangle corners when corner_threshold <= 90°)
        if turn_angle >= corner_rad {
            nodes.push(PathNode::corner(curr));
        } else {
            let d_in = prev.distance_to(curr);
            let d_out = curr.distance_to(next);
            let tangent_dir = (next - prev).normalize_or_zero();
            let h_in = curr - tangent_dir * (d_in * tension);
            let h_out = curr + tangent_dir * (d_out * tension);
            nodes.push(PathNode::new(
                curr,
                Some(h_in),
                Some(h_out),
                NodeKind::Smooth,
            ));
        }
    }
    Subpath { nodes, closed }
}

fn point_seg_dist(p: Point2D, a: Point2D, b: Point2D) -> f64 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= 1e-12 {
        return p.distance_to(a);
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    p.distance_to(a + ab * t)
}
