// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust bitmap-to-vector tracing engine supporting:
//! - [`TraceMode::QuickTrace`]
//! - [`TraceMode::OutlineTrace`] with presets (`LineArt`, `Logo`, `DetailedLogo`, `Clipart`, etc.)
//! - [`TraceMode::CenterlineTrace`] via Zhang-Suen morphological skeletonization for CNC / laser / plotters!
//!
//! Pipeline: >16MP safe downscale -> K-Means++ `Vec<u8>` quantization -> speckle reassignment ->
//! topological shared-edge pixel-grid Potrace (`calc_lon`, `bestpolygon`, `adjust_vertices`,
//! `smooth`, `opticurve`) -> zero-gap shared Cubic Bézier curves.

use crate::backend::validate_surface_dimensions;
use coree_core::math::{sanitize_f64, MM_PER_INCH};
use coree_core::{BezierPath, Color, NodeKind, PathNode, Point2D, RasterAsset, Subpath, Vec2D};
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
    /// Minimum speckle area in pixels (`0` derives automatically from `detail`).
    pub speckle_size: usize,
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
            speckle_size: 0,
        }
    }
}

const MAX_TRACE_DIM: u32 = 4096;
const MAX_TRACE_PIXELS: u64 = 12_000_000;
const EDGE_BUDGET_MIN_PIXELS: usize = 1_000_000;
const MAX_SAFE_BOUNDARY_EDGES: usize = 1_200_000;
const MAX_SAFE_POTRACE_ARCS: usize = 200_000;

/// Downscales oversized rasters (>4096px or >16MP) directly from CoW tiles without
/// materializing a full-res RGBA8 intermediate buffer, preserving aspect ratio and physical mm size.
fn prepare_trace_rgba8(asset: &RasterAsset) -> (u32, u32, Vec<u8>) {
    let w = asset.width;
    let h = asset.height;
    let total = u64::from(w) * u64::from(h);
    if w <= MAX_TRACE_DIM && h <= MAX_TRACE_DIM && total <= MAX_TRACE_PIXELS {
        return (w, h, asset.to_rgba8_vec());
    }
    let scale_dim =
        (f64::from(MAX_TRACE_DIM) / f64::from(w)).min(f64::from(MAX_TRACE_DIM) / f64::from(h));
    let scale_px = (MAX_TRACE_PIXELS as f64 / total as f64).sqrt();
    let s = scale_dim.min(scale_px).clamp(0.01, 1.0);
    let nw = ((f64::from(w) * s).floor() as u32).clamp(2, MAX_TRACE_DIM);
    let nh = ((f64::from(h) * s).floor() as u32).clamp(2, MAX_TRACE_DIM);
    let mut out = vec![0u8; (nw as usize) * (nh as usize) * 4];
    for ny in 0..nh {
        let sy0 = ((u64::from(ny) * u64::from(h)) / u64::from(nh)) as u32;
        let sy1 = (((u64::from(ny) + 1) * u64::from(h)) / u64::from(nh))
            .max(u64::from(sy0) + 1)
            .min(u64::from(h)) as u32;
        let sample_y = sy0 + (sy1 - sy0) / 2;
        for nx in 0..nw {
            let sx0 = ((u64::from(nx) * u64::from(w)) / u64::from(nw)) as u32;
            let sx1 = (((u64::from(nx) + 1) * u64::from(w)) / u64::from(nw))
                .max(u64::from(sx0) + 1)
                .min(u64::from(w)) as u32;
            let sample_x = sx0 + (sx1 - sx0) / 2;
            let px = asset.read_pixel_rgba8(sample_x, sample_y).unwrap_or([0; 4]);
            let dst = ((ny as usize) * (nw as usize) + (nx as usize)) * 4;
            out[dst..dst + 4].copy_from_slice(&px);
        }
    }
    (nw, nh, out)
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
        || !asset.has_valid_layout()
    {
        return Vec::new();
    }
    let w_mm = sanitize_f64(width_mm, f64::from(asset.width)).max(1e-3);
    let h_mm = sanitize_f64(height_mm, f64::from(asset.height)).max(1e-3);
    let (tw, th, rgba8) = prepare_trace_rgba8(asset);
    let scale_x = w_mm / f64::from(tw);
    let scale_y = h_mm / f64::from(th);

    match config.mode {
        TraceMode::CenterlineTrace => {
            trace_centerline_rgba(&rgba8, tw as usize, th as usize, config, scale_x, scale_y)
        }
        TraceMode::QuickTrace | TraceMode::OutlineTrace(_) => {
            trace_outline_potrace(rgba8, tw as usize, th as usize, config, scale_x, scale_y)
        }
    }
}

fn effective_params(config: &TraceConfig) -> (usize, f64, f64, f64, usize) {
    let (preset_detail, preset_smooth) = match config.mode {
        TraceMode::QuickTrace => (0.80, 0.50),
        TraceMode::OutlineTrace(OutlinePreset::LineArt) => (0.90, 0.35),
        TraceMode::OutlineTrace(OutlinePreset::Logo) => (0.82, 0.50),
        TraceMode::OutlineTrace(OutlinePreset::DetailedLogo) => (0.92, 0.42),
        TraceMode::OutlineTrace(OutlinePreset::Clipart) => (0.80, 0.55),
        TraceMode::OutlineTrace(OutlinePreset::LowQualityImage) => (0.60, 0.72),
        TraceMode::OutlineTrace(OutlinePreset::HighQualityImage) => (0.92, 0.48),
        TraceMode::CenterlineTrace => (0.85, 0.40),
    };
    let k = match config.mode {
        TraceMode::CenterlineTrace | TraceMode::OutlineTrace(OutlinePreset::LineArt) => 2,
        _ => config.max_colors.clamp(2, 64),
    };
    let detail = sanitize_f64(config.detail, preset_detail).clamp(0.05_f64, 1.0);
    let smooth = sanitize_f64(config.smoothing, preset_smooth).clamp(0.0_f64, 1.0);
    let corner_deg = sanitize_f64(config.corner_threshold, 60.0).clamp(5.0_f64, 175.0);
    let min_area_px = if config.speckle_size > 0 {
        config.speckle_size
    } else {
        ((1.0 - detail) * 12.0).round() as usize + 1
    };
    (k, detail, smooth, corner_deg, min_area_px)
}

/// Topological shared-edge Potrace outline vectorizer:
/// builds a planar pixel-grid label boundary graph, traces each canonical arc/loop once
/// through Potrace (`calc_lon` -> `bestpolygon` -> `adjust_vertices` -> `smooth` -> `opticurve`),
/// and stitches reversed shared Bézier chains per color region so adjacent shapes share exact
/// zero-gap boundaries with 0 white seam pixels.
fn trace_outline_potrace(
    rgba8: Vec<u8>,
    mut w: usize,
    mut h: usize,
    config: &TraceConfig,
    mut scale_x: f64,
    mut scale_y: f64,
) -> Vec<(BezierPath, Color)> {
    let (k, detail, smooth, corner_deg, turd_size) = effective_params(config);
    let allow_bimodal_lineart = !matches!(
        config.mode,
        TraceMode::OutlineTrace(OutlinePreset::LowQualityImage | OutlinePreset::HighQualityImage)
    );
    let orig_w = w;
    let orig_h = h;
    let bg_rgb_est = estimate_border_rgb_rgba(&rgba8, w, h);
    let (palette, mut labels) = quantize_kmeans_u8(&rgba8, w, h, k, allow_bimodal_lineart);
    if palette.is_empty() {
        return Vec::new();
    }
    suppress_antialias_halos(&rgba8, &palette, &mut labels, w, h);

    if config.remove_background {
        if let Some(bg_lbl) = detect_border_background_label_u8(&labels, w, h, palette.len()) {
            for l in &mut labels {
                if *l == bg_lbl {
                    *l = u8::MAX;
                }
            }
        }
    }

    prune_speckles_reassign(&mut labels, w, h, turd_size);
    prune_sideways_1px_burrs(&mut labels, w, h);
    enforce_trace_edge_budget(&mut labels, &mut w, &mut h, &mut scale_x, &mut scale_y);

    // Count area per label after background removal, speckle reassignment, and edge-budget pass
    let mut color_counts = vec![0usize; palette.len()];
    for &l in &labels {
        if (l as usize) < palette.len() {
            color_counts[l as usize] += 1;
        }
    }

    let potrace_alphamax = potrace_alphamax_from_params(corner_deg, smooth);
    let potrace_opttol = 0.32 + smooth * 0.58 + (1.0 - detail) * 0.30;
    let subpixel_ctx = if w == orig_w && h == orig_h {
        Some(SubpixelCtx {
            rgba8: &rgba8,
            w,
            h,
            palette: &palette,
            bg_rgb: bg_rgb_est,
        })
    } else {
        None
    };
    let region_subpaths = potrace_extract_shared_subpaths_with_subpixel(
        &labels,
        w,
        h,
        palette.len(),
        potrace_alphamax,
        potrace_opttol,
        smooth,
        corner_deg,
        scale_x,
        scale_y,
        subpixel_ctx,
    );
    drop(rgba8);
    drop(labels);

    let mut ordered: Vec<(usize, usize)> = color_counts
        .iter()
        .enumerate()
        .filter(|&(_, &cnt)| cnt > 0)
        .map(|(i, &cnt)| (i, cnt))
        .collect();
    ordered.sort_by_key(|a| std::cmp::Reverse(a.1));

    let mut results = Vec::new();
    for (label_idx, _) in ordered {
        if let Some(subpaths) = region_subpaths.get(&label_idx) {
            if !subpaths.is_empty() {
                let [r, g, b] = palette[label_idx];
                results.push((
                    BezierPath {
                        subpaths: subpaths.clone(),
                    },
                    Color::rgba8(r, g, b, 255),
                ));
            }
        }
    }
    results
}

fn potrace_alphamax_from_params(corner_deg: f64, smooth: f64) -> f64 {
    if smooth <= 0.02 {
        return 0.0;
    }
    // Maps default (60 deg, smooth=0.5) to canonical Potrace alphamax ~= 1.0,
    // while keeping circles smooth even at lower CLI corner thresholds.
    let base = 0.65 + (corner_deg / 180.0) * 0.65;
    (base * (0.85 + 0.60 * smooth)).clamp(0.55, 1.3333)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArcKind {
    ClosedLoop,
    OpenArc,
}

#[derive(Debug, Clone, Copy)]
struct PotraceChain<'a> {
    pts: &'a [(i32, i32)],
    kind: ArcKind,
}

#[derive(Debug, Clone)]
struct TracedBoundarySeg {
    left_label: u8,
    right_label: u8,
    start_v: u32,
    end_v: u32,
    /// Exact orthogonal grid unit step leaving `start_v` (`pts[0] -> pts[1]`).
    start_step: (i8, i8),
    /// Exact orthogonal grid unit step arriving at `end_v` (`pts[len - 2] -> pts[len - 1]`).
    end_step: (i8, i8),
    /// Nodes along `start_v -> end_v`, with `start_v` (first) and `end_v` (last) pinned endpoints.
    nodes: Vec<PathNode>,
}

/// Computes `(boundary_edge_count, open_arc_estimate)` over quantized `labels` in `O(1)` extra memory.
fn estimate_label_boundary_complexity(labels: &[u8], w: usize, h: usize) -> (usize, usize) {
    if w == 0 || h == 0 || labels.len() < w * h {
        return (0, 0);
    }
    let lbl_at = |px: isize, py: isize| -> u8 {
        if px < 0 || py < 0 || px >= w as isize || py >= h as isize {
            u8::MAX
        } else {
            labels[(py as usize) * w + (px as usize)]
        }
    };
    let mut edge_count = 0usize;
    let mut junction_half_arcs = 0usize;
    for gy in 0..=h {
        let gy_i = gy as isize;
        for gx in 0..=w {
            let gx_i = gx as isize;
            let tl = lbl_at(gx_i - 1, gy_i - 1);
            let tr = lbl_at(gx_i, gy_i - 1);
            let bl = lbl_at(gx_i - 1, gy_i);
            let br = lbl_at(gx_i, gy_i);
            let has_e = u8::from(tr != br);
            let has_s = u8::from(br != bl);
            let has_w = u8::from(bl != tl);
            let has_n = u8::from(tl != tr);
            edge_count += usize::from(has_e + has_s);
            let deg = has_e + has_s + has_w + has_n;
            if deg != 0 && deg != 2 {
                junction_half_arcs += usize::from(deg);
            }
        }
    }
    (edge_count, junction_half_arcs / 2)
}

/// Deterministic mode/center downsample of quantized `u8` label grids using `O(1)` scratch per pixel.
fn downsample_labels_mode(labels: &[u8], w: usize, h: usize, nw: usize, nh: usize) -> Vec<u8> {
    let mut out = vec![u8::MAX; nw * nh];
    for ny in 0..nh {
        let sy0 = (ny * h) / nh;
        let sy1 = (((ny + 1) * h) / nh).max(sy0 + 1).min(h);
        let mid_y = sy0 + (sy1 - sy0) / 2;
        for nx in 0..nw {
            let sx0 = (nx * w) / nw;
            let sx1 = (((nx + 1) * w) / nw).max(sx0 + 1).min(w);
            let mid_x = sx0 + (sx1 - sx0) / 2;
            let center_lbl = labels[mid_y * w + mid_x];
            if (sx1 - sx0) == 1 && (sy1 - sy0) == 1 {
                out[ny * nw + nx] = center_lbl;
                continue;
            }
            let mut counts: [(u8, u16); 16] = [(u8::MAX, 0); 16];
            let mut used_slots = 0usize;
            for sy in sy0..sy1 {
                let row = sy * w;
                for sx in sx0..sx1 {
                    let lbl = labels[row + sx];
                    let mut found = false;
                    for slot in &mut counts[..used_slots] {
                        if slot.0 == lbl {
                            slot.1 = slot.1.saturating_add(1);
                            found = true;
                            break;
                        }
                    }
                    if !found && used_slots < counts.len() {
                        counts[used_slots] = (lbl, 1);
                        used_slots += 1;
                    }
                }
            }
            let mut best_lbl = center_lbl;
            let mut best_cnt = 0u16;
            for &(lbl, cnt) in &counts[..used_slots] {
                if cnt > best_cnt || (cnt == best_cnt && lbl == center_lbl) {
                    best_cnt = cnt;
                    best_lbl = lbl;
                }
            }
            out[ny * nw + nx] = best_lbl;
        }
    }
    out
}

