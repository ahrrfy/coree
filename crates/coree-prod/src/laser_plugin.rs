// SPDX-License-Identifier: MIT OR Apache-2.0

//! Production Laser Cutting, Engraving & Plotter Toolpath Engine.
//!
//! Provides industry-standard geometric preparation, toolpath planning, and
//! machine-ready exporters for CO2 lasers, fiber lasers, diode lasers, and
//! vinyl/HPGL plotters:
//! - Spatial-hashed undirected duplicate-segment removal (prevents double-burn on shared edges)
//! - Open-curve endpoint stitching with automatic reversal & loop closure
//! - Miter-limited geometric kerf compensation (`+kerf/2` outward / `-kerf/2` inward)
//! - Rotated scanline even-odd hatch fill generator (standard & 90° cross-hatch for fiber/CO2)
//! - Inner-loops-first containment sorting + nearest-neighbor travel optimization
//! - Clean exporters for GRBL 1.1 / Marlin / Ruida G-Code, AutoCAD R12 DXF, HPGL `.plt`,
//!   mm-scaled SVG, and EPSF-3.0 PostScript.

use crate::error::{ProdError, ProdResult};
use crate::scene_helpers::{collect_effective_layers, get_page, MAX_SCENE_DEPTH};
use coree_core::{
    Affine2D, BezierPath, Color, Document, Fill, Layer, NodePayload, ObjectId, Point2D, Rect2D,
    SceneObject, Vec2D,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

/// Adaptive Bézier flattening chord tolerance in millimeters (`<= 0.05 mm`).
const FLATTEN_CHORD_TOLERANCE_MM: f64 = 0.04;

/// Maximum miter extension multiplier for [`apply_kerf_offset`].
const KERF_MITER_LIMIT: f64 = 4.0;

/// Maximum scanlines generated per hatch pass (protects against adversarial tiny spacings).
const MAX_HATCH_SCANLINES: usize = 20_000;

/// Maximum total polylines processed in a single toolpath plan (OOM safety guard).
const MAX_PLAN_POLYLINES: usize = 250_000;

/// Perforation dash length (burn) and gap length (skip) in millimeters.
const PERFORATE_DASH_MM: f64 = 2.0;
const PERFORATE_GAP_MM: f64 = 1.5;

/// HPGL plotter resolution: 40 plotter units per millimeter (`1016 units/inch`).
const HPGL_UNITS_PER_MM: f64 = 40.0;

/// Millimeters-to-PostScript-points conversion factor (`72 / 25.4`).
const MM_TO_PT: f64 = 72.0 / 25.4;

/// Kind of laser / plotter machine operation assigned to a vector layer rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum LaserOperationKind {
    /// Full through-cut along vector contour.
    #[default]
    Cut,
    /// Light vector scoring / line marking along vector contour.
    VectorEngrave,
    /// Scanline hatch fill inside closed regions (Fiber / CO2 engraving).
    HatchEngrave,
    /// Dashed perforation / tear-line cut.
    Perforate,
    /// Explicitly ignored (guide/reference artwork).
    Ignore,
}

impl LaserOperationKind {
    /// Canonical CAD/SVG layer name for this operation.
    #[must_use]
    pub const fn layer_name(self) -> &'static str {
        match self {
            Self::Cut => "CUT",
            Self::VectorEngrave => "ENGRAVE",
            Self::HatchEngrave => "HATCH",
            Self::Perforate => "PERFORATE",
            Self::Ignore => "IGNORE",
        }
    }

    /// Standard AutoCAD Color Index (ACI) for DXF R12 export.
    #[must_use]
    pub const fn aci_color(self) -> u8 {
        match self {
            Self::Cut => 1,           // Red
            Self::VectorEngrave => 5, // Blue
            Self::HatchEngrave => 7,  // Black / White
            Self::Perforate => 3,     // Green
            Self::Ignore => 8,        // Gray
        }
    }

    /// Standard 1-based HPGL pen index (`SP1..SP4`).
    #[must_use]
    pub const fn hpgl_pen(self) -> u8 {
        match self {
            Self::Cut => 1,
            Self::VectorEngrave => 2,
            Self::HatchEngrave => 3,
            Self::Perforate => 4,
            Self::Ignore => 0,
        }
    }

    /// Default canonical sRGB display/export color for this operation.
    #[must_use]
    pub const fn default_rgb(self) -> [u8; 3] {
        match self {
            Self::Cut => [255, 0, 0],
            Self::VectorEngrave => [0, 0, 255],
            Self::HatchEngrave => [0, 0, 0],
            Self::Perforate => [0, 170, 0],
            Self::Ignore => [128, 128, 128],
        }
    }

    /// Execution priority: engrave first, perforate next, cut last so parts do not drop out early.
    #[must_use]
    const fn execution_priority(self) -> u8 {
        match self {
            Self::HatchEngrave => 0,
            Self::VectorEngrave => 1,
            Self::Perforate => 2,
            Self::Cut => 3,
            Self::Ignore => 4,
        }
    }
}

/// Machine bed coordinate origin convention used during G-Code export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum LaserOrigin {
    /// Origin `(0, 0)` at top-left of page (`+X` right, `+Y` down).
    #[default]
    TopLeft,
    /// Origin `(0, 0)` at bottom-left of page (`+X` right, `+Y` up — Cartesian CNC standard).
    BottomLeft,
    /// Origin `(0, 0)` at the center of the job bounding box (or page center if empty), `+Y` up.
    Center,
}

/// Target CNC/laser G-Code firmware dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum GcodeDialect {
    /// GRBL 1.1+ laser mode (`G21 G90 M4 S0`, dynamic `S0..S1000`, `M8`/`M9` air assist).
    #[default]
    Grbl11,
    /// Marlin laser firmware (`G21 G90`, `M106 S0..S255` / `M107`, `G1` moves).
    Marlin,
    /// Ruida-compatible G-Code dialect with min/max power and pulse frequency annotations.
    RuidaGcode,
}

/// Per-color / per-layer machine parameter rule matching artwork to a [`LaserOperationKind`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaserLayerRule {
    pub name: String,
    pub match_rgb: [u8; 3],
    pub color_tolerance: u8,
    pub match_cut_layer: bool,
    pub operation: LaserOperationKind,
    pub speed_mm_s: f64,
    pub min_power_pct: f64,
    pub max_power_pct: f64,
    pub passes: u32,
    pub air_assist: bool,
    pub kerf_mm: f64,
    pub z_step_mm: f64,
    pub frequency_khz: f64,
    pub hatch_spacing_mm: f64,
    pub hatch_angle_deg: f64,
    pub cross_hatch: bool,
    pub enabled: bool,
}

impl LaserLayerRule {
    /// Returns the Chebyshev (L-infinity) RGB channel distance between `rgb` and `self.match_rgb`.
    #[must_use]
    pub fn rgb_distance(&self, rgb: [u8; 3]) -> u8 {
        let dr = self.match_rgb[0].abs_diff(rgb[0]);
        let dg = self.match_rgb[1].abs_diff(rgb[1]);
        let db = self.match_rgb[2].abs_diff(rgb[2]);
        dr.max(dg).max(db)
    }

    /// Returns `true` if `rgb` falls within `self.color_tolerance` of `self.match_rgb`.
    #[must_use]
    pub fn matches_rgb(&self, rgb: [u8; 3]) -> bool {
        self.enabled && self.rgb_distance(rgb) <= self.color_tolerance
    }

    fn sanitized_speed(&self) -> f64 {
        if self.speed_mm_s.is_finite() && self.speed_mm_s > 0.0 {
            self.speed_mm_s.clamp(0.1, 10_000.0)
        } else {
            20.0
        }
    }

    fn sanitized_power(&self) -> (f64, f64) {
        let max_p = if self.max_power_pct.is_finite() {
            self.max_power_pct.clamp(0.0, 100.0)
        } else {
            50.0
        };
        let min_p = if self.min_power_pct.is_finite() {
            self.min_power_pct.clamp(0.0, max_p)
        } else {
            0.0
        };
        (min_p, max_p)
    }

    fn sanitized_passes(&self) -> u32 {
        self.passes.clamp(1, 100)
    }

    fn sanitized_z_step(&self) -> f64 {
        if self.z_step_mm.is_finite() {
            self.z_step_mm.clamp(0.0, 100.0)
        } else {
            0.0
        }
    }

    fn sanitized_freq(&self) -> f64 {
        if self.frequency_khz.is_finite() && self.frequency_khz > 0.0 {
            self.frequency_khz.clamp(0.1, 1_000.0)
        } else {
            20.0
        }
    }
}

/// Complete laser cutting / engraving job configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaserJobConfig {
    pub rules: Vec<LaserLayerRule>,
    pub origin: LaserOrigin,
    pub dedupe_tolerance_mm: f64,
    pub join_tolerance_mm: f64,
    pub inner_first: bool,
    pub optimize_travel: bool,
    pub add_weeding_box: bool,
    pub weeding_margin_mm: f64,
    pub rapid_speed_mm_s: f64,
    pub dialect: GcodeDialect,
}

impl Default for LaserJobConfig {
    fn default() -> Self {
        Self {
            rules: default_laser_layer_rules(),
            origin: LaserOrigin::BottomLeft,
            dedupe_tolerance_mm: 0.05,
            join_tolerance_mm: 0.05,
            inner_first: true,
            optimize_travel: true,
            add_weeding_box: false,
            weeding_margin_mm: 5.0,
            rapid_speed_mm_s: 300.0,
            dialect: GcodeDialect::Grbl11,
        }
    }
}

/// Returns the standard industry 4-color laser palette rules:
/// - Red `#FF0000` / `CutContour` -> Cut (`15 mm/s`, `80%`, air assist)
/// - Blue `#0000FF` -> Vector Engrave / Score (`120 mm/s`, `30%`)
/// - Black `#000000` -> Hatch Engrave (`250 mm/s`, `45%`, `0.15 mm` spacing)
/// - Green `#00AA00` -> Perforate (`40 mm/s`, `60%`, air assist)
#[must_use]
pub fn default_laser_layer_rules() -> Vec<LaserLayerRule> {
    vec![
        LaserLayerRule {
            name: "Cut (Red / CutContour)".to_owned(),
            match_rgb: [255, 0, 0],
            color_tolerance: 45,
            match_cut_layer: true,
            operation: LaserOperationKind::Cut,
            speed_mm_s: 15.0,
            min_power_pct: 60.0,
            max_power_pct: 80.0,
            passes: 1,
            air_assist: true,
            kerf_mm: 0.0,
            z_step_mm: 0.0,
            frequency_khz: 20.0,
            hatch_spacing_mm: 0.1,
            hatch_angle_deg: 0.0,
            cross_hatch: false,
            enabled: true,
        },
        LaserLayerRule {
            name: "Vector Engrave / Score (Blue)".to_owned(),
            match_rgb: [0, 0, 255],
            color_tolerance: 45,
            match_cut_layer: false,
            operation: LaserOperationKind::VectorEngrave,
            speed_mm_s: 120.0,
            min_power_pct: 20.0,
            max_power_pct: 30.0,
            passes: 1,
            air_assist: false,
            kerf_mm: 0.0,
            z_step_mm: 0.0,
            frequency_khz: 20.0,
            hatch_spacing_mm: 0.1,
            hatch_angle_deg: 0.0,
            cross_hatch: false,
            enabled: true,
        },
        LaserLayerRule {
            name: "Hatch Engrave (Black)".to_owned(),
            match_rgb: [0, 0, 0],
            color_tolerance: 45,
            match_cut_layer: false,
            operation: LaserOperationKind::HatchEngrave,
            speed_mm_s: 250.0,
            min_power_pct: 35.0,
            max_power_pct: 45.0,
            passes: 1,
            air_assist: false,
            kerf_mm: 0.0,
            z_step_mm: 0.0,
            frequency_khz: 30.0,
            hatch_spacing_mm: 0.15,
            hatch_angle_deg: 45.0,
            cross_hatch: false,
            enabled: true,
        },
        LaserLayerRule {
            name: "Perforate (Green)".to_owned(),
            match_rgb: [0, 170, 0],
            color_tolerance: 55,
            match_cut_layer: false,
            operation: LaserOperationKind::Perforate,
            speed_mm_s: 40.0,
            min_power_pct: 45.0,
            max_power_pct: 60.0,
            passes: 1,
            air_assist: true,
            kerf_mm: 0.0,
            z_step_mm: 0.0,
            frequency_khz: 20.0,
            hatch_spacing_mm: 0.1,
            hatch_angle_deg: 0.0,
            cross_hatch: false,
            enabled: true,
        },
    ]
}

/// A single ordered, machine-ready vector path segment inside [`LaserToolpathPlan`].
///
/// Coordinates in `points` are in canonical `coree` page millimeters (top-left origin, `+Y` down)
/// so GUI preview overlays can render toolpaths directly on the page canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolpathSegment {
    pub operation: LaserOperationKind,
    pub rule_name: String,
    pub color_rgb: [u8; 3],
    pub points: Vec<Point2D>,
    pub closed: bool,
    pub speed_mm_s: f64,
    pub min_power_pct: f64,
    pub max_power_pct: f64,
    pub passes: u32,
    pub air_assist: bool,
    pub z_step_mm: f64,
    pub frequency_khz: f64,
    pub containment_depth: usize,
    pub is_weeding_box: bool,
}

