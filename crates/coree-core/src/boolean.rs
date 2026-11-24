// SPDX-License-Identifier: MIT OR Apache-2.0

//! Robust vector boolean path operations backed by `i_overlay`'s integer sweep engine.
//!
//! Millimeter coordinates are dynamically scaled onto an `i64` integer grid
//! (default `10_000` units/mm = `0.1 µm` precision, matching `.cdr` internal units)
//! with automatic coordinate-range clamping so degenerate or extreme inputs never panic.

use crate::error::{CoreError, CoreResult};
use crate::geometry::{fit_cubic_bezier_subpath, BezierPath};
use crate::math::{sanitize_f64, Point2D};
use crate::style::{LineCap, LineJoin, OutlineStyle};
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay::Overlay;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;
use i_overlay::i_shape::int::shape::{IntContour, IntShapes};
use i_overlay::mesh::float::outline::offset::OutlineOffset;
use i_overlay::mesh::float::stroke::offset::StrokeOffset;
use i_overlay::mesh::float::style::{
    LineCap as IoLineCap, LineJoin as IoLineJoin, OutlineStyle as IoOutlineStyle,
    StrokeStyle as IoStrokeStyle,
};
use serde::{Deserialize, Serialize};

/// Default integer grid scale (`10_000` units per millimeter = `0.1 µm`).
const DEFAULT_INT_SCALE: f64 = 10_000.0;

/// Safe maximum absolute integer coordinate for `i_overlay`'s `i64` engine (`< 2^61`).
const MAX_SAFE_INT_COORD: f64 = 1_000_000_000_000_000_000.0; // 1e18 < 2^62

/// Default curve flattening tolerance (mm) before integer polygon boolean solving.
const DEFAULT_FLATTEN_TOLERANCE_MM: f64 = 0.02;

/// Vector shaping & boolean operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BooleanOp {
    /// Union of all shapes into a single silhouette.
    Weld,
    /// Trims `subject` by subtracting `clip` (`subject - clip`).
    Trim,
    /// Overlapping area shared by `subject` and `clip`.
    Intersect,
    /// Trims underlying (back) shapes by overlapping shapes above them in z-order.
    Simplify,
    /// Keeps the front shape minus the back shape (`front - back`).
    FrontMinusBack,
    /// Keeps the back shape minus the front shape (`back - front`).
    BackMinusFront,
    /// Creates a single outer boundary enclosing the union of all shapes (holes removed).
    CreateBoundary,
    /// Merges subpaths into one compound [`BezierPath`] (creating even-odd holes where overlapped).
    Combine,
    /// Splits every subpath of a compound [`BezierPath`] into independent paths.
    BreakApart,
}

/// Applies a [`BooleanOp`] to a pair of paths `(subject, clip)`.
///
/// Convention for ordered operations:
/// - `subject` is the **back / target** object.
/// - `clip` is the **front / source** object.
///
/// Therefore:
/// - `Trim` computes `subject - clip` (clip trims subject).
/// - `FrontMinusBack` computes `clip - subject`.
/// - `BackMinusFront` computes `subject - clip`.
/// - `Simplify` returns `[subject - clip, clip]` (back trimmed first, front intact).
pub fn apply_boolean(
    op: BooleanOp,
    subject: &BezierPath,
    clip: &BezierPath,
) -> CoreResult<Vec<BezierPath>> {
    match op {
        BooleanOp::Combine => {
            let mut subpaths = subject.subpaths.clone();
            subpaths.extend(clip.subpaths.iter().cloned());
            Ok(if subpaths.is_empty() {
                Vec::new()
            } else {
                vec![BezierPath { subpaths }]
            })
        }
        BooleanOp::BreakApart => {
            let mut out = Vec::new();
            for sp in subject.subpaths.iter().chain(&clip.subpaths) {
                if !sp.nodes.is_empty() {
                    out.push(BezierPath::from_subpath(sp.clone()));
                }
            }
            Ok(out)
        }
        BooleanOp::Weld => run_int_overlay(subject, clip, OverlayRule::Union, false),
        BooleanOp::Trim | BooleanOp::BackMinusFront => {
            run_int_overlay(subject, clip, OverlayRule::Difference, false)
        }
        BooleanOp::FrontMinusBack => {
            run_int_overlay(subject, clip, OverlayRule::InverseDifference, false)
        }
        BooleanOp::Intersect => run_int_overlay(subject, clip, OverlayRule::Intersect, false),
        BooleanOp::CreateBoundary => run_int_overlay(subject, clip, OverlayRule::Union, true),
        BooleanOp::Simplify => {
            let mut out = Vec::new();
            let trimmed_back = run_int_overlay(subject, clip, OverlayRule::Difference, false)?;
            out.extend(trimmed_back);
            if !clip.is_empty() {
                out.push(clip.clone());
            }
            Ok(out)
        }
    }
}

