// SPDX-License-Identifier: MPL-2.0

//! Smart Fill vector enclosed-region detection engine.
//!
//! Rasterizes all visible boundary edges on a page into a high-resolution
//! supercover barrier grid around the clicked region, flood-fills the enclosed
//! pocket containing `click_pt_mm`, and traces the exact boundary back into a
//! clean smooth vector [`BezierPath`].

use crate::trace::{extract_marching_squares_contours, fit_smooth_subpath, rdp_simplify_closed};
use coree_core::{
    Affine2D, BezierPath, Document, Fill, MasterLayerScope, NodePayload, ObjectId, OutlineStyle,
    Point2D, Rect2D, SceneObject,
};
use std::collections::{HashSet, VecDeque};

/// Target grid resolution along the longest axis of the active Smart Fill region.
const SMART_FILL_GRID_MAX: usize = 640;

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
    if !click_pt_mm.is_finite() || page_index >= doc.pages.len() {
        return None;
    }

    let paths = collect_page_vector_paths(doc, page_index);
    if paths.is_empty() {
        return None;
    }

    // Compute tight bounding box of all vector curves on the page plus the click point
    let mut bounds: Option<Rect2D> = None;
    for path in &paths {
        if let Some(bb) = path.bounding_box() {
            if !bb.is_finite() {
                continue;
            }
            bounds = Some(match bounds {
                Some(prev) => prev.union(bb),
                None => bb,
            });
        }
    }
    let bounds = bounds?;
    if !bounds.is_finite() || !bounds.expand(2.0).contains_point(click_pt_mm) {
        return None;
    }

    // Add a clear exterior margin around all artwork so unbounded space always leaks to border
    let margin = (bounds.width().max(bounds.height()) * 0.08).clamp(2.0, 15.0);
    let region_rect = bounds.expand(margin);
    let rw = region_rect.width();
    let rh = region_rect.height();
    if !region_rect.is_finite() || !rw.is_finite() || !rh.is_finite() || rw <= 1e-6 || rh <= 1e-6 {
        return None;
    }

    let max_dim = rw.max(rh);
    let px_per_mm = (SMART_FILL_GRID_MAX as f64) / max_dim;
    let gw = ((rw * px_per_mm).ceil() as usize).clamp(32, SMART_FILL_GRID_MAX + 4);
    let gh = ((rh * px_per_mm).ceil() as usize).clamp(32, SMART_FILL_GRID_MAX + 4);
    let mm_per_px = 1.0 / px_per_mm;

    let mut barrier = vec![false; gw * gh];
    let flatten_tol = (mm_per_px * 0.25).clamp(0.01, 0.2);

    for path in &paths {
        for poly in path.flatten(flatten_tol) {
            for win in poly.windows(2) {
                if !win[0].is_finite() || !win[1].is_finite() {
                    continue;
                }
                let x0 = ((win[0].x - region_rect.min_x) * px_per_mm)
                    .floor()
                    .clamp(-1.0, gw as f64) as i32;
                let y0 = ((win[0].y - region_rect.min_y) * px_per_mm)
                    .floor()
                    .clamp(-1.0, gh as f64) as i32;
                let x1 = ((win[1].x - region_rect.min_x) * px_per_mm)
                    .floor()
                    .clamp(-1.0, gw as f64) as i32;
                let y1 = ((win[1].y - region_rect.min_y) * px_per_mm)
                    .floor()
                    .clamp(-1.0, gh as f64) as i32;
                draw_supercover_line(&mut barrier, gw, gh, x0, y0, x1, y1);
            }
        }
    }

    let sx_i = ((click_pt_mm.x - region_rect.min_x) * px_per_mm).floor() as i32;
    let sy_i = ((click_pt_mm.y - region_rect.min_y) * px_per_mm).floor() as i32;
    if sx_i <= 0 || sy_i <= 0 || sx_i >= (gw as i32 - 1) || sy_i >= (gh as i32 - 1) {
        return None;
    }
    let sx = sx_i as usize;
    let sy = sy_i as usize;
    if barrier[sy * gw + sx] {
        return None;
    }

    // 4-connected BFS flood fill; abort if it reaches the outer border (unbounded open space)
    let mut filled = vec![false; gw * gh];
    let mut queue = VecDeque::new();
    filled[sy * gw + sx] = true;
    queue.push_back((sx, sy));
    let mut area_px = 0usize;

    while let Some((x, y)) = queue.pop_front() {
        if x == 0 || y == 0 || x + 1 >= gw || y + 1 >= gh {
            return None;
        }
        area_px += 1;
        let neighbors = [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)];
        for (nx, ny) in neighbors {
            let idx = ny * gw + nx;
            if !filled[idx] && !barrier[idx] {
                filled[idx] = true;
                queue.push_back((nx, ny));
            }
        }
    }

    if area_px < 4 {
        return None;
    }

    // Extract exact Marching Squares contours (handles both outer loop and interior holes)
    let loops = extract_marching_squares_contours(&filled, gw, gh);
    if loops.is_empty() {
        return None;
    }

    let rdp_eps_px = 0.85;
    let mut subpaths = Vec::new();
    for ring in loops {
        if ring.len() < 3 {
            continue;
        }
        let simplified = rdp_simplify_closed(&ring, rdp_eps_px);
        if simplified.len() < 3 {
            continue;
        }
        let world_pts: Vec<Point2D> = simplified
            .into_iter()
            .map(|p| {
                Point2D::new(
                    region_rect.min_x + p.x * mm_per_px,
                    region_rect.min_y + p.y * mm_per_px,
                )
            })
            .collect();
        let sp = fit_smooth_subpath(&world_pts, true, 0.35, 50.0);
        if sp.nodes.len() >= 3 {
            subpaths.push(sp);
        }
    }

    if subpaths.is_empty() {
        None
    } else {
        Some(BezierPath { subpaths })
    }
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