/// Complete planned laser / plotter toolpath with travel statistics and ordered segments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaserToolpathPlan {
    pub page_index: usize,
    pub page_width_mm: f64,
    pub page_height_mm: f64,
    pub bounds: Option<Rect2D>,
    pub segments: Vec<ToolpathSegment>,
    pub total_cut_mm: f64,
    pub total_engrave_mm: f64,
    pub total_rapid_mm: f64,
    pub estimated_seconds: f64,
    pub cut_count: usize,
    pub duplicate_segments_removed: usize,
    pub curves_joined: usize,
}

// =============================================================================
// 1. Spatial-Grid Undirected Duplicate Segment Removal
// =============================================================================

#[inline]
fn grid_cell_coord(val: f64, cell_size: f64) -> i64 {
    let q = (val / cell_size).clamp(-1e15, 1e15).floor();
    q as i64
}

#[inline]
fn clean_finite_polyline(poly: &[Point2D]) -> Vec<Point2D> {
    let mut out = Vec::with_capacity(poly.len());
    for &p in poly {
        if !p.is_finite() {
            continue;
        }
        if out
            .last()
            .is_none_or(|prev: &Point2D| prev.distance_to(p) > 1e-9)
        {
            out.push(p);
        }
    }
    out
}

/// Removes duplicate undirected line segments (`(p0, p1) == (p1, p0)` within `tolerance_mm`)
/// across `polylines` using 9-neighborhood spatial grid hashing of segment midpoints.
///
/// Adjacent laser-cut parts sharing an edge will cut that shared edge exactly once instead
/// of double-burning the material. Returns `(deduped_polylines, duplicate_segments_removed)`.
#[must_use]
pub fn remove_duplicate_segments(
    polylines: &[Vec<Point2D>],
    tolerance_mm: f64,
) -> (Vec<Vec<Point2D>>, usize) {
    if !tolerance_mm.is_finite() || tolerance_mm <= 0.0 {
        let cleaned: Vec<Vec<Point2D>> = polylines
            .iter()
            .map(|p| clean_finite_polyline(p))
            .filter(|p| p.len() >= 2)
            .collect();
        return (cleaned, 0);
    }

    let tol = tolerance_mm.max(1e-6);
    // Cell size >= tol guarantees any two segments whose endpoints are within `tol` have
    // midpoints within `tol <= cell_size`, so they ALWAYS fall in the same or 8-neighbor cell.
    let cell_size = (tol * 2.0).max(1e-4);
    type SegEntry = (Point2D, Point2D, f64);
    let mut grid: HashMap<(i64, i64), Vec<SegEntry>> = HashMap::new();
    let mut result: Vec<Vec<Point2D>> = Vec::with_capacity(polylines.len());
    let mut duplicates_removed = 0usize;

    for raw_poly in polylines {
        let poly = clean_finite_polyline(raw_poly);
        if poly.len() < 2 {
            continue;
        }
        let was_closed = is_genuine_closed_loop(&poly, tol.max(1e-6));

        let mut runs: Vec<Vec<Point2D>> = Vec::new();
        let mut current: Vec<Point2D> = Vec::new();

        for win in poly.windows(2) {
            let p0 = win[0];
            let p1 = win[1];
            let len_p = p0.distance_to(p1);
            if !len_p.is_finite() || len_p <= 1e-9 {
                continue;
            }
            let mx = 0.5 * (p0.x + p1.x);
            let my = 0.5 * (p0.y + p1.y);
            if !mx.is_finite() || !my.is_finite() {
                continue;
            }
            let cx = grid_cell_coord(mx, cell_size);
            let cy = grid_cell_coord(my, cell_size);

            let mut is_dup = false;
            'neighbor_search: for dx in -1..=1 {
                for dy in -1..=1 {
                    if let Some(bucket) = grid.get(&(cx + dx, cy + dy)) {
                        for &(q0, q1, len_q) in bucket {
                            // Bound effective endpoint match tolerance strictly below half the shorter
                            // segment length so consecutive short chords (A->B, B->C) on a single curve
                            // can NEVER falsely match each other!
                            let eff_tol = tol.min(len_p * 0.45).min(len_q * 0.45);
                            let same_dir =
                                p0.distance_to(q0) <= eff_tol && p1.distance_to(q1) <= eff_tol;
                            let rev_dir =
                                p0.distance_to(q1) <= eff_tol && p1.distance_to(q0) <= eff_tol;
                            if same_dir || rev_dir {
                                is_dup = true;
                                break 'neighbor_search;
                            }
                        }
                    }
                }
            }

            if is_dup {
                duplicates_removed += 1;
                if current.len() >= 2 {
                    runs.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            } else {
                grid.entry((cx, cy)).or_default().push((p0, p1, len_p));
                if current.is_empty() {
                    current.push(p0);
                    current.push(p1);
                } else {
                    current.push(p1);
                }
            }
        }

        if current.len() >= 2 {
            runs.push(current);
        }

        // If a closed loop lost a middle segment, stitch the wrap-around tail + head runs into one continuous run.
        if was_closed && runs.len() >= 2 {
            let can_wrap = runs
                .last()
                .and_then(|last| last.last())
                .zip(runs.first().and_then(|first| first.first()))
                .is_some_and(|(&tail_end, &head_start)| {
                    tail_end.distance_to(head_start) <= tol.max(1e-6)
                });
            if can_wrap {
                let mut merged = runs.pop().unwrap_or_default();
                let first_run = runs.remove(0);
                merged.extend(first_run.into_iter().skip(1));
                runs.insert(0, merged);
            }
        }

        result.extend(runs);
    }

    (result, duplicates_removed)
}

// =============================================================================
// 2. Open-Curve Endpoint Stitching & Loop Closure
// =============================================================================

#[inline]
fn is_polyline_closed(poly: &[Point2D], tol: f64) -> bool {
    poly.len() >= 4 && poly[0].distance_to(poly[poly.len() - 1]) <= tol.max(1e-6)
}

/// Returns `true` only when `poly` forms a genuine non-degenerate closed loop (`len >= 4`,
/// endpoints within `tol`, AND the curve genuinely leaves the `tol`-radius ball around `poly[0]`
/// with total arc length `> 2.5 * tol`), preventing short consecutive chords from collapsing early.
fn is_genuine_closed_loop(poly: &[Point2D], tol: f64) -> bool {
    let eps = tol.max(1e-6);
    if !is_polyline_closed(poly, eps) {
        return false;
    }
    let first = poly[0];
    let max_excursion = poly[1..poly.len() - 1]
        .iter()
        .map(|&p| first.distance_to(p))
        .filter(|d| d.is_finite())
        .fold(0.0_f64, f64::max);
    let arc_len = polyline_length_mm(poly);
    max_excursion > eps && arc_len > 2.5 * eps
}

/// Stitches open polylines whose endpoints fall within `tolerance_mm` (reversing curve
/// orientation as needed) into continuous paths and closed cutting loops.
///
/// Returns `(joined_polylines, curves_joined)` where `curves_joined` counts the number of
/// open-curve merge operations performed.
#[must_use]
pub fn join_open_curves(
    polylines: &[Vec<Point2D>],
    tolerance_mm: f64,
) -> (Vec<Vec<Point2D>>, usize) {
    let mut already_closed: Vec<Vec<Point2D>> = Vec::new();
    let mut open_chains: Vec<Option<Vec<Point2D>>> = Vec::new();

    let clean_tol = if tolerance_mm.is_finite() && tolerance_mm > 0.0 {
        tolerance_mm
    } else {
        0.0
    };

    for raw in polylines {
        let poly = clean_finite_polyline(raw);
        if poly.len() < 2 {
            continue;
        }
        if is_polyline_closed(&poly, 1e-6) {
            already_closed.push(poly);
        } else {
            open_chains.push(Some(poly));
        }
    }

    if clean_tol <= 0.0 || open_chains.is_empty() {
        let mut out = already_closed;
        out.extend(open_chains.into_iter().flatten());
        return (out, 0);
    }

    let cell_size = (clean_tol * 2.0).max(1e-4);
    // Spatial hash mapping endpoint grid cell -> list of (chain_idx, is_tail)
    let mut endpoint_grid: HashMap<(i64, i64), Vec<(usize, bool)>> = HashMap::new();

    let register_endpoints =
        |idx: usize, chain: &[Point2D], grid: &mut HashMap<(i64, i64), Vec<(usize, bool)>>| {
            let start = chain[0];
            let end = chain[chain.len() - 1];
            let cs = (
                grid_cell_coord(start.x, cell_size),
                grid_cell_coord(start.y, cell_size),
            );
            let ce = (
                grid_cell_coord(end.x, cell_size),
                grid_cell_coord(end.y, cell_size),
            );
            grid.entry(cs).or_default().push((idx, false));
            grid.entry(ce).or_default().push((idx, true));
        };

    for (idx, chain_opt) in open_chains.iter().enumerate() {
        if let Some(chain) = chain_opt {
            register_endpoints(idx, chain, &mut endpoint_grid);
        }
    }

    let find_best_neighbor = |pt: Point2D,
                              self_idx: usize,
                              chains: &[Option<Vec<Point2D>>],
                              grid: &HashMap<(i64, i64), Vec<(usize, bool)>>|
     -> Option<(usize, bool)> {
        let cx = grid_cell_coord(pt.x, cell_size);
        let cy = grid_cell_coord(pt.y, cell_size);
        let mut best: Option<(usize, bool, f64)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                let Some(bucket) = grid.get(&(cx + dx, cy + dy)) else {
                    continue;
                };
                for &(cand_idx, is_tail) in bucket {
                    if cand_idx == self_idx {
                        continue;
                    }
                    let Some(ref cand_chain) = chains[cand_idx] else {
                        continue;
                    };
                    let cand_pt = if is_tail {
                        cand_chain[cand_chain.len() - 1]
                    } else {
                        cand_chain[0]
                    };
                    let d = pt.distance_to(cand_pt);
                    if d <= clean_tol && best.is_none_or(|(_, _, bd)| d < bd) {
                        best = Some((cand_idx, is_tail, d));
                    }
                }
            }
        }
        best.map(|(i, t, _)| (i, t))
    };

    let mut joined_count = 0usize;
    let mut finished_open: Vec<Vec<Point2D>> = Vec::new();

    for i in 0..open_chains.len() {
        let Some(mut chain) = open_chains[i].take() else {
            continue;
        };

        // Grow tail repeatedly
        while let Some((other_idx, matched_tail)) =
            find_best_neighbor(chain[chain.len() - 1], i, &open_chains, &endpoint_grid)
        {
            let Some(mut other) = open_chains[other_idx].take() else {
                continue;
            };
            if matched_tail {
                other.reverse();
            }
            // Snap seam cleanly without duplicating identical vertex
            chain.extend(other.into_iter().skip(1));
            joined_count += 1;
            if is_genuine_closed_loop(&chain, clean_tol) {
                break;
            }
        }

        // Grow head repeatedly (if not already closed into a genuine loop)
        if !is_genuine_closed_loop(&chain, clean_tol) {
            while let Some((other_idx, matched_tail)) =
                find_best_neighbor(chain[0], i, &open_chains, &endpoint_grid)
            {
                let Some(mut other) = open_chains[other_idx].take() else {
                    continue;
                };
                if !matched_tail {
                    other.reverse();
                }
                // `other` tail now matches `chain` head
                other.extend(chain.into_iter().skip(1));
                chain = other;
                joined_count += 1;
                if is_genuine_closed_loop(&chain, clean_tol) {
                    break;
                }
            }
        }

        // Close loop cleanly only if start and end meet after a genuine excursion
        if is_genuine_closed_loop(&chain, clean_tol) {
            let first = chain[0];
            let last_idx = chain.len() - 1;
            let dist = first.distance_to(chain[last_idx]);
            if dist > 1e-9 {
                chain[last_idx] = first;
            }
        }

        finished_open.push(chain);
    }

    already_closed.extend(finished_open);
    (already_closed, joined_count)
}

// =============================================================================
// 3. Miter-Limited Geometric Kerf Compensation Offset
// =============================================================================

#[inline]
fn signed_polygon_area(ring: &[Point2D]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    let n = ring.len();
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        sum += a.x * b.y - b.x * a.y;
    }
    0.5 * sum
}

