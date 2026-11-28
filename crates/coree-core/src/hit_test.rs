// SPDX-License-Identifier: MIT OR Apache-2.0

//! Spatial R-Tree accelerated hit-testing (`hit_test_point`, `hit_test_rect`,
//! `hit_test_node`) and intelligent multi-target [`SnapEngine`] emitting
//! [`DynamicGuideLine`] alignment overlays.

use crate::geometry::{
    closed_subpaths_contain_even_odd, point_to_segment_distance, ArtisticMedia, NodeKind,
    ParametricShape, PathNode, Subpath,
};
use crate::math::{sanitize_f64, Perspective3x3};
use crate::{
    Affine2D, ArtisticText, BezierPath, DimensionKind, DimensionObject, Document, Effect,
    GuideOrientation, NodePayload, NodeRef, ObjectId, ParagraphText, Point2D, Rect2D, SceneObject,
    SymbolId, TextAlign, TextDirection, Vec2D,
};
use std::collections::{HashMap, HashSet};

/// Hit result identifying a specific Bézier anchor node on a scene object.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeHit {
    pub node: NodeRef,
    pub position_mm: Point2D,
    pub distance_mm: f64,
}

/// Returns the topmost visible, unlocked scene object hit at `pt_mm` within `tolerance_mm`
/// on `page_index`, accelerated by `crate::SpatialIndex` R-Tree culling.
///
/// Objects residing on locked or hidden layers are skipped.
#[must_use]
pub fn hit_test_point(
    doc: &Document,
    page_index: usize,
    pt_mm: Point2D,
    tolerance_mm: f64,
) -> Option<ObjectId> {
    hit_test_point_exact(doc, page_index, pt_mm, tolerance_mm, false)
}

/// Non-AABB exact point hit-test with configurable `treat_all_filled` behavior (for Coree
/// "Treat all objects as filled" Pick mode without falling back to coarse AABB boxes).
#[must_use]
pub fn hit_test_point_exact(
    doc: &Document,
    page_index: usize,
    pt_mm: Point2D,
    tolerance_mm: f64,
    treat_all_filled: bool,
) -> Option<ObjectId> {
    if !pt_mm.is_finite() || page_index >= doc.pages().len() {
        return None;
    }
    let tol = sanitize_f64(tolerance_mm, 1.0).abs();
    let blocked = locked_or_hidden_layer_objects(doc, page_index);
    let index = doc.cached_spatial_index(page_index);
    // Candidates are returned frontmost-first
    for oid in index.query_point(pt_mm, tol) {
        if blocked.contains(&oid) {
            continue;
        }
        let Some(obj) = doc.object(oid) else {
            continue;
        };
        if !obj.visible || obj.locked {
            continue;
        }
        if object_hits_point_impl(
            doc,
            oid,
            &Affine2D::IDENTITY,
            None,
            pt_mm,
            tol,
            treat_all_filled,
            0,
        ) {
            return Some(oid);
        }
    }
    None
}

/// Hit-tests the internal `contents` of an active `PowerClip` container `pc_id` at `pt_mm`
/// (in page/world mm), accounting for the container's world transform.
#[must_use]
pub fn hit_test_powerclip_contents(
    doc: &Document,
    pc_id: ObjectId,
    pt_mm: Point2D,
    tolerance_mm: f64,
) -> Option<ObjectId> {
    hit_test_powerclip_contents_exact(doc, pc_id, pt_mm, tolerance_mm, false)
}

/// Non-AABB exact `PowerClip` inner-contents hit-test with configurable `treat_all_filled`.
#[must_use]
pub fn hit_test_powerclip_contents_exact(
    doc: &Document,
    pc_id: ObjectId,
    pt_mm: Point2D,
    tolerance_mm: f64,
    treat_all_filled: bool,
) -> Option<ObjectId> {
    if !pt_mm.is_finite() {
        return None;
    }
    let tol = sanitize_f64(tolerance_mm, 1.0).abs();
    let pc_obj = doc.object(pc_id)?;
    let crate::NodePayload::PowerClip { contents, .. } = &pc_obj.payload else {
        return None;
    };
    let pc_xf = doc.world_transform_of(pc_id)?;
    let pc_persp = powerclip_world_persp(doc, pc_id);
    for &cid in contents.iter().rev() {
        let Some(child) = doc.object(cid) else {
            continue;
        };
        if !child.visible || child.locked {
            continue;
        }
        if object_hits_point_impl(doc, cid, &pc_xf, pc_persp, pt_mm, tol, treat_all_filled, 0) {
            return Some(cid);
        }
    }
    None
}

/// Returns all visible, unlocked child objects inside `PowerClip` container `pc_id`
/// intersecting (or fully enclosed by) `rect_mm` (in page/world mm), ordered frontmost-first.
#[must_use]
pub fn hit_test_powerclip_contents_rect(
    doc: &Document,
    pc_id: ObjectId,
    rect_mm: Rect2D,
    fully_enclosed: bool,
) -> Vec<ObjectId> {
    if rect_mm.is_empty() {
        return Vec::new();
    }
    let Some(pc_obj) = doc.object(pc_id) else {
        return Vec::new();
    };
    let NodePayload::PowerClip { contents, .. } = &pc_obj.payload else {
        return Vec::new();
    };
    let Some(pc_xf) = doc.world_transform_of(pc_id) else {
        return Vec::new();
    };
    let pc_persp = powerclip_world_persp(doc, pc_id);
    let mut hits = Vec::new();
    for &cid in contents.iter().rev() {
        let Some(child) = doc.object(cid) else {
            continue;
        };
        if !child.visible || child.locked {
            continue;
        }
        if fully_enclosed {
            let mut visited_syms = HashSet::new();
            if let Some(bb) =
                object_warped_world_bounds(doc, child, &pc_xf, pc_persp, None, &mut visited_syms, 0)
            {
                if rect_mm.contains_rect(bb) {
                    hits.push(cid);
                }
            }
        } else {
            let mut visited_syms = HashSet::new();
            if object_intersects_rect(
                doc,
                child,
                &pc_xf,
                pc_persp,
                rect_mm,
                None,
                &mut visited_syms,
                0,
            ) {
                hits.push(cid);
            }
        }
    }
    hits
}

/// Returns all visible, unlocked objects on `page_index` intersecting (or fully enclosed by)
/// `rect_mm`, ordered frontmost-first, using the R-Tree spatial index.
///
/// Objects residing on locked or hidden layers are skipped.
#[must_use]
pub fn hit_test_rect(
    doc: &Document,
    page_index: usize,
    rect_mm: Rect2D,
    fully_enclosed: bool,
) -> Vec<ObjectId> {
    if rect_mm.is_empty() || page_index >= doc.pages().len() {
        return Vec::new();
    }
    let blocked = locked_or_hidden_layer_objects(doc, page_index);
    let index = doc.cached_spatial_index(page_index);
    let mut hits = Vec::new();
    for oid in index.query_rect(rect_mm) {
        if blocked.contains(&oid) {
            continue;
        }
        let Some(obj) = doc.object(oid) else {
            continue;
        };
        if !obj.visible || obj.locked {
            continue;
        }
        if fully_enclosed {
            let mut visited_syms = HashSet::new();
            if let Some(bb) = object_warped_world_bounds(
                doc,
                obj,
                &Affine2D::IDENTITY,
                None,
                None,
                &mut visited_syms,
                0,
            ) {
                if rect_mm.contains_rect(bb) {
                    hits.push(oid);
                }
            }
        } else {
            let mut visited_syms = HashSet::new();
            if object_intersects_rect(
                doc,
                obj,
                &Affine2D::IDENTITY,
                None,
                rect_mm,
                None,
                &mut visited_syms,
                0,
            ) {
                hits.push(oid);
            }
        }
    }
    hits
}

/// Collects all top-level object IDs on locked or invisible layers (including master layers)
/// applicable to `page_index`.
fn locked_or_hidden_layer_objects(doc: &Document, page_index: usize) -> HashSet<ObjectId> {
    let mut blocked = HashSet::new();
    for (p_idx, page) in doc.pages().iter().enumerate() {
        for layer in &page.layers {
            if Document::layer_applies_to_page(layer, p_idx, page_index)
                && (layer.locked || !layer.visible)
            {
                blocked.extend(layer.objects.iter().copied());
            }
        }
    }
    blocked
}

/// Finds the closest Bézier anchor node or B-Spline control point on any object in `object_ids`
/// within `tolerance_mm` of `pt_mm`.
#[must_use]
pub fn hit_test_node(
    doc: &Document,
    object_ids: &[ObjectId],
    pt_mm: Point2D,
    tolerance_mm: f64,
) -> Option<NodeHit> {
    if !pt_mm.is_finite() {
        return None;
    }
    let tol = sanitize_f64(tolerance_mm, 2.0).abs();
    let mut best: Option<NodeHit> = None;

    for &oid in object_ids {
        if let Some(obj) = doc.object(oid) {
            if let NodePayload::BSpline(bs) = &obj.payload {
                let world_xf = doc.world_transform_of(oid).unwrap_or(obj.transform);
                for (node_idx, &cp) in bs.control_points.iter().enumerate() {
                    let wp = world_xf.transform_point(cp);
                    let d = wp.distance_to(pt_mm);
                    if d <= tol && best.is_none_or(|b| d < b.distance_mm) {
                        best = Some(NodeHit {
                            node: NodeRef {
                                object_id: oid,
                                subpath_index: 0,
                                node_index: node_idx,
                            },
                            position_mm: wp,
                            distance_mm: d,
                        });
                    }
                }
                continue;
            }
            // Zero-allocation fast path for affine-only `NodePayload::Path` (avoids cloning
            // 40,000+ nodes on every pointer move when `Snap to Nodes` or `F10` is active).
            let has_persp_or_warp = obj.perspective.is_some()
                || obj.effects.iter().any(|e| {
                    matches!(
                        e,
                        Effect::Perspective { .. }
                            | Effect::Envelope { .. }
                            | Effect::Distort { .. }
                    )
                });
            if !has_persp_or_warp {
                if let NodePayload::Path(local_path) = &obj.payload {
                    let world_xf = doc.world_transform_of(oid).unwrap_or(obj.transform);
                    if let Some(local_bb) = local_path.bounding_box() {
                        if let Some(wb) = world_xf.try_transform_rect(local_bb) {
                            let ex = wb.expand(tol);
                            if pt_mm.x < ex.min_x
                                || pt_mm.x > ex.max_x
                                || pt_mm.y < ex.min_y
                                || pt_mm.y > ex.max_y
                            {
                                continue;
                            }
                        }
                    }
                    for (sp_idx, sp) in local_path.subpaths.iter().enumerate() {
                        for (node_idx, node) in sp.nodes.iter().enumerate() {
                            let wp = world_xf.transform_point(node.pos);
                            let d = wp.distance_to(pt_mm);
                            if d <= tol && best.is_none_or(|b| d < b.distance_mm) {
                                best = Some(NodeHit {
                                    node: NodeRef {
                                        object_id: oid,
                                        subpath_index: sp_idx,
                                        node_index: node_idx,
                                    },
                                    position_mm: wp,
                                    distance_mm: d,
                                });
                            }
                        }
                    }
                    continue;
                }
            }
        }
        let Some(world_path) = doc.world_path_of(oid) else {
            continue;
        };
        for (sp_idx, sp) in world_path.subpaths.iter().enumerate() {
            for (node_idx, node) in sp.nodes.iter().enumerate() {
                let d = node.pos.distance_to(pt_mm);
                if d <= tol && best.is_none_or(|b| d < b.distance_mm) {
                    best = Some(NodeHit {
                        node: NodeRef {
                            object_id: oid,
                            subpath_index: sp_idx,
                            node_index: node_idx,
                        },
                        position_mm: node.pos,
                        distance_mm: d,
                    });
                }
            }
        }
    }
    best
}

#[allow(clippy::too_many_arguments)]
fn object_hits_point_impl(
    doc: &Document,
    id: ObjectId,
    parent_xf: &Affine2D,
    parent_persp: Option<Perspective3x3>,
    pt_mm: Point2D,
    tol_mm: f64,
    treat_all_filled: bool,
    depth: usize,
) -> bool {
    let Some(obj) = doc.object(id) else {
        return false;
    };
    let mut visited_syms = HashSet::new();
    object_ref_hits_point(
        doc,
        obj,
        parent_xf,
        parent_persp,
        pt_mm,
        tol_mm,
        treat_all_filled,
        None,
        &mut visited_syms,
        depth,
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum HitSymParentRef {
    Id(ObjectId),
    RootIndex(usize),
}

fn find_sym_objects_for_map<'a>(
    doc: &'a Document,
    id: ObjectId,
    sym_map: &HashMap<ObjectId, &SceneObject>,
) -> Option<&'a [SceneObject]> {
    let target_ptr = sym_map.get(&id).copied()?;
    doc.symbols().values().find_map(|sym| {
        sym.objects
            .iter()
            .any(|o| std::ptr::eq(o, target_ptr))
            .then_some(sym.objects.as_slice())
    })
}