/// Guards high-pixel (>1MP) dense textures/checkerboards so Potrace stays comfortably within
/// a 512 MiB address-space budget without altering <=1MP or low-edge >1MP artwork.
fn enforce_trace_edge_budget(
    labels: &mut Vec<u8>,
    w: &mut usize,
    h: &mut usize,
    scale_x: &mut f64,
    scale_y: &mut f64,
) {
    if (*w) * (*h) <= EDGE_BUDGET_MIN_PIXELS {
        return;
    }
    while *w > 256 || *h > 256 {
        let (edge_count, open_arcs) = estimate_label_boundary_complexity(labels, *w, *h);
        if edge_count <= MAX_SAFE_BOUNDARY_EDGES && open_arcs <= MAX_SAFE_POTRACE_ARCS {
            break;
        }
        let nw = (*w / 2).max(256).min(*w);
        let nh = (*h / 2).max(256).min(*h);
        if nw == *w && nh == *h {
            break;
        }
        let next_labels = downsample_labels_mode(labels, *w, *h, nw, nh);
        *scale_x = ((*w as f64) * *scale_x) / (nw as f64);
        *scale_y = ((*h as f64) * *scale_y) / (nh as f64);
        *w = nw;
        *h = nh;
        *labels = next_labels;
    }
}

#[derive(Clone, Copy)]
struct SubpixelCtx<'a> {
    rgba8: &'a [u8],
    w: usize,
    h: usize,
    palette: &'a [[u8; 3]],
    bg_rgb: [u8; 3],
}

/// Topological shared-edge Potrace boundary extractor.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn potrace_extract_shared_subpaths(
    labels: &[u8],
    w: usize,
    h: usize,
    palette_len: usize,
    alphamax: f64,
    opttol: f64,
    smooth: f64,
    corner_deg: f64,
    scale_x: f64,
    scale_y: f64,
) -> HashMap<usize, Vec<Subpath>> {
    potrace_extract_shared_subpaths_with_subpixel(
        labels,
        w,
        h,
        palette_len,
        alphamax,
        opttol,
        smooth,
        corner_deg,
        scale_x,
        scale_y,
        None,
    )
}

/// Topological shared-edge Potrace boundary extractor with optional sub-pixel `rgba8` iso-contouring.
/// Uses a 1-byte-per-vertex packed bitmask over `(w + 1) * (h + 1)` vertices (no per-vertex
/// `HashMap` or `UndirEdge` heap bloat), traces every boundary arc between `(left_label, right_label)`
/// ONCE by Potrace + Taubin-Schneider, and shares it identically (forward for `left_label`, reversed for `right_label`).
#[allow(clippy::too_many_arguments)]
fn potrace_extract_shared_subpaths_with_subpixel(
    labels: &[u8],
    w: usize,
    h: usize,
    palette_len: usize,
    alphamax: f64,
    opttol: f64,
    smooth: f64,
    corner_deg: f64,
    scale_x: f64,
    scale_y: f64,
    subpixel: Option<SubpixelCtx<'_>>,
) -> HashMap<usize, Vec<Subpath>> {
    const BIT_E: u8 = 1 << 0;
    const BIT_S: u8 = 1 << 1;
    const BIT_W: u8 = 1 << 2;
    const BIT_N: u8 = 1 << 3;

    let gw = w + 1;
    let gh = h + 1;
    let lbl_at = |px: isize, py: isize| -> u8 {
        if px < 0 || py < 0 || px >= w as isize || py >= h as isize {
            u8::MAX
        } else {
            labels[(py as usize) * w + (px as usize)]
        }
    };
    let vid = |gx: usize, gy: usize| -> usize { gy * gw + gx };
    let vcoords = |v: usize| -> (i32, i32) { ((v % gw) as i32, (v / gw) as i32) };
    let opp_bit = |bit: u8| -> u8 {
        match bit {
            BIT_E => BIT_W,
            BIT_W => BIT_E,
            BIT_S => BIT_N,
            BIT_N => BIT_S,
            _ => 0,
        }
    };
    let step_vertex = |v: usize, bit: u8| -> usize {
        match bit {
            BIT_E => v + 1,
            BIT_S => v + gw,
            BIT_W => v - 1,
            BIT_N => v - gw,
            _ => v,
        }
    };
    let edge_labels = |v: usize, bit: u8| -> (u8, u8) {
        let gx = (v % gw) as isize;
        let gy = (v / gw) as isize;
        match bit {
            BIT_E => (lbl_at(gx, gy - 1), lbl_at(gx, gy)),
            BIT_S => (lbl_at(gx, gy), lbl_at(gx - 1, gy)),
            BIT_W => (lbl_at(gx - 1, gy), lbl_at(gx - 1, gy - 1)),
            BIT_N => (lbl_at(gx - 1, gy - 1), lbl_at(gx, gy - 1)),
            _ => (u8::MAX, u8::MAX),
        }
    };

    // Lower 4 bits: unused incident edge mask; upper 4 bits: original incident edge mask.
    let mut vert_mask: Vec<u8> = vec![0u8; gw * gh];
    let mut total_edges = 0usize;

    // Horizontal grid edges: (gx, gy) -> (gx+1, gy)
    for gy in 0..=h {
        for gx in 0..w {
            let lu = lbl_at(gx as isize, gy as isize - 1);
            let ru = lbl_at(gx as isize, gy as isize);
            if lu != ru {
                let u = vid(gx, gy);
                let v = u + 1;
                vert_mask[u] |= BIT_E | (BIT_E << 4);
                vert_mask[v] |= BIT_W | (BIT_W << 4);
                total_edges += 1;
            }
        }
    }
    // Vertical grid edges: (gx, gy) -> (gx, gy+1)
    for gy in 0..h {
        for gx in 0..=w {
            let lu = lbl_at(gx as isize, gy as isize);
            let ru = lbl_at(gx as isize - 1, gy as isize);
            if lu != ru {
                let u = vid(gx, gy);
                let v = u + gw;
                vert_mask[u] |= BIT_S | (BIT_S << 4);
                vert_mask[v] |= BIT_N | (BIT_N << 4);
                total_edges += 1;
            }
        }
    }

    if total_edges == 0 {
        return HashMap::new();
    }

    let num_labels = palette_len.max(1);
    let mut per_label_loops: Vec<Vec<Subpath>> = vec![Vec::new(); num_labels];
    let mut traced_segs: Vec<TracedBoundarySeg> = Vec::new();
    let mut pts: Vec<(i32, i32)> = Vec::new();

    // 1. Extract all open arcs starting/ending at junction vertices (original degree != 2)
    for start_v in 0..(gw * gh) {
        let orig = vert_mask[start_v] >> 4;
        if orig == 0 || orig.count_ones() == 2 {
            continue;
        }
        for bit in [BIT_W, BIT_E, BIT_N, BIT_S] {
            if (vert_mask[start_v] & bit) == 0 {
                continue;
            }
            vert_mask[start_v] &= !bit;
            let next_v = step_vertex(start_v, bit);
            vert_mask[next_v] &= !opp_bit(bit);
            let (left_lbl, right_lbl) = edge_labels(start_v, bit);

            pts.clear();
            pts.push(vcoords(start_v));
            pts.push(vcoords(next_v));
            let mut cur_v = next_v;
            let mut guard = 0usize;
            while (vert_mask[cur_v] >> 4).count_ones() == 2 && guard < total_edges + 4 {
                guard += 1;
                let rem = vert_mask[cur_v] & 0x0F;
                if rem == 0 {
                    break;
                }
                let nbit = 1u8 << rem.trailing_zeros();
                vert_mask[cur_v] &= !nbit;
                let nv = step_vertex(cur_v, nbit);
                vert_mask[nv] &= !opp_bit(nbit);
                pts.push(vcoords(nv));
                cur_v = nv;
            }
            let start_step = grid_unit_step(pts[0], pts[1]);
            let end_step = grid_unit_step(pts[pts.len() - 2], pts[pts.len() - 1]);
            let chain = PotraceChain {
                pts: &pts,
                kind: ArcKind::OpenArc,
            };
            let nodes = potrace(
                &chain, left_lbl, right_lbl, subpixel, alphamax, opttol, smooth, corner_deg,
                scale_x, scale_y,
            );
            traced_segs.push(TracedBoundarySeg {
                left_label: left_lbl,
                right_label: right_lbl,
                start_v: start_v as u32,
                end_v: cur_v as u32,
                start_step,
                end_step,
                nodes,
            });
        }
    }

    // 2. Extract all remaining pure degree-2 closed loops directly into `per_label_loops`
    for start_v in 0..(gw * gh) {
        if (vert_mask[start_v] & 0x0F) == 0 {
            continue;
        }
        for bit in [BIT_E, BIT_S, BIT_W, BIT_N] {
            if (vert_mask[start_v] & bit) == 0 {
                continue;
            }
            vert_mask[start_v] &= !bit;
            let mut cur_v = step_vertex(start_v, bit);
            vert_mask[cur_v] &= !opp_bit(bit);
            let (left_lbl, right_lbl) = edge_labels(start_v, bit);

            pts.clear();
            pts.push(vcoords(start_v));
            pts.push(vcoords(cur_v));
            let mut guard = 0usize;
            while cur_v != start_v && guard < total_edges + 4 {
                guard += 1;
                let rem = vert_mask[cur_v] & 0x0F;
                if rem == 0 {
                    break;
                }
                let nbit = 1u8 << rem.trailing_zeros();
                vert_mask[cur_v] &= !nbit;
                let nv = step_vertex(cur_v, nbit);
                vert_mask[nv] &= !opp_bit(nbit);
                if nv != start_v {
                    pts.push(vcoords(nv));
                }
                cur_v = nv;
            }
            if pts.len() >= 4 {
                let chain = PotraceChain {
                    pts: &pts,
                    kind: ArcKind::ClosedLoop,
                };
                let nodes = potrace(
                    &chain, left_lbl, right_lbl, subpixel, alphamax, opttol, smooth, corner_deg,
                    scale_x, scale_y,
                );
                if nodes.len() >= 2 {
                    if (left_lbl as usize) < num_labels {
                        per_label_loops[left_lbl as usize].push(Subpath {
                            nodes: nodes.clone(),
                            closed: true,
                        });
                    }
                    if (right_lbl as usize) < num_labels {
                        per_label_loops[right_lbl as usize].push(Subpath {
                            nodes: reverse_nodes(&nodes),
                            closed: true,
                        });
                    }
                }
            }
        }
    }
    drop(pts);
    drop(vert_mask);

    // 3. Assemble closed Subpaths for every foreground label from shared OpenArc TracedBoundarySegs
    #[derive(Clone, Copy)]
    struct HalfSeg {
        seg_idx: u32,
        from_v: u32,
        to_v: u32,
        in_dir: (i8, i8),
        out_dir: (i8, i8),
        forward: bool,
        used: bool,
    }

    let mut by_label: Vec<Vec<HalfSeg>> = vec![Vec::new(); num_labels];
    for (seg_idx, seg) in traced_segs.iter().enumerate() {
        if seg.nodes.len() < 2 {
            continue;
        }
        let out_fwd = seg.start_step;
        let in_fwd = seg.end_step;

        if (seg.left_label as usize) < num_labels {
            by_label[seg.left_label as usize].push(HalfSeg {
                seg_idx: seg_idx as u32,
                from_v: seg.start_v,
                to_v: seg.end_v,
                in_dir: out_fwd,
                out_dir: in_fwd,
                forward: true,
                used: false,
            });
        }
        if (seg.right_label as usize) < num_labels {
            by_label[seg.right_label as usize].push(HalfSeg {
                seg_idx: seg_idx as u32,
                from_v: seg.end_v,
                to_v: seg.start_v,
                in_dir: (-in_fwd.0, -in_fwd.1),
                out_dir: (-out_fwd.0, -out_fwd.1),
                forward: false,
                used: false,
            });
        }
    }

    for (lbl, mut hsegs) in by_label.into_iter().enumerate() {
        if hsegs.is_empty() {
            continue;
        }
        let mut adj: Vec<(u32, u32)> = hsegs
            .iter()
            .enumerate()
            .map(|(i, hs)| (hs.from_v, i as u32))
            .collect();
        adj.sort_unstable_by_key(|&(v, _)| v);

        for start_i in 0..hsegs.len() {
            if hsegs[start_i].used {
                continue;
            }
            hsegs[start_i].used = true;
            let start_v = hsegs[start_i].from_v;
            let mut chain_indices = vec![(hsegs[start_i].seg_idx, hsegs[start_i].forward)];
            let mut cur_v = hsegs[start_i].to_v;
            let mut cur_dir = hsegs[start_i].out_dir;
            let mut guard = 0usize;
            while cur_v != start_v && guard < hsegs.len() + 2 {
                guard += 1;
                let pos = adj.partition_point(|&(v, _)| v < cur_v);
                if pos >= adj.len() || adj[pos].0 != cur_v {
                    break;
                }
                // Pick sharpest LEFT turn in screen coords so diagonal touchers stay simple & non-crossing
                let mut best_cand: Option<(usize, i32)> = None;
                for &(v, ci_u32) in &adj[pos..] {
                    if v != cur_v {
                        break;
                    }
                    let ci = ci_u32 as usize;
                    if hsegs[ci].used {
                        continue;
                    }
                    let prio = left_turn_priority(cur_dir, hsegs[ci].in_dir);
                    if best_cand.is_none_or(|(_, bp)| prio < bp) {
                        best_cand = Some((ci, prio));
                    }
                }
                let Some((chosen, _)) = best_cand else {
                    break;
                };
                hsegs[chosen].used = true;
                chain_indices.push((hsegs[chosen].seg_idx, hsegs[chosen].forward));
                cur_v = hsegs[chosen].to_v;
                cur_dir = hsegs[chosen].out_dir;
            }
            if cur_v != start_v {
                continue;
            }

            let mut stitched: Vec<PathNode> = Vec::new();
            for (seg_idx, forward) in chain_indices {
                let seg_nodes = &traced_segs[seg_idx as usize].nodes;
                if forward {
                    if stitched.is_empty() {
                        stitched.extend_from_slice(seg_nodes);
                    } else if let Some(first) = seg_nodes.first() {
                        if let Some(last) = stitched.last_mut() {
                            last.handle_out = first.handle_out;
                            last.kind = NodeKind::Cusp;
                        }
                        stitched.extend(seg_nodes.iter().skip(1).cloned());
                    }
                } else {
                    let rev = reverse_nodes(seg_nodes);
                    if stitched.is_empty() {
                        stitched.extend(rev);
                    } else if let Some(first) = rev.first() {
                        if let Some(last) = stitched.last_mut() {
                            last.handle_out = first.handle_out;
                            last.kind = NodeKind::Cusp;
                        }
                        stitched.extend(rev.into_iter().skip(1));
                    }
                }
            }
            if stitched.len() >= 3 {
                // Close back onto stitched[0] cleanly
                if let Some(last) = stitched.pop() {
                    stitched[0].handle_in = last.handle_in;
                    stitched[0].kind = NodeKind::Cusp;
                }
                if stitched.len() >= 3 {
                    per_label_loops[lbl].push(Subpath {
                        nodes: stitched,
                        closed: true,
                    });
                }
            }
        }
    }

    let mut out = HashMap::new();
    for (lbl, subpaths) in per_label_loops.into_iter().enumerate() {
        if !subpaths.is_empty() {
            out.insert(lbl, subpaths);
        }
    }
    out
}

