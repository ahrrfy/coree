// SPDX-License-Identifier: MIT OR Apache-2.0

//! Analytical DCEL Smart Fill vector enclosed-region detection engine.
//!
//! Builds an exact planar Doubly-Connected Edge List (`PlanarDcel`) from all
//! intersecting vector segments on a page, splits crossings analytically,
//! traces oriented half-edge cycles (`DcelFace`), resolves interior hole nesting,
//! and refits smooth cubic curves via [`fit_cubic_bezier_subpath`].

use coree_core::{
    fit_cubic_bezier_subpath, Affine2D, ArtisticMedia, BezierPath, Document, Fill,
    MasterLayerScope, NodePayload, ObjectId, OutlineStyle, Point2D, Rect2D, SceneObject,
};
use std::collections::{HashMap, HashSet};

const SNAP_EPS: f64 = 1e-4;
const SPLIT_T_EPS: f64 = 1e-7;
const AREA_EPS: f64 = 1e-5;

/// Vertex in the planar DCEL arrangement.
#[derive(Debug, Clone)]
pub struct DcelVertex {
    pub pos: Point2D,
    pub outgoing: Vec<usize>,
}

/// Directed half-edge in the planar DCEL arrangement.
#[derive(Debug, Clone)]
pub struct HalfEdge {
    pub origin: usize,
    pub dest: usize,
    pub twin: usize,
    pub next: usize,
    pub prev: usize,
    pub face: Option<usize>,
}

/// Enclosed planar face with an outer boundary loop and zero or more direct interior holes.
#[derive(Debug, Clone)]
pub struct DcelFace {
    pub id: usize,
    pub outer_points: Vec<Point2D>,
    pub holes: Vec<Vec<Point2D>>,
    pub signed_area: f64,
    pub bounds: Rect2D,
}

#[derive(Debug, Clone)]
struct TracedCycle {
    points: Vec<Point2D>,
    signed_area: f64,
    bounds: Rect2D,
    comp_id: usize,
}

/// Analytical Doubly-Connected Edge List built from intersecting planar vector segments.
#[derive(Debug, Clone, Default)]
pub struct PlanarDcel {
    pub vertices: Vec<DcelVertex>,
    pub half_edges: Vec<HalfEdge>,
    pub faces: Vec<DcelFace>,
    cw_shells: Vec<TracedCycle>,
}