/// Applies a [`BooleanOp`] across an ordered slice of paths (ordered bottom-to-top / back-to-front).
pub fn apply_boolean_many(op: BooleanOp, paths: &[BezierPath]) -> CoreResult<Vec<BezierPath>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    match op {
        BooleanOp::BreakApart => {
            let mut out = Vec::new();
            for p in paths {
                for sp in &p.subpaths {
                    if !sp.nodes.is_empty() {
                        out.push(BezierPath::from_subpath(sp.clone()));
                    }
                }
            }
            Ok(out)
        }
        BooleanOp::Combine => {
            let mut subpaths = Vec::new();
            for p in paths {
                subpaths.extend(p.subpaths.iter().cloned());
            }
            Ok(if subpaths.is_empty() {
                Vec::new()
            } else {
                vec![BezierPath { subpaths }]
            })
        }
        BooleanOp::Weld | BooleanOp::CreateBoundary => {
            let outer_only = op == BooleanOp::CreateBoundary;
            let mut acc = paths[0].clone();
            for next in &paths[1..] {
                let merged = run_int_overlay(&acc, next, OverlayRule::Union, outer_only)?;
                acc = merged.into_iter().next().unwrap_or_default();
            }
            if paths.len() == 1 {
                let single =
                    run_int_overlay(&acc, &BezierPath::new(), OverlayRule::Subject, outer_only)?;
                return Ok(single);
            }
            Ok(if acc.is_empty() {
                Vec::new()
            } else {
                vec![acc]
            })
        }
        BooleanOp::Intersect => {
            if paths.len() < 2 {
                return Ok(paths.to_vec());
            }
            let mut acc = paths[0].clone();
            for next in &paths[1..] {
                let inter = run_int_overlay(&acc, next, OverlayRule::Intersect, false)?;
                acc = inter.into_iter().next().unwrap_or_default();
                if acc.is_empty() {
                    return Ok(Vec::new());
                }
            }
            Ok(vec![acc])
        }
        BooleanOp::Trim | BooleanOp::BackMinusFront => {
            if paths.len() < 2 {
                return Ok(paths.to_vec());
            }
            // Subtract every front shape [1..] from back shape [0]
            let mut acc = paths[0].clone();
            for cutter in &paths[1..] {
                let trimmed = run_int_overlay(&acc, cutter, OverlayRule::Difference, false)?;
                acc = trimmed.into_iter().next().unwrap_or_default();
                if acc.is_empty() {
                    return Ok(Vec::new());
                }
            }
            Ok(vec![acc])
        }
        BooleanOp::FrontMinusBack => {
            if paths.len() < 2 {
                return Ok(paths.to_vec());
            }
            // Keep topmost shape minus all underlying shapes
            let mut acc = paths[paths.len() - 1].clone();
            for back in &paths[..paths.len() - 1] {
                let trimmed = run_int_overlay(&acc, back, OverlayRule::Difference, false)?;
                acc = trimmed.into_iter().next().unwrap_or_default();
                if acc.is_empty() {
                    return Ok(Vec::new());
                }
            }
            Ok(vec![acc])
        }
        BooleanOp::Simplify => {
            // Each path i is trimmed by all paths j > i above it in z-order
            let mut out = Vec::with_capacity(paths.len());
            for i in 0..paths.len() {
                let mut current = paths[i].clone();
                for upper in &paths[(i + 1)..] {
                    let trimmed = run_int_overlay(&current, upper, OverlayRule::Difference, false)?;
                    current = trimmed.into_iter().next().unwrap_or_default();
                    if current.is_empty() {
                        break;
                    }
                }
                if !current.is_empty() {
                    out.push(current);
                }
            }
            Ok(out)
        }
    }
}