fn sym_object_world_xf(
    doc: &Document,
    id: ObjectId,
    sym_root_xf: &Affine2D,
    sym_map: &HashMap<ObjectId, &SceneObject>,
    sym_objects: Option<&[SceneObject]>,
) -> Option<Affine2D> {
    let eff_objs = sym_objects.or_else(|| find_sym_objects_for_map(doc, id, sym_map));
    let mut chain = Vec::new();
    let mut cur = HitSymParentRef::Id(id);
    let mut seen = HashSet::new();
    while seen.insert(cur) {
        let obj = match cur {
            HitSymParentRef::Id(oid) => sym_map.get(&oid).copied()?,
            HitSymParentRef::RootIndex(idx) => eff_objs.and_then(|objs| objs.get(idx))?,
        };
        chain.push(obj.transform);
        match cur {
            HitSymParentRef::Id(oid) => {
                let parent = if let Some(objs) = eff_objs {
                    objs.iter().enumerate().find_map(|(idx, po)| {
                        let contains = match &po.payload {
                            NodePayload::Group(c) => c.contains(&oid),
                            NodePayload::PowerClip { contents, .. } => contents.contains(&oid),
                            _ => false,
                        };
                        contains.then_some(if po.id.0 != 0 {
                            HitSymParentRef::Id(po.id)
                        } else {
                            HitSymParentRef::RootIndex(idx)
                        })
                    })
                } else {
                    sym_map.iter().find_map(|(&pid, po)| match &po.payload {
                        NodePayload::Group(c) if c.contains(&oid) => Some(HitSymParentRef::Id(pid)),
                        NodePayload::PowerClip { contents, .. } if contents.contains(&oid) => {
                            Some(HitSymParentRef::Id(pid))
                        }
                        _ => None,
                    })
                };
                match parent {
                    Some(pid) => cur = pid,
                    None => break,
                }
            }
            HitSymParentRef::RootIndex(_) => break,
        }
    }
    let mut xf = *sym_root_xf;
    for local in chain.into_iter().rev() {
        xf = xf.compose(&local);
    }
    xf.is_finite().then_some(xf)
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn sym_sub_world_bounds(
    doc: &Document,
    id: ObjectId,
    sym_map: &HashMap<ObjectId, &SceneObject>,
    sym_root_xf: Affine2D,
    visiting: &mut HashMap<ObjectId, usize>,
    visited_syms: &mut HashSet<SymbolId>,
    cache: &mut HashMap<ObjectId, Option<Rect2D>>,
    depth: usize,
) -> (Option<Rect2D>, usize) {
    sym_sub_world_bounds_with_objs(
        doc,
        id,
        sym_map,
        None,
        sym_root_xf,
        visiting,
        visited_syms,
        cache,
        depth,
    )
}

#[allow(clippy::too_many_arguments)]
fn sym_sub_world_bounds_with_objs(
    doc: &Document,
    id: ObjectId,
    sym_map: &HashMap<ObjectId, &SceneObject>,
    sym_objects: Option<&[SceneObject]>,
    sym_root_xf: Affine2D,
    visiting: &mut HashMap<ObjectId, usize>,
    visited_syms: &mut HashSet<SymbolId>,
    cache: &mut HashMap<ObjectId, Option<Rect2D>>,
    depth: usize,
) -> (Option<Rect2D>, usize) {
    if depth > 64 {
        return (None, 0);
    }
    if let Some(&active_depth) = visiting.get(&id) {
        return (None, active_depth);
    }
    if let Some(&cached) = cache.get(&id) {
        return (cached, usize::MAX);
    }
    let my_depth = visiting.len();
    visiting.insert(id, my_depth);
    let mut min_backedge = usize::MAX;
    let res = sym_map.get(&id).copied().and_then(|sub| {
        let p_xf = sym_object_world_xf(doc, id, &sym_root_xf, sym_map, sym_objects)?;
        let (b, backedge) = sym_obj_bounds_with_objs(
            doc,
            sub,
            p_xf,
            sym_map,
            sym_objects,
            sym_root_xf,
            visiting,
            visited_syms,
            cache,
            depth,
        );
        min_backedge = min_backedge.min(backedge);
        b
    });
    let filtered = res.filter(|r| r.is_finite());
    visiting.remove(&id);
    if min_backedge == usize::MAX {
        cache.insert(id, filtered);
        (filtered, usize::MAX)
    } else if min_backedge >= my_depth {
        (filtered, usize::MAX)
    } else {
        (filtered, min_backedge)
    }
}

#[allow(clippy::too_many_arguments)]
fn sym_obj_bounds_with_objs(
    doc: &Document,
    sub: &SceneObject,
    p_xf: Affine2D,
    sym_map: &HashMap<ObjectId, &SceneObject>,
    sym_objects: Option<&[SceneObject]>,
    sym_root_xf: Affine2D,
    visiting: &mut HashMap<ObjectId, usize>,
    visited_syms: &mut HashSet<SymbolId>,
    cache: &mut HashMap<ObjectId, Option<Rect2D>>,
    depth: usize,
) -> (Option<Rect2D>, usize) {
    if depth > 64 {
        return (None, 0);
    }
    if !p_xf.is_finite() {
        return (None, usize::MAX);
    }
    let mut min_backedge = usize::MAX;
    let apply_sub_persp = |bp: BezierPath| apply_obj_perspective_to_local_path(bp, sub);
    let rect_with_sub_persp = |r: Rect2D| {
        let mut warped = if let Some(persp) = sub.perspective {
            persp.try_transform_rect(r)?
        } else {
            r
        };
        for eff in &sub.effects {
            if let Effect::Perspective { quad } = eff {
                let h = Perspective3x3::from_rect_to_quad(warped, quad)?;
                warped = h.try_transform_rect(warped)?;
            }
        }
        p_xf.try_transform_rect(warped)
    };
    let res = match &sub.payload {
        NodePayload::Path(p) => apply_sub_persp(p.clone()).transformed(&p_xf).bounding_box(),
        NodePayload::Shape(s) => apply_sub_persp(s.to_bezier_path())
            .transformed(&p_xf)
            .bounding_box(),
        NodePayload::BSpline(bs) => apply_sub_persp(bs.to_bezier_path())
            .transformed(&p_xf)
            .bounding_box(),
        NodePayload::ArtisticMedia { spine, style } => {
            apply_sub_persp(ArtisticMedia::expand_stroke(spine, style))
                .transformed(&p_xf)
                .bounding_box()
        }
        NodePayload::Clone { .. } => {
            if let Some((eff_p, eff_fill, eff_outline, in_sym)) =
                doc.resolve_clone_payload_and_style_with_sym_scope(sub, Some(sym_map))
            {
                let mut tmp = sub.clone();
                tmp.payload = eff_p;
                tmp.fill = eff_fill;
                tmp.outline = eff_outline;
                if in_sym {
                    let (b, back) = sym_obj_bounds_with_objs(
                        doc,
                        &tmp,
                        p_xf,
                        sym_map,
                        sym_objects,
                        sym_root_xf,
                        visiting,
                        visited_syms,
                        cache,
                        depth + 1,
                    );
                    min_backedge = min_backedge.min(back);
                    b
                } else {
                    tmp.transform = Affine2D::IDENTITY;
                    object_warped_world_bounds(
                        doc,
                        &tmp,
                        &p_xf,
                        None,
                        None,
                        visited_syms,
                        depth + 1,
                    )
                }
            } else {
                None
            }
        }
        NodePayload::PowerClip { container, .. } => apply_sub_persp((**container).clone())
            .transformed(&p_xf)
            .bounding_box(),
        NodePayload::ArtisticText(t) => rect_with_sub_persp(t.estimated_bounds()),
        NodePayload::ParagraphText(t) => rect_with_sub_persp(t.bounding_box()),
        NodePayload::Table(t) => rect_with_sub_persp(t.to_bounding_box()),
        NodePayload::Dimension(d) => rect_with_sub_persp(d.bounding_box()),
        NodePayload::Raster(r) => rect_with_sub_persp(raster_local_rect(r)),
        NodePayload::TextOnPath(top) => resolve_sym_world_path_with_objs(
            doc,
            top.path_id,
            Some((sym_map, sym_objects, sym_root_xf)),
        )
        .and_then(|wp| {
            if sub.perspective.is_some()
                || sub
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::Perspective { .. }))
            {
                let sub_parent_xf = sub
                    .transform
                    .inverse()
                    .map_or(p_xf, |inv| p_xf.compose(&inv));
                let inv_parent = sub_parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                let warped = apply_obj_perspective_to_local_path(wp.transformed(&inv_parent), sub);
                warped.transformed(&sub_parent_xf).bounding_box()
            } else {
                wp.bounding_box()
            }
        })
        .or_else(|| rect_with_sub_persp(top.text.estimated_bounds())),
        NodePayload::Group(children) => {
            let mut acc: Option<Rect2D> = None;
            for &cid in children {
                let (cb, backedge) = sym_sub_world_bounds_with_objs(
                    doc,
                    cid,
                    sym_map,
                    sym_objects,
                    sym_root_xf,
                    visiting,
                    visited_syms,
                    cache,
                    depth + 1,
                );
                min_backedge = min_backedge.min(backedge);
                if let Some(cb) = cb {
                    acc = Some(match acc {
                        Some(prev) => prev.union(cb),
                        None => cb,
                    });
                }
            }
            if sub.perspective.is_some()
                || sub
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::Perspective { .. }))
            {
                let mut local =
                    acc.and_then(|r| p_xf.inverse().and_then(|inv| inv.try_transform_rect(r)));
                if let Some(persp) = sub.perspective {
                    local = local.and_then(|lb| persp.try_transform_rect(lb));
                }
                for eff in &sub.effects {
                    if let Effect::Perspective { quad } = eff {
                        local = local.and_then(|lb| {
                            Perspective3x3::from_rect_to_quad(lb, quad)
                                .and_then(|h| h.try_transform_rect(lb))
                        });
                    }
                }
                local.and_then(|wb| p_xf.try_transform_rect(wb))
            } else {
                acc
            }
        }
        NodePayload::Connector(c) => {
            let ((a, b), backedge) = resolve_connector_endpoints_with_sym_objs(
                doc,
                c,
                &p_xf,
                Some((sym_map, sym_objects, sym_root_xf)),
                visiting,
                visited_syms,
                cache,
                depth + 1,
            );
            min_backedge = min_backedge.min(backedge);
            c.route_path(a, b).bounding_box()
        }
        NodePayload::SymbolInstance(sym_id) => {
            if !visited_syms.insert(*sym_id) {
                return (None, 0);
            }
            let inner_res = doc.symbol(*sym_id).and_then(|sym| {
                let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut referenced: HashSet<ObjectId> = HashSet::new();
                for inner in &sym.objects {
                    if inner.id.0 != 0 {
                        child_map.insert(inner.id, inner);
                    }
                    match &inner.payload {
                        NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                        NodePayload::PowerClip { contents, .. } => {
                            referenced.extend(contents.iter().copied());
                        }
                        _ => {}
                    }
                }
                let mut inner_visiting = HashMap::new();
                let mut inner_cache = HashMap::new();
                let mut acc: Option<Rect2D> = None;
                for inner in &sym.objects {
                    let (ib, backedge) = if inner.id.0 == 0 {
                        let inner_xf = p_xf.compose(&inner.transform);
                        sym_obj_bounds_with_objs(
                            doc,
                            inner,
                            inner_xf,
                            &child_map,
                            Some(&sym.objects),
                            p_xf,
                            &mut inner_visiting,
                            visited_syms,
                            &mut inner_cache,
                            depth + 1,
                        )
                    } else if !referenced.contains(&inner.id) {
                        sym_sub_world_bounds_with_objs(
                            doc,
                            inner.id,
                            &child_map,
                            Some(&sym.objects),
                            p_xf,
                            &mut inner_visiting,
                            visited_syms,
                            &mut inner_cache,
                            depth + 1,
                        )
                    } else {
                        continue;
                    };
                    if backedge == 0 {
                        min_backedge = 0;
                    }
                    if let Some(ib) = ib {
                        acc = Some(match acc {
                            Some(prev) => prev.union(ib),
                            None => ib,
                        });
                    }
                }
                acc
            });
            visited_syms.remove(sym_id);
            if sub.perspective.is_some()
                || sub
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::Perspective { .. }))
            {
                let mut local = inner_res
                    .and_then(|r| p_xf.inverse().and_then(|inv| inv.try_transform_rect(r)));
                if let Some(persp) = sub.perspective {
                    local = local.and_then(|lb| persp.try_transform_rect(lb));
                }
                for eff in &sub.effects {
                    if let Effect::Perspective { quad } = eff {
                        local = local.and_then(|lb| {
                            Perspective3x3::from_rect_to_quad(lb, quad)
                                .and_then(|h| h.try_transform_rect(lb))
                        });
                    }
                }
                local.and_then(|wb| p_xf.try_transform_rect(wb))
            } else {
                inner_res
            }
        }
    };
    (res.filter(|r| r.is_finite()), min_backedge)
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn resolve_connector_endpoints_with_sym(
    doc: &Document,
    conn: &crate::ConnectorObject,
    base_world_xf: &Affine2D,
    sym_ctx: Option<(&HashMap<ObjectId, &SceneObject>, Affine2D)>,
    visiting: &mut HashMap<ObjectId, usize>,
    visited_syms: &mut HashSet<SymbolId>,
    cache: &mut HashMap<ObjectId, Option<Rect2D>>,
    depth: usize,
) -> ((Point2D, Point2D), usize) {
    resolve_connector_endpoints_with_sym_objs(
        doc,
        conn,
        base_world_xf,
        sym_ctx.map(|(m, xf)| (m, None, xf)),
        visiting,
        visited_syms,
        cache,
        depth,
    )
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn resolve_connector_endpoints_with_sym_objs(
    doc: &Document,
    conn: &crate::ConnectorObject,
    base_world_xf: &Affine2D,
    sym_ctx: Option<HitSymCtx<'_>>,
    visiting: &mut HashMap<ObjectId, usize>,
    visited_syms: &mut HashSet<SymbolId>,
    cache: &mut HashMap<ObjectId, Option<Rect2D>>,
    depth: usize,
) -> ((Point2D, Point2D), usize) {
    let mut min_backedge = usize::MAX;
    let mut resolve_ep =
        |obj_id: Option<ObjectId>, anchor: &crate::AnchorPoint, fallback: Point2D| {
            if let Some(oid) = obj_id {
                if let Some((sym_map, sym_objects, sym_root_xf)) = sym_ctx {
                    if sym_map.contains_key(&oid) {
                        let (bb, backedge) = sym_sub_world_bounds_with_objs(
                            doc,
                            oid,
                            sym_map,
                            sym_objects,
                            sym_root_xf,
                            visiting,
                            visited_syms,
                            cache,
                            depth,
                        );
                        min_backedge = min_backedge.min(backedge);
                        if let Some(bb) = bb {
                            return anchor.resolve(bb);
                        }
                    }
                }
                if let Some(bb) = doc.world_object_bounds(oid) {
                    return anchor.resolve(bb);
                }
            }
            base_world_xf.transform_point(fallback)
        };
    let a = resolve_ep(conn.from_obj, &conn.from_anchor, conn.from_fallback);
    let b = resolve_ep(conn.to_obj, &conn.to_anchor, conn.to_fallback);
    ((a, b), min_backedge)
}

fn resolve_sym_world_path_with_objs(
    doc: &Document,
    path_id: ObjectId,
    sym_ctx: Option<HitSymCtx<'_>>,
) -> Option<BezierPath> {
    if let Some((sym_map, sym_objects, sym_root_xf)) = sym_ctx {
        if let Some(sub) = sym_map.get(&path_id).copied() {
            let p_xf = sym_object_world_xf(doc, path_id, &sym_root_xf, sym_map, sym_objects)?;
            let local = match &sub.payload {
                NodePayload::Path(p) => Some(p.clone()),
                NodePayload::Shape(s) => Some(s.to_bezier_path()),
                NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
                NodePayload::ArtisticMedia { spine, style } => {
                    Some(ArtisticMedia::expand_stroke(spine, style))
                }
                NodePayload::Clone { .. } => doc
                    .resolve_clone_payload_and_style_with_sym(sub, Some(sym_map))
                    .and_then(|(eff_p, _, _)| match eff_p {
                        NodePayload::Path(p) => Some(p),
                        NodePayload::Shape(s) => Some(s.to_bezier_path()),
                        NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
                        NodePayload::ArtisticMedia { spine, style } => {
                            Some(ArtisticMedia::expand_stroke(&spine, &style))
                        }
                        NodePayload::PowerClip { container, .. } => Some(*container),
                        _ => None,
                    }),
                NodePayload::PowerClip { container, .. } => Some((**container).clone()),
                _ => None,
            }?;
            let warped = apply_obj_perspective_to_local_path(local, sub);
            return Some(warped.transformed(&p_xf));
        }
    }
    doc.world_path_of(path_id)
}

fn dimension_local_geometry(
    dim: &DimensionObject,
    doc: &Document,
) -> (Vec<BezierPath>, Option<Rect2D>) {
    let mut strokes = Vec::new();
    let label_text = dim.compute_measurement_with_dpi(doc.default_unit(), f64::from(doc.dpi()));
    let label_anchor = match dim.kind {
        DimensionKind::Parallel
        | DimensionKind::Horizontal
        | DimensionKind::Vertical
        | DimensionKind::Segment => {
            let (d1, d2, ext_dir) = match dim.kind {
                DimensionKind::Horizontal => {
                    let y_line = dim.p1.y.min(dim.p2.y) - dim.offset_mm;
                    (
                        Point2D::new(dim.p1.x, y_line),
                        Point2D::new(dim.p2.x, y_line),
                        Vec2D::new(0.0, -1.0),
                    )
                }
                DimensionKind::Vertical => {
                    let x_line = dim.p1.x.min(dim.p2.x) - dim.offset_mm;
                    (
                        Point2D::new(x_line, dim.p1.y),
                        Point2D::new(x_line, dim.p2.y),
                        Vec2D::new(-1.0, 0.0),
                    )
                }
                _ => {
                    let dir = (dim.p2 - dim.p1)
                        .normalized()
                        .unwrap_or(Vec2D::new(1.0, 0.0));
                    let perp = Vec2D::new(dir.y, -dir.x);
                    let off = perp * dim.offset_mm;
                    (dim.p1 + off, dim.p2 + off, perp)
                }
            };
            let overshoot = ext_dir * 1.5;
            strokes.push(BezierPath::from_line(dim.p1, d1 + overshoot));
            strokes.push(BezierPath::from_line(dim.p2, d2 + overshoot));
            strokes.push(BezierPath::from_line(d1, d2));
            d1.midpoint(d2) + ext_dir * 1.2
        }
        DimensionKind::Angular => {
            let v = dim.p1;
            let r1 = (dim.p2 - v).normalized().unwrap_or(Vec2D::new(1.0, 0.0));
            let r2 = dim
                .p3
                .map_or(Vec2D::new(1.0, 0.0), |p| (p - v).normalize_or_zero());
            let rad = dim.offset_mm.abs().max(5.0);
            strokes.push(BezierPath::from_line(v, v + r1 * (rad + 2.0)));
            strokes.push(BezierPath::from_line(v, v + r2 * (rad + 2.0)));
            let a1 = r1.angle_rad();
            let mut a2 = r2.angle_rad();
            let mut sweep = a2 - a1;
            while sweep > std::f64::consts::PI {
                sweep -= std::f64::consts::TAU;
            }
            while sweep < -std::f64::consts::PI {
                sweep += std::f64::consts::TAU;
            }
            a2 = a1 + sweep;
            let steps = 24;
            let arc_pts: Vec<Point2D> = (0..=steps)
                .map(|i| {
                    let a = a1 + (a2 - a1) * (i as f64 / steps as f64);
                    v + Vec2D::from_angle(a) * rad
                })
                .collect();
            strokes.push(BezierPath::from_polygon(arc_pts, false));
            let mid_a = 0.5 * (a1 + a2);
            v + Vec2D::from_angle(mid_a) * (rad + 3.0)
        }
        DimensionKind::Callout3Pt => {
            let mut pts = vec![dim.p1, dim.p2];
            let anchor = if let Some(p3) = dim.p3 {
                pts.push(p3);
                p3
            } else {
                dim.p2
            };
            strokes.push(BezierPath::from_polygon(pts, false));
            anchor
        }
    };

    let label_rect = if !label_text.trim().is_empty() {
        let at = ArtisticText::from_plain(label_text, dim.text_style.clone());
        let bb = at.estimated_bounds();
        Some(Rect2D::from_xywh(
            label_anchor.x + bb.min_x,
            label_anchor.y - dim.text_style.font_size_mm() + bb.min_y,
            bb.width(),
            bb.height(),
        ))
    } else {
        None
    };
    (strokes, label_rect)
}

fn raster_local_rect(r: &crate::RasterLayerData) -> Rect2D {
    let w_mm = sanitize_f64(r.width_mm, 50.0).max(0.1);
    let h_mm = sanitize_f64(r.height_mm, 50.0).max(0.1);
    let full_rect = Rect2D::from_xywh(0.0, 0.0, w_mm, h_mm);
    r.crop_rect
        .filter(|cr| cr.is_finite())
        .and_then(|cr| cr.intersection(full_rect))
        .unwrap_or(full_rect)
}

fn apply_obj_perspective_to_local_path(local: BezierPath, obj: &SceneObject) -> BezierPath {
    let mut cur = if let Some(persp) = obj.perspective {
        persp.transform_bezier_path(&local)
    } else {
        local
    };
    for eff in &obj.effects {
        if let Effect::Perspective { quad } = eff {
            if let Some(bb) = cur.bounding_box() {
                if let Some(h) = Perspective3x3::from_rect_to_quad(bb, quad) {
                    cur = h.transform_bezier_path(&cur);
                }
            }
        }
    }
    cur
}

fn apply_obj_perspective_with_ref_bounds(
    local: BezierPath,
    ref_rect: Rect2D,
    obj: &SceneObject,
) -> BezierPath {
    let mut cur = if let Some(persp) = obj.perspective {
        persp.transform_bezier_path(&local)
    } else {
        local
    };
    let mut cur_ref = if let Some(persp) = obj.perspective {
        persp.try_transform_rect(ref_rect).unwrap_or(ref_rect)
    } else {
        ref_rect
    };
    for eff in &obj.effects {
        if let Effect::Perspective { quad } = eff {
            if let Some(h) = Perspective3x3::from_rect_to_quad(cur_ref, quad) {
                cur = h.transform_bezier_path(&cur);
                cur_ref = h.try_transform_rect(cur_ref).unwrap_or(cur_ref);
            }
        }
    }
    cur
}

#[inline]
fn warp_local_to_world(
    local: &BezierPath,
    world_xf: &Affine2D,
    obj_xf: &Affine2D,
    parent_persp: Option<Perspective3x3>,
) -> BezierPath {
    if let Some(pp) = parent_persp {
        pp.compose(&Perspective3x3::from_affine(*obj_xf))
            .transform_bezier_path(local)
    } else {
        local.transformed(world_xf)
    }
}

#[inline]
fn compose_hit_parent_persp(
    parent_persp: Option<Perspective3x3>,
    parent_xf: &Affine2D,
    obj_xf: &Affine2D,
    own_persp: Option<Perspective3x3>,
) -> Option<Perspective3x3> {
    match (parent_persp, own_persp) {
        (Some(pp), Some(op)) => Some(
            pp.compose(&Perspective3x3::from_affine(*obj_xf))
                .compose(&op),
        ),
        (Some(pp), None) => Some(pp.compose(&Perspective3x3::from_affine(*obj_xf))),
        (None, Some(op)) => {
            let world_xf = parent_xf.compose(obj_xf);
            Some(Perspective3x3::from_affine(world_xf).compose(&op))
        }
        (None, None) => None,
    }
}

type HitSymCtx<'a> = (
    &'a HashMap<ObjectId, &'a SceneObject>,
    Option<&'a [SceneObject]>,
    Affine2D,
);