#[inline]
fn grid_unit_step(a: (i32, i32), b: (i32, i32)) -> (i8, i8) {
    ((b.0 - a.0).signum() as i8, (b.1 - a.1).signum() as i8)
}

/// Priority for sharpest LEFT turn in screen coords (`+X right, +Y down`):
/// left (-90 deg) = 0, straight = 1, right (+90 deg) = 2, u-turn = 3.
fn left_turn_priority(d_in: (i8, i8), d_out: (i8, i8)) -> i32 {
    let left = (d_in.1, -d_in.0);
    let right = (-d_in.1, d_in.0);
    if d_out == left {
        0
    } else if d_out == d_in {
        1
    } else if d_out == right {
        2
    } else {
        3
    }
}

fn reverse_nodes(nodes: &[PathNode]) -> Vec<PathNode> {
    nodes
        .iter()
        .rev()
        .map(|n| PathNode::new(n.pos, n.handle_out, n.handle_in, n.kind))
        .collect()
}

/// Peter Selinger's Potrace polygon + Taubin shrinkage-free fairing + Schneider cubic Bézier fit.
#[allow(clippy::too_many_arguments)]
fn potrace(
    chain: &PotraceChain<'_>,
    left_lbl: u8,
    right_lbl: u8,
    subpixel: Option<SubpixelCtx<'_>>,
    alphamax: f64,
    opttol: f64,
    smooth: f64,
    corner_deg: f64,
    scale_x: f64,
    scale_y: f64,
) -> Vec<PathNode> {
    potrace_vectorize_chain(
        chain, left_lbl, right_lbl, subpixel, alphamax, opttol, smooth, corner_deg, scale_x,
        scale_y,
    )
}

#[allow(clippy::too_many_arguments)]
fn potrace_vectorize_chain(
    chain: &PotraceChain<'_>,
    left_lbl: u8,
    right_lbl: u8,
    subpixel: Option<SubpixelCtx<'_>>,
    alphamax: f64,
    opttol: f64,
    smooth: f64,
    corner_deg: f64,
    scale_x: f64,
    scale_y: f64,
) -> Vec<PathNode> {
    let m = chain.pts.len();
    if m < 2 {
        return Vec::new();
    }
    let closed = chain.kind == ArcKind::ClosedLoop;
    let sums = potrace_calc_sums(chain.pts, closed);
    let lon = potrace_calc_lon(chain.pts, closed);
    let po = potrace_bestpolygon(chain.pts, &sums, &lon, closed);
    let vertices = potrace_adjust_vertices(chain.pts, &sums, &po, closed);
    let mut nodes = potrace_taubin_schneider_fit(
        chain.pts, &po, &vertices, closed, left_lbl, right_lbl, subpixel, alphamax, opttol, smooth,
        corner_deg, scale_x, scale_y,
    );
    if nodes.len() < 2 {
        nodes = chain
            .pts
            .iter()
            .map(|&(x, y)| {
                PathNode::corner(Point2D::new(f64::from(x) * scale_x, f64::from(y) * scale_y))
            })
            .collect();
    }
    nodes
}

#[derive(Default, Clone, Copy)]
struct PotraceSum {
    x: f64,
    y: f64,
    x2: f64,
    xy: f64,
    y2: f64,
}

fn potrace_calc_sums(pts: &[(i32, i32)], closed: bool) -> Vec<PotraceSum> {
    let n = pts.len();
    let total = if closed { n * 2 + 1 } else { n + 1 };
    let mut sums = vec![PotraceSum::default(); total];
    let x0 = f64::from(pts[0].0);
    let y0 = f64::from(pts[0].1);
    for i in 1..total {
        let idx = if closed { (i - 1) % n } else { i - 1 };
        let px = f64::from(pts[idx].0) - x0;
        let py = f64::from(pts[idx].1) - y0;
        let prev = sums[i - 1];
        sums[i] = PotraceSum {
            x: prev.x + px,
            y: prev.y + py,
            x2: prev.x2 + px * px,
            xy: prev.xy + px * py,
            y2: prev.y2 + py * py,
        };
    }
    sums
}

fn potrace_penalty3(pts: &[(i32, i32)], sums: &[PotraceSum], i: usize, j: usize) -> f64 {
    let n = pts.len();
    let k = j - i;
    if k <= 1 {
        return 0.0;
    }
    let s = PotraceSum {
        x: sums[j + 1].x - sums[i].x,
        y: sums[j + 1].y - sums[i].y,
        x2: sums[j + 1].x2 - sums[i].x2,
        xy: sums[j + 1].xy - sums[i].xy,
        y2: sums[j + 1].y2 - sums[i].y2,
    };
    let x0 = f64::from(pts[0].0);
    let y0 = f64::from(pts[0].1);
    let pi = pts[i % n];
    let pj = pts[j % n];
    let px = (f64::from(pi.0) + f64::from(pj.0)) * 0.5 - x0;
    let py = (f64::from(pi.1) + f64::from(pj.1)) * 0.5 - y0;
    let ey = f64::from(pj.0 - pi.0);
    let ex = -f64::from(pj.1 - pi.1);
    let len_sq = ex * ex + ey * ey;
    if len_sq <= 1e-9 {
        return 1e9;
    }
    let kf = (k + 1) as f64;
    let a = (s.x2 - 2.0 * s.x * px) / kf + px * px;
    let b = (s.xy - s.x * py - s.y * px) / kf + px * py;
    let c = (s.y2 - 2.0 * s.y * py) / kf + py * py;
    let s_val = ex * ex * a + 2.0 * ex * ey * b + ey * ey * c;
    s_val.max(0.0).sqrt()
}

fn potrace_calc_lon(pts: &[(i32, i32)], closed: bool) -> Vec<usize> {
    let n = pts.len();
    if n < 2 {
        return vec![0; n];
    }
    let mut lon = vec![0usize; n];
    let limit = if closed { n } else { n - 1 };

    for i in 0..limit {
        let p0 = pts[i];
        let mut dir_mask = 0u8;
        let mut c0 = (0i64, 0i64);
        let mut c1 = (0i64, 0i64);
        let mut last_valid = i + 1;
        let max_j = if closed {
            (i + n).min(i + 2048)
        } else {
            (n - 1).min(i + 2048)
        };

        let mut prev_dir = 0u8;
        let mut last_dir_change_j = i + 1;
        for j in (i + 1)..=max_j {
            let cur = pts[j % n];
            let prev = pts[(j - 1) % n];
            let step_dx = cur.0 - prev.0;
            let step_dy = cur.1 - prev.1;
            let bit = match (step_dx.signum(), step_dy.signum()) {
                (1, 0) => 1u8,
                (0, 1) => 2u8,
                (-1, 0) => 4u8,
                (0, -1) => 8u8,
                _ => 0u8,
            };
            dir_mask |= bit;
            // Guard against all 4 directions OR any 180-degree U-turn (E+W or S+N)
            // so 1px border strips and 1px peninsulas cannot collapse into chords.
            if dir_mask == 0x0F || (dir_mask & 0x05) == 0x05 || (dir_mask & 0x0A) == 0x0A {
                // Rewind past a 1-2px peninsula tip before breaking so straight
                // runs do not cut diagonally across 1px-wide features.
                if j.saturating_sub(last_dir_change_j) <= 2 && last_dir_change_j > i + 1 {
                    last_valid = last_valid.min(last_dir_change_j - 1);
                }
                break;
            }
            if prev_dir != 0 && bit != 0 && bit != prev_dir {
                last_dir_change_j = j;
            }
            if bit != 0 {
                prev_dir = bit;
            }
            // Scale by 2 so ±1 offsets represent exact ±0.5-pixel box tolerance,
            // keeping 1px peninsulas and border edges exact while Taubin-Schneider smooths curves.
            let cur_x = 2 * i64::from(cur.0 - p0.0);
            let cur_y = 2 * i64::from(cur.1 - p0.1);
            let inside_cone = if closed {
                let box_c0_pt = (
                    cur_x
                        + if c0.1 < 0 {
                            1
                        } else if c0.1 > 0 {
                            -1
                        } else {
                            0
                        },
                    cur_y
                        + if c0.0 > 0 {
                            1
                        } else if c0.0 < 0 {
                            -1
                        } else {
                            0
                        },
                );
                let box_c1_pt = (
                    cur_x
                        + if c1.1 > 0 {
                            1
                        } else if c1.1 < 0 {
                            -1
                        } else {
                            0
                        },
                    cur_y
                        + if c1.0 < 0 {
                            1
                        } else if c1.0 > 0 {
                            -1
                        } else {
                            0
                        },
                );
                xprod(c0.0, c0.1, box_c0_pt.0, box_c0_pt.1) >= 0
                    && xprod(c1.0, c1.1, box_c1_pt.0, box_c1_pt.1) <= 0
            } else {
                xprod(c0.0, c0.1, cur_x, cur_y) >= 0 && xprod(c1.0, c1.1, cur_x, cur_y) <= 0
            };
            if !inside_cone {
                break;
            }
            last_valid = j;
            if cur_x.abs() > 1 || cur_y.abs() > 1 {
                let off0 = (
                    cur_x
                        + if cur_y >= 0 && (cur_y > 0 || cur_x < 0) {
                            1
                        } else {
                            -1
                        },
                    cur_y
                        + if cur_x <= 0 && (cur_x < 0 || cur_y < 0) {
                            1
                        } else {
                            -1
                        },
                );
                let off1 = (
                    cur_x
                        + if cur_y <= 0 && (cur_y < 0 || cur_x < 0) {
                            1
                        } else {
                            -1
                        },
                    cur_y
                        + if cur_x >= 0 && (cur_x > 0 || cur_y < 0) {
                            1
                        } else {
                            -1
                        },
                );
                if xprod(c0.0, c0.1, off0.0, off0.1) >= 0
                    && (c1 == (0, 0) || xprod(off0.0, off0.1, c1.0, c1.1) >= 0)
                {
                    c0 = off0;
                }
                if xprod(c1.0, c1.1, off1.0, off1.1) <= 0
                    && (c0 == (0, 0) || xprod(c0.0, c0.1, off1.0, off1.1) >= 0)
                {
                    c1 = off1;
                }
            }
        }
        lon[i] = last_valid;
    }
    if !closed {
        lon[n - 1] = n - 1;
    }
    lon
}

#[inline]
const fn xprod(ax: i64, ay: i64, bx: i64, by: i64) -> i64 {
    ax * by - ay * bx
}

fn potrace_bestpolygon(
    pts: &[(i32, i32)],
    sums: &[PotraceSum],
    lon: &[usize],
    closed: bool,
) -> Vec<usize> {
    let n = pts.len();
    if n <= 3 {
        return (0..n).collect();
    }
    let max_span = lon
        .iter()
        .enumerate()
        .map(|(i, &l)| l.saturating_sub(i))
        .max()
        .unwrap_or(1)
        .max(1);
    if !closed {
        let mut pen = vec![f64::INFINITY; n];
        let mut prev = vec![0usize; n];
        let mut segs = vec![usize::MAX; n];
        pen[0] = 0.0;
        segs[0] = 0;
        for j in 1..n {
            let start_i = j.saturating_sub(max_span);
            for i in start_i..j {
                if lon[i] < j || segs[i] == usize::MAX {
                    continue;
                }
                let cand_segs = segs[i] + 1;
                let cand_pen = pen[i] + potrace_penalty3(pts, sums, i, j);
                if cand_segs < segs[j] || (cand_segs == segs[j] && cand_pen < pen[j]) {
                    segs[j] = cand_segs;
                    pen[j] = cand_pen;
                    prev[j] = i;
                }
            }
        }
        let mut po = Vec::new();
        let mut cur = n - 1;
        while cur > 0 {
            po.push(cur);
            cur = prev[cur];
        }
        po.push(0);
        po.reverse();
        return po;
    }

    // Closed loop DP starting from index 0..n
    let mut pen = vec![f64::INFINITY; n + 1];
    let mut prev = vec![0usize; n + 1];
    let mut segs = vec![usize::MAX; n + 1];
    pen[0] = 0.0;
    segs[0] = 0;
    for j in 1..=n {
        let start_i = j.saturating_sub(max_span);
        for i in start_i..j {
            if lon[i] < j || segs[i] == usize::MAX {
                continue;
            }
            let cand_segs = segs[i] + 1;
            let cand_pen = pen[i] + potrace_penalty3(pts, sums, i, j);
            if cand_segs < segs[j] || (cand_segs == segs[j] && cand_pen < pen[j]) {
                segs[j] = cand_segs;
                pen[j] = cand_pen;
                prev[j] = i;
            }
        }
    }
    let mut po = Vec::new();
    let mut cur = n;
    while cur > 0 {
        cur = prev[cur];
        po.push(cur);
    }
    po.reverse();
    if po.len() < 3 {
        // Fallback evenly spaced indices around loop
        let step = (n / 4).max(1);
        po = (0..n).step_by(step).collect();
    }
    po
}