/// Applies a miter-limited geometric normal offset of `kerf_mm / 2.0` to `polyline`.
///
/// - For **closed** polygons (`closed == true`), positive `kerf_mm` offsets **outward** by
///   `+kerf_mm / 2` (compensating outer part perimeters) and negative `kerf_mm` offsets
///   **inward** by `-|kerf_mm| / 2` (compensating inner holes/slots), independent of CW/CCW
///   input winding.
/// - For **open** polylines (`closed == false`), offsets consistently along the left normal
///   of the traversal direction by `kerf_mm / 2`.
/// - Sharp acute corners exceeding [`KERF_MITER_LIMIT`] (`4.0`) are cleanly beveled.
#[must_use]
pub fn apply_kerf_offset(polyline: &[Point2D], closed: bool, kerf_mm: f64) -> Vec<Point2D> {
    let clean = clean_finite_polyline(polyline);
    if clean.len() < 2 || !kerf_mm.is_finite() || kerf_mm.abs() <= 1e-9 {
        return clean;
    }

    let half = kerf_mm * 0.5;

    if closed {
        let had_closing_dup =
            clean.len() >= 3 && clean[0].distance_to(clean[clean.len() - 1]) <= 1e-6;
        let ring: &[Point2D] = if had_closing_dup {
            &clean[..clean.len() - 1]
        } else {
            &clean
        };
        if ring.len() < 3 {
            return clean;
        }

        let area = signed_polygon_area(ring);
        // Universal outward normal sign invariant across both Y-up and Y-down coordinate systems:
        // If signed_area >= 0, outward normal of unit tangent (tx, ty) is (ty, -tx).
        // If signed_area < 0, outward normal is (-ty, tx).
        let normal_sign = if area >= 0.0 { 1.0 } else { -1.0 };

        let n = ring.len();
        let mut edge_normals: Vec<Vec2D> = Vec::with_capacity(n);
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            let edge = b - a;
            let unit = edge.normalized().unwrap_or(Vec2D::new(1.0, 0.0));
            edge_normals.push(Vec2D::new(normal_sign * unit.y, -normal_sign * unit.x));
        }

        let mut out: Vec<Point2D> = Vec::with_capacity(n + 4);
        for i in 0..n {
            let v = ring[i];
            let n_prev = edge_normals[(i + n - 1) % n];
            let n_next = edge_normals[i];
            emit_miter_or_bevel(v, n_prev, n_next, half, &mut out);
        }

        if !out.is_empty() && (had_closing_dup || closed) {
            let first = out[0];
            if out
                .last()
                .is_some_and(|last| last.distance_to(first) > 1e-9)
            {
                out.push(first);
            }
        }
        out
    } else {
        let seg_count = clean.len() - 1;
        let mut edge_normals: Vec<Vec2D> = Vec::with_capacity(seg_count);
        for i in 0..seg_count {
            let edge = clean[i + 1] - clean[i];
            let unit = edge.normalized().unwrap_or(Vec2D::new(1.0, 0.0));
            edge_normals.push(Vec2D::new(unit.y, -unit.x));
        }

        let mut out: Vec<Point2D> = Vec::with_capacity(clean.len() + 2);
        out.push(clean[0] + edge_normals[0] * half);
        for i in 1..seg_count {
            emit_miter_or_bevel(
                clean[i],
                edge_normals[i - 1],
                edge_normals[i],
                half,
                &mut out,
            );
        }
        out.push(clean[seg_count] + edge_normals[seg_count - 1] * half);
        out
    }
}

fn emit_miter_or_bevel(
    v: Point2D,
    n_prev: Vec2D,
    n_next: Vec2D,
    offset: f64,
    out: &mut Vec<Point2D>,
) {
    let denom = 1.0 + n_prev.dot(n_next);
    if denom > 1e-6 {
        let bisector = n_prev + n_next;
        let miter = bisector * (offset / denom);
        if miter.is_finite() && miter.length() <= KERF_MITER_LIMIT * offset.abs() {
            out.push(v + miter);
            return;
        }
    }
    // Miter exceeded limit (acute corner or 180-degree reversal): emit clean bevel pair
    let p_prev = v + n_prev * offset;
    let p_next = v + n_next * offset;
    if p_prev.is_finite() {
        out.push(p_prev);
    }
    if p_next.is_finite() && p_prev.distance_to(p_next) > 1e-9 {
        out.push(p_next);
    }
}

// =============================================================================
// 4. Rotated Scanline Even-Odd Hatch Fill Generator (Fiber & CO2 Engraving)
// =============================================================================