fn container_own_persp(
    doc: &Document,
    obj: &SceneObject,
    sym_ctx: Option<HitSymCtx<'_>>,
    visited_syms: &mut HashSet<SymbolId>,
) -> Option<Perspective3x3> {
    let mut own = obj.perspective;
    let has_eff_persp = obj
        .effects
        .iter()
        .any(|e| matches!(e, Effect::Perspective { .. }));
    if has_eff_persp {
        let raw_bb = match &obj.payload {
            NodePayload::SymbolInstance(sym_id) => {
                let space_world = (sym_ctx.is_none() && obj.id.0 != 0)
                    .then(|| doc.world_transform_of(obj.id))
                    .flatten();
                doc.symbol_bounds_with_xf_and_space(
                    *sym_id,
                    &Affine2D::IDENTITY,
                    space_world.as_ref(),
                )
            }
            _ if sym_ctx.is_none() && obj.id.0 != 0 => doc.local_object_bounds(obj.id),
            NodePayload::Group(children) => {
                let mut acc: Option<Rect2D> = None;
                if let Some((sym_map, sym_objs, _)) = sym_ctx {
                    let mut visiting = HashMap::new();
                    let mut cache = HashMap::new();
                    for &cid in children {
                        if sym_map.contains_key(&cid) {
                            let (cb, _) = sym_sub_world_bounds_with_objs(
                                doc,
                                cid,
                                sym_map,
                                sym_objs,
                                Affine2D::IDENTITY,
                                &mut visiting,
                                visited_syms,
                                &mut cache,
                                0,
                            );
                            if let Some(cb) = cb {
                                acc = Some(acc.map_or(cb, |prev| prev.union(cb)));
                            }
                        }
                    }
                    let obj_sym_xf = if obj.id.0 != 0 {
                        sym_object_world_xf(doc, obj.id, &Affine2D::IDENTITY, sym_map, sym_objs)
                    } else {
                        Some(obj.transform)
                    };
                    if let Some(inv) = obj_sym_xf.and_then(|xf| xf.inverse()) {
                        acc = acc.and_then(|r| inv.try_transform_rect(r));
                    }
                }
                acc
            }
            NodePayload::PowerClip { container, .. } => container.bounding_box(),
            _ => None,
        };
        let mut cur_bb = match (own, raw_bb) {
            (Some(p), Some(bb)) => p.try_transform_rect(bb),
            (None, Some(bb)) => Some(bb),
            (_, None) => None,
        };
        for eff in &obj.effects {
            if let Effect::Perspective { quad } = eff {
                if let Some(bb) = cur_bb {
                    if let Some(h) = Perspective3x3::from_rect_to_quad(bb, quad) {
                        own = Some(match own {
                            Some(prev) => h.compose(&prev),
                            None => h,
                        });
                        cur_bb = h.try_transform_rect(bb);
                    }
                }
            }
        }
    }
    own
}

fn powerclip_world_persp(doc: &Document, pc_id: ObjectId) -> Option<Perspective3x3> {
    let mut chain = Vec::new();
    let mut cur = Some(pc_id);
    let mut seen = HashSet::new();
    while let Some(oid) = cur {
        if !seen.insert(oid) {
            break;
        }
        chain.push(oid);
        cur = doc.parent_container_of(oid);
    }
    let mut persp: Option<Perspective3x3> = None;
    let mut parent_xf = Affine2D::IDENTITY;
    let mut visited_syms = HashSet::new();
    for oid in chain.into_iter().rev() {
        let obj = doc.object(oid)?;
        let own = container_own_persp(doc, obj, None, &mut visited_syms);
        persp = compose_hit_parent_persp(persp, &parent_xf, &obj.transform, own);
        parent_xf = parent_xf.compose(&obj.transform);
    }
    persp
}

fn resolve_warped_parent_context(
    doc: &Document,
    id: ObjectId,
    powerclip_edit_id: Option<ObjectId>,
) -> Option<(Affine2D, Option<Perspective3x3>)> {
    let parent_id = powerclip_edit_id
        .filter(|&pc_id| {
            matches!(
                doc.object(pc_id).map(|o| &o.payload),
                Some(NodePayload::PowerClip { contents, .. }) if contents.contains(&id)
            )
        })
        .or_else(|| doc.parent_container_of(id));
    if let Some(pid) = parent_id {
        Some((
            doc.world_transform_of(pid)?,
            powerclip_world_persp(doc, pid),
        ))
    } else {
        Some((Affine2D::IDENTITY, None))
    }
}