fn potrace_adjust_vertices(
    pts: &[(i32, i32)],
    sums: &[PotraceSum],
    po: &[usize],
    closed: bool,
) -> Vec<Point2D> {
    let n = pts.len();
    let m = po.len();
    if m < 2 {
        return pts
            .iter()
            .map(|&(x, y)| Point2D::new(f64::from(x), f64::from(y)))
            .collect();
    }
    let x0 = f64::from(pts[0].0);
    let y0 = f64::from(pts[0].1);
    let raw_vert = |idx: usize| -> Point2D {
        let p = pts[idx % n];
        Point2D::new(f64::from(p.0), f64::from(p.1))
    };

    // Fit optimal line (point ctr, unit direction dir) for segment i..j
    let fit_line = |i: usize, j: usize| -> (Point2D, Vec2D) {
        let jj = if j < i { j + n } else { j };
        let k = (jj - i + 1) as f64;
        let s = PotraceSum {
            x: sums[jj + 1].x - sums[i].x,
            y: sums[jj + 1].y - sums[i].y,
            x2: sums[jj + 1].x2 - sums[i].x2,
            xy: sums[jj + 1].xy - sums[i].xy,
            y2: sums[jj + 1].y2 - sums[i].y2,
        };
        let cx = s.x / k;
        let cy = s.y / k;
        let a = (s.x2 / k) - cx * cx;
        let b = (s.xy / k) - cx * cy;
        let c = (s.y2 / k) - cy * cy;
        let trace = a + c;
        let det = a * c - b * b;
        let disc = ((trace * trace * 0.25) - det).max(0.0).sqrt();
        let lambda = trace * 0.5 + disc;
        let mut dir = if a >= c {
            Vec2D::new(b, lambda - a)
        } else {
            Vec2D::new(lambda - c, b)
        };
        if dir.length_sq() <= 1e-9 {
            let p_a = raw_vert(i);
            let p_b = raw_vert(jj);
            dir = p_b - p_a;
        }
        dir = dir.normalize_or_zero();
        (Point2D::new(x0 + cx, y0 + cy), dir)
    };

    let seg_count = if closed { m } else { m - 1 };
    let mut lines = Vec::with_capacity(seg_count);
    for k in 0..seg_count {
        let i = po[k];
        let j = po[(k + 1) % m];
        lines.push(fit_line(i, j));
    }

    let mut out = Vec::with_capacity(m);
    for k in 0..m {
        let base = raw_vert(po[k]);
        if !closed && (k == 0 || k + 1 == m) {
            // Pin junction endpoints strictly!
            out.push(base);
            continue;
        }
        let l_prev = lines[(k + seg_count - 1) % seg_count];
        let l_curr = lines[k];
        if let Some(inter) = intersect_lines(l_prev.0, l_prev.1, l_curr.0, l_curr.1) {
            let dx = (inter.x - base.x).clamp(-0.48, 0.48);
            let dy = (inter.y - base.y).clamp(-0.48, 0.48);
            out.push(Point2D::new(base.x + dx, base.y + dy));
        } else {
            out.push(base);
        }
    }
    out
}

fn intersect_lines(p0: Point2D, d0: Vec2D, p1: Point2D, d1: Vec2D) -> Option<Point2D> {
    let denom = d0.x * d1.y - d0.y * d1.x;
    if denom.abs() <= 1e-6 {
        return None;
    }
    let dp = p1 - p0;
    let t = (dp.x * d1.y - dp.y * d1.x) / denom;
    let pt = p0 + d0 * t;
    pt.is_finite().then_some(pt)
}

#[allow(clippy::too_many_arguments)]
fn potrace_taubin_schneider_fit(
    pts: &[(i32, i32)],
    po: &[usize],
    vertices: &[Point2D],
    closed: bool,
    left_lbl: u8,
    right_lbl: u8,
    subpixel: Option<SubpixelCtx<'_>>,
    alphamax: f64,
    opttol: f64,
    smooth: f64,
    corner_deg: f64,
    scale_x: f64,
    scale_y: f64,
) -> Vec<PathNode> {
    let m = vertices.len();
    if m < 2 {
        return Vec::new();
    }
    let scale = |p: Point2D| Point2D::new(p.x * scale_x, p.y * scale_y);
    if smooth <= 0.02 || alphamax <= 0.0 || (closed && m < 3) {
        return vertices
            .iter()
            .map(|&p| PathNode::corner(scale(p)))
            .collect();
    }

    let corner_rad = corner_deg.to_radians();
    let mut is_corner = vec![false; m];
    for i in 0..m {
        if !closed && (i == 0 || i + 1 == m) {
            is_corner[i] = true;
            continue;
        }
        let prev = vertices[(i + m - 1) % m];
        let curr = vertices[i];
        let next = vertices[(i + 1) % m];
        let e_in = curr - prev;
        let e_out = next - curr;
        let len_in = e_in.length();
        let len_out = e_out.length();
        if len_in <= 1e-6 || len_out <= 1e-6 {
            is_corner[i] = true;
            continue;
        }
        let v_in = e_in / len_in;
        let v_out = e_out / len_out;
        let turn = v_in.dot(v_out).clamp(-1.0, 1.0).acos();
        let span = next - prev;
        let ddenom = span.x.abs() + span.y.abs();
        let dpara = (e_in.x * span.y - e_in.y * span.x).abs();
        let dd = if ddenom > 1e-6 {
            dpara / ddenom
        } else {
            f64::INFINITY
        };
        let alpha_potrace = if dd > 1.0 {
            (4.0 / 3.0) * (1.0 - 1.0 / dd)
        } else {
            0.0
        };
        let min_edge = len_in.min(len_out);
        let max_edge = len_in.max(len_out);
        // True 1-2px peninsula end-cap makes a 180-deg U-turn across a short cap edge
        // (whereas a shallow-arch 1px staircase step continues in the same forward direction!).
        let sharp_asymmetric_cap = if max_edge >= 5.5 && min_edge <= 2.5 && turn >= 1.25 {
            if len_in < len_out {
                let prev2 = vertices[(i + m - 2) % m];
                let v_prev_long = (prev - prev2).normalize_or_zero();
                v_prev_long.dot(v_out) < -0.5
            } else {
                let next2 = vertices[(i + 2) % m];
                let v_next_long = (next2 - next).normalize_or_zero();
                v_in.dot(v_next_long) < -0.5
            }
        } else {
            false
        };
        let sharp_long_corner = dd > 3.2 && min_edge >= 6.0 && turn >= corner_rad;
        if alpha_potrace > alphamax || sharp_long_corner || sharp_asymmetric_cap {
            is_corner[i] = true;
        }
    }

    let taubin_passes = (3.0 + smooth * 6.0).round() as usize;
    let fit_tol_px = opttol.clamp(0.22, 1.35);
    let n = pts.len();
    let corner_indices: Vec<usize> = (0..m).filter(|&i| is_corner[i]).collect();

    // Case 1: Closed smooth loop with ZERO sharp corners (circles, ovals, smooth organic holes/blobs)
    if closed && corner_indices.is_empty() {
        let mut ring = Vec::with_capacity(n);
        for idx in 0..n {
            let a = pts[idx];
            let b = pts[(idx + 1) % n];
            ring.push(subpixel_edge_midpoint(a, b, left_lbl, right_lbl, subpixel));
        }
        taubin_fair_points(&mut ring, true, taubin_passes);
        if ring.len() < 4 {
            return vertices
                .iter()
                .map(|&p| PathNode::corner(scale(p)))
                .collect();
        }
        let half = ring.len() / 2;
        let win = (ring.len() / 16).clamp(1, 5);
        let t0 = cyclic_unit_tangent(&ring, 0, win);
        let th = cyclic_unit_tangent(&ring, half, win);

        let seg1 = ring[0..=half].to_vec();
        let mut seg2 = ring[half..].to_vec();
        seg2.push(ring[0]);
        if seg1.len() < 2 || seg2.len() < 2 {
            return vertices
                .iter()
                .map(|&p| PathNode::corner(scale(p)))
                .collect();
        }
        let fit1 = schneider_fit_segment(&seg1, t0, -th, fit_tol_px);
        let fit2 = schneider_fit_segment(&seg2, th, -t0, fit_tol_px);
        let mut out_nodes: Vec<PathNode> = Vec::with_capacity(fit1.len() + fit2.len());
        append_cubic_chain(&mut out_nodes, &fit1, scale, true);
        append_cubic_chain(&mut out_nodes, &fit2, scale, true);
        if out_nodes.len() >= 3
            && out_nodes.last().unwrap().pos.distance_to(out_nodes[0].pos) < 1e-5
        {
            let last = out_nodes.pop().unwrap();
            out_nodes[0].handle_in = last.handle_in;
            out_nodes[0].kind = NodeKind::Smooth;
        }
        return out_nodes;
    }

    // Case 2: OpenArc or ClosedLoop with >= 1 sharp corner(s)
    let span_count = if closed {
        corner_indices.len()
    } else {
        corner_indices.len().saturating_sub(1)
    };
    let mut out_nodes: Vec<PathNode> = Vec::with_capacity(m * 2);
    for s_idx in 0..span_count {
        let ca = corner_indices[s_idx];
        let cb = corner_indices[(s_idx + 1) % corner_indices.len()];
        let start_pt = vertices[ca];
        let end_pt = vertices[cb];
        let poly_steps = if closed {
            if cb > ca {
                cb - ca
            } else {
                m - ca + cb
            }
        } else {
            cb - ca
        };

        // Single optimal-polygon edge between two sharp corners -> pure straight line segment!
        if poly_steps == 1 {
            let s0 = scale(start_pt);
            let s1 = scale(end_pt);
            if out_nodes.is_empty() {
                out_nodes.push(PathNode::corner(s0));
            } else if let Some(last) = out_nodes.last_mut() {
                last.kind = NodeKind::Cusp;
            }
            out_nodes.push(PathNode::corner(s1));
            continue;
        }

        // Multi-edge smooth curve span between sharp corners `ca` and `cb`
        let raw_i0 = po[ca] % n;
        let raw_i1 = po[cb] % n;
        let mut curve_pts = Vec::new();
        curve_pts.push(start_pt);
        if closed {
            let steps = if raw_i1 > raw_i0 {
                raw_i1 - raw_i0
            } else {
                n - raw_i0 + raw_i1
            };
            for step in 0..steps {
                let idx = (raw_i0 + step) % n;
                let a = pts[idx];
                let b = pts[(idx + 1) % n];
                curve_pts.push(subpixel_edge_midpoint(a, b, left_lbl, right_lbl, subpixel));
            }
        } else {
            for idx in raw_i0..raw_i1 {
                let a = pts[idx];
                let b = pts[idx + 1];
                curve_pts.push(subpixel_edge_midpoint(a, b, left_lbl, right_lbl, subpixel));
            }
        }
        curve_pts.push(end_pt);

        taubin_fair_points(&mut curve_pts, false, taubin_passes);
        let win = (curve_pts.len() / 8).clamp(1, 4);
        let t_start = open_start_tangent(&curve_pts, win);
        let t_end = open_end_tangent(&curve_pts, win);
        let fitted = schneider_fit_segment(&curve_pts, t_start, t_end, fit_tol_px);
        append_cubic_chain(&mut out_nodes, &fitted, scale, false);
    }

    if closed
        && out_nodes.len() >= 3
        && out_nodes.last().unwrap().pos.distance_to(out_nodes[0].pos) < 1e-5
    {
        let last = out_nodes.pop().unwrap();
        out_nodes[0].handle_in = last.handle_in;
        out_nodes[0].kind = NodeKind::Cusp;
    }
    out_nodes
}

/// Computes the sub-pixel midpoint of grid step `a -> b` shifted along its perpendicular normal
/// using the continuous 0.5-iso-surface crossing of `rgba8` between `left_lbl` and `right_lbl`.
fn subpixel_edge_midpoint(
    a: (i32, i32),
    b: (i32, i32),
    left_lbl: u8,
    right_lbl: u8,
    subpixel: Option<SubpixelCtx<'_>>,
) -> Point2D {
    let mx = (f64::from(a.0) + f64::from(b.0)) * 0.5;
    let my = (f64::from(a.1) + f64::from(b.1)) * 0.5;
    let Some(ctx) = subpixel else {
        return Point2D::new(mx, my);
    };
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let (pl, pr) = match (dx, dy) {
        (1, 0) => ((a.0, a.1 - 1), (a.0, a.1)),
        (0, 1) => ((a.0, a.1), (a.0 - 1, a.1)),
        (-1, 0) => ((a.0 - 1, a.1), (a.0 - 1, a.1 - 1)),
        (0, -1) => ((a.0 - 1, a.1 - 1), (a.0, a.1 - 1)),
        _ => return Point2D::new(mx, my),
    };
    if pl.0 < 0
        || pl.1 < 0
        || pr.0 < 0
        || pr.1 < 0
        || (pl.0 as usize) >= ctx.w
        || (pl.1 as usize) >= ctx.h
        || (pr.0 as usize) >= ctx.w
        || (pr.1 as usize) >= ctx.h
    {
        return Point2D::new(mx, my);
    }
    let il = ((pl.1 as usize) * ctx.w + (pl.0 as usize)) * 4;
    let ir = ((pr.1 as usize) * ctx.w + (pr.0 as usize)) * 4;
    let pxl = &ctx.rgba8[il..il + 4];
    let pxr = &ctx.rgba8[ir..ir + 4];

    // Alpha boundary sub-pixel interpolation for transparent PNGs
    if (pxl[3] < 128) != (pxr[3] < 128) {
        let fl = f64::from(pxl[3]) / 255.0;
        let fr = f64::from(pxr[3]) / 255.0;
        let denom = fr - fl;
        if denom.abs() > 0.15 {
            let delta = ((0.5 - fl) / denom - 0.5).clamp(-0.40, 0.40);
            let nx = f64::from(-dy);
            let ny = f64::from(dx);
            return Point2D::new(mx + nx * delta, my + ny * delta);
        }
        return Point2D::new(mx, my);
    }

    let cl = if (left_lbl as usize) < ctx.palette.len() {
        ctx.palette[left_lbl as usize]
    } else {
        ctx.bg_rgb
    };
    let cr = if (right_lbl as usize) < ctx.palette.len() {
        ctx.palette[right_lbl as usize]
    } else {
        ctx.bg_rgb
    };
    let dcr = f64::from(cr[0]) - f64::from(cl[0]);
    let dcg = f64::from(cr[1]) - f64::from(cl[1]);
    let dcb = f64::from(cr[2]) - f64::from(cl[2]);
    let dc_sq = dcr * dcr + dcg * dcg + dcb * dcb;
    if dc_sq <= 400.0 {
        return Point2D::new(mx, my);
    }
    let proj = |px: &[u8]| -> f64 {
        let vr = f64::from(px[0]) - f64::from(cl[0]);
        let vg = f64::from(px[1]) - f64::from(cl[1]);
        let vb = f64::from(px[2]) - f64::from(cl[2]);
        ((vr * dcr + vg * dcg + vb * dcb) / dc_sq).clamp(0.0, 1.0)
    };
    let fl = proj(pxl);
    let fr = proj(pxr);
    if fr - fl > 0.15 {
        let delta = ((0.5 - fl) / (fr - fl) - 0.5).clamp(-0.40, 0.40);
        let nx = f64::from(-dy);
        let ny = f64::from(dx);
        Point2D::new(mx + nx * delta, my + ny * delta)
    } else {
        Point2D::new(mx, my)
    }
}