/// Exact point-in-polygon test using half-open ray casting (handles vertices & horizontal edges).
fn point_in_polygon(pt: Point2D, ring: &[Point2D]) -> bool {
    if ring.len() < 3 || !pt.is_finite() {
        return false;
    }
    let mut inside = false;
    let n = ring.len();
    let mut j = n - 1;
    for i in 0..n {
        let pi = ring[i];
        let pj = ring[j];
        let intersects_y = (pi.y > pt.y) != (pj.y > pt.y);
        if intersects_y {
            let x_cross = pj.x + (pt.y - pj.y) * (pi.x - pj.x) / (pi.y - pj.y);
            if pt.x < x_cross {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

fn ring_without_closing_dup(poly: &[Point2D]) -> Vec<Point2D> {
    let mut clean = clean_finite_polyline(poly);
    if clean.len() >= 3 && clean[0].distance_to(clean[clean.len() - 1]) <= 1e-6 {
        clean.pop();
    }
    clean
}

fn ring_bbox(ring: &[Point2D]) -> Option<Rect2D> {
    let first = *ring.first()?;
    let mut bb = Rect2D::new(first.x, first.y, first.x, first.y);
    for &p in &ring[1..] {
        bb.min_x = bb.min_x.min(p.x);
        bb.min_y = bb.min_y.min(p.y);
        bb.max_x = bb.max_x.max(p.x);
        bb.max_y = bb.max_y.max(p.y);
    }
    bb.is_finite().then_some(bb)
}

fn compute_ring_containment_depths(rings: &[Vec<Point2D>]) -> Vec<usize> {
    let n = rings.len();
    let areas: Vec<f64> = rings.iter().map(|r| signed_polygon_area(r).abs()).collect();
    let bboxes: Vec<Option<Rect2D>> = rings.iter().map(|r| ring_bbox(r)).collect();
    let mut depths = vec![0usize; n];

    for i in 0..n {
        if rings[i].len() < 3 {
            continue;
        }
        let test_pt = rings[i][0];
        let mut depth = 0usize;
        for j in 0..n {
            if i == j || rings[j].len() < 3 || areas[j] <= areas[i] + 1e-6 {
                continue;
            }
            if let Some(bb_j) = bboxes[j] {
                if !bb_j.contains_point(test_pt) {
                    continue;
                }
            }
            if point_in_polygon(test_pt, &rings[j]) {
                depth += 1;
            }
        }
        depths[i] = depth;
    }
    depths
}

/// Generates parallel engraving hatch fill segments (and optional 90° cross-hatch)
/// inside `closed_polylines` using exact rotated scanline even-odd polygon clipping.
///
/// - Handles arbitrary concave shapes, multi-island contours, and nested holes via even-odd parity.
/// - Alternates scanline direction (boustrophedon zig-zag) to minimize rapid travel during engraving.
/// - Optional `inset_mm > 0.0` shrinks outer contours inward and expands inner holes outward so
///   hatch lines remain safely inside cut edges.
#[must_use]
pub fn generate_hatch_fill(
    closed_polylines: &[Vec<Point2D>],
    spacing_mm: f64,
    angle_deg: f64,
    cross_hatch: bool,
    inset_mm: f64,
) -> Vec<(Point2D, Point2D)> {
    if closed_polylines.is_empty() || !spacing_mm.is_finite() || spacing_mm <= 0.0 {
        return Vec::new();
    }
    let spacing = spacing_mm.max(0.01);
    let base_angle = if angle_deg.is_finite() {
        angle_deg
    } else {
        0.0
    };
    let inset = if inset_mm.is_finite() && inset_mm > 0.0 {
        inset_mm
    } else {
        0.0
    };

    let raw_rings: Vec<Vec<Point2D>> = closed_polylines
        .iter()
        .map(|p| ring_without_closing_dup(p))
        .filter(|r| r.len() >= 3 && signed_polygon_area(r).abs() > 1e-9)
        .collect();
    if raw_rings.is_empty() {
        return Vec::new();
    }

    let effective_rings: Vec<Vec<Point2D>> = if inset > 1e-6 {
        let depths = compute_ring_containment_depths(&raw_rings);
        raw_rings
            .iter()
            .zip(depths.iter())
            .filter_map(|(ring, &depth)| {
                let orig_area = signed_polygon_area(ring);
                // Outer contours (even depth) shrink inward (-2*inset); holes (odd depth) expand (+2*inset).
                let kerf_arg = if depth % 2 == 0 {
                    -2.0 * inset
                } else {
                    2.0 * inset
                };
                let shifted = ring_without_closing_dup(&apply_kerf_offset(ring, true, kerf_arg));
                if shifted.len() < 3 {
                    return None;
                }
                let new_area = signed_polygon_area(&shifted);
                // Reject polygons that inverted/collapsed when inset exceeded their half-width
                if new_area.abs() <= 1e-9 || (orig_area * new_area < 0.0) {
                    return None;
                }
                Some(shifted)
            })
            .collect()
    } else {
        raw_rings
    };

    if effective_rings.is_empty() {
        return Vec::new();
    }

    let mut segments = hatch_pass_at_angle(&effective_rings, spacing, base_angle);
    if cross_hatch {
        segments.extend(hatch_pass_at_angle(
            &effective_rings,
            spacing,
            base_angle + 90.0,
        ));
    }
    segments
}

fn hatch_pass_at_angle(
    rings: &[Vec<Point2D>],
    spacing_mm: f64,
    angle_deg: f64,
) -> Vec<(Point2D, Point2D)> {
    let rad = angle_deg.to_radians();
    let cos_a = rad.cos();
    let sin_a = rad.sin();

    // Rotate polygons by -angle into scanline-aligned space where hatch lines are horizontal (constant ry)
    let mut rotated_rings: Vec<Vec<Point2D>> = Vec::with_capacity(rings.len());
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for ring in rings {
        let mut rot = Vec::with_capacity(ring.len());
        for &p in ring {
            let rx = p.x * cos_a + p.y * sin_a;
            let ry = -p.x * sin_a + p.y * cos_a;
            if rx.is_finite() && ry.is_finite() {
                min_y = min_y.min(ry);
                max_y = max_y.max(ry);
                rot.push(Point2D::new(rx, ry));
            }
        }
        if rot.len() >= 3 {
            rotated_rings.push(rot);
        }
    }

    if rotated_rings.is_empty() || !min_y.is_finite() || !max_y.is_finite() || max_y <= min_y {
        return Vec::new();
    }

    let span_y = max_y - min_y;
    if !span_y.is_finite() || span_y <= 0.0 {
        return Vec::new();
    }
    let est_rows = span_y / spacing_mm;
    let effective_spacing = if !est_rows.is_finite() || est_rows > (MAX_HATCH_SCANLINES as f64) {
        let s = span_y / (MAX_HATCH_SCANLINES as f64);
        if !s.is_finite() || s <= 0.0 {
            return Vec::new();
        }
        s
    } else {
        spacing_mm
    };

    let mut out: Vec<(Point2D, Point2D)> = Vec::new();
    let mut intersections: Vec<f64> = Vec::new();
    let mut row_idx = 0usize;
    let mut y = min_y + 0.5 * effective_spacing;

    while y.is_finite()
        && y < max_y
        && row_idx < MAX_HATCH_SCANLINES
        && out.len() < MAX_PLAN_POLYLINES
    {
        intersections.clear();
        for ring in &rotated_rings {
            let n = ring.len();
            for i in 0..n {
                let p0 = ring[i];
                let p1 = ring[(i + 1) % n];
                // Half-open interval [min_y, max_y) guarantees exact even-odd parity at shared vertices
                if (p0.y <= y && p1.y > y) || (p1.y <= y && p0.y > y) {
                    let t = (y - p0.y) / (p1.y - p0.y);
                    let x_hit = p0.x + t * (p1.x - p0.x);
                    if x_hit.is_finite() {
                        intersections.push(x_hit);
                    }
                }
            }
        }

        if intersections.len() >= 2 {
            intersections.sort_by(f64::total_cmp);
            let mut row_segs: Vec<(Point2D, Point2D)> = Vec::with_capacity(intersections.len() / 2);
            for &[x0, x1] in intersections.as_chunks::<2>().0 {
                if x1 - x0 <= 1e-6 {
                    continue;
                }
                // Rotate back to world space by +angle
                let a = Point2D::new(x0 * cos_a - y * sin_a, x0 * sin_a + y * cos_a);
                let b = Point2D::new(x1 * cos_a - y * sin_a, x1 * sin_a + y * cos_a);
                if a.is_finite() && b.is_finite() {
                    row_segs.push((a, b));
                }
            }

            // Boustrophedon (zig-zag) scan order cuts rapid travel between adjacent hatch lines in half
            if row_idx % 2 == 1 {
                for (a, b) in row_segs.into_iter().rev() {
                    out.push((b, a));
                }
            } else {
                out.extend(row_segs);
            }
        }

        row_idx += 1;
        y = min_y + (row_idx as f64 + 0.5) * effective_spacing;
    }

    out
}

// =============================================================================
// 5. Full Laser Toolpath Planner (Inner-First + Nearest-Neighbor Optimization)
// =============================================================================

#[derive(Debug, Clone)]
struct ExtractedPolyline {
    points: Vec<Point2D>,
    closed: bool,
}

/// Plans a complete, optimized laser / plotter toolpath for `doc.pages[page_idx]`.
///
/// Pipeline:
/// 1. Extracts visible vector geometry respecting nested `Affine2D` world transforms, hidden
///    layers, hidden objects, and optional `selection_filter`.
/// 2. Adaptively flattens Bézier curves (`<= 0.04 mm` chord error).
/// 3. Classifies contours into matching [`LaserLayerRule`]s (by `is_cut_layer` / `CutContour`
///    spot ink, stroke RGB, and fill RGB).
/// 4. Runs duplicate-segment removal, open-curve stitching, inner-loop containment analysis,
///    kerf offset compensation, hatch fill generation, perforation dashing, and optional weeding box.
/// 5. Orders operations (`HatchEngrave` -> `VectorEngrave` -> `Perforate` -> `Cut` -> weeding box),
///    cuts deeper inner loops before outer perimeters (`inner_first`), and applies nearest-neighbor
///    travel minimization (`optimize_travel`) reversing open paths and rotating closed start vertices.
pub fn plan_laser_toolpath(
    doc: &Document,
    page_idx: usize,
    selection_filter: Option<&[ObjectId]>,
    config: &LaserJobConfig,
) -> ProdResult<LaserToolpathPlan> {
    let page = get_page(doc, page_idx)?;
    if !page.width_mm.is_finite()
        || page.width_mm <= 0.0
        || !page.height_mm.is_finite()
        || page.height_mm <= 0.0
    {
        return Err(ProdError::InvalidParameter(
            "page dimensions must be positive and finite for laser toolpath planning".into(),
        ));
    }
    let page_width_mm = page.width_mm;
    let page_height_mm = page.height_mm;

    let dedupe_tol = if config.dedupe_tolerance_mm.is_finite() && config.dedupe_tolerance_mm > 0.0 {
        config.dedupe_tolerance_mm
    } else {
        0.0
    };
    let join_tol = if config.join_tolerance_mm.is_finite() && config.join_tolerance_mm > 0.0 {
        config.join_tolerance_mm
    } else {
        0.0
    };
    let rapid_speed = if config.rapid_speed_mm_s.is_finite() && config.rapid_speed_mm_s > 0.0 {
        config.rapid_speed_mm_s.clamp(1.0, 20_000.0)
    } else {
        300.0
    };

    let sel_set: Option<HashSet<ObjectId>> =
        selection_filter.map(|ids| ids.iter().copied().collect());

    // Bucket index: Some(rule_idx) for explicit rules, None for unmatched fallback vector engrave
    let mut rule_buckets: Vec<Vec<ExtractedPolyline>> = vec![Vec::new(); config.rules.len()];
    let mut fallback_cut_bucket: Vec<ExtractedPolyline> = Vec::new();
    let mut fallback_engrave_bucket: Vec<ExtractedPolyline> = Vec::new();
    let mut total_extracted = 0usize;

    let effective_layers = collect_effective_layers(doc, page_idx);
    for layer in effective_layers {
        if !layer.visible {
            continue;
        }
        let mut visiting = HashSet::new();
        for &oid in &layer.objects {
            collect_laser_polylines_recursive(
                doc,
                layer,
                oid,
                Affine2D::IDENTITY,
                sel_set.as_ref(),
                false,
                0,
                &mut visiting,
                &config.rules,
                &mut rule_buckets,
                &mut fallback_cut_bucket,
                &mut fallback_engrave_bucket,
                &mut total_extracted,
            );
        }
    }

    let mut segments: Vec<ToolpathSegment> = Vec::new();
    let mut duplicate_segments_removed = 0usize;
    let mut curves_joined = 0usize;

    // Process each configured rule bucket
    for (rule_idx, rule) in config.rules.iter().enumerate() {
        if !rule.enabled || rule.operation == LaserOperationKind::Ignore {
            continue;
        }
        let extracted = &rule_buckets[rule_idx];
        if extracted.is_empty() {
            continue;
        }
        process_rule_bucket(
            rule,
            extracted,
            dedupe_tol,
            join_tol,
            &mut segments,
            &mut duplicate_segments_removed,
            &mut curves_joined,
        );
    }

    // Process fallback buckets (unmatched hairline/cut-layer -> Cut, other unmatched visible artwork -> VectorEngrave)
    if !fallback_cut_bucket.is_empty() {
        let fallback_cut_rule = config
            .rules
            .iter()
            .find(|r| r.enabled && r.operation == LaserOperationKind::Cut)
            .cloned()
            .unwrap_or_else(|| default_laser_layer_rules()[0].clone());
        process_rule_bucket(
            &fallback_cut_rule,
            &fallback_cut_bucket,
            dedupe_tol,
            join_tol,
            &mut segments,
            &mut duplicate_segments_removed,
            &mut curves_joined,
        );
    }
    if !fallback_engrave_bucket.is_empty() {
        let fallback_eng_rule = LaserLayerRule {
            name: "Unmatched Vector Engrave".to_owned(),
            match_rgb: [0, 0, 255],
            color_tolerance: 0,
            match_cut_layer: false,
            operation: LaserOperationKind::VectorEngrave,
            speed_mm_s: 120.0,
            min_power_pct: 20.0,
            max_power_pct: 30.0,
            passes: 1,
            air_assist: false,
            kerf_mm: 0.0,
            z_step_mm: 0.0,
            frequency_khz: 20.0,
            hatch_spacing_mm: 0.15,
            hatch_angle_deg: 45.0,
            cross_hatch: false,
            enabled: true,
        };
        process_rule_bucket(
            &fallback_eng_rule,
            &fallback_engrave_bucket,
            dedupe_tol,
            join_tol,
            &mut segments,
            &mut duplicate_segments_removed,
            &mut curves_joined,
        );
    }

    // Recompute cross-segment containment depth across ALL Cut segments (handles holes from different rules!)
    update_global_cut_containment_depths(&mut segments);

    // Optional weeding box surrounding all non-weeding geometry (always cut very last!)
    if config.add_weeding_box {
        if let Some(job_bb) = compute_segments_bounds(&segments) {
            let margin = if config.weeding_margin_mm.is_finite() {
                config.weeding_margin_mm.max(0.0)
            } else {
                5.0
            };
            let wb = job_bb.expand(margin);
            let cut_rule = config
                .rules
                .iter()
                .find(|r| r.enabled && r.operation == LaserOperationKind::Cut)
                .cloned()
                .unwrap_or_else(|| default_laser_layer_rules()[0].clone());
            let (min_p, max_p) = cut_rule.sanitized_power();
            let corners = wb.corners();
            let box_pts = vec![corners[0], corners[1], corners[2], corners[3], corners[0]];
            segments.push(ToolpathSegment {
                operation: LaserOperationKind::Cut,
                rule_name: "Weeding Box".to_owned(),
                color_rgb: LaserOperationKind::Cut.default_rgb(),
                points: box_pts,
                closed: true,
                speed_mm_s: cut_rule.sanitized_speed(),
                min_power_pct: min_p,
                max_power_pct: max_p,
                passes: cut_rule.sanitized_passes(),
                air_assist: cut_rule.air_assist,
                z_step_mm: cut_rule.sanitized_z_step(),
                frequency_khz: cut_rule.sanitized_freq(),
                containment_depth: 0,
                is_weeding_box: true,
            });
        }
    }

    // Order operations, apply inner-first depth priority, and optimize rapid travel
    let initial_bounds = compute_segments_bounds(&segments);
    let home =
        origin_home_in_page_coords(page_width_mm, page_height_mm, initial_bounds, config.origin);
    let ordered =
        order_and_optimize_segments(segments, config.inner_first, config.optimize_travel, home);

    // Compute final job metrics and time estimation
    let bounds = compute_segments_bounds(&ordered);
    let mut total_cut_mm = 0.0_f64;
    let mut total_engrave_mm = 0.0_f64;
    let mut total_rapid_mm = 0.0_f64;
    let mut estimated_seconds = 0.0_f64;
    let mut cut_count = 0usize;

    let mut cursor =
        origin_home_in_page_coords(page_width_mm, page_height_mm, bounds, config.origin);

    for seg in &ordered {
        if seg.points.len() < 2 {
            continue;
        }
        let start = seg.points[0];
        let end = seg.points[seg.points.len() - 1];
        let rapid_to_start = cursor.distance_to(start);
        if rapid_to_start.is_finite() {
            total_rapid_mm += rapid_to_start;
            estimated_seconds += rapid_to_start / rapid_speed;
        }

        let path_len = polyline_length_mm(&seg.points);
        let passes = seg.passes.max(1);
        let work_len = path_len * f64::from(passes);

        match seg.operation {
            LaserOperationKind::Cut | LaserOperationKind::Perforate => {
                total_cut_mm += work_len;
                cut_count += 1;
            }
            LaserOperationKind::VectorEngrave | LaserOperationKind::HatchEngrave => {
                total_engrave_mm += work_len;
            }
            LaserOperationKind::Ignore => {}
        }

        estimated_seconds += work_len / seg.speed_mm_s.max(0.1);

        // Multi-pass open paths rapid back from end to start between passes
        if passes > 1 && !seg.closed {
            let return_dist = end.distance_to(start) * f64::from(passes - 1);
            if return_dist.is_finite() {
                total_rapid_mm += return_dist;
                estimated_seconds += return_dist / rapid_speed;
            }
        }
        cursor = end;
    }

    Ok(LaserToolpathPlan {
        page_index: page_idx,
        page_width_mm,
        page_height_mm,
        bounds,
        segments: ordered,
        total_cut_mm,
        total_engrave_mm,
        total_rapid_mm,
        estimated_seconds,
        cut_count,
        duplicate_segments_removed,
        curves_joined,
    })
}

fn origin_home_in_page_coords(
    page_w: f64,
    page_h: f64,
    bounds: Option<Rect2D>,
    origin: LaserOrigin,
) -> Point2D {
    match origin {
        LaserOrigin::TopLeft => Point2D::ZERO,
        LaserOrigin::BottomLeft => Point2D::new(0.0, page_h),
        LaserOrigin::Center => bounds
            .map(|b| b.center())
            .unwrap_or_else(|| Point2D::new(page_w * 0.5, page_h * 0.5)),
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_laser_polylines_recursive(
    doc: &Document,
    layer: &Layer,
    id: ObjectId,
    parent_xf: Affine2D,
    sel_set: Option<&HashSet<ObjectId>>,
    ancestor_selected: bool,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    rules: &[LaserLayerRule],
    rule_buckets: &mut [Vec<ExtractedPolyline>],
    fallback_cut: &mut Vec<ExtractedPolyline>,
    fallback_engrave: &mut Vec<ExtractedPolyline>,
    total_extracted: &mut usize,
) {
    if depth > MAX_SCENE_DEPTH || *total_extracted >= MAX_PLAN_POLYLINES || !visiting.insert(id) {
        return;
    }

    if let Some(obj) = doc.object(id) {
        if !obj.visible {
            visiting.remove(&id);
            return;
        }
        let world_xf = parent_xf.compose(&obj.transform);
        if !world_xf.is_finite() {
            visiting.remove(&id);
            return;
        }

        let is_selected = ancestor_selected || sel_set.is_none_or(|set| set.contains(&id));

        match &obj.payload {
            NodePayload::Group(children) => {
                for &cid in children {
                    collect_laser_polylines_recursive(
                        doc,
                        layer,
                        cid,
                        world_xf,
                        sel_set,
                        is_selected,
                        depth + 1,
                        visiting,
                        rules,
                        rule_buckets,
                        fallback_cut,
                        fallback_engrave,
                        total_extracted,
                    );
                }
                visiting.remove(&id);
                return;
            }
            NodePayload::PowerClip {
                container,
                contents,
                ..
            } => {
                if is_selected {
                    let world_path = container.transformed(&world_xf);
                    classify_and_push_path(
                        layer,
                        obj,
                        &world_path,
                        rules,
                        rule_buckets,
                        fallback_cut,
                        fallback_engrave,
                        total_extracted,
                    );
                }
                for &cid in contents {
                    collect_laser_polylines_recursive(
                        doc,
                        layer,
                        cid,
                        world_xf,
                        sel_set,
                        is_selected,
                        depth + 1,
                        visiting,
                        rules,
                        rule_buckets,
                        fallback_cut,
                        fallback_engrave,
                        total_extracted,
                    );
                }
                visiting.remove(&id);
                return;
            }
            NodePayload::SymbolInstance(sym_id) => {
                if is_selected {
                    if let Some(sym) = doc.symbol(*sym_id) {
                        for sub in &sym.objects {
                            if !sub.visible {
                                continue;
                            }
                            let sub_xf = world_xf.compose(&sub.transform);
                            if !sub_xf.is_finite() {
                                continue;
                            }
                            let sub_path = match &sub.payload {
                                NodePayload::Path(p) => Some(p.transformed(&sub_xf)),
                                NodePayload::Shape(s) => {
                                    Some(s.to_bezier_path().transformed(&sub_xf))
                                }
                                _ => None,
                            };
                            if let Some(path) = sub_path {
                                classify_and_push_path(
                                    layer,
                                    sub,
                                    &path,
                                    rules,
                                    rule_buckets,
                                    fallback_cut,
                                    fallback_engrave,
                                    total_extracted,
                                );
                            }
                        }
                    }
                }
                visiting.remove(&id);
                return;
            }
            _ => {}
        }

        if is_selected {
            let world_path = match &obj.payload {
                NodePayload::Path(p) => Some(p.transformed(&world_xf)),
                NodePayload::Shape(s) => Some(s.to_bezier_path().transformed(&world_xf)),
                NodePayload::Table(t) => Some(t.to_grid_paths().transformed(&world_xf)),
                NodePayload::Connector(c) => {
                    let (a, b) = doc.resolve_connector_endpoints_with_transform(c, &world_xf);
                    Some(c.route_path(a, b))
                }
                _ => None,
            };
            if let Some(path) = world_path {
                classify_and_push_path(
                    layer,
                    obj,
                    &path,
                    rules,
                    rule_buckets,
                    fallback_cut,
                    fallback_engrave,
                    total_extracted,
                );
            }
        }
    }

    visiting.remove(&id);
}

#[allow(clippy::too_many_arguments)]
fn classify_and_push_path(
    layer: &Layer,
    obj: &SceneObject,
    world_path: &BezierPath,
    rules: &[LaserLayerRule],
    rule_buckets: &mut [Vec<ExtractedPolyline>],
    fallback_cut: &mut Vec<ExtractedPolyline>,
    fallback_engrave: &mut Vec<ExtractedPolyline>,
    total_extracted: &mut usize,
) {
    if world_path.is_empty() || *total_extracted >= MAX_PLAN_POLYLINES {
        return;
    }

    let extracted = flatten_bezier_preserving_closure(world_path);
    if extracted.is_empty() {
        return;
    }

    let is_cut_spot = matches!(
        &obj.outline.color,
        Color::Spot { name, .. }
            if name.to_ascii_lowercase().contains("cut")
                || name.to_ascii_lowercase().contains("die")
    );
    let is_cut_tagged = layer.is_cut_layer || is_cut_spot;

    let mut matched_rule_indices: Vec<usize> = Vec::new();
    let mut explicitly_ignored = false;

    // 1. Cut layer / CutContour spot rule check
    if is_cut_tagged {
        if let Some(idx) = rules.iter().position(|r| r.enabled && r.match_cut_layer) {
            if rules[idx].operation == LaserOperationKind::Ignore {
                explicitly_ignored = true;
            } else {
                matched_rule_indices.push(idx);
            }
        } else {
            // Cut layer object with no explicit match_cut_layer rule goes to fallback cut
            *total_extracted += extracted.len();
            fallback_cut.extend(extracted);
            return;
        }
    }

    // 2. Outline RGB rule match (if not already classified via cut-layer tag)
    let has_visible_outline = obj.outline.is_visible() && obj.outline.color.alpha() > 1e-4;
    if !is_cut_tagged && has_visible_outline {
        let rgba = obj.outline.color.to_rgba8();
        if let Some(idx) = best_matching_rule_idx(rules, [rgba[0], rgba[1], rgba[2]]) {
            if rules[idx].operation == LaserOperationKind::Ignore {
                explicitly_ignored = true;
            } else if !matched_rule_indices.contains(&idx) {
                matched_rule_indices.push(idx);
            }
        }
    }

    // 3. Solid/primary fill RGB rule match (e.g., Black fill -> HatchEngrave even when outline is Red Cut!)
    if let Some(fill_color) = primary_fill_color(&obj.fill) {
        if fill_color.alpha() > 1e-4 {
            let rgba = fill_color.to_rgba8();
            if let Some(idx) = best_matching_rule_idx(rules, [rgba[0], rgba[1], rgba[2]]) {
                if rules[idx].operation == LaserOperationKind::Ignore {
                    if matched_rule_indices.is_empty() {
                        explicitly_ignored = true;
                    }
                } else if !matched_rule_indices.contains(&idx) {
                    matched_rule_indices.push(idx);
                }
            }
        }
    }

    if !matched_rule_indices.is_empty() {
        for idx in matched_rule_indices {
            *total_extracted += extracted.len();
            rule_buckets[idx].extend(extracted.clone());
        }
        return;
    }

    if explicitly_ignored {
        return;
    }

    // 4. Fallback for visible non-white unmatched artwork so colorful user designs export cleanly
    if has_visible_outline {
        *total_extracted += extracted.len();
        if obj.outline.is_hairline {
            fallback_cut.extend(extracted);
        } else {
            fallback_engrave.extend(extracted);
        }
    } else if let Some(fill_color) = primary_fill_color(&obj.fill) {
        if fill_color.alpha() > 1e-4 {
            let [r, g, b, _] = fill_color.to_rgba8();
            // Skip pure/near-white background plates when unmatched by any rule
            let is_white_bg = r >= 245 && g >= 245 && b >= 245;
            if !is_white_bg {
                *total_extracted += extracted.len();
                fallback_engrave.extend(extracted);
            }
        }
    }
}

fn primary_fill_color(fill: &Fill) -> Option<Color> {
    match fill {
        Fill::None | Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => None,
        Fill::Solid(c) => Some(c.clone()),
        Fill::TwoColorPattern { fg, .. } | Fill::TextureFill { fg, .. } => Some(fg.clone()),
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => stops.first().map(|s| s.color.clone()),
        Fill::MeshFill { nodes, .. } => nodes.first().map(|n| n.color.clone()),
    }
}

fn best_matching_rule_idx(rules: &[LaserLayerRule], rgb: [u8; 3]) -> Option<usize> {
    let mut best: Option<(usize, u8)> = None;
    for (idx, rule) in rules.iter().enumerate() {
        if !rule.enabled {
            continue;
        }
        let dist = rule.rgb_distance(rgb);
        if dist <= rule.color_tolerance && best.is_none_or(|(_, bd)| dist < bd) {
            best = Some((idx, dist));
        }
    }
    best.map(|(idx, _)| idx)
}

fn flatten_bezier_preserving_closure(path: &BezierPath) -> Vec<ExtractedPolyline> {
    let mut out = Vec::new();
    for sp in &path.subpaths {
        if sp.nodes.len() < 2 {
            continue;
        }
        let single = BezierPath::from_subpath(sp.clone());
        let flattened = single.flatten(FLATTEN_CHORD_TOLERANCE_MM);
        for mut pts in flattened {
            pts = clean_finite_polyline(&pts);
            if pts.len() < 2 {
                continue;
            }
            if sp.closed && pts.len() >= 3 {
                let first = pts[0];
                if pts
                    .last()
                    .is_some_and(|last| last.distance_to(first) > 1e-9)
                {
                    pts.push(first);
                }
            }
            let closed = sp.closed || is_polyline_closed(&pts, 1e-6);
            out.push(ExtractedPolyline {
                points: pts,
                closed,
            });
        }
    }
    out
}

fn process_rule_bucket(
    rule: &LaserLayerRule,
    extracted: &[ExtractedPolyline],
    dedupe_tol: f64,
    join_tol: f64,
    out_segments: &mut Vec<ToolpathSegment>,
    duplicate_segments_removed: &mut usize,
    curves_joined: &mut usize,
) {
    let speed = rule.sanitized_speed();
    let (min_power, max_power) = rule.sanitized_power();
    let passes = rule.sanitized_passes();
    let z_step = rule.sanitized_z_step();
    let freq = rule.sanitized_freq();

    if rule.operation == LaserOperationKind::HatchEngrave {
        let closed_polys: Vec<Vec<Point2D>> = extracted
            .iter()
            .filter(|e| e.closed && e.points.len() >= 3)
            .map(|e| e.points.clone())
            .collect();
        let inset = if rule.kerf_mm.is_finite() && rule.kerf_mm > 0.0 {
            rule.kerf_mm * 0.5
        } else {
            0.0
        };
        let hatch_lines = generate_hatch_fill(
            &closed_polys,
            rule.hatch_spacing_mm,
            rule.hatch_angle_deg,
            rule.cross_hatch,
            inset,
        );
        for (a, b) in hatch_lines {
            out_segments.push(ToolpathSegment {
                operation: LaserOperationKind::HatchEngrave,
                rule_name: rule.name.clone(),
                color_rgb: rule.match_rgb,
                points: vec![a, b],
                closed: false,
                speed_mm_s: speed,
                min_power_pct: min_power,
                max_power_pct: max_power,
                passes,
                air_assist: rule.air_assist,
                z_step_mm: z_step,
                frequency_khz: freq,
                containment_depth: 0,
                is_weeding_box: false,
            });
        }
        return;
    }

    let raw_polys: Vec<Vec<Point2D>> = extracted.iter().map(|e| e.points.clone()).collect();

    // Step 1: Duplicate undirected segment removal
    let (after_dedupe, removed) = if dedupe_tol > 0.0 {
        remove_duplicate_segments(&raw_polys, dedupe_tol)
    } else {
        (raw_polys, 0)
    };
    *duplicate_segments_removed += removed;

    // Step 2: Open-curve endpoint stitching
    let (after_join, joined) = if join_tol > 0.0 {
        join_open_curves(&after_dedupe, join_tol)
    } else {
        (after_dedupe, 0)
    };
    *curves_joined += joined;

    let close_eps = join_tol.max(1e-4);
    let closed_flags: Vec<bool> = after_join
        .iter()
        .map(|p| is_polyline_closed(p, close_eps))
        .collect();

    // Compute local containment depth for kerf sign (holes offset inward, outer parts offset outward)
    let local_rings: Vec<Vec<Point2D>> = after_join
        .iter()
        .zip(closed_flags.iter())
        .map(|(p, &is_closed)| {
            if is_closed {
                ring_without_closing_dup(p)
            } else {
                Vec::new()
            }
        })
        .collect();
    let local_depths = compute_ring_containment_depths(&local_rings);

    let kerf = if rule.kerf_mm.is_finite() {
        rule.kerf_mm
    } else {
        0.0
    };

    for (idx, poly) in after_join.into_iter().enumerate() {
        let is_closed = closed_flags[idx];
        let depth = local_depths[idx];

        let kerfed = if kerf.abs() > 1e-9 {
            let signed_kerf = if is_closed && depth % 2 == 1 {
                -kerf
            } else {
                kerf
            };
            apply_kerf_offset(&poly, is_closed, signed_kerf)
        } else {
            poly
        };

        if kerfed.len() < 2 {
            continue;
        }

        if rule.operation == LaserOperationKind::Perforate {
            let remaining_budget = MAX_PLAN_POLYLINES.saturating_sub(out_segments.len());
            if remaining_budget == 0 {
                break;
            }
            let dashes = dash_polyline(
                &kerfed,
                PERFORATE_DASH_MM,
                PERFORATE_GAP_MM,
                remaining_budget,
            );
            for dash in dashes {
                out_segments.push(ToolpathSegment {
                    operation: LaserOperationKind::Perforate,
                    rule_name: rule.name.clone(),
                    color_rgb: rule.match_rgb,
                    points: dash,
                    closed: false,
                    speed_mm_s: speed,
                    min_power_pct: min_power,
                    max_power_pct: max_power,
                    passes,
                    air_assist: rule.air_assist,
                    z_step_mm: z_step,
                    frequency_khz: freq,
                    containment_depth: depth,
                    is_weeding_box: false,
                });
            }
        } else {
            out_segments.push(ToolpathSegment {
                operation: rule.operation,
                rule_name: rule.name.clone(),
                color_rgb: rule.match_rgb,
                points: kerfed,
                closed: is_closed,
                speed_mm_s: speed,
                min_power_pct: min_power,
                max_power_pct: max_power,
                passes,
                air_assist: rule.air_assist,
                z_step_mm: z_step,
                frequency_khz: freq,
                containment_depth: depth,
                is_weeding_box: false,
            });
        }
    }
}

/// Splits a polyline into alternating burn (`dash_mm`) and skip (`gap_mm`) sub-polylines for perforation.
///
/// Advances parametrically along each finite segment with a strict dash-count budget (`max_dashes`)
/// so huge coordinates, `Inf` lengths, or ULP stalls can never spin or OOM.
fn dash_polyline(
    poly: &[Point2D],
    dash_mm: f64,
    gap_mm: f64,
    max_dashes: usize,
) -> Vec<Vec<Point2D>> {
    if max_dashes == 0 {
        return Vec::new();
    }
    if poly.len() < 2
        || !dash_mm.is_finite()
        || !gap_mm.is_finite()
        || dash_mm <= 0.0
        || gap_mm <= 0.0
    {
        return vec![poly.to_vec()];
    }
    let mut dashes: Vec<Vec<Point2D>> = Vec::new();
    let mut current_dash: Vec<Point2D> = vec![poly[0]];
    let mut drawing = true;
    let mut remaining = dash_mm;

    for win in poly.windows(2) {
        if dashes.len() >= max_dashes {
            break;
        }
        let p0 = win[0];
        let p1 = win[1];
        let total_seg_len = p0.distance_to(p1);
        if !total_seg_len.is_finite() || total_seg_len <= 1e-9 {
            continue;
        }

        let mut pos = 0.0_f64;
        while pos < total_seg_len - 1e-9 && dashes.len() < max_dashes {
            let rem_seg = total_seg_len - pos;
            if rem_seg <= remaining + 1e-9 {
                if drawing {
                    current_dash.push(p1);
                }
                remaining = (remaining - rem_seg).max(0.0);
                if remaining <= 1e-9 {
                    if drawing && current_dash.len() >= 2 {
                        dashes.push(std::mem::take(&mut current_dash));
                    } else {
                        current_dash.clear();
                    }
                    drawing = !drawing;
                    remaining = if drawing { dash_mm } else { gap_mm };
                    if drawing {
                        current_dash.push(p1);
                    }
                }
                break;
            } else {
                let next_pos = pos + remaining;
                // Guard against floating-point ULP stall when coordinates/lengths are huge
                if !next_pos.is_finite() || next_pos <= pos {
                    break;
                }
                let t = (next_pos / total_seg_len).clamp(0.0, 1.0);
                let split = Point2D::new(p0.x + (p1.x - p0.x) * t, p0.y + (p1.y - p0.y) * t);
                if !split.is_finite() {
                    break;
                }
                if drawing {
                    current_dash.push(split);
                    if current_dash.len() >= 2 {
                        dashes.push(std::mem::take(&mut current_dash));
                    } else {
                        current_dash.clear();
                    }
                }
                pos = next_pos;
                drawing = !drawing;
                remaining = if drawing { dash_mm } else { gap_mm };
                if drawing {
                    current_dash.push(split);
                }
            }
        }
    }

    if drawing && current_dash.len() >= 2 && dashes.len() < max_dashes {
        dashes.push(current_dash);
    }
    dashes
}

/// Computes exact point-in-polygon containment depth across all Cut segments (including open
/// cut paths lying inside closed outer perimeters) so inner cutouts always run before outer walls.
fn update_global_cut_containment_depths(segments: &mut [ToolpathSegment]) {
    let closed_cuts: Vec<(usize, Vec<Point2D>, f64, Rect2D)> = segments
        .iter()
        .enumerate()
        .filter_map(|(idx, seg)| {
            if seg.operation != LaserOperationKind::Cut || !seg.closed || seg.is_weeding_box {
                return None;
            }
            let ring = ring_without_closing_dup(&seg.points);
            if ring.len() < 3 {
                return None;
            }
            let area = signed_polygon_area(&ring).abs();
            let bb = ring_bbox(&ring)?;
            (area > 1e-9).then_some((idx, ring, area, bb))
        })
        .collect();

    for (idx, seg) in segments.iter_mut().enumerate() {
        if seg.operation != LaserOperationKind::Cut || seg.is_weeding_box || seg.points.is_empty() {
            continue;
        }
        let own_area = if seg.closed {
            signed_polygon_area(&ring_without_closing_dup(&seg.points)).abs()
        } else {
            0.0
        };
        let test_pt = seg.points[0];
        let mut depth = 0usize;
        for (other_idx, other_ring, other_area, other_bb) in &closed_cuts {
            if *other_idx == idx || *other_area <= own_area + 1e-6 {
                continue;
            }
            if !other_bb.contains_point(test_pt) {
                continue;
            }
            if point_in_polygon(test_pt, other_ring) {
                depth += 1;
            }
        }
        seg.containment_depth = depth;
    }
}

fn order_and_optimize_segments(
    segments: Vec<ToolpathSegment>,
    inner_first: bool,
    optimize_travel: bool,
    start_cursor: Point2D,
) -> Vec<ToolpathSegment> {
    if segments.is_empty() {
        return segments;
    }

    // Group segments into strict priority buckets:
    // (operation_priority, is_weeding_box, inverted_depth_key)
    type SegmentSortKey = (u8, bool, std::cmp::Reverse<usize>);
    let mut keyed: Vec<(SegmentSortKey, ToolpathSegment)> = segments
        .into_iter()
        .map(|seg| {
            let depth_key = if inner_first && seg.operation == LaserOperationKind::Cut {
                seg.containment_depth
            } else {
                0
            };
            let key = (
                seg.operation.execution_priority(),
                seg.is_weeding_box,
                std::cmp::Reverse(depth_key),
            );
            (key, seg)
        })
        .collect();

    // Stable sort preserves deterministic order within identical buckets
    keyed.sort_by_key(|(k, _)| *k);

    if !optimize_travel {
        return keyed.into_iter().map(|(_, s)| s).collect();
    }

    let mut cursor = start_cursor;

    let mut out: Vec<ToolpathSegment> = Vec::with_capacity(keyed.len());
    let mut i = 0usize;

    while i < keyed.len() {
        let bucket_key = keyed[i].0;
        let mut end = i + 1;
        while end < keyed.len() && keyed[end].0 == bucket_key {
            end += 1;
        }

        let mut bucket: Vec<Option<ToolpathSegment>> =
            keyed[i..end].iter().map(|(_, s)| Some(s.clone())).collect();
        let mut remaining = bucket.len();

        while remaining > 0 {
            // Find nearest candidate in bucket from `cursor`, always selecting at least one
            // remaining segment even if distances overflow to Inf/NaN.
            let mut best_idx: Option<usize> = None;
            let mut best_dist = f64::INFINITY;
            let mut best_reverse = false;
            let mut best_rot = 0usize;

            for (bi, opt) in bucket.iter().enumerate() {
                let Some(seg) = opt else {
                    continue;
                };
                if seg.points.len() < 2 {
                    best_idx = Some(bi);
                    best_reverse = false;
                    best_rot = 0;
                    break;
                }

                if best_idx.is_none() {
                    best_idx = Some(bi);
                    best_reverse = false;
                    best_rot = 0;
                }

                if seg.closed && seg.points.len() >= 4 {
                    // Check every unique vertex on the closed loop to minimize rapid travel
                    let ring_len = seg.points.len() - 1;
                    for (vi, &v) in seg.points.iter().take(ring_len).enumerate() {
                        let d = cursor.distance_to(v);
                        if d.is_finite() && d < best_dist {
                            best_dist = d;
                            best_idx = Some(bi);
                            best_reverse = false;
                            best_rot = vi;
                        }
                    }
                } else {
                    let d_start = cursor.distance_to(seg.points[0]);
                    if d_start.is_finite() && d_start < best_dist {
                        best_dist = d_start;
                        best_idx = Some(bi);
                        best_reverse = false;
                        best_rot = 0;
                    }
                    if !seg.closed {
                        let d_end = cursor.distance_to(seg.points[seg.points.len() - 1]);
                        if d_end.is_finite() && d_end < best_dist {
                            best_dist = d_end;
                            best_idx = Some(bi);
                            best_reverse = true;
                            best_rot = 0;
                        }
                    }
                }
            }

            if let Some(idx) = best_idx {
                if let Some(mut chosen) = bucket[idx].take() {
                    if chosen.closed && best_rot > 0 && chosen.points.len() >= 4 {
                        rotate_closed_polyline(&mut chosen.points, best_rot);
                    } else if best_reverse && !chosen.closed {
                        chosen.points.reverse();
                    }
                    if let Some(&last_pt) = chosen.points.last() {
                        cursor = last_pt;
                    }
                    out.push(chosen);
                }
            }
            remaining -= 1;
        }

        i = end;
    }

    out
}

fn rotate_closed_polyline(pts: &mut Vec<Point2D>, start_idx: usize) {
    if pts.len() < 4 {
        return;
    }
    let ring_len = pts.len() - 1;
    let k = start_idx % ring_len;
    if k == 0 {
        return;
    }
    let mut rotated = Vec::with_capacity(pts.len());
    rotated.extend_from_slice(&pts[k..ring_len]);
    rotated.extend_from_slice(&pts[..=k]);
    *pts = rotated;
}

fn polyline_length_mm(pts: &[Point2D]) -> f64 {
    let mut total = 0.0_f64;
    for win in pts.windows(2) {
        let d = win[0].distance_to(win[1]);
        if d.is_finite() {
            total += d;
        }
    }
    total
}

fn compute_segments_bounds(segments: &[ToolpathSegment]) -> Option<Rect2D> {
    let mut acc: Option<Rect2D> = None;
    for seg in segments {
        for &p in &seg.points {
            if !p.is_finite() {
                continue;
            }
            acc = Some(match acc {
                Some(r) => Rect2D::new(
                    r.min_x.min(p.x),
                    r.min_y.min(p.y),
                    r.max_x.max(p.x),
                    r.max_y.max(p.y),
                ),
                None => Rect2D::new(p.x, p.y, p.x, p.y),
            });
        }
    }
    acc.filter(|r| r.is_finite())
}

// =============================================================================
// 6. Production Exporters: G-Code, AutoCAD R12 DXF, HPGL PLT, SVG, EPS
// =============================================================================

fn map_point_for_gcode(pt: Point2D, plan: &LaserToolpathPlan, origin: LaserOrigin) -> (f64, f64) {
    match origin {
        LaserOrigin::TopLeft => (pt.x, pt.y),
        LaserOrigin::BottomLeft => (pt.x, plan.page_height_mm - pt.y),
        LaserOrigin::Center => {
            let center = plan.bounds.map(|b| b.center()).unwrap_or_else(|| {
                Point2D::new(plan.page_width_mm * 0.5, plan.page_height_mm * 0.5)
            });
            (pt.x - center.x, center.y - pt.y)
        }
    }
}

/// Sanitizes arbitrary user/rule text placed inside a G-Code `;` comment so `\r`, `\n`,
/// or other control characters can never break out into injected G-Code commands.
fn sanitize_gcode_comment(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[inline]
fn finite_or(v: f64, fallback: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

/// Exports `plan` as valid CNC/laser G-Code in the configured [`GcodeDialect`].
///
/// Emits `G21` (mm), `G90` (absolute), laser mode initialization (`M4 S0` for GRBL 1.1 /
/// Ruida, `M107` for Marlin), air-assist `M8`/`M9`, multi-pass Z-step descent, and safe
/// laser-off homing footer (`M5`, `M9`, `G0 X0 Y0`).
#[must_use]
pub fn export_laser_gcode(plan: &LaserToolpathPlan, config: &LaserJobConfig) -> String {
    let mut out = String::with_capacity(plan.segments.len() * 96 + 256);
    let _ = writeln!(out, "; coree Laser Toolpath G-Code");
    let _ = writeln!(
        out,
        "; Dialect: {:?} | Origin: {:?} | Segments: {} | Cut: {:.2} mm | Engrave: {:.2} mm | Rapid: {:.2} mm | Est: {:.1}s",
        config.dialect,
        config.origin,
        plan.segments.len(),
        finite_or(plan.total_cut_mm, 0.0).max(0.0),
        finite_or(plan.total_engrave_mm, 0.0).max(0.0),
        finite_or(plan.total_rapid_mm, 0.0).max(0.0),
        finite_or(plan.estimated_seconds, 0.0).max(0.0)
    );
    let _ = writeln!(out, "G21 ; millimeters");
    let _ = writeln!(out, "G90 ; absolute positioning");
    let _ = writeln!(out, "G17 ; XY plane");

    match config.dialect {
        GcodeDialect::Grbl11 => {
            let _ = writeln!(out, "M4 S0 ; GRBL 1.1 dynamic laser mode (laser off)");
        }
        GcodeDialect::Marlin => {
            let _ = writeln!(out, "M107 ; Marlin laser off");
        }
        GcodeDialect::RuidaGcode => {
            let _ = writeln!(out, "; Ruida G-Code dialect");
            let _ = writeln!(out, "M4 S0 ; laser off");
        }
    }

    let mut air_on = false;
    let mut z_moved = false;

    for (seg_idx, seg) in plan.segments.iter().enumerate() {
        if seg.points.len() < 2 || seg.operation == LaserOperationKind::Ignore {
            continue;
        }
        let safe_speed = finite_or(seg.speed_mm_s, 20.0).clamp(0.1, 10_000.0);
        let feed_mm_min = (safe_speed * 60.0).round();
        let max_pct = finite_or(seg.max_power_pct, 0.0).clamp(0.0, 100.0);
        let min_pct = finite_or(seg.min_power_pct, 0.0).clamp(0.0, max_pct);
        let passes = seg.passes.clamp(1, 100);
        let z_step = finite_or(seg.z_step_mm, 0.0).clamp(0.0, 100.0);
        let safe_freq = finite_or(seg.frequency_khz, 20.0).clamp(0.1, 1_000.0);
        let safe_rule_name = sanitize_gcode_comment(&seg.rule_name);

        // Pre-map and filter to finite machine coordinates before emitting
        let mapped_pts: Vec<(f64, f64)> = seg
            .points
            .iter()
            .filter(|p| p.is_finite())
            .map(|&p| map_point_for_gcode(p, plan, config.origin))
            .filter(|&(x, y)| x.is_finite() && y.is_finite())
            .collect();
        if mapped_pts.len() < 2 {
            continue;
        }

        let _ = writeln!(
            out,
            "; Op #{}: {:?} ({safe_rule_name}) | Speed={safe_speed:.1}mm/s Power={min_pct:.0}%..{max_pct:.0}% Passes={passes}",
            seg_idx + 1,
            seg.operation,
        );
        if config.dialect == GcodeDialect::RuidaGcode {
            let _ = writeln!(
                out,
                "; Ruida MinPower={min_pct:.1}% MaxPower={max_pct:.1}% Freq={safe_freq:.1}kHz",
            );
        }

        // Air assist transition
        if seg.air_assist && !air_on {
            let _ = writeln!(out, "M8 ; air assist on");
            air_on = true;
        } else if !seg.air_assist && air_on {
            let _ = writeln!(out, "M9 ; air assist off");
            air_on = false;
        }

        let s_val: u32 = match config.dialect {
            GcodeDialect::Grbl11 | GcodeDialect::RuidaGcode => {
                ((max_pct / 100.0) * 1000.0).round() as u32
            }
            GcodeDialect::Marlin => ((max_pct / 100.0) * 255.0).round() as u32,
        };

        for pass in 0..passes {
            if passes > 1 {
                let _ = writeln!(out, "; Pass {}/{}", pass + 1, passes);
            }
            if z_step > 1e-6 {
                let target_z = -(f64::from(pass) * z_step);
                let _ = writeln!(out, "G0 Z{target_z:.3}");
                z_moved = true;
            }

            let (sx, sy) = mapped_pts[0];
            match config.dialect {
                GcodeDialect::Grbl11 | GcodeDialect::RuidaGcode => {
                    let _ = writeln!(out, "G0 X{sx:.3} Y{sy:.3} S0");
                }
                GcodeDialect::Marlin => {
                    let _ = writeln!(out, "M107");
                    let _ = writeln!(out, "G0 X{sx:.3} Y{sy:.3}");
                    let _ = writeln!(out, "M106 S{s_val}");
                }
            }

            for (pt_idx, &(gx, gy)) in mapped_pts.iter().enumerate().skip(1) {
                match config.dialect {
                    GcodeDialect::Grbl11 | GcodeDialect::RuidaGcode => {
                        if pt_idx == 1 {
                            let _ =
                                writeln!(out, "G1 X{gx:.3} Y{gy:.3} F{feed_mm_min:.0} S{s_val}");
                        } else {
                            let _ = writeln!(out, "G1 X{gx:.3} Y{gy:.3}");
                        }
                    }
                    GcodeDialect::Marlin => {
                        if pt_idx == 1 {
                            let _ = writeln!(out, "G1 X{gx:.3} Y{gy:.3} F{feed_mm_min:.0}");
                        } else {
                            let _ = writeln!(out, "G1 X{gx:.3} Y{gy:.3}");
                        }
                    }
                }
            }

            if config.dialect == GcodeDialect::Marlin {
                let _ = writeln!(out, "M107");
            }
        }

        if z_moved && z_step > 1e-6 {
            let _ = writeln!(out, "G0 Z0.000");
        }
    }

    // Safe shutdown & return to origin
    match config.dialect {
        GcodeDialect::Grbl11 | GcodeDialect::RuidaGcode => {
            let _ = writeln!(out, "M5 ; laser off");
        }
        GcodeDialect::Marlin => {
            let _ = writeln!(out, "M107 ; laser off");
            let _ = writeln!(out, "M5 ; spindle/laser off");
        }
    }
    if air_on {
        let _ = writeln!(out, "M9 ; air assist off");
    }
    let _ = writeln!(out, "G0 X0.000 Y0.000");
    let _ = writeln!(out, "M2 ; program end");
    out
}

/// Exports `plan` as a clean AutoCAD R12 (`AC1009`) ASCII DXF file with color-coded layers
/// (`CUT`, `ENGRAVE`, `HATCH`, `PERFORATE`) using classic `LINE` and `POLYLINE`/`VERTEX`/`SEQEND`
/// entities for 100% compatibility with RDWorks, EZCad, LightBurn, and CAD/CAM tools.
#[must_use]
pub fn export_laser_dxf_r12(plan: &LaserToolpathPlan) -> String {
    let page_w = finite_or(plan.page_width_mm, 210.0).max(1.0);
    let page_h = finite_or(plan.page_height_mm, 297.0).max(1.0);
    let mut out = String::with_capacity(plan.segments.len() * 160 + 1024);

    // HEADER section
    let _ = writeln!(out, "0\nSECTION\n2\nHEADER");
    let _ = writeln!(out, "9\n$ACADVER\n1\nAC1009");
    let _ = writeln!(out, "9\n$INSUNITS\n70\n4"); // 4 = Millimeters
    let (ext_min_x, ext_min_y, ext_max_x, ext_max_y) = match plan.bounds.filter(|b| b.is_finite()) {
        Some(bb) => (bb.min_x, page_h - bb.max_y, bb.max_x, page_h - bb.min_y),
        None => (0.0, 0.0, page_w, page_h),
    };
    let _ = writeln!(
        out,
        "9\n$EXTMIN\n10\n{ext_min_x:.4}\n20\n{ext_min_y:.4}\n30\n0.0"
    );
    let _ = writeln!(
        out,
        "9\n$EXTMAX\n10\n{ext_max_x:.4}\n20\n{ext_max_y:.4}\n30\n0.0"
    );
    let _ = writeln!(out, "0\nENDSEC");

    // TABLES section with dedicated per-operation colored layers
    let layers = [
        LaserOperationKind::Cut,
        LaserOperationKind::VectorEngrave,
        LaserOperationKind::HatchEngrave,
        LaserOperationKind::Perforate,
    ];
    let _ = writeln!(out, "0\nSECTION\n2\nTABLES");
    let _ = writeln!(out, "0\nTABLE\n2\nLAYER\n70\n{}", layers.len());
    for op in layers {
        let _ = writeln!(
            out,
            "0\nLAYER\n2\n{}\n70\n0\n62\n{}\n6\nCONTINUOUS",
            op.layer_name(),
            op.aci_color()
        );
    }
    let _ = writeln!(out, "0\nENDTAB\n0\nENDSEC");

    // ENTITIES section (Cartesian Y-up)
    let _ = writeln!(out, "0\nSECTION\n2\nENTITIES");
    for seg in &plan.segments {
        if seg.operation == LaserOperationKind::Ignore {
            continue;
        }
        let finite_pts: Vec<Point2D> = seg
            .points
            .iter()
            .copied()
            .filter(|p| p.is_finite() && (page_h - p.y).is_finite())
            .collect();
        if finite_pts.len() < 2 {
            continue;
        }
        let layer = seg.operation.layer_name();
        let aci = seg.operation.aci_color();

        if finite_pts.len() == 2 && !seg.closed {
            let p0 = finite_pts[0];
            let p1 = finite_pts[1];
            let y0 = page_h - p0.y;
            let y1 = page_h - p1.y;
            let _ = writeln!(
                out,
                "0\nLINE\n8\n{layer}\n62\n{aci}\n10\n{:.4}\n20\n{y0:.4}\n30\n0.0\n11\n{:.4}\n21\n{y1:.4}\n31\n0.0",
                p0.x, p1.x
            );
        } else {
            let closed_flag = if seg.closed { 1 } else { 0 };
            let _ = writeln!(
                out,
                "0\nPOLYLINE\n8\n{layer}\n62\n{aci}\n66\n1\n10\n0.0\n20\n0.0\n30\n0.0\n70\n{closed_flag}"
            );
            let emit_pts = if seg.closed
                && finite_pts.len() >= 3
                && finite_pts[0].distance_to(finite_pts[finite_pts.len() - 1]) <= 1e-6
            {
                &finite_pts[..finite_pts.len() - 1]
            } else {
                &finite_pts[..]
            };
            for &pt in emit_pts {
                let y = page_h - pt.y;
                let _ = writeln!(
                    out,
                    "0\nVERTEX\n8\n{layer}\n10\n{:.4}\n20\n{y:.4}\n30\n0.0",
                    pt.x
                );
            }
            let _ = writeln!(out, "0\nSEQEND\n8\n{layer}");
        }
    }
    let _ = writeln!(out, "0\nENDSEC\n0\nEOF");
    out
}

/// Exports `plan` as an HPGL (`.plt`) plotter stream at exact 40 plotter units/mm (`1016 units/inch`)
/// for vinyl cutters, pen plotters, RDWorks, and LaserDRW / M2 Nano controllers.
#[must_use]
pub fn export_laser_plt(plan: &LaserToolpathPlan) -> String {
    let page_h = finite_or(plan.page_height_mm, 297.0).max(1.0);
    let mut out = String::with_capacity(plan.segments.len() * 64 + 64);
    out.push_str("IN;SP1;\n");

    let mut active_pen: u8 = 1;
    let to_hpgl = |pt: Point2D| -> (i64, i64) {
        let hx = finite_or(pt.x * HPGL_UNITS_PER_MM, 0.0)
            .clamp(-1e12, 1e12)
            .round() as i64;
        let hy = finite_or((page_h - pt.y) * HPGL_UNITS_PER_MM, 0.0)
            .clamp(-1e12, 1e12)
            .round() as i64;
        (hx, hy)
    };

    for seg in &plan.segments {
        if seg.operation == LaserOperationKind::Ignore {
            continue;
        }
        let finite_pts: Vec<Point2D> = seg
            .points
            .iter()
            .copied()
            .filter(|p| p.is_finite())
            .collect();
        if finite_pts.len() < 2 {
            continue;
        }
        let pen = seg.operation.hpgl_pen().max(1);
        if pen != active_pen {
            let _ = writeln!(out, "SP{pen};");
            active_pen = pen;
        }

        let (x0, y0) = to_hpgl(finite_pts[0]);
        let _ = write!(out, "PU{x0},{y0};PD");
        for (idx, &pt) in finite_pts.iter().enumerate().skip(1) {
            if idx > 1 {
                out.push(',');
            }
            let (hx, hy) = to_hpgl(pt);
            let _ = write!(out, "{hx},{hy}");
        }
        out.push_str(";\n");
    }

    out.push_str("PU0,0;SP0;IN;\n");
    out
}

/// Exports `plan` as a clean, millimeter-scaled, color-coded SVG grouped by operation layer
/// (`CUT`, `ENGRAVE`, `HATCH`, `PERFORATE`) for LightBurn, InkCut, and VisiCut.
#[must_use]
pub fn export_laser_svg(plan: &LaserToolpathPlan) -> String {
    let w = finite_or(plan.page_width_mm, 210.0).max(1.0);
    let h = finite_or(plan.page_height_mm, 297.0).max(1.0);

    let mut out = String::with_capacity(plan.segments.len() * 96 + 512);
    let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w:.3}mm" height="{h:.3}mm" viewBox="0 0 {w:.3} {h:.3}">"#
    );
    let _ = writeln!(
        out,
        "  <!-- Generated by coree laser toolpath engine: {} segment(s), cut={:.2}mm, engrave={:.2}mm -->",
        plan.segments.len(),
        finite_or(plan.total_cut_mm, 0.0).max(0.0),
        finite_or(plan.total_engrave_mm, 0.0).max(0.0)
    );

    let groups = [
        LaserOperationKind::HatchEngrave,
        LaserOperationKind::VectorEngrave,
        LaserOperationKind::Perforate,
        LaserOperationKind::Cut,
    ];

    for op in groups {
        let op_segs: Vec<&ToolpathSegment> = plan
            .segments
            .iter()
            .filter(|s| s.operation == op && s.points.len() >= 2)
            .collect();
        if op_segs.is_empty() {
            continue;
        }
        let [r, g, b] = op.default_rgb();
        let hex = format!("#{r:02X}{g:02X}{b:02X}");
        let _ = writeln!(
            out,
            r#"  <g id="{}" fill="none" stroke="{hex}" stroke-width="0.0762" stroke-linecap="round" stroke-linejoin="round">"#,
            op.layer_name()
        );
        for seg in op_segs {
            let finite_pts: Vec<Point2D> = seg
                .points
                .iter()
                .copied()
                .filter(|p| p.is_finite())
                .collect();
            if finite_pts.len() < 2 {
                continue;
            }
            let mut d = String::new();
            let _ = write!(d, "M {:.4} {:.4}", finite_pts[0].x, finite_pts[0].y);
            let body = if seg.closed
                && finite_pts.len() >= 3
                && finite_pts[0].distance_to(finite_pts[finite_pts.len() - 1]) <= 1e-6
            {
                &finite_pts[1..finite_pts.len() - 1]
            } else {
                &finite_pts[1..]
            };
            for &pt in body {
                let _ = write!(d, " L {:.4} {:.4}", pt.x, pt.y);
            }
            if seg.closed {
                d.push_str(" Z");
            }
            let _ = writeln!(out, r#"    <path d="{d}"/>"#);
        }
        let _ = writeln!(out, "  </g>");
    }

    out.push_str("</svg>\n");
    out
}

/// Exports `plan` as a clean PostScript Level 2 / EPSF-3.0 vector file (mm scaled to PostScript
/// points, bottom-left Y-up origin, RGB operation strokes) for print/cut RIP workflows.
#[must_use]
pub fn export_laser_eps(plan: &LaserToolpathPlan) -> String {
    let page_w = finite_or(plan.page_width_mm, 210.0).max(1.0);
    let page_h = finite_or(plan.page_height_mm, 297.0).max(1.0);
    let w_pt = page_w * MM_TO_PT;
    let h_pt = page_h * MM_TO_PT;
    let w_i = w_pt.ceil() as i64;
    let h_i = h_pt.ceil() as i64;

    let mut out = String::with_capacity(plan.segments.len() * 96 + 512);
    let _ = writeln!(out, "%!PS-Adobe-3.0 EPSF-3.0");
    let _ = writeln!(out, "%%Creator: coree-prod laser engine");
    let _ = writeln!(out, "%%BoundingBox: 0 0 {w_i} {h_i}");
    let _ = writeln!(out, "%%HiResBoundingBox: 0.000 0.000 {w_pt:.3} {h_pt:.3}");
    let _ = writeln!(out, "%%EndComments");
    let _ = writeln!(out, "0.216 setlinewidth 1 setlinecap 1 setlinejoin");

    for seg in &plan.segments {
        if seg.operation == LaserOperationKind::Ignore {
            continue;
        }
        let finite_pts: Vec<Point2D> = seg
            .points
            .iter()
            .copied()
            .filter(|p| p.is_finite() && (page_h - p.y).is_finite())
            .collect();
        if finite_pts.len() < 2 {
            continue;
        }
        let [r, g, b] = seg.operation.default_rgb();
        let _ = writeln!(
            out,
            "{:.4} {:.4} {:.4} setrgbcolor",
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0
        );
        let x0 = finite_pts[0].x * MM_TO_PT;
        let y0 = (page_h - finite_pts[0].y) * MM_TO_PT;
        let _ = writeln!(out, "newpath {x0:.3} {y0:.3} moveto");
        let body = if seg.closed
            && finite_pts.len() >= 3
            && finite_pts[0].distance_to(finite_pts[finite_pts.len() - 1]) <= 1e-6
        {
            &finite_pts[1..finite_pts.len() - 1]
        } else {
            &finite_pts[1..]
        };
        for &pt in body {
            let x = pt.x * MM_TO_PT;
            let y = (page_h - pt.y) * MM_TO_PT;
            let _ = writeln!(out, "{x:.3} {y:.3} lineto");
        }
        if seg.closed {
            let _ = writeln!(out, "closepath");
        }
        let _ = writeln!(out, "stroke");
    }

    let _ = writeln!(out, "showpage");
    let _ = writeln!(out, "%%EOF");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{CornerStyle, MasterLayerScope, OutlineStyle, ParametricShape};

    fn square_poly(x: f64, y: f64, size: f64) -> Vec<Point2D> {
        vec![
            Point2D::new(x, y),
            Point2D::new(x + size, y),
            Point2D::new(x + size, y + size),
            Point2D::new(x, y + size),
            Point2D::new(x, y),
        ]
    }

    #[test]
    fn duplicate_segment_removal_eliminates_shared_edge_in_either_direction() {
        // Two 20x20 squares sharing edge (20,0)-(20,20) in opposite directions
        let sq1 = square_poly(0.0, 0.0, 20.0);
        let sq2 = square_poly(20.0, 0.0, 20.0);
        let (deduped, removed) = remove_duplicate_segments(&[sq1, sq2], 0.05);
        assert_eq!(removed, 1, "shared edge must be removed exactly once");
        // Total remaining cut length = 7 edges * 20mm = 140mm (instead of 160mm)
        let total_len: f64 = deduped.iter().map(|p| polyline_length_mm(p)).sum();
        assert!(
            (total_len - 140.0).abs() < 1e-6,
            "expected 140mm remaining, got {total_len}"
        );
    }

    #[test]
    fn duplicate_segment_removal_handles_grid_boundary_tolerance() {
        // Endpoints straddle 1.000mm grid boundary by 0.01mm (< 0.05mm tolerance)
        let a = vec![Point2D::new(0.995, 0.0), Point2D::new(0.995, 10.0)];
        let b = vec![Point2D::new(1.005, 10.0), Point2D::new(1.005, 0.0)];
        let (deduped, removed) = remove_duplicate_segments(&[a, b], 0.05);
        assert_eq!(removed, 1);
        assert_eq!(deduped.len(), 1);
    }

    #[test]
    fn join_open_curves_stitches_disordered_and_reversed_segments_into_closed_loop() {
        let segs = vec![
            vec![Point2D::new(0.0, 0.0), Point2D::new(10.0, 0.0)],
            vec![Point2D::new(10.0, 10.0), Point2D::new(10.0, 0.01)], // reversed + tiny gap
            vec![Point2D::new(10.0, 10.0), Point2D::new(0.0, 10.0)],
            vec![Point2D::new(0.0, 10.0), Point2D::new(0.0, 0.0)],
        ];
        let (joined, count) = join_open_curves(&segs, 0.05);
        assert_eq!(joined.len(), 1, "4 segments must merge into 1 closed loop");
        assert_eq!(count, 3);
        assert!(is_polyline_closed(&joined[0], 1e-6));
        assert!((polyline_length_mm(&joined[0]) - 40.0).abs() < 0.05);
    }

    #[test]
    fn kerf_offset_expands_outward_and_shrinks_inward_independent_of_winding() {
        let cw = square_poly(10.0, 10.0, 20.0); // [10..30] x [10..30]
        let mut ccw = cw.clone();
        ccw.reverse();

        // +0.4mm kerf -> +0.2mm outward on all 4 sides -> 20.4 x 20.4
        for poly in [&cw, &ccw] {
            let expanded = apply_kerf_offset(poly, true, 0.4);
            let bb = ring_bbox(&expanded).unwrap();
            assert!((bb.min_x - 9.8).abs() < 1e-6, "min_x={}", bb.min_x);
            assert!((bb.max_x - 30.2).abs() < 1e-6, "max_x={}", bb.max_x);
            assert!((bb.width() - 20.4).abs() < 1e-6);
            assert!((bb.height() - 20.4).abs() < 1e-6);

            // -0.4mm kerf -> -0.2mm inward -> 19.6 x 19.6
            let shrunk = apply_kerf_offset(poly, true, -0.4);
            let sbb = ring_bbox(&shrunk).unwrap();
            assert!((sbb.min_x - 10.2).abs() < 1e-6);
            assert!((sbb.max_x - 29.8).abs() < 1e-6);
            assert!((sbb.width() - 19.6).abs() < 1e-6);
        }

        // Sharp acute triangle must trigger miter limit bevel without exploding
        let spike = vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(100.0, 1.0),
            Point2D::new(0.0, 2.0),
            Point2D::new(0.0, 0.0),
        ];
        let offset_spike = apply_kerf_offset(&spike, true, 1.0);
        let bb = ring_bbox(&offset_spike).unwrap();
        assert!(
            bb.max_x < 103.0,
            "miter limit must prevent spike explosion, got max_x={}",
            bb.max_x
        );
    }

    #[test]
    fn hatch_fill_clips_holes_and_supports_rotation_cross_hatch_and_inset() {
        // 20x20 outer square with 10x10 inner hole [5..15] x [5..15]
        let outer = square_poly(0.0, 0.0, 20.0);
        let hole = square_poly(5.0, 5.0, 10.0);

        let lines = generate_hatch_fill(&[outer.clone(), hole.clone()], 1.0, 0.0, false, 0.0);
        // 10 rows without hole (1 segment each) + 10 rows crossing hole (2 segments each) = 30 segments
        assert_eq!(lines.len(), 30);
        // Total hatch length = 10*20 + 10*(5+5) = 300mm
        let total_len: f64 = lines.iter().map(|(a, b)| a.distance_to(*b)).sum();
        assert!(
            (total_len - 300.0).abs() < 1e-4,
            "expected 300mm hatch, got {total_len}"
        );

        // Cross hatch at 45 degrees produces both +45 and +135 passes
        let cross = generate_hatch_fill(std::slice::from_ref(&outer), 2.0, 45.0, true, 0.5);
        assert!(cross.len() >= 18);
        // Verify inset kept all hatch points strictly inside [0.5..19.5]
        for (a, b) in &cross {
            assert!(a.x >= 0.49 && a.x <= 19.51 && a.y >= 0.49 && a.y <= 19.51);
            assert!(b.x >= 0.49 && b.x <= 19.51 && b.y >= 0.49 && b.y <= 19.51);
        }
    }

    #[test]
    fn plan_laser_toolpath_orders_inner_before_outer_and_exports_all_formats() {
        let mut doc = Document::new_a4("Laser Bracket");
        let page_id = doc.pages()[0].id;
        let visible_layer = doc.pages()[0].layers[0].id;

        // Hidden layer must be excluded from laser output!
        let hidden_layer = doc
            .add_layer(page_id, "Hidden Notes", false, MasterLayerScope::None)
            .unwrap();
        doc.set_layer_flags(hidden_layer, Some(false), None, None, None)
            .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Hidden Red Box",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 50.0,
                    height: 50.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_outline(OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255))),
            Some(hidden_layer),
        )
        .unwrap();

        // Outer red cut perimeter: [10..90] x [10..90] (80x80)
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Outer Perimeter",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 80.0,
                    height: 80.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255)))
            .with_transform(Affine2D::translate(10.0, 10.0)),
            Some(visible_layer),
        )
        .unwrap();

        // Inner red cut hole: [30..50] x [30..50] (20x20) — added AFTER outer!
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Inner Hole",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 20.0,
                    height: 20.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::hairline(Color::rgba8(255, 0, 0, 255)))
            .with_transform(Affine2D::translate(30.0, 30.0)),
            Some(visible_layer),
        )
        .unwrap();

        // Blue score line
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Blue Score",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(15.0, 70.0),
                    Point2D::new(85.0, 70.0),
                )),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::solid(Color::rgba8(0, 0, 255, 255), 0.2)),
            Some(visible_layer),
        )
        .unwrap();

        // Black filled plaque for hatch engraving
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Black Hatch Patch",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 15.0,
                    height: 10.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::rgba8(0, 0, 0, 255)))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(60.0, 20.0)),
            Some(visible_layer),
        )
        .unwrap();

        // Green perforation line
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Green Perforation",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(15.0, 80.0),
                    Point2D::new(45.0, 80.0),
                )),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::solid(Color::rgba8(0, 170, 0, 255), 0.2)),
            Some(visible_layer),
        )
        .unwrap();

        let mut cfg = LaserJobConfig {
            add_weeding_box: true,
            weeding_margin_mm: 3.0,
            ..LaserJobConfig::default()
        };
        // Use 1.0mm hatch spacing so test stays crisp and fast
        cfg.rules[2].hatch_spacing_mm = 1.0;
        cfg.rules[0].passes = 2;
        cfg.rules[0].z_step_mm = 1.5;

        let plan = plan_laser_toolpath(&doc, 0, None, &cfg).unwrap();
        assert!(plan.total_cut_mm > 0.0);
        assert!(plan.total_engrave_mm > 0.0);
        assert!(plan.estimated_seconds > 0.0);
        assert!(plan.cut_count >= 3); // inner hole + outer perimeter + weeding box + perforations

        // Verify operation ordering: Hatch -> VectorEngrave -> Perforate -> Cut (inner before outer) -> Weeding Box
        let cut_segs: Vec<&ToolpathSegment> = plan
            .segments
            .iter()
            .filter(|s| s.operation == LaserOperationKind::Cut)
            .collect();
        assert_eq!(cut_segs.len(), 3, "hidden box must be skipped");
        assert_eq!(cut_segs[0].containment_depth, 1, "inner hole cuts first");
        assert_eq!(
            cut_segs[1].containment_depth, 0,
            "outer perimeter cuts second"
        );
        assert!(cut_segs[2].is_weeding_box, "weeding box cuts very last");

        // Verify Hatch comes before VectorEngrave which comes before Cut
        let first_hatch = plan
            .segments
            .iter()
            .position(|s| s.operation == LaserOperationKind::HatchEngrave)
            .unwrap();
        let first_vec = plan
            .segments
            .iter()
            .position(|s| s.operation == LaserOperationKind::VectorEngrave)
            .unwrap();
        let first_perf = plan
            .segments
            .iter()
            .position(|s| s.operation == LaserOperationKind::Perforate)
            .unwrap();
        let first_cut = plan
            .segments
            .iter()
            .position(|s| s.operation == LaserOperationKind::Cut)
            .unwrap();
        assert!(first_hatch < first_vec);
        assert!(first_vec < first_perf);
        assert!(first_perf < first_cut);

        // Export GRBL 1.1 G-Code
        let gcode = export_laser_gcode(&plan, &cfg);
        assert!(gcode.contains("G21"));
        assert!(gcode.contains("G90"));
        assert!(gcode.contains("M4 S0"));
        assert!(gcode.contains("M8"));
        assert!(gcode.contains("G0 Z-1.500"));
        assert!(gcode.contains("M5"));
        assert!(gcode.contains("G0 X0.000 Y0.000"));

        // Export Marlin G-Code
        let mut marlin_cfg = cfg.clone();
        marlin_cfg.dialect = GcodeDialect::Marlin;
        let marlin = export_laser_gcode(&plan, &marlin_cfg);
        assert!(marlin.contains("M106 S"));
        assert!(marlin.contains("M107"));

        // Export AutoCAD R12 DXF
        let dxf = export_laser_dxf_r12(&plan);
        assert!(dxf.contains("AC1009"));
        assert!(dxf.contains("CUT"));
        assert!(dxf.contains("ENGRAVE"));
        assert!(dxf.contains("HATCH"));
        assert!(dxf.contains("PERFORATE"));
        assert!(dxf.contains("POLYLINE"));
        assert!(dxf.contains("SEQEND"));
        assert!(dxf.ends_with("EOF\n"));

        // Export HPGL PLT
        let plt = export_laser_plt(&plan);
        assert!(plt.starts_with("IN;SP1;"));
        assert!(plt.contains("PU"));
        assert!(plt.contains("PD"));
        assert!(plt.ends_with("PU0,0;SP0;IN;\n"));

        // Export SVG
        let svg = export_laser_svg(&plan);
        assert!(svg.contains("width=\"210.000mm\""));
        assert!(svg.contains("id=\"CUT\""));
        assert!(svg.contains("id=\"ENGRAVE\""));
        assert!(svg.contains("id=\"HATCH\""));
        assert!(svg.contains("id=\"PERFORATE\""));

        // Export EPS
        let eps = export_laser_eps(&plan);
        assert!(eps.starts_with("%!PS-Adobe-3.0 EPSF-3.0"));
        assert!(eps.contains("%%BoundingBox:"));
        assert!(eps.contains("%%EOF"));
    }
}
