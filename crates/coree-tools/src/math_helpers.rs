// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure geometry, coordinate conjugation, hit-test, and parametric/gradient/effect helpers
//! shared between [`ToolCore`](crate::ToolCore) and `coree-app`.

use crate::types::{NodeDragPart, ParametricHandleKind, ScaleHandle};
use coree_core::{
    hit_test_point, hit_test_powerclip_contents, Affine2D, AnchorPoint, BezierPath, Color,
    CommandOp, Document, Effect, EllipseMode, Fill, GradientStop, GuideOrientation, LayerId,
    MeshNode, NodeKind, NodePayload, NodeRef, ObjectId, ParametricShape, PathNode, Point2D, Rect2D,
    SceneObject, Subpath, Viewport,
};
use std::collections::{HashMap, HashSet};

/// Returns `true` iff `oid` exists, is unlocked and visible, AND every enclosing container
/// (`Group` / `PowerClip`) and its owning `Layer` are unlocked and visible.
#[must_use]
pub fn is_object_editable_in_doc(doc: &Document, oid: ObjectId) -> bool {
    let mut cur = oid;
    let mut depth = 0usize;
    loop {
        if depth > 64 {
            return false;
        }
        let Some(obj) = doc.object(cur) else {
            return false;
        };
        if obj.locked || !obj.visible {
            return false;
        }
        if let Some(lid) = doc.layer_of_object(cur) {
            return doc.find_layer(lid).is_some_and(|l| !l.locked && l.visible);
        }
        if let Some(parent_id) = doc.parent_container_of(cur) {
            cur = parent_id;
            depth += 1;
        } else {
            return false;
        }
    }
}

/// Filters `selection` down to editable, topmost (non-redundant descendant) object IDs.
#[must_use]
pub fn editable_topmost_selection(doc: &Document, selection: &[ObjectId]) -> Vec<ObjectId> {
    doc.filter_topmost_selection(selection)
        .into_iter()
        .filter(|&id| is_object_editable_in_doc(doc, id))
        .collect()
}

/// Conjugates a world-space transform `world_affine` into the parent-local coordinate
/// space of `id` (`inv_parent_world * world_affine * parent_world`).
#[must_use]
pub fn conjugate_world_affine_for_object(
    doc: &Document,
    id: ObjectId,
    world_affine: Affine2D,
) -> Affine2D {
    if let Some(pid) = doc.parent_container_of(id) {
        if let Some(pw) = doc.world_transform_of(pid) {
            if let Some(inv_pw) = pw.inverse() {
                let conjugated = inv_pw.compose(&world_affine).compose(&pw);
                if conjugated.is_finite() {
                    return conjugated;
                }
            }
        }
    }
    world_affine
}

/// Converts a parent-local transform `local_affine` applied to `id` back into its
/// world-space equivalent (`parent_world * local_affine * inv_parent_world`).
#[must_use]
pub fn unconjugate_local_affine_to_world(
    doc: &Document,
    id: ObjectId,
    local_affine: Affine2D,
) -> Affine2D {
    if let Some(pid) = doc.parent_container_of(id) {
        if let Some(pw) = doc.world_transform_of(pid) {
            if let Some(inv_pw) = pw.inverse() {
                let world = pw.compose(&local_affine).compose(&inv_pw);
                if world.is_finite() {
                    return world;
                }
            }
        }
    }
    local_affine
}

/// Builds a [`CommandOp`] that applies `world_affine` cleanly in world space across all
/// editable topmost objects in `selection`.
#[must_use]
pub fn build_world_transform_command(
    doc: &Document,
    selection: &[ObjectId],
    world_affine: Affine2D,
) -> Option<CommandOp> {
    let ids = editable_topmost_selection(doc, selection);
    if ids.is_empty() {
        return None;
    }
    let per_obj: Vec<(ObjectId, Affine2D)> = ids
        .iter()
        .map(|&id| (id, conjugate_world_affine_for_object(doc, id, world_affine)))
        .collect();
    if per_obj.iter().all(|(_, aff)| *aff == world_affine) {
        return Some(CommandOp::TransformObjects {
            ids,
            affine: world_affine,
        });
    }
    let mut ops: Vec<CommandOp> = per_obj
        .into_iter()
        .map(|(id, local_aff)| CommandOp::TransformObjects {
            ids: vec![id],
            affine: local_aff,
        })
        .collect();
    if ops.len() == 1 {
        ops.pop()
    } else {
        Some(CommandOp::Batch(ops))
    }
}

/// Resolves the [`LayerId`] containing `oid` directly or via any enclosing `Group` / `PowerClip`
/// ancestor (with cycle/depth guard).
#[must_use]
pub fn enclosing_layer_of_object(doc: &Document, oid: ObjectId) -> Option<LayerId> {
    let mut cur = oid;
    let mut seen = HashSet::new();
    while seen.len() <= 64 && seen.insert(cur) {
        if let Some(lid) = doc.layer_of_object(cur) {
            return Some(lid);
        }
        cur = doc.parent_container_of(cur)?;
    }
    None
}

/// Returns the composed world transform of `id` (including all enclosing `Group` / `PowerClip`
/// containers), falling back to `obj.transform` or [`Affine2D::IDENTITY`].
#[must_use]
pub fn world_transform_of_or_local(doc: &Document, id: ObjectId) -> Affine2D {
    doc.world_transform_of(id)
        .unwrap_or_else(|| doc.object(id).map_or(Affine2D::IDENTITY, |o| o.transform))
}

