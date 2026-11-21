// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

/// Returns `true` iff `oid` exists, is unlocked and visible, AND every enclosing container
/// (`Group` / `PowerClip`) and its owning `Layer` are unlocked and visible.
pub(crate) fn is_object_editable_in_doc(doc: &Document, oid: ObjectId) -> bool {
    let mut cur = oid;
    let mut depth = 0usize;
    loop {
        if depth > 64 {
            return false;
        }
        let Some(obj) = doc.objects.get(&cur) else {
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

/// Recursively copies `id`, all `Group` / `PowerClip` descendants, and any referenced
/// `RasterAsset`s from `src_doc` into `dst_doc` (with cycle/depth guard).
pub(crate) fn collect_subtree_into_snapshot(
    src_doc: &Document,
    dst_doc: &mut Document,
    id: ObjectId,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
) {
    if depth > 64 || !visiting.insert(id) {
        return;
    }
    if let Some(obj) = src_doc.objects.get(&id) {
        dst_doc.objects.entry(id).or_insert_with(|| obj.clone());
        match &obj.payload {
            NodePayload::Raster(r) => {
                if let Some(asset) = src_doc.assets.get(&r.asset_id) {
                    dst_doc
                        .assets
                        .entry(r.asset_id)
                        .or_insert_with(|| asset.clone());
                }
            }
            NodePayload::Group(children) => {
                for &cid in children {
                    collect_subtree_into_snapshot(src_doc, dst_doc, cid, depth + 1, visiting);
                }
            }
            NodePayload::PowerClip { contents, .. } => {
                for &cid in contents {
                    collect_subtree_into_snapshot(src_doc, dst_doc, cid, depth + 1, visiting);
                }
            }
            _ => {}
        }
    }
    visiting.remove(&id);
}

/// Recursively builds deterministic, undoable [`CommandOp`]s to clone an object subtree
/// (including `Group` and `PowerClip` hierarchies) with cycle/depth guards.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_duplicate_tree_ops(
    doc: &Document,
    src_id: ObjectId,
    layer_id: Option<LayerId>,
    delta_xf: Affine2D,
    depth: usize,
    next_id: &mut u64,
    visiting: &mut HashSet<ObjectId>,
    id_map: &mut HashMap<ObjectId, ObjectId>,
    ops: &mut Vec<CommandOp>,
) -> Option<ObjectId> {
    if depth > 64 || !visiting.insert(src_id) {
        return None;
    }
    let orig = match doc.objects.get(&src_id) {
        Some(o) => o,
        None => {
            visiting.remove(&src_id);
            return None;
        }
    };

    let result_id = match &orig.payload {
        NodePayload::Group(children) => {
            let mut new_children = Vec::with_capacity(children.len());
            for &cid in children {
                if let Some(nid) = build_duplicate_tree_ops(
                    doc,
                    cid,
                    layer_id,
                    Affine2D::IDENTITY,
                    depth + 1,
                    next_id,
                    visiting,
                    id_map,
                    ops,
                ) {
                    new_children.push(nid);
                }
            }
            if new_children.is_empty() {
                None
            } else {
                let group_id = ObjectId(*next_id);
                *next_id = next_id.saturating_add(1);
                id_map.insert(src_id, group_id);
                ops.push(CommandOp::Group {
                    ids: new_children,
                    name: orig.name.clone(),
                });
                let grp_xf = delta_xf.compose(&orig.transform);
                if grp_xf != Affine2D::IDENTITY {
                    ops.push(CommandOp::TransformObjects {
                        ids: vec![group_id],
                        affine: grp_xf,
                    });
                }
                if orig.lens.is_some() {
                    ops.push(CommandOp::SetLens {
                        ids: vec![group_id],
                        lens: orig.lens.clone(),
                    });
                }
                for eff in &orig.effects {
                    ops.push(CommandOp::AddEffect {
                        id: group_id,
                        effect: eff.clone(),
                    });
                }
                Some(group_id)
            }
        }
        NodePayload::PowerClip {
            container,
            container_parametric,
            lock_contents,
            contents,
        } => {
            let container_id = ObjectId(*next_id);
            *next_id = next_id.saturating_add(1);
            id_map.insert(src_id, container_id);

            let mut container_obj = orig.clone();
            container_obj.id = container_id;
            container_obj.payload = match container_parametric {
                Some(shape) => NodePayload::Shape(shape.clone()),
                None => NodePayload::Path((**container).clone()),
            };
            container_obj.transform = delta_xf.compose(&orig.transform);
            let container_xf = container_obj.transform;
            ops.push(CommandOp::create_object(container_obj, layer_id));

            let mut new_contents = Vec::with_capacity(contents.len());
            for &cid in contents {
                // `create_powerclip` converts child transforms by `inv(container_xf)`, so
                // pre-compose `container_xf` onto each cloned child to preserve exact local coords.
                if let Some(nid) = build_duplicate_tree_ops(
                    doc,
                    cid,
                    layer_id,
                    container_xf,
                    depth + 1,
                    next_id,
                    visiting,
                    id_map,
                    ops,
                ) {
                    new_contents.push(nid);
                }
            }
            ops.push(CommandOp::CreatePowerClip {
                container_id,
                content_ids: new_contents,
            });
            if !*lock_contents {
                ops.push(CommandOp::SetPowerClipLock {
                    id: container_id,
                    lock_contents: false,
                });
            }
            Some(container_id)
        }
        _ => {
            let new_id = ObjectId(*next_id);
            *next_id = next_id.saturating_add(1);
            id_map.insert(src_id, new_id);
            let mut cloned = orig.clone();
            cloned.id = new_id;
            cloned.transform = delta_xf.compose(&cloned.transform);
            ops.push(CommandOp::create_object(cloned, layer_id));
            Some(new_id)
        }
    };

    visiting.remove(&src_id);
    result_id
}

/// Remaps cross-object references (`TextOnPath`, `Connector`, `ParagraphText`, `Effect::Blend`)
/// inside queued duplicate [`CommandOp`]s according to `id_map`.
pub(crate) fn remap_duplicate_ops(ops: &mut [CommandOp], id_map: &HashMap<ObjectId, ObjectId>) {
    if id_map.is_empty() {
        return;
    }
    for op in ops {
        match op {
            CommandOp::CreateObject { object, .. } => {
                match &mut object.payload {
                    NodePayload::TextOnPath(top) => {
                        if let Some(&mapped) = id_map.get(&top.path_id) {
                            top.path_id = mapped;
                        }
                    }
                    NodePayload::Connector(conn) => {
                        if let Some(fid) = conn.from_obj {
                            if let Some(&mapped) = id_map.get(&fid) {
                                conn.from_obj = Some(mapped);
                            }
                        }
                        if let Some(tid) = conn.to_obj {
                            if let Some(&mapped) = id_map.get(&tid) {
                                conn.to_obj = Some(mapped);
                            }
                        }
                    }
                    NodePayload::ParagraphText(para) => {
                        if let Some(next_id) = para.linked_next_frame {
                            if let Some(&mapped) = id_map.get(&next_id) {
                                para.linked_next_frame = Some(mapped);
                            }
                        }
                    }
                    _ => {}
                }
                for eff in &mut object.effects {
                    if let Effect::Blend {
                        target_id,
                        along_path,
                        ..
                    } = eff
                    {
                        if let Some(&mapped) = id_map.get(target_id) {
                            *target_id = mapped;
                        }
                        if let Some(ap) = *along_path {
                            if let Some(&mapped) = id_map.get(&ap) {
                                *along_path = Some(mapped);
                            }
                        }
                    }
                }
            }
            CommandOp::AddEffect {
                effect:
                    Effect::Blend {
                        target_id,
                        along_path,
                        ..
                    },
                ..
            } => {
                if let Some(&mapped) = id_map.get(target_id) {
                    *target_id = mapped;
                }
                if let Some(ap) = *along_path {
                    if let Some(&mapped) = id_map.get(&ap) {
                        *along_path = Some(mapped);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Lightweight fuzzy subsequence scorer for Command Palette search.
pub(crate) fn fuzzy_score(haystack: &str, needle: &str) -> Option<i32> {
    if haystack.contains(needle) {
        let bonus = if haystack.starts_with(needle) {
            100
        } else {
            50
        };
        return Some(bonus - (haystack.len() as i32 - needle.len() as i32).min(40));
    }
    let mut score = 0i32;
    let mut h_chars = haystack.chars().peekable();
    let mut consecutive = 0i32;
    for nc in needle.chars() {
        let mut matched = false;
        for hc in h_chars.by_ref() {
            if hc == nc {
                matched = true;
                consecutive += 1;
                score += 5 + consecutive * 2;
                break;
            }
            consecutive = 0;
        }
        if !matched {
            return None;
        }
    }
    Some(score)
}

/// Ray-casting point-in-polygon check for Freehand Pick lasso selection.
#[must_use]
pub(crate) fn point_in_polygon(pt: Point2D, poly: &[Point2D]) -> bool {
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

/// Converts a sequence of B-Spline / Catmull-Rom control points into a smooth cubic [`BezierPath`].
#[must_use]
pub(crate) fn bspline_to_bezier_path(pts: &[Point2D], closed: bool) -> BezierPath {
    if pts.len() < 2 {
        return BezierPath::new();
    }
    if pts.len() == 2 {
        return BezierPath::from_line(pts[0], pts[1]);
    }
    let n = pts.len();
    let mut nodes = Vec::with_capacity(n);
    for i in 0..n {
        let p_prev = if i == 0 {
            if closed {
                pts[n - 1]
            } else {
                pts[0]
            }
        } else {
            pts[i - 1]
        };
        let p_curr = pts[i];
        let p_next = if i + 1 == n {
            if closed {
                pts[0]
            } else {
                pts[n - 1]
            }
        } else {
            pts[i + 1]
        };
        // Smooth Catmull-Rom / cubic spline tangent
        let tang = (p_next - p_prev) * (1.0 / 6.0);
        let h_in = if !closed && i == 0 {
            None
        } else {
            Some(p_curr - tang)
        };
        let h_out = if !closed && i + 1 == n {
            None
        } else {
            Some(p_curr + tang)
        };
        nodes.push(PathNode::new(p_curr, h_in, h_out, NodeKind::Smooth));
    }
    BezierPath::from_subpath(Subpath { nodes, closed })
}

/// Builds a 3-Point Curve arc from `p1` to `p2` passing cleanly through `p3` at `t = 0.5`
/// by computing the quadratic control point `q = 2*p3 - 0.5*(p1 + p2)` and elevating exactly to cubic.
#[must_use]
pub(crate) fn three_point_arc_path(p1: Point2D, p2: Point2D, p3: Point2D) -> BezierPath {
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
pub(crate) fn compute_free_transform_affine(
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

fn finite_egui_rect(r: egui::Rect) -> egui::Rect {
    if r.is_finite() {
        r
    } else {
        egui::Rect::ZERO
    }
}

/// Replaces `egui_dock`'s initial `Rect::NOTHING` (`±inf` sentinel coordinates, which
/// `serde_json` emits as `null` and fails to deserialize) with finite `Rect::ZERO` before
/// writing JSON to SQLite or `.coreews` bundles.
pub(crate) fn sanitize_dock_state_for_serde(state: &DockState<DockerTab>) -> DockState<DockerTab> {
    let mut cloned = state.clone();
    for surface in cloned.iter_surfaces_mut() {
        for node in surface.iter_nodes_mut() {
            match node {
                egui_dock::Node::Leaf(leaf) => {
                    leaf.rect = finite_egui_rect(leaf.rect);
                    leaf.viewport = finite_egui_rect(leaf.viewport);
                    if !leaf.scroll.is_finite() {
                        leaf.scroll = 0.0;
                    }
                }
                egui_dock::Node::Vertical(sp) | egui_dock::Node::Horizontal(sp) => {
                    sp.rect = finite_egui_rect(sp.rect);
                    if !sp.fraction.is_finite() {
                        sp.fraction = 0.5;
                    }
                }
                egui_dock::Node::Empty => {}
            }
        }
    }
    cloned
}
