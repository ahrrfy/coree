// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure `CommandOp` builders for vector creation, cutting, sculpting, fill, and effect tools.

use crate::context::ToolReadContext;
use crate::math_helpers::{
    build_mesh_fill_for_bounds, compute_free_transform_affine, enclosing_layer_of_object,
    find_nearest_object_anchor_in_doc, is_object_editable_in_doc, point_in_polygon,
    three_point_arc_path, world_inverse_transform_of, world_transform_of_or_local,
};
use crate::output::{ToolActionRequest, ToolOutput};
use crate::types::ToolId;
use coree_core::{
    closed_subpaths_contain_even_odd, hit_test_powerclip_contents_rect, hit_test_rect,
    segment_min_dist_to_point, Affine2D, AnchorPoint, ArtisticMediaStyle, ArtisticText,
    BSplineCurve, BezierPath, Color, CommandOp, ConnectorObject, ConnectorStyle, ContourMode,
    DimensionKind, DimensionObject, Document, Effect, EllipseMode, FeatherDirection, Fill,
    GradientStop, LayerId, LiveSketch, NodeKind, NodePayload, ObjectId, OutlineStyle,
    ParagraphText, ParametricShape, PathNode, Perspective3x3, Point2D, Rect2D, SceneObject,
    SmartDrawing, Subpath, TransparencyMode, Vec2D, VectorTable,
};
use std::collections::{HashMap, HashSet};

pub(crate) fn build_edit_path_op(doc: &Document, id: ObjectId, path: BezierPath) -> CommandOp {
    if doc
        .object(id)
        .is_some_and(|o| matches!(o.payload, NodePayload::Path(_)))
    {
        CommandOp::EditPath { id, path }
    } else {
        CommandOp::Batch(vec![
            CommandOp::ConvertToCurves { id },
            CommandOp::EditPath { id, path },
        ])
    }
}

pub(crate) fn build_replace_effect_op(
    object_id: ObjectId,
    effect_index: Option<usize>,
    new_effect: Effect,
) -> CommandOp {
    if let Some(idx) = effect_index {
        CommandOp::Batch(vec![
            CommandOp::RemoveEffect {
                id: object_id,
                effect_index: idx,
            },
            CommandOp::AddEffect {
                id: object_id,
                effect: new_effect,
            },
        ])
    } else {
        CommandOp::AddEffect {
            id: object_id,
            effect: new_effect,
        }
    }
}

/// Computes lasso-selected object IDs for `FreehandPick`, scoped to `powerclip_edit_id` when active.
#[must_use]
pub fn compute_lasso_hits_scoped(
    doc: &Document,
    powerclip_edit_id: Option<ObjectId>,
    polygon: &[Point2D],
) -> Vec<ObjectId> {
    if polygon.len() < 3 {
        return Vec::new();
    }
    let Some(poly_bb) = Rect2D::from_iter_points(polygon.iter().copied()) else {
        return Vec::new();
    };
    let candidates = if let Some(pc_id) = powerclip_edit_id {
        hit_test_powerclip_contents_rect(doc, pc_id, poly_bb, false)
    } else {
        hit_test_rect(doc, doc.active_page_index(), poly_bb, false)
    };
    let mut hits = Vec::new();
    for oid in candidates {
        let Some(bb) = doc.world_object_bounds(oid) else {
            continue;
        };
        let test_pts = [
            bb.center(),
            bb.min(),
            Point2D::new(bb.max_x, bb.min_y),
            bb.max(),
            Point2D::new(bb.min_x, bb.max_y),
        ];
        let inside = test_pts.iter().any(|&p| point_in_polygon(p, polygon))
            || polygon.iter().any(|&p| bb.contains_point(p));
        if inside {
            hits.push(oid);
        }
    }
    hits
}

/// Computes lasso-selected object IDs for `FreehandPick`.
#[must_use]
pub fn compute_lasso_hits(doc: &Document, polygon: &[Point2D]) -> Vec<ObjectId> {
    compute_lasso_hits_scoped(doc, None, polygon)
}

/// Builds a [`CommandOp::CreateObject`] for a B-Spline through `pts`.
#[must_use]
pub fn build_bspline_create_op(
    ctx: &ToolReadContext<'_>,
    pts: &[Point2D],
    closed: bool,
) -> Option<CommandOp> {
    if pts.len() < 2 {
        return None;
    }
    let layer_id = ctx.target_creation_layer_id()?;
    let bspline = BSplineCurve::from_control_points(pts.to_vec(), 3, closed);
    let fill = if closed {
        ctx.default_fill.clone()
    } else {
        Fill::None
    };
    let mut obj = SceneObject::new(ObjectId(0), "B-Spline", NodePayload::BSpline(bspline))
        .with_fill(fill)
        .with_outline(ctx.default_outline.clone());
    if ctx.settings.symmetry_enabled && ctx.settings.symmetry_folds >= 2 {
        let origin = ctx.settings.symmetry_center_mm.unwrap_or_else(|| {
            ctx.document.active_page().map_or(Point2D::ZERO, |p| {
                Point2D::new(p.width_mm * 0.5, p.height_mm * 0.5)
            })
        });
        obj.effects.push(Effect::Symmetry {
            axes: ctx.settings.symmetry_folds,
            mirror: ctx.settings.symmetry_mirror,
            origin,
        });
    }
    Some(CommandOp::create_object(obj, Some(layer_id)))
}

/// Builds a [`CommandOp::CreateObject`] for a 3-Point Curve arc.
#[must_use]
pub fn build_three_point_curve_op(
    ctx: &ToolReadContext<'_>,
    p1: Point2D,
    p2: Point2D,
    p3: Point2D,
) -> Option<CommandOp> {
    if p1.distance_to(p2) < 0.25 {
        return None;
    }
    let layer_id = ctx.target_creation_layer_id()?;
    let path = three_point_arc_path(p1, p2, p3);
    let obj = SceneObject::new(ObjectId(0), "3-Point Curve", NodePayload::Path(path))
        .with_fill(Fill::None)
        .with_outline(ctx.default_outline.clone());
    Some(CommandOp::create_object(obj, Some(layer_id)))
}

/// Builds a [`CommandOp::CreateObject`] for a 3-Point Rectangle or 3-Point Ellipse.
#[must_use]
pub fn build_three_point_shape_op(
    ctx: &ToolReadContext<'_>,
    tool: ToolId,
    p1: Point2D,
    p2: Point2D,
    p3: Point2D,
) -> Option<CommandOp> {
    let base = p2 - p1;
    let width = base.length();
    if width < 0.5 {
        return None;
    }
    let layer_id = ctx.target_creation_layer_id()?;
    let angle = base.y.atan2(base.x);
    let u = base / width;
    let perp = Vec2D::new(-u.y, u.x);
    let signed_h = (p3 - p1).dot(perp);
    let height = signed_h.abs().max(1.0);
    let ts = ctx.settings;

    let (name, payload, xf) = if tool == ToolId::ThreePointEllipse {
        let center = p1 + u * (width * 0.5) + perp * (signed_h * 0.5);
        (
            "3-Point Ellipse",
            NodePayload::Shape(ParametricShape::Ellipse {
                rx: width * 0.5,
                ry: height * 0.5,
                mode: ts.ellipse_mode,
                start_deg: 0.0,
                end_deg: 360.0,
            }),
            Affine2D::translate(center.x, center.y).compose(&Affine2D::rotate(angle)),
        )
    } else {
        let origin = if signed_h >= 0.0 {
            p1
        } else {
            p1 + perp * signed_h
        };
        (
            "3-Point Rectangle",
            NodePayload::Shape(ParametricShape::Rectangle {
                width,
                height,
                corner_radii: [ts.rect_corner_radius_mm; 4],
                corner_style: ts.rect_corner_style,
                lock_corners: true,
            }),
            Affine2D::translate(origin.x, origin.y).compose(&Affine2D::rotate(angle)),
        )
    };
    let obj = SceneObject::new(ObjectId(0), name, payload)
        .with_fill(ctx.default_fill.clone())
        .with_outline(ctx.default_outline.clone())
        .with_transform(xf);
    Some(CommandOp::create_object(obj, Some(layer_id)))
}

/// Builds a connector [`CommandOp::CreateObject`] from `start_mm` to `end_mm`.
#[must_use]
pub fn build_connector_op(
    ctx: &ToolReadContext<'_>,
    start_mm: Point2D,
    end_mm: Point2D,
    right_angle: bool,
) -> Option<CommandOp> {
    let layer_id = ctx.target_creation_layer_id()?;
    let pidx = ctx.document.active_page_index();
    let from_s = find_nearest_object_anchor_in_doc(
        ctx.document,
        pidx,
        ctx.powerclip_edit_id,
        ctx.treat_all_objects_as_filled,
        start_mm,
        8.0,
    );
    let to_s = find_nearest_object_anchor_in_doc(
        ctx.document,
        pidx,
        ctx.powerclip_edit_id,
        ctx.treat_all_objects_as_filled,
        end_mm,
        8.0,
    );
    let (from_obj, from_anchor, from_fallback) = match from_s {
        Some((oid, anc, pos)) => (Some(oid), anc, pos),
        None => (None, AnchorPoint::Center, start_mm),
    };
    let (to_obj, to_anchor, to_fallback) = match to_s {
        Some((oid, anc, pos)) if Some(oid) != from_obj => (Some(oid), anc, pos),
        _ => (None, AnchorPoint::Center, end_mm),
    };
    let conn = ConnectorObject {
        from_obj,
        from_anchor,
        from_fallback,
        to_obj,
        to_anchor,
        to_fallback,
        style: if right_angle {
            ConnectorStyle::RightAngle
        } else {
            ConnectorStyle::Straight
        },
        corner_radius_mm: 2.0,
    };
    let obj = SceneObject::new(
        ObjectId(0),
        if right_angle {
            "Right-Angle Connector"
        } else {
            "Straight Connector"
        },
        NodePayload::Connector(conn),
    )
    .with_fill(Fill::None)
    .with_outline(ctx.default_outline.clone());
    Some(CommandOp::create_object(obj, Some(layer_id)))
}