/// Returns the inverse composed world transform of `id`.
#[must_use]
pub fn world_inverse_transform_of(doc: &Document, id: ObjectId) -> Affine2D {
    world_transform_of_or_local(doc, id)
        .inverse()
        .unwrap_or(Affine2D::IDENTITY)
}

/// Ray-casting point-in-polygon check for Freehand Pick lasso selection.
#[must_use]
pub fn point_in_polygon(pt: Point2D, poly: &[Point2D]) -> bool {
    if poly.len() < 3 || !pt.is_finite() {
        return false;
    }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let pi = poly[i];
        let pj = poly[j];
        let intersects = ((pi.y > pt.y) != (pj.y > pt.y))
            && (pt.x < (pj.x - pi.x) * (pt.y - pi.y) / (pj.y - pi.y) + pi.x);
        if intersects {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Converts a sequence of B-Spline control points into a smooth cubic [`BezierPath`].
#[must_use]
pub fn bspline_to_bezier_path(pts: &[Point2D], closed: bool) -> BezierPath {
    coree_core::BSplineCurve::from_control_points(pts.to_vec(), 3, closed).to_bezier_path()
}

/// Builds a 3-Point Curve arc from `p1` to `p2` passing cleanly through `p3` at `t = 0.5`.
#[must_use]
pub fn three_point_arc_path(p1: Point2D, p2: Point2D, p3: Point2D) -> BezierPath {
    let mid = p1.midpoint(p2);
    let q = Point2D::new(2.0 * p3.x - mid.x, 2.0 * p3.y - mid.y);
    let c1 = p1.lerp(q, 2.0 / 3.0);
    let c2 = p2.lerp(q, 2.0 / 3.0);
    let sp = Subpath {
        nodes: vec![
            PathNode::new(p1, None, Some(c1), NodeKind::Smooth),
            PathNode::new(p2, Some(c2), None, NodeKind::Smooth),
        ],
        closed: false,
    };
    BezierPath::from_subpath(sp)
}

/// Computes an interactive `FreeTransform` affine around `anchor` from drag `start -> cur`.
#[must_use]
pub fn compute_free_transform_affine(
    anchor: Point2D,
    start: Point2D,
    cur: Point2D,
    snap_15: bool,
    mirror: bool,
) -> Option<Affine2D> {
    if !anchor.is_finite() || !start.is_finite() || !cur.is_finite() {
        return None;
    }
    let v0 = start - anchor;
    let v1 = cur - anchor;
    let r0 = v0.length();
    let r1 = v1.length();
    if r1 < 0.25 {
        return None;
    }
    if mirror {
        let sx: f64 = if (cur.x - anchor.x) < 0.0 { -1.0 } else { 1.0 };
        let sy: f64 = if (cur.y - anchor.y) < 0.0 { -1.0 } else { 1.0 };
        if (sx - 1.0).abs() < 1e-6 && (sy - 1.0).abs() < 1e-6 {
            return None;
        }
        return Some(
            Affine2D::translate(anchor.x, anchor.y)
                .compose(&Affine2D::scale(sx, sy))
                .compose(&Affine2D::translate(-anchor.x, -anchor.y)),
        );
    }
    let a0 = if r0 > 0.25 { v0.y.atan2(v0.x) } else { 0.0 };
    let a1 = v1.y.atan2(v1.x);
    let mut delta_ang = a1 - a0;
    if snap_15 {
        let step = std::f64::consts::PI / 12.0;
        delta_ang = (delta_ang / step).round() * step;
    }
    let scale = if r0 > 1.0 {
        (r1 / r0).clamp(0.1, 10.0)
    } else {
        1.0
    };
    if delta_ang.abs() < 1e-4 && (scale - 1.0).abs() < 1e-4 {
        return None;
    }
    Some(
        Affine2D::translate(anchor.x, anchor.y)
            .compose(&Affine2D::rotate(delta_ang))
            .compose(&Affine2D::scale(scale, scale))
            .compose(&Affine2D::translate(-anchor.x, -anchor.y)),
    )
}

/// Computes a constrained bounding box from `start` to `cur` (`ctrl_square` => equal sides; `shift_center` => symmetric around `start`).
#[must_use]
pub fn compute_constrained_box(
    start: Point2D,
    cur: Point2D,
    ctrl_square: bool,
    shift_center: bool,
) -> Rect2D {
    let mut dx = cur.x - start.x;
    let mut dy = cur.y - start.y;
    if ctrl_square {
        let side = dx.abs().max(dy.abs());
        dx = if dx < 0.0 { -side } else { side };
        dy = if dy < 0.0 { -side } else { side };
    }
    if shift_center {
        let p0 = Point2D::new(start.x - dx, start.y - dy);
        let p1 = Point2D::new(start.x + dx, start.y + dy);
        Rect2D::from_points(p0, p1)
    } else {
        Rect2D::from_points(start, Point2D::new(start.x + dx, start.y + dy))
    }
}

/// Constrains segment `start -> cur` to 15-degree increments.
#[must_use]
pub fn constrain_line_15deg(start: Point2D, cur: Point2D) -> Point2D {
    let v = cur - start;
    let len = v.length();
    if len < 1e-6 {
        return cur;
    }
    let step = std::f64::consts::PI / 12.0;
    let ang = (v.y.atan2(v.x) / step).round() * step;
    Point2D::new(start.x + len * ang.cos(), start.y + len * ang.sin())
}

/// Returns the 8 world-space positions of selection resize handles for bounding box `bb`.
#[must_use]
pub fn selection_handle_world_positions(bb: Rect2D) -> [(ScaleHandle, Point2D); 8] {
    let tl = Point2D::new(bb.min_x, bb.min_y);
    let tr = Point2D::new(bb.max_x, bb.min_y);
    let br = Point2D::new(bb.max_x, bb.max_y);
    let bl = Point2D::new(bb.min_x, bb.max_y);
    let tc = Point2D::new((tl.x + tr.x) * 0.5, tl.y);
    let bc = Point2D::new((bl.x + br.x) * 0.5, bl.y);
    let lc = Point2D::new(tl.x, (tl.y + bl.y) * 0.5);
    let rc = Point2D::new(tr.x, (tr.y + br.y) * 0.5);
    [
        (ScaleHandle::TopLeft, tl),
        (ScaleHandle::Top, tc),
        (ScaleHandle::TopRight, tr),
        (ScaleHandle::Right, rc),
        (ScaleHandle::BottomRight, br),
        (ScaleHandle::Bottom, bc),
        (ScaleHandle::BottomLeft, bl),
        (ScaleHandle::Left, lc),
    ]
}

/// Hit-tests the 8 selection handles in world mm using viewport logical pixel radius `8.0 px`.
#[must_use]
pub fn hit_scale_handle_world(
    viewport: &Viewport,
    bb: Rect2D,
    world_pt: Point2D,
) -> Option<ScaleHandle> {
    let tol_mm = (8.0 / viewport.logical_px_per_mm().max(1e-4)).max(0.5);
    for (h, pos) in selection_handle_world_positions(bb) {
        if pos.distance_to(world_pt) <= tol_mm {
            return Some(h);
        }
    }
    None
}

/// Computes the world-space scaling affine for a dragged selection handle.
#[must_use]
pub fn compute_scale_affine(
    handle: ScaleHandle,
    orig: Rect2D,
    cur: Point2D,
    ctrl_proportional: bool,
    shift_from_center: bool,
) -> Option<Affine2D> {
    let ow = orig.width().max(1e-3);
    let oh = orig.height().max(1e-3);
    let (anchor, mut sx, mut sy) = if shift_from_center {
        let c = orig.center();
        let hw = (ow * 0.5).max(1e-3);
        let hh = (oh * 0.5).max(1e-3);
        match handle {
            ScaleHandle::Left | ScaleHandle::Right => {
                (c, ((cur.x - c.x).abs() / hw).clamp(0.05, 50.0), 1.0)
            }
            ScaleHandle::Top | ScaleHandle::Bottom => {
                (c, 1.0, ((cur.y - c.y).abs() / hh).clamp(0.05, 50.0))
            }
            _ => (
                c,
                ((cur.x - c.x).abs() / hw).clamp(0.05, 50.0),
                ((cur.y - c.y).abs() / hh).clamp(0.05, 50.0),
            ),
        }
    } else {
        match handle {
            ScaleHandle::BottomRight => (
                Point2D::new(orig.min_x, orig.min_y),
                ((cur.x - orig.min_x) / ow).clamp(0.05, 50.0),
                ((cur.y - orig.min_y) / oh).clamp(0.05, 50.0),
            ),
            ScaleHandle::TopLeft => (
                Point2D::new(orig.max_x, orig.max_y),
                ((orig.max_x - cur.x) / ow).clamp(0.05, 50.0),
                ((orig.max_y - cur.y) / oh).clamp(0.05, 50.0),
            ),
            ScaleHandle::TopRight => (
                Point2D::new(orig.min_x, orig.max_y),
                ((cur.x - orig.min_x) / ow).clamp(0.05, 50.0),
                ((orig.max_y - cur.y) / oh).clamp(0.05, 50.0),
            ),
            ScaleHandle::BottomLeft => (
                Point2D::new(orig.max_x, orig.min_y),
                ((orig.max_x - cur.x) / ow).clamp(0.05, 50.0),
                ((cur.y - orig.min_y) / oh).clamp(0.05, 50.0),
            ),
            ScaleHandle::Right => (
                Point2D::new(orig.min_x, orig.center().y),
                ((cur.x - orig.min_x) / ow).clamp(0.05, 50.0),
                1.0,
            ),
            ScaleHandle::Left => (
                Point2D::new(orig.max_x, orig.center().y),
                ((orig.max_x - cur.x) / ow).clamp(0.05, 50.0),
                1.0,
            ),
            ScaleHandle::Bottom => (
                Point2D::new(orig.center().x, orig.min_y),
                1.0,
                ((cur.y - orig.min_y) / oh).clamp(0.05, 50.0),
            ),
            ScaleHandle::Top => (
                Point2D::new(orig.center().x, orig.max_y),
                1.0,
                ((orig.max_y - cur.y) / oh).clamp(0.05, 50.0),
            ),
        }
    };
    if ctrl_proportional
        && matches!(
            handle,
            ScaleHandle::TopLeft
                | ScaleHandle::TopRight
                | ScaleHandle::BottomLeft
                | ScaleHandle::BottomRight
        )
    {
        let u = sx.max(sy);
        sx = u;
        sy = u;
    }
    if (sx - 1.0).abs() < 1e-4 && (sy - 1.0).abs() < 1e-4 {
        return None;
    }
    Some(
        Affine2D::translate(anchor.x, anchor.y)
            .compose(&Affine2D::scale(sx, sy))
            .compose(&Affine2D::translate(-anchor.x, -anchor.y)),
    )
}

/// Returns world-space interactive diamond handle positions for a [`ParametricShape`].
#[must_use]
pub fn parametric_shape_handles(
    shape: &ParametricShape,
    transform: &Affine2D,
) -> Vec<(ParametricHandleKind, Point2D)> {
    match *shape {
        ParametricShape::Rectangle {
            width,
            height,
            corner_radii,
            ..
        } => {
            let max_r = (width.min(height) * 0.5).max(0.0);
            let r0 = corner_radii[0].clamp(0.0, max_r);
            let r1 = corner_radii[1].clamp(0.0, max_r);
            let r2 = corner_radii[2].clamp(0.0, max_r);
            let r3 = corner_radii[3].clamp(0.0, max_r);
            vec![
                (
                    ParametricHandleKind::RectCorner(0),
                    transform.transform_point(Point2D::new(r0, 0.0)),
                ),
                (
                    ParametricHandleKind::RectCorner(1),
                    transform.transform_point(Point2D::new(width - r1, 0.0)),
                ),
                (
                    ParametricHandleKind::RectCorner(2),
                    transform.transform_point(Point2D::new(width - r2, height)),
                ),
                (
                    ParametricHandleKind::RectCorner(3),
                    transform.transform_point(Point2D::new(r3, height)),
                ),
            ]
        }
        ParametricShape::Ellipse {
            rx,
            ry,
            mode,
            start_deg,
            end_deg,
        } => {
            let a0 = start_deg.to_radians();
            let mut out = vec![(
                ParametricHandleKind::EllipseAngle { is_end: false },
                transform.transform_point(Point2D::new(rx * a0.cos(), ry * a0.sin())),
            )];
            if mode != EllipseMode::Full {
                let a1 = end_deg.to_radians();
                out.push((
                    ParametricHandleKind::EllipseAngle { is_end: true },
                    transform.transform_point(Point2D::new(rx * a1.cos(), ry * a1.sin())),
                ));
            }
            out
        }
        ParametricShape::Polygon { radius_y, .. } => vec![(
            ParametricHandleKind::PolygonVertex,
            transform.transform_point(Point2D::new(0.0, -radius_y)),
        )],
        ParametricShape::Star {
            points,
            outer_radius,
            inner_ratio,
            ..
        } => {
            let n = points.max(3) as f64;
            let valley_ang = -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI / n;
            let ir = outer_radius * inner_ratio.clamp(0.1, 0.92);
            vec![
                (
                    ParametricHandleKind::StarOuter,
                    transform.transform_point(Point2D::new(0.0, -outer_radius)),
                ),
                (
                    ParametricHandleKind::StarInner,
                    transform.transform_point(Point2D::new(
                        ir * valley_ang.cos(),
                        ir * valley_ang.sin(),
                    )),
                ),
            ]
        }
        _ => Vec::new(),
    }
}

/// Hit-tests parametric shape diamond handles on selected objects.
#[must_use]
pub fn hit_parametric_shape_handle_world(
    doc: &Document,
    selection: &[ObjectId],
    viewport: &Viewport,
    world_pt: Point2D,
) -> Option<(ObjectId, ParametricHandleKind, ParametricShape)> {
    let tol_mm = (9.0 / viewport.logical_px_per_mm().max(1e-4)).max(0.6);
    for &oid in selection {
        if !is_object_editable_in_doc(doc, oid) {
            continue;
        }
        let Some(obj) = doc.object(oid) else {
            continue;
        };
        let NodePayload::Shape(ref shape) = obj.payload else {
            continue;
        };
        let world_xf = world_transform_of_or_local(doc, oid);
        for (kind, wpt) in parametric_shape_handles(shape, &world_xf) {
            if wpt.distance_to(world_pt) <= tol_mm {
                return Some((oid, kind, shape.clone()));
            }
        }
    }
    None
}

/// Applies an interactive handle drag update to a [`ParametricShape`].
pub fn update_parametric_shape_live(
    shape: &mut ParametricShape,
    handle: ParametricHandleKind,
    local_pt: Point2D,
    ctrl: bool,
    shift: bool,
) {
    match (shape, handle) {
        (
            ParametricShape::Rectangle {
                width,
                height,
                corner_radii,
                lock_corners,
                ..
            },
            ParametricHandleKind::RectCorner(idx),
        ) => {
            let max_r = (width.min(*height) * 0.5).max(0.0);
            let raw_r = match idx {
                0 | 3 => local_pt.x.clamp(0.0, max_r),
                _ => (*width - local_pt.x).clamp(0.0, max_r),
            };
            if *lock_corners && !shift {
                *corner_radii = [raw_r; 4];
            } else if idx < 4 {
                corner_radii[idx] = raw_r;
            }
        }
        (
            ParametricShape::Ellipse {
                rx,
                ry,
                mode,
                start_deg,
                end_deg,
            },
            ParametricHandleKind::EllipseAngle { is_end },
        ) => {
            let nx = local_pt.x / rx.max(1e-3);
            let ny = local_pt.y / ry.max(1e-3);
            let r_norm = nx.hypot(ny);
            *mode = if r_norm > 1.0 {
                EllipseMode::Arc
            } else {
                EllipseMode::Pie
            };
            let mut deg = ny.atan2(nx).to_degrees();
            if deg < 0.0 {
                deg += 360.0;
            }
            if ctrl {
                deg = (deg / 15.0).round() * 15.0;
            }
            if is_end {
                *end_deg = deg;
            } else {
                *start_deg = deg;
                if (*end_deg - *start_deg).abs() < 1.0 || (*end_deg >= 359.5 && *start_deg > 1.0) {
                    *end_deg = 360.0;
                }
            }
        }
        (
            ParametricShape::Polygon {
                radius_x, radius_y, ..
            },
            ParametricHandleKind::PolygonVertex,
        ) => {
            let dist = local_pt.x.hypot(local_pt.y).clamp(2.0, 2000.0);
            if ctrl {
                *radius_x = dist;
                *radius_y = dist;
            } else {
                *radius_y = dist;
                *radius_x = (*radius_x).max(2.0);
            }
        }
        (ParametricShape::Star { outer_radius, .. }, ParametricHandleKind::StarOuter) => {
            *outer_radius = local_pt.x.hypot(local_pt.y).clamp(2.0, 2000.0);
        }
        (
            ParametricShape::Star {
                outer_radius,
                inner_ratio,
                ..
            },
            ParametricHandleKind::StarInner,
        ) => {
            let d = local_pt.x.hypot(local_pt.y);
            *inner_ratio = (d / outer_radius.max(1e-3)).clamp(0.1, 0.92);
        }
        _ => {}
    }
}

#[must_use]
fn eval_cubic(p0: Point2D, p1: Point2D, p2: Point2D, p3: Point2D, t: f64) -> Point2D {
    let mt = 1.0 - t;
    let mt2 = mt * mt;
    let t2 = t * t;
    Point2D::new(
        mt2 * mt * p0.x + 3.0 * mt2 * t * p1.x + 3.0 * mt * t2 * p2.x + t2 * t * p3.x,
        mt2 * mt * p0.y + 3.0 * mt2 * t * p1.y + 3.0 * mt * t2 * p2.y + t2 * t * p3.y,
    )
}

/// Hit-tests interior curve segments on selected paths.
#[must_use]
pub fn hit_curve_segment_on_selection_world(
    doc: &Document,
    selection: &[ObjectId],
    viewport: &Viewport,
    world_pt: Point2D,
) -> Option<(ObjectId, usize, usize, f64)> {
    let tol_mm = (8.0 / viewport.logical_px_per_mm().max(1e-4)).max(0.5);
    let mut best: Option<(f64, ObjectId, usize, usize, f64)> = None;
    for &oid in selection {
        if !is_object_editable_in_doc(doc, oid) {
            continue;
        }
        let Some(obj) = doc.object(oid) else {
            continue;
        };
        let NodePayload::Path(ref path) = obj.payload else {
            continue;
        };
        let world_xf = world_transform_of_or_local(doc, oid);
        for (sp_idx, sp) in path.subpaths.iter().enumerate() {
            let sc = sp.segment_count();
            for seg_idx in 0..sc {
                let Some((i0, i1)) = sp.segment_node_indices(seg_idx) else {
                    continue;
                };
                let n0 = sp.nodes[i0];
                let n1 = sp.nodes[i1];
                let p0 = n0.pos;
                let p1 = n0.handle_out.unwrap_or(p0);
                let p2 = n1.handle_in.unwrap_or(n1.pos);
                let p3 = n1.pos;
                for k in 1..16 {
                    let t = f64::from(k) / 16.0;
                    let pt = world_xf.transform_point(eval_cubic(p0, p1, p2, p3, t));
                    let d = pt.distance_to(world_pt);
                    if d <= tol_mm && best.is_none_or(|b| d < b.0) {
                        best = Some((d, oid, sp_idx, seg_idx, t));
                    }
                }
            }
        }
    }
    best.map(|(_, oid, sp, seg, t)| (oid, sp, seg, t))
}

/// Hit-tests Bezier anchor nodes and control handles on selected editable paths.
#[must_use]
pub fn hit_editable_bezier_node_or_handle_world(
    doc: &Document,
    selection: &[ObjectId],
    viewport: &Viewport,
    world_pt: Point2D,
) -> Option<(NodeRef, NodeDragPart, BezierPath)> {
    let px = viewport.logical_px_per_mm().max(1e-4);
    let handle_tol_mm = (7.0 / px).max(0.5);
    let anchor_tol_mm = (7.5 / px).max(0.5);
    for &oid in selection {
        if !is_object_editable_in_doc(doc, oid) {
            continue;
        }
        let Some(obj) = doc.object(oid) else {
            continue;
        };
        let NodePayload::Path(ref path) = obj.payload else {
            continue;
        };
        let world_xf = world_transform_of_or_local(doc, oid);
        for (sp_idx, sp) in path.subpaths.iter().enumerate() {
            for (n_idx, node) in sp.nodes.iter().enumerate() {
                if let Some(h_in) = node.handle_in {
                    let w = world_xf.transform_point(h_in);
                    if w.distance_to(world_pt) <= handle_tol_mm {
                        return Some((
                            NodeRef {
                                object_id: oid,
                                subpath_index: sp_idx,
                                node_index: n_idx,
                            },
                            NodeDragPart::HandleIn,
                            path.clone(),
                        ));
                    }
                }
                if let Some(h_out) = node.handle_out {
                    let w = world_xf.transform_point(h_out);
                    if w.distance_to(world_pt) <= handle_tol_mm {
                        return Some((
                            NodeRef {
                                object_id: oid,
                                subpath_index: sp_idx,
                                node_index: n_idx,
                            },
                            NodeDragPart::HandleOut,
                            path.clone(),
                        ));
                    }
                }
                let a = world_xf.transform_point(node.pos);
                if a.distance_to(world_pt) <= anchor_tol_mm {
                    return Some((
                        NodeRef {
                            object_id: oid,
                            subpath_index: sp_idx,
                            node_index: n_idx,
                        },
                        NodeDragPart::Anchor,
                        path.clone(),
                    ));
                }
            }
        }
    }
    None
}

/// Hit-tests document guide lines near `pt_mm`.
#[must_use]
pub fn hit_test_canvas_guide(
    doc: &Document,
    pt_mm: Point2D,
    tol_mm: f64,
) -> Option<(u64, GuideOrientation, bool)> {
    for g in doc.guides().iter().rev() {
        let hit = match g.orientation {
            GuideOrientation::Horizontal(y_mm) => (pt_mm.y - y_mm).abs() <= tol_mm,
            GuideOrientation::Vertical(x_mm) => (pt_mm.x - x_mm).abs() <= tol_mm,
            GuideOrientation::Angled { origin, angle_deg } => {
                let rad = angle_deg.to_radians();
                let nx = -rad.sin();
                let ny = rad.cos();
                let dist = ((pt_mm.x - origin.x) * nx + (pt_mm.y - origin.y) * ny).abs();
                dist <= tol_mm
            }
        };
        if hit {
            return Some((g.id, g.orientation, g.locked));
        }
    }
    None
}

/// Hit-tests ONLY the inner contents of `pc_id` during PowerClip Edit Mode without falling through to page objects.
#[must_use]
pub fn hit_test_powerclip_edit_point(
    doc: &Document,
    pc_id: ObjectId,
    treat_all_objects_as_filled: bool,
    world_pt: Point2D,
    tol_mm: f64,
) -> Option<ObjectId> {
    if let Some(cid) = hit_test_powerclip_contents(doc, pc_id, world_pt, tol_mm) {
        return Some(cid);
    }
    if treat_all_objects_as_filled {
        if let Some(NodePayload::PowerClip { contents, .. }) = doc.object(pc_id).map(|o| &o.payload)
        {
            for &cid in contents.iter().rev() {
                let Some(obj) = doc.object(cid) else {
                    continue;
                };
                if !obj.visible || obj.locked {
                    continue;
                }
                if let Some(wp) = doc.world_path_of(cid) {
                    if wp.subpaths.iter().any(|sp| sp.closed) && wp.contains_point(world_pt) {
                        return Some(cid);
                    }
                } else if let Some(bb) = doc.world_object_bounds(cid) {
                    if bb.contains_point(world_pt) {
                        return Some(cid);
                    }
                }
            }
        }
    }
    None
}

/// Hit-tests canvas points across active PowerClip contents, page objects, and optional filled fallback.
#[must_use]
pub fn hit_test_canvas_point_in_doc(
    doc: &Document,
    powerclip_edit_id: Option<ObjectId>,
    treat_all_objects_as_filled: bool,
    world_pt: Point2D,
    tol_mm: f64,
) -> Option<ObjectId> {
    if let Some(pc_id) = powerclip_edit_id {
        if let Some(cid) =
            hit_test_powerclip_edit_point(doc, pc_id, treat_all_objects_as_filled, world_pt, tol_mm)
        {
            return Some(cid);
        }
    }
    let page_idx = doc.active_page_index();
    if let Some(hit) = hit_test_point(doc, page_idx, world_pt, tol_mm) {
        return Some(hit);
    }
    if treat_all_objects_as_filled {
        for layer in doc.effective_layers_for_page(page_idx).into_iter().rev() {
            if !layer.visible || layer.locked {
                continue;
            }
            for &oid in layer.objects.iter().rev() {
                let Some(obj) = doc.object(oid) else {
                    continue;
                };
                if !obj.visible || obj.locked {
                    continue;
                }
                if let Some(wp) = doc.world_path_of(oid) {
                    if wp.subpaths.iter().any(|sp| sp.closed) && wp.contains_point(world_pt) {
                        return Some(oid);
                    }
                } else if let Some(bb) = doc.world_object_bounds(oid) {
                    if bb.contains_point(world_pt) {
                        return Some(oid);
                    }
                }
            }
        }
    }
    None
}

/// Returns world-space `(anchor, knob)` points for an interactive effect handle on `obj`.
#[must_use]
pub fn interactive_effect_handle_points(
    doc: &Document,
    obj: &SceneObject,
    eff: &Effect,
    tool: crate::types::ToolId,
) -> Option<(Point2D, Point2D)> {
    use crate::types::ToolId;
    let center = doc
        .world_object_bounds(obj.id)
        .map_or(Point2D::ZERO, |b| b.center());
    match (tool, eff) {
        (ToolId::DropShadow, Effect::DropShadow { dx_mm, dy_mm, .. })
        | (ToolId::BlockShadow, Effect::BlockShadow { dx_mm, dy_mm, .. }) => {
            Some((center, Point2D::new(center.x + *dx_mm, center.y + *dy_mm)))
        }
        (ToolId::Contour, Effect::Contour { offset_mm, .. }) => {
            Some((center, Point2D::new(center.x + *offset_mm, center.y)))
        }
        (
            ToolId::Extrude25D,
            Effect::Extrude25D {
                vanishing_point, ..
            },
        ) => Some((center, *vanishing_point)),
        (
            ToolId::Distort,
            Effect::Distort {
                amplitude,
                center: lc,
                ..
            },
        ) => {
            let world_xf = world_transform_of_or_local(doc, obj.id);
            let wc = world_xf.transform_point(*lc);
            let wk = world_xf.transform_point(Point2D::new(lc.x + *amplitude, lc.y));
            Some((wc, wk))
        }
        (ToolId::Blend, Effect::Blend { target_id, .. }) => {
            let target_c = doc
                .world_object_bounds(*target_id)
                .map_or(Point2D::new(center.x + 20.0, center.y), |b| b.center());
            Some((center, target_c))
        }
        _ => None,
    }
}

/// Extracts local gradient ray `(start, end, stops)` across all 4 interactive gradient fill types.
#[must_use]
pub fn gradient_ray_local(fill: &Fill) -> Option<(Point2D, Point2D, &[GradientStop])> {
    match fill {
        Fill::LinearGradient { start, end, stops } => Some((*start, *end, stops.as_slice())),
        Fill::RadialGradient {
            center,
            radius,
            stops,
            ..
        } => Some((
            *center,
            Point2D::new(center.x + radius.max(1.0), center.y),
            stops.as_slice(),
        )),
        Fill::ConicalGradient {
            center,
            angle_deg,
            stops,
        } => {
            let rad = angle_deg.to_radians();
            Some((
                *center,
                Point2D::new(center.x + 25.0 * rad.cos(), center.y + 25.0 * rad.sin()),
                stops.as_slice(),
            ))
        }
        Fill::SquareGradient {
            center,
            size,
            angle_deg,
            stops,
        } => {
            let rad = angle_deg.to_radians();
            let half = size.max(2.0) * 0.5;
            Some((
                *center,
                Point2D::new(center.x + half * rad.cos(), center.y + half * rad.sin()),
                stops.as_slice(),
            ))
        }
        _ => None,
    }
}

/// Mutable gradient ray + stops accessor.
#[must_use]
pub fn gradient_stops_mut(fill: &mut Fill) -> Option<(Point2D, Point2D, &mut Vec<GradientStop>)> {
    match fill {
        Fill::LinearGradient { start, end, stops } => Some((*start, *end, stops)),
        Fill::RadialGradient {
            center,
            radius,
            stops,
            ..
        } => Some((
            *center,
            Point2D::new(center.x + radius.max(1.0), center.y),
            stops,
        )),
        Fill::ConicalGradient {
            center,
            angle_deg,
            stops,
        } => {
            let rad = angle_deg.to_radians();
            Some((
                *center,
                Point2D::new(center.x + 25.0 * rad.cos(), center.y + 25.0 * rad.sin()),
                stops,
            ))
        }
        Fill::SquareGradient {
            center,
            size,
            angle_deg,
            stops,
        } => {
            let rad = angle_deg.to_radians();
            let half = size.max(2.0) * 0.5;
            Some((
                *center,
                Point2D::new(center.x + half * rad.cos(), center.y + half * rad.sin()),
                stops,
            ))
        }
        _ => None,
    }
}

/// Linearly interpolates a gradient color at normalized offset `t`.
#[must_use]
pub fn interpolate_gradient_stop_color(stops: &[GradientStop], t: f32) -> Color {
    if stops.is_empty() {
        return Color::KEY_BLACK;
    }
    let mut sorted: Vec<&GradientStop> = stops.iter().collect();
    sorted.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    if t <= sorted[0].offset {
        return sorted[0].color.clone();
    }
    if t >= sorted[sorted.len() - 1].offset {
        return sorted[sorted.len() - 1].color.clone();
    }
    for win in sorted.windows(2) {
        let (a, b) = (win[0], win[1]);
        if t >= a.offset && t <= b.offset {
            let span = (b.offset - a.offset).max(1e-6);
            let frac = ((t - a.offset) / span).clamp(0.0, 1.0);
            let [r1, g1, b1, a1] = a.color.to_rgba_f32();
            let [r2, g2, b2, a2] = b.color.to_rgba_f32();
            return Color::Rgba {
                r: r1 + (r2 - r1) * frac,
                g: g1 + (g2 - g1) * frac,
                b: b1 + (b2 - b1) * frac,
                a: a1 + (a2 - a1) * frac,
            };
        }
    }
    sorted[0].color.clone()
}

/// Attempts to insert an interpolated gradient stop along the fill ray near `world_pt`.
#[must_use]
pub fn try_insert_gradient_stop_on_ray_world(
    doc: &Document,
    viewport: &Viewport,
    obj: &SceneObject,
    world_pt: Point2D,
) -> Option<Fill> {
    let (start_l, end_l, stops) = gradient_ray_local(&obj.fill)?;
    let world_xf = world_transform_of_or_local(doc, obj.id);
    let a = world_xf.transform_point(start_l);
    let b = world_xf.transform_point(end_l);
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq < 1e-3 {
        return None;
    }
    let t = ((world_pt - a).dot(ab) / len_sq) as f32;
    if !(0.02..=0.98).contains(&t) {
        return None;
    }
    let px = viewport.logical_px_per_mm().max(1e-4);
    let line_tol = (10.0 / px).max(0.8);
    let stop_tol = (7.0 / px).max(0.5);
    let closest = a + ab * f64::from(t);
    if closest.distance_to(world_pt) > line_tol {
        return None;
    }
    for st in stops {
        let st_pt = a + ab * f64::from(st.offset);
        if st_pt.distance_to(world_pt) <= stop_tol {
            return None;
        }
    }
    let color = interpolate_gradient_stop_color(stops, t);
    let mut updated = obj.fill.clone();
    if let Some((_, _, stops_mut)) = gradient_stops_mut(&mut updated) {
        stops_mut.push(GradientStop::new(t, color));
        stops_mut.sort_by(|x, y| x.offset.total_cmp(&y.offset));
        Some(updated)
    } else {
        None
    }
}

/// Finds the nearest connectable object & bounding-box anchor (`Top`/`Bottom`/`Left`/`Right`/`Center`) near `pt`.
#[must_use]
pub fn find_nearest_object_anchor(
    doc: &Document,
    page_index: usize,
    pt: Point2D,
    tol_mm: f64,
) -> Option<(ObjectId, AnchorPoint, Point2D)> {
    find_nearest_object_anchor_in_doc(doc, page_index, None, false, pt, tol_mm)
}

/// PowerClip-edit-aware variant of [`find_nearest_object_anchor`].
#[must_use]
pub fn find_nearest_object_anchor_in_doc(
    doc: &Document,
    page_index: usize,
    powerclip_edit_id: Option<ObjectId>,
    treat_all_objects_as_filled: bool,
    pt: Point2D,
    tol_mm: f64,
) -> Option<(ObjectId, AnchorPoint, Point2D)> {
    let search_tol = tol_mm.max(6.0);
    let oid = if page_index == doc.active_page_index() {
        hit_test_canvas_point_in_doc(
            doc,
            powerclip_edit_id,
            treat_all_objects_as_filled,
            pt,
            search_tol,
        )?
    } else if let Some(pc_id) = powerclip_edit_id {
        hit_test_powerclip_contents(doc, pc_id, pt, search_tol)
            .or_else(|| hit_test_point(doc, page_index, pt, search_tol))?
    } else {
        hit_test_point(doc, page_index, pt, search_tol)?
    };
    let obj = doc.object(oid)?;
    if matches!(obj.payload, NodePayload::Connector(_)) {
        return None;
    }
    let bb = doc.world_object_bounds(oid)?;
    let c = bb.center();
    let candidates = [
        (AnchorPoint::Top, Point2D::new(c.x, bb.min_y)),
        (AnchorPoint::Bottom, Point2D::new(c.x, bb.max_y)),
        (AnchorPoint::Left, Point2D::new(bb.min_x, c.y)),
        (AnchorPoint::Right, Point2D::new(bb.max_x, c.y)),
        (AnchorPoint::Center, c),
    ];
    candidates
        .into_iter()
        .min_by(|a, b| {
            a.1.distance_to(pt)
                .partial_cmp(&b.1.distance_to(pt))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(anc, pos)| (oid, anc, pos))
}

/// Builds a default `MeshFill` grid in object-local coordinates (`rows` x `cols`, clamped `2..=32`).
#[must_use]
pub fn build_mesh_fill_for_bounds(local_bounds: Rect2D, rows: usize, cols: usize) -> Fill {
    let rows = rows.clamp(2, 32);
    let cols = cols.clamp(2, 32);
    let mut nodes = Vec::with_capacity(rows * cols);
    for r in 0..rows {
        let v = r as f64 / (rows - 1) as f64;
        for c in 0..cols {
            let u = c as f64 / (cols - 1) as f64;
            let pos = Point2D::new(
                local_bounds.min_x + local_bounds.width() * u,
                local_bounds.min_y + local_bounds.height() * v,
            );
            let color = Color::cmyk(u as f32, v as f32, ((1.0 - u) * 0.4) as f32, 0.0);
            nodes.push(MeshNode {
                row: r,
                col: c,
                pos,
                color,
            });
        }
    }
    Fill::MeshFill { rows, cols, nodes }
}

/// Builds a closed or open rectangular [`BezierPath`] from `rect`.
#[must_use]
pub fn rect_to_bezier_path(rect: Rect2D) -> BezierPath {
    BezierPath::from_polygon(
        vec![
            Point2D::new(rect.min_x, rect.min_y),
            Point2D::new(rect.max_x, rect.min_y),
            Point2D::new(rect.max_x, rect.max_y),
            Point2D::new(rect.min_x, rect.max_y),
        ],
        true,
    )
}

/// Builds a closed 48-segment elliptical [`BezierPath`] fitting `rect`.
#[must_use]
pub fn ellipse_rect_to_bezier_path(rect: Rect2D) -> BezierPath {
    let c = rect.center();
    let rx = rect.width() * 0.5;
    let ry = rect.height() * 0.5;
    let pts: Vec<Point2D> = (0..48)
        .map(|i| {
            let th = (i as f64 / 48.0) * std::f64::consts::TAU;
            Point2D::new(c.x + rx * th.cos(), c.y + ry * th.sin())
        })
        .collect();
    BezierPath::from_polygon(pts, true)
}

/// Builds a closed world-space bounding-box outline [`BezierPath`] for a [`SceneObject`],
/// or transforms its vector/shape outline into world space for crisp live drag previews.
#[must_use]
pub fn object_preview_world_path(
    doc: &Document,
    id: ObjectId,
    extra_world_xf: &Affine2D,
) -> Option<BezierPath> {
    let obj = doc.object(id)?;
    let world_xf = extra_world_xf.compose(&world_transform_of_or_local(doc, id));
    match &obj.payload {
        NodePayload::Path(p) => Some(p.transformed(&world_xf)),
        NodePayload::Shape(s) => Some(s.to_bezier_path().transformed(&world_xf)),
        NodePayload::PowerClip { container, .. } => Some(container.transformed(&world_xf)),
        _ => doc
            .local_object_bounds(id)
            .map(|bb| rect_to_bezier_path(bb).transformed(&world_xf)),
    }
}

/// Re-exports used only for compatibility when copying subtree helpers.
#[allow(dead_code)]
fn _unused_hashmap_check(_: &HashMap<ObjectId, ObjectId>) {}