/// Taubin SIGGRAPH '95 shrinkage-free low-pass contour filter (`lambda = 0.45, mu = -0.473`).
/// Eliminates pixel-grid staircase oscillation while preserving circle/stroke radii to < 0.1%.
fn taubin_fair_points(pts: &mut [Point2D], closed: bool, passes: usize) {
    let n = pts.len();
    if (closed && n < 4) || (!closed && n < 4) || passes == 0 {
        return;
    }
    const LAMBDA: f64 = 0.45;
    const MU: f64 = -0.473;
    let mut tmp = pts.to_vec();
    for _ in 0..passes {
        for weight in [LAMBDA, MU] {
            if closed {
                for i in 0..n {
                    let prev = pts[(i + n - 1) % n];
                    let curr = pts[i];
                    let next = pts[(i + 1) % n];
                    let lap_x = 0.5 * (prev.x + next.x) - curr.x;
                    let lap_y = 0.5 * (prev.y + next.y) - curr.y;
                    tmp[i] = Point2D::new(curr.x + weight * lap_x, curr.y + weight * lap_y);
                }
                pts.copy_from_slice(&tmp);
            } else {
                tmp[0] = pts[0];
                tmp[n - 1] = pts[n - 1];
                for i in 1..(n - 1) {
                    let prev = pts[i - 1];
                    let curr = pts[i];
                    let next = pts[i + 1];
                    let lap_x = 0.5 * (prev.x + next.x) - curr.x;
                    let lap_y = 0.5 * (prev.y + next.y) - curr.y;
                    tmp[i] = Point2D::new(curr.x + weight * lap_x, curr.y + weight * lap_y);
                }
                pts.copy_from_slice(&tmp);
            }
        }
    }
}

fn cyclic_unit_tangent(pts: &[Point2D], idx: usize, win: usize) -> Vec2D {
    let n = pts.len();
    let a = pts[(idx + n - win.min(n - 1)) % n];
    let b = pts[(idx + win.min(n - 1)) % n];
    let d = b - a;
    if d.length_sq() > 1e-12 {
        d.normalize_or_zero()
    } else {
        (pts[(idx + 1) % n] - pts[idx]).normalize_or_zero()
    }
}

fn open_start_tangent(pts: &[Point2D], win: usize) -> Vec2D {
    let k = win.min(pts.len() - 1).max(1);
    let d = pts[k] - pts[0];
    if d.length_sq() > 1e-12 {
        d.normalize_or_zero()
    } else {
        Vec2D::new(1.0, 0.0)
    }
}

fn open_end_tangent(pts: &[Point2D], win: usize) -> Vec2D {
    let last = pts.len() - 1;
    let k = win.min(last).max(1);
    let d = pts[last - k] - pts[last];
    if d.length_sq() > 1e-12 {
        d.normalize_or_zero()
    } else {
        Vec2D::new(-1.0, 0.0)
    }
}

/// Appends fitted `[p0, c1, c2, p3]` cubic segments onto `out_nodes` with `G1` `NodeKind::Smooth`
/// interior joins and `NodeKind::Cusp` (or `Smooth` when `smooth_ends`) span endpoints.
fn append_cubic_chain(
    out_nodes: &mut Vec<PathNode>,
    cubics: &[[Point2D; 4]],
    scale: impl Fn(Point2D) -> Point2D,
    smooth_ends: bool,
) {
    if cubics.is_empty() {
        return;
    }
    let end_kind = if smooth_ends {
        NodeKind::Smooth
    } else {
        NodeKind::Cusp
    };
    for (idx, seg) in cubics.iter().enumerate() {
        let p0 = scale(seg[0]);
        let c1 = scale(seg[1]);
        let c2 = scale(seg[2]);
        let p3 = scale(seg[3]);
        let is_line = c1.distance_to(p0.lerp(p3, 1.0 / 3.0)) < 1e-4
            && c2.distance_to(p0.lerp(p3, 2.0 / 3.0)) < 1e-4;
        let h_out = (!is_line).then_some(c1);
        let h_in = (!is_line).then_some(c2);

        if out_nodes.is_empty() {
            out_nodes.push(PathNode::new(p0, None, h_out, end_kind));
        } else if let Some(last) = out_nodes.last_mut() {
            last.handle_out = h_out;
            if idx > 0 {
                last.kind = NodeKind::Smooth;
            } else {
                last.kind = end_kind;
            }
        }
        let next_kind = if idx + 1 == cubics.len() {
            end_kind
        } else {
            NodeKind::Smooth
        };
        out_nodes.push(PathNode::new(p3, h_in, None, next_kind));
    }
}

/// Philip J. Schneider's Least-Squares Cubic Bézier Curve Fitter (`Graphics Gems`)
/// with Newton-Raphson parameter refinement and bounded recursion depth (`<= 14`).
fn schneider_fit_segment(
    pts: &[Point2D],
    t_hat1: Vec2D,
    t_hat2: Vec2D,
    tol_px: f64,
) -> Vec<[Point2D; 4]> {
    let mut out = Vec::new();
    schneider_fit_recursive(pts, t_hat1, t_hat2, tol_px * tol_px, 0, &mut out);
    out
}

fn schneider_fit_recursive(
    pts: &[Point2D],
    t_hat1: Vec2D,
    t_hat2: Vec2D,
    tol_sq: f64,
    depth: usize,
    out: &mut Vec<[Point2D; 4]>,
) {
    let n = pts.len();
    if n < 2 {
        return;
    }
    let p0 = pts[0];
    let p3 = pts[n - 1];
    if n == 2 {
        let dist = p0.distance_to(p3) / 3.0;
        out.push([p0, p0 + t_hat1 * dist, p3 + t_hat2 * dist, p3]);
        return;
    }

    // Collinear fast-path: emit exact straight line segment when all points lie within 0.18 px of chord
    let max_line_err_sq = (0.18_f64 * 0.18_f64).min(tol_sq * 0.5);
    if pts[1..(n - 1)]
        .iter()
        .all(|&p| point_seg_dist_sq(p, p0, p3) <= max_line_err_sq)
    {
        out.push([p0, p0.lerp(p3, 1.0 / 3.0), p0.lerp(p3, 2.0 / 3.0), p3]);
        return;
    }

    let mut u = chord_length_parameterize(pts);
    let mut bez = generate_least_squares_bezier(pts, &u, t_hat1, t_hat2);
    let (mut max_err, mut split_idx) = compute_max_bezier_error(pts, &bez, &u);
    if max_err <= tol_sq {
        out.push(bez);
        return;
    }

    // Newton-Raphson parameter refinement when close to tolerance
    if max_err <= tol_sq * 12.0 {
        for _ in 0..4 {
            reparameterize_newton_raphson(pts, &bez, &mut u);
            bez = generate_least_squares_bezier(pts, &u, t_hat1, t_hat2);
            let (err, s_idx) = compute_max_bezier_error(pts, &bez, &u);
            max_err = err;
            split_idx = s_idx;
            if max_err <= tol_sq {
                out.push(bez);
                return;
            }
        }
    }

    if depth >= 14 || n <= 3 {
        out.push(bez);
        return;
    }

    let split = split_idx.clamp(1, n - 2);
    let win = 3.min(split).min(n - 1 - split).max(1);
    let mut t_center = (pts[split - win] - pts[split + win]).normalize_or_zero();
    if t_center.length_sq() <= 1e-12 {
        t_center = (pts[split - 1] - pts[split + 1]).normalize_or_zero();
    }
    schneider_fit_recursive(&pts[..=split], t_hat1, t_center, tol_sq, depth + 1, out);
    schneider_fit_recursive(&pts[split..], -t_center, t_hat2, tol_sq, depth + 1, out);
}

fn chord_length_parameterize(pts: &[Point2D]) -> Vec<f64> {
    let mut u = vec![0.0_f64; pts.len()];
    for i in 1..pts.len() {
        u[i] = u[i - 1] + pts[i].distance_to(pts[i - 1]);
    }
    let total = u[pts.len() - 1];
    if total > 1e-12 {
        for v in &mut u[1..] {
            *v /= total;
        }
        u[pts.len() - 1] = 1.0;
    }
    u
}

fn generate_least_squares_bezier(
    pts: &[Point2D],
    u: &[f64],
    t_hat1: Vec2D,
    t_hat2: Vec2D,
) -> [Point2D; 4] {
    let p0 = pts[0];
    let p3 = pts[pts.len() - 1];
    let chord_len = p0.distance_to(p3);
    let mut c00 = 0.0_f64;
    let mut c01 = 0.0_f64;
    let mut c11 = 0.0_f64;
    let mut x0 = 0.0_f64;
    let mut x1 = 0.0_f64;

    for (&d_i, &t) in pts.iter().zip(u.iter()) {
        let inv = 1.0 - t;
        let b0 = inv * inv * inv;
        let b1 = 3.0 * inv * inv * t;
        let b2 = 3.0 * inv * t * t;
        let b3 = t * t * t;
        let a1 = t_hat1 * b1;
        let a2 = t_hat2 * b2;
        c00 += a1.dot(a1);
        c01 += a1.dot(a2);
        c11 += a2.dot(a2);
        let base_x = p0.x * (b0 + b1) + p3.x * (b2 + b3);
        let base_y = p0.y * (b0 + b1) + p3.y * (b2 + b3);
        let tmp = Vec2D::new(d_i.x - base_x, d_i.y - base_y);
        x0 += a1.dot(tmp);
        x1 += a2.dot(tmp);
    }

    let det_c = c00 * c11 - c01 * c01;
    let fallback = (chord_len / 3.0).max(1e-3);
    let (mut alpha1, mut alpha2) = if det_c.abs() > 1e-9 {
        ((x0 * c11 - c01 * x1) / det_c, (c00 * x1 - x0 * c01) / det_c)
    } else {
        (fallback, fallback)
    };
    let seg_len = pts.windows(2).map(|w| w[0].distance_to(w[1])).sum::<f64>();
    let eps = 1e-4 * seg_len;
    if alpha1 < eps || alpha2 < eps {
        alpha1 = fallback;
        alpha2 = fallback;
    } else {
        let max_handle = (seg_len * 0.75).max(chord_len * 1.15).max(fallback);
        alpha1 = alpha1.min(max_handle);
        alpha2 = alpha2.min(max_handle);
    }
    [p0, p0 + t_hat1 * alpha1, p3 + t_hat2 * alpha2, p3]
}

fn compute_max_bezier_error(pts: &[Point2D], bez: &[Point2D; 4], u: &[f64]) -> (f64, usize) {
    let mut max_d = 0.0_f64;
    let mut split = pts.len() / 2;
    for i in 1..(pts.len() - 1) {
        let p = cubic_eval(bez[0], bez[1], bez[2], bez[3], u[i]);
        let d = p.distance_sq(pts[i]);
        if d >= max_d {
            max_d = d;
            split = i;
        }
    }
    (max_d, split)
}

fn reparameterize_newton_raphson(pts: &[Point2D], bez: &[Point2D; 4], u: &mut [f64]) {
    let q1_0 = (bez[1] - bez[0]) * 3.0;
    let q1_1 = (bez[2] - bez[1]) * 3.0;
    let q1_2 = (bez[3] - bez[2]) * 3.0;
    let q2_0 = (q1_1 - q1_0) * 2.0;
    let q2_1 = (q1_2 - q1_1) * 2.0;

    for i in 1..(pts.len() - 1) {
        let t = u[i];
        let inv = 1.0 - t;
        let qt = cubic_eval(bez[0], bez[1], bez[2], bez[3], t);
        let q1t = q1_0 * (inv * inv) + q1_1 * (2.0 * inv * t) + q1_2 * (t * t);
        let q2t = q2_0 * inv + q2_1 * t;
        let diff = qt - pts[i];
        let num = diff.dot(q1t);
        let den = q1t.dot(q1t) + diff.dot(q2t);
        if den.abs() > 1e-12 {
            let next_t = (t - num / den).clamp(u[i - 1], 1.0);
            u[i] = next_t;
        }
    }
}

#[inline]
fn point_seg_dist_sq(p: Point2D, a: Point2D, b: Point2D) -> f64 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= 1e-12 {
        return p.distance_sq(a);
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    p.distance_sq(a + ab * t)
}

#[allow(dead_code)]
fn potrace_opticurve_merge(nodes: &mut Vec<PathNode>, closed: bool, tol_mm: f64) {
    if nodes.len() < 4 || tol_mm <= 1e-6 {
        return;
    }
    let mut i = if closed { 0 } else { 1 };
    let mut guard = 0usize;
    while nodes.len() >= 4 && guard < nodes.len() * 2 {
        guard += 1;
        let len = nodes.len();
        if !closed && i + 2 >= len {
            break;
        }
        if closed && i >= len {
            break;
        }
        let i0 = i % len;
        let i1 = (i + 1) % len;
        let i2 = (i + 2) % len;
        if nodes[i1].kind != NodeKind::Smooth
            || nodes[i0].handle_out.is_none()
            || nodes[i1].handle_in.is_none()
            || nodes[i1].handle_out.is_none()
            || nodes[i2].handle_in.is_none()
        {
            i += 1;
            continue;
        }
        let p0 = nodes[i0].pos;
        let p1 = nodes[i1].pos;
        let p2 = nodes[i2].pos;
        // Check matching bend side (skip S-inflections) & forward tangent progression along chord
        let c1 = nodes[i0].handle_out.unwrap();
        let c2 = nodes[i2].handle_in.unwrap();
        let chord = p2 - p0;
        let t1 = (c1 - p0).normalize_or_zero();
        let t2 = (c2 - p2).normalize_or_zero();
        let cross1 = (c1 - p0).x * chord.y - (c1 - p0).y * chord.x;
        let cross2 = (c2 - p2).x * chord.y - (c2 - p2).y * chord.x;
        if cross1 * cross2 <= 0.0 || t1.dot(chord) <= 0.0 || t2.dot(p0 - p2) <= 0.0 {
            i += 1;
            continue;
        }
        let h1_dir = c1 - p0;
        let h2_dir = c2 - p2;
        let d = h1_dir + h2_dir;
        let m02 = p0.lerp(p2, 0.5);
        let scale = if d.length_sq() > 1e-12 {
            ((p1 - m02).dot(d) / (0.375 * d.length_sq())).clamp(1.25, 2.25)
        } else {
            1.5
        };
        let h1_new = p0 + h1_dir * scale;
        let h2_new = p2 + h2_dir * scale;
        let mid_merged = cubic_eval(p0, h1_new, h2_new, p2, 0.5);
        if mid_merged.distance_to(p1) <= tol_mm {
            nodes[i0].handle_out = Some(h1_new);
            nodes[i2].handle_in = Some(h2_new);
            nodes.remove(i1);
        } else {
            i += 1;
        }
    }
}