/// Returns the perspective-warped world-space axis-aligned bounding box of `id`,
/// incorporating enclosing `PowerClip` / `Group` `Perspective3x3` & `Effect::Perspective` transforms.
#[must_use]
pub fn hit_test_object_warped_world_bounds(
    doc: &Document,
    id: ObjectId,
    powerclip_edit_id: Option<ObjectId>,
) -> Option<Rect2D> {
    let obj = doc.object(id)?;
    let (parent_xf, parent_persp) = resolve_warped_parent_context(doc, id, powerclip_edit_id)?;
    let mut visited_syms = HashSet::new();
    object_warped_world_bounds(
        doc,
        obj,
        &parent_xf,
        parent_persp,
        None,
        &mut visited_syms,
        0,
    )
}

/// Returns the perspective-warped world-space path of `id` when `id` has a path-based payload,
/// incorporating enclosing `PowerClip` / `Group` `Perspective3x3` & `Effect::Perspective` transforms.
#[must_use]
pub fn hit_test_object_warped_world_path(
    doc: &Document,
    id: ObjectId,
    powerclip_edit_id: Option<ObjectId>,
) -> Option<BezierPath> {
    let (parent_xf, parent_persp) = resolve_warped_parent_context(doc, id, powerclip_edit_id)?;
    let mut cur_id = id;
    let mut depth = 0usize;
    let obj = loop {
        let o = doc.object(cur_id)?;
        match &o.payload {
            NodePayload::Clone { master_id, .. } if depth < 64 => {
                if let Some((eff_p, eff_fill, eff_outline)) = doc.resolve_clone_payload_and_style(o)
                {
                    let mut tmp = o.clone();
                    tmp.payload = eff_p;
                    tmp.fill = eff_fill;
                    tmp.outline = eff_outline;
                    let world_xf = parent_xf.compose(&tmp.transform);
                    if !world_xf.is_finite() {
                        return None;
                    }
                    let local = match &tmp.payload {
                        NodePayload::Path(p) => p.clone(),
                        NodePayload::Shape(s) => s.to_bezier_path(),
                        NodePayload::BSpline(bs) => bs.to_bezier_path(),
                        NodePayload::ArtisticMedia { spine, style } => {
                            ArtisticMedia::expand_stroke(spine, style)
                        }
                        NodePayload::PowerClip { container, .. } => (**container).clone(),
                        _ => return None,
                    };
                    let warped = apply_obj_perspective_to_local_path(local, &tmp);
                    return Some(warp_local_to_world(
                        &warped,
                        &world_xf,
                        &tmp.transform,
                        parent_persp,
                    ));
                }
                cur_id = *master_id;
                depth += 1;
            }
            _ => break o,
        }
    };
    if !obj.visible || !parent_xf.is_finite() || !obj.transform.is_finite() {
        return None;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        return None;
    }
    let local = match &obj.payload {
        NodePayload::Path(p) => p.clone(),
        NodePayload::Shape(s) => s.to_bezier_path(),
        NodePayload::BSpline(bs) => bs.to_bezier_path(),
        NodePayload::ArtisticMedia { spine, style } => ArtisticMedia::expand_stroke(spine, style),
        NodePayload::PowerClip { container, .. } => (**container).clone(),
        _ => return None,
    };
    let warped = apply_obj_perspective_to_local_path(local, obj);
    Some(warp_local_to_world(
        &warped,
        &world_xf,
        &obj.transform,
        parent_persp,
    ))
}

#[allow(clippy::too_many_arguments)]
fn object_warped_world_bounds(
    doc: &Document,
    obj: &SceneObject,
    parent_xf: &Affine2D,
    parent_persp: Option<Perspective3x3>,
    sym_ctx: Option<HitSymCtx<'_>>,
    visited_syms: &mut HashSet<SymbolId>,
    depth: usize,
) -> Option<Rect2D> {
    if depth > 64 || !obj.visible || !parent_xf.is_finite() || !obj.transform.is_finite() {
        return None;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        return None;
    }

    let bounds = match &obj.payload {
        NodePayload::Path(p) => {
            let local = apply_obj_perspective_to_local_path(p.clone(), obj);
            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp).bounding_box()
        }
        NodePayload::Shape(s) => {
            let local = apply_obj_perspective_to_local_path(s.to_bezier_path(), obj);
            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp).bounding_box()
        }
        NodePayload::BSpline(bs) => {
            let local = apply_obj_perspective_to_local_path(bs.to_bezier_path(), obj);
            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp).bounding_box()
        }
        NodePayload::ArtisticMedia { spine, style } => {
            let expanded = ArtisticMedia::expand_stroke(spine, style);
            let local = apply_obj_perspective_to_local_path(expanded, obj);
            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp).bounding_box()
        }
        NodePayload::Clone { .. } => {
            let (eff_p, eff_fill, eff_outline, in_sym) = doc
                .resolve_clone_payload_and_style_with_sym_scope(obj, sym_ctx.map(|(m, _, _)| m))?;
            let mut tmp = obj.clone();
            tmp.payload = eff_p;
            tmp.fill = eff_fill;
            tmp.outline = eff_outline;
            object_warped_world_bounds(
                doc,
                &tmp,
                parent_xf,
                parent_persp,
                if in_sym { sym_ctx } else { None },
                visited_syms,
                depth + 1,
            )
        }
        NodePayload::PowerClip { container, .. } => {
            let local = apply_obj_perspective_to_local_path((**container).clone(), obj);
            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp).bounding_box()
        }
        NodePayload::Connector(c) => {
            let mut sym_visiting = HashMap::new();
            let mut sym_cache = HashMap::new();
            let ((a, b), _) = resolve_connector_endpoints_with_sym_objs(
                doc,
                c,
                &world_xf,
                sym_ctx,
                &mut sym_visiting,
                visited_syms,
                &mut sym_cache,
                depth + 1,
            );
            let route = c.route_path(a, b);
            let has_own_persp = obj.perspective.is_some()
                || obj
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::Perspective { .. }));
            let wp = if parent_persp.is_some() || has_own_persp {
                let inv_parent = parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                let local_route = route.transformed(&inv_parent);
                let warped_local = apply_obj_perspective_to_local_path(local_route, obj);
                if let Some(pp) = parent_persp {
                    pp.transform_bezier_path(&warped_local)
                } else {
                    warped_local.transformed(parent_xf)
                }
            } else {
                route
            };
            wp.bounding_box()
        }
        NodePayload::Group(children) => {
            let own_persp = container_own_persp(doc, obj, sym_ctx, visited_syms);
            let child_persp =
                compose_hit_parent_persp(parent_persp, parent_xf, &obj.transform, own_persp);
            let mut acc: Option<Rect2D> = None;
            for &cid in children {
                let child_opt = sym_ctx
                    .and_then(|(m, _, _)| m.get(&cid).copied())
                    .or_else(|| doc.object(cid));
                if let Some(child) = child_opt {
                    if let Some(cb) = object_warped_world_bounds(
                        doc,
                        child,
                        &world_xf,
                        child_persp,
                        sym_ctx,
                        visited_syms,
                        depth + 1,
                    ) {
                        acc = Some(match acc {
                            Some(prev) => prev.union(cb),
                            None => cb,
                        });
                    }
                }
            }
            acc
        }
        NodePayload::ArtisticText(at) => {
            let local = apply_obj_perspective_to_local_path(
                BezierPath::from_rect(at.estimated_bounds()),
                obj,
            );
            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp).bounding_box()
        }
        NodePayload::ParagraphText(para) => {
            let local = apply_obj_perspective_to_local_path(
                BezierPath::from_rect(para.bounding_box()),
                obj,
            );
            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp).bounding_box()
        }
        NodePayload::Raster(r) => {
            let local = apply_obj_perspective_to_local_path(
                BezierPath::from_rect(raster_local_rect(r)),
                obj,
            );
            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp).bounding_box()
        }
        NodePayload::Table(table) => {
            let local = apply_obj_perspective_to_local_path(
                BezierPath::from_rect(table.to_bounding_box()),
                obj,
            );
            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp).bounding_box()
        }
        NodePayload::Dimension(dim) => {
            let local =
                apply_obj_perspective_to_local_path(BezierPath::from_rect(dim.bounding_box()), obj);
            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp).bounding_box()
        }
        NodePayload::TextOnPath(top) => {
            if let Some(host_path) = resolve_sym_world_path_with_objs(doc, top.path_id, sym_ctx) {
                let has_own_persp = obj.perspective.is_some()
                    || obj
                        .effects
                        .iter()
                        .any(|e| matches!(e, Effect::Perspective { .. }));
                let wp = if parent_persp.is_some() || has_own_persp {
                    let inv_parent = parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_host = host_path.transformed(&inv_parent);
                    let warped_local = apply_obj_perspective_to_local_path(local_host, obj);
                    if let Some(pp) = parent_persp {
                        pp.transform_bezier_path(&warped_local)
                    } else {
                        warped_local.transformed(parent_xf)
                    }
                } else {
                    host_path
                };
                wp.bounding_box()
            } else {
                let local = apply_obj_perspective_to_local_path(
                    BezierPath::from_rect(top.text.estimated_bounds()),
                    obj,
                );
                warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp).bounding_box()
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if !visited_syms.insert(*sym_id) {
                return None;
            }
            let own_persp = container_own_persp(doc, obj, sym_ctx, visited_syms);
            let child_persp =
                compose_hit_parent_persp(parent_persp, parent_xf, &obj.transform, own_persp);
            let res = doc.symbol(*sym_id).and_then(|sym| {
                let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut referenced: HashSet<ObjectId> = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 {
                        child_map.insert(sub.id, sub);
                    }
                    match &sub.payload {
                        NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                        NodePayload::PowerClip { contents, .. } => {
                            referenced.extend(contents.iter().copied());
                        }
                        _ => {}
                    }
                }
                let mut acc: Option<Rect2D> = None;
                for sub in &sym.objects {
                    if sub.id.0 == 0 || !referenced.contains(&sub.id) {
                        if let Some(sb) = object_warped_world_bounds(
                            doc,
                            sub,
                            &world_xf,
                            child_persp,
                            Some((&child_map, Some(&sym.objects), world_xf)),
                            visited_syms,
                            depth + 1,
                        ) {
                            acc = Some(match acc {
                                Some(prev) => prev.union(sb),
                                None => sb,
                            });
                        }
                    }
                }
                acc
            });
            visited_syms.remove(sym_id);
            res
        }
    };
    bounds.filter(|r| r.is_finite())
}