impl PlanarDcel {
    /// Builds a planar DCEL arrangement from `paths`, splitting all segment intersections
    /// and tracing bounded CCW faces plus interior hole boundaries.
    #[must_use]
    pub fn from_paths(paths: &[BezierPath]) -> Self {
        const MAX_DCEL_COORD_MM: f64 = 1e12;
        let mut raw_segs: Vec<(Point2D, Point2D)> = Vec::new();
        for path in paths {
            for poly in path.flatten(0.025) {
                for win in poly.windows(2) {
                    let a = win[0];
                    let b = win[1];
                    if a.is_finite()
                        && b.is_finite()
                        && a.x.abs() <= MAX_DCEL_COORD_MM
                        && a.y.abs() <= MAX_DCEL_COORD_MM
                        && b.x.abs() <= MAX_DCEL_COORD_MM
                        && b.y.abs() <= MAX_DCEL_COORD_MM
                        && a.distance_to(b) > SNAP_EPS
                    {
                        raw_segs.push((a, b));
                    }
                }
            }
        }
        if raw_segs.is_empty() {
            return Self::default();
        }

        let seg_bbs: Vec<Rect2D> = raw_segs
            .iter()
            .map(|&(a, b)| Rect2D::from_points(a, b).expand(SNAP_EPS))
            .collect();
        let mut splits: Vec<Vec<f64>> = vec![vec![0.0, 1.0]; raw_segs.len()];

        for i in 0..raw_segs.len() {
            let (a0, a1) = raw_segs[i];
            let ba = seg_bbs[i];
            let (left, right) = splits.split_at_mut(i + 1);
            let sa = &mut left[i];
            for (rel_j, sb_j) in right.iter_mut().enumerate() {
                let j = i + 1 + rel_j;
                if !ba.intersects(seg_bbs[j]) {
                    continue;
                }
                let (b0, b1) = raw_segs[j];
                segment_intersections_params(a0, a1, b0, b1, sa, sb_j);
            }
        }

        let mut vertices: Vec<DcelVertex> = Vec::new();
        let mut grid_buckets: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        let intern_vertex = |p: Point2D,
                             verts: &mut Vec<DcelVertex>,
                             buckets: &mut HashMap<(i64, i64), Vec<usize>>|
         -> usize {
            let cell = 1.0 / (SNAP_EPS * 2.0);
            let gx = (p.x * cell).round() as i64;
            let gy = (p.y * cell).round() as i64;
            for dx in -1..=1_i64 {
                for dy in -1..=1_i64 {
                    if let (Some(bx), Some(by)) = (gx.checked_add(dx), gy.checked_add(dy)) {
                        if let Some(list) = buckets.get(&(bx, by)) {
                            for &vid in list {
                                if verts[vid].pos.distance_to(p) <= SNAP_EPS {
                                    return vid;
                                }
                            }
                        }
                    }
                }
            }
            let id = verts.len();
            verts.push(DcelVertex {
                pos: p,
                outgoing: Vec::new(),
            });
            buckets.entry((gx, gy)).or_default().push(id);
            id
        };

        let mut undirected_edges: HashSet<(usize, usize)> = HashSet::new();
        for (i, &(p0, p1)) in raw_segs.iter().enumerate() {
            let ts = &mut splits[i];
            ts.sort_by(f64::total_cmp);
            ts.dedup_by(|a, b| (*a - *b).abs() <= SPLIT_T_EPS);
            for win in ts.windows(2) {
                let pa = p0.lerp(p1, win[0].clamp(0.0, 1.0));
                let pb = p0.lerp(p1, win[1].clamp(0.0, 1.0));
                if pa.distance_to(pb) <= SNAP_EPS {
                    continue;
                }
                let u = intern_vertex(pa, &mut vertices, &mut grid_buckets);
                let v = intern_vertex(pb, &mut vertices, &mut grid_buckets);
                if u != v {
                    let key = if u < v { (u, v) } else { (v, u) };
                    undirected_edges.insert(key);
                }
            }
        }

        if undirected_edges.is_empty() {
            return Self::default();
        }

        // Prune degree <= 1 dangling hairs iteratively so enclosed cycles remain clean.
        let mut deg = vec![0usize; vertices.len()];
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); vertices.len()];
        for &(u, v) in &undirected_edges {
            adj[u].push(v);
            adj[v].push(u);
            deg[u] += 1;
            deg[v] += 1;
        }
        let mut removed_v = vec![false; vertices.len()];
        let mut prune_stack: Vec<usize> = (0..vertices.len()).filter(|&v| deg[v] <= 1).collect();
        while let Some(v) = prune_stack.pop() {
            if removed_v[v] || deg[v] > 1 {
                continue;
            }
            removed_v[v] = true;
            for &nb in &adj[v] {
                if !removed_v[nb] && deg[nb] > 0 {
                    deg[nb] -= 1;
                    if deg[nb] <= 1 {
                        prune_stack.push(nb);
                    }
                }
            }
        }

        let surviving_edges: Vec<(usize, usize)> = undirected_edges
            .into_iter()
            .filter(|&(u, v)| !removed_v[u] && !removed_v[v])
            .collect();
        if surviving_edges.is_empty() {
            return Self::default();
        }

        // Compute connected components on surviving graph for hole attribution.
        let mut comp_of_v = vec![usize::MAX; vertices.len()];
        let mut surv_adj: Vec<Vec<usize>> = vec![Vec::new(); vertices.len()];
        for &(u, v) in &surviving_edges {
            surv_adj[u].push(v);
            surv_adj[v].push(u);
        }
        let mut comp_count = 0usize;
        for v in 0..vertices.len() {
            if removed_v[v] || surv_adj[v].is_empty() || comp_of_v[v] != usize::MAX {
                continue;
            }
            let cid = comp_count;
            comp_count += 1;
            let mut stack = vec![v];
            comp_of_v[v] = cid;
            while let Some(cur) = stack.pop() {
                for &nb in &surv_adj[cur] {
                    if comp_of_v[nb] == usize::MAX {
                        comp_of_v[nb] = cid;
                        stack.push(nb);
                    }
                }
            }
        }

        let mut half_edges = Vec::with_capacity(surviving_edges.len() * 2);
        for &(u, v) in &surviving_edges {
            let fwd = half_edges.len();
            let rev = fwd + 1;
            half_edges.push(HalfEdge {
                origin: u,
                dest: v,
                twin: rev,
                next: 0,
                prev: 0,
                face: None,
            });
            half_edges.push(HalfEdge {
                origin: v,
                dest: u,
                twin: fwd,
                next: 0,
                prev: 0,
                face: None,
            });
            vertices[u].outgoing.push(fwd);
            vertices[v].outgoing.push(rev);
        }

        // Sort each vertex's outgoing half-edges CCW by angle.
        let v_positions: Vec<Point2D> = vertices.iter().map(|v| v.pos).collect();
        for (v_idx, v) in vertices.iter_mut().enumerate() {
            let vp = v_positions[v_idx];
            v.outgoing.sort_by(|&ea, &eb| {
                let da = v_positions[half_edges[ea].dest] - vp;
                let db = v_positions[half_edges[eb].dest] - vp;
                da.y.atan2(da.x).total_cmp(&db.y.atan2(db.x))
            });
        }

        let mut pos_in_outgoing = vec![0usize; half_edges.len()];
        for v in &vertices {
            for (idx, &he_id) in v.outgoing.iter().enumerate() {
                pos_in_outgoing[he_id] = idx;
            }
        }

        // Link `next` / `prev`: keeping the face on the LEFT chooses the CW-preceding outgoing edge from `twin`.
        for e_id in 0..half_edges.len() {
            let v_dest = half_edges[e_id].dest;
            let twin = half_edges[e_id].twin;
            let out = &vertices[v_dest].outgoing;
            let idx = pos_in_outgoing[twin];
            let next_idx = if idx == 0 { out.len() - 1 } else { idx - 1 };
            let next_e = out[next_idx];
            half_edges[e_id].next = next_e;
            half_edges[next_e].prev = e_id;
        }

        // Walk cycles.
        let mut faces = Vec::new();
        let mut cw_shells = Vec::new();
        let mut visited = vec![false; half_edges.len()];
        let max_steps = half_edges.len() + 2;

        for start_e in 0..half_edges.len() {
            if visited[start_e] {
                continue;
            }
            let mut cycle_edges = Vec::new();
            let mut cur = start_e;
            for _ in 0..max_steps {
                if visited[cur] {
                    break;
                }
                visited[cur] = true;
                cycle_edges.push(cur);
                cur = half_edges[cur].next;
                if cur == start_e {
                    break;
                }
            }
            if cur != start_e || cycle_edges.len() < 3 {
                continue;
            }
            let pts: Vec<Point2D> = cycle_edges
                .iter()
                .map(|&e| vertices[half_edges[e].origin].pos)
                .collect();
            let area = signed_polygon_area(&pts);
            let Some(bounds) = Rect2D::from_iter_points(pts.iter().copied()) else {
                continue;
            };
            let comp_id = comp_of_v[half_edges[start_e].origin];
            if area > AREA_EPS {
                let face_id = faces.len();
                for &e in &cycle_edges {
                    half_edges[e].face = Some(face_id);
                }
                faces.push(DcelFace {
                    id: face_id,
                    outer_points: pts,
                    holes: Vec::new(),
                    signed_area: area,
                    bounds,
                });
            } else if area < -AREA_EPS {
                cw_shells.push(TracedCycle {
                    points: pts,
                    signed_area: area,
                    bounds,
                    comp_id,
                });
            }
        }

        // Keep only the outermost CW shell per connected component (largest |area|) to represent holes inside outer faces.
        let mut outer_cw_by_comp: HashMap<usize, TracedCycle> = HashMap::new();
        for cyc in cw_shells {
            outer_cw_by_comp
                .entry(cyc.comp_id)
                .and_modify(|best| {
                    if cyc.signed_area.abs() > best.signed_area.abs() {
                        *best = cyc.clone();
                    }
                })
                .or_insert(cyc);
        }

        let retained_shells: Vec<TracedCycle> = outer_cw_by_comp.into_values().collect();
        // Attach each CW shell as a direct hole of the smallest-area CCW face strictly containing it.
        for shell in &retained_shells {
            let Some(&probe) = shell.points.first() else {
                continue;
            };
            let abs_shell_area = shell.signed_area.abs();
            let mut parent_face_idx: Option<usize> = None;
            let mut best_area = f64::INFINITY;
            for (fi, f) in faces.iter().enumerate() {
                if f.signed_area > abs_shell_area + AREA_EPS
                    && f.bounds.expand(SNAP_EPS).contains_point(probe)
                    && point_in_polygon(probe, &f.outer_points)
                    && f.signed_area < best_area
                {
                    best_area = f.signed_area;
                    parent_face_idx = Some(fi);
                }
            }
            if let Some(pfi) = parent_face_idx {
                faces[pfi].holes.push(shell.points.clone());
            }
        }

        Self {
            vertices,
            half_edges,
            faces,
            cw_shells: retained_shells,
        }
    }

    /// Finds the innermost bounded face containing `pt` (respecting interior holes) and
    /// converts its outer loop + direct hole loops into a fitted [`BezierPath`].
    #[must_use]
    pub fn face_path_containing(&self, pt: Point2D) -> Option<BezierPath> {
        if !pt.is_finite() {
            return None;
        }
        // Reject points directly on any half-edge boundary (mirrors barrier semantics)
        let on_edge_tol = 1e-4;
        for he in &self.half_edges {
            if he.origin < he.dest {
                let a = self.vertices[he.origin].pos;
                let b = self.vertices[he.dest].pos;
                if point_seg_dist(pt, a, b) <= on_edge_tol {
                    return None;
                }
            }
        }

        let mut best_face: Option<&DcelFace> = None;
        for f in &self.faces {
            if !f.bounds.contains_point(pt) || !point_in_polygon(pt, &f.outer_points) {
                continue;
            }
            let in_hole = f.holes.iter().any(|h| point_in_polygon(pt, h));
            if in_hole {
                continue;
            }
            if best_face.is_none_or(|cur| f.signed_area < cur.signed_area) {
                best_face = Some(f);
            }
        }
        let face = best_face?;

        // Extra guard against a disjoint nested CW shell whose interior isn't covered by any CCW face.
        // Require a strict area gap (accounting for forward/reverse summation ULP drift) so a loop's
        // own twin exterior CW shell is never misclassified as a nested hole.
        let area_tol = AREA_EPS.max(face.signed_area * 1e-10);
        for shell in &self.cw_shells {
            if shell.signed_area.abs() + area_tol < face.signed_area
                && shell.bounds.contains_point(pt)
                && point_in_polygon(pt, &shell.points)
                && !self.faces.iter().any(|sub_f| {
                    sub_f.signed_area < face.signed_area
                        && sub_f.bounds.contains_point(pt)
                        && point_in_polygon(pt, &sub_f.outer_points)
                })
            {
                return None;
            }
        }

        let mut subpaths = Vec::with_capacity(1 + face.holes.len());
        let outer_sp = fit_cubic_bezier_subpath(&face.outer_points, true, 0.025);
        if outer_sp.nodes.len() >= 3 {
            subpaths.push(outer_sp);
        }
        for hole in &face.holes {
            let hole_sp = fit_cubic_bezier_subpath(hole, true, 0.025);
            if hole_sp.nodes.len() >= 3 {
                subpaths.push(hole_sp);
            }
        }
        if subpaths.is_empty() {
            None
        } else {
            Some(BezierPath { subpaths })
        }
    }
}

