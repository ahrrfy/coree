// SPDX-License-Identifier: MPL-2.0

//! Spatial R-Tree accelerated hit-testing (`hit_test_point`, `hit_test_rect`,
//! `hit_test_node`) and intelligent multi-target [`SnapEngine`] emitting
//! [`DynamicGuideLine`] alignment overlays.

use coree_core::geometry::point_to_segment_distance;
use coree_core::math::sanitize_f64;
use coree_core::{
    Affine2D, BezierPath, Document, GuideOrientation, NodePayload, NodeRef, ObjectId, Point2D,
    Rect2D, Vec2D,
};
use std::collections::HashSet;

/// Hit result identifying a specific Bézier anchor node on a scene object.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeHit {
    pub node: NodeRef,
    pub position_mm: Point2D,
    pub distance_mm: f64,
}

/// Returns the topmost visible, unlocked scene object hit at `pt_mm` within `tolerance_mm`
/// on `page_index`, accelerated by `coree_core::SpatialIndex` R-Tree culling.
///
/// Objects residing on locked or hidden layers are skipped.
#[must_use]
pub fn hit_test_point(
    doc: &Document,
    page_index: usize,
    pt_mm: Point2D,
    tolerance_mm: f64,
) -> Option<ObjectId> {
    if !pt_mm.is_finite() || page_index >= doc.pages.len() {
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
        let Some(obj) = doc.objects.get(&oid) else {
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
    if rect_mm.is_empty() || page_index >= doc.pages.len() {
        return Vec::new();
    }
    let blocked = locked_or_hidden_layer_objects(doc, page_index);
    let index = doc.build_spatial_index(page_index);
    let mut hits = Vec::new();
    for oid in index.query_rect(rect_mm) {
        if blocked.contains(&oid) {
            continue;
        }
        let Some(obj) = doc.objects.get(&oid) else {
            continue;
        };
        if !obj.visible || obj.locked {
            continue;
        }
        if fully_enclosed {
            if let Some(bb) = doc.object_bounds(oid) {
                if rect_mm.contains_rect(bb) {
                    hits.push(oid);
                }
            }
        } else {
            hits.push(oid);
        }
    }
    hits
}

/// Collects all top-level object IDs on locked or invisible layers (including master layers)
/// applicable to `page_index`.
fn locked_or_hidden_layer_objects(doc: &Document, page_index: usize) -> HashSet<ObjectId> {
    let mut blocked = HashSet::new();
    for page in &doc.pages {
        for layer in &page.layers {
            if layer.master_scope != coree_core::MasterLayerScope::None
                && (layer.locked || !layer.visible)
            {
                blocked.extend(layer.objects.iter().copied());
            }
        }
    }
    if let Some(page) = doc.pages.get(page_index) {
        for layer in &page.layers {
            if layer.locked || !layer.visible {
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
    if depth > 64 {
        return false;
    }
    let Some(obj) = doc.objects.get(&id) else {
        return false;
    };
    if !obj.visible {
        return false;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    let stroke_tol = tol_mm + obj.outline.effective_width_mm() * 0.5;

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
            let (a, b) = doc.resolve_connector_endpoints_with_transform(c, &world_xf);
            let route = c.route_path(a, b);
            path_hits_point(&route, false, pt_mm, stroke_tol)
        }
        NodePayload::Group(children) => children
            .iter()
            .rev()
            .any(|&cid| object_hits_point(doc, cid, &world_xf, pt_mm, tol_mm, depth + 1)),
        NodePayload::ArtisticText(_)
        | NodePayload::ParagraphText(_)
        | NodePayload::TextOnPath(_)
        | NodePayload::Raster(_)
        | NodePayload::Table(_)
        | NodePayload::Dimension(_)
        | NodePayload::SymbolInstance(_) => doc.object_bounds(id).is_some_and(|local_bb| {
            // `doc.object_bounds(id)` includes `obj.transform` but NOT ancestor `parent_xf`.
            if let Some(inv_parent) = parent_xf.inverse() {
                let local_pt = inv_parent.transform_point(pt_mm);
                let sx = parent_xf.m[0].hypot(parent_xf.m[1]);
                let sy = parent_xf.m[2].hypot(parent_xf.m[3]);
                let scale = (0.5 * (sx + sy)).max(1e-6);
                local_bb.expand(tol_mm / scale).contains_point(local_pt)
            } else {
                parent_xf
                    .try_transform_rect(local_bb)
                    .is_some_and(|world_bb| world_bb.expand(tol_mm).contains_point(pt_mm))
            }
        }),
    }
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
        let page = doc.pages.get(page_index);
        let page_rect = page
            .map(coree_core::Page::trim_rect)
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
        if options.objects {
            if let Some(pg) = page {
                for layer in &pg.layers {
                    if !layer.visible {
                        continue;
                    }
                    for &oid in &layer.objects {
                        if exclude.contains(&oid) {
                            continue;
                        }
                        let Some(obj) = doc.objects.get(&oid) else {
                            continue;
                        };
                        if !obj.visible {
                            continue;
                        }
                        let Some(bb) = doc.object_bounds(oid) else {
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
            for guide in &doc.guides {
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
            let subs = f64::from(doc.grid.subdivisions.max(1));
            let step_x = sanitize_f64(doc.grid.spacing_x_mm, 10.0).max(0.1) / subs;
            let step_y = sanitize_f64(doc.grid.spacing_y_mm, 10.0).max(0.1) / subs;
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