#[inline]
fn cubic_eval(p0: Point2D, p1: Point2D, p2: Point2D, p3: Point2D, t: f64) -> Point2D {
    let u = 1.0 - t;
    let w0 = u * u * u;
    let w1 = 3.0 * u * u * t;
    let w2 = 3.0 * u * t * t;
    let w3 = t * t * t;
    Point2D::new(
        p0.x * w0 + p1.x * w1 + p2.x * w2 + p3.x * w3,
        p0.y * w0 + p1.y * w1 + p2.y * w2 + p3.y * w3,
    )
}

/// Centerline Trace using Zhang-Suen iterative morphological thinning/skeletonization.
fn trace_centerline_rgba(
    rgba8: &[u8],
    w: usize,
    h: usize,
    config: &TraceConfig,
    scale_x: f64,
    scale_y: f64,
) -> Vec<(BezierPath, Color)> {
    let (_, detail, smooth, corner_deg, _) = effective_params(config);

    // Identify background RGB from border pixels
    let bg_rgb = estimate_border_rgb_rgba(rgba8, w, h);
    let mut fg_mask = vec![false; w * h];
    let mut sum_r = 0u64;
    let mut sum_g = 0u64;
    let mut sum_b = 0u64;
    let mut fg_count = 0u64;

    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let a = rgba8[idx + 3];
            if a < 128 {
                continue;
            }
            let r = rgba8[idx];
            let g = rgba8[idx + 1];
            let b = rgba8[idx + 2];
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
/// Automatically detects bimodal achromatic line-art / coloring-book / text drawings via Otsu
/// luminance separation so anti-aliased / JPEG-compressed B&W line art never splits into multi-gray
/// onion-skin halos, and uses 5x5-cross flat interior seeding on full-color artwork.
fn quantize_kmeans_u8(
    rgba8: &[u8],
    w: usize,
    h: usize,
    max_colors: usize,
    allow_bimodal_lineart: bool,
) -> (Vec<[u8; 3]>, Vec<u8>) {
    let n_px = w * h;
    let stride = (n_px / 32_768).max(1);
    let mut flat_samples: Vec<[f32; 3]> = Vec::new();
    let mut all_samples: Vec<[f32; 3]> = Vec::new();
    let mut luma_hist = [0u32; 256];
    let mut chromatic_samples = 0usize;
    let chunks = rgba8.as_chunks::<4>().0;

    for (i, px) in chunks.iter().enumerate() {
        if i % stride != 0 || px[3] < 128 {
            continue;
        }
        let sample = [f32::from(px[0]), f32::from(px[1]), f32::from(px[2])];
        all_samples.push(sample);
        let cmax = px[0].max(px[1]).max(px[2]);
        let cmin = px[0].min(px[1]).min(px[2]);
        if cmax - cmin > 28 {
            chromatic_samples += 1;
        }
        let luma = ((u32::from(px[0]) * 54 + u32::from(px[1]) * 183 + u32::from(px[2]) * 19) >> 8)
            .min(255) as usize;
        luma_hist[luma] += 1;

        if px[3] < 220 {
            continue;
        }
        let x = i % w;
        let y = i / w;
        if x >= 2 && x + 2 < w && y >= 2 && y + 2 < h {
            let nbrs = [
                i - 1,
                i + 1,
                i - 2,
                i + 2,
                i - w,
                i + w,
                i - 2 * w,
                i + 2 * w,
            ];
            let is_flat = nbrs.iter().all(|&ni| {
                let np = chunks[ni];
                np[3] >= 220
                    && px[0].abs_diff(np[0]) <= 12
                    && px[1].abs_diff(np[1]) <= 12
                    && px[2].abs_diff(np[2]) <= 12
            });
            if is_flat {
                flat_samples.push(sample);
            }
        }
    }
    if all_samples.is_empty() {
        return (Vec::new(), vec![u8::MAX; n_px]);
    }

    // Automatic bimodal Otsu ink-vs-paper binarization for monochrome line art / coloring books
    if allow_bimodal_lineart
        && chromatic_samples < 4
        && chromatic_samples * 1000 <= all_samples.len()
    {
        if let Some((thresh, ink_rgb, paper_rgb)) =
            try_otsu_bimodal_achromatic(&luma_hist, all_samples.len())
        {
            let palette = vec![ink_rgb, paper_rgb];
            let mut labels = vec![u8::MAX; n_px];
            for (i, px) in chunks.iter().enumerate() {
                if px[3] >= 128 {
                    let luma =
                        (u32::from(px[0]) * 54 + u32::from(px[1]) * 183 + u32::from(px[2]) * 19)
                            >> 8;
                    labels[i] = u8::from(luma > thresh);
                }
            }
            return (palette, labels);
        }
    }

    let eff_max = max_colors.clamp(1, 254);
    let primary_pool = if flat_samples.len() >= 16 {
        &flat_samples
    } else {
        &all_samples
    };

    let mut distinct: Vec<[u8; 3]> = Vec::new();
    for s in primary_pool {
        let rgb = [s[0] as u8, s[1] as u8, s[2] as u8];
        if !distinct.contains(&rgb) {
            distinct.push(rgb);
            if distinct.len() > eff_max {
                break;
            }
        }
    }

    let mut centroids: Vec<[f32; 3]> = if distinct.len() <= eff_max {
        distinct
            .iter()
            .map(|&[r, g, b]| [f32::from(r), f32::from(g), f32::from(b)])
            .collect()
    } else {
        let mut init = vec![primary_pool[0]];
        while init.len() < eff_max {
            let mut best_dist = -1.0_f32;
            let mut best_sample = primary_pool[0];
            for &s in primary_pool {
                let min_d = init
                    .iter()
                    .map(|c| color_dist_sq(s, *c))
                    .fold(f32::INFINITY, f32::min);
                if min_d > best_dist {
                    best_dist = min_d;
                    best_sample = s;
                }
            }
            if best_dist <= 16.0 {
                break;
            }
            init.push(best_sample);
        }
        init
    };

    // Rescue genuine thin-stroke colors absent from `flat_samples` while rejecting linear AA blends
    while centroids.len() < eff_max {
        let mut best_dist = 2025.0_f32; // > 45 RGB units from all current centroids
        let mut rescue_sample: Option<[f32; 3]> = None;
        for &s in &all_samples {
            let min_d = centroids
                .iter()
                .map(|c| color_dist_sq(s, *c))
                .fold(f32::INFINITY, f32::min);
            if min_d > best_dist && !is_linear_blend_of_any(s, &centroids, 324.0) {
                best_dist = min_d;
                rescue_sample = Some(s);
            }
        }
        if let Some(rs) = rescue_sample {
            centroids.push(rs);
        } else {
            break;
        }
    }

    let refine_pool = if flat_samples.len() >= 16 && centroids.len() <= distinct.len().max(1) {
        &flat_samples
    } else {
        &all_samples
    };
    for _ in 0..10 {
        let mut sums = vec![[0.0_f64; 3]; centroids.len()];
        let mut counts = vec![0u64; centroids.len()];
        for &s in refine_pool {
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

    // Merge near-identical centroids (perceptual^2 < 36.0)
    let mut merged: Vec<[f32; 3]> = Vec::new();
    for c in centroids {
        if !merged.iter().any(|&m| color_dist_sq(c, m) < 36.0) {
            merged.push(c);
        }
    }
    if merged.is_empty() {
        return (Vec::new(), vec![u8::MAX; n_px]);
    }

    let palette: Vec<[u8; 3]> = merged
        .iter()
        .map(|c| {
            [
                c[0].round().clamp(0.0, 255.0) as u8,
                c[1].round().clamp(0.0, 255.0) as u8,
                c[2].round().clamp(0.0, 255.0) as u8,
            ]
        })
        .collect();

    let mut labels = vec![u8::MAX; n_px];
    for (i, px) in chunks.iter().enumerate() {
        if px[3] >= 128 {
            let s = [f32::from(px[0]), f32::from(px[1]), f32::from(px[2])];
            labels[i] = nearest_centroid(s, &merged) as u8;
        }
    }

    (palette, labels)
}

/// Returns `true` if `s` lies within `max_perp_sq` of the RGB segment between any pair in `centroids`.
fn is_linear_blend_of_any(s: [f32; 3], centroids: &[[f32; 3]], max_perp_sq: f32) -> bool {
    if centroids.len() < 2 {
        return false;
    }
    for i in 0..centroids.len() {
        let a = centroids[i];
        for &b in &centroids[(i + 1)..] {
            let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let len_sq = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
            if len_sq <= 64.0 {
                continue;
            }
            let ap = [s[0] - a[0], s[1] - a[1], s[2] - a[2]];
            let t = (ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / len_sq;
            if (0.04..=0.96).contains(&t) {
                let proj = [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t];
                let dr = s[0] - proj[0];
                let dg = s[1] - proj[1];
                let db = s[2] - proj[2];
                if dr * dr + dg * dg + db * db <= max_perp_sq {
                    return true;
                }
            }
        }
    }
    false
}

/// Detects bimodal achromatic drawings (black/dark ink on white/light paper with anti-aliased or
/// JPEG-compressed edges) via Otsu inter-class variance and returns `(threshold, ink_rgb, paper_rgb)`.
fn try_otsu_bimodal_achromatic(
    hist: &[u32; 256],
    total_samples: usize,
) -> Option<(u32, [u8; 3], [u8; 3])> {
    if total_samples < 16 {
        return None;
    }
    let total_f = total_samples as f64;
    let total_sum: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| (i as f64) * f64::from(c))
        .sum();
    let mut w0 = 0.0_f64;
    let mut sum0 = 0.0_f64;
    let mut best_var = -1.0_f64;
    let mut best_t = 128usize;
    let mut best_mu0 = 0.0_f64;
    let mut best_mu1 = 255.0_f64;

    for (t, &cnt) in hist.iter().enumerate().take(255) {
        w0 += f64::from(cnt);
        if w0 <= 0.0 {
            continue;
        }
        let w1 = total_f - w0;
        if w1 <= 0.0 {
            break;
        }
        sum0 += (t as f64) * f64::from(cnt);
        let mu0 = sum0 / w0;
        let mu1 = (total_sum - sum0) / w1;
        let diff = mu1 - mu0;
        let between = w0 * w1 * diff * diff;
        if between > best_var {
            best_var = between;
            best_t = t;
            best_mu0 = mu0;
            best_mu1 = mu1;
        }
    }

    if best_mu1 - best_mu0 < 85.0 {
        return None;
    }
    let lo_band = best_t.saturating_sub(24);
    let hi_band = (best_t + 24).min(255);
    let mid_count: u32 = hist[lo_band..=hi_band].iter().sum();
    if (mid_count as usize) * 100 >= total_samples * 18 {
        return None;
    }
    let ink_v = best_mu0.round().clamp(0.0, 255.0) as u8;
    let paper_v = best_mu1.round().clamp(0.0, 255.0) as u8;
    Some((
        best_t as u32,
        [ink_v, ink_v, ink_v],
        [paper_v, paper_v, paper_v],
    ))
}

/// Shaves isolated 1-pixel JPEG/staircase pimples protruding sideways from thick walls
/// while leaving 1px thin strokes and 1px peninsulas 100% intact.
fn prune_sideways_1px_burrs(labels: &mut [u8], w: usize, h: usize) {
    if w < 3 || h < 3 {
        return;
    }
    let mut flips: Vec<(usize, u8)> = Vec::new();
    for y in 1..(h - 1) {
        let row = y * w;
        for x in 1..(w - 1) {
            let idx = row + x;
            let a = labels[idx];
            let n = labels[idx - w];
            let s = labels[idx + w];
            let w_l = labels[idx - 1];
            let e_l = labels[idx + 1];
            // North-pointing 1px pimple on a solid South wall `(SW == A && S == A && SE == A)`
            if n == w_l
                && n == e_l
                && n != a
                && s == a
                && labels[idx + w - 1] == a
                && labels[idx + w + 1] == a
            {
                flips.push((idx, n));
                continue;
            }
            // South-pointing 1px pimple on a solid North wall
            if s == w_l
                && s == e_l
                && s != a
                && n == a
                && labels[idx - w - 1] == a
                && labels[idx - w + 1] == a
            {
                flips.push((idx, s));
                continue;
            }
            // West-pointing 1px pimple on a solid East wall
            if w_l == n
                && w_l == s
                && w_l != a
                && e_l == a
                && labels[idx - w + 1] == a
                && labels[idx + w + 1] == a
            {
                flips.push((idx, w_l));
                continue;
            }
            // East-pointing 1px pimple on a solid West wall
            if e_l == n
                && e_l == s
                && e_l != a
                && w_l == a
                && labels[idx - w - 1] == a
                && labels[idx + w - 1] == a
            {
                flips.push((idx, e_l));
            }
        }
    }
    for (idx, nl) in flips {
        labels[idx] = nl;
    }
}

/// Reassigns 1..3px anti-aliasing transition pixels sandwiched between two distinct regions `L_a != L_b`
/// whose RGB value is a linear blend of `palette[L_a]` and `palette[L_b]`, eliminating halo rings
/// while leaving 1px thin strokes (`L_a == L_b`) completely untouched.
fn suppress_antialias_halos(
    rgba8: &[u8],
    palette: &[[u8; 3]],
    labels: &mut [u8],
    w: usize,
    h: usize,
) {
    if palette.len() < 3 || w < 5 || h < 5 {
        return;
    }
    let pal_f: Vec<[f32; 3]> = palette
        .iter()
        .map(|&[r, g, b]| [f32::from(r), f32::from(g), f32::from(b)])
        .collect();
    let chunks = rgba8.as_chunks::<4>().0;
    let mut updates: Vec<(usize, u8)> = Vec::new();
    for _pass in 0..3 {
        updates.clear();
        for y in 3..(h - 3) {
            let row = y * w;
            for x in 3..(w - 3) {
                let idx = row + x;
                let lp = labels[idx];
                if lp == u8::MAX {
                    continue;
                }
                let px = chunks[idx];
                let s = [f32::from(px[0]), f32::from(px[1]), f32::from(px[2])];
                let mut matched = false;
                for r in 1..=3usize {
                    let rw = r * w;
                    let pairs = [
                        (idx - r, idx + r),
                        (idx - rw, idx + rw),
                        (idx - rw - r, idx + rw + r),
                        (idx - rw + r, idx + rw - r),
                    ];
                    for (n1, n2) in pairs {
                        let la = labels[n1];
                        let lb = labels[n2];
                        if la == u8::MAX || lb == u8::MAX || la == lb || lp == la || lp == lb {
                            continue;
                        }
                        let a = pal_f[la as usize];
                        let b = pal_f[lb as usize];
                        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                        let len_sq = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
                        if len_sq <= 64.0 {
                            continue;
                        }
                        let ap = [s[0] - a[0], s[1] - a[1], s[2] - a[2]];
                        let t = (ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / len_sq;
                        if (0.04..=0.96).contains(&t) {
                            let proj = [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t];
                            let dr = s[0] - proj[0];
                            let dg = s[1] - proj[1];
                            let db = s[2] - proj[2];
                            if dr * dr + dg * dg + db * db <= 625.0 {
                                updates.push((idx, if t < 0.5 { la } else { lb }));
                                matched = true;
                                break;
                            }
                        }
                    }
                    if matched {
                        break;
                    }
                }
            }
        }
        if updates.is_empty() {
            break;
        }
        for &(idx, nl) in &updates {
            labels[idx] = nl;
        }
    }
}

#[inline]
fn color_dist_sq(a: [f32; 3], b: [f32; 3]) -> f32 {
    let rmean = 0.5 * (a[0] + b[0]);
    let dr = a[0] - b[0];
    let dg = a[1] - b[1];
    let db = a[2] - b[2];
    let wr = 2.0 + rmean / 256.0;
    let wg = 4.0;
    let wb = 2.0 + (255.0 - rmean) / 256.0;
    (wr * dr * dr + wg * dg * dg + wb * db * db) * (1.0 / 3.0)
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

fn detect_border_background_label_u8(
    labels: &[u8],
    w: usize,
    h: usize,
    palette_len: usize,
) -> Option<u8> {
    if palette_len <= 1 || w < 2 || h < 2 {
        return None;
    }
    // Never remove the sole opaque foreground color in an image
    let mut seen_opaque = [false; 255];
    let mut distinct_opaque = 0usize;
    for &l in labels {
        if l != u8::MAX && !seen_opaque[l as usize] {
            seen_opaque[l as usize] = true;
            distinct_opaque += 1;
            if distinct_opaque >= 2 {
                break;
            }
        }
    }
    if distinct_opaque < 2 {
        return None;
    }

    let mut counts: [usize; 255] = [0; 255];
    let mut side_mask: [u8; 255] = [0; 255];
    let mut transparent_border = 0usize;
    let mut total_border = 0usize;
    let mut visit = |x: usize, y: usize, side_bit: u8| {
        total_border += 1;
        let lbl = labels[y * w + x];
        if lbl == u8::MAX {
            transparent_border += 1;
        } else {
            counts[lbl as usize] += 1;
            side_mask[lbl as usize] |= side_bit;
        }
    };
    for x in 0..w {
        visit(x, 0, 1 << 0);
        if h > 1 {
            visit(x, h - 1, 1 << 1);
        }
    }
    for y in 1..h.saturating_sub(1) {
        visit(0, y, 1 << 2);
        if w > 1 {
            visit(w - 1, y, 1 << 3);
        }
    }
    // If >= 20% of the perimeter is already alpha-transparent, keep all opaque foreground colors!
    if transparent_border * 5 >= total_border {
        return None;
    }
    counts
        .iter()
        .enumerate()
        .max_by_key(|&(_, &c)| c)
        .filter(|&(lbl, &c)| {
            c > 0 && c * 100 > total_border * 52 && side_mask[lbl].count_ones() >= 3
        })
        .map(|(lbl, _)| lbl as u8)
}

/// Estimates the dominant background RGB color from opaque border pixels using
/// quantized mode voting so edge-touching foreground strokes cannot skew the average.
fn estimate_border_rgb_rgba(rgba8: &[u8], w: usize, h: usize) -> [u8; 3] {
    let mut buckets: HashMap<[u8; 3], [u64; 4]> = HashMap::new();
    let mut add = |x: usize, y: usize| {
        let idx = (y * w + x) * 4;
        if rgba8[idx + 3] >= 128 {
            let r = rgba8[idx];
            let g = rgba8[idx + 1];
            let b = rgba8[idx + 2];
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

/// Reassigns speckles smaller than `min_area` pixels to their dominant neighbor label so
/// removing small speckles never introduces white pinholes along color boundaries.
fn prune_speckles_reassign(labels: &mut [u8], w: usize, h: usize, min_area: usize) {
    if min_area <= 1 || w == 0 || h == 0 {
        return;
    }
    let total = w * h;
    let mut visited = vec![0u64; total.div_ceil(64)];
    let is_visited =
        |vis: &[u64], idx: usize| -> bool { (vis[idx >> 6] & (1u64 << (idx & 63))) != 0 };
    let mark_visited = |vis: &mut [u64], idx: usize| {
        vis[idx >> 6] |= 1u64 << (idx & 63);
    };

    let mut queue: VecDeque<u32> = VecDeque::new();
    let mut comp: Vec<u32> = Vec::with_capacity(min_area);
    for start in 0..total {
        let lbl = labels[start];
        if lbl == u8::MAX || is_visited(&visited, start) {
            continue;
        }
        mark_visited(&mut visited, start);
        queue.push_back(start as u32);
        comp.clear();
        let mut comp_count = 0usize;
        let mut nb_counts = [0u32; 255];
        while let Some(idx_u32) = queue.pop_front() {
            let idx = idx_u32 as usize;
            comp_count += 1;
            if comp.len() < min_area {
                comp.push(idx_u32);
            }
            let cx = idx % w;
            let cy = idx / w;
            let nbs = [
                (cx > 0).then(|| idx - 1),
                (cx + 1 < w).then(|| idx + 1),
                (cy > 0).then(|| idx - w),
                (cy + 1 < h).then(|| idx + w),
            ];
            for ni in nbs.into_iter().flatten() {
                let nl = labels[ni];
                if nl == lbl {
                    if !is_visited(&visited, ni) {
                        mark_visited(&mut visited, ni);
                        queue.push_back(ni as u32);
                    }
                } else if comp_count < min_area && nl != u8::MAX {
                    nb_counts[nl as usize] = nb_counts[nl as usize].saturating_add(1);
                }
            }
        }
        if comp_count < min_area {
            let replacement = nb_counts
                .iter()
                .enumerate()
                .filter(|&(_, &c)| c > 0)
                .max_by_key(|&(_, &c)| c)
                .map(|(k, _)| k as u8)
                .unwrap_or(u8::MAX);
            for &idx in &comp {
                labels[idx as usize] = replacement;
            }
        }
    }
}

/// Exact Marching Squares contour extraction for a binary pixel mask `(w x h)`.
///
/// Operates on a 1-pixel zero-padded grid so shapes touching image edges always produce
/// clean closed boundary loops in `[0..w, 0..h]` pixel coordinates. Shared with `smart_fill.rs`.
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_page_to_rgba;
    use coree_core::{ColorMode, Document, Fill, ObjectId, OutlineStyle, SceneObject, ViewMode};

    #[test]
    fn potrace_adjacent_blocks_produce_zero_white_seam_pixels() {
        let w = 64u32;
        let h = 64u32;
        let mut buf = vec![255u8; (w * h * 4) as usize];
        for y in 8..56 {
            for x in 8..32 {
                let i = ((y * w + x) * 4) as usize;
                buf[i..i + 4].copy_from_slice(&[220, 20, 20, 255]);
            }
            for x in 32..56 {
                let i = ((y * w + x) * 4) as usize;
                buf[i..i + 4].copy_from_slice(&[20, 60, 220, 255]);
            }
        }
        let asset = RasterAsset::new(w, h, buf, 96.0, ColorMode::Rgb).unwrap();
        let traced = trace_raster_to_mm(
            &asset,
            64.0,
            64.0,
            &TraceConfig {
                mode: TraceMode::OutlineTrace(OutlinePreset::Logo),
                max_colors: 4,
                remove_background: true,
                ..TraceConfig::default()
            },
        );
        assert_eq!(traced.len(), 2);

        let mut doc = Document::new("PotraceZeroGap", 64.0, 64.0, ColorMode::Rgb, 96.0);
        for (idx, (path, col)) in traced.into_iter().enumerate() {
            doc.add_object(
                SceneObject::new(
                    ObjectId(0),
                    format!("Region{idx}"),
                    coree_core::NodePayload::Path(path),
                )
                .with_fill(Fill::Solid(col))
                .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();
        }
        let rendered = render_page_to_rgba(&doc, 0, 101.6, ViewMode::Normal).unwrap();
        let rw = rendered.width as usize;
        let rh = rendered.height as usize;
        let cx = rw / 2;
        let mut white_seams = 0usize;
        for y in (rh / 5)..(rh * 4 / 5) {
            for dx in -1isize..=1 {
                let x = (cx as isize + dx) as usize;
                let i = (y * rw + x) * 4;
                if rendered.rgba[i] >= 245
                    && rendered.rgba[i + 1] >= 245
                    && rendered.rgba[i + 2] >= 245
                {
                    white_seams += 1;
                }
            }
        }
        assert_eq!(
            white_seams, 0,
            "Expected 0 white seam pixels along shared Potrace edge"
        );
    }

    #[test]
    fn potrace_sparse_graph_handles_12mp_quadrants_cleanly() {
        // 3464 x 3464 (~12MP) 4-quadrant label grid (~14k boundary vertices vs 12M total grid vertices)
        let w = 3464usize;
        let h = 3464usize;
        let mut labels = vec![0u8; w * h];
        for y in 0..h {
            let row_off = y * w;
            let top = y < h / 2;
            for x in 0..w {
                let left = x < w / 2;
                labels[row_off + x] = match (top, left) {
                    (true, true) => 0,
                    (true, false) => 1,
                    (false, true) => 2,
                    (false, false) => 3,
                };
            }
        }
        let regions =
            potrace_extract_shared_subpaths(&labels, w, h, 4, 1.0, 0.2, 0.5, 60.0, 0.1, 0.1);
        assert_eq!(regions.len(), 4);
        for lbl in 0..4usize {
            assert!(!regions[&lbl].is_empty());
        }
    }

    #[test]
    fn potrace_dense_checkerboard_enforces_edge_budget_without_oom() {
        // 1) <= 1MP (400x400) 1px checkerboard must NOT be downscaled by edge budget.
        let small_w = 400usize;
        let small_h = 400usize;
        let mut small_labels = vec![0u8; small_w * small_h];
        for y in 0..small_h {
            for x in 0..small_w {
                small_labels[y * small_w + x] = ((x + y) & 1) as u8;
            }
        }
        let (mut sw, mut sh, mut ssx, mut ssy) = (small_w, small_h, 0.25, 0.25);
        enforce_trace_edge_budget(&mut small_labels, &mut sw, &mut sh, &mut ssx, &mut ssy);
        assert_eq!((sw, sh), (400, 400));

        // 2) > 1MP (1400x1400) 4-quadrant (low edge count) must NOT be downscaled.
        let quad_w = 1400usize;
        let quad_h = 1400usize;
        let mut quad_labels = vec![0u8; quad_w * quad_h];
        for y in 0..quad_h {
            for x in 0..quad_w {
                quad_labels[y * quad_w + x] =
                    u8::from(x >= quad_w / 2) + 2 * u8::from(y >= quad_h / 2);
            }
        }
        let (mut qw, mut qh, mut qsx, mut qsy) = (quad_w, quad_h, 0.1, 0.1);
        enforce_trace_edge_budget(&mut quad_labels, &mut qw, &mut qh, &mut qsx, &mut qsy);
        assert_eq!((qw, qh), (1400, 1400));

        // 3) > 1MP (1400x1400) 2px checkerboard exceeds edge/arc budget and must downscale cleanly
        // while preserving exact physical mm dimensions and producing valid Potrace regions.
        let chk_w = 1400usize;
        let chk_h = 1400usize;
        let mut chk_labels = vec![0u8; chk_w * chk_h];
        for y in 0..chk_h {
            for x in 0..chk_w {
                chk_labels[y * chk_w + x] = (((x / 2) + (y / 2)) & 1) as u8;
            }
        }
        let (mut cw, mut ch, mut csx, mut csy) = (chk_w, chk_h, 0.1, 0.1);
        enforce_trace_edge_budget(&mut chk_labels, &mut cw, &mut ch, &mut csx, &mut csy);
        assert!(cw < chk_w && ch < chk_h);
        assert!(((cw as f64) * csx - 140.0).abs() < 1e-6);
        assert!(((ch as f64) * csy - 140.0).abs() < 1e-6);
        let (edges_after, arcs_after) = estimate_label_boundary_complexity(&chk_labels, cw, ch);
        assert!(edges_after <= MAX_SAFE_BOUNDARY_EDGES);
        assert!(arcs_after <= MAX_SAFE_POTRACE_ARCS);
        let regions =
            potrace_extract_shared_subpaths(&chk_labels, cw, ch, 2, 0.5, 0.2, 0.5, 60.0, csx, csy);
        assert!(!regions.is_empty());
    }

    #[test]
    fn potrace_diagonal_x_junction_preserves_full_border_zero_gaps() {
        let w = 400u32;
        let h = 400u32;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let colors = [
            [220u8, 25, 25, 255],
            [25u8, 160, 50, 255],
            [30u8, 80, 220, 255],
            [230u8, 200, 25, 255],
        ];
        for y in 0..h {
            let dy = f64::from(y) - 199.5;
            for x in 0..w {
                let dx = f64::from(x) - 199.5;
                let idx = if dy.abs() >= dx.abs() {
                    if dy < 0.0 {
                        0
                    } else {
                        1
                    }
                } else if dx < 0.0 {
                    2
                } else {
                    3
                };
                let off = ((y * w + x) * 4) as usize;
                buf[off..off + 4].copy_from_slice(&colors[idx]);
            }
        }
        let asset = RasterAsset::new(w, h, buf, 96.0, ColorMode::Rgb).unwrap();
        let traced = trace_raster_to_mm(
            &asset,
            400.0,
            400.0,
            &TraceConfig {
                mode: TraceMode::OutlineTrace(OutlinePreset::Logo),
                max_colors: 4,
                remove_background: false,
                speckle_size: 1,
                ..TraceConfig::default()
            },
        );
        assert_eq!(traced.len(), 4);

        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        let mut doc = Document::new("DiagXJunction", 400.0, 400.0, ColorMode::Rgb, 96.0);
        for (idx, (path, col)) in traced.into_iter().enumerate() {
            if let Some(bb) = path.bounding_box() {
                min_x = min_x.min(bb.min().x);
                min_y = min_y.min(bb.min().y);
                max_x = max_x.max(bb.max().x);
                max_y = max_y.max(bb.max().y);
            }
            doc.add_object(
                SceneObject::new(
                    ObjectId(0),
                    format!("Quad{idx}"),
                    coree_core::NodePayload::Path(path),
                )
                .with_fill(Fill::Solid(col))
                .with_outline(OutlineStyle::none()),
                None,
            )
            .unwrap();
        }
        assert!(
            min_x <= 0.01 && min_y <= 0.01 && max_x >= 399.99 && max_y >= 399.99,
            "Expected full [0..400, 0..400] coverage, got ({min_x}, {min_y})..({max_x}, {max_y})"
        );

        let rendered = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Normal).unwrap();
        let near_white = rendered
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[0] >= 245 && px[1] >= 245 && px[2] >= 245)
            .count();
        assert_eq!(
            near_white, 0,
            "Expected 0 white/near-white gap pixels on 400x400 diagonal X-junction"
        );
    }

    #[test]
    fn potrace_one_px_interior_peninsula_preserves_width() {
        let w = 400u32;
        let h = 400u32;
        let mut buf = vec![255u8; (w * h * 4) as usize];
        for y in 150..250 {
            for x in 100..200 {
                let off = ((y * w + x) * 4) as usize;
                buf[off..off + 4].copy_from_slice(&[210, 30, 30, 255]);
            }
        }
        // 1px-tall, 120px-long red finger at y = 200, x = 200..320
        for x in 200..320 {
            let off = ((200 * w + x) * 4) as usize;
            buf[off..off + 4].copy_from_slice(&[210, 30, 30, 255]);
        }
        let asset = RasterAsset::new(w, h, buf, 96.0, ColorMode::Rgb).unwrap();
        let traced = trace_raster_to_mm(
            &asset,
            400.0,
            400.0,
            &TraceConfig {
                mode: TraceMode::OutlineTrace(OutlinePreset::Logo),
                max_colors: 4,
                remove_background: true,
                speckle_size: 1,
                ..TraceConfig::default()
            },
        );
        assert_eq!(traced.len(), 1);
        let (path, col) = traced.into_iter().next().unwrap();
        let bb = path.bounding_box().unwrap();
        assert!(
            bb.max().x >= 319.5,
            "Expected 1px peninsula tip preserved to x>=319.5, got {}",
            bb.max().x
        );

        let mut doc = Document::new("Peninsula1Px", 400.0, 400.0, ColorMode::Rgb, 96.0);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Peninsula".to_string(),
                coree_core::NodePayload::Path(path),
            )
            .with_fill(Fill::Solid(col))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();
        // Render at 25.4 DPI (1 px == 1 mm) so pixel (310, 200) probes exact 1px peninsula width
        let rendered = render_page_to_rgba(&doc, 0, 25.4, ViewMode::Normal).unwrap();
        let rw = rendered.width as usize;
        for x in [210usize, 260, 310, 318] {
            let off = (200 * rw + x) * 4;
            assert!(
                rendered.rgba[off] > 150 && rendered.rgba[off + 1] < 100,
                "Expected intact 1px red peninsula at ({x}, 200), got rgba={:?}",
                &rendered.rgba[off..off + 4]
            );
        }
    }

    #[test]
    fn potrace_opticurve_merges_convex_circle_arcs_and_rejects_s_inflections() {
        // 1. Synthetic convex circular arc nodes (quarter circle radius=100, 3 nodes) -> merges to 2
        let r = 100.0;
        let k_half = 0.2652 * r;
        let p0 = Point2D::new(r, 0.0);
        let p1 = Point2D::new(
            r * std::f64::consts::FRAC_1_SQRT_2,
            r * std::f64::consts::FRAC_1_SQRT_2,
        );
        let p2 = Point2D::new(0.0, r);
        let t1 = Vec2D::new(
            -std::f64::consts::FRAC_1_SQRT_2,
            std::f64::consts::FRAC_1_SQRT_2,
        );
        let mut convex_nodes = vec![
            PathNode::new(
                p0,
                None,
                Some(p0 + Vec2D::new(0.0, k_half)),
                NodeKind::Smooth,
            ),
            PathNode::new(
                p1,
                Some(p1 - t1 * k_half),
                Some(p1 + t1 * k_half),
                NodeKind::Smooth,
            ),
            PathNode::new(
                p2,
                Some(p2 + Vec2D::new(k_half, 0.0)),
                None,
                NodeKind::Smooth,
            ),
            PathNode::corner(Point2D::ZERO),
        ];
        potrace_opticurve_merge(&mut convex_nodes, false, 1.5);
        // Wait: potrace_opticurve_merge starts at i=1 for open; test closed 4-node-to-2-node or i=1
        let mut closed_convex = vec![
            PathNode::new(
                Point2D::new(r, 0.0),
                Some(Point2D::new(r, -k_half)),
                Some(Point2D::new(r, k_half)),
                NodeKind::Smooth,
            ),
            PathNode::new(
                p1,
                Some(p1 - t1 * k_half),
                Some(p1 + t1 * k_half),
                NodeKind::Smooth,
            ),
            PathNode::new(
                p2,
                Some(p2 + Vec2D::new(k_half, 0.0)),
                Some(p2 - Vec2D::new(k_half, 0.0)),
                NodeKind::Smooth,
            ),
            PathNode::new(
                Point2D::new(-r, 0.0),
                Some(Point2D::new(-r, k_half)),
                Some(Point2D::new(-r, -k_half)),
                NodeKind::Smooth,
            ),
        ];
        potrace_opticurve_merge(&mut closed_convex, true, 1.5);
        assert!(
            closed_convex.len() < 4,
            "convex circular arc triplet must merge"
        );

        // 2. S-curve inflection triplet (opposite cross product signs) must NOT merge
        let mut s_curve = vec![
            PathNode::corner(Point2D::new(-10.0, 0.0)),
            PathNode::new(
                Point2D::new(0.0, 0.0),
                None,
                Some(Point2D::new(3.0, 10.0)),
                NodeKind::Smooth,
            ),
            PathNode::new(
                Point2D::new(10.0, 0.0),
                Some(Point2D::new(7.0, 5.0)),
                Some(Point2D::new(13.0, -5.0)),
                NodeKind::Smooth,
            ),
            PathNode::new(
                Point2D::new(20.0, 0.0),
                Some(Point2D::new(17.0, -10.0)),
                None,
                NodeKind::Smooth,
            ),
        ];
        potrace_opticurve_merge(&mut s_curve, false, 10.0);
        assert_eq!(s_curve.len(), 4, "S-inflection must never merge");

        // 3. Real 256x256 black circle trace emits smooth cubic Bezier handles (nodes_with_handles > 0)
        let w = 256u32;
        let h = 256u32;
        let mut buf = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let dx = f64::from(x) - 127.5;
                let dy = f64::from(y) - 127.5;
                if dx * dx + dy * dy <= 90.0 * 90.0 {
                    let off = ((y * w + x) * 4) as usize;
                    buf[off..off + 4].copy_from_slice(&[0, 0, 0, 255]);
                }
            }
        }
        let asset = RasterAsset::new(w, h, buf, 96.0, ColorMode::Rgb).unwrap();
        let traced = trace_raster_to_mm(
            &asset,
            64.0,
            64.0,
            &TraceConfig {
                mode: TraceMode::OutlineTrace(OutlinePreset::Logo),
                smoothing: 0.8,
                remove_background: true,
                ..TraceConfig::default()
            },
        );
        assert_eq!(traced.len(), 1);
        let (path, _) = &traced[0];
        let total_nodes: usize = path.subpaths.iter().map(|s| s.nodes.len()).sum();
        let nodes_with_handles: usize = path
            .subpaths
            .iter()
            .flat_map(|s| s.nodes.iter())
            .filter(|n| n.handle_in.is_some() || n.handle_out.is_some())
            .count();
        assert!(
            nodes_with_handles > 0,
            "traced 256x256 circle must emit smooth cubic Bezier handles (total_nodes={total_nodes})"
        );
        assert!(
            total_nodes < 120,
            "opticurve must simplify circle below 120 nodes, got {total_nodes}"
        );
    }

    #[test]
    fn potrace_antialiased_logo_and_transparent_png_never_vanish_or_halo() {
        // 1. Transparent PNG where opaque blue badge touches ~40% of image border:
        // remove_background=true must NEVER wipe the foreground badge!
        let w = 128u32;
        let h = 128u32;
        let mut trans_buf = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 20..108 {
                if y < 96 {
                    let off = ((y * w + x) * 4) as usize;
                    trans_buf[off..off + 4].copy_from_slice(&[20, 90, 220, 255]);
                }
            }
        }
        let trans_asset = RasterAsset::new(w, h, trans_buf, 96.0, ColorMode::Rgb).unwrap();
        let trans_traced = trace_raster_to_mm(
            &trans_asset,
            128.0,
            128.0,
            &TraceConfig {
                remove_background: true,
                ..TraceConfig::default()
            },
        );
        assert_eq!(
            trans_traced.len(),
            1,
            "transparent PNG foreground touching border must never vanish"
        );

        // 2. Anti-aliased 2-color ring logo (Red + Navy on White) with max_colors = 12:
        // K-Means++ flat seeding + AA halo suppression must produce exactly 2 clean vector regions
        // (zero gray/pink onion-skin halo contours) and < 1 px radial circle accuracy!
        let mut aa_buf = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let mut r_acc = 0.0_f64;
                let mut g_acc = 0.0_f64;
                let mut b_acc = 0.0_f64;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let px = f64::from(x) + (f64::from(sx) + 0.5) * 0.25 - 64.0;
                        let py = f64::from(y) + (f64::from(sy) + 0.5) * 0.25 - 64.0;
                        let dist = px.hypot(py);
                        let rgb = if dist <= 18.0 {
                            [20.0, 45.0, 140.0]
                        } else if dist <= 44.0 {
                            [220.0, 30.0, 35.0]
                        } else {
                            [255.0, 255.0, 255.0]
                        };
                        r_acc += rgb[0];
                        g_acc += rgb[1];
                        b_acc += rgb[2];
                    }
                }
                let off = ((y * w + x) * 4) as usize;
                aa_buf[off] = (r_acc / 16.0).round() as u8;
                aa_buf[off + 1] = (g_acc / 16.0).round() as u8;
                aa_buf[off + 2] = (b_acc / 16.0).round() as u8;
                aa_buf[off + 3] = 255;
            }
        }
        let aa_asset = RasterAsset::new(w, h, aa_buf, 96.0, ColorMode::Rgb).unwrap();
        let aa_traced = trace_raster_to_mm(
            &aa_asset,
            128.0,
            128.0,
            &TraceConfig {
                mode: TraceMode::OutlineTrace(OutlinePreset::Logo),
                max_colors: 12,
                remove_background: true,
                ..TraceConfig::default()
            },
        );
        assert_eq!(
            aa_traced.len(),
            2,
            "expected exactly 2 clean regions (Red ring + Navy core) with 0 AA halo rings, got {}",
            aa_traced.len()
        );
        for (path, _) in &aa_traced {
            for sub in &path.subpaths {
                for n in &sub.nodes {
                    let r = (n.pos.x - 64.0).hypot(n.pos.y - 64.0);
                    assert!(
                        (r - 44.0).abs() < 1.2 || (r - 18.0).abs() < 1.2,
                        "circle contour anchor drifted outside subpixel tolerance: r={r}"
                    );
                }
            }
        }
    }

    #[test]
    fn potrace_coloring_book_lineart_produces_single_ultra_smooth_curve() {
        // Simulates an anti-aliased + JPEG-ringing coloring-book drawing (256x256) with concentric
        // intersecting curved strokes, traced with `Logo` preset (`max_colors = 12`).
        // Must produce 1 single crisp black vector path (0 gray halo layers!) with smooth cubic handles!
        let w = 256u32;
        let h = 256u32;
        let mut buf = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let mut luma_sum = 0.0_f64;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let px = f64::from(x) + (f64::from(sx) + 0.5) * 0.25 - 128.0;
                        let py = f64::from(y) + (f64::from(sy) + 0.5) * 0.25 - 128.0;
                        let r1 = px.hypot(py);
                        let r2 = (px - 32.0).hypot(py + 18.0);
                        let on_ring1 = (r1 - 78.0).abs() <= 3.2;
                        let on_ring2 = (r2 - 42.0).abs() <= 2.6;
                        luma_sum += if on_ring1 || on_ring2 { 10.0 } else { 252.0 };
                    }
                }
                // Add slight deterministic JPEG-like 8x8 ringing (±5 luma) on background/edges
                let ring_noise = if ((x / 4 + y / 4) & 1) == 0 {
                    3.0
                } else {
                    -4.0
                };
                let v = (luma_sum / 16.0 + ring_noise).round().clamp(0.0, 255.0) as u8;
                let off = ((y * w + x) * 4) as usize;
                buf[off..off + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let asset = RasterAsset::new(w, h, buf, 96.0, ColorMode::Rgb).unwrap();
        let traced = trace_raster_to_mm(
            &asset,
            128.0,
            128.0,
            &TraceConfig {
                mode: TraceMode::OutlineTrace(OutlinePreset::Logo),
                max_colors: 12,
                smoothing: 0.5,
                detail: 0.82,
                remove_background: true,
                ..TraceConfig::default()
            },
        );
        assert_eq!(
            traced.len(),
            1,
            "B&W coloring-book drawing must produce 1 clean ink curve (0 gray halo layers), got {}",
            traced.len()
        );
        let (path, col) = &traced[0];
        let rgba = col.to_rgba8();
        assert!(rgba[0] < 40 && rgba[1] < 40 && rgba[2] < 40);
        let total_nodes: usize = path.subpaths.iter().map(|s| s.nodes.len()).sum();
        let smooth_nodes: usize = path
            .subpaths
            .iter()
            .flat_map(|s| s.nodes.iter())
            .filter(|n| n.handle_in.is_some() || n.handle_out.is_some())
            .count();
        assert!(
            smooth_nodes * 2 >= total_nodes && total_nodes <= 120,
            "expected compact smooth Schneider Bezier curve (total_nodes={total_nodes}, smooth={smooth_nodes})"
        );
    }
}