/// Analytical planar DCEL Smart Fill directly over a collection of `paths`.
#[must_use]
pub fn dcel_smart_fill(paths: &[BezierPath], click_pt_mm: Point2D) -> Option<BezierPath> {
    if paths.is_empty() || !click_pt_mm.is_finite() {
        return None;
    }
    let dcel = PlanarDcel::from_paths(paths);
    dcel.face_path_containing(click_pt_mm)
}

/// Computes the enclosed vector region containing `click_pt_mm` formed by all
/// visible intersecting paths/shapes on `page_index`.
///
/// Returns `None` if `click_pt_mm` lies in open unbounded space or directly on an edge.
#[must_use]
pub fn compute_smart_fill(
    doc: &Document,
    page_index: usize,
    click_pt_mm: Point2D,
    _fill: Fill,
    _outline: OutlineStyle,
) -> Option<BezierPath> {
    if !click_pt_mm.is_finite() || page_index >= doc.pages().len() {
        return None;
    }
    let paths = collect_page_vector_paths(doc, page_index);
    dcel_smart_fill(&paths, click_pt_mm)
}

/// Convenience helper returning a ready-to-insert [`SceneObject`] from [`compute_smart_fill`].
#[must_use]
pub fn create_smart_fill_object(
    doc: &Document,
    page_index: usize,
    click_pt_mm: Point2D,
    fill: Fill,
    outline: OutlineStyle,
) -> Option<SceneObject> {
    let path = compute_smart_fill(doc, page_index, click_pt_mm, fill.clone(), outline.clone())?;
    Some(
        SceneObject::new(ObjectId(0), "Smart Fill", NodePayload::Path(path))
            .with_fill(fill)
            .with_outline(outline),
    )
}

