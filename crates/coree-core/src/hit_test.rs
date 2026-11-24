// SPDX-License-Identifier: MIT OR Apache-2.0

//! Spatial R-Tree accelerated hit-testing (`hit_test_point`, `hit_test_rect`,
//! `hit_test_node`) and intelligent multi-target [`SnapEngine`] emitting
//! [`DynamicGuideLine`] alignment overlays.

use crate::geometry::point_to_segment_distance;
use crate::math::sanitize_f64;
use crate::{
    Affine2D, ArtisticText, BezierPath, DimensionKind, DimensionObject, Document, GuideOrientation,
    NodePayload, NodeRef, ObjectId, Point2D, Rect2D, SceneObject, SymbolId, Vec2D,
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
    if !pt_mm.is_finite() || page_index >= doc.pages().len() {
        return None;
    }
    let tol = sanitize_f64(tolerance_mm, 1.0).abs();
    let blocked = locked_or_hidden_layer_objects(doc, page_index);
    let index = doc.build_spatial_index(page_index);
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
        if object_hits_point(doc, oid, &Affine2D::IDENTITY, pt_mm, tol, 0) {
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
    if !pt_mm.is_finite() {
        return None;
    }
    let tol = sanitize_f64(tolerance_mm, 1.0).abs();
    let pc_obj = doc.object(pc_id)?;
    let crate::NodePayload::PowerClip { contents, .. } = &pc_obj.payload else {
        return None;
    };
    let pc_xf = doc.world_transform_of(pc_id)?;
    for &cid in contents.iter().rev() {
        let Some(child) = doc.object(cid) else {
            continue;
        };
        if !child.visible || child.locked {
            continue;
        }
        if object_hits_point(doc, cid, &pc_xf, pt_mm, tol, 0) {
            return Some(cid);
        }
    }
    None
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
    let index = doc.build_spatial_index(page_index);
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
            if let Some(bb) = doc.world_object_bounds(oid) {
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

/// Finds the closest Bézier anchor node on any object in `object_ids` within `tolerance_mm` of `pt_mm`.
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

fn object_hits_point(
    doc: &Document,
    id: ObjectId,
    parent_xf: &Affine2D,
    pt_mm: Point2D,
    tol_mm: f64,
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
        pt_mm,
        tol_mm,
        None,
        &mut visited_syms,
        depth,
    )
}

fn sym_object_world_xf(
    id: ObjectId,
    sym_root_xf: &Affine2D,
    sym_map: &HashMap<ObjectId, &SceneObject>,
) -> Option<Affine2D> {
    let mut chain = Vec::new();
    let mut cur = id;
    let mut seen = HashSet::new();
    while seen.insert(cur) {
        let obj = sym_map.get(&cur).copied()?;
        chain.push(obj.transform);
        let parent = sym_map.iter().find_map(|(&pid, po)| match &po.payload {
            NodePayload::Group(c) if c.contains(&cur) => Some(pid),
            NodePayload::PowerClip { contents, .. } if contents.contains(&cur) => Some(pid),
            _ => None,
        });
        match parent {
            Some(pid) => cur = pid,
            None => break,
        }
    }
    let mut xf = *sym_root_xf;
    for local in chain.into_iter().rev() {
        xf = xf.compose(&local);
    }
    Some(xf)
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
        let p_xf = sym_object_world_xf(id, &sym_root_xf, sym_map)?;
        if !p_xf.is_finite() {
            return None;
        }
        match &sub.payload {
            NodePayload::Path(p) => p.transformed(&p_xf).bounding_box(),
            NodePayload::Shape(s) => s.to_bezier_path().transformed(&p_xf).bounding_box(),
            NodePayload::PowerClip { container, .. } => container.transformed(&p_xf).bounding_box(),
            NodePayload::ArtisticText(t) => p_xf.try_transform_rect(t.estimated_bounds()),
            NodePayload::ParagraphText(t) => p_xf.try_transform_rect(t.bounding_box()),
            NodePayload::Table(t) => p_xf.try_transform_rect(t.to_bounding_box()),
            NodePayload::Dimension(d) => p_xf.try_transform_rect(d.bounding_box()),
            NodePayload::Raster(r) => p_xf.try_transform_rect(raster_local_rect(r)),
            NodePayload::TextOnPath(top) => {
                resolve_sym_world_path(doc, top.path_id, Some((sym_map, sym_root_xf)))
                    .and_then(|wp| wp.bounding_box())
                    .or_else(|| p_xf.try_transform_rect(top.text.estimated_bounds()))
            }
            NodePayload::Group(children) => {
                let mut acc: Option<Rect2D> = None;
                for &cid in children {
                    let (cb, backedge) = sym_sub_world_bounds(
                        doc,
                        cid,
                        sym_map,
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
                acc
            }
            NodePayload::Connector(c) => {
                let ((a, b), backedge) = resolve_connector_endpoints_with_sym(
                    doc,
                    c,
                    &p_xf,
                    Some((sym_map, sym_root_xf)),
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
                    min_backedge = 0;
                    return None;
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
                        if inner.id.0 != 0 && !referenced.contains(&inner.id) {
                            let (ib, backedge) = sym_sub_world_bounds(
                                doc,
                                inner.id,
                                &child_map,
                                p_xf,
                                &mut inner_visiting,
                                visited_syms,
                                &mut inner_cache,
                                depth + 1,
                            );
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
                    }
                    acc
                });
                visited_syms.remove(sym_id);
                inner_res
            }
        }
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
    let mut min_backedge = usize::MAX;
    let mut resolve_ep =
        |obj_id: Option<ObjectId>, anchor: &crate::AnchorPoint, fallback: Point2D| {
            if let Some(oid) = obj_id {
                if let Some((sym_map, sym_root_xf)) = sym_ctx {
                    if sym_map.contains_key(&oid) {
                        let (bb, backedge) = sym_sub_world_bounds(
                            doc,
                            oid,
                            sym_map,
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

fn resolve_sym_world_path(
    doc: &Document,
    path_id: ObjectId,
    sym_ctx: Option<(&HashMap<ObjectId, &SceneObject>, Affine2D)>,
) -> Option<BezierPath> {
    if let Some((sym_map, sym_root_xf)) = sym_ctx {
        if let Some(sub) = sym_map.get(&path_id).copied() {
            let p_xf = sym_object_world_xf(path_id, &sym_root_xf, sym_map)?;
            return match &sub.payload {
                NodePayload::Path(p) => Some(p.transformed(&p_xf)),
                NodePayload::Shape(s) => Some(s.to_bezier_path().transformed(&p_xf)),
                NodePayload::PowerClip { container, .. } => Some(container.transformed(&p_xf)),
                _ => None,
            };
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

#[allow(clippy::too_many_arguments)]
fn object_ref_hits_point(
    doc: &Document,
    obj: &SceneObject,
    parent_xf: &Affine2D,
    pt_mm: Point2D,
    tol_mm: f64,
    sym_ctx: Option<(&HashMap<ObjectId, &SceneObject>, Affine2D)>,
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

    match &obj.payload {
        NodePayload::Path(p) => {
            let wp = p.transformed(&world_xf);
            path_hits_point(
                &wp,
                obj.fill.is_visible() || obj.lens.is_some(),
                pt_mm,
                stroke_tol,
            )
        }
        NodePayload::Shape(s) => {
            let wp = s.to_bezier_path().transformed(&world_xf);
            path_hits_point(
                &wp,
                obj.fill.is_visible() || obj.lens.is_some(),
                pt_mm,
                stroke_tol,
            )
        }
        NodePayload::PowerClip { container, .. } => {
            let wp = container.transformed(&world_xf);
            path_hits_point(&wp, true, pt_mm, stroke_tol)
        }
        NodePayload::Connector(c) => {
            let mut sym_visiting = HashMap::new();
            let mut sym_cache = HashMap::new();
            let ((a, b), _) = resolve_connector_endpoints_with_sym(
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
            path_hits_point(&route, false, pt_mm, stroke_tol)
        }
        NodePayload::Group(children) => children.iter().rev().any(|&cid| {
            let child_opt = sym_ctx
                .and_then(|(m, _)| m.get(&cid).copied())
                .or_else(|| doc.object(cid));
            child_opt.is_some_and(|child| {
                object_ref_hits_point(
                    doc,
                    child,
                    &world_xf,
                    pt_mm,
                    tol_mm,
                    sym_ctx,
                    visited_syms,
                    depth + 1,
                )
            })
        }),
        NodePayload::ArtisticText(at) => {
            let poly = BezierPath::from_rect(at.estimated_bounds()).transformed(&world_xf);
            path_hits_point(&poly, true, pt_mm, stroke_tol)
        }
        NodePayload::ParagraphText(para) => {
            let poly = BezierPath::from_rect(para.bounding_box()).transformed(&world_xf);
            path_hits_point(&poly, true, pt_mm, tol_mm)
        }
        NodePayload::Raster(r) => {
            let poly = BezierPath::from_rect(raster_local_rect(r)).transformed(&world_xf);
            path_hits_point(&poly, true, pt_mm, tol_mm)
        }
        NodePayload::Table(table) => {
            let poly = BezierPath::from_rect(table.to_bounding_box()).transformed(&world_xf);
            path_hits_point(&poly, true, pt_mm, stroke_tol)
        }
        NodePayload::Dimension(dim) => {
            let (strokes, label_rect) = dimension_local_geometry(dim, doc);
            if strokes.iter().any(|sp| {
                let wp = sp.transformed(&world_xf);
                path_hits_point(&wp, false, pt_mm, stroke_tol)
            }) {
                return true;
            }
            label_rect.is_some_and(|lr| {
                let poly = BezierPath::from_rect(lr).transformed(&world_xf);
                path_hits_point(&poly, true, pt_mm, tol_mm)
            })
        }
        NodePayload::TextOnPath(top) => {
            if let Some(host_path) = resolve_sym_world_path(doc, top.path_id, sym_ctx) {
                let font_h = top
                    .text
                    .runs
                    .iter()
                    .map(|r| r.style.font_size_mm())
                    .filter(|v| v.is_finite())
                    .fold(4.0_f64, f64::max);
                let band_tol = tol_mm + sanitize_f64(top.distance_mm, 0.0).abs() + font_h;
                path_hits_point(&host_path, false, pt_mm, band_tol)
            } else {
                let poly =
                    BezierPath::from_rect(top.text.estimated_bounds()).transformed(&world_xf);
                path_hits_point(&poly, true, pt_mm, stroke_tol)
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if !visited_syms.insert(*sym_id) {
                return false;
            }
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
                            pt_mm,
                            tol_mm,
                            Some((&child_map, world_xf)),
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
    rect_mm: Rect2D,
    sym_ctx: Option<(&HashMap<ObjectId, &SceneObject>, Affine2D)>,
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
            let wp = p.transformed(&world_xf);
            path_intersects_rect(&wp, obj.fill.is_visible() || obj.lens.is_some(), rect_mm)
        }
        NodePayload::Shape(s) => {
            let wp = s.to_bezier_path().transformed(&world_xf);
            path_intersects_rect(&wp, obj.fill.is_visible() || obj.lens.is_some(), rect_mm)
        }
        NodePayload::PowerClip { container, .. } => {
            let wp = container.transformed(&world_xf);
            path_intersects_rect(&wp, true, rect_mm)
        }
        NodePayload::Connector(c) => {
            let mut sym_visiting = HashMap::new();
            let mut sym_cache = HashMap::new();
            let ((a, b), _) = resolve_connector_endpoints_with_sym(
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
            path_intersects_rect(&route, false, rect_mm)
        }
        NodePayload::Group(children) => children.iter().any(|&cid| {
            let child_opt = sym_ctx
                .and_then(|(m, _)| m.get(&cid).copied())
                .or_else(|| doc.object(cid));
            child_opt.is_some_and(|child| {
                object_intersects_rect(
                    doc,
                    child,
                    &world_xf,
                    rect_mm,
                    sym_ctx,
                    visited_syms,
                    depth + 1,
                )
            })
        }),
        NodePayload::ArtisticText(at) => {
            let poly = BezierPath::from_rect(at.estimated_bounds()).transformed(&world_xf);
            path_intersects_rect(&poly, true, rect_mm)
        }
        NodePayload::ParagraphText(para) => {
            let poly = BezierPath::from_rect(para.bounding_box()).transformed(&world_xf);
            path_intersects_rect(&poly, true, rect_mm)
        }
        NodePayload::Raster(r) => {
            let poly = BezierPath::from_rect(raster_local_rect(r)).transformed(&world_xf);
            path_intersects_rect(&poly, true, rect_mm)
        }
        NodePayload::Table(table) => {
            let poly = BezierPath::from_rect(table.to_bounding_box()).transformed(&world_xf);
            path_intersects_rect(&poly, true, rect_mm)
        }
        NodePayload::Dimension(dim) => {
            let (strokes, label_rect) = dimension_local_geometry(dim, doc);
            if strokes
                .iter()
                .any(|sp| path_intersects_rect(&sp.transformed(&world_xf), false, rect_mm))
            {
                return true;
            }
            label_rect.is_some_and(|lr| {
                let poly = BezierPath::from_rect(lr).transformed(&world_xf);
                path_intersects_rect(&poly, true, rect_mm)
            })
        }
        NodePayload::TextOnPath(top) => {
            if let Some(host_path) = resolve_sym_world_path(doc, top.path_id, sym_ctx) {
                let font_h = top
                    .text
                    .runs
                    .iter()
                    .map(|r| r.style.font_size_mm())
                    .filter(|v| v.is_finite())
                    .fold(4.0_f64, f64::max);
                let margin = sanitize_f64(top.distance_mm, 0.0).abs() + font_h;
                path_intersects_rect(&host_path, false, rect_mm.expand(margin))
            } else {
                let poly =
                    BezierPath::from_rect(top.text.estimated_bounds()).transformed(&world_xf);
                path_intersects_rect(&poly, true, rect_mm)
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if !visited_syms.insert(*sym_id) {
                return false;
            }
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
                            rect_mm,
                            Some((&child_map, world_xf)),
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

fn path_hits_point(path: &BezierPath, check_fill: bool, pt: Point2D, stroke_tol: f64) -> bool {
    if check_fill && path.contains_point(pt) {
        return true;
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
/// and live interactive dynamic alignment guides.
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

        // Priority 1: Exact 2D node snapping (highest geometric precision)
        if options.nodes && page.is_some() {
            let index = doc.build_spatial_index(page_index);
            let candidates: Vec<ObjectId> = index
                .query_point(pt_mm, tol)
                .into_iter()
                .filter(|id| !exclude.contains(id))
                .collect();
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