/// Builds a knife-cut [`CommandOp::ReplaceObjectInParent`] along `a -> b`.
#[must_use]
pub fn build_knife_cut_op(
    doc: &Document,
    selection: &[ObjectId],
    active_layer_id: Option<LayerId>,
    a: Point2D,
    b: Point2D,
) -> Option<(CommandOp, Vec<ObjectId>)> {
    #[allow(clippy::too_many_arguments)]
    fn collect_cuttable_targets(
        doc: &Document,
        id: ObjectId,
        a: Point2D,
        b: Point2D,
        cut_bb: Rect2D,
        depth: usize,
        visiting: &mut HashSet<ObjectId>,
        out: &mut Vec<ObjectId>,
    ) {
        if depth > 64 || !visiting.insert(id) {
            return;
        }
        if !is_object_editable_in_doc(doc, id) {
            return;
        }
        let Some(obj) = doc.object(id) else {
            return;
        };
        match &obj.payload {
            NodePayload::Path(_) | NodePayload::Shape(_) => {
                if doc
                    .world_object_bounds(id)
                    .is_some_and(|wb| wb.intersects(cut_bb))
                {
                    out.push(id);
                }
            }
            NodePayload::Group(children) => {
                for &cid in children.iter().rev() {
                    collect_cuttable_targets(doc, cid, a, b, cut_bb, depth + 1, visiting, out);
                }
            }
            NodePayload::PowerClip { contents, .. } => {
                let Some(pc_wp) = doc.world_path_of(id) else {
                    return;
                };
                if !pc_wp.bounding_box().is_some_and(|bb| bb.intersects(cut_bb)) {
                    return;
                }
                let enters_pc = pc_wp.contains_point(a)
                    || pc_wp.contains_point(b)
                    || coree_core::knife_cut(&pc_wp, a, b, true).len() > 1
                    || coree_core::knife_cut(&pc_wp, a, b, false).len() > 1;
                if enters_pc {
                    for &cid in contents.iter().rev() {
                        collect_cuttable_targets(doc, cid, a, b, cut_bb, depth + 1, visiting, out);
                    }
                }
            }
            _ => {}
        }
    }

    let cut_bb = Rect2D::from_points(a, b).expand(1.0);
    let mut visiting = HashSet::new();
    let mut candidates = Vec::new();
    if selection.is_empty() {
        let hits = hit_test_rect(doc, doc.active_page_index(), cut_bb, false);
        for hit_id in hits {
            collect_cuttable_targets(doc, hit_id, a, b, cut_bb, 0, &mut visiting, &mut candidates);
        }
    } else {
        for id in order_selection_frontmost_first(doc, selection) {
            collect_cuttable_targets(doc, id, a, b, cut_bb, 0, &mut visiting, &mut candidates);
        }
    }

    let mut chosen: Option<(ObjectId, &SceneObject, Vec<BezierPath>)> = None;
    for oid in candidates {
        if !is_object_editable_in_doc(doc, oid) {
            continue;
        }
        let target_layer_id = enclosing_layer_of_object(doc, oid).or(active_layer_id);
        if target_layer_id.is_some_and(|lid| doc.find_layer(lid).is_some_and(|l| l.locked)) {
            continue;
        }
        let Some(wp) = doc.world_path_of(oid) else {
            continue;
        };
        let Some(obj) = doc.object(oid) else {
            continue;
        };
        let pieces = coree_core::knife_cut(&wp, a, b, true);
        if pieces.len() > 1 {
            chosen = Some((oid, obj, pieces));
            break;
        }
    }

    let (oid, obj, pieces) = chosen?;
    let obj_world = world_transform_of_or_local(doc, oid);
    let (inv_piece_xf, preserved_transform) = if let Some(inv_world) = obj_world.inverse() {
        (inv_world, obj.transform)
    } else {
        let inv_parent = doc
            .parent_container_of(oid)
            .and_then(|pid| doc.world_transform_of(pid))
            .and_then(|xf| xf.inverse())
            .unwrap_or(Affine2D::IDENTITY);
        (inv_parent, Affine2D::IDENTITY)
    };

    let mut next_id = doc.next_id().max(1);
    let mut new_ids = Vec::with_capacity(pieces.len());
    let mut replacements = Vec::with_capacity(pieces.len());
    for (i, piece) in pieces.into_iter().enumerate() {
        let nid = ObjectId(next_id);
        next_id = next_id.saturating_add(1);
        new_ids.push(nid);
        let local_piece = piece.transformed(&inv_piece_xf);
        let mut new_obj = SceneObject::new(
            nid,
            format!("{} Slice {}", obj.name, i + 1),
            NodePayload::Path(local_piece),
        );
        new_obj.transform = preserved_transform;
        new_obj.fill = obj.fill.clone();
        new_obj.outline = obj.outline.clone();
        new_obj.effects = obj.effects.clone();
        new_obj.lens = obj.lens.clone();
        new_obj.style_id = obj.style_id;
        new_obj.opacity = obj.opacity;
        new_obj.blend_mode = obj.blend_mode;
        new_obj.visible = obj.visible;
        new_obj.printable = obj.printable;
        new_obj.unknown_chunks = obj.unknown_chunks.clone();
        replacements.push(new_obj);
    }
    Some((
        CommandOp::ReplaceObjectInParent {
            id: oid,
            replacements,
        },
        new_ids,
    ))
}

fn is_editable_vector_object(doc: &Document, oid: ObjectId) -> bool {
    is_object_editable_in_doc(doc, oid)
        && doc
            .object(oid)
            .is_some_and(|o| matches!(o.payload, NodePayload::Path(_) | NodePayload::Shape(_)))
}

fn path_hits_point_within_tol(path: &BezierPath, pt: Point2D, tol: f64) -> bool {
    if path.is_empty() || !pt.is_finite() || !tol.is_finite() || tol <= 0.0 {
        return false;
    }
    if let Some(bb) = path.bounding_box() {
        if !bb.expand(tol + 0.05).contains_point(pt) {
            return false;
        }
    }
    let tol_sq = tol * tol;
    for poly in path.flatten(0.05) {
        if poly.len() == 1 {
            let d = pt - poly[0];
            if d.x * d.x + d.y * d.y <= tol_sq {
                return true;
            }
            continue;
        }
        for win in poly.windows(2) {
            if point_to_segment_distance_sq(pt, win[0], win[1]) <= tol_sq {
                return true;
            }
        }
    }
    false
}