/// Offsets closed polygon contours by `delta_mm` (positive expands outward, negative shrinks inward).
///
/// Useful for contour effects, bleed generation, and CutContour die-line offsets.
pub fn offset_polygon(path: &BezierPath, delta_mm: f64) -> CoreResult<BezierPath> {
    if path.is_empty() {
        return Ok(BezierPath::new());
    }
    let delta = sanitize_f64(delta_mm, 0.0);
    if delta.abs() < 1e-6 {
        return Ok(path.clone());
    }
    if delta.abs() > 1e6 {
        return Err(CoreError::InvalidParameter(
            "offset_polygon delta exceeds safe range".into(),
        ));
    }
    let contours: Vec<Vec<[f64; 2]>> = extract_float_rings(path)
        .into_iter()
        .filter(|ring| {
            ring.iter()
                .all(|&[x, y]| x.abs() <= 1e12 && y.abs() <= 1e12)
        })
        .collect();
    if contours.is_empty() {
        return Err(CoreError::InvalidGeometry(
            "offset_polygon requires at least one valid closed contour".into(),
        ));
    }
    let style = IoOutlineStyle::new(delta);
    let shapes = contours.outline_as::<i64>(&style);
    let mut subpaths = Vec::new();
    for shape in shapes {
        for contour in shape {
            if contour.len() >= 3 {
                let pts: Vec<Point2D> = contour
                    .into_iter()
                    .map(|[x, y]| Point2D::new(x, y))
                    .collect();
                subpaths.push(fit_cubic_bezier_subpath(&pts, true, 0.025));
            }
        }
    }
    Ok(BezierPath { subpaths })
}

/// Converts a vector path's stroked outline into closed fillable contours (`Ctrl+Shift+Q`),
/// honoring `cap`, `join`, `miter_limit`, and open vs. closed subpaths.
pub fn stroke_to_closed_path(
    path: &BezierPath,
    outline: &OutlineStyle,
    width_override_mm: Option<f64>,
) -> CoreResult<BezierPath> {
    if path.is_empty() {
        return Err(CoreError::InvalidGeometry(
            "cannot convert empty path outline to object".into(),
        ));
    }
    let width = sanitize_f64(
        width_override_mm.unwrap_or_else(|| outline.effective_width_mm()),
        0.0,
    );
    if !width.is_finite() || width <= 1e-6 || width > 1e6 {
        return Err(CoreError::InvalidParameter(
            "outline width must be positive and within safe bounds".into(),
        ));
    }

    let io_cap = match outline.cap {
        LineCap::Butt => IoLineCap::Butt,
        LineCap::Round => IoLineCap::Round(0.15),
        LineCap::Square => IoLineCap::Square,
    };
    let io_join = match outline.join {
        LineJoin::Miter => {
            let lim = sanitize_f64(outline.miter_limit, 4.0).max(1.0);
            let min_angle = (2.0 * (1.0 / lim).asin()).clamp(0.02, std::f64::consts::PI);
            IoLineJoin::Miter(min_angle)
        }
        LineJoin::Round => IoLineJoin::Round(0.15),
        LineJoin::Bevel => IoLineJoin::Bevel,
    };
    let style: IoStrokeStyle<[f64; 2]> = IoStrokeStyle {
        width,
        start_cap: io_cap.clone(),
        end_cap: io_cap,
        join: io_join,
        ..IoStrokeStyle::default()
    };

    let mut piece_paths: Vec<BezierPath> = Vec::new();
    for sp in &path.subpaths {
        let single = BezierPath::from_subpath(sp.clone());
        for poly in single.flatten(DEFAULT_FLATTEN_TOLERANCE_MM) {
            let mut pts: Vec<[f64; 2]> = poly
                .into_iter()
                .filter(|p| p.is_finite() && p.x.abs() <= 1e9 && p.y.abs() <= 1e9)
                .map(|p| [p.x, p.y])
                .collect();
            if sp.closed && pts.len() > 1 && pts.first() == pts.last() {
                pts.pop();
            }
            if pts.len() < 2 {
                continue;
            }
            let shapes = pts.stroke_as::<i64>(style.clone(), sp.closed);
            let mut subpaths = Vec::new();
            for shape in shapes {
                for contour in shape {
                    if contour.len() >= 3 {
                        let pts: Vec<Point2D> = contour
                            .into_iter()
                            .map(|[x, y]| Point2D::new(x, y))
                            .collect();
                        subpaths.push(fit_cubic_bezier_subpath(&pts, true, 0.025));
                    }
                }
            }
            if !subpaths.is_empty() {
                piece_paths.push(BezierPath { subpaths });
            }
        }
    }

    if piece_paths.is_empty() {
        return Err(CoreError::InvalidGeometry(
            "outline stroke produced no closed contours".into(),
        ));
    }
    if piece_paths.len() == 1 {
        return Ok(piece_paths.into_iter().next().unwrap_or_default());
    }
    let unioned = apply_boolean_many(BooleanOp::Weld, &piece_paths)?;
    Ok(BezierPath {
        subpaths: unioned.into_iter().flat_map(|p| p.subpaths).collect(),
    })
}