fn segment_intersections_params(
    a0: Point2D,
    a1: Point2D,
    b0: Point2D,
    b1: Point2D,
    ta_out: &mut Vec<f64>,
    tb_out: &mut Vec<f64>,
) {
    let da = a1 - a0;
    let db = b1 - b0;
    let denom = da.x * db.y - da.y * db.x;
    let d = b0 - a0;
    if denom.abs() > 1e-9 {
        let t = (d.x * db.y - d.y * db.x) / denom;
        let u = (d.x * da.y - d.y * da.x) / denom;
        if (-SPLIT_T_EPS..=1.0 + SPLIT_T_EPS).contains(&t)
            && (-SPLIT_T_EPS..=1.0 + SPLIT_T_EPS).contains(&u)
        {
            ta_out.push(t.clamp(0.0, 1.0));
            tb_out.push(u.clamp(0.0, 1.0));
        }
        return;
    }

    // Collinear overlap check
    let da_len_sq = da.x * da.x + da.y * da.y;
    let db_len_sq = db.x * db.x + db.y * db.y;
    if da_len_sq <= 1e-18 || db_len_sq <= 1e-18 {
        return;
    }
    let cross = d.x * da.y - d.y * da.x;
    if cross * cross > SNAP_EPS * SNAP_EPS * da_len_sq {
        return;
    }
    for &p in &[b0, b1] {
        let t = (p - a0).dot(da) / da_len_sq;
        if t > SPLIT_T_EPS && t < 1.0 - SPLIT_T_EPS {
            ta_out.push(t);
        }
    }
    for &p in &[a0, a1] {
        let u = (p - b0).dot(db) / db_len_sq;
        if u > SPLIT_T_EPS && u < 1.0 - SPLIT_T_EPS {
            tb_out.push(u);
        }
    }
}