/// Draws a 4-connected supercover line so diagonal edges never leak a 4-connected flood fill.
/// Clamps endpoints safely to grid bounds in `i64` so extreme coordinates never overflow or hang.
fn draw_supercover_line(grid: &mut [bool], w: usize, h: usize, x0: i32, y0: i32, x1: i32, y1: i32) {
    if w == 0 || h == 0 {
        return;
    }
    let max_x = w as i64;
    let max_y = h as i64;
    let mut x = i64::from(x0).clamp(-1, max_x);
    let mut y = i64::from(y0).clamp(-1, max_y);
    let end_x = i64::from(x1).clamp(-1, max_x);
    let end_y = i64::from(y1).clamp(-1, max_y);

    let mut set = |px: i64, py: i64| {
        if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
            grid[(py as usize) * w + (px as usize)] = true;
        }
    };

    let dx = (end_x - x).abs();
    let dy = (end_y - y).abs();
    let sx = if x < end_x { 1_i64 } else { -1_i64 };
    let sy = if y < end_y { 1_i64 } else { -1_i64 };

    set(x, y);
    if dx >= dy {
        let mut err = dx / 2;
        while x != end_x {
            x += sx;
            err -= dy;
            if err < 0 {
                // Mark intermediate corner pixel for strict 4-connected supercover seal
                set(x, y);
                y += sy;
                err += dx;
            }
            set(x, y);
        }
    } else {
        let mut err = dy / 2;
        while y != end_y {
            y += sy;
            err -= dx;
            if err < 0 {
                set(x, y);
                x += sx;
                err += dy;
            }
            set(x, y);
        }
    }
}

fn collect_page_vector_paths(doc: &Document, page_index: usize) -> Vec<BezierPath> {
    let mut out = Vec::new();
    let mut seen_layers = HashSet::new();
    let is_odd_page = page_index.is_multiple_of(2); // 1-based Page 1 is odd

    // Master layers applicable to this page
    for page in &doc.pages {
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
    if let Some(page) = doc.pages.get(page_index) {
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
    let Some(obj) = doc.objects.get(&id) else {
        return;
    };
    if !obj.visible {
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    match &obj.payload {
        NodePayload::Path(p) => out.push(p.transformed(&world_xf)),
        NodePayload::Shape(s) => out.push(s.to_bezier_path().transformed(&world_xf)),
        NodePayload::PowerClip { container, .. } => out.push(container.transformed(&world_xf)),
        NodePayload::Table(t) => out.push(t.to_grid_paths().transformed(&world_xf)),
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
            if let Some(sym) = doc.symbols.get(sym_id) {
                for sub in &sym.objects {
                    if !sub.visible {
                        continue;
                    }
                    let sub_xf = world_xf.compose(&sub.transform);
                    match &sub.payload {
                        NodePayload::Path(p) => out.push(p.transformed(&sub_xf)),
                        NodePayload::Shape(s) => {
                            out.push(s.to_bezier_path().transformed(&sub_xf));
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}