#[allow(clippy::too_many_arguments)]
fn object_ref_hits_point(
    doc: &Document,
    obj: &SceneObject,
    parent_xf: &Affine2D,
    parent_persp: Option<Perspective3x3>,
    pt_mm: Point2D,
    tol_mm: f64,
    treat_all_filled: bool,
    sym_ctx: Option<HitSymCtx<'_>>,
    visited_syms: &mut HashSet<SymbolId>,
    depth: usize,
) -> bool {
    if depth > 64 || !obj.visible || !parent_xf.is_finite() || !obj.transform.is_finite() {
        return false;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        return false;
    }
    let stroke_tol = tol_mm + obj.outline.scaled_width_mm_for_transform(&world_xf) * 0.5;
    let fill_hit = obj.fill.is_visible() || obj.lens.is_some() || treat_all_filled;

    match &obj.payload {
        NodePayload::Path(p) => {
            let local = apply_obj_perspective_to_local_path(p.clone(), obj);
            let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_hits_point(&wp, fill_hit, pt_mm, stroke_tol)
        }
        NodePayload::Shape(s) => {
            let local = apply_obj_perspective_to_local_path(s.to_bezier_path(), obj);
            let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_hits_point(&wp, fill_hit, pt_mm, stroke_tol)
        }
        NodePayload::BSpline(bs) => {
            let local = apply_obj_perspective_to_local_path(bs.to_bezier_path(), obj);
            let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_hits_point(&wp, fill_hit, pt_mm, stroke_tol)
        }
        NodePayload::ArtisticMedia { spine, style } => {
            let expanded = ArtisticMedia::expand_stroke(spine, style);
            let local = apply_obj_perspective_to_local_path(expanded, obj);
            let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_hits_point(&wp, fill_hit, pt_mm, stroke_tol)
        }
        NodePayload::Clone { .. } => {
            let Some((eff_p, eff_fill, eff_outline, in_sym)) =
                doc.resolve_clone_payload_and_style_with_sym_scope(obj, sym_ctx.map(|(m, _, _)| m))
            else {
                return false;
            };
            let mut tmp = obj.clone();
            tmp.payload = eff_p;
            tmp.fill = eff_fill;
            tmp.outline = eff_outline;
            object_ref_hits_point(
                doc,
                &tmp,
                parent_xf,
                parent_persp,
                pt_mm,
                tol_mm,
                treat_all_filled,
                if in_sym { sym_ctx } else { None },
                visited_syms,
                depth + 1,
            )
        }
        NodePayload::PowerClip { container, .. } => {
            let local = apply_obj_perspective_to_local_path((**container).clone(), obj);
            let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_hits_point_rule(&wp, true, obj.clip_even_odd, pt_mm, stroke_tol)
        }
        NodePayload::Connector(c) => {
            let mut sym_visiting = HashMap::new();
            let mut sym_cache = HashMap::new();
            let ((a, b), _) = resolve_connector_endpoints_with_sym_objs(
                doc,
                c,
                &world_xf,
                sym_ctx,
                &mut sym_visiting,
                visited_syms,
                &mut sym_cache,
                depth + 1,
            );
            let route = c.route_path(a, b);
            let has_own_persp = obj.perspective.is_some()
                || obj
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::Perspective { .. }));
            let wp = if parent_persp.is_some() || has_own_persp {
                let inv_parent = parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                let local_route = route.transformed(&inv_parent);
                let warped_local = apply_obj_perspective_to_local_path(local_route, obj);
                if let Some(pp) = parent_persp {
                    pp.transform_bezier_path(&warped_local)
                } else {
                    warped_local.transformed(parent_xf)
                }
            } else {
                route
            };
            path_hits_point(&wp, false, pt_mm, stroke_tol)
        }
        NodePayload::Group(children) => {
            let own_persp = container_own_persp(doc, obj, sym_ctx, visited_syms);
            let child_persp =
                compose_hit_parent_persp(parent_persp, parent_xf, &obj.transform, own_persp);
            children.iter().rev().any(|&cid| {
                let child_opt = sym_ctx
                    .and_then(|(m, _, _)| m.get(&cid).copied())
                    .or_else(|| doc.object(cid));
                child_opt.is_some_and(|child| {
                    object_ref_hits_point(
                        doc,
                        child,
                        &world_xf,
                        child_persp,
                        pt_mm,
                        tol_mm,
                        treat_all_filled,
                        sym_ctx,
                        visited_syms,
                        depth + 1,
                    )
                })
            })
        }
        NodePayload::ArtisticText(at) => {
            let line_rects = artistic_text_line_rects(at);
            line_rects.into_iter().any(|lr| {
                let local = apply_obj_perspective_to_local_path(BezierPath::from_rect(lr), obj);
                let poly = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                path_hits_point(&poly, true, pt_mm, stroke_tol)
            })
        }
        NodePayload::ParagraphText(para) => {
            let ref_box = para.bounding_box();
            let frame_local =
                apply_obj_perspective_with_ref_bounds(BezierPath::from_rect(ref_box), ref_box, obj);
            let frame_poly =
                warp_local_to_world(&frame_local, &world_xf, &obj.transform, parent_persp);
            if fill_hit {
                return path_hits_point(&frame_poly, true, pt_mm, tol_mm.max(stroke_tol));
            }
            if path_hits_point(&frame_poly, false, pt_mm, tol_mm.max(stroke_tol)) {
                return true;
            }
            paragraph_text_line_rects(para).into_iter().any(|lr| {
                let local =
                    apply_obj_perspective_with_ref_bounds(BezierPath::from_rect(lr), ref_box, obj);
                let poly = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                path_hits_point(&poly, true, pt_mm, tol_mm.max(stroke_tol))
            })
        }
        NodePayload::Raster(r) => {
            let local = apply_obj_perspective_to_local_path(
                BezierPath::from_rect(raster_local_rect(r)),
                obj,
            );
            let poly = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_hits_point(&poly, true, pt_mm, tol_mm)
        }
        NodePayload::Table(table) => {
            let grid_local = apply_obj_perspective_to_local_path(table.to_grid_paths(), obj);
            let grid_wp = warp_local_to_world(&grid_local, &world_xf, &obj.transform, parent_persp);
            if path_hits_point(&grid_wp, false, pt_mm, stroke_tol) {
                return true;
            }
            for r in 0..table.rows {
                for c in 0..table.cols {
                    let Some(cell) = table.cell(r, c) else {
                        continue;
                    };
                    if cell.is_covered() {
                        continue;
                    }
                    let cell_filled =
                        fill_hit || cell.fill.is_visible() || !cell.text.trim().is_empty();
                    if !cell_filled {
                        continue;
                    }
                    if let Some(cr) = table.cell_rect(r, c) {
                        let local =
                            apply_obj_perspective_to_local_path(BezierPath::from_rect(cr), obj);
                        let poly =
                            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                        if path_hits_point(&poly, true, pt_mm, stroke_tol) {
                            return true;
                        }
                    }
                }
            }
            false
        }
        NodePayload::Dimension(dim) => {
            let (strokes, label_rect) = dimension_local_geometry(dim, doc);
            if strokes.iter().any(|sp| {
                let local = apply_obj_perspective_to_local_path(sp.clone(), obj);
                let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                path_hits_point(&wp, false, pt_mm, stroke_tol)
            }) {
                return true;
            }
            label_rect.is_some_and(|lr| {
                let local = apply_obj_perspective_to_local_path(BezierPath::from_rect(lr), obj);
                let poly = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                path_hits_point(&poly, true, pt_mm, tol_mm)
            })
        }
        NodePayload::TextOnPath(top) => {
            if let Some(host_path) = resolve_sym_world_path_with_objs(doc, top.path_id, sym_ctx) {
                let font_h = top
                    .text
                    .runs
                    .iter()
                    .map(|r| r.style.font_size_mm())
                    .filter(|v| v.is_finite())
                    .fold(4.0_f64, f64::max);
                let text_chars = top.text.plain_text().chars().count().max(1) as f64;
                let est_len_mm = (text_chars * font_h * 0.65).max(font_h);
                let offset_mm = sanitize_f64(top.offset_mm, 0.0).max(0.0);
                let sliced_host = slice_path_arc_interval(&host_path, offset_mm, est_len_mm);
                let band_tol = tol_mm + sanitize_f64(top.distance_mm, 0.0).abs() + font_h;
                let has_own_persp = obj.perspective.is_some()
                    || obj
                        .effects
                        .iter()
                        .any(|e| matches!(e, Effect::Perspective { .. }));
                let wp = if parent_persp.is_some() || has_own_persp {
                    let inv_parent = parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_host = sliced_host.transformed(&inv_parent);
                    let warped_local = apply_obj_perspective_to_local_path(local_host, obj);
                    if let Some(pp) = parent_persp {
                        pp.transform_bezier_path(&warped_local)
                    } else {
                        warped_local.transformed(parent_xf)
                    }
                } else {
                    sliced_host
                };
                path_hits_point(&wp, false, pt_mm, band_tol)
            } else {
                let local = apply_obj_perspective_to_local_path(
                    BezierPath::from_rect(top.text.estimated_bounds()),
                    obj,
                );
                let poly = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                path_hits_point(&poly, true, pt_mm, stroke_tol)
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if !visited_syms.insert(*sym_id) {
                return false;
            }
            let own_persp = container_own_persp(doc, obj, sym_ctx, visited_syms);
            let child_persp =
                compose_hit_parent_persp(parent_persp, parent_xf, &obj.transform, own_persp);
            let hit = doc.symbol(*sym_id).is_some_and(|sym| {
                let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut referenced: HashSet<ObjectId> = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 {
                        child_map.insert(sub.id, sub);
                    }
                    match &sub.payload {
                        NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                        NodePayload::PowerClip { contents, .. } => {
                            referenced.extend(contents.iter().copied());
                        }
                        _ => {}
                    }
                }
                sym.objects
                    .iter()
                    .rev()
                    .filter(|sub| sub.id.0 == 0 || !referenced.contains(&sub.id))
                    .any(|sub| {
                        object_ref_hits_point(
                            doc,
                            sub,
                            &world_xf,
                            child_persp,
                            pt_mm,
                            tol_mm,
                            treat_all_filled,
                            Some((&child_map, Some(&sym.objects), world_xf)),
                            visited_syms,
                            depth + 1,
                        )
                    })
            });
            visited_syms.remove(sym_id);
            hit
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn object_intersects_rect(
    doc: &Document,
    obj: &SceneObject,
    parent_xf: &Affine2D,
    parent_persp: Option<Perspective3x3>,
    rect_mm: Rect2D,
    sym_ctx: Option<HitSymCtx<'_>>,
    visited_syms: &mut HashSet<SymbolId>,
    depth: usize,
) -> bool {
    if depth > 64 || !obj.visible || !parent_xf.is_finite() || !obj.transform.is_finite() {
        return false;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        return false;
    }

    match &obj.payload {
        NodePayload::Path(p) => {
            let local = apply_obj_perspective_to_local_path(p.clone(), obj);
            let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_intersects_rect(&wp, obj.fill.is_visible() || obj.lens.is_some(), rect_mm)
        }
        NodePayload::Shape(s) => {
            let local = apply_obj_perspective_to_local_path(s.to_bezier_path(), obj);
            let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_intersects_rect(&wp, obj.fill.is_visible() || obj.lens.is_some(), rect_mm)
        }
        NodePayload::BSpline(bs) => {
            let local = apply_obj_perspective_to_local_path(bs.to_bezier_path(), obj);
            let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_intersects_rect(&wp, obj.fill.is_visible() || obj.lens.is_some(), rect_mm)
        }
        NodePayload::ArtisticMedia { spine, style } => {
            let expanded = ArtisticMedia::expand_stroke(spine, style);
            let local = apply_obj_perspective_to_local_path(expanded, obj);
            let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_intersects_rect(&wp, obj.fill.is_visible() || obj.lens.is_some(), rect_mm)
        }
        NodePayload::Clone { .. } => {
            let Some((eff_p, eff_fill, eff_outline, in_sym)) =
                doc.resolve_clone_payload_and_style_with_sym_scope(obj, sym_ctx.map(|(m, _, _)| m))
            else {
                return false;
            };
            let mut tmp = obj.clone();
            tmp.payload = eff_p;
            tmp.fill = eff_fill;
            tmp.outline = eff_outline;
            object_intersects_rect(
                doc,
                &tmp,
                parent_xf,
                parent_persp,
                rect_mm,
                if in_sym { sym_ctx } else { None },
                visited_syms,
                depth + 1,
            )
        }
        NodePayload::PowerClip { container, .. } => {
            let local = apply_obj_perspective_to_local_path((**container).clone(), obj);
            let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_intersects_rect(&wp, true, rect_mm)
        }
        NodePayload::Connector(c) => {
            let mut sym_visiting = HashMap::new();
            let mut sym_cache = HashMap::new();
            let ((a, b), _) = resolve_connector_endpoints_with_sym_objs(
                doc,
                c,
                &world_xf,
                sym_ctx,
                &mut sym_visiting,
                visited_syms,
                &mut sym_cache,
                depth + 1,
            );
            let route = c.route_path(a, b);
            let has_own_persp = obj.perspective.is_some()
                || obj
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::Perspective { .. }));
            let wp = if parent_persp.is_some() || has_own_persp {
                let inv_parent = parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                let local_route = route.transformed(&inv_parent);
                let warped_local = apply_obj_perspective_to_local_path(local_route, obj);
                if let Some(pp) = parent_persp {
                    pp.transform_bezier_path(&warped_local)
                } else {
                    warped_local.transformed(parent_xf)
                }
            } else {
                route
            };
            path_intersects_rect(&wp, false, rect_mm)
        }
        NodePayload::Group(children) => {
            let own_persp = container_own_persp(doc, obj, sym_ctx, visited_syms);
            let child_persp =
                compose_hit_parent_persp(parent_persp, parent_xf, &obj.transform, own_persp);
            children.iter().any(|&cid| {
                let child_opt = sym_ctx
                    .and_then(|(m, _, _)| m.get(&cid).copied())
                    .or_else(|| doc.object(cid));
                child_opt.is_some_and(|child| {
                    object_intersects_rect(
                        doc,
                        child,
                        &world_xf,
                        child_persp,
                        rect_mm,
                        sym_ctx,
                        visited_syms,
                        depth + 1,
                    )
                })
            })
        }
        NodePayload::ArtisticText(at) => artistic_text_line_rects(at).into_iter().any(|lr| {
            let local = apply_obj_perspective_to_local_path(BezierPath::from_rect(lr), obj);
            let poly = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_intersects_rect(&poly, true, rect_mm)
        }),
        NodePayload::ParagraphText(para) => {
            let ref_box = para.bounding_box();
            let frame_local =
                apply_obj_perspective_with_ref_bounds(BezierPath::from_rect(ref_box), ref_box, obj);
            let frame_poly =
                warp_local_to_world(&frame_local, &world_xf, &obj.transform, parent_persp);
            if obj.fill.is_visible() || obj.lens.is_some() {
                return path_intersects_rect(&frame_poly, true, rect_mm);
            }
            if path_intersects_rect(&frame_poly, false, rect_mm) {
                return true;
            }
            paragraph_text_line_rects(para).into_iter().any(|lr| {
                let local =
                    apply_obj_perspective_with_ref_bounds(BezierPath::from_rect(lr), ref_box, obj);
                let poly = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                path_intersects_rect(&poly, true, rect_mm)
            })
        }
        NodePayload::Raster(r) => {
            let local = apply_obj_perspective_to_local_path(
                BezierPath::from_rect(raster_local_rect(r)),
                obj,
            );
            let poly = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
            path_intersects_rect(&poly, true, rect_mm)
        }
        NodePayload::Table(table) => {
            let grid_local = apply_obj_perspective_to_local_path(table.to_grid_paths(), obj);
            let grid_wp = warp_local_to_world(&grid_local, &world_xf, &obj.transform, parent_persp);
            if path_intersects_rect(&grid_wp, false, rect_mm) {
                return true;
            }
            for r in 0..table.rows {
                for c in 0..table.cols {
                    let Some(cell) = table.cell(r, c) else {
                        continue;
                    };
                    if cell.is_covered() {
                        continue;
                    }
                    let cell_filled = obj.fill.is_visible()
                        || cell.fill.is_visible()
                        || !cell.text.trim().is_empty();
                    if !cell_filled {
                        continue;
                    }
                    if let Some(cr) = table.cell_rect(r, c) {
                        let local =
                            apply_obj_perspective_to_local_path(BezierPath::from_rect(cr), obj);
                        let poly =
                            warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                        if path_intersects_rect(&poly, true, rect_mm) {
                            return true;
                        }
                    }
                }
            }
            false
        }
        NodePayload::Dimension(dim) => {
            let (strokes, label_rect) = dimension_local_geometry(dim, doc);
            if strokes.iter().any(|sp| {
                let local = apply_obj_perspective_to_local_path(sp.clone(), obj);
                let wp = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                path_intersects_rect(&wp, false, rect_mm)
            }) {
                return true;
            }
            label_rect.is_some_and(|lr| {
                let local = apply_obj_perspective_to_local_path(BezierPath::from_rect(lr), obj);
                let poly = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                path_intersects_rect(&poly, true, rect_mm)
            })
        }
        NodePayload::TextOnPath(top) => {
            if let Some(host_path) = resolve_sym_world_path_with_objs(doc, top.path_id, sym_ctx) {
                let font_h = top
                    .text
                    .runs
                    .iter()
                    .map(|r| r.style.font_size_mm())
                    .filter(|v| v.is_finite())
                    .fold(4.0_f64, f64::max);
                let text_chars = top.text.plain_text().chars().count().max(1) as f64;
                let est_len_mm = (text_chars * font_h * 0.65).max(font_h);
                let offset_mm = sanitize_f64(top.offset_mm, 0.0).max(0.0);
                let sliced_host = slice_path_arc_interval(&host_path, offset_mm, est_len_mm);
                let margin = sanitize_f64(top.distance_mm, 0.0).abs() + font_h;
                let has_own_persp = obj.perspective.is_some()
                    || obj
                        .effects
                        .iter()
                        .any(|e| matches!(e, Effect::Perspective { .. }));
                let wp = if parent_persp.is_some() || has_own_persp {
                    let inv_parent = parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_host = sliced_host.transformed(&inv_parent);
                    let warped_local = apply_obj_perspective_to_local_path(local_host, obj);
                    if let Some(pp) = parent_persp {
                        pp.transform_bezier_path(&warped_local)
                    } else {
                        warped_local.transformed(parent_xf)
                    }
                } else {
                    sliced_host
                };
                path_intersects_rect(&wp, false, rect_mm.expand(margin))
            } else {
                let local = apply_obj_perspective_to_local_path(
                    BezierPath::from_rect(top.text.estimated_bounds()),
                    obj,
                );
                let poly = warp_local_to_world(&local, &world_xf, &obj.transform, parent_persp);
                path_intersects_rect(&poly, true, rect_mm)
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if !visited_syms.insert(*sym_id) {
                return false;
            }
            let own_persp = container_own_persp(doc, obj, sym_ctx, visited_syms);
            let child_persp =
                compose_hit_parent_persp(parent_persp, parent_xf, &obj.transform, own_persp);
            let hit = doc.symbol(*sym_id).is_some_and(|sym| {
                let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
                let mut referenced: HashSet<ObjectId> = HashSet::new();
                for sub in &sym.objects {
                    if sub.id.0 != 0 {
                        child_map.insert(sub.id, sub);
                    }
                    match &sub.payload {
                        NodePayload::Group(c) => referenced.extend(c.iter().copied()),
                        NodePayload::PowerClip { contents, .. } => {
                            referenced.extend(contents.iter().copied());
                        }
                        _ => {}
                    }
                }
                sym.objects
                    .iter()
                    .filter(|sub| sub.id.0 == 0 || !referenced.contains(&sub.id))
                    .any(|sub| {
                        object_intersects_rect(
                            doc,
                            sub,
                            &world_xf,
                            child_persp,
                            rect_mm,
                            Some((&child_map, Some(&sym.objects), world_xf)),
                            visited_syms,
                            depth + 1,
                        )
                    })
            });
            visited_syms.remove(sym_id);
            hit
        }
    }
}