fn find_vector_hit_in_subtree(
    doc: &Document,
    id: ObjectId,
    skip_id: Option<ObjectId>,
    pt: Point2D,
    tol: f64,
    modified_world_paths: &HashMap<ObjectId, BezierPath>,
    depth: usize,
) -> Option<ObjectId> {
    if depth > 64 || skip_id == Some(id) {
        return None;
    }
    let obj = doc.object(id)?;
    if !obj.visible || obj.locked {
        return None;
    }
    match &obj.payload {
        NodePayload::Path(_) | NodePayload::Shape(_) => {
            if !is_object_editable_in_doc(doc, id) {
                return None;
            }
            let wp = modified_world_paths
                .get(&id)
                .cloned()
                .or_else(|| doc.world_path_of(id))?;
            path_hits_point_within_tol(&wp, pt, tol).then_some(id)
        }
        NodePayload::Group(children) => {
            for &cid in children.iter().rev() {
                if let Some(found) = find_vector_hit_in_subtree(
                    doc,
                    cid,
                    skip_id,
                    pt,
                    tol,
                    modified_world_paths,
                    depth + 1,
                ) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn collect_visible_vsd_cutter_ids(ctx: &ToolReadContext<'_>) -> Vec<ObjectId> {
    fn walk(
        ctx: &ToolReadContext<'_>,
        oid: ObjectId,
        depth: usize,
        visited: &mut HashSet<ObjectId>,
        out: &mut Vec<ObjectId>,
    ) {
        if depth > 64 || !visited.insert(oid) {
            return;
        }
        let Some(obj) = ctx.document.object(oid) else {
            return;
        };
        if !obj.visible {
            return;
        }
        match &obj.payload {
            NodePayload::Path(_) | NodePayload::Shape(_) => {
                out.push(oid);
            }
            NodePayload::Group(children) => {
                for &cid in children {
                    walk(ctx, cid, depth + 1, visited, out);
                }
            }
            NodePayload::PowerClip { contents, .. } => {
                out.push(oid);
                if ctx.powerclip_edit_id == Some(oid) {
                    for &cid in contents {
                        walk(ctx, cid, depth + 1, visited, out);
                    }
                }
            }
            _ => {}
        }
    }

    let active_page_idx = ctx.document.active_page_index();
    let mut visited = HashSet::new();
    let mut out = Vec::new();
    for (p_idx, page) in ctx.document.pages().iter().enumerate() {
        for layer in &page.layers {
            if Document::layer_applies_to_page(layer, p_idx, active_page_idx) && layer.visible {
                for &oid in &layer.objects {
                    walk(ctx, oid, 0, &mut visited, &mut out);
                }
            }
        }
    }
    out
}

fn resolve_virtual_segment_target_at(
    ctx: &ToolReadContext<'_>,
    pt: Point2D,
    tol: f64,
    modified_world_paths: &HashMap<ObjectId, BezierPath>,
) -> Option<ObjectId> {
    let hit = hit_test_canvas_point_opt_tol(ctx, pt, tol)?;
    let is_group = ctx
        .document
        .object(hit)
        .is_some_and(|o| matches!(o.payload, NodePayload::Group(_)));
    if is_group {
        if let Some(leaf_id) =
            find_vector_hit_in_subtree(ctx.document, hit, None, pt, tol, modified_world_paths, 0)
        {
            return Some(leaf_id);
        }
    } else if let Some(mod_path) = modified_world_paths.get(&hit) {
        if path_hits_point_within_tol(mod_path, pt, tol) {
            return is_editable_vector_object(ctx.document, hit).then_some(hit);
        }
    } else if is_editable_vector_object(ctx.document, hit)
        && ctx
            .document
            .world_path_of(hit)
            .is_some_and(|wp| path_hits_point_within_tol(&wp, pt, tol))
    {
        return Some(hit);
    }

    if let Some(pc_id) = ctx.powerclip_edit_id {
        if let Some(NodePayload::PowerClip { contents, .. }) =
            ctx.document.object(pc_id).map(|o| &o.payload)
        {
            for &cid in contents.iter().rev() {
                if let Some(found) = find_vector_hit_in_subtree(
                    ctx.document,
                    cid,
                    Some(hit),
                    pt,
                    tol,
                    modified_world_paths,
                    0,
                ) {
                    return Some(found);
                }
            }
        }
    }
    let search_rect = Rect2D::from_points(pt, pt).expand(tol);
    for cand in hit_test_rect(
        ctx.document,
        ctx.document.active_page_index(),
        search_rect,
        false,
    ) {
        if let Some(found) = find_vector_hit_in_subtree(
            ctx.document,
            cand,
            Some(hit),
            pt,
            tol,
            modified_world_paths,
            0,
        ) {
            return Some(found);
        }
    }
    None
}

/// Builds cumulative Virtual Segment Delete [`CommandOp`]s along a stroke `points`.
///
/// Resamples each stroke segment `points.windows(2)` with step `<= (tol * 0.5).max(0.5)`
/// (bounded for extreme stroke lengths and always visiting every original vertex including
/// `points.last()`), and threads cumulative modified world paths as both target and cutter
/// geometry so multiple deletions on the same object accumulate into a single final edit
/// (emitting `ConvertToCurves` at most once for parametric shapes).
#[must_use]
pub fn build_virtual_segment_delete_stroke_ops(
    ctx: &ToolReadContext<'_>,
    points: &[Point2D],
) -> Vec<CommandOp> {
    if points.is_empty() {
        return Vec::new();
    }
    let tol = (8.0 / ctx.viewport.px_per_mm().max(1e-4)).max(2.0);
    let step = (tol * 0.5).max(0.5);

    let mut samples = Vec::with_capacity(points.len());
    if points[0].is_finite() {
        samples.push(points[0]);
    }
    for seg in points.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        if !a.is_finite() || !b.is_finite() {
            if b.is_finite() && samples.last().is_none_or(|&last| last != b) {
                samples.push(b);
            }
            continue;
        }
        if samples.last().is_none_or(|&last| last != a) {
            samples.push(a);
        }
        let dist = a.distance_to(b);
        if dist <= 1e-9 {
            if b != a {
                samples.push(b);
            }
            continue;
        }
        let steps = ((dist / step).ceil() as usize).clamp(1, 4096);
        for k in 1..=steps {
            let pt = if k == steps {
                b
            } else {
                a.lerp(b, k as f64 / steps as f64)
            };
            samples.push(pt);
        }
    }
    if samples.is_empty() {
        return Vec::new();
    }

    let cutter_ids = collect_visible_vsd_cutter_ids(ctx);
    let mut modified_world_paths: HashMap<ObjectId, BezierPath> = HashMap::new();
    let mut modified_order: Vec<ObjectId> = Vec::new();

    for pt in samples {
        let Some(oid) = resolve_virtual_segment_target_at(ctx, pt, tol, &modified_world_paths)
        else {
            continue;
        };
        let Some(target_path) = modified_world_paths
            .get(&oid)
            .cloned()
            .or_else(|| ctx.document.world_path_of(oid))
        else {
            continue;
        };
        if target_path.is_empty() {
            continue;
        }
        let cutters: Vec<BezierPath> = cutter_ids
            .iter()
            .copied()
            .filter(|&id| id != oid)
            .filter_map(|id| {
                modified_world_paths
                    .get(&id)
                    .cloned()
                    .or_else(|| ctx.document.world_path_of(id))
            })
            .collect();
        let updated_world = coree_core::virtual_segment_delete(&target_path, &cutters, pt, tol);
        if updated_world == target_path {
            continue;
        }
        if !modified_world_paths.contains_key(&oid) {
            modified_order.push(oid);
        }
        modified_world_paths.insert(oid, updated_world);
    }

    let mut ops = Vec::with_capacity(modified_order.len());
    for oid in modified_order {
        let Some(updated_world) = modified_world_paths.get(&oid) else {
            continue;
        };
        let Some(orig_world) = ctx.document.world_path_of(oid) else {
            continue;
        };
        if *updated_world == orig_world {
            continue;
        }
        let inv = world_inverse_transform_of(ctx.document, oid);
        let updated_local = updated_world.transformed(&inv);
        let orig_local = match ctx.document.object(oid).map(|o| &o.payload) {
            Some(NodePayload::Path(p)) => p.clone(),
            Some(NodePayload::Shape(s)) => s.to_bezier_path(),
            _ => continue,
        };
        if updated_local == orig_local {
            continue;
        }
        ops.push(build_edit_path_op(ctx.document, oid, updated_local));
    }
    ops
}

/// Builds a Virtual Segment Delete [`CommandOp`] at `pt`.
#[must_use]
pub fn build_virtual_segment_delete_op(
    ctx: &ToolReadContext<'_>,
    pt: Point2D,
) -> Option<CommandOp> {
    build_virtual_segment_delete_stroke_ops(ctx, &[pt])
        .into_iter()
        .next()
}

fn point_to_segment_distance_sq(p: Point2D, a: Point2D, b: Point2D) -> f64 {
    let ab = b - a;
    let len_sq = ab.x * ab.x + ab.y * ab.y;
    if len_sq <= 1e-18 {
        let d = p - a;
        return d.x * d.x + d.y * d.y;
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    let proj = a + ab * t;
    let d = p - proj;
    d.x * d.x + d.y * d.y
}

fn path_intersects_disk(
    path: &BezierPath,
    center: Point2D,
    radius: f64,
    include_interior: bool,
) -> bool {
    if path.is_empty() || !center.is_finite() || !radius.is_finite() || radius <= 0.0 {
        return false;
    }
    if let Some(bb) = path.bounding_box() {
        if !bb.expand(radius + 0.05).contains_point(center) {
            return false;
        }
    }
    for sp in &path.subpaths {
        let seg_count = sp.segment_count();
        if seg_count == 0 {
            if let Some(first) = sp.nodes.first() {
                if center.distance_to(first.pos) <= radius {
                    return true;
                }
            }
            continue;
        }
        for seg_idx in 0..seg_count {
            if let Some(seg) = sp.segment_to_kurbo(seg_idx) {
                if segment_min_dist_to_point(seg, center) <= radius {
                    return true;
                }
            }
        }
    }
    include_interior && closed_subpaths_contain_even_odd(&path.subpaths, center)
}

fn find_eraser_hit_in_subtree(
    doc: &Document,
    id: ObjectId,
    centers: &[Point2D],
    eraser_bb: Option<Rect2D>,
    radius: f64,
    depth: usize,
) -> Option<ObjectId> {
    if depth > 64 {
        return None;
    }
    let obj = doc.object(id)?;
    if !obj.visible || obj.locked {
        return None;
    }
    match &obj.payload {
        NodePayload::Path(_) | NodePayload::Shape(_) | NodePayload::BSpline(_) => {
            if !is_object_editable_in_doc(doc, id) {
                return None;
            }
            let wp = doc.world_path_of(id)?;
            let bb_ok = eraser_bb
                .zip(wp.bounding_box())
                .is_none_or(|(eb, wb)| wb.intersects(eb));
            let include_interior = obj.fill.is_visible();
            if bb_ok
                && centers
                    .iter()
                    .any(|&c| path_intersects_disk(&wp, c, radius, include_interior))
            {
                Some(id)
            } else {
                None
            }
        }
        NodePayload::Group(children) => {
            for &cid in children.iter().rev() {
                if let Some(found) =
                    find_eraser_hit_in_subtree(doc, cid, centers, eraser_bb, radius, depth + 1)
                {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Builds Vector Eraser [`CommandOp`]s for all distinct editable vector objects intersected
/// along `points` (resolving the frontmost visible editable target at each sample along the
/// stroke so occluded objects behind opaque shapes are respected while strokes spanning
/// multiple objects in a `Group` or multi-selection erase every crossed object atomically).
#[must_use]
pub fn build_vector_eraser_ops(ctx: &ToolReadContext<'_>, points: &[Point2D]) -> Vec<CommandOp> {
    if points.is_empty() {
        return Vec::new();
    }
    let radius = ctx.settings.eraser_radius_mm.max(0.25);
    let step = (radius * 0.4).max(0.25);
    let mut centers = Vec::with_capacity(points.len());
    if points[0].is_finite() {
        centers.push(points[0]);
    }
    for seg in points.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        if !a.is_finite() || !b.is_finite() {
            if b.is_finite() && centers.last().is_none_or(|&last| last != b) {
                centers.push(b);
            }
            continue;
        }
        if centers.last().is_none_or(|&last| last != a) {
            centers.push(a);
        }
        let dist = a.distance_to(b);
        let steps = ((dist / step).ceil() as usize).clamp(1, 256);
        for k in 1..=steps {
            let pt = if k == steps {
                b
            } else {
                a.lerp(b, k as f64 / steps as f64)
            };
            centers.push(pt);
        }
    }
    if centers.is_empty() {
        return Vec::new();
    }
    let is_editable_vector = |id: ObjectId| {
        is_object_editable_in_doc(ctx.document, id)
            && ctx.document.object(id).is_some_and(|o| {
                matches!(
                    o.payload,
                    NodePayload::Path(_) | NodePayload::Shape(_) | NodePayload::BSpline(_)
                )
            })
            && ctx.document.world_path_of(id).is_some()
    };
    let eraser_bb =
        Rect2D::from_iter_points(centers.iter().copied()).map(|r| r.expand(radius + 0.5));
    let mut seen = HashSet::new();
    let mut hit_oids = Vec::new();

    if !ctx.selection.objects.is_empty() {
        let ordered_sel = order_selection_frontmost_first(ctx.document, &ctx.selection.objects);
        for &p in &centers {
            let single = [p];
            if let Some(oid) = ordered_sel.iter().find_map(|&id| {
                find_eraser_hit_in_subtree(ctx.document, id, &single, eraser_bb, radius, 0)
            }) {
                if seen.insert(oid) {
                    hit_oids.push(oid);
                }
            }
        }
    } else {
        for &p in &centers {
            let single = [p];
            let mut found_at_p = None;
            if let Some(hit) = hit_test_canvas_point_opt_tol(ctx, p, radius) {
                let hit_include_interior = ctx
                    .document
                    .object(hit)
                    .is_some_and(|o| o.fill.is_visible());
                if is_editable_vector(hit)
                    && ctx.document.world_path_of(hit).is_some_and(|wp| {
                        path_intersects_disk(&wp, p, radius, hit_include_interior)
                    })
                {
                    found_at_p = Some(hit);
                } else if ctx
                    .document
                    .object(hit)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Group(_)))
                {
                    found_at_p = find_eraser_hit_in_subtree(
                        ctx.document,
                        hit,
                        &single,
                        eraser_bb,
                        radius,
                        0,
                    );
                }
                if found_at_p.is_none() {
                    if let Some(pc_id) = ctx.powerclip_edit_id {
                        if let Some(NodePayload::PowerClip { contents, .. }) =
                            ctx.document.object(pc_id).map(|o| &o.payload)
                        {
                            for &cid in contents.iter().rev() {
                                if cid == hit {
                                    continue;
                                }
                                if let Some(found) = find_eraser_hit_in_subtree(
                                    ctx.document,
                                    cid,
                                    &single,
                                    eraser_bb,
                                    radius,
                                    0,
                                ) {
                                    found_at_p = Some(found);
                                    break;
                                }
                            }
                        }
                    }
                }
                if found_at_p.is_none() {
                    let search_rect = Rect2D::from_points(p, p).expand(radius);
                    for cand in hit_test_rect(
                        ctx.document,
                        ctx.document.active_page_index(),
                        search_rect,
                        false,
                    ) {
                        if cand == hit {
                            continue;
                        }
                        if let Some(found) = find_eraser_hit_in_subtree(
                            ctx.document,
                            cand,
                            &single,
                            eraser_bb,
                            radius,
                            0,
                        ) {
                            found_at_p = Some(found);
                            break;
                        }
                    }
                }
            }
            if let Some(oid) = found_at_p {
                if seen.insert(oid) {
                    hit_oids.push(oid);
                }
            }
        }
    }

    let mut ops = Vec::new();
    for oid in hit_oids {
        if !is_object_editable_in_doc(ctx.document, oid) {
            continue;
        }
        let Some(wp) = ctx.document.world_path_of(oid) else {
            continue;
        };
        if let (Some(wp_bb), Some(ebb)) = (wp.bounding_box(), eraser_bb) {
            if !wp_bb.intersects(ebb) {
                continue;
            }
        }
        let include_interior = ctx
            .document
            .object(oid)
            .is_some_and(|o| o.fill.is_visible());
        if !centers
            .iter()
            .any(|&c| path_intersects_disk(&wp, c, radius, include_interior))
        {
            continue;
        }
        let mut pieces = vec![wp.clone()];
        let mut any_erased = false;
        for &center in &centers {
            let mut next_pieces = Vec::new();
            for piece in &pieces {
                if !piece.is_empty() {
                    if path_intersects_disk(piece, center, radius, include_interior) {
                        any_erased = true;
                        next_pieces.extend(coree_core::erase_circle(piece, center, radius));
                    } else {
                        next_pieces.push(piece.clone());
                    }
                }
            }
            pieces = next_pieces;
            if pieces.is_empty() {
                break;
            }
        }
        if !any_erased {
            continue;
        }
        let combined = BezierPath {
            subpaths: pieces.into_iter().flat_map(|p| p.subpaths).collect(),
        };
        if combined == wp {
            continue;
        }
        let inv = world_inverse_transform_of(ctx.document, oid);
        let updated_local = combined.transformed(&inv);
        let orig_local = match ctx.document.object(oid).map(|o| &o.payload) {
            Some(NodePayload::Path(p)) => p.clone(),
            Some(NodePayload::Shape(s)) => s.to_bezier_path(),
            Some(NodePayload::BSpline(bs)) => bs.to_bezier_path(),
            _ => continue,
        };
        if updated_local == orig_local {
            continue;
        }
        ops.push(build_edit_path_op(ctx.document, oid, updated_local));
    }
    ops
}

/// Builds a Vector Eraser [`CommandOp`] along `points` (batching multiple erased targets atomically).
#[must_use]
pub fn build_vector_eraser_op(ctx: &ToolReadContext<'_>, points: &[Point2D]) -> Option<CommandOp> {
    let mut ops = build_vector_eraser_ops(ctx, points);
    match ops.len() {
        0 => None,
        1 => ops.pop(),
        _ => Some(CommandOp::Batch(ops)),
    }
}

/// Returns `true` if `payload` directly renders [`SceneObject::fill`].
#[must_use]
pub fn is_fillable_leaf_payload(payload: &NodePayload) -> bool {
    matches!(
        payload,
        NodePayload::Path(_)
            | NodePayload::Shape(_)
            | NodePayload::ArtisticText(_)
            | NodePayload::ParagraphText(_)
            | NodePayload::TextOnPath(_)
            | NodePayload::Table(_)
            | NodePayload::PowerClip { .. }
    )
}

fn collect_editable_fillable_leaves_in_subtree(
    doc: &Document,
    id: ObjectId,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
    out: &mut Vec<ObjectId>,
) {
    if depth > 64 || !visited.insert(id) {
        return;
    }
    if !is_object_editable_in_doc(doc, id) {
        return;
    }
    let Some(obj) = doc.object(id) else {
        return;
    };
    match &obj.payload {
        NodePayload::Group(children) => {
            for &cid in children.iter().rev() {
                collect_editable_fillable_leaves_in_subtree(doc, cid, depth + 1, visited, out);
            }
        }
        payload if is_fillable_leaf_payload(payload) => {
            out.push(id);
        }
        _ => {}
    }
}

/// Expands `ids` (ordered frontmost-first on the active page) recursively through
/// [`NodePayload::Group`] into all distinct editable fillable leaf [`ObjectId`]s
/// (skipping hidden/locked/non-fillable items and keeping `PowerClip` containers intact).
#[must_use]
pub fn collect_editable_fillable_targets(doc: &Document, ids: &[ObjectId]) -> Vec<ObjectId> {
    let mut visited = HashSet::new();
    let mut out = Vec::new();
    for oid in order_selection_frontmost_first(doc, ids) {
        collect_editable_fillable_leaves_in_subtree(doc, oid, 0, &mut visited, &mut out);
    }
    out
}

fn find_fillable_hit_in_subtree(
    doc: &Document,
    id: ObjectId,
    pt: Point2D,
    tol_mm: f64,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) -> Option<ObjectId> {
    if depth > 64 || !visited.insert(id) {
        return None;
    }
    if !is_object_editable_in_doc(doc, id) {
        return None;
    }
    let obj = doc.object(id)?;
    match &obj.payload {
        NodePayload::Group(children) => {
            for &cid in children.iter().rev() {
                if let Some(found) =
                    find_fillable_hit_in_subtree(doc, cid, pt, tol_mm, depth + 1, visited)
                {
                    return Some(found);
                }
            }
            None
        }
        payload if is_fillable_leaf_payload(payload) => {
            let hit = doc
                .world_path_of(id)
                .is_some_and(|wp| path_intersects_disk(&wp, pt, tol_mm, true))
                || doc
                    .world_object_bounds(id)
                    .is_some_and(|bb| bb.expand(tol_mm).contains_point(pt));
            hit.then_some(id)
        }
        _ => None,
    }
}

/// Builds an Interactive Fill linear gradient [`CommandOp`] along `start_mm -> end_mm`.
#[must_use]
pub fn build_interactive_fill_op(
    ctx: &ToolReadContext<'_>,
    start_mm: Point2D,
    end_mm: Point2D,
) -> Option<CommandOp> {
    let target_roots: Vec<ObjectId> = if !ctx.selection.objects.is_empty() {
        ctx.selection.objects.clone()
    } else {
        hit_test_canvas_point_opt_tol(ctx, start_mm, 8.0)
            .into_iter()
            .collect()
    };
    if target_roots.is_empty() {
        return None;
    }
    let leaf_ids = collect_editable_fillable_targets(ctx.document, &target_roots);
    let per_obj_ops: Vec<CommandOp> = leaf_ids
        .into_iter()
        .map(|oid| {
            let inv = world_inverse_transform_of(ctx.document, oid);
            let fill = Fill::LinearGradient {
                start: inv.transform_point(start_mm),
                end: inv.transform_point(end_mm),
                stops: vec![
                    GradientStop::new(0.0, ctx.settings.grad_start_color.clone()),
                    GradientStop::new(1.0, ctx.settings.grad_end_color.clone()),
                ],
            };
            CommandOp::SetFill {
                ids: vec![oid],
                fill,
            }
        })
        .collect();
    if per_obj_ops.is_empty() {
        None
    } else if per_obj_ops.len() == 1 {
        per_obj_ops.into_iter().next()
    } else {
        Some(CommandOp::Batch(per_obj_ops))
    }
}

/// Builds a [`ToolOutput`] for an interactive effect drag (`DropShadow`..`Transparency`).
#[must_use]
pub fn build_interactive_effect_output(
    ctx: &ToolReadContext<'_>,
    tool: ToolId,
    start_mm: Point2D,
    end_mm: Point2D,
) -> ToolOutput {
    let target_id = if !ctx.selection.objects.is_empty() {
        resolve_selected_object_at_or_first(ctx, start_mm, 8.0)
    } else {
        hit_test_canvas_point_opt_tol(ctx, start_mm, 8.0)
            .filter(|&id| is_object_editable_in_doc(ctx.document, id))
    };
    let Some(oid) = target_id else {
        return ToolOutput::actions(vec![ToolActionRequest::Toast {
            message: "Select or click an object to apply this interactive effect".to_owned(),
            is_error: false,
        }]);
    };

    let mut post_actions = Vec::new();
    if !ctx.selection.contains(oid) {
        post_actions.push(ToolActionRequest::SelectObjects {
            ids: vec![oid],
            additive: false,
            toggle: false,
        });
    }

    let delta = end_mm - start_mm;
    let dist = delta.length();
    let ts = ctx.settings;

    let (existing_idx, new_effect) = match tool {
        ToolId::DropShadow => {
            let idx = ctx.document.object(oid).and_then(|o| {
                o.effects
                    .iter()
                    .position(|e| matches!(e, Effect::DropShadow { .. }))
            });
            let dx = if dist > 0.25 {
                delta.x
            } else {
                ts.effect_dx_mm
            };
            let dy = if dist > 0.25 {
                delta.y
            } else {
                ts.effect_dy_mm
            };
            (
                idx,
                Effect::DropShadow {
                    dx_mm: dx,
                    dy_mm: dy,
                    blur_mm: ts.effect_blur_mm.max(0.5),
                    opacity: ts.effect_opacity.clamp(0.1, 1.0),
                    color: ts.effect_color.clone(),
                    feather_direction: FeatherDirection::Middle,
                },
            )
        }
        ToolId::BlockShadow => {
            let idx = ctx.document.object(oid).and_then(|o| {
                o.effects
                    .iter()
                    .position(|e| matches!(e, Effect::BlockShadow { .. }))
            });
            let dx = if dist > 0.25 {
                delta.x
            } else {
                ts.effect_dx_mm
            };
            let dy = if dist > 0.25 {
                delta.y
            } else {
                ts.effect_dy_mm
            };
            (
                idx,
                Effect::BlockShadow {
                    dx_mm: dx,
                    dy_mm: dy,
                    color: ts.effect_color.clone(),
                    simplify: true,
                    overprint: false,
                },
            )
        }
        ToolId::Contour => {
            let idx = ctx.document.object(oid).and_then(|o| {
                o.effects
                    .iter()
                    .position(|e| matches!(e, Effect::Contour { .. }))
            });
            let wb = ctx.document.world_object_bounds(oid);
            let (mode, offset_mm) = if let Some(bb) = wb {
                let c = bb.center();
                let rx = (bb.width() * 0.5).max(1.0);
                let ry = (bb.height() * 0.5).max(1.0);
                let rel = ((end_mm.x - c.x) / rx).hypot((end_mm.y - c.y) / ry);
                if rel < 0.25 {
                    (ContourMode::ToCenter, (dist / 3.0).clamp(0.5, 25.0))
                } else if bb.contains_point(end_mm) {
                    (ContourMode::Inside, (dist / 2.5).clamp(0.5, 25.0))
                } else {
                    (ContourMode::Outside, (dist / 2.5).clamp(0.5, 25.0))
                }
            } else {
                (ts.effect_contour_mode, dist.clamp(0.5, 25.0))
            };
            (
                idx,
                Effect::Contour {
                    mode,
                    steps: ts.effect_steps.clamp(1, 50),
                    offset_mm,
                    fill_color: ts.effect_color.clone(),
                    outline_color: Color::BLACK,
                },
            )
        }
        ToolId::Blend => {
            let idx = ctx.document.object(oid).and_then(|o| {
                o.effects
                    .iter()
                    .position(|e| matches!(e, Effect::Blend { .. }))
            });
            let target_b = hit_test_canvas_point_opt_tol(ctx, end_mm, 12.0)
                .filter(|&id| id != oid)
                .or_else(|| ctx.selection.objects.iter().copied().find(|&id| id != oid))
                .or_else(|| ctx.document.objects().keys().copied().find(|&id| id != oid));
            let Some(target_id) = target_b else {
                return ToolOutput::actions(vec![ToolActionRequest::Toast {
                    message: "Drag from one object to a second object to Blend".to_owned(),
                    is_error: false,
                }]);
            };
            (
                idx,
                Effect::Blend {
                    target_id,
                    steps: ts.effect_steps.clamp(1, 100),
                    rotation_deg: 0.0,
                    along_path: None,
                },
            )
        }
        ToolId::Distort => {
            let idx = ctx.document.object(oid).and_then(|o| {
                o.effects
                    .iter()
                    .position(|e| matches!(e, Effect::Distort { .. }))
            });
            let inv = world_inverse_transform_of(ctx.document, oid);
            let ls = inv.transform_point(start_mm);
            let le = inv.transform_point(end_mm);
            (
                idx,
                Effect::Distort {
                    kind: ts.effect_distort_kind,
                    amplitude: ls.distance_to(le).clamp(2.0, 120.0),
                    frequency: ts.effect_frequency.max(0.5),
                    center: ls,
                },
            )
        }
        ToolId::Envelope => {
            let existing = ctx.document.object(oid).and_then(|o| {
                o.effects
                    .iter()
                    .position(|e| matches!(e, Effect::Envelope { .. }))
            });
            if let Some(idx) = existing {
                if let Some(Effect::Envelope { mode, mut cage }) = ctx
                    .document
                    .object(oid)
                    .and_then(|o| o.effects.get(idx).cloned())
                {
                    let inv = world_inverse_transform_of(ctx.document, oid);
                    let ls = inv.transform_point(start_mm);
                    let le = inv.transform_point(end_mm);
                    if let Some((best_i, _)) = cage.iter().enumerate().min_by(|a, b| {
                        a.1.distance_to(ls)
                            .partial_cmp(&b.1.distance_to(ls))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }) {
                        cage[best_i] = le;
                    }
                    let op =
                        build_replace_effect_op(oid, Some(idx), Effect::Envelope { mode, cage });
                    return ToolOutput::Commit {
                        ops: vec![op],
                        clear_overlay: true,
                        post_actions,
                    };
                }
            }
            let local_bb = ctx
                .document
                .local_object_bounds(oid)
                .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 40.0, 40.0));
            let cx = local_bb.center().x;
            let cy = local_bb.center().y;
            let mut cage = vec![
                Point2D::new(local_bb.min_x, local_bb.min_y),
                Point2D::new(cx, local_bb.min_y),
                Point2D::new(local_bb.max_x, local_bb.min_y),
                Point2D::new(local_bb.max_x, cy),
                Point2D::new(local_bb.max_x, local_bb.max_y),
                Point2D::new(cx, local_bb.max_y),
                Point2D::new(local_bb.min_x, local_bb.max_y),
                Point2D::new(local_bb.min_x, cy),
            ];
            let inv = world_inverse_transform_of(ctx.document, oid);
            let ls = inv.transform_point(start_mm);
            let le = inv.transform_point(end_mm);
            if let Some((best_i, _)) = cage.iter().enumerate().min_by(|a, b| {
                a.1.distance_to(ls)
                    .partial_cmp(&b.1.distance_to(ls))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                cage[best_i] = le;
            }
            (
                None,
                Effect::Envelope {
                    mode: ts.effect_envelope_mode,
                    cage,
                },
            )
        }
        ToolId::Extrude25D => {
            let idx = ctx.document.object(oid).and_then(|o| {
                o.effects
                    .iter()
                    .position(|e| matches!(e, Effect::Extrude25D { .. }))
            });
            (
                idx,
                Effect::Extrude25D {
                    vanishing_point: end_mm,
                    depth: dist.clamp(2.0, 200.0),
                    bevel_depth: 1.0,
                    shade_color: ts.effect_color.clone(),
                    light_dir: [0.4, -0.5, 0.75],
                },
            )
        }
        ToolId::Transparency => {
            let idx = ctx.document.object(oid).and_then(|o| {
                o.effects
                    .iter()
                    .position(|e| matches!(e, Effect::Transparency { .. }))
            });
            let inv = world_inverse_transform_of(ctx.document, oid);
            let local_start = inv.transform_point(start_mm);
            let local_end = inv.transform_point(end_mm);
            let mode = if local_start.distance_to(local_end) < 0.5 {
                TransparencyMode::Uniform(ts.effect_opacity.clamp(0.05, 0.95))
            } else {
                TransparencyMode::FountainLinear {
                    start: local_start,
                    end: local_end,
                    start_alpha: 1.0,
                    end_alpha: 0.0,
                }
            };
            (
                idx,
                Effect::Transparency {
                    mode,
                    blend_mode: ts.effect_blend_mode,
                },
            )
        }
        _ => return ToolOutput::noop(),
    };

    post_actions.push(ToolActionRequest::Toast {
        message: format!("Applied {}", tool.label()),
        is_error: false,
    });
    ToolOutput::Commit {
        ops: vec![build_replace_effect_op(oid, existing_idx, new_effect)],
        clear_overlay: true,
        post_actions,
    }
}

/// Builds a [`ToolOutput`] for a drawn rectangle gesture across any of the 65 [`ToolId`]s.
#[must_use]
pub fn build_drawn_box_output(ctx: &ToolReadContext<'_>, tool: ToolId, rect: Rect2D) -> ToolOutput {
    if !rect.is_finite() || (rect.width() < 0.5 && rect.height() < 0.5) {
        return ToolOutput::noop();
    }
    let fill = ctx.default_fill.clone();
    let outline = ctx.default_outline.clone();
    let ts = ctx.settings;

    let (name, payload, xf) = match tool {
        ToolId::Pick => {
            let hits = if let Some(pc_id) = ctx.powerclip_edit_id {
                hit_test_powerclip_contents_rect(ctx.document, pc_id, rect, false)
            } else {
                hit_test_rect(ctx.document, ctx.document.active_page_index(), rect, false)
            };
            return ToolOutput::actions(vec![ToolActionRequest::SelectObjects {
                ids: hits,
                additive: false,
                toggle: false,
            }]);
        }
        ToolId::FreehandPick => {
            let poly = vec![
                rect.min(),
                Point2D::new(rect.max_x, rect.min_y),
                rect.max(),
                Point2D::new(rect.min_x, rect.max_y),
            ];
            let hits = compute_lasso_hits_scoped(ctx.document, ctx.powerclip_edit_id, &poly);
            return ToolOutput::actions(vec![ToolActionRequest::SelectObjects {
                ids: hits,
                additive: false,
                toggle: false,
            }]);
        }
        ToolId::FreeTransform => {
            let ids = if ctx.selection.objects.is_empty() {
                hit_test_canvas_point_opt(ctx, rect.min())
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                ctx.selection.objects.clone()
            };
            if let Some(aff) =
                compute_free_transform_affine(rect.min(), rect.center(), rect.max(), false, false)
            {
                if let Some(cmd) =
                    crate::math_helpers::build_world_transform_command(ctx.document, &ids, aff)
                {
                    return ToolOutput::Commit {
                        ops: vec![cmd],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    };
                }
            }
            return ToolOutput::noop();
        }
        ToolId::ShapeNode => {
            return ToolOutput::actions(vec![ToolActionRequest::SelectNodes {
                nodes: collect_nodes_in_rect(ctx.document, &ctx.selection.objects, rect),
                additive: false,
            }]);
        }
        ToolId::Smooth | ToolId::Smear | ToolId::Twirl | ToolId::AttractRepel | ToolId::Roughen => {
            return build_sculpt_stroke_output(
                ctx,
                tool,
                &[rect.min(), rect.center(), rect.max()],
                false,
            );
        }
        ToolId::Crop => {
            return ToolOutput::actions(vec![ToolActionRequest::ApplyCropRect(rect)]);
        }
        ToolId::Knife => {
            if let Some((op, new_ids)) = build_knife_cut_op(
                ctx.document,
                &ctx.selection.objects,
                ctx.active_layer_id,
                rect.min(),
                rect.max(),
            ) {
                return ToolOutput::Commit {
                    ops: vec![op],
                    clear_overlay: true,
                    post_actions: vec![ToolActionRequest::SelectObjects {
                        ids: new_ids,
                        additive: false,
                        toggle: false,
                    }],
                };
            }
            return ToolOutput::noop();
        }
        ToolId::VirtualSegmentDelete => {
            if let Some(op) = build_virtual_segment_delete_op(ctx, rect.center()) {
                return ToolOutput::Commit {
                    ops: vec![op],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                };
            }
            return ToolOutput::noop();
        }
        ToolId::Eraser => {
            let ops = build_vector_eraser_ops(ctx, &[rect.min(), rect.center(), rect.max()]);
            if !ops.is_empty() {
                return ToolOutput::Commit {
                    ops,
                    clear_overlay: true,
                    post_actions: Vec::new(),
                };
            }
            return ToolOutput::noop();
        }
        ToolId::Zoom => {
            return ToolOutput::actions(vec![ToolActionRequest::ZoomFitRect {
                rect_mm: rect,
                margin_px: 12.0,
                one_shot_complete: true,
            }]);
        }
        ToolId::Pan => {
            let mut pan = ctx.viewport.pan_mm;
            pan.x -= rect.width() * 0.5;
            pan.y -= rect.height() * 0.5;
            return ToolOutput::actions(vec![ToolActionRequest::PanTo {
                pan_mm: pan,
                lod_dragging: false,
            }]);
        }
        ToolId::Freehand => {
            let path = BezierPath::from_polygon(vec![rect.min(), rect.center(), rect.max()], false);
            (
                "Freehand Curve".to_owned(),
                NodePayload::Path(path),
                Affine2D::IDENTITY,
            )
        }
        ToolId::TwoPointLine => {
            let path = BezierPath::from_line(rect.min(), rect.max());
            (
                "Line".to_owned(),
                NodePayload::Path(path),
                Affine2D::IDENTITY,
            )
        }
        ToolId::Bezier | ToolId::Pen => {
            let p0 = rect.min();
            let p1 = rect.max();
            let c1 = Point2D::new(rect.max_x, rect.min_y);
            let c2 = Point2D::new(rect.min_x, rect.max_y);
            let sp = Subpath {
                nodes: vec![
                    PathNode::new(p0, None, Some(c1), NodeKind::Smooth),
                    PathNode::new(p1, Some(c2), None, NodeKind::Smooth),
                ],
                closed: false,
            };
            (
                tool.label().to_owned(),
                NodePayload::Path(BezierPath::from_subpath(sp)),
                Affine2D::IDENTITY,
            )
        }
        ToolId::BSpline => {
            let pts = vec![
                rect.min(),
                Point2D::new(rect.center().x, rect.min_y),
                Point2D::new(rect.center().x, rect.max_y),
                rect.max(),
            ];
            if let Some(op) = build_bspline_create_op(ctx, &pts, false) {
                return ToolOutput::Commit {
                    ops: vec![op],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                };
            }
            return ToolOutput::noop();
        }
        ToolId::Polyline => {
            let path = BezierPath::from_polygon(
                vec![rect.min(), Point2D::new(rect.max_x, rect.min_y), rect.max()],
                false,
            );
            (
                "Polyline".to_owned(),
                NodePayload::Path(path),
                Affine2D::IDENTITY,
            )
        }
        ToolId::ThreePointCurve => {
            let p1 = Point2D::new(rect.min_x, rect.max_y);
            let p2 = Point2D::new(rect.max_x, rect.max_y);
            let p3 = Point2D::new(rect.center().x, rect.min_y);
            if let Some(op) = build_three_point_curve_op(ctx, p1, p2, p3) {
                return ToolOutput::Commit {
                    ops: vec![op],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                };
            }
            return ToolOutput::noop();
        }
        ToolId::SmartDrawing => {
            let pts = vec![
                rect.min(),
                Point2D::new(rect.max_x, rect.min_y),
                rect.max(),
                Point2D::new(rect.min_x, rect.max_y),
                rect.min(),
            ];
            let recognized = SmartDrawing::recognize_rough_stroke(&pts);
            (
                "Smart Shape".to_owned(),
                NodePayload::Path(recognized.to_bezier_path()),
                Affine2D::IDENTITY,
            )
        }
        ToolId::LiveSketch => {
            let stroke = vec![rect.min(), rect.center(), rect.max()];
            let path = LiveSketch::average_strokes(&[stroke], 24);
            (
                "LiveSketch Curve".to_owned(),
                NodePayload::Path(path),
                Affine2D::IDENTITY,
            )
        }
        ToolId::ArtisticMedia => {
            let center = BezierPath::from_line(rect.min(), rect.max());
            (
                "Artistic Media".to_owned(),
                NodePayload::ArtisticMedia {
                    spine: center,
                    style: ArtisticMediaStyle::Preset {
                        max_width_mm: ts.artistic_media_width_mm,
                        profile: ts.artistic_media_profile.clone(),
                    },
                },
                Affine2D::IDENTITY,
            )
        }
        ToolId::Rectangle | ToolId::ThreePointRectangle => (
            "Rectangle".to_owned(),
            NodePayload::Shape(ParametricShape::Rectangle {
                width: rect.width(),
                height: rect.height(),
                corner_radii: [ts.rect_corner_radius_mm; 4],
                corner_style: ts.rect_corner_style,
                lock_corners: true,
            }),
            Affine2D::translate(rect.min_x, rect.min_y),
        ),
        ToolId::Ellipse | ToolId::ThreePointEllipse => (
            "Ellipse".to_owned(),
            NodePayload::Shape(ParametricShape::Ellipse {
                rx: rect.width() * 0.5,
                ry: rect.height() * 0.5,
                mode: ts.ellipse_mode,
                start_deg: 0.0,
                end_deg: 360.0,
            }),
            Affine2D::translate(rect.center().x, rect.center().y),
        ),
        ToolId::Polygon => (
            "Polygon".to_owned(),
            NodePayload::Shape(ParametricShape::Polygon {
                sides: ts.polygon_sides.clamp(3, 64),
                radius_x: rect.width() * 0.5,
                radius_y: rect.height() * 0.5,
            }),
            Affine2D::translate(rect.center().x, rect.center().y),
        ),
        ToolId::Star | ToolId::ComplexStar => (
            "Star".to_owned(),
            NodePayload::Shape(ParametricShape::Star {
                points: ts.star_points.clamp(3, 64),
                outer_radius: (rect.width().max(rect.height())) * 0.5,
                inner_ratio: ts.star_inner_ratio.clamp(0.1, 0.9),
                complex: tool == ToolId::ComplexStar,
            }),
            Affine2D::translate(rect.center().x, rect.center().y),
        ),
        ToolId::Spiral => (
            "Spiral".to_owned(),
            NodePayload::Shape(ParametricShape::Spiral {
                revolutions: ts.spiral_revolutions.clamp(1.0, 30.0),
                radius: (rect.width().max(rect.height())) * 0.5,
                logarithmic: ts.spiral_logarithmic,
                expansion: 1.5,
            }),
            Affine2D::translate(rect.center().x, rect.center().y),
        ),
        ToolId::GraphPaper => (
            "Graph Paper".to_owned(),
            NodePayload::Shape(ParametricShape::GraphPaper {
                cols: ts.graph_cols.clamp(1, 50),
                rows: ts.graph_rows.clamp(1, 50),
                width: rect.width(),
                height: rect.height(),
            }),
            Affine2D::translate(rect.min_x, rect.min_y),
        ),
        ToolId::Impact => (
            "Impact Rays".to_owned(),
            NodePayload::Shape(ParametricShape::Impact {
                rays: ts.impact_rays.clamp(4, 128),
                inner_radius: (rect.width().min(rect.height())) * 0.15,
                outer_radius: (rect.width().max(rect.height())) * 0.5,
                style: ts.impact_style,
            }),
            Affine2D::translate(rect.center().x, rect.center().y),
        ),
        ToolId::BasicShapes => (
            format!("{:?}", ts.basic_shape_kind),
            NodePayload::Shape(ParametricShape::BasicShape {
                kind: ts.basic_shape_kind,
                width: rect.width(),
                height: rect.height(),
            }),
            Affine2D::translate(rect.min_x, rect.min_y),
        ),
        ToolId::ArtisticText => {
            let Some(layer_id) = ctx.target_creation_layer_id() else {
                return ToolOutput::noop();
            };
            let text = ArtisticText::from_plain(
                ts.text_insert_draft.clone(),
                ctx.default_text_style.clone(),
            );
            let obj = SceneObject::new(
                ObjectId(0),
                "Artistic Text",
                NodePayload::ArtisticText(text),
            )
            .with_fill(Fill::Solid(ctx.default_text_style.fill.clone()))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(rect.min().x, rect.min().y));
            return ToolOutput::Commit {
                ops: vec![CommandOp::create_object(obj, Some(layer_id))],
                clear_overlay: true,
                post_actions: Vec::new(),
            };
        }
        ToolId::ParagraphText => (
            "Paragraph Text".to_owned(),
            NodePayload::ParagraphText(ParagraphText::new(
                ts.text_insert_draft.clone(),
                Vec2D::new(rect.width().max(20.0), rect.height().max(12.0)),
                ctx.default_text_style.clone(),
            )),
            Affine2D::translate(rect.min_x, rect.min_y),
        ),
        ToolId::Table => {
            let rows = ts.table_rows.clamp(1, 30);
            let cols = ts.table_cols.clamp(1, 30);
            (
                format!("Table {rows}x{cols}"),
                NodePayload::Table(VectorTable::new(
                    rows,
                    cols,
                    (rect.width() / cols as f64).max(4.0),
                    (rect.height() / rows as f64).max(4.0),
                )),
                Affine2D::translate(rect.min_x, rect.min_y),
            )
        }
        ToolId::ParallelDimension
        | ToolId::HvDimension
        | ToolId::AngularDimension
        | ToolId::SegmentDimension
        | ToolId::ThreePointCallout => {
            let kind = match tool {
                ToolId::HvDimension => {
                    if rect.width() >= rect.height() {
                        DimensionKind::Horizontal
                    } else {
                        DimensionKind::Vertical
                    }
                }
                ToolId::AngularDimension => DimensionKind::Angular,
                ToolId::SegmentDimension => DimensionKind::Segment,
                ToolId::ThreePointCallout => DimensionKind::Callout3Pt,
                _ => DimensionKind::Parallel,
            };
            let mut dim = if kind == DimensionKind::Angular {
                DimensionObject::angular(
                    rect.min(),
                    Point2D::new(rect.max_x, rect.min_y),
                    rect.max(),
                    ts.dimension_offset_mm,
                )
            } else {
                DimensionObject::linear(kind, rect.min(), rect.max(), ts.dimension_offset_mm)
            };
            if kind == DimensionKind::Callout3Pt {
                dim.p3 = Some(Point2D::new(rect.max_x + 10.0, rect.min_y));
                dim.label_override = Some("Callout".to_owned());
            }
            (
                format!("{kind:?} Dimension"),
                NodePayload::Dimension(dim),
                Affine2D::IDENTITY,
            )
        }
        ToolId::StraightConnector | ToolId::RightAngleConnector => {
            if let Some(op) = build_connector_op(
                ctx,
                rect.min(),
                rect.max(),
                tool == ToolId::RightAngleConnector,
            ) {
                return ToolOutput::Commit {
                    ops: vec![op],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                };
            }
            return ToolOutput::noop();
        }
        ToolId::DropShadow
        | ToolId::BlockShadow
        | ToolId::Contour
        | ToolId::Blend
        | ToolId::Distort
        | ToolId::Envelope
        | ToolId::Extrude25D
        | ToolId::Transparency => {
            return build_interactive_effect_output(ctx, tool, rect.min(), rect.max());
        }
        ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
            return ToolOutput::actions(vec![ToolActionRequest::ApplyEyedropper {
                pt_mm: rect.center(),
                attributes_mode: tool == ToolId::AttributesEyedropper,
                force_sample: false,
            }]);
        }
        ToolId::InteractiveFill => {
            if let Some(op) = build_interactive_fill_op(ctx, rect.min(), rect.max()) {
                return ToolOutput::Commit {
                    ops: vec![op],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                };
            }
            return ToolOutput::noop();
        }
        ToolId::MeshFill => {
            return build_mesh_fill_output(ctx, rect.min(), rect.max(), false);
        }
        ToolId::SmartFill => {
            return ToolOutput::actions(vec![ToolActionRequest::SmartFillAt {
                pt_mm: rect.center(),
            }]);
        }
        ToolId::LensTool => {
            let Some(layer_id) = ctx.target_creation_layer_id() else {
                return ToolOutput::noop();
            };
            let lens = ctx.default_lens.clone();
            let mut obj = SceneObject::new(
                ObjectId(0),
                format!("{} Lens", lens.kind.name()),
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: rect.width() * 0.5,
                    ry: rect.height() * 0.5,
                    mode: EllipseMode::Full,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.25))
            .with_transform(Affine2D::translate(rect.center().x, rect.center().y));
            obj.lens = Some(lens);
            return ToolOutput::Commit {
                ops: vec![CommandOp::create_object(obj, Some(layer_id))],
                clear_overlay: true,
                post_actions: Vec::new(),
            };
        }
        ToolId::BitmapBrush | ToolId::BitmapEraser | ToolId::CloneStamp | ToolId::Smudge => {
            return ToolOutput::actions(vec![ToolActionRequest::ApplyRasterStroke {
                tool,
                points_mm: vec![rect.min(), rect.max()],
            }]);
        }
        ToolId::MaskSelect => {
            return ToolOutput::actions(vec![
                ToolActionRequest::SetRasterMaskRect(Some(rect)),
                ToolActionRequest::Toast {
                    message: format!(
                        "Set bitmap mask ({:.1} × {:.1} mm)",
                        rect.width(),
                        rect.height()
                    ),
                    is_error: false,
                },
            ]);
        }
    };

    let Some(layer_id) = ctx.target_creation_layer_id() else {
        return ToolOutput::noop();
    };
    let is_open_stroke = matches!(
        tool,
        ToolId::Freehand
            | ToolId::TwoPointLine
            | ToolId::Bezier
            | ToolId::Pen
            | ToolId::BSpline
            | ToolId::Polyline
            | ToolId::ThreePointCurve
            | ToolId::LiveSketch
            | ToolId::Spiral
            | ToolId::ParallelDimension
            | ToolId::HvDimension
            | ToolId::AngularDimension
            | ToolId::SegmentDimension
            | ToolId::ThreePointCallout
            | ToolId::StraightConnector
            | ToolId::RightAngleConnector
    );
    let mut obj = SceneObject::new(ObjectId(0), name, payload)
        .with_fill(if is_open_stroke { Fill::None } else { fill })
        .with_outline(if tool == ToolId::ArtisticMedia {
            OutlineStyle::none()
        } else {
            outline
        })
        .with_transform(xf);
    if ts.draw_in_perspective {
        if let (Some(world_h), Some(inv_xf)) = (
            ctx.document
                .perspective_grid()
                .plane_homography_for_rect(rect),
            xf.inverse(),
        ) {
            let local_h = Perspective3x3::from_affine(inv_xf)
                .compose(&world_h)
                .compose(&Perspective3x3::from_affine(xf));
            obj.perspective = Some(local_h);
        }
    }
    if ts.symmetry_enabled && ts.symmetry_folds >= 2 {
        let origin = ts.symmetry_center_mm.unwrap_or_else(|| {
            ctx.document.active_page().map_or(Point2D::ZERO, |p| {
                Point2D::new(p.width_mm * 0.5, p.height_mm * 0.5)
            })
        });
        obj.effects.push(Effect::Symmetry {
            axes: ts.symmetry_folds,
            mirror: ts.symmetry_mirror,
            origin,
        });
    }
    ToolOutput::Commit {
        ops: vec![CommandOp::create_object(obj, Some(layer_id))],
        clear_overlay: true,
        post_actions: Vec::new(),
    }
}

/// Builds a [`ToolOutput`] for a MeshFill interaction (`start_mm -> end_mm`).
#[must_use]
pub fn build_mesh_fill_output(
    ctx: &ToolReadContext<'_>,
    start_mm: Point2D,
    end_mm: Point2D,
    shift_recolor: bool,
) -> ToolOutput {
    let oid = if !ctx.selection.objects.is_empty() {
        resolve_selected_object_at_or_first(ctx, start_mm, 8.0)
    } else {
        hit_test_canvas_point_opt_tol(ctx, start_mm, 8.0)
            .filter(|&id| is_object_editable_in_doc(ctx.document, id))
    };
    let Some(oid) = oid else {
        return ToolOutput::noop();
    };
    let Some(root_obj) = ctx.document.object(oid) else {
        return ToolOutput::noop();
    };
    let target_ids: Vec<ObjectId> = match &root_obj.payload {
        NodePayload::Group(_) => {
            let mut visited = HashSet::new();
            if let Some(hit_leaf) =
                find_fillable_hit_in_subtree(ctx.document, oid, start_mm, 8.0, 0, &mut visited)
            {
                vec![hit_leaf]
            } else {
                collect_editable_fillable_targets(ctx.document, &[oid])
            }
        }
        payload if is_fillable_leaf_payload(payload) => vec![oid],
        _ => Vec::new(),
    };
    if target_ids.is_empty() {
        return ToolOutput::noop();
    }

    let rows = ctx.settings.mesh_rows.max(2);
    let cols = ctx.settings.mesh_cols.max(2);
    let mut ops = Vec::with_capacity(target_ids.len());
    for target_id in target_ids {
        let Some(obj) = ctx.document.object(target_id) else {
            continue;
        };
        let inv = world_inverse_transform_of(ctx.document, target_id);
        let mut mesh = match &obj.fill {
            Fill::MeshFill { .. } => obj.fill.clone(),
            _ => {
                let local_bb = ctx
                    .document
                    .local_object_bounds(target_id)
                    .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 40.0, 40.0));
                build_mesh_fill_for_bounds(local_bb, rows, cols)
            }
        };
        if let Fill::MeshFill { ref mut nodes, .. } = mesh {
            let ls = inv.transform_point(start_mm);
            let le = inv.transform_point(end_mm);
            if let Some((best_idx, _)) = nodes.iter().enumerate().min_by(|a, b| {
                a.1.pos
                    .distance_to(ls)
                    .partial_cmp(&b.1.pos.distance_to(ls))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                if ls.distance_to(le) > 0.25 {
                    nodes[best_idx].pos = le;
                }
                if shift_recolor {
                    if let Some(col) = ctx.default_fill.referenced_colors().into_iter().next() {
                        nodes[best_idx].color = col;
                    }
                }
            }
        }
        ops.push(CommandOp::SetFill {
            ids: vec![target_id],
            fill: mesh,
        });
    }
    if ops.is_empty() {
        return ToolOutput::noop();
    }
    let mut post_actions = Vec::new();
    if !is_object_in_or_descendant_of_selection(ctx.document, &ctx.selection.objects, oid) {
        post_actions.push(ToolActionRequest::SelectObjects {
            ids: vec![oid],
            additive: false,
            toggle: false,
        });
    }
    post_actions.push(ToolActionRequest::Toast {
        message: "Updated Mesh Fill grid".to_owned(),
        is_error: false,
    });
    ToolOutput::Commit {
        ops,
        clear_overlay: true,
        post_actions,
    }
}

fn object_active_page_z_rank(doc: &Document, oid: ObjectId) -> Option<(usize, usize, Vec<usize>)> {
    let mut cur = oid;
    let mut chain = HashSet::new();
    let mut rev_child_indices = Vec::new();
    while chain.len() <= 64 && chain.insert(cur) {
        let Some(pid) = doc.parent_container_of(cur) else {
            break;
        };
        let parent = doc.object(pid)?;
        let c_idx = match &parent.payload {
            NodePayload::Group(children) => children.iter().position(|&c| c == cur),
            NodePayload::PowerClip { contents, .. } => contents.iter().position(|&c| c == cur),
            _ => None,
        }?;
        rev_child_indices.push(c_idx);
        cur = pid;
    }
    rev_child_indices.reverse();
    let active_page_idx = doc.active_page_index();
    for (eff_idx, layer) in doc
        .effective_layers_for_page(active_page_idx)
        .into_iter()
        .enumerate()
    {
        if let Some(o_idx) = layer.objects.iter().position(|&id| id == cur) {
            if layer.visible && !layer.locked {
                return Some((eff_idx, o_idx, rev_child_indices));
            }
            return None;
        }
    }
    None
}

pub(crate) fn order_selection_frontmost_first(
    doc: &Document,
    selection: &[ObjectId],
) -> Vec<ObjectId> {
    let mut seen = HashSet::new();
    let mut ranked = Vec::with_capacity(selection.len());
    for (sel_idx, &id) in selection.iter().enumerate() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(rank) = object_active_page_z_rank(doc, id) {
            ranked.push((rank, sel_idx, id));
        }
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    ranked.into_iter().map(|(_, _, id)| id).collect()
}

pub(crate) fn is_object_in_or_descendant_of_selection(
    doc: &Document,
    selection: &[ObjectId],
    oid: ObjectId,
) -> bool {
    if selection.is_empty() {
        return false;
    }
    if selection.contains(&oid) {
        return true;
    }
    let mut cur = oid;
    let mut seen = HashSet::new();
    while seen.len() <= 64 && seen.insert(cur) {
        let Some(pid) = doc.parent_container_of(cur) else {
            break;
        };
        if selection.contains(&pid) {
            return true;
        }
        cur = pid;
    }
    false
}

pub(crate) fn resolve_selected_object_at_or_first(
    ctx: &ToolReadContext<'_>,
    pt: Point2D,
    tol_mm: f64,
) -> Option<ObjectId> {
    let ordered = order_selection_frontmost_first(ctx.document, &ctx.selection.objects);
    for &id in &ordered {
        if !is_object_editable_in_doc(ctx.document, id) {
            continue;
        }
        let hit = ctx
            .document
            .world_path_of(id)
            .is_some_and(|wp| path_intersects_disk(&wp, pt, tol_mm, true))
            || ctx
                .document
                .world_object_bounds(id)
                .is_some_and(|bb| bb.expand(tol_mm).contains_point(pt));
        if hit {
            return Some(id);
        }
    }
    ordered
        .into_iter()
        .find(|&id| is_object_editable_in_doc(ctx.document, id))
}

pub(crate) fn resolve_sculpt_target_in_selection(
    doc: &Document,
    selection: &[ObjectId],
    points: &[Point2D],
    radius_mm: f64,
) -> Option<ObjectId> {
    if selection.is_empty() || points.is_empty() {
        return None;
    }
    let brush_bb =
        Rect2D::from_iter_points(points.iter().copied()).map(|r| r.expand(radius_mm + 0.5));
    for id in order_selection_frontmost_first(doc, selection) {
        if let Some(found) = find_eraser_hit_in_subtree(doc, id, points, brush_bb, radius_mm, 0) {
            return Some(found);
        }
    }
    None
}

pub(crate) fn resolve_sculpt_target_at(
    ctx: &ToolReadContext<'_>,
    pt: Point2D,
    radius_mm: f64,
) -> Option<ObjectId> {
    let hit = hit_test_canvas_point_opt_tol(ctx, pt, radius_mm)?;
    if is_editable_vector_object(ctx.document, hit) {
        return Some(hit);
    }
    let eraser_bb = Some(Rect2D::from_points(pt, pt).expand(radius_mm + 0.5));
    if ctx
        .document
        .object(hit)
        .is_some_and(|o| matches!(o.payload, NodePayload::Group(_)))
    {
        if let Some(found) =
            find_eraser_hit_in_subtree(ctx.document, hit, &[pt], eraser_bb, radius_mm, 0)
        {
            return Some(found);
        }
    }
    if let Some(pc_id) = ctx.powerclip_edit_id {
        if let Some(NodePayload::PowerClip { contents, .. }) =
            ctx.document.object(pc_id).map(|o| &o.payload)
        {
            for &cid in contents.iter().rev() {
                if cid == hit {
                    continue;
                }
                if let Some(found) =
                    find_eraser_hit_in_subtree(ctx.document, cid, &[pt], eraser_bb, radius_mm, 0)
                {
                    return Some(found);
                }
            }
        }
    }
    let search_rect = Rect2D::from_points(pt, pt).expand(radius_mm);
    for cand in hit_test_rect(
        ctx.document,
        ctx.document.active_page_index(),
        search_rect,
        false,
    ) {
        if cand == hit {
            continue;
        }
        if let Some(found) =
            find_eraser_hit_in_subtree(ctx.document, cand, &[pt], eraser_bb, radius_mm, 0)
        {
            return Some(found);
        }
    }
    None
}

/// Builds a [`ToolOutput`] for a sculpt stroke (`Smooth`..`Roughen`).
#[must_use]
pub fn build_sculpt_stroke_output(
    ctx: &ToolReadContext<'_>,
    tool: ToolId,
    points: &[Point2D],
    shift_invert: bool,
) -> ToolOutput {
    if points.is_empty() {
        return ToolOutput::noop();
    }
    let radius_mm = ctx.settings.sculpt_radius_mm.clamp(1.0, 200.0);
    let strength = ctx.settings.sculpt_strength.clamp(0.05, 1.0);
    let target_id = if !ctx.selection.objects.is_empty() {
        resolve_sculpt_target_in_selection(ctx.document, &ctx.selection.objects, points, radius_mm)
    } else {
        points
            .iter()
            .find_map(|&pt| resolve_sculpt_target_at(ctx, pt, radius_mm))
    };
    let Some(oid) = target_id else {
        return ToolOutput::noop();
    };
    let Some(obj) = ctx.document.object(oid) else {
        return ToolOutput::noop();
    };
    let orig_path = match &obj.payload {
        NodePayload::Path(p) => p.clone(),
        NodePayload::Shape(s) => s.to_bezier_path(),
        _ => return ToolOutput::noop(),
    };
    let mut path = orig_path.clone();
    let inv = world_inverse_transform_of(ctx.document, oid);
    let scale_est = ((inv.m[0].hypot(inv.m[1]) + inv.m[2].hypot(inv.m[3])) * 0.5).max(1e-3);
    let local_radius = radius_mm * scale_est;

    for (idx, &pt) in points.iter().enumerate() {
        let lp = inv.transform_point(pt);
        let prev_lp = if idx > 0 {
            inv.transform_point(points[idx - 1])
        } else {
            lp
        };
        match tool {
            ToolId::Smooth => path.smooth_near(lp, local_radius, strength),
            ToolId::Smear => {
                let delta = if idx > 0 {
                    lp - prev_lp
                } else if points.len() >= 2 {
                    (inv.transform_point(points[1]) - lp) * 0.5
                } else {
                    Vec2D::new(local_radius * 0.25, 0.0)
                };
                path.smear_near(lp, delta, local_radius);
            }
            ToolId::Twirl => {
                let sign = if shift_invert { -1.0 } else { 1.0 };
                let angle = sign * (0.45 * strength + 0.15);
                path.twirl_near(lp, local_radius, angle);
            }
            ToolId::AttractRepel => {
                path.attract_repel_near(lp, local_radius, strength, !shift_invert);
            }
            ToolId::Roughen => {
                let amp = (local_radius * 0.18 * strength).max(0.4);
                path.roughen_near(lp, local_radius, amp, 0x51A7_u64.wrapping_add(idx as u64));
            }
            _ => return ToolOutput::noop(),
        }
    }

    if path == orig_path {
        return ToolOutput::noop();
    }

    let op = build_edit_path_op(ctx.document, oid, path);
    let mut post_actions = Vec::new();
    if !is_object_in_or_descendant_of_selection(ctx.document, &ctx.selection.objects, oid) {
        post_actions.push(ToolActionRequest::SelectObjects {
            ids: vec![oid],
            additive: false,
            toggle: false,
        });
    }
    ToolOutput::Commit {
        ops: vec![op],
        clear_overlay: true,
        post_actions,
    }
}

pub(crate) fn hit_test_canvas_point_opt_tol(
    ctx: &ToolReadContext<'_>,
    world_pt: Point2D,
    tol_mm: f64,
) -> Option<ObjectId> {
    crate::math_helpers::hit_test_canvas_point_in_doc(
        ctx.document,
        ctx.powerclip_edit_id,
        ctx.treat_all_objects_as_filled,
        world_pt,
        tol_mm,
    )
}

pub(crate) fn hit_test_canvas_point_opt(
    ctx: &ToolReadContext<'_>,
    world_pt: Point2D,
) -> Option<ObjectId> {
    let tol = ctx.hit_tol_mm(6.0, 0.8);
    hit_test_canvas_point_opt_tol(ctx, world_pt, tol)
}

pub(crate) fn hit_test_canvas_point_scoped_opt_tol(
    ctx: &ToolReadContext<'_>,
    world_pt: Point2D,
    tol_mm: f64,
) -> Option<ObjectId> {
    if let Some(pc_id) = ctx.powerclip_edit_id {
        crate::math_helpers::hit_test_powerclip_edit_point(
            ctx.document,
            pc_id,
            ctx.treat_all_objects_as_filled,
            world_pt,
            tol_mm,
        )
    } else {
        crate::math_helpers::hit_test_canvas_point_in_doc(
            ctx.document,
            None,
            ctx.treat_all_objects_as_filled,
            world_pt,
            tol_mm,
        )
    }
}

pub(crate) fn hit_test_canvas_point_scoped_opt(
    ctx: &ToolReadContext<'_>,
    world_pt: Point2D,
) -> Option<ObjectId> {
    let tol = ctx.hit_tol_mm(6.0, 0.8);
    hit_test_canvas_point_scoped_opt_tol(ctx, world_pt, tol)
}

pub(crate) fn collect_nodes_in_rect(
    doc: &Document,
    selected_objects: &[ObjectId],
    rect: Rect2D,
) -> Vec<coree_core::NodeRef> {
    let mut nodes = Vec::new();
    for &oid in selected_objects {
        let Some(obj) = doc.object(oid) else {
            continue;
        };
        let NodePayload::Path(ref path) = obj.payload else {
            continue;
        };
        let world_xf = world_transform_of_or_local(doc, oid);
        for (sp_idx, sp) in path.subpaths.iter().enumerate() {
            for (n_idx, node) in sp.nodes.iter().enumerate() {
                let wp = world_xf.transform_point(node.pos);
                if rect.contains_point(wp) {
                    nodes.push(coree_core::NodeRef {
                        object_id: oid,
                        subpath_index: sp_idx,
                        node_index: n_idx,
                    });
                }
            }
        }
    }
    nodes
}