fn extract_float_rings(path: &BezierPath) -> Vec<Vec<[f64; 2]>> {
    path.flatten(DEFAULT_FLATTEN_TOLERANCE_MM)
        .into_iter()
        .filter_map(|poly| {
            let mut ring: Vec<[f64; 2]> = poly
                .into_iter()
                .filter(|p| p.is_finite())
                .map(|p| [p.x, p.y])
                .collect();
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            if ring.len() >= 3 && ring_abs_area(&ring) > 1e-9 {
                Some(ring)
            } else {
                None
            }
        })
        .collect()
}

fn ring_abs_area(ring: &[[f64; 2]]) -> f64 {
    let mut area2 = 0.0;
    for i in 0..ring.len() {
        let a = ring[i];
        let b = ring[(i + 1) % ring.len()];
        area2 += a[0] * b[1] - b[0] * a[1];
    }
    0.5 * area2.abs()
}

fn compute_safe_scale(subj_rings: &[Vec<[f64; 2]>], clip_rings: &[Vec<[f64; 2]>]) -> f64 {
    let mut max_abs = 1.0_f64;
    for ring in subj_rings.iter().chain(clip_rings) {
        for &[x, y] in ring {
            max_abs = max_abs.max(x.abs()).max(y.abs());
        }
    }
    if !max_abs.is_finite() {
        return 1.0;
    }
    if max_abs * DEFAULT_INT_SCALE <= MAX_SAFE_INT_COORD {
        DEFAULT_INT_SCALE
    } else {
        (MAX_SAFE_INT_COORD / max_abs).max(1e-6)
    }
}

fn to_int_contours(rings: &[Vec<[f64; 2]>], scale: f64) -> Vec<IntContour<i64>> {
    rings
        .iter()
        .filter_map(|ring| {
            let mut pts: Vec<IntPoint<i64>> = Vec::with_capacity(ring.len());
            for &[x, y] in ring {
                let ix = (x * scale).round();
                let iy = (y * scale).round();
                if !ix.is_finite()
                    || !iy.is_finite()
                    || ix.abs() > MAX_SAFE_INT_COORD
                    || iy.abs() > MAX_SAFE_INT_COORD
                {
                    return None;
                }
                let pt = IntPoint::new(ix as i64, iy as i64);
                if pts.last() != Some(&pt) {
                    pts.push(pt);
                }
            }
            if pts.len() > 1 && pts.first() == pts.last() {
                pts.pop();
            }
            (pts.len() >= 3).then_some(pts)
        })
        .collect()
}