fn segment_intersects_rect(a: Point2D, b: Point2D, r: Rect2D) -> bool {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let mut t0 = 0.0_f64;
    let mut t1 = 1.0_f64;
    for (p, q) in [
        (-dx, a.x - r.min_x),
        (dx, r.max_x - a.x),
        (-dy, a.y - r.min_y),
        (dy, r.max_y - a.y),
    ] {
        if p.abs() <= 1e-12 {
            if q < 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                if t > t1 {
                    return false;
                }
                if t > t0 {
                    t0 = t;
                }
            } else {
                if t < t0 {
                    return false;
                }
                if t < t1 {
                    t1 = t;
                }
            }
        }
    }
    t0 <= t1
}

fn segment_intersection(a1: Point2D, a2: Point2D, b1: Point2D, b2: Point2D) -> Option<Point2D> {
    let r = a2 - a1;
    let s = b2 - b1;
    let denom = r.cross(s);
    if denom.abs() <= 1e-12 {
        return None;
    }
    let qp = b1 - a1;
    let t = qp.cross(s) / denom;
    let u = qp.cross(r) / denom;
    if (-1e-7..=1.0 + 1e-7).contains(&t) && (-1e-7..=1.0 + 1e-7).contains(&u) {
        let tc = t.clamp(0.0, 1.0);
        Some(a1 + r * tc)
    } else {
        None
    }
}

fn path_intersects_rect(path: &BezierPath, check_fill: bool, rect: Rect2D) -> bool {
    if !path.bounding_box().is_some_and(|bb| bb.intersects(rect)) {
        return false;
    }
    if check_fill {
        let corners = [
            rect.center(),
            Point2D::new(rect.min_x, rect.min_y),
            Point2D::new(rect.max_x, rect.min_y),
            Point2D::new(rect.max_x, rect.max_y),
            Point2D::new(rect.min_x, rect.max_y),
        ];
        if corners.into_iter().any(|c| path.contains_point(c)) {
            return true;
        }
    }
    for poly in path.flatten(0.2) {
        if poly.iter().any(|&p| rect.contains_point(p)) {
            return true;
        }
        for win in poly.windows(2) {
            if segment_intersects_rect(win[0], win[1], rect) {
                return true;
            }
        }
    }
    false
}

fn artistic_text_line_rects(at: &ArtisticText) -> Vec<Rect2D> {
    let full = at.plain_text();
    if full.is_empty() {
        return vec![at.estimated_bounds()];
    }
    let max_size_mm = at
        .runs
        .iter()
        .map(|r| r.style.font_size_mm())
        .fold(4.0_f64, f64::max);
    let line_ratio = at.runs.first().map_or(1.2, |r| {
        sanitize_f64(r.style.line_height_ratio, 1.2).max(0.5)
    });
    let line_h = max_size_mm * line_ratio;
    let lines: Vec<&str> = full.split('\n').collect();
    let max_chars = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .max(1);
    let max_w = (max_chars as f64) * max_size_mm * 0.6;
    let mut rects = Vec::with_capacity(lines.len());
    for (idx, line) in lines.iter().enumerate() {
        let chars = line.chars().count();
        if chars == 0 {
            continue;
        }
        let w = ((chars as f64) * max_size_mm * 0.6).max(max_size_mm * 0.35);
        let x0 = match at.align {
            TextAlign::Center => (max_w - w) * 0.5,
            TextAlign::Right => (max_w - w).max(0.0),
            TextAlign::Left | TextAlign::Justify | TextAlign::ForceJustifyKashida => 0.0,
        };
        rects.push(Rect2D::from_xywh(x0, (idx as f64) * line_h, w, line_h));
    }
    if rects.is_empty() {
        rects.push(at.estimated_bounds());
    }
    rects
}

fn paragraph_text_line_rects(para: &ParagraphText) -> Vec<Rect2D> {
    let full = para.plain_text();
    if full.trim().is_empty() {
        return Vec::new();
    }
    let cols = para.columns.clamp(1, 16) as usize;
    let frame_w = sanitize_f64(para.frame_size.x, 80.0).max(2.0);
    let frame_h = sanitize_f64(para.frame_size.y, 40.0).max(2.0);
    let gutter = sanitize_f64(para.column_gutter_mm, 5.0).max(0.0);
    let col_w = ((frame_w - gutter * (cols.saturating_sub(1) as f64)) / (cols as f64)).max(2.0);

    let font_mm = para
        .runs
        .iter()
        .map(|r| r.style.font_size_mm())
        .fold(4.0_f64, f64::max);
    let line_ratio = para.runs.first().map_or(1.2, |r| {
        sanitize_f64(r.style.line_height_ratio, 1.2).max(0.5)
    });
    let line_h = (font_mm * line_ratio).max(0.5);
    let char_w = (font_mm * 0.6).max(0.5);
    let max_chars = (col_w / char_w).floor().max(1.0) as usize;
    let is_rtl = para.effective_direction() == TextDirection::Rtl;

    let mut rects = Vec::new();
    let mut current_col = 0usize;
    let mut col_y = 0.0_f64;

    for raw_para in full.split('\n') {
        if raw_para.trim().is_empty() {
            if col_y + line_h > frame_h && col_y > 0.0 {
                current_col += 1;
                col_y = 0.0;
            }
            if current_col >= cols {
                break;
            }
            col_y += line_h;
            continue;
        }

        let mut wrapped_lines: Vec<usize> = Vec::new();
        let mut cur_chars = 0usize;
        for word in raw_para.split_whitespace() {
            let wlen = word.chars().count();
            if cur_chars == 0 {
                if wlen <= max_chars {
                    cur_chars = wlen;
                } else {
                    let mut rem = wlen;
                    while rem > max_chars {
                        wrapped_lines.push(max_chars);
                        rem -= max_chars;
                    }
                    cur_chars = rem;
                }
            } else if cur_chars + 1 + wlen <= max_chars {
                cur_chars += 1 + wlen;
            } else {
                wrapped_lines.push(cur_chars);
                if wlen <= max_chars {
                    cur_chars = wlen;
                } else {
                    let mut rem = wlen;
                    while rem > max_chars {
                        wrapped_lines.push(max_chars);
                        rem -= max_chars;
                    }
                    cur_chars = rem;
                }
            }
        }
        if cur_chars > 0 {
            wrapped_lines.push(cur_chars);
        }

        let total_lines = wrapped_lines.len();
        for (l_idx, chars) in wrapped_lines.into_iter().enumerate() {
            if col_y + line_h > frame_h && col_y > 0.0 {
                current_col += 1;
                col_y = 0.0;
            }
            if current_col >= cols {
                break;
            }
            let is_last = l_idx + 1 == total_lines;
            let visual_col = if is_rtl {
                cols - 1 - current_col
            } else {
                current_col
            };
            let col_x0 = (visual_col as f64) * (col_w + gutter);
            let w = if matches!(
                para.align,
                TextAlign::Justify | TextAlign::ForceJustifyKashida
            ) && !is_last
            {
                col_w
            } else {
                ((chars as f64) * char_w).clamp(font_mm * 0.35, col_w)
            };
            let x0 = match para.align {
                TextAlign::Center => col_x0 + (col_w - w) * 0.5,
                TextAlign::Right => col_x0 + (col_w - w).max(0.0),
                TextAlign::Justify | TextAlign::ForceJustifyKashida if is_rtl => {
                    col_x0 + (col_w - w).max(0.0)
                }
                _ => col_x0,
            };
            let h = line_h.min((frame_h - col_y).max(0.1));
            rects.push(Rect2D::from_xywh(x0, col_y, w, h));
            col_y += line_h;
        }
        if current_col >= cols {
            break;
        }
    }
    rects
}

fn slice_path_arc_interval(path: &BezierPath, start_mm: f64, len_mm: f64) -> BezierPath {
    let total = path.path_length_mm();
    if total <= 1e-6 || len_mm <= 1e-6 {
        return path.clone();
    }
    let is_closed = path.subpaths.first().is_some_and(|sp| sp.closed);
    let end_mm = start_mm + len_mm;
    let steps = ((len_mm / 1.0).ceil() as usize).clamp(4, 128);
    let mut pts = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let mut d = start_mm + (end_mm - start_mm) * (i as f64 / steps as f64);
        if is_closed {
            d = d.rem_euclid(total);
        } else {
            if d < 0.0 || d > total {
                continue;
            }
        }
        if let Some((pt, _)) = path.point_and_tangent_at(d) {
            pts.push(pt);
        }
    }
    if pts.len() < 2 {
        return path.clone();
    }
    let nodes = pts
        .into_iter()
        .map(|p| PathNode::new(p, None, None, NodeKind::Cusp))
        .collect();
    BezierPath::from_subpath(Subpath {
        nodes,
        closed: false,
    })
}

fn path_hits_point(path: &BezierPath, check_fill: bool, pt: Point2D, stroke_tol: f64) -> bool {
    path_hits_point_rule(path, check_fill, false, pt, stroke_tol)
}

fn path_hits_point_rule(
    path: &BezierPath,
    check_fill: bool,
    even_odd: bool,
    pt: Point2D,
    stroke_tol: f64,
) -> bool {
    if check_fill {
        let inside = if even_odd {
            closed_subpaths_contain_even_odd(&path.subpaths, pt)
        } else {
            path.contains_point(pt)
        };
        if inside {
            return true;
        }
    }
    for poly in path.flatten(0.15) {
        for win in poly.windows(2) {
            if point_to_segment_distance(pt, win[0], win[1]) <= stroke_tol {
                return true;
            }
        }
    }
    false
}

/// Snapping target toggles and threshold for [`SnapEngine`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapOptions {
    pub grid: bool,
    pub guidelines: bool,
    pub page_edges: bool,
    pub objects: bool,
    pub nodes: bool,
    pub dynamic_guides: bool,
    pub tangent: bool,
    pub perpendicular: bool,
    pub quadrant: bool,
    pub intersection: bool,
    pub baseline: bool,
    pub tolerance_mm: f64,
}

impl Default for SnapOptions {
    fn default() -> Self {
        Self {
            grid: false,
            guidelines: true,
            page_edges: true,
            objects: true,
            nodes: true,
            dynamic_guides: true,
            tangent: true,
            perpendicular: true,
            quadrant: true,
            intersection: true,
            baseline: true,
            tolerance_mm: 2.5,
        }
    }
}

/// Visual classification of an emitted [`DynamicGuideLine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DynamicGuideKind {
    AlignmentX,
    AlignmentY,
    CenterX,
    CenterY,
    PageEdge,
    GuideLine,
    Node,
    Grid,
    Tangent,
    Perpendicular,
    Quadrant,
    Intersection,
    Baseline,
    ExtensionRay,
}

/// Dynamic visual alignment guide emitted by [`SnapEngine::snap_point`].
#[derive(Debug, Clone, PartialEq)]
pub struct DynamicGuideLine {
    pub start: Point2D,
    pub end: Point2D,
    pub kind: DynamicGuideKind,
    pub label: Option<String>,
}

/// Multi-target snapping engine supporting grid, guidelines, page edges, objects, nodes,
/// 5 geometric CAD snaps (`Tangent`, `Perpendicular`, `Quadrant`, `Intersection`, `Baseline`),
/// and live interactive dynamic alignment & extension-ray guides.
pub struct SnapEngine;