fn signed_polygon_area(pts: &[Point2D]) -> f64 {
    if pts.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    let n = pts.len();
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        sum += a.x * b.y - b.x * a.y;
    }
    0.5 * sum
}

fn point_in_polygon(pt: Point2D, poly: &[Point2D]) -> bool {
    let mut inside = false;
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let a = poly[i];
        let b = poly[j];
        let crosses = ((a.y > pt.y) != (b.y > pt.y))
            && (pt.x < (b.x - a.x) * (pt.y - a.y) / (b.y - a.y) + a.x);
        if crosses {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn point_seg_dist(p: Point2D, a: Point2D, b: Point2D) -> f64 {
    let ab = b - a;
    let len_sq = ab.x * ab.x + ab.y * ab.y;
    if len_sq <= 1e-18 {
        return p.distance_to(a);
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    p.distance_to(a + ab * t)
}

fn collect_page_vector_paths(doc: &Document, page_index: usize) -> Vec<BezierPath> {
    let mut out = Vec::new();
    let mut seen_layers = HashSet::new();
    let is_odd_page = page_index.is_multiple_of(2); // 1-based Page 1 is odd

    // Master layers applicable to this page
    for page in doc.pages() {
        for layer in &page.layers {
            if !layer.visible {
                continue;
            }
            let applies = match layer.master_scope {
                MasterLayerScope::None => false,
                MasterLayerScope::AllPages => true,
                MasterLayerScope::OddPages => is_odd_page,
                MasterLayerScope::EvenPages => !is_odd_page,
            };
            if applies && seen_layers.insert(layer.id) {
                for &oid in &layer.objects {
                    collect_object_paths(doc, oid, &Affine2D::IDENTITY, 0, &mut out);
                }
            }
        }
    }

    // Active page local layers (non-master)
    if let Some(page) = doc.page(page_index) {
        for layer in &page.layers {
            if layer.visible
                && layer.master_scope == MasterLayerScope::None
                && seen_layers.insert(layer.id)
            {
                for &oid in &layer.objects {
                    collect_object_paths(doc, oid, &Affine2D::IDENTITY, 0, &mut out);
                }
            }
        }
    }
    out
}

fn apply_persp_then_xf(path: BezierPath, obj: &SceneObject, world_xf: &Affine2D) -> BezierPath {
    let p = if let Some(persp) = obj.perspective {
        persp.transform_bezier_path(&path)
    } else {
        path
    };
    p.transformed(world_xf)
}

fn collect_object_paths(
    doc: &Document,
    id: ObjectId,
    parent_xf: &Affine2D,
    depth: usize,
    out: &mut Vec<BezierPath>,
) {
    if depth > 64 {
        return;
    }
    let Some(obj) = doc.object(id) else {
        return;
    };
    if !obj.visible {
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    let resolved_payload = if matches!(obj.payload, NodePayload::Clone { .. }) {
        doc.resolve_clone_payload_and_style(obj).map(|(p, _, _)| p)
    } else {
        Some(obj.payload.clone())
    };
    let Some(payload) = resolved_payload else {
        return;
    };
    match &payload {
        NodePayload::Path(p) => out.push(apply_persp_then_xf(p.clone(), obj, &world_xf)),
        NodePayload::Shape(s) => {
            out.push(apply_persp_then_xf(s.to_bezier_path(), obj, &world_xf));
        }
        NodePayload::BSpline(bs) => {
            out.push(apply_persp_then_xf(bs.to_bezier_path(), obj, &world_xf));
        }
        NodePayload::ArtisticMedia { spine, style } => {
            out.push(apply_persp_then_xf(
                ArtisticMedia::expand_stroke(spine, style),
                obj,
                &world_xf,
            ));
        }
        NodePayload::PowerClip { container, .. } => {
            out.push(apply_persp_then_xf((**container).clone(), obj, &world_xf));
        }
        NodePayload::Table(t) => out.push(apply_persp_then_xf(t.to_grid_paths(), obj, &world_xf)),
        NodePayload::Connector(c) => {
            let (a, b) = doc.resolve_connector_endpoints_with_transform(c, &world_xf);
            out.push(c.route_path(a, b));
        }
        NodePayload::Group(children) => {
            for &cid in children {
                collect_object_paths(doc, cid, &world_xf, depth + 1, out);
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = doc.symbol(*sym_id) {
                for sub in &sym.objects {
                    if !sub.visible {
                        continue;
                    }
                    let sub_xf = world_xf.compose(&sub.transform);
                    match &sub.payload {
                        NodePayload::Path(p) => {
                            out.push(apply_persp_then_xf(p.clone(), sub, &sub_xf));
                        }
                        NodePayload::Shape(s) => {
                            out.push(apply_persp_then_xf(s.to_bezier_path(), sub, &sub_xf));
                        }
                        NodePayload::BSpline(bs) => {
                            out.push(apply_persp_then_xf(bs.to_bezier_path(), sub, &sub_xf));
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dcel_smart_fill_overlapping_shapes_annulus_hole_and_unbounded() {
        // 1. Overlapping square [0,40]^2 + square [20,60]x[0,40] => lens/overlap pocket [20,40]x[0,40]
        let s1 = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 40.0, 40.0));
        let s2 = BezierPath::from_rect(Rect2D::from_xywh(20.0, 0.0, 40.0, 40.0));
        let pocket = dcel_smart_fill(&[s1, s2], Point2D::new(30.0, 20.0)).expect("overlap pocket");
        let bb = pocket.bounding_box().unwrap();
        assert!((bb.min_x - 20.0).abs() < 0.05 && (bb.max_x - 40.0).abs() < 0.05);
        assert!((bb.width() - 20.0).abs() < 0.1 && (bb.height() - 40.0).abs() < 0.1);

        // 2. Unbounded click outside returns None
        let outer = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 100.0, 100.0));
        let hole = BezierPath::from_rect(Rect2D::from_xywh(25.0, 25.0, 50.0, 50.0));
        assert!(
            dcel_smart_fill(&[outer.clone(), hole.clone()], Point2D::new(150.0, 50.0)).is_none()
        );

        // 3. Annulus click between outer and inner yields outer + hole subpaths
        let ring = dcel_smart_fill(&[outer, hole], Point2D::new(10.0, 10.0)).expect("annulus");
        assert_eq!(ring.subpaths.len(), 2);
        let outer_bb = BezierPath::from_subpath(ring.subpaths[0].clone())
            .bounding_box()
            .unwrap();
        let inner_bb = BezierPath::from_subpath(ring.subpaths[1].clone())
            .bounding_box()
            .unwrap();
        let area_approx =
            outer_bb.width() * outer_bb.height() - inner_bb.width() * inner_bb.height();
        assert!((area_approx - 7500.0).abs() < 50.0);
    }

    #[test]
    fn dcel_smart_fill_rejects_extreme_coordinates_without_overflow_panic() {
        let pos_rect = BezierPath::from_rect(Rect2D::from_xywh(1e16, 1e16, 10.0, 10.0));
        assert!(dcel_smart_fill(&[pos_rect], Point2D::new(1e16 + 5.0, 1e16 + 5.0)).is_none());

        let neg_rect = BezierPath::from_rect(Rect2D::from_xywh(-1e16, -1e16, 10.0, 10.0));
        assert!(dcel_smart_fill(&[neg_rect], Point2D::new(-1e16 + 5.0, -1e16 + 5.0)).is_none());
    }

    #[test]
    fn dcel_smart_fill_standalone_circles_cw_and_ccw_radius_sweep() {
        use coree_core::{EllipseMode, ParametricShape};

        let center = Point2D::new(100.0, 100.0);
        for r_int in 5..=50 {
            let r = f64::from(r_int);
            let shape = ParametricShape::Ellipse {
                rx: r,
                ry: r,
                start_deg: 0.0,
                end_deg: 360.0,
                mode: EllipseMode::Full,
            };
            let cw_path = shape
                .to_bezier_path()
                .transformed(&Affine2D::translate(center.x, center.y));
            let cw_fill = dcel_smart_fill(std::slice::from_ref(&cw_path), center);
            assert!(
                cw_fill.is_some(),
                "CW standalone circle r={r} failed dcel_smart_fill"
            );
            let cw_bb = cw_fill.unwrap().bounding_box().unwrap();
            assert!(
                (cw_bb.width() - 2.0 * r).abs() < 0.2 && (cw_bb.height() - 2.0 * r).abs() < 0.2,
                "CW circle r={r} bbox mismatch: {cw_bb:?}"
            );

            let mut ccw_path = cw_path;
            ccw_path.reverse_subpath(0).unwrap();
            let ccw_fill = dcel_smart_fill(&[ccw_path], center);
            assert!(
                ccw_fill.is_some(),
                "CCW standalone circle r={r} failed dcel_smart_fill"
            );
        }
    }
}