fn from_int_shapes(shapes: IntShapes<i64>, inv_scale: f64, outer_only: bool) -> BezierPath {
    let mut subpaths = Vec::new();
    for shape in shapes {
        let contour_iter = if outer_only {
            shape.into_iter().take(1).collect::<Vec<_>>()
        } else {
            shape
        };
        for contour in contour_iter {
            if contour.len() < 3 {
                continue;
            }
            let pts: Vec<Point2D> = contour
                .into_iter()
                .map(|p| Point2D::new((p.x as f64) * inv_scale, (p.y as f64) * inv_scale))
                .collect();
            subpaths.push(fit_cubic_bezier_subpath(&pts, true, 0.025));
        }
    }
    BezierPath { subpaths }
}

fn run_int_overlay(
    subject: &BezierPath,
    clip: &BezierPath,
    rule: OverlayRule,
    outer_only: bool,
) -> CoreResult<Vec<BezierPath>> {
    let subj_rings = extract_float_rings(subject);
    let clip_rings = extract_float_rings(clip);

    if subj_rings.is_empty() && clip_rings.is_empty() {
        return Ok(Vec::new());
    }

    let scale = compute_safe_scale(&subj_rings, &clip_rings);
    let subj_int = to_int_contours(&subj_rings, scale);
    let clip_int = to_int_contours(&clip_rings, scale);

    if subj_int.is_empty() && clip_int.is_empty() {
        return Ok(Vec::new());
    }

    let shapes =
        Overlay::<i64>::from_subj_and_clip(&subj_int, &clip_int).overlay(rule, FillRule::EvenOdd);
    let result = from_int_shapes(shapes, 1.0 / scale, outer_only);
    if result.is_empty() {
        Ok(Vec::new())
    } else {
        Ok(vec![result])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Rect2D;

    fn rect_path(x: f64, y: f64, w: f64, h: f64) -> BezierPath {
        BezierPath::from_rect(Rect2D::from_xywh(x, y, w, h))
    }

    #[test]
    fn all_boolean_ops_produce_accurate_areas() {
        // Two overlapping 10x10 squares: A=[0..10, 0..10], B=[5..15, 0..10]
        // Overlap = 5x10 = 50, Union = 150, A-B = 50, B-A = 50
        let a = rect_path(0.0, 0.0, 10.0, 10.0);
        let b = rect_path(5.0, 0.0, 10.0, 10.0);

        let weld = apply_boolean(BooleanOp::Weld, &a, &b).unwrap();
        assert_eq!(weld.len(), 1);
        assert!((weld[0].area_mm2() - 150.0).abs() < 0.2);

        let trim = apply_boolean(BooleanOp::Trim, &a, &b).unwrap();
        assert_eq!(trim.len(), 1);
        assert!((trim[0].area_mm2() - 50.0).abs() < 0.2);

        let inter = apply_boolean(BooleanOp::Intersect, &a, &b).unwrap();
        assert_eq!(inter.len(), 1);
        assert!((inter[0].area_mm2() - 50.0).abs() < 0.2);

        let fmb = apply_boolean(BooleanOp::FrontMinusBack, &a, &b).unwrap();
        assert_eq!(fmb.len(), 1);
        assert!((fmb[0].area_mm2() - 50.0).abs() < 0.2);
        let fmb_bb = fmb[0].bounding_box().unwrap();
        assert!((fmb_bb.min_x - 10.0).abs() < 0.1);

        let bmf = apply_boolean(BooleanOp::BackMinusFront, &a, &b).unwrap();
        assert_eq!(bmf.len(), 1);
        assert!((bmf[0].area_mm2() - 50.0).abs() < 0.2);
        let bmf_bb = bmf[0].bounding_box().unwrap();
        assert!(bmf_bb.min_x.abs() < 0.1);

        let simp = apply_boolean(BooleanOp::Simplify, &a, &b).unwrap();
        assert_eq!(simp.len(), 2);
        assert!((simp[0].area_mm2() - 50.0).abs() < 0.2);
        assert!((simp[1].area_mm2() - 100.0).abs() < 0.2);

        // CreateBoundary removes holes and keeps outer contour
        let outer = rect_path(0.0, 0.0, 20.0, 20.0);
        let hole = rect_path(5.0, 5.0, 10.0, 10.0);
        let ring = apply_boolean(BooleanOp::Trim, &outer, &hole).unwrap();
        assert_eq!(ring[0].subpaths.len(), 2);
        let boundary = apply_boolean_many(BooleanOp::CreateBoundary, &ring).unwrap();
        assert_eq!(boundary.len(), 1);
        assert_eq!(boundary[0].subpaths.len(), 1);
        assert!((boundary[0].area_mm2() - 400.0).abs() < 0.5);

        // Combine & BreakApart
        let combined = apply_boolean(BooleanOp::Combine, &a, &b).unwrap();
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].subpaths.len(), 2);
        let broken = apply_boolean_many(BooleanOp::BreakApart, &combined).unwrap();
        assert_eq!(broken.len(), 2);
    }

    #[test]
    fn boolean_handles_degenerate_empty_and_nan_without_panicking() {
        let empty = BezierPath::new();
        let line = BezierPath::from_line(Point2D::new(0.0, 0.0), Point2D::new(10.0, 0.0));
        let nan_path = BezierPath::from_polygon(
            [
                Point2D::new(f64::NAN, 0.0),
                Point2D::new(10.0, f64::INFINITY),
                Point2D::new(0.0, 10.0),
            ],
            true,
        );

        assert!(apply_boolean(BooleanOp::Weld, &empty, &empty)
            .unwrap()
            .is_empty());
        assert!(apply_boolean(BooleanOp::Intersect, &line, &nan_path)
            .unwrap()
            .is_empty());
        assert!(apply_boolean_many(BooleanOp::Simplify, &[])
            .unwrap()
            .is_empty());

        // Polygon offset works
        let sq = rect_path(0.0, 0.0, 10.0, 10.0);
        let expanded = offset_polygon(&sq, 2.0).unwrap();
        assert!(expanded.area_mm2() > sq.area_mm2());
    }

    #[test]
    fn boolean_refits_smooth_cubic_curves_while_keeping_rect_corners_sharp() {
        use crate::geometry::{EllipseMode, ParametricShape};

        let c1 = ParametricShape::Ellipse {
            rx: 20.0,
            ry: 20.0,
            mode: EllipseMode::Full,
            start_deg: 0.0,
            end_deg: 360.0,
        }
        .to_bezier_path()
        .transformed(&crate::math::Affine2D::translate(30.0, 30.0));
        let c2 = ParametricShape::Ellipse {
            rx: 20.0,
            ry: 20.0,
            mode: EllipseMode::Full,
            start_deg: 0.0,
            end_deg: 360.0,
        }
        .to_bezier_path()
        .transformed(&crate::math::Affine2D::translate(50.0, 30.0));

        let welded = apply_boolean(BooleanOp::Weld, &c1, &c2).unwrap();
        assert_eq!(welded.len(), 1);
        let nodes = &welded[0].subpaths[0].nodes;
        assert!(
            nodes.len() <= 24,
            "curve re-fit should avoid 100+ polygon node explosion (got {})",
            nodes.len()
        );
        assert!(
            nodes
                .iter()
                .any(|n| n.handle_in.is_some() || n.handle_out.is_some()),
            "welded circles should preserve cubic Bezier handles"
        );
        let exact_circle_area = std::f64::consts::PI * 400.0;
        assert!(
            welded[0].area_mm2() > exact_circle_area
                && welded[0].area_mm2() < exact_circle_area * 2.0
        );

        // Rectangular booleans stay sharp linear corners with zero spurious handles
        let r1 = rect_path(0.0, 0.0, 10.0, 10.0);
        let r2 = rect_path(5.0, 0.0, 10.0, 10.0);
        let r_weld = apply_boolean(BooleanOp::Weld, &r1, &r2).unwrap();
        assert_eq!(r_weld[0].subpaths[0].nodes.len(), 4);
        assert!(r_weld[0].subpaths[0]
            .nodes
            .iter()
            .all(|n| n.handle_in.is_none() && n.handle_out.is_none()));
    }
}