impl SnapEngine {
    /// Snaps `pt_mm` on `page_index` according to `options`, ignoring objects in `exclude_ids`.
    ///
    /// Returns `(snapped_point_mm, active_dynamic_guides)`.
    #[must_use]
    pub fn snap_point(
        doc: &Document,
        page_index: usize,
        pt_mm: Point2D,
        exclude_ids: &[ObjectId],
        options: &SnapOptions,
    ) -> (Point2D, Vec<DynamicGuideLine>) {
        Self::snap_point_with_reference(doc, page_index, pt_mm, None, exclude_ids, options)
    }

    /// Snaps `pt_mm` on `page_index` with an optional drawing anchor `ref_pt_mm` (used to
    /// compute exact `Tangent` and `Perpendicular` rays/points from the active anchor).
    #[must_use]
    pub fn snap_point_with_reference(
        doc: &Document,
        page_index: usize,
        pt_mm: Point2D,
        ref_pt_mm: Option<Point2D>,
        exclude_ids: &[ObjectId],
        options: &SnapOptions,
    ) -> (Point2D, Vec<DynamicGuideLine>) {
        if !pt_mm.is_finite() {
            return (pt_mm, Vec::new());
        }
        let tol = sanitize_f64(options.tolerance_mm, 2.5).abs();
        if tol <= 1e-9 {
            return (pt_mm, Vec::new());
        }
        let exclude: HashSet<ObjectId> = exclude_ids.iter().copied().collect();
        let page = doc.page(page_index);
        let page_rect = page
            .map(crate::Page::trim_rect)
            .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0));

        let spatial_index = page.map(|_| doc.cached_spatial_index(page_index));
        let candidates: Vec<ObjectId> = if let Some(ref index) = spatial_index {
            index
                .query_point(pt_mm, tol.max(8.0))
                .into_iter()
                .filter(|id| !exclude.contains(id))
                .collect()
        } else {
            Vec::new()
        };

        // Priority 1: Exact 2D node snapping (highest geometric precision)
        if options.nodes && page.is_some() {
            if let Some(hit) = hit_test_node(doc, &candidates, pt_mm, tol) {
                let mut guides = Vec::new();
                if options.dynamic_guides {
                    guides.push(DynamicGuideLine {
                        start: Point2D::new(hit.position_mm.x - 3.0, hit.position_mm.y),
                        end: Point2D::new(hit.position_mm.x + 3.0, hit.position_mm.y),
                        kind: DynamicGuideKind::Node,
                        label: Some("node".into()),
                    });
                    guides.push(DynamicGuideLine {
                        start: Point2D::new(hit.position_mm.x, hit.position_mm.y - 3.0),
                        end: Point2D::new(hit.position_mm.x, hit.position_mm.y + 3.0),
                        kind: DynamicGuideKind::Node,
                        label: None,
                    });
                }
                return (hit.position_mm, guides);
            }
        }

        // Priority 1.5: Exact 2D Geometric Snaps (Intersection, Quadrant, Tangent, Perpendicular, Baseline)
        let mut geom_best: Option<(f64, Point2D, Vec<DynamicGuideLine>)> = None;
        let mut try_geom = |cand_pt: Point2D, mut g_lines: Vec<DynamicGuideLine>| {
            if !cand_pt.is_finite() {
                return;
            }
            let d = cand_pt.distance_to(pt_mm);
            if d <= tol && geom_best.as_ref().is_none_or(|b| d < b.0) {
                if !options.dynamic_guides {
                    g_lines.clear();
                }
                geom_best = Some((d, cand_pt, g_lines));
            }
        };

        if page.is_some() {
            // Collect visible object paths & ellipses in/near query radius
            let mut seg_groups: Vec<(ObjectId, Vec<(Point2D, Point2D)>)> = Vec::new();
            let mut world_paths: Vec<(ObjectId, BezierPath, bool)> = Vec::new();

            for &oid in &candidates {
                let Some(obj) = doc.object(oid) else {
                    continue;
                };
                if !obj.visible {
                    continue;
                }
                let world_xf = doc.world_transform_of(oid).unwrap_or(obj.transform);

                // Quadrant & analytical circle/ellipse Tangent
                if let NodePayload::Shape(ParametricShape::Ellipse { rx, ry, .. }) = &obj.payload {
                    let rx = sanitize_f64(*rx, 1.0).abs();
                    let ry = sanitize_f64(*ry, 1.0).abs();
                    if rx > 1e-6 && ry > 1e-6 {
                        if options.quadrant {
                            let wc = world_xf.transform_point(Point2D::ZERO);
                            for (dx, dy, lbl) in [
                                (rx, 0.0, "quadrant 0°"),
                                (0.0, ry, "quadrant 90°"),
                                (-rx, 0.0, "quadrant 180°"),
                                (0.0, -ry, "quadrant 270°"),
                            ] {
                                let qp = world_xf.transform_point(Point2D::new(dx, dy));
                                let guides = vec![
                                    DynamicGuideLine {
                                        start: wc,
                                        end: qp,
                                        kind: DynamicGuideKind::ExtensionRay,
                                        label: None,
                                    },
                                    DynamicGuideLine {
                                        start: Point2D::new(qp.x - 2.5, qp.y),
                                        end: Point2D::new(qp.x + 2.5, qp.y),
                                        kind: DynamicGuideKind::Quadrant,
                                        label: Some(lbl.into()),
                                    },
                                ];
                                try_geom(qp, guides);
                            }
                        }

                        if options.tangent {
                            if let Some(ref_pt) = ref_pt_mm.filter(|p| p.is_finite()) {
                                let ellipse_to_world = world_xf.compose(&Affine2D::scale(rx, ry));
                                if let Some(world_to_unit) = ellipse_to_world.inverse() {
                                    let u_ref = world_to_unit.transform_point(ref_pt);
                                    let d2 = u_ref.x * u_ref.x + u_ref.y * u_ref.y;
                                    if d2 > 1.0 + 1e-9 {
                                        let inv_d2 = 1.0 / d2;
                                        let perp_scale = (d2 - 1.0).sqrt() * inv_d2;
                                        for sign in [-1.0_f64, 1.0_f64] {
                                            let ux = u_ref.x * inv_d2 - sign * u_ref.y * perp_scale;
                                            let uy = u_ref.y * inv_d2 + sign * u_ref.x * perp_scale;
                                            let tp = ellipse_to_world
                                                .transform_point(Point2D::new(ux, uy));
                                            let guides = vec![
                                                DynamicGuideLine {
                                                    start: ref_pt,
                                                    end: tp,
                                                    kind: DynamicGuideKind::ExtensionRay,
                                                    label: Some("tangent".into()),
                                                },
                                                DynamicGuideLine {
                                                    start: Point2D::new(tp.x - 2.5, tp.y),
                                                    end: Point2D::new(tp.x + 2.5, tp.y),
                                                    kind: DynamicGuideKind::Tangent,
                                                    label: Some("tangent".into()),
                                                },
                                            ];
                                            try_geom(tp, guides);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Text Baseline snaps
                if options.baseline {
                    match &obj.payload {
                        NodePayload::ArtisticText(at) => {
                            let font_mm = at
                                .runs
                                .iter()
                                .map(|r| r.style.font_size_mm())
                                .fold(4.0_f64, f64::max);
                            let line_ratio = at.runs.first().map_or(1.2, |r| {
                                sanitize_f64(r.style.line_height_ratio, 1.2).max(0.5)
                            });
                            let est_w = at.estimated_bounds().width().max(10.0);
                            let line_count = at.plain_text().split('\n').count().max(1);
                            for li in 0..line_count {
                                let base_y = (li as f64) * font_mm * line_ratio + font_mm * 0.85;
                                let p0 = world_xf.transform_point(Point2D::new(0.0, base_y));
                                let p1 = world_xf.transform_point(Point2D::new(est_w, base_y));
                                let dir = p1 - p0;
                                let len2 = dir.dot(dir);
                                if len2 > 1e-9 {
                                    let t = (pt_mm - p0).dot(dir) / len2;
                                    let proj = p0 + dir * t;
                                    let guides = vec![
                                        DynamicGuideLine {
                                            start: p0,
                                            end: proj,
                                            kind: DynamicGuideKind::ExtensionRay,
                                            label: None,
                                        },
                                        DynamicGuideLine {
                                            start: Point2D::new(proj.x - 3.0, proj.y),
                                            end: Point2D::new(proj.x + 3.0, proj.y),
                                            kind: DynamicGuideKind::Baseline,
                                            label: Some("baseline".into()),
                                        },
                                    ];
                                    try_geom(proj, guides);
                                }
                            }
                        }
                        NodePayload::ParagraphText(para) => {
                            let font_mm = para
                                .runs
                                .iter()
                                .map(|r| r.style.font_size_mm())
                                .fold(3.5_f64, f64::max);
                            let base_y = font_mm * 0.85;
                            let p0 = world_xf.transform_point(Point2D::new(0.0, base_y));
                            let p1 = world_xf.transform_point(Point2D::new(
                                sanitize_f64(para.frame_size.x, 40.0).max(5.0),
                                base_y,
                            ));
                            let dir = p1 - p0;
                            let len2 = dir.dot(dir);
                            if len2 > 1e-9 {
                                let t = (pt_mm - p0).dot(dir) / len2;
                                let proj = p0 + dir * t;
                                let guides = vec![DynamicGuideLine {
                                    start: p0,
                                    end: p1,
                                    kind: DynamicGuideKind::Baseline,
                                    label: Some("baseline".into()),
                                }];
                                try_geom(proj, guides);
                            }
                        }
                        NodePayload::TextOnPath(top) => {
                            if let Some(hp) = doc.world_path_of(top.path_id) {
                                if let Some((pt, _)) = hp
                                    .point_and_tangent_at(sanitize_f64(top.offset_mm, 0.0).max(0.0))
                                {
                                    let guides = vec![DynamicGuideLine {
                                        start: Point2D::new(pt.x - 3.0, pt.y),
                                        end: Point2D::new(pt.x + 3.0, pt.y),
                                        kind: DynamicGuideKind::Baseline,
                                        label: Some("baseline".into()),
                                    }];
                                    try_geom(pt, guides);
                                }
                            }
                        }
                        _ => {}
                    }
                }

                // Collect world path ONLY when Perpendicular / Intersection / Tangent snaps are enabled
                if options.perpendicular || options.tangent || options.intersection {
                    if let Some(wp) = doc.world_path_of(oid) {
                        let is_full_ellipse = matches!(
                            obj.payload,
                            NodePayload::Shape(ParametricShape::Ellipse {
                                mode: crate::EllipseMode::Full,
                                ..
                            })
                        );
                        if options.perpendicular || options.intersection {
                            let mut segs = Vec::new();
                            for poly in wp.flatten(0.02) {
                                for w in poly.windows(2) {
                                    if w[0].distance_to(w[1]) > 1e-7 {
                                        segs.push((w[0], w[1]));
                                    }
                                }
                            }
                            if !segs.is_empty() {
                                seg_groups.push((oid, segs));
                            }
                        }
                        world_paths.push((oid, wp, is_full_ellipse));
                    }
                }
            }

            // Perpendicular snap from ref_pt_mm onto nearby segments / curves
            if options.perpendicular {
                if let Some(ref_pt) = ref_pt_mm.filter(|p| p.is_finite()) {
                    for (_, wp, _) in &world_paths {
                        let total = wp.path_length_mm();
                        if total <= 1e-6 {
                            continue;
                        }
                        // Coarse sample + Newton/golden refinement for < 0.01mm precision
                        let samples = ((total / 0.5).ceil() as usize).clamp(16, 256);
                        let step = total / samples as f64;
                        for i in 0..samples {
                            let s0 = i as f64 * step;
                            let s1 = (i + 1) as f64 * step;
                            let Some((p0, t0)) = wp.point_and_tangent_at(s0) else {
                                continue;
                            };
                            let Some((p1, t1)) = wp.point_and_tangent_at(s1) else {
                                continue;
                            };
                            let f0 = (p0 - ref_pt).dot(t0);
                            let f1 = (p1 - ref_pt).dot(t1);
                            let root_s = if f0.abs() < 1e-9 {
                                Some(s0)
                            } else if f1.abs() < 1e-9 {
                                Some(s1)
                            } else if (f0 < 0.0) != (f1 < 0.0) {
                                let mut a = s0;
                                let mut b = s1;
                                let mut fa = f0;
                                for _ in 0..24 {
                                    let m = 0.5 * (a + b);
                                    let Some((pm, tm)) = wp.point_and_tangent_at(m) else {
                                        break;
                                    };
                                    let fm = (pm - ref_pt).dot(tm);
                                    if fm.abs() < 1e-9 || (b - a) < 1e-7 {
                                        a = m;
                                        b = m;
                                        break;
                                    }
                                    if fa * fm > 0.0 {
                                        a = m;
                                        fa = fm;
                                    } else {
                                        b = m;
                                    }
                                }
                                Some(0.5 * (a + b))
                            } else {
                                None
                            };
                            if let Some(sm) = root_s {
                                if let Some((foot, _)) = wp.point_and_tangent_at(sm) {
                                    let guides = vec![
                                        DynamicGuideLine {
                                            start: ref_pt,
                                            end: foot,
                                            kind: DynamicGuideKind::ExtensionRay,
                                            label: Some("perpendicular".into()),
                                        },
                                        DynamicGuideLine {
                                            start: Point2D::new(foot.x - 2.5, foot.y),
                                            end: Point2D::new(foot.x + 2.5, foot.y),
                                            kind: DynamicGuideKind::Perpendicular,
                                            label: Some("perpendicular".into()),
                                        },
                                    ];
                                    try_geom(foot, guides);
                                }
                            }
                        }
                    }
                    // Also exact segment-projection fallback for sharp corners
                    for (_, segs) in &seg_groups {
                        for &(a, b) in segs {
                            let ab = b - a;
                            let len2 = ab.dot(ab);
                            if len2 <= 1e-9 {
                                continue;
                            }
                            let t = (ref_pt - a).dot(ab) / len2;
                            if (0.0..=1.0).contains(&t) {
                                let foot = a + ab * t;
                                let guides = vec![
                                    DynamicGuideLine {
                                        start: ref_pt,
                                        end: foot,
                                        kind: DynamicGuideKind::ExtensionRay,
                                        label: Some("perpendicular".into()),
                                    },
                                    DynamicGuideLine {
                                        start: Point2D::new(foot.x - 2.5, foot.y),
                                        end: Point2D::new(foot.x + 2.5, foot.y),
                                        kind: DynamicGuideKind::Perpendicular,
                                        label: Some("perpendicular".into()),
                                    },
                                ];
                                try_geom(foot, guides);
                            }
                        }
                    }
                }
            }

            // General curve Tangent snap from ref_pt_mm when not already handled analytically by Ellipse
            if options.tangent {
                if let Some(ref_pt) = ref_pt_mm.filter(|p| p.is_finite()) {
                    for (_, wp, is_full_ellipse) in &world_paths {
                        if *is_full_ellipse {
                            continue;
                        }
                        let total = wp.path_length_mm();
                        if total <= 1e-6 {
                            continue;
                        }
                        let samples = ((total / 0.5).ceil() as usize).clamp(16, 256);
                        let step = total / samples as f64;
                        for i in 0..samples {
                            let s0 = i as f64 * step;
                            let s1 = (i + 1) as f64 * step;
                            let Some((p0, t0)) = wp.point_and_tangent_at(s0) else {
                                continue;
                            };
                            let Some((p1, t1)) = wp.point_and_tangent_at(s1) else {
                                continue;
                            };
                            let c0 = (p0 - ref_pt).cross(t0);
                            let c1 = (p1 - ref_pt).cross(t1);
                            let root_s = if c0.abs() < 1e-9 {
                                Some(s0)
                            } else if c1.abs() < 1e-9 {
                                Some(s1)
                            } else if (c0 < 0.0) != (c1 < 0.0) {
                                let mut a = s0;
                                let mut b = s1;
                                let mut ca = c0;
                                for _ in 0..24 {
                                    let m = 0.5 * (a + b);
                                    let Some((pm, tm)) = wp.point_and_tangent_at(m) else {
                                        break;
                                    };
                                    let cm = (pm - ref_pt).cross(tm);
                                    if cm.abs() < 1e-9 || (b - a) < 1e-7 {
                                        a = m;
                                        b = m;
                                        break;
                                    }
                                    if ca * cm > 0.0 {
                                        a = m;
                                        ca = cm;
                                    } else {
                                        b = m;
                                    }
                                }
                                Some(0.5 * (a + b))
                            } else {
                                None
                            };
                            if let Some(sm) = root_s {
                                if let Some((tp, _)) = wp.point_and_tangent_at(sm) {
                                    if tp.distance_to(ref_pt) > 0.1 {
                                        let guides = vec![
                                            DynamicGuideLine {
                                                start: ref_pt,
                                                end: tp,
                                                kind: DynamicGuideKind::ExtensionRay,
                                                label: Some("tangent".into()),
                                            },
                                            DynamicGuideLine {
                                                start: Point2D::new(tp.x - 2.5, tp.y),
                                                end: Point2D::new(tp.x + 2.5, tp.y),
                                                kind: DynamicGuideKind::Tangent,
                                                label: Some("tangent".into()),
                                            },
                                        ];
                                        try_geom(tp, guides);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Intersection snap across distinct candidate objects
            if options.intersection && seg_groups.len() >= 2 {
                let query_box =
                    Rect2D::from_xywh(pt_mm.x - tol, pt_mm.y - tol, tol * 2.0, tol * 2.0);
                for i in 0..seg_groups.len() {
                    let (_, ref segs_a) = seg_groups[i];
                    for (_, ref segs_b) in seg_groups.iter().skip(i + 1) {
                        for &(a1, a2) in segs_a {
                            if !segment_intersects_rect(a1, a2, query_box) {
                                continue;
                            }
                            for &(b1, b2) in segs_b {
                                if !segment_intersects_rect(b1, b2, query_box) {
                                    continue;
                                }
                                if let Some(ip) = segment_intersection(a1, a2, b1, b2) {
                                    let guides = vec![
                                        DynamicGuideLine {
                                            start: Point2D::new(ip.x - 3.0, ip.y - 3.0),
                                            end: Point2D::new(ip.x + 3.0, ip.y + 3.0),
                                            kind: DynamicGuideKind::Intersection,
                                            label: Some("intersection".into()),
                                        },
                                        DynamicGuideLine {
                                            start: Point2D::new(ip.x - 3.0, ip.y + 3.0),
                                            end: Point2D::new(ip.x + 3.0, ip.y - 3.0),
                                            kind: DynamicGuideKind::Intersection,
                                            label: None,
                                        },
                                    ];
                                    try_geom(ip, guides);
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some((_, g_pt, g_lines)) = geom_best {
            return (g_pt, g_lines);
        }

        // Priority 2: Independent X and Y axis snapping across objects, guidelines, page edges, grid
        // Track best candidate per axis: (distance, snapped_coord, optional_guide)
        let mut best_x: Option<(f64, f64, Option<DynamicGuideLine>)> = None;
        let mut best_y: Option<(f64, f64, Option<DynamicGuideLine>)> = None;

        let mut try_x = |target_x: f64, guide: Option<DynamicGuideLine>| {
            let d = (pt_mm.x - target_x).abs();
            if d <= tol && best_x.as_ref().is_none_or(|b| d < b.0) {
                best_x = Some((d, target_x, guide));
            }
        };
        let mut try_y = |target_y: f64, guide: Option<DynamicGuideLine>| {
            let d = (pt_mm.y - target_y).abs();
            if d <= tol && best_y.as_ref().is_none_or(|b| d < b.0) {
                best_y = Some((d, target_y, guide));
            }
        };

        // 2a. Object edges & centers (emits dynamic alignment guides!)
        if options.objects && page.is_some() {
            // On large scenes (> 256 objects), pre-filter via R-Tree X/Y strip queries in O(log N)
            // since any object whose min/center/max lies within `tol` of `pt_mm.x` or `pt_mm.y`
            // must intersect one of the two infinite tolerance bands.
            let strip_candidates: Option<HashSet<ObjectId>> = if doc.objects().len() > 256 {
                spatial_index.as_ref().map(|idx| {
                    let mut set: HashSet<ObjectId> = idx
                        .query_rect(Rect2D::new(pt_mm.x - tol, -1.0e9, pt_mm.x + tol, 1.0e9))
                        .into_iter()
                        .collect();
                    set.extend(idx.query_rect(Rect2D::new(
                        -1.0e9,
                        pt_mm.y - tol,
                        1.0e9,
                        pt_mm.y + tol,
                    )));
                    set
                })
            } else {
                None
            };
            for (p_idx, pg) in doc.pages().iter().enumerate() {
                for layer in &pg.layers {
                    if !layer.visible || !Document::layer_applies_to_page(layer, p_idx, page_index)
                    {
                        continue;
                    }
                    for &oid in &layer.objects {
                        if exclude.contains(&oid) {
                            continue;
                        }
                        if let Some(ref allow) = strip_candidates {
                            if !allow.contains(&oid) {
                                continue;
                            }
                        }
                        let Some(obj) = doc.object(oid) else {
                            continue;
                        };
                        if !obj.visible {
                            continue;
                        }
                        let Some(bb) = doc.world_object_bounds(oid) else {
                            continue;
                        };
                        let c = bb.center();
                        let y_span_min = bb.min_y.min(pt_mm.y);
                        let y_span_max = bb.max_y.max(pt_mm.y);
                        let x_span_min = bb.min_x.min(pt_mm.x);
                        let x_span_max = bb.max_x.max(pt_mm.x);

                        for (x_val, kind, lbl) in [
                            (bb.min_x, DynamicGuideKind::AlignmentX, "left"),
                            (c.x, DynamicGuideKind::CenterX, "center"),
                            (bb.max_x, DynamicGuideKind::AlignmentX, "right"),
                        ] {
                            let g = options.dynamic_guides.then(|| DynamicGuideLine {
                                start: Point2D::new(x_val, y_span_min),
                                end: Point2D::new(x_val, y_span_max),
                                kind,
                                label: Some(lbl.into()),
                            });
                            try_x(x_val, g);
                        }

                        for (y_val, kind, lbl) in [
                            (bb.min_y, DynamicGuideKind::AlignmentY, "top"),
                            (c.y, DynamicGuideKind::CenterY, "center"),
                            (bb.max_y, DynamicGuideKind::AlignmentY, "bottom"),
                        ] {
                            let g = options.dynamic_guides.then(|| DynamicGuideLine {
                                start: Point2D::new(x_span_min, y_val),
                                end: Point2D::new(x_span_max, y_val),
                                kind,
                                label: Some(lbl.into()),
                            });
                            try_y(y_val, g);
                        }
                    }
                }
            }
        }

        // 2b. Document guidelines (Horizontal, Vertical, and Angled projection)
        let mut angled_snap: Option<(f64, Point2D, DynamicGuideLine)> = None;
        if options.guidelines {
            for guide in doc.guides() {
                match guide.orientation {
                    GuideOrientation::Vertical(gx) => {
                        let g = options.dynamic_guides.then(|| DynamicGuideLine {
                            start: Point2D::new(gx, page_rect.min_y),
                            end: Point2D::new(gx, page_rect.max_y),
                            kind: DynamicGuideKind::GuideLine,
                            label: Some("guide".into()),
                        });
                        try_x(gx, g);
                    }
                    GuideOrientation::Horizontal(gy) => {
                        let g = options.dynamic_guides.then(|| DynamicGuideLine {
                            start: Point2D::new(page_rect.min_x, gy),
                            end: Point2D::new(page_rect.max_x, gy),
                            kind: DynamicGuideKind::GuideLine,
                            label: Some("guide".into()),
                        });
                        try_y(gy, g);
                    }
                    GuideOrientation::Angled { origin, angle_deg } => {
                        let dir = Vec2D::from_angle(angle_deg.to_radians());
                        let rel = pt_mm - origin;
                        let proj_pt = origin + dir * rel.dot(dir);
                        let d = pt_mm.distance_to(proj_pt);
                        if d <= tol && angled_snap.as_ref().is_none_or(|b| d < b.0) {
                            let span = page_rect.width().max(page_rect.height());
                            angled_snap = Some((
                                d,
                                proj_pt,
                                DynamicGuideLine {
                                    start: origin - dir * span,
                                    end: origin + dir * span,
                                    kind: DynamicGuideKind::GuideLine,
                                    label: Some("guide".into()),
                                },
                            ));
                        }
                    }
                }
            }
        }

        // 2c. Page edges & center
        if options.page_edges {
            let pc = page_rect.center();
            for (x_val, lbl) in [
                (page_rect.min_x, "page left"),
                (pc.x, "page center"),
                (page_rect.max_x, "page right"),
            ] {
                let g = options.dynamic_guides.then(|| DynamicGuideLine {
                    start: Point2D::new(x_val, page_rect.min_y),
                    end: Point2D::new(x_val, page_rect.max_y),
                    kind: DynamicGuideKind::PageEdge,
                    label: Some(lbl.into()),
                });
                try_x(x_val, g);
            }
            for (y_val, lbl) in [
                (page_rect.min_y, "page top"),
                (pc.y, "page center"),
                (page_rect.max_y, "page bottom"),
            ] {
                let g = options.dynamic_guides.then(|| DynamicGuideLine {
                    start: Point2D::new(page_rect.min_x, y_val),
                    end: Point2D::new(page_rect.max_x, y_val),
                    kind: DynamicGuideKind::PageEdge,
                    label: Some(lbl.into()),
                });
                try_y(y_val, g);
            }
        }

        // 2d. Document Grid
        if options.grid {
            let grid = doc.grid();
            let subs = f64::from(grid.subdivisions.max(1));
            let step_x = sanitize_f64(grid.spacing_x_mm, 10.0).max(0.1) / subs;
            let step_y = sanitize_f64(grid.spacing_y_mm, 10.0).max(0.1) / subs;
            let gx = (pt_mm.x / step_x).round() * step_x;
            let gy = (pt_mm.y / step_y).round() * step_y;
            let g_x = options.dynamic_guides.then(|| DynamicGuideLine {
                start: Point2D::new(gx, page_rect.min_y),
                end: Point2D::new(gx, page_rect.max_y),
                kind: DynamicGuideKind::Grid,
                label: Some("grid".into()),
            });
            let g_y = options.dynamic_guides.then(|| DynamicGuideLine {
                start: Point2D::new(page_rect.min_x, gy),
                end: Point2D::new(page_rect.max_x, gy),
                kind: DynamicGuideKind::Grid,
                label: Some("grid".into()),
            });
            try_x(gx, g_x);
            try_y(gy, g_y);
        }

        // Assemble final snapped point & dynamic guides
        let mut snapped = pt_mm;
        let mut guides = Vec::new();
        let snapped_x = best_x.is_some();
        let snapped_y = best_y.is_some();
        if let Some((_, x_val, g_opt)) = best_x {
            snapped.x = x_val;
            if let Some(g) = g_opt {
                guides.push(g);
            }
        }
        if let Some((_, y_val, g_opt)) = best_y {
            snapped.y = y_val;
            if let Some(g) = g_opt {
                guides.push(g);
            }
        }

        // If neither axis snapped but an angled guideline snapped, apply the angled snap
        if !snapped_x && !snapped_y {
            if let Some((_, apt, ag)) = angled_snap {
                snapped = apt;
                if options.dynamic_guides {
                    guides.push(ag);
                }
            }
        }

        (snapped, guides)
    }
}
