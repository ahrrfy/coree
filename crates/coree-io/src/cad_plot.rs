// SPDX-License-Identifier: MIT OR Apache-2.0

//! CAD & cutting-plotter importers and exporters:
//! - AutoCAD ASCII **DXF** (`R12` / `R2000+` `AC1015` with `HEADER`, `$INSUNITS`, `TABLES`/`LAYER`,
//!   `BLOCKS`+`INSERT`, `LINE`, `LWPOLYLINE` with bulge arcs, `POLYLINE`/`VERTEX`/`SEQEND`,
//!   `CIRCLE`, `ARC`, `ELLIPSE`, `SPLINE`, `SOLID`, `TEXT`/`MTEXT`) for CNC routers & laser cutters.
//! - **PLT (HPGL)** (`IN`, `IP`, `SC`, `SP`, `PU`, `PD`, `PA`, `PR`, `AA`, `AR`, `CI`, `EA`,
//!   `ER`, `PW`, `LB` at `1 mm = 40 HPGL units`) for Roland / Graphtec vinyl cutting plotters.

use crate::atomic::atomic_write_bytes;
use crate::error::{IoError, IoResult, DEFAULT_MAX_SVG_BYTES};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, Color, ColorMode, Document, EllipseMode, Fill, Layer,
    LayerId, MasterLayerScope, NodePayload, ObjectId, OutlineStyle, Page, ParametricShape, Point2D,
    Rect2D, SceneObject, TextDirection, TextStyle, Vec2D,
};
use kurbo::{BezPath, PathSeg};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

/// Standard HPGL plotter resolution (`1 mm = 40 HPGL units`, i.e., `0.025 mm/step`).
pub const HPGL_UNITS_PER_MM: f64 = 40.0;
const MAX_CAD_DEPTH: usize = 64;
const MAX_DXF_INSERT_DEPTH: usize = 32;
const MAX_DXF_EXPANDED_ENTITIES: usize = 250_000;
const MAX_DXF_EXPAND_STEPS: usize = 1_000_000;
const MAX_CAD_INPUT_BYTES: usize = DEFAULT_MAX_SVG_BYTES * 2; // 128 MiB

/// Options controlling AutoCAD ASCII DXF export.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DxfExportOptions {
    /// When `true`, flattens cubic Bézier curves into `LWPOLYLINE` chords instead of `SPLINE`.
    pub flatten_curves: bool,
    /// Chord tolerance in millimeters when flattening curves.
    pub flatten_tolerance_mm: f64,
    /// When `true`, exports only layers marked `is_cut_layer = true` (e.g. `CutContour`).
    pub cut_layers_only: bool,
}

impl Default for DxfExportOptions {
    fn default() -> Self {
        Self {
            flatten_curves: false,
            flatten_tolerance_mm: 0.05,
            cut_layers_only: false,
        }
    }
}

/// Options controlling HPGL `.plt` cutting plotter export.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PltExportOptions {
    /// Chord tolerance in millimeters for flattening cubic curves into plotter steps.
    pub flatten_tolerance_mm: f64,
    /// When `true`, exports only layers marked `is_cut_layer = true`.
    pub cut_layers_only: bool,
}

impl Default for PltExportOptions {
    fn default() -> Self {
        Self {
            flatten_tolerance_mm: 0.05,
            cut_layers_only: false,
        }
    }
}

/// Exports `page_index` of `doc` as an AutoCAD ASCII DXF string in millimeters.
pub fn export_dxf_to_string(doc: &Document, page_index: usize) -> IoResult<String> {
    export_dxf_to_string_with_options(doc, page_index, &DxfExportOptions::default())
}

/// Exports `page_index` of `doc` as an AutoCAD ASCII DXF string with explicit options.
pub fn export_dxf_to_string_with_options(
    doc: &Document,
    page_index: usize,
    opts: &DxfExportOptions,
) -> IoResult<String> {
    let page = doc.page(page_index).ok_or(IoError::PageOutOfRange {
        page_index,
        page_count: doc.pages().len(),
    })?;

    let layers = collect_page_layers(doc, page_index, opts.cut_layers_only);

    let mut out = String::with_capacity(4096);

    // 1. HEADER section (AC1015 / R2000, millimeter units $INSUNITS = 4)
    out.push_str("0\nSECTION\n2\nHEADER\n");
    out.push_str("9\n$ACADVER\n1\nAC1015\n");
    out.push_str("9\n$INSUNITS\n70\n4\n"); // 4 = Millimeters
    out.push_str("9\n$MEASUREMENT\n70\n1\n"); // 1 = Metric
    let page_w = if page.width_mm.is_finite() && page.width_mm > 0.0 {
        page.width_mm
    } else {
        210.0
    };
    let page_h = if page.height_mm.is_finite() && page.height_mm > 0.0 {
        page.height_mm
    } else {
        297.0
    };
    let _ = writeln!(out, "9\n$EXTMIN\n10\n0.0\n20\n0.0\n30\n0.0");
    let _ = writeln!(out, "9\n$EXTMAX\n10\n{page_w:.4}\n20\n{page_h:.4}\n30\n0.0");
    out.push_str("0\nENDSEC\n");

    // 2. TABLES section with LAYER definitions (always includes layer `0` and all page layers)
    let mut layer_defs: Vec<(String, i32)> = vec![("0".to_owned(), 7)];
    let mut seen_layer_names: HashSet<String> = HashSet::new();
    seen_layer_names.insert("0".to_owned());

    for layer in &layers {
        let safe = sanitize_dxf_layer_name(&layer.name);
        if seen_layer_names.insert(safe.clone()) {
            let aci_color = if layer.is_cut_layer || safe.eq_ignore_ascii_case("CutContour") {
                6 // Magenta (standard laser/vinyl CutContour ACI)
            } else {
                7 // Default White/Black
            };
            layer_defs.push((safe, aci_color));
        }
    }
    // Always guarantee a `CutContour` layer table entry exists for CNC/laser tooling compatibility
    if seen_layer_names.insert("CutContour".to_owned()) {
        layer_defs.push(("CutContour".to_owned(), 6));
    }

    out.push_str("0\nSECTION\n2\nTABLES\n");
    out.push_str("0\nTABLE\n2\nLAYER\n");
    let _ = writeln!(out, "70\n{}", layer_defs.len());
    for (name, aci) in &layer_defs {
        out.push_str("0\nLAYER\n");
        let _ = writeln!(out, "2\n{name}");
        out.push_str("70\n0\n");
        let _ = writeln!(out, "62\n{aci}");
        out.push_str("6\nCONTINUOUS\n");
    }
    out.push_str("0\nENDTAB\n");
    out.push_str("0\nENDSEC\n");

    // 3. ENTITIES section
    out.push_str("0\nSECTION\n2\nENTITIES\n");
    for layer in &layers {
        let layer_name = sanitize_dxf_layer_name(&layer.name);
        let mut visited = HashSet::new();
        for &oid in &layer.objects {
            if let Some(obj) = doc.object(oid) {
                emit_dxf_object_entities(
                    doc,
                    page,
                    obj,
                    Affine2D::IDENTITY,
                    &layer_name,
                    opts,
                    &mut out,
                    0,
                    &mut visited,
                );
            }
        }
    }
    out.push_str("0\nENDSEC\n");
    out.push_str("0\nEOF\n");
    Ok(out)
}

/// Atomically exports `page_index` of `doc` to a DXF file at `path`.
pub fn export_dxf(doc: &Document, page_index: usize, path: &Path) -> IoResult<()> {
    export_dxf_with_options(doc, page_index, &DxfExportOptions::default(), path)
}

/// Atomically exports `page_index` of `doc` to a DXF file at `path` with options.
pub fn export_dxf_with_options(
    doc: &Document,
    page_index: usize,
    opts: &DxfExportOptions,
    path: &Path,
) -> IoResult<()> {
    let dxf = export_dxf_to_string_with_options(doc, page_index, opts)?;
    atomic_write_bytes(path, dxf.as_bytes())
}

#[allow(clippy::too_many_arguments)]
fn emit_dxf_object_entities(
    doc: &Document,
    page: &Page,
    obj: &SceneObject,
    parent_xf: Affine2D,
    layer_name: &str,
    opts: &DxfExportOptions,
    out: &mut String,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) {
    if depth > MAX_CAD_DEPTH || !obj.visible || !visited.insert(obj.id) {
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        visited.remove(&obj.id);
        return;
    }

    // Native exact CAD CIRCLE & ARC export when shape is a uniformly-scaled circular arc/circle
    // (avoids segmented/splined circles on CNC, plasma & laser cutters).
    if obj.effects.is_empty() {
        if let NodePayload::Shape(ParametricShape::Ellipse {
            rx,
            ry,
            mode,
            start_deg,
            end_deg,
        }) = &obj.payload
        {
            if try_emit_exact_dxf_circle_or_arc(
                page, world_xf, *rx, *ry, *mode, *start_deg, *end_deg, layer_name, out,
            ) {
                visited.remove(&obj.id);
                return;
            }
        }
    }

    match &obj.payload {
        NodePayload::Path(p) => {
            emit_dxf_path(page, &p.transformed(&world_xf), layer_name, opts, out);
        }
        NodePayload::Shape(s) => {
            emit_dxf_path(
                page,
                &s.to_bezier_path().transformed(&world_xf),
                layer_name,
                opts,
                out,
            );
        }
        NodePayload::BSpline(bs) => {
            emit_dxf_path(
                page,
                &bs.to_bezier_path().transformed(&world_xf),
                layer_name,
                opts,
                out,
            );
        }
        NodePayload::ArtisticMedia { spine, style } => {
            emit_dxf_path(
                page,
                &coree_core::ArtisticMedia::expand_stroke(spine, style).transformed(&world_xf),
                layer_name,
                opts,
                out,
            );
        }
        NodePayload::Clone { .. } => {
            if let Some((p, f, o)) = doc.resolve_clone_payload_and_style(obj) {
                let mut resolved = obj.clone();
                resolved.payload = p;
                resolved.fill = f;
                resolved.outline = o;
                emit_dxf_object_entities(
                    doc,
                    page,
                    &resolved,
                    parent_xf,
                    layer_name,
                    opts,
                    out,
                    depth + 1,
                    visited,
                );
            }
        }
        NodePayload::Table(t) => {
            emit_dxf_path(
                page,
                &t.to_grid_paths().transformed(&world_xf),
                layer_name,
                opts,
                out,
            );
        }
        NodePayload::Connector(c) => {
            let (a, b) = doc.resolve_connector_endpoints_with_transform(c, &world_xf);
            emit_dxf_path(page, &c.route_path(a, b), layer_name, opts, out);
        }
        NodePayload::PowerClip { container, .. } => {
            emit_dxf_path(
                page,
                &container.transformed(&world_xf),
                layer_name,
                opts,
                out,
            );
        }
        NodePayload::Group(children) => {
            for &cid in children {
                if let Some(child) = doc.object(cid) {
                    emit_dxf_object_entities(
                        doc,
                        page,
                        child,
                        world_xf,
                        layer_name,
                        opts,
                        out,
                        depth + 1,
                        visited,
                    );
                }
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = doc.symbol(*sym_id) {
                for sub in &sym.objects {
                    let mut sym_visited = HashSet::new();
                    emit_dxf_object_entities(
                        doc,
                        page,
                        sub,
                        world_xf,
                        layer_name,
                        opts,
                        out,
                        depth + 1,
                        &mut sym_visited,
                    );
                }
            }
        }
        NodePayload::ArtisticText(_)
        | NodePayload::ParagraphText(_)
        | NodePayload::TextOnPath(_)
        | NodePayload::Raster(_)
        | NodePayload::Dimension(_) => {}
    }

    visited.remove(&obj.id);
}

#[allow(clippy::too_many_arguments)]
fn try_emit_exact_dxf_circle_or_arc(
    page: &Page,
    world_xf: Affine2D,
    rx: f64,
    ry: f64,
    mode: EllipseMode,
    start_deg: f64,
    end_deg: f64,
    layer_name: &str,
    out: &mut String,
) -> bool {
    if !rx.is_finite() || !ry.is_finite() || rx <= 1e-4 || (rx - ry).abs() > 1e-3 * rx.max(1.0) {
        return false;
    }
    let sx = world_xf.m[0].hypot(world_xf.m[1]);
    let sy = world_xf.m[2].hypot(world_xf.m[3]);
    let skew = (world_xf.m[0] * world_xf.m[2] + world_xf.m[1] * world_xf.m[3]).abs();
    if !sx.is_finite()
        || !sy.is_finite()
        || sx <= 1e-6
        || (sx - sy).abs() > 1e-3 * sx.max(1.0)
        || skew > 1e-3 * sx * sy
    {
        return false;
    }

    let radius = rx * 0.5 * (sx + sy);
    let center_w = world_xf.transform_point(Point2D::ZERO);
    if !center_w.is_finite() || !radius.is_finite() || radius <= 1e-5 {
        return false;
    }
    let (cx, cy) = to_cad_mm(page, center_w);

    if mode == EllipseMode::Full {
        let _ = writeln!(
            out,
            "0\nCIRCLE\n8\n{layer_name}\n10\n{cx:.4}\n20\n{cy:.4}\n30\n0.0\n40\n{radius:.4}"
        );
        return true;
    }

    if mode == EllipseMode::Arc {
        let mut sweep_deg = end_deg - start_deg;
        if !start_deg.is_finite() || !end_deg.is_finite() || sweep_deg.abs() < 1e-3 {
            return false;
        }
        sweep_deg = sweep_deg.clamp(-360.0, 360.0);
        if sweep_deg.abs() >= 359.99 {
            let _ = writeln!(
                out,
                "0\nCIRCLE\n8\n{layer_name}\n10\n{cx:.4}\n20\n{cy:.4}\n30\n0.0\n40\n{radius:.4}"
            );
            return true;
        }
        let s_rad = start_deg.to_radians();
        let m_rad = (start_deg + 0.5 * sweep_deg).to_radians();
        let e_rad = (start_deg + sweep_deg).to_radians();
        let p_s = to_cad_mm(
            page,
            world_xf.transform_point(Point2D::new(rx * s_rad.cos(), ry * s_rad.sin())),
        );
        let p_m = to_cad_mm(
            page,
            world_xf.transform_point(Point2D::new(rx * m_rad.cos(), ry * m_rad.sin())),
        );
        let p_e = to_cad_mm(
            page,
            world_xf.transform_point(Point2D::new(rx * e_rad.cos(), ry * e_rad.sin())),
        );

        let ang_s = (p_s.1 - cy)
            .atan2(p_s.0 - cx)
            .to_degrees()
            .rem_euclid(360.0);
        let ang_m = (p_m.1 - cy)
            .atan2(p_m.0 - cx)
            .to_degrees()
            .rem_euclid(360.0);
        let ang_e = (p_e.1 - cy)
            .atan2(p_e.0 - cx)
            .to_degrees()
            .rem_euclid(360.0);
        let mid_on_ccw = (ang_m - ang_s).rem_euclid(360.0) <= (ang_e - ang_s).rem_euclid(360.0);
        let (dxf_start, dxf_end) = if mid_on_ccw {
            (ang_s, ang_e)
        } else {
            (ang_e, ang_s)
        };
        let _ = writeln!(
            out,
            "0\nARC\n8\n{layer_name}\n10\n{cx:.4}\n20\n{cy:.4}\n30\n0.0\n40\n{radius:.4}\n50\n{dxf_start:.4}\n51\n{dxf_end:.4}"
        );
        return true;
    }

    false
}

#[allow(clippy::too_many_arguments)]
fn emit_dxf_path(
    page: &Page,
    path: &BezierPath,
    layer_name: &str,
    opts: &DxfExportOptions,
    out: &mut String,
) {
    let safe_tol = if opts.flatten_tolerance_mm.is_finite() && opts.flatten_tolerance_mm > 0.0 {
        opts.flatten_tolerance_mm
    } else {
        0.05
    };
    if opts.flatten_curves {
        for poly in path.flatten(safe_tol) {
            let poly: Vec<Point2D> = poly
                .into_iter()
                .filter(|p| p.x.is_finite() && p.y.is_finite())
                .collect();
            if poly.len() < 2 {
                continue;
            }
            let closed = poly.len() > 2
                && poly
                    .first()
                    .zip(poly.last())
                    .is_some_and(|(a, b)| a.distance_to(*b) < 1e-4);
            let pts = if closed {
                &poly[..poly.len() - 1]
            } else {
                poly.as_slice()
            };
            emit_dxf_lwpolyline(page, pts, closed, layer_name, out);
        }
        return;
    }

    for sp in &path.subpaths {
        if sp.nodes.len() < 2 {
            continue;
        }
        let has_curves = sp
            .nodes
            .iter()
            .any(|n| n.handle_in.is_some() || n.handle_out.is_some());

        if !has_curves {
            if !sp.closed && sp.nodes.len() == 2 {
                emit_dxf_line(page, sp.nodes[0].pos, sp.nodes[1].pos, layer_name, out);
            } else {
                let pts: Vec<Point2D> = sp.nodes.iter().map(|n| n.pos).collect();
                emit_dxf_lwpolyline(page, &pts, sp.closed, layer_name, out);
            }
        } else {
            // Emit exact segments: LINE for straight segments, degree-3 SPLINE for cubic Béziers
            for seg_idx in 0..sp.segment_count() {
                let Some(seg) = sp.segment_to_kurbo(seg_idx) else {
                    continue;
                };
                match seg {
                    PathSeg::Line(l) => {
                        emit_dxf_line(
                            page,
                            Point2D::from_kurbo(l.p0),
                            Point2D::from_kurbo(l.p1),
                            layer_name,
                            out,
                        );
                    }
                    PathSeg::Cubic(c) => {
                        emit_dxf_cubic_spline(
                            page,
                            [
                                Point2D::from_kurbo(c.p0),
                                Point2D::from_kurbo(c.p1),
                                Point2D::from_kurbo(c.p2),
                                Point2D::from_kurbo(c.p3),
                            ],
                            layer_name,
                            out,
                        );
                    }
                    PathSeg::Quad(q) => {
                        let c = q.raise();
                        emit_dxf_cubic_spline(
                            page,
                            [
                                Point2D::from_kurbo(c.p0),
                                Point2D::from_kurbo(c.p1),
                                Point2D::from_kurbo(c.p2),
                                Point2D::from_kurbo(c.p3),
                            ],
                            layer_name,
                            out,
                        );
                    }
                }
            }
        }
    }
}

fn emit_dxf_line(page: &Page, a: Point2D, b: Point2D, layer_name: &str, out: &mut String) {
    if !(a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite()) {
        return;
    }
    let (ax, ay) = to_cad_mm(page, a);
    let (bx, by) = to_cad_mm(page, b);
    let _ = writeln!(
        out,
        "0\nLINE\n8\n{layer_name}\n10\n{ax:.4}\n20\n{ay:.4}\n30\n0.0\n11\n{bx:.4}\n21\n{by:.4}\n31\n0.0"
    );
}

fn emit_dxf_lwpolyline(
    page: &Page,
    pts: &[Point2D],
    closed: bool,
    layer_name: &str,
    out: &mut String,
) {
    let finite_pts: Vec<Point2D> = pts
        .iter()
        .copied()
        .filter(|p| p.x.is_finite() && p.y.is_finite())
        .collect();
    if finite_pts.len() < 2 {
        return;
    }
    let flags = if closed { 1 } else { 0 };
    let _ = writeln!(
        out,
        "0\nLWPOLYLINE\n8\n{layer_name}\n90\n{}\n70\n{flags}",
        finite_pts.len()
    );
    for &p in &finite_pts {
        let (x, y) = to_cad_mm(page, p);
        let _ = writeln!(out, "10\n{x:.4}\n20\n{y:.4}");
    }
}

fn emit_dxf_cubic_spline(page: &Page, ctrl: [Point2D; 4], layer_name: &str, out: &mut String) {
    if !ctrl.iter().all(|p| p.x.is_finite() && p.y.is_finite()) {
        return;
    }
    // Clamped degree-3 Bezier B-spline: degree=3, 8 knots [0,0,0,0, 1,1,1,1], 4 control points.
    let _ = writeln!(
        out,
        "0\nSPLINE\n8\n{layer_name}\n70\n8\n71\n3\n72\n8\n73\n4\n74\n0"
    );
    for knot in [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0] {
        let _ = writeln!(out, "40\n{knot:.4}");
    }
    for pt in ctrl {
        let (x, y) = to_cad_mm(page, pt);
        let _ = writeln!(out, "10\n{x:.4}\n20\n{y:.4}\n30\n0.0");
    }
}

// =============================================================================
// HPGL / PLT Vinyl Cutting Plotter Export (1 mm = 40 HPGL units)
// =============================================================================

/// Exports `page_index` of `doc` as a valid HPGL `.plt` string (`1 mm = 40 HPGL units`).
pub fn export_plt_to_string(doc: &Document, page_index: usize) -> IoResult<String> {
    export_plt_to_string_with_options(doc, page_index, &PltExportOptions::default())
}

/// Exports `page_index` of `doc` as a valid HPGL `.plt` string with custom options.
pub fn export_plt_to_string_with_options(
    doc: &Document,
    page_index: usize,
    opts: &PltExportOptions,
) -> IoResult<String> {
    let page = doc.page(page_index).ok_or(IoError::PageOutOfRange {
        page_index,
        page_count: doc.pages().len(),
    })?;

    let layers = collect_page_layers(doc, page_index, opts.cut_layers_only);

    let mut out = String::with_capacity(2048);
    out.push_str("IN;");

    let mut pen_idx = 0usize;
    for layer in &layers {
        let mut paths = Vec::new();
        let mut visited = HashSet::new();
        for &oid in &layer.objects {
            if let Some(obj) = doc.object(oid) {
                collect_vector_paths(doc, obj, Affine2D::IDENTITY, &mut paths, 0, &mut visited);
            }
        }
        if paths.is_empty() {
            continue;
        }
        let pen = (pen_idx % 8) + 1; // HPGL standard pen slots SP1..SP8 (SP1 = primary blade)
        pen_idx += 1;
        let _ = write!(out, "SP{pen};");
        let safe_tol = if opts.flatten_tolerance_mm.is_finite() && opts.flatten_tolerance_mm > 0.0 {
            opts.flatten_tolerance_mm
        } else {
            0.05
        };
        for path in &paths {
            for poly in path.flatten(safe_tol) {
                let poly: Vec<Point2D> = poly
                    .into_iter()
                    .filter(|p| p.x.is_finite() && p.y.is_finite())
                    .collect();
                if poly.len() < 2 {
                    continue;
                }
                let (x0, y0) = to_hpgl_units(page, poly[0]);
                let _ = write!(out, "PU{x0},{y0};PD");
                for (i, &pt) in poly.iter().skip(1).enumerate() {
                    let (x, y) = to_hpgl_units(page, pt);
                    if i > 0 {
                        out.push(',');
                    }
                    let _ = write!(out, "{x},{y}");
                }
                out.push_str(";PU;");
            }
        }
    }

    out.push_str("SP0;\n");
    Ok(out)
}

/// Atomically exports `page_index` of `doc` to an HPGL `.plt` plotter file at `path`.
pub fn export_plt(doc: &Document, page_index: usize, path: &Path) -> IoResult<()> {
    export_plt_with_options(doc, page_index, &PltExportOptions::default(), path)
}

/// Atomically exports `page_index` of `doc` to an HPGL `.plt` file with custom options.
pub fn export_plt_with_options(
    doc: &Document,
    page_index: usize,
    opts: &PltExportOptions,
    path: &Path,
) -> IoResult<()> {
    let plt = export_plt_to_string_with_options(doc, page_index, opts)?;
    atomic_write_bytes(path, plt.as_bytes())
}

fn collect_page_layers(doc: &Document, page_index: usize, cut_layers_only: bool) -> Vec<&Layer> {
    let mut layers = Vec::new();
    // Emit applicable master layers from OTHER pages first (bottom), then target page's own layers.
    for (p_idx, p) in doc.pages().iter().enumerate() {
        if p_idx == page_index {
            continue;
        }
        for layer in &p.layers {
            if !layer.visible || (cut_layers_only && !layer.is_cut_layer) {
                continue;
            }
            let include = match layer.master_scope {
                MasterLayerScope::None => false,
                MasterLayerScope::AllPages => true,
                MasterLayerScope::OddPages => (page_index + 1) % 2 == 1,
                MasterLayerScope::EvenPages => (page_index + 1).is_multiple_of(2),
            };
            if include {
                layers.push(layer);
            }
        }
    }
    if let Some(page) = doc.page(page_index) {
        for layer in &page.layers {
            if layer.visible && (!cut_layers_only || layer.is_cut_layer) {
                layers.push(layer);
            }
        }
    }
    layers
}

fn collect_vector_paths(
    doc: &Document,
    obj: &SceneObject,
    parent_xf: Affine2D,
    out: &mut Vec<BezierPath>,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) {
    if depth > MAX_CAD_DEPTH || !obj.visible || !visited.insert(obj.id) {
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        visited.remove(&obj.id);
        return;
    }
    match &obj.payload {
        NodePayload::Path(p) => out.push(p.transformed(&world_xf)),
        NodePayload::Shape(s) => out.push(s.to_bezier_path().transformed(&world_xf)),
        NodePayload::BSpline(bs) => out.push(bs.to_bezier_path().transformed(&world_xf)),
        NodePayload::ArtisticMedia { spine, style } => {
            out.push(coree_core::ArtisticMedia::expand_stroke(spine, style).transformed(&world_xf))
        }
        NodePayload::Clone { .. } => {
            if let Some((p, f, o)) = doc.resolve_clone_payload_and_style(obj) {
                let mut resolved = obj.clone();
                resolved.payload = p;
                resolved.fill = f;
                resolved.outline = o;
                collect_vector_paths(doc, &resolved, parent_xf, out, depth + 1, visited);
            }
        }
        NodePayload::Table(t) => out.push(t.to_grid_paths().transformed(&world_xf)),
        NodePayload::Connector(c) => {
            let (a, b) = doc.resolve_connector_endpoints(c);
            out.push(c.route_path(a, b));
        }
        NodePayload::PowerClip { container, .. } => out.push(container.transformed(&world_xf)),
        NodePayload::Group(children) => {
            for &cid in children {
                if let Some(child) = doc.object(cid) {
                    collect_vector_paths(doc, child, world_xf, out, depth + 1, visited);
                }
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = doc.symbol(*sym_id) {
                for sub in &sym.objects {
                    let mut sym_visited = HashSet::new();
                    collect_vector_paths(doc, sub, world_xf, out, depth + 1, &mut sym_visited);
                }
            }
        }
        NodePayload::ArtisticText(_)
        | NodePayload::ParagraphText(_)
        | NodePayload::TextOnPath(_)
        | NodePayload::Raster(_)
        | NodePayload::Dimension(_) => {}
    }
    visited.remove(&obj.id);
}

fn to_cad_mm(page: &Page, pt: Point2D) -> (f64, f64) {
    let h = if page.height_mm.is_finite() && page.height_mm > 0.0 {
        page.height_mm
    } else {
        297.0
    };
    let x = if pt.x.is_finite() { pt.x } else { 0.0 };
    let y = if pt.y.is_finite() { h - pt.y } else { 0.0 };
    (x, y)
}

fn to_hpgl_units(page: &Page, pt: Point2D) -> (i64, i64) {
    let (x_mm, y_mm) = to_cad_mm(page, pt);
    let x = (x_mm * HPGL_UNITS_PER_MM)
        .round()
        .clamp(i32::MIN as f64, i32::MAX as f64) as i64;
    let y = (y_mm * HPGL_UNITS_PER_MM)
        .round()
        .clamp(i32::MIN as f64, i32::MAX as f64) as i64;
    (x, y)
}

fn sanitize_dxf_layer_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '$') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        "Layer".to_owned()
    } else {
        clean
    }
}

// =============================================================================
// AutoCAD ASCII DXF Importer (R12 / R2000+)
// =============================================================================

/// Imports an AutoCAD ASCII `.dxf` file from `path` into a millimeter [`Document`].
pub fn import_dxf(path: &Path) -> IoResult<Document> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((MAX_CAD_INPUT_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CAD_INPUT_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_dxf_bytes={MAX_CAD_INPUT_BYTES}"),
            actual: format!("file exceeds {MAX_CAD_INPUT_BYTES} bytes"),
        });
    }
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Imported DXF");
    import_dxf_bytes(&bytes, title)
}

/// Imports an AutoCAD ASCII DXF byte stream into a millimeter [`Document`].
pub fn import_dxf_bytes(bytes: &[u8], title: &str) -> IoResult<Document> {
    if bytes.is_empty() {
        return Err(IoError::InvalidFormat {
            format: "dxf".into(),
            message: "empty DXF input".into(),
        });
    }
    if bytes.len() > MAX_CAD_INPUT_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_dxf_bytes={MAX_CAD_INPUT_BYTES}"),
            actual: format!("{} bytes", bytes.len()),
        });
    }
    if bytes.starts_with(b"AutoCAD Binary DXF") {
        return Err(IoError::InvalidFormat {
            format: "dxf".into(),
            message: "binary DXF is not supported (expected ASCII DXF)".into(),
        });
    }

    let text = String::from_utf8_lossy(bytes);
    let pairs = parse_dxf_pairs(&text)?;
    if pairs.is_empty() {
        return Err(IoError::InvalidFormat {
            format: "dxf".into(),
            message: "no DXF group-code pairs found".into(),
        });
    }

    let mut unit_scale_mm = 1.0_f64;
    let mut ext_min: Option<Point2D> = None;
    let mut ext_max: Option<Point2D> = None;
    let mut dxf_layers: HashMap<String, DxfLayerInfo> = HashMap::new();
    let mut layer_order: Vec<String> = Vec::new();
    let mut blocks: HashMap<String, DxfBlock> = HashMap::new();
    let mut entities: Vec<DxfEntity> = Vec::new();
    let mut saw_section = false;

    let mut i = 0usize;
    while i < pairs.len() {
        let (code, ref val) = pairs[i];
        if code == 0 && val.eq_ignore_ascii_case("EOF") {
            break;
        }
        if code == 0 && val.eq_ignore_ascii_case("SECTION") {
            saw_section = true;
            i += 1;
            let section_name = if i < pairs.len() && pairs[i].0 == 2 {
                let s = pairs[i].1.trim().to_ascii_uppercase();
                i += 1;
                s
            } else {
                String::new()
            };

            match section_name.as_str() {
                "HEADER" => {
                    parse_dxf_header(
                        &pairs,
                        &mut i,
                        &mut unit_scale_mm,
                        &mut ext_min,
                        &mut ext_max,
                    );
                }
                "TABLES" => {
                    parse_dxf_tables(&pairs, &mut i, &mut dxf_layers, &mut layer_order);
                }
                "BLOCKS" => {
                    parse_dxf_blocks(&pairs, &mut i, &mut blocks);
                }
                "ENTITIES" => {
                    entities.extend(parse_dxf_entity_list(&pairs, &mut i, "ENDSEC"));
                }
                _ => {
                    while i < pairs.len() {
                        let is_end = pairs[i].0 == 0 && pairs[i].1.eq_ignore_ascii_case("ENDSEC");
                        i += 1;
                        if is_end {
                            break;
                        }
                    }
                }
            }
        } else {
            i += 1;
        }
    }

    if !saw_section {
        return Err(IoError::InvalidFormat {
            format: "dxf".into(),
            message: "missing DXF SECTION markers".into(),
        });
    }

    // Flatten all entities (resolving BLOCK/INSERT recursively with cycle & DAG fanout guards) into Y-up mm space.
    let mut resolved = Vec::new();
    let scale_xf = Affine2D::scale_uniform(unit_scale_mm);
    let mut visited_blocks = HashSet::new();
    let mut expand_steps = 0usize;
    for ent in &entities {
        expand_dxf_entity(
            ent,
            &blocks,
            &dxf_layers,
            scale_xf,
            None,
            0,
            &mut visited_blocks,
            &mut expand_steps,
            &mut resolved,
        )?;
    }

    // Determine page bounds in DXF Y-up millimeter coordinates.
    let valid_ext = ext_min.zip(ext_max).and_then(|(lo, hi)| {
        let lo_mm = Point2D::new(lo.x * unit_scale_mm, lo.y * unit_scale_mm);
        let hi_mm = Point2D::new(hi.x * unit_scale_mm, hi.y * unit_scale_mm);
        let sane = lo_mm.is_finite()
            && hi_mm.is_finite()
            && lo_mm.x.abs() < 1e12
            && lo_mm.y.abs() < 1e12
            && hi_mm.x.abs() < 1e12
            && hi_mm.y.abs() < 1e12
            && hi_mm.x > lo_mm.x + 1e-3
            && hi_mm.y > lo_mm.y + 1e-3;
        sane.then_some((lo_mm, hi_mm))
    });

    let (min_x, max_y, page_w, page_h) = if let Some((lo, hi)) = valid_ext {
        (lo.x, hi.y, (hi.x - lo.x).max(1.0), (hi.y - lo.y).max(1.0))
    } else {
        let mut geom_bb: Option<Rect2D> = None;
        for item in &resolved {
            if let Some(bb) = item.cad_bounds() {
                geom_bb = Some(match geom_bb {
                    Some(acc) => acc.union(bb),
                    None => bb,
                });
            }
        }
        if let Some(bb) = geom_bb {
            let max_span = bb.width().max(bb.height());
            let margin = (max_span * 0.05).clamp(5.0, 25.0);
            let w = (bb.width() + 2.0 * margin).max(10.0);
            let h = (bb.height() + 2.0 * margin).max(10.0);
            (bb.min_x - margin, bb.max_y + margin, w, h)
        } else {
            (0.0, 297.0, 210.0, 297.0)
        }
    };

    // Map DXF Y-up mm (x, y) -> coree Y-down mm (x - min_x, max_y - y).
    let cad_to_page = Affine2D::new(1.0, 0.0, 0.0, -1.0, -min_x, max_y);

    let clean_title = if title.trim().is_empty() {
        "Imported DXF"
    } else {
        title
    };
    let mut doc = Document::new(clean_title, page_w, page_h, ColorMode::Rgb, 300.0);
    doc.metadata_mut().source_format = Some("dxf".to_owned());
    let page_id = doc.pages()[0].id;
    let default_layer_id = doc.pages()[0].layers[0].id;

    // Create document layers in table order (plus any entity-referenced layers not in TABLES).
    let mut layer_ids: HashMap<String, LayerId> = HashMap::new();
    for lname in &layer_order {
        let info = dxf_layers.get(lname);
        let is_cut = is_cut_layer_name(lname);
        let lid = doc.add_layer(page_id, lname.clone(), is_cut, MasterLayerScope::None)?;
        if let (Some(layer), Some(meta)) = (doc.find_layer_mut(lid), info) {
            layer.visible = meta.visible;
        }
        layer_ids.insert(lname.clone(), lid);
    }

    for (idx, item) in resolved.into_iter().enumerate() {
        let lid = if let Some(&existing) = layer_ids.get(&item.layer) {
            existing
        } else {
            let is_cut = is_cut_layer_name(&item.layer);
            let created =
                doc.add_layer(page_id, item.layer.clone(), is_cut, MasterLayerScope::None)?;
            layer_ids.insert(item.layer.clone(), created);
            created
        };

        let is_cut = is_cut_layer_name(&item.layer);
        match item.kind {
            ResolvedCadItemKind::Path {
                mut path,
                filled,
                color,
            } => {
                path.transform(&cad_to_page);
                if path.is_empty() {
                    continue;
                }
                let (fill, outline) = if filled {
                    (Fill::Solid(color), OutlineStyle::none())
                } else if is_cut {
                    (Fill::None, OutlineStyle::cut_contour())
                } else {
                    (Fill::None, OutlineStyle::solid(color, 0.25))
                };
                let obj = SceneObject::new(
                    ObjectId(0),
                    format!("DXF Entity {}", idx + 1),
                    NodePayload::Path(path),
                )
                .with_fill(fill)
                .with_outline(outline);
                doc.add_object(obj, Some(lid))?;
            }
            ResolvedCadItemKind::Text {
                text,
                pos,
                height_mm,
                rotation_deg,
                color,
            } => {
                if text.trim().is_empty() {
                    continue;
                }
                let font_size_pt = (height_mm * (72.0 / 25.4)).clamp(2.0, 2000.0);
                let page_pos = cad_to_page.transform_point(pos);
                // DXF rotation is CCW in Y-up -> CW (negative) in Y-down page coordinates
                let xf = Affine2D::translate(page_pos.x, page_pos.y)
                    .compose(&Affine2D::rotate_deg(-rotation_deg))
                    .compose(&Affine2D::translate(0.0, -height_mm));
                let style = TextStyle {
                    font_size_pt,
                    fill: color,
                    direction: TextDirection::Auto,
                    ..TextStyle::default()
                };
                let obj = SceneObject::new(
                    ObjectId(0),
                    format!("DXF Text {}", idx + 1),
                    NodePayload::ArtisticText(ArtisticText::from_plain(text, style)),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none())
                .with_transform(xf);
                doc.add_object(obj, Some(lid))?;
            }
        }
    }

    // Remove unused empty placeholder "Layer 1" when DXF layers were created.
    if !layer_ids.is_empty() && doc.pages()[0].layers.len() > 1 {
        if let Some(pos) = doc.pages()[0]
            .layers
            .iter()
            .position(|l| l.id == default_layer_id && l.objects.is_empty())
        {
            doc.remove_page_layer(0, pos);
        }
    }

    doc.refresh_document_palette();
    Ok(doc)
}

fn parse_finite_f64(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn is_cut_layer_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("CutContour") || name.eq_ignore_ascii_case("CUT")
}

fn parse_dxf_pairs(text: &str) -> IoResult<Vec<(i32, String)>> {
    let mut lines = text.lines();
    let mut pairs = Vec::new();
    while let Some(code_line) = lines.next() {
        let trimmed = code_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let code = trimmed.parse::<i32>().map_err(|_| IoError::InvalidFormat {
            format: "dxf".into(),
            message: format!("invalid DXF group code '{trimmed}'"),
        })?;
        let val_line = lines.next().unwrap_or("");
        let val = val_line.trim_end_matches('\r').to_owned();
        pairs.push((code, val));
    }
    Ok(pairs)
}

fn dxf_insunits_to_mm(code: i32) -> f64 {
    match code {
        1 => 25.4,        // Inches
        2 => 304.8,       // Feet
        3 => 1_609_344.0, // Miles
        4 => 1.0,         // Millimeters
        5 => 10.0,        // Centimeters
        6 => 1_000.0,     // Meters
        7 => 1_000_000.0, // Kilometers
        8 => 0.000_025_4, // Microinches
        9 => 0.0254,      // Mils
        10 => 914.4,      // Yards
        11 => 1e-7,       // Angstroms
        12 => 1e-6,       // Nanometers
        13 => 1e-3,       // Microns
        14 => 100.0,      // Decimeters
        _ => 1.0,         // Unitless / default mm
    }
}

fn parse_dxf_header(
    pairs: &[(i32, String)],
    i: &mut usize,
    unit_scale_mm: &mut f64,
    ext_min: &mut Option<Point2D>,
    ext_max: &mut Option<Point2D>,
) {
    let mut current_var = String::new();
    let mut min_x = None;
    let mut min_y = None;
    let mut max_x = None;
    let mut max_y = None;

    while *i < pairs.len() {
        let (code, ref val) = pairs[*i];
        *i += 1;
        if code == 0 && val.eq_ignore_ascii_case("ENDSEC") {
            break;
        }
        if code == 9 {
            current_var = val.trim().to_ascii_uppercase();
            continue;
        }
        match current_var.as_str() {
            "$INSUNITS" if code == 70 => {
                if let Ok(u) = val.trim().parse::<i32>() {
                    *unit_scale_mm = dxf_insunits_to_mm(u);
                }
            }
            "$EXTMIN" => match code {
                10 => min_x = parse_finite_f64(val),
                20 => min_y = parse_finite_f64(val),
                _ => {}
            },
            "$EXTMAX" => match code {
                10 => max_x = parse_finite_f64(val),
                20 => max_y = parse_finite_f64(val),
                _ => {}
            },
            _ => {}
        }
    }

    if let (Some(x), Some(y)) = (min_x, min_y) {
        *ext_min = Some(Point2D::new(x, y));
    }
    if let (Some(x), Some(y)) = (max_x, max_y) {
        *ext_max = Some(Point2D::new(x, y));
    }
}

#[derive(Debug, Clone)]
struct DxfLayerInfo {
    color: Color,
    visible: bool,
}

fn parse_dxf_tables(
    pairs: &[(i32, String)],
    i: &mut usize,
    layers: &mut HashMap<String, DxfLayerInfo>,
    layer_order: &mut Vec<String>,
) {
    let mut in_layer_table = false;
    while *i < pairs.len() {
        let (code, ref val) = pairs[*i];
        *i += 1;
        if code == 0 && val.eq_ignore_ascii_case("ENDSEC") {
            break;
        }
        if code == 0 && val.eq_ignore_ascii_case("TABLE") {
            if *i < pairs.len() && pairs[*i].0 == 2 {
                in_layer_table = pairs[*i].1.trim().eq_ignore_ascii_case("LAYER");
                *i += 1;
            }
            continue;
        }
        if code == 0 && val.eq_ignore_ascii_case("ENDTAB") {
            in_layer_table = false;
            continue;
        }
        if in_layer_table && code == 0 && val.eq_ignore_ascii_case("LAYER") {
            let mut name = String::new();
            let mut aci = 7i32;
            let mut truecolor: Option<u32> = None;
            let mut flags = 0i32;
            while *i < pairs.len() && pairs[*i].0 != 0 {
                let (c, ref v) = pairs[*i];
                *i += 1;
                match c {
                    2 => name = v.trim().to_owned(),
                    62 => {
                        if let Ok(n) = v.trim().parse::<i32>() {
                            aci = n;
                        }
                    }
                    420 => {
                        if let Ok(tc) = v.trim().parse::<u32>() {
                            truecolor = Some(tc);
                        }
                    }
                    70 => {
                        if let Ok(f) = v.trim().parse::<i32>() {
                            flags = f;
                        }
                    }
                    _ => {}
                }
            }
            if !name.is_empty() {
                let visible = aci >= 0 && (flags & 1) == 0;
                let color = truecolor
                    .map(dxf_truecolor_to_color)
                    .unwrap_or_else(|| aci_to_color(aci.saturating_abs()));
                if !layers.contains_key(&name) {
                    layer_order.push(name.clone());
                }
                layers.insert(name, DxfLayerInfo { color, visible });
            }
        }
    }
}

#[derive(Debug, Clone)]
struct DxfBlock {
    base: Point2D,
    entities: Vec<DxfEntity>,
}

fn parse_dxf_blocks(
    pairs: &[(i32, String)],
    i: &mut usize,
    blocks: &mut HashMap<String, DxfBlock>,
) {
    while *i < pairs.len() {
        let (code, ref val) = pairs[*i];
        *i += 1;
        if code == 0 && val.eq_ignore_ascii_case("ENDSEC") {
            break;
        }
        if code == 0 && val.eq_ignore_ascii_case("BLOCK") {
            let mut name = String::new();
            let mut bx = 0.0_f64;
            let mut by = 0.0_f64;
            while *i < pairs.len() && pairs[*i].0 != 0 {
                let (c, ref v) = pairs[*i];
                *i += 1;
                match c {
                    2 => name = v.trim().to_owned(),
                    10 => bx = parse_finite_f64(v).unwrap_or(0.0),
                    20 => by = parse_finite_f64(v).unwrap_or(0.0),
                    _ => {}
                }
            }
            let block_entities = parse_dxf_entity_list(pairs, i, "ENDBLK");
            if !name.is_empty() {
                blocks.insert(
                    name,
                    DxfBlock {
                        base: Point2D::new(bx, by),
                        entities: block_entities,
                    },
                );
            }
        }
    }
}

#[derive(Debug, Clone)]
struct DxfEntity {
    layer: String,
    aci: Option<i32>,
    truecolor: Option<u32>,
    kind: DxfEntityKind,
}

#[derive(Debug, Clone)]
enum DxfEntityKind {
    Line {
        p0: Point2D,
        p1: Point2D,
    },
    Polyline {
        vertices: Vec<(Point2D, f64)>, // (pos, bulge)
        closed: bool,
    },
    Circle {
        center: Point2D,
        radius: f64,
    },
    Arc {
        center: Point2D,
        radius: f64,
        start_deg: f64,
        end_deg: f64,
    },
    Ellipse {
        center: Point2D,
        major_end: Vec2D,
        ratio: f64,
        start_param: f64,
        end_param: f64,
    },
    Spline {
        degree: usize,
        closed: bool,
        knots: Vec<f64>,
        ctrl_pts: Vec<Point2D>,
        fit_pts: Vec<Point2D>,
    },
    Solid {
        corners: Vec<Point2D>,
    },
    Text {
        text: String,
        pos: Point2D,
        height: f64,
        rotation_deg: f64,
    },
    Insert {
        block_name: String,
        pos: Point2D,
        scale_x: f64,
        scale_y: f64,
        rotation_deg: f64,
    },
}

fn parse_dxf_entity_list(
    pairs: &[(i32, String)],
    i: &mut usize,
    stop_marker: &str,
) -> Vec<DxfEntity> {
    let mut out = Vec::new();
    while *i < pairs.len() {
        let (code, ref val) = pairs[*i];
        if code != 0 {
            *i += 1;
            continue;
        }
        let kind_str = val.trim().to_ascii_uppercase();
        *i += 1;
        if kind_str == stop_marker || kind_str == "ENDSEC" || kind_str == "EOF" {
            // Skip remaining attributes of ENDBLK if stop_marker == "ENDBLK"
            if kind_str == "ENDBLK" {
                while *i < pairs.len() && pairs[*i].0 != 0 {
                    *i += 1;
                }
            }
            break;
        }

        // Collect all group pairs belonging to this entity up to the next code 0
        let attr_start = *i;
        while *i < pairs.len() && pairs[*i].0 != 0 {
            *i += 1;
        }
        let attrs = &pairs[attr_start..*i];

        let mut layer = "0".to_owned();
        let mut aci: Option<i32> = None;
        let mut truecolor: Option<u32> = None;
        for (c, v) in attrs {
            match *c {
                8 => layer = v.trim().to_owned(),
                62 => aci = v.trim().parse::<i32>().ok(),
                420 => truecolor = v.trim().parse::<u32>().ok(),
                _ => {}
            }
        }

        let parsed = match kind_str.as_str() {
            "LINE" => parse_dxf_line(attrs),
            "LWPOLYLINE" => parse_dxf_lwpolyline(attrs),
            "POLYLINE" => parse_dxf_old_polyline(attrs, pairs, i),
            "CIRCLE" => parse_dxf_circle(attrs),
            "ARC" => parse_dxf_arc(attrs),
            "ELLIPSE" => parse_dxf_ellipse(attrs),
            "SPLINE" => parse_dxf_spline(attrs),
            "SOLID" | "TRACE" => parse_dxf_solid(attrs),
            "TEXT" => parse_dxf_text(attrs, false),
            "MTEXT" => parse_dxf_text(attrs, true),
            "INSERT" => parse_dxf_insert(attrs),
            _ => None,
        };

        if let Some(kind) = parsed {
            out.push(DxfEntity {
                layer,
                aci,
                truecolor,
                kind,
            });
        }
    }
    out
}

fn parse_dxf_line(attrs: &[(i32, String)]) -> Option<DxfEntityKind> {
    let (mut x0, mut y0, mut x1, mut y1) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for (c, v) in attrs {
        match *c {
            10 => x0 = parse_finite_f64(v).unwrap_or(0.0),
            20 => y0 = parse_finite_f64(v).unwrap_or(0.0),
            11 => x1 = parse_finite_f64(v).unwrap_or(0.0),
            21 => y1 = parse_finite_f64(v).unwrap_or(0.0),
            _ => {}
        }
    }
    Some(DxfEntityKind::Line {
        p0: Point2D::new(x0, y0),
        p1: Point2D::new(x1, y1),
    })
}

fn parse_dxf_lwpolyline(attrs: &[(i32, String)]) -> Option<DxfEntityKind> {
    let mut closed = false;
    let mut vertices: Vec<(Point2D, f64)> = Vec::new();
    let mut cur_x: Option<f64> = None;
    let mut cur_y: Option<f64> = None;
    let mut cur_bulge = 0.0_f64;

    for (c, v) in attrs {
        match *c {
            70 => {
                if let Ok(f) = v.trim().parse::<i32>() {
                    closed = (f & 1) != 0;
                }
            }
            10 => {
                if let (Some(x), Some(y)) = (cur_x.take(), cur_y.take()) {
                    vertices.push((Point2D::new(x, y), cur_bulge));
                    cur_bulge = 0.0;
                }
                cur_x = parse_finite_f64(v);
            }
            20 => {
                cur_y = parse_finite_f64(v);
            }
            42 => {
                cur_bulge = parse_finite_f64(v).unwrap_or(0.0);
            }
            _ => {}
        }
    }
    if let (Some(x), Some(y)) = (cur_x, cur_y) {
        vertices.push((Point2D::new(x, y), cur_bulge));
    }
    if vertices.len() < 2 {
        return None;
    }
    Some(DxfEntityKind::Polyline { vertices, closed })
}

fn parse_dxf_old_polyline(
    attrs: &[(i32, String)],
    pairs: &[(i32, String)],
    i: &mut usize,
) -> Option<DxfEntityKind> {
    let mut flags = 0i32;
    for (c, v) in attrs {
        if *c == 70 {
            flags = v.trim().parse().unwrap_or(0);
        }
    }
    let closed = (flags & 1) != 0;
    let mut vertices = Vec::new();

    while *i < pairs.len() {
        let (code, ref val) = pairs[*i];
        if code != 0 {
            *i += 1;
            continue;
        }
        let kind = val.trim().to_ascii_uppercase();
        if kind == "SEQEND" {
            *i += 1;
            while *i < pairs.len() && pairs[*i].0 != 0 {
                *i += 1;
            }
            break;
        }
        if kind != "VERTEX" {
            break;
        }
        *i += 1;
        let mut x = 0.0_f64;
        let mut y = 0.0_f64;
        let mut bulge = 0.0_f64;
        let mut vflags = 0i32;
        while *i < pairs.len() && pairs[*i].0 != 0 {
            let (c, ref v) = pairs[*i];
            *i += 1;
            match c {
                10 => x = parse_finite_f64(v).unwrap_or(0.0),
                20 => y = parse_finite_f64(v).unwrap_or(0.0),
                42 => bulge = parse_finite_f64(v).unwrap_or(0.0),
                70 => vflags = v.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
        // Skip polyface mesh face-index records (bit 128 without bit 64)
        if (vflags & 128) == 0 || (vflags & 64) != 0 {
            vertices.push((Point2D::new(x, y), bulge));
        }
    }

    if vertices.len() < 2 {
        return None;
    }
    Some(DxfEntityKind::Polyline { vertices, closed })
}

fn parse_dxf_circle(attrs: &[(i32, String)]) -> Option<DxfEntityKind> {
    let (mut cx, mut cy, mut r) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (c, v) in attrs {
        match *c {
            10 => cx = parse_finite_f64(v).unwrap_or(0.0),
            20 => cy = parse_finite_f64(v).unwrap_or(0.0),
            40 => r = parse_finite_f64(v).unwrap_or(0.0),
            _ => {}
        }
    }
    (r.is_finite() && r > 1e-9).then_some(DxfEntityKind::Circle {
        center: Point2D::new(cx, cy),
        radius: r,
    })
}

fn parse_dxf_arc(attrs: &[(i32, String)]) -> Option<DxfEntityKind> {
    let (mut cx, mut cy, mut r, mut a0, mut a1) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64, 360.0_f64);
    for (c, v) in attrs {
        match *c {
            10 => cx = parse_finite_f64(v).unwrap_or(0.0),
            20 => cy = parse_finite_f64(v).unwrap_or(0.0),
            40 => r = parse_finite_f64(v).unwrap_or(0.0),
            50 => a0 = parse_finite_f64(v).unwrap_or(0.0),
            51 => a1 = parse_finite_f64(v).unwrap_or(360.0),
            _ => {}
        }
    }
    (r.is_finite() && r > 1e-9 && a0.is_finite() && a1.is_finite()).then_some(DxfEntityKind::Arc {
        center: Point2D::new(cx, cy),
        radius: r,
        start_deg: a0,
        end_deg: a1,
    })
}

fn parse_dxf_ellipse(attrs: &[(i32, String)]) -> Option<DxfEntityKind> {
    let (mut cx, mut cy, mut mx, mut my) = (0.0_f64, 0.0_f64, 1.0_f64, 0.0_f64);
    let (mut ratio, mut p0, mut p1) = (1.0_f64, 0.0_f64, std::f64::consts::TAU);
    for (c, v) in attrs {
        match *c {
            10 => cx = parse_finite_f64(v).unwrap_or(0.0),
            20 => cy = parse_finite_f64(v).unwrap_or(0.0),
            11 => mx = parse_finite_f64(v).unwrap_or(1.0),
            21 => my = parse_finite_f64(v).unwrap_or(0.0),
            40 => ratio = parse_finite_f64(v).unwrap_or(1.0),
            41 => p0 = parse_finite_f64(v).unwrap_or(0.0),
            42 => p1 = parse_finite_f64(v).unwrap_or(std::f64::consts::TAU),
            _ => {}
        }
    }
    let major = Vec2D::new(mx, my);
    (major.length() > 1e-9
        && ratio.is_finite()
        && ratio.abs() > 1e-9
        && p0.is_finite()
        && p1.is_finite())
    .then_some(DxfEntityKind::Ellipse {
        center: Point2D::new(cx, cy),
        major_end: major,
        ratio,
        start_param: p0,
        end_param: p1,
    })
}

fn parse_dxf_spline(attrs: &[(i32, String)]) -> Option<DxfEntityKind> {
    let mut degree = 3usize;
    let mut closed = false;
    let mut knots = Vec::new();
    let mut ctrl_pts = Vec::new();
    let mut fit_pts = Vec::new();
    let mut cx: Option<f64> = None;
    let mut fx: Option<f64> = None;

    for (c, v) in attrs {
        match *c {
            70 => {
                if let Ok(f) = v.trim().parse::<i32>() {
                    closed = (f & 1) != 0;
                }
            }
            71 => {
                if let Ok(d) = v.trim().parse::<usize>() {
                    degree = d.clamp(1, 10);
                }
            }
            40 => {
                if let Some(k) = parse_finite_f64(v) {
                    knots.push(k);
                }
            }
            10 => cx = parse_finite_f64(v),
            20 => {
                if let (Some(x), Some(y)) = (cx.take(), parse_finite_f64(v)) {
                    ctrl_pts.push(Point2D::new(x, y));
                }
            }
            11 => fx = parse_finite_f64(v),
            21 => {
                if let (Some(x), Some(y)) = (fx.take(), parse_finite_f64(v)) {
                    fit_pts.push(Point2D::new(x, y));
                }
            }
            _ => {}
        }
    }
    if ctrl_pts.len() < 2 && fit_pts.len() < 2 {
        return None;
    }
    Some(DxfEntityKind::Spline {
        degree,
        closed,
        knots,
        ctrl_pts,
        fit_pts,
    })
}

fn parse_dxf_solid(attrs: &[(i32, String)]) -> Option<DxfEntityKind> {
    let mut pts = [Point2D::ZERO; 4];
    let mut has = [false; 4];
    for (c, v) in attrs {
        let val = parse_finite_f64(v).unwrap_or(0.0);
        match *c {
            10 => {
                pts[0].x = val;
                has[0] = true;
            }
            20 => pts[0].y = val,
            11 => {
                pts[1].x = val;
                has[1] = true;
            }
            21 => pts[1].y = val,
            12 => {
                pts[2].x = val;
                has[2] = true;
            }
            22 => pts[2].y = val,
            13 => {
                pts[3].x = val;
                has[3] = true;
            }
            23 => pts[3].y = val,
            _ => {}
        }
    }
    if !(has[0] && has[1] && has[2]) {
        return None;
    }
    // DXF SOLID stores 4 corners in bow-tie order (0, 1, 3, 2); reorder to perimeter loop.
    let corners = if has[3] && pts[2].distance_to(pts[3]) > 1e-6 {
        vec![pts[0], pts[1], pts[3], pts[2]]
    } else {
        vec![pts[0], pts[1], pts[2]]
    };
    Some(DxfEntityKind::Solid { corners })
}

fn parse_dxf_text(attrs: &[(i32, String)], is_mtext: bool) -> Option<DxfEntityKind> {
    let (mut x, mut y, mut height, mut rot) = (0.0_f64, 0.0_f64, 5.0_f64, 0.0_f64);
    let mut dir_x: Option<f64> = None;
    let mut dir_y: Option<f64> = None;
    let mut raw_text = String::new();

    for (c, v) in attrs {
        match *c {
            10 => x = parse_finite_f64(v).unwrap_or(0.0),
            20 => y = parse_finite_f64(v).unwrap_or(0.0),
            11 if is_mtext => dir_x = parse_finite_f64(v),
            21 if is_mtext => dir_y = parse_finite_f64(v),
            40 => height = parse_finite_f64(v).unwrap_or(5.0),
            50 => rot = parse_finite_f64(v).unwrap_or(0.0),
            3 if is_mtext => raw_text.push_str(v),
            1 => raw_text.push_str(v),
            _ => {}
        }
    }
    if let (Some(dx), Some(dy)) = (dir_x, dir_y) {
        if dx.hypot(dy) > 1e-6 {
            rot = dy.atan2(dx).to_degrees();
        }
    }
    let clean = clean_dxf_text(&raw_text, is_mtext);
    if clean.is_empty() {
        return None;
    }
    Some(DxfEntityKind::Text {
        text: clean,
        pos: Point2D::new(x, y),
        height: height.abs().max(0.5),
        rotation_deg: rot,
    })
}

fn clean_dxf_text(raw: &str, is_mtext: bool) -> String {
    let replaced = raw
        .replace("%%d", "°")
        .replace("%%D", "°")
        .replace("%%c", "∅")
        .replace("%%C", "∅")
        .replace("%%p", "±")
        .replace("%%P", "±");
    if !is_mtext {
        return replaced;
    }
    let mut out = String::with_capacity(replaced.len());
    let chars: Vec<char> = replaced.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' | '}' => {
                i += 1;
            }
            '\\' if i + 1 < chars.len() => {
                let next = chars[i + 1];
                match next {
                    'P' | 'p' => {
                        out.push('\n');
                        i += 2;
                    }
                    '~' => {
                        out.push(' ');
                        i += 2;
                    }
                    '\\' | '{' | '}' => {
                        out.push(next);
                        i += 2;
                    }
                    'f' | 'F' | 'H' | 'C' | 'Q' | 'W' | 'T' | 'A' | 'S' => {
                        // Skip MTEXT inline formatting code up to ';'
                        i += 2;
                        while i < chars.len() && chars[i] != ';' {
                            i += 1;
                        }
                        if i < chars.len() {
                            i += 1;
                        }
                    }
                    _ => {
                        i += 2;
                    }
                }
            }
            ch => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

fn parse_dxf_insert(attrs: &[(i32, String)]) -> Option<DxfEntityKind> {
    let mut block_name = String::new();
    let (mut x, mut y, mut sx, mut sy, mut rot) = (0.0_f64, 0.0_f64, 1.0_f64, 1.0_f64, 0.0_f64);
    for (c, v) in attrs {
        match *c {
            2 => block_name = v.trim().to_owned(),
            10 => x = parse_finite_f64(v).unwrap_or(0.0),
            20 => y = parse_finite_f64(v).unwrap_or(0.0),
            41 => sx = parse_finite_f64(v).unwrap_or(1.0),
            42 => sy = parse_finite_f64(v).unwrap_or(1.0),
            50 => rot = parse_finite_f64(v).unwrap_or(0.0),
            _ => {}
        }
    }
    if block_name.is_empty() {
        return None;
    }
    Some(DxfEntityKind::Insert {
        block_name,
        pos: Point2D::new(x, y),
        scale_x: sx,
        scale_y: sy,
        rotation_deg: rot,
    })
}

struct ResolvedCadItem {
    layer: String,
    kind: ResolvedCadItemKind,
}

enum ResolvedCadItemKind {
    Path {
        path: BezierPath,
        filled: bool,
        color: Color,
    },
    Text {
        text: String,
        pos: Point2D,
        height_mm: f64,
        rotation_deg: f64,
        color: Color,
    },
}

impl ResolvedCadItem {
    fn cad_bounds(&self) -> Option<Rect2D> {
        match &self.kind {
            ResolvedCadItemKind::Path { path, .. } => path.bounding_box(),
            ResolvedCadItemKind::Text {
                pos,
                height_mm,
                text,
                ..
            } => {
                let w = (text.chars().count().max(1) as f64) * height_mm * 0.6;
                Some(Rect2D::from_xywh(pos.x, pos.y, w, *height_mm))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn expand_dxf_entity(
    ent: &DxfEntity,
    blocks: &HashMap<String, DxfBlock>,
    layers: &HashMap<String, DxfLayerInfo>,
    xf: Affine2D,
    inherited_layer: Option<&str>,
    depth: usize,
    visited_blocks: &mut HashSet<String>,
    expand_steps: &mut usize,
    out: &mut Vec<ResolvedCadItem>,
) -> IoResult<()> {
    *expand_steps = expand_steps.saturating_add(1);
    if *expand_steps > MAX_DXF_EXPAND_STEPS || out.len() >= MAX_DXF_EXPANDED_ENTITIES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_dxf_entities={MAX_DXF_EXPANDED_ENTITIES}"),
            actual: format!(
                "DXF block/entity expansion exceeded cap (entities={}, steps={})",
                out.len(),
                *expand_steps
            ),
        });
    }
    if depth > MAX_DXF_INSERT_DEPTH || !xf.is_finite() {
        return Ok(());
    }

    // Per DXF spec, entities inside a BLOCK on layer "0" inherit the INSERT's layer.
    let eff_layer = if ent.layer == "0" {
        inherited_layer.unwrap_or("0").to_owned()
    } else {
        ent.layer.clone()
    };

    let color = if let Some(tc) = ent.truecolor {
        dxf_truecolor_to_color(tc)
    } else if let Some(aci) = ent.aci.filter(|&c| c != 256 && c != 0) {
        aci_to_color(aci.saturating_abs())
    } else {
        layers
            .get(&eff_layer)
            .map_or(Color::BLACK, |l| l.color.clone())
    };

    match &ent.kind {
        DxfEntityKind::Insert {
            block_name,
            pos,
            scale_x,
            scale_y,
            rotation_deg,
        } => {
            if !visited_blocks.insert(block_name.clone()) {
                return Ok(()); // Cycle guard!
            }
            if let Some(blk) = blocks.get(block_name) {
                let ins_xf = Affine2D::translate(pos.x, pos.y)
                    .compose(&Affine2D::rotate_deg(*rotation_deg))
                    .compose(&Affine2D::scale(*scale_x, *scale_y))
                    .compose(&Affine2D::translate(-blk.base.x, -blk.base.y));
                let combined = xf.compose(&ins_xf);
                for sub in &blk.entities {
                    expand_dxf_entity(
                        sub,
                        blocks,
                        layers,
                        combined,
                        Some(&eff_layer),
                        depth + 1,
                        visited_blocks,
                        expand_steps,
                        out,
                    )?;
                }
            }
            visited_blocks.remove(block_name);
        }
        DxfEntityKind::Line { p0, p1 } => {
            let path = BezierPath::from_line(*p0, *p1).transformed(&xf);
            out.push(ResolvedCadItem {
                layer: eff_layer,
                kind: ResolvedCadItemKind::Path {
                    path,
                    filled: false,
                    color,
                },
            });
        }
        DxfEntityKind::Polyline { vertices, closed } => {
            let kb = build_dxf_bulge_polyline(vertices, *closed);
            let path = BezierPath::from_kurbo(&kb).transformed(&xf);
            if !path.is_empty() {
                out.push(ResolvedCadItem {
                    layer: eff_layer,
                    kind: ResolvedCadItemKind::Path {
                        path,
                        filled: false,
                        color,
                    },
                });
            }
        }
        DxfEntityKind::Circle { center, radius } => {
            let kb = build_circular_arc_bezpath(*center, *radius, 0.0, std::f64::consts::TAU, true);
            let path = BezierPath::from_kurbo(&kb).transformed(&xf);
            out.push(ResolvedCadItem {
                layer: eff_layer,
                kind: ResolvedCadItemKind::Path {
                    path,
                    filled: false,
                    color,
                },
            });
        }
        DxfEntityKind::Arc {
            center,
            radius,
            start_deg,
            end_deg,
        } => {
            if !(start_deg.is_finite() && end_deg.is_finite()) {
                return Ok(());
            }
            // O(1) modular sweep calculation (never loops on extreme or non-finite angles)
            let a0 = start_deg.rem_euclid(360.0).to_radians();
            let mut sweep_deg = (end_deg - start_deg).rem_euclid(360.0);
            if sweep_deg <= 1e-9 {
                sweep_deg = 360.0;
            }
            let a1 = a0 + sweep_deg.to_radians();
            let kb = build_circular_arc_bezpath(*center, *radius, a0, a1, false);
            let path = BezierPath::from_kurbo(&kb).transformed(&xf);
            out.push(ResolvedCadItem {
                layer: eff_layer,
                kind: ResolvedCadItemKind::Path {
                    path,
                    filled: false,
                    color,
                },
            });
        }
        DxfEntityKind::Ellipse {
            center,
            major_end,
            ratio,
            start_param,
            end_param,
        } => {
            if !(start_param.is_finite() && end_param.is_finite()) {
                return Ok(());
            }
            let p0 = start_param.rem_euclid(std::f64::consts::TAU);
            let mut sweep = (end_param - start_param).rem_euclid(std::f64::consts::TAU);
            if sweep <= 1e-9 {
                sweep = std::f64::consts::TAU;
            }
            let full = sweep >= std::f64::consts::TAU - 1e-4;
            let p1 = p0 + sweep;
            let unit_arc = build_circular_arc_bezpath(Point2D::ZERO, 1.0, p0, p1, full);
            let minor = major_end.perp() * (*ratio);
            let ellipse_xf = Affine2D::new(
                major_end.x,
                major_end.y,
                minor.x,
                minor.y,
                center.x,
                center.y,
            );
            let path = BezierPath::from_kurbo(&unit_arc).transformed(&xf.compose(&ellipse_xf));
            out.push(ResolvedCadItem {
                layer: eff_layer,
                kind: ResolvedCadItemKind::Path {
                    path,
                    filled: false,
                    color,
                },
            });
        }
        DxfEntityKind::Spline {
            degree,
            closed,
            knots,
            ctrl_pts,
            fit_pts,
        } => {
            let kb = build_dxf_spline_bezpath(*degree, *closed, knots, ctrl_pts, fit_pts);
            let path = BezierPath::from_kurbo(&kb).transformed(&xf);
            if !path.is_empty() {
                out.push(ResolvedCadItem {
                    layer: eff_layer,
                    kind: ResolvedCadItemKind::Path {
                        path,
                        filled: false,
                        color,
                    },
                });
            }
        }
        DxfEntityKind::Solid { corners } => {
            let path = BezierPath::from_polygon(corners.iter().copied(), true).transformed(&xf);
            out.push(ResolvedCadItem {
                layer: eff_layer,
                kind: ResolvedCadItemKind::Path {
                    path,
                    filled: true,
                    color,
                },
            });
        }
        DxfEntityKind::Text {
            text,
            pos,
            height,
            rotation_deg,
        } => {
            let world_pos = xf.transform_point(*pos);
            let scale = 0.5 * (xf.m[0].hypot(xf.m[1]) + xf.m[2].hypot(xf.m[3]));
            let xf_rot = xf.m[1].atan2(xf.m[0]).to_degrees();
            out.push(ResolvedCadItem {
                layer: eff_layer,
                kind: ResolvedCadItemKind::Text {
                    text: text.clone(),
                    pos: world_pos,
                    height_mm: (*height * scale.max(1e-6)).max(0.5),
                    rotation_deg: *rotation_deg + xf_rot,
                    color,
                },
            });
        }
    }
    Ok(())
}

/// Converts a DXF polyline with per-vertex bulge values into an exact cubic Bézier path.
fn build_dxf_bulge_polyline(vertices: &[(Point2D, f64)], closed: bool) -> BezPath {
    let mut kb = BezPath::new();
    if vertices.len() < 2 {
        return kb;
    }
    kb.move_to(vertices[0].0.to_kurbo());
    let seg_count = if closed {
        vertices.len()
    } else {
        vertices.len() - 1
    };

    for idx in 0..seg_count {
        let (p0, bulge) = vertices[idx];
        let p1 = vertices[(idx + 1) % vertices.len()].0;
        let chord = p1 - p0;
        let dist = chord.length();
        if dist <= 1e-9 {
            continue;
        }
        if !bulge.is_finite() || bulge.abs() <= 1e-7 {
            kb.line_to(p1.to_kurbo());
        } else {
            // DXF bulge = tan(sweep / 4)
            let sweep = 4.0 * bulge.atan();
            let mid = p0.midpoint(p1);
            let left_normal = Vec2D::new(-chord.y / dist, chord.x / dist);
            let offset = (dist * 0.5) * (1.0 - bulge * bulge) / (2.0 * bulge);
            let center = mid + left_normal * offset;
            let radius = p0.distance_to(center);
            let a0 = (p0.y - center.y).atan2(p0.x - center.x);
            append_cubic_arc_segments(&mut kb, center, radius, a0, sweep);
        }
    }
    if closed {
        kb.close_path();
    }
    kb
}

fn build_circular_arc_bezpath(
    center: Point2D,
    radius: f64,
    start_rad: f64,
    end_rad: f64,
    closed: bool,
) -> BezPath {
    let mut kb = BezPath::new();
    if !radius.is_finite() || radius <= 0.0 {
        return kb;
    }
    let start_pt = Point2D::new(
        center.x + radius * start_rad.cos(),
        center.y + radius * start_rad.sin(),
    );
    kb.move_to(start_pt.to_kurbo());
    append_cubic_arc_segments(&mut kb, center, radius, start_rad, end_rad - start_rad);
    if closed {
        kb.close_path();
    }
    kb
}

fn append_cubic_arc_segments(
    kb: &mut BezPath,
    center: Point2D,
    radius: f64,
    start_rad: f64,
    sweep_rad: f64,
) {
    if sweep_rad.abs() <= 1e-9 || radius <= 1e-9 {
        return;
    }
    let segs = ((sweep_rad.abs() / std::f64::consts::FRAC_PI_2).ceil() as usize).clamp(1, 32);
    let step = sweep_rad / (segs as f64);
    let kappa = (4.0 / 3.0) * (step / 4.0).tan();
    let mut a = start_rad;
    for _ in 0..segs {
        let next_a = a + step;
        let (s0, c0) = a.sin_cos();
        let (s1, c1) = next_a.sin_cos();
        let p1 = kurbo::Point::new(
            center.x + radius * (c0 - kappa * s0),
            center.y + radius * (s0 + kappa * c0),
        );
        let p2 = kurbo::Point::new(
            center.x + radius * (c1 + kappa * s1),
            center.y + radius * (s1 - kappa * c1),
        );
        let p3 = kurbo::Point::new(center.x + radius * c1, center.y + radius * s1);
        kb.curve_to(p1, p2, p3);
        a = next_a;
    }
}

fn build_dxf_spline_bezpath(
    degree: usize,
    closed: bool,
    knots: &[f64],
    ctrl_pts: &[Point2D],
    fit_pts: &[Point2D],
) -> BezPath {
    let mut kb = BezPath::new();
    // Exact fast-path for clamped cubic Bézier spline (4 control points, degree 3).
    if degree == 3 && ctrl_pts.len() == 4 && !closed {
        kb.move_to(ctrl_pts[0].to_kurbo());
        kb.curve_to(
            ctrl_pts[1].to_kurbo(),
            ctrl_pts[2].to_kurbo(),
            ctrl_pts[3].to_kurbo(),
        );
        return kb;
    }

    if ctrl_pts.len() > degree {
        // Construct uniform clamped knot vector if missing or mismatched.
        let n = ctrl_pts.len();
        let expected_knots = n + degree + 1;
        let knot_vec: Vec<f64> = if knots.len() == expected_knots {
            knots.to_vec()
        } else {
            let mut kv = Vec::with_capacity(expected_knots);
            for idx in 0..expected_knots {
                if idx <= degree {
                    kv.push(0.0);
                } else if idx >= n {
                    kv.push(1.0);
                } else {
                    kv.push((idx - degree) as f64 / ((n - degree) as f64));
                }
            }
            kv
        };

        let u_min = knot_vec[degree];
        let u_max = knot_vec[n];
        if u_max > u_min + 1e-12 {
            let samples = (n * 12).clamp(16, 512);
            for s in 0..=samples {
                let t = (s as f64) / (samples as f64);
                let u = u_min + t * (u_max - u_min);
                let pt = eval_bspline_de_boor(degree, &knot_vec, ctrl_pts, u);
                if s == 0 {
                    kb.move_to(pt.to_kurbo());
                } else {
                    kb.line_to(pt.to_kurbo());
                }
            }
            if closed {
                kb.close_path();
            }
            return kb;
        }
    }

    // Fallback: connect fit points or control points directly
    let pts = if fit_pts.len() >= 2 {
        fit_pts
    } else {
        ctrl_pts
    };
    if let Some(&first) = pts.first() {
        kb.move_to(first.to_kurbo());
        for &p in &pts[1..] {
            kb.line_to(p.to_kurbo());
        }
        if closed {
            kb.close_path();
        }
    }
    kb
}

fn eval_bspline_de_boor(p: usize, knots: &[f64], ctrl: &[Point2D], u: f64) -> Point2D {
    let n = ctrl.len() - 1;
    // Find knot span k such that knots[k] <= u < knots[k+1]
    let k = if u >= knots[n + 1] {
        n
    } else {
        let mut span = p;
        for idx in p..=n {
            if u >= knots[idx] && u < knots[idx + 1] {
                span = idx;
                break;
            }
        }
        span
    };

    let mut d: Vec<Point2D> = (0..=p)
        .map(|j| ctrl[(k + j).saturating_sub(p).min(n)])
        .collect();

    for r in 1..=p {
        for j in (r..=p).rev() {
            let left = knots[j + k - p];
            let right = knots[j + 1 + k - r];
            let denom = right - left;
            let alpha = if denom.abs() > 1e-12 {
                (u - left) / denom
            } else {
                0.0
            };
            d[j] = d[j - 1].lerp(d[j], alpha);
        }
    }
    d[p]
}

fn dxf_truecolor_to_color(rgb24: u32) -> Color {
    let r = ((rgb24 >> 16) & 0xFF) as u8;
    let g = ((rgb24 >> 8) & 0xFF) as u8;
    let b = (rgb24 & 0xFF) as u8;
    Color::rgba8(r, g, b, 255)
}

/// Maps AutoCAD Color Index (ACI 1..=255) to sRGB [`Color`].
fn aci_to_color(aci: i32) -> Color {
    match aci {
        1 => Color::rgba8(255, 0, 0, 255),     // Red
        2 => Color::rgba8(255, 255, 0, 255),   // Yellow
        3 => Color::rgba8(0, 255, 0, 255),     // Green
        4 => Color::rgba8(0, 255, 255, 255),   // Cyan
        5 => Color::rgba8(0, 0, 255, 255),     // Blue
        6 => Color::rgba8(255, 0, 255, 255),   // Magenta
        7 => Color::BLACK,                     // White/Black (contrasting black on white paper)
        8 => Color::rgba8(128, 128, 128, 255), // Dark gray
        9 => Color::rgba8(192, 192, 192, 255), // Light gray
        10..=249 => {
            // Standard 24-hue × 5-value × 2-saturation AutoCAD ACI wheel
            let idx = (aci - 10) as usize;
            let hue_deg = ((idx / 10) as f32) * 15.0;
            let sub = idx % 10;
            let light = match sub / 2 {
                0 => 0.50,
                1 => 0.70,
                2 => 0.40,
                3 => 0.30,
                _ => 0.20,
            };
            let sat = if sub.is_multiple_of(2) { 1.0 } else { 0.5 };
            Color::hsl(hue_deg, sat, light)
        }
        250..=255 => {
            let v = match aci {
                250 => 51,
                251 => 91,
                252 => 132,
                253 => 173,
                254 => 214,
                _ => 255,
            };
            Color::rgba8(v, v, v, 255)
        }
        _ => Color::BLACK,
    }
}

// =============================================================================
// HPGL / PLT Cutting Plotter Importer (40 plotter units = 1 mm)
// =============================================================================

/// Imports an HPGL `.plt` cutting-plotter file from `path` into a millimeter [`Document`].
pub fn import_plt(path: &Path) -> IoResult<Document> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((MAX_CAD_INPUT_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CAD_INPUT_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_plt_bytes={MAX_CAD_INPUT_BYTES}"),
            actual: format!("file exceeds {MAX_CAD_INPUT_BYTES} bytes"),
        });
    }
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Imported PLT");
    import_plt_bytes(&bytes, title)
}

/// Imports an HPGL `.plt` byte stream (`1 mm = 40 HPGL units`) into a millimeter [`Document`].
pub fn import_plt_bytes(bytes: &[u8], title: &str) -> IoResult<Document> {
    if bytes.is_empty() {
        return Err(IoError::InvalidFormat {
            format: "plt".into(),
            message: "empty HPGL/PLT input".into(),
        });
    }
    if bytes.len() > MAX_CAD_INPUT_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_plt_bytes={MAX_CAD_INPUT_BYTES}"),
            actual: format!("{} bytes", bytes.len()),
        });
    }

    let text = String::from_utf8_lossy(bytes);
    let mut state = HpglState::new();
    parse_and_execute_hpgl(&text, &mut state);
    state.flush_stroke();

    if !state.saw_valid_cmd {
        return Err(IoError::InvalidFormat {
            format: "plt".into(),
            message: "no recognized HPGL commands found".into(),
        });
    }

    // Compute geometry bounds in HPGL Y-up millimeters
    let mut geom_bb: Option<Rect2D> = None;
    for stroke in &state.strokes {
        if let Some(bb) = stroke.path.bounding_box() {
            geom_bb = Some(match geom_bb {
                Some(acc) => acc.union(bb),
                None => bb,
            });
        }
    }
    for lbl in &state.labels {
        let w = (lbl.text.chars().count().max(1) as f64) * lbl.height_mm * 0.6;
        let r = Rect2D::from_xywh(lbl.pos.x, lbl.pos.y, w, lbl.height_mm);
        geom_bb = Some(match geom_bb {
            Some(acc) => acc.union(r),
            None => r,
        });
    }

    let (offset_x, max_y, page_w, page_h) = if let Some(bb) = geom_bb {
        let margin = 5.0;
        let min_x = bb.min_x.min(0.0);
        let min_y = bb.min_y.min(0.0);
        let shift_x = if min_x < 0.0 { -min_x + margin } else { 0.0 };
        let top_y = bb.max_y + margin;
        let bottom_y = if min_y < 0.0 { min_y - margin } else { 0.0 };
        let w = (bb.max_x + shift_x + margin).max(10.0);
        let h = (top_y - bottom_y).max(10.0);
        (shift_x, top_y, w, h)
    } else {
        (0.0, 297.0, 210.0, 297.0)
    };

    let hpgl_to_page = Affine2D::new(1.0, 0.0, 0.0, -1.0, offset_x, max_y);

    let clean_title = if title.trim().is_empty() {
        "Imported PLT"
    } else {
        title
    };
    let mut doc = Document::new(clean_title, page_w, page_h, ColorMode::Rgb, 300.0);
    doc.metadata_mut().source_format = Some("plt".to_owned());
    let page_id = doc.pages()[0].id;
    let default_layer_id = doc.pages()[0].layers[0].id;
    let cut_layer_id = doc.add_layer(page_id, "CutContour", true, MasterLayerScope::None)?;

    for (idx, stroke) in state.strokes.into_iter().enumerate() {
        let path = stroke.path.transformed(&hpgl_to_page);
        if path.is_empty() {
            continue;
        }
        let target_layer = if stroke.pen == 1 {
            cut_layer_id
        } else {
            default_layer_id
        };
        let color = hpgl_pen_color(stroke.pen);
        let outline = if stroke.pen == 1 && (stroke.width_mm - 0.25).abs() < 1e-6 {
            OutlineStyle::cut_contour()
        } else {
            OutlineStyle::solid(color, stroke.width_mm)
        };
        let obj = SceneObject::new(
            ObjectId(0),
            format!("HPGL Pen {} Path {}", stroke.pen, idx + 1),
            NodePayload::Path(path),
        )
        .with_fill(Fill::None)
        .with_outline(outline);
        doc.add_object(obj, Some(target_layer))?;
    }

    for (idx, lbl) in state.labels.into_iter().enumerate() {
        if lbl.text.trim().is_empty() {
            continue;
        }
        let p = hpgl_to_page.transform_point(lbl.pos);
        let target_layer = if lbl.pen == 1 {
            cut_layer_id
        } else {
            default_layer_id
        };
        let style = TextStyle {
            font_size_pt: (lbl.height_mm * (72.0 / 25.4)).clamp(4.0, 500.0),
            fill: hpgl_pen_color(lbl.pen),
            direction: TextDirection::Auto,
            ..TextStyle::default()
        };
        let obj = SceneObject::new(
            ObjectId(0),
            format!("HPGL Label {}", idx + 1),
            NodePayload::ArtisticText(ArtisticText::from_plain(lbl.text, style)),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(p.x, p.y - lbl.height_mm));
        doc.add_object(obj, Some(target_layer))?;
    }

    // Prune unused empty layer so single-pen files cleanly show `CutContour`
    // and Pen 2+ files cleanly show `Layer 1` (while multi-pen files preserve both!).
    if doc.pages()[0].layers.len() > 1 {
        let non_empty_count = doc.pages()[0]
            .layers
            .iter()
            .filter(|l| !l.objects.is_empty())
            .count();
        if non_empty_count > 0 {
            doc.retain_page_layers(0, |l| !l.objects.is_empty());
        }
    }

    doc.refresh_document_palette();
    Ok(doc)
}

fn hpgl_pen_color(pen: u8) -> Color {
    match ((pen.saturating_sub(1)) % 8) + 1 {
        1 => Color::cut_contour_spot(), // Pen 1: CutContour primary blade
        2 => Color::rgba8(220, 30, 30, 255), // Pen 2: Red
        3 => Color::rgba8(20, 160, 50, 255), // Pen 3: Green
        4 => Color::rgba8(25, 90, 220, 255), // Pen 4: Blue
        5 => Color::rgba8(0, 175, 210, 255), // Pen 5: Cyan
        6 => Color::rgba8(200, 30, 180, 255), // Pen 6: Magenta
        7 => Color::rgba8(215, 175, 10, 255), // Pen 7: Yellow/Gold
        _ => Color::rgba8(230, 115, 20, 255), // Pen 8: Orange
    }
}

struct HpglStroke {
    pen: u8,
    width_mm: f64,
    path: BezierPath,
}

struct HpglLabel {
    pen: u8,
    pos: Point2D,
    height_mm: f64,
    text: String,
}

struct HpglState {
    pen: u8,
    pen_down: bool,
    relative: bool,
    pos_mm: Point2D,
    p1_units: Point2D,
    p2_units: Point2D,
    user_scale: Option<(f64, f64, f64, f64)>, // (xmin, xmax, ymin, ymax)
    pen_widths_mm: [f64; 9],
    label_term: String,
    char_height_mm: f64,
    cur_path: BezPath,
    cur_path_has_draw: bool,
    strokes: Vec<HpglStroke>,
    labels: Vec<HpglLabel>,
    saw_valid_cmd: bool,
}

impl HpglState {
    fn new() -> Self {
        Self {
            pen: 1,
            pen_down: false,
            relative: false,
            pos_mm: Point2D::ZERO,
            p1_units: Point2D::ZERO,
            p2_units: Point2D::new(10_000.0, 10_000.0),
            user_scale: None,
            pen_widths_mm: [0.25; 9],
            label_term: "\x03".to_owned(),
            char_height_mm: 3.5,
            cur_path: BezPath::new(),
            cur_path_has_draw: false,
            strokes: Vec::new(),
            labels: Vec::new(),
            saw_valid_cmd: false,
        }
    }

    fn reset(&mut self) {
        self.flush_stroke();
        self.pen = 1;
        self.pen_down = false;
        self.relative = false;
        self.pos_mm = Point2D::ZERO;
        self.user_scale = None;
        self.label_term = "\x03".to_owned();
        self.char_height_mm = 3.5;
    }

    fn map_coord_abs(&self, x: f64, y: f64) -> Point2D {
        if let Some((xmin, xmax, ymin, ymax)) = self.user_scale {
            let ux = if (xmax - xmin).abs() > 1e-9 {
                self.p1_units.x + (x - xmin) * (self.p2_units.x - self.p1_units.x) / (xmax - xmin)
            } else {
                self.p1_units.x
            };
            let uy = if (ymax - ymin).abs() > 1e-9 {
                self.p1_units.y + (y - ymin) * (self.p2_units.y - self.p1_units.y) / (ymax - ymin)
            } else {
                self.p1_units.y
            };
            Point2D::new(ux / HPGL_UNITS_PER_MM, uy / HPGL_UNITS_PER_MM)
        } else {
            Point2D::new(x / HPGL_UNITS_PER_MM, y / HPGL_UNITS_PER_MM)
        }
    }

    fn map_delta(&self, dx: f64, dy: f64) -> Vec2D {
        if let Some((xmin, xmax, ymin, ymax)) = self.user_scale {
            let dux = if (xmax - xmin).abs() > 1e-9 {
                dx * (self.p2_units.x - self.p1_units.x) / (xmax - xmin)
            } else {
                0.0
            };
            let duy = if (ymax - ymin).abs() > 1e-9 {
                dy * (self.p2_units.y - self.p1_units.y) / (ymax - ymin)
            } else {
                0.0
            };
            Vec2D::new(dux / HPGL_UNITS_PER_MM, duy / HPGL_UNITS_PER_MM)
        } else {
            Vec2D::new(dx / HPGL_UNITS_PER_MM, dy / HPGL_UNITS_PER_MM)
        }
    }

    fn current_pen_width(&self) -> f64 {
        let idx = usize::from(self.pen).min(8);
        self.pen_widths_mm[idx]
    }

    fn flush_stroke(&mut self) {
        if self.cur_path_has_draw && !self.cur_path.is_empty() && self.pen > 0 {
            let kb = std::mem::take(&mut self.cur_path);
            let bez = BezierPath::from_kurbo(&kb);
            if !bez.is_empty() {
                self.strokes.push(HpglStroke {
                    pen: self.pen,
                    width_mm: self.current_pen_width(),
                    path: bez,
                });
            }
        } else {
            self.cur_path = BezPath::new();
        }
        self.cur_path_has_draw = false;
    }

    fn move_or_draw_pairs(&mut self, args: &[f64]) {
        for pair in args.as_chunks::<2>().0 {
            let target = if self.relative {
                self.pos_mm + self.map_delta(pair[0], pair[1])
            } else {
                self.map_coord_abs(pair[0], pair[1])
            };
            if self.pen_down && self.pen > 0 {
                if !self.cur_path_has_draw {
                    self.cur_path.move_to(self.pos_mm.to_kurbo());
                }
                self.cur_path.line_to(target.to_kurbo());
                self.cur_path_has_draw = true;
            }
            self.pos_mm = target;
        }
    }
}

fn parse_and_execute_hpgl(input: &str, st: &mut HpglState) {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;

    while i < len {
        // Skip delimiters, whitespace, and escape sequences
        while i < len && (bytes[i].is_ascii_whitespace() || bytes[i] == b';' || bytes[i] == b'\x1b')
        {
            if bytes[i] == b'\x1b' {
                i += 1;
                while i < len && bytes[i] != b';' && bytes[i] != b'\n' {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        if i + 2 > len {
            break;
        }
        if !bytes[i].is_ascii_alphabetic() || !bytes[i + 1].is_ascii_alphabetic() {
            i += 1;
            continue;
        }

        let cmd = [
            bytes[i].to_ascii_uppercase(),
            bytes[i + 1].to_ascii_uppercase(),
        ];
        i += 2;

        // Special handling for LB (Label) and DT (Define Label Terminator) — UTF-8 boundary safe!
        if &cmd == b"LB" {
            st.saw_valid_cmd = true;
            let start = i;
            let term_bytes = st.label_term.as_bytes();
            while i < len {
                if !term_bytes.is_empty() && bytes[i..].starts_with(term_bytes) {
                    break;
                }
                if bytes[i] == b';' {
                    break;
                }
                i += 1;
            }
            let lbl = String::from_utf8_lossy(&bytes[start..i]).into_owned();
            if !term_bytes.is_empty() && bytes[i..].starts_with(term_bytes) {
                i += term_bytes.len();
            }
            if st.pen > 0 && !lbl.is_empty() {
                st.labels.push(HpglLabel {
                    pen: st.pen,
                    pos: st.pos_mm,
                    height_mm: st.char_height_mm,
                    text: lbl,
                });
            }
            continue;
        }
        if &cmd == b"DT" {
            st.saw_valid_cmd = true;
            if i < len && bytes[i] != b';' {
                // Decode complete UTF-8 char (or single byte fallback) so cursor never splits a codepoint
                if let Some(ch) = std::str::from_utf8(&bytes[i..])
                    .ok()
                    .and_then(|s| s.chars().next())
                {
                    st.label_term = ch.to_string();
                    i += ch.len_utf8();
                } else {
                    st.label_term = "\x03".to_owned();
                    i += 1;
                }
            } else {
                st.label_term = "\x03".to_owned();
            }
            continue;
        }

        // Parse parameter slice up to ';' or next alphabetic command mnemonic
        let arg_start = i;
        while i < len && bytes[i] != b';' && !bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let arg_str = String::from_utf8_lossy(&bytes[arg_start..i]);
        if i < len && bytes[i] == b';' {
            i += 1;
        }
        let args = parse_hpgl_numbers(&arg_str);

        match &cmd {
            b"IN" | b"DF" => {
                st.saw_valid_cmd = true;
                st.reset();
            }
            b"IP" => {
                st.saw_valid_cmd = true;
                if args.len() >= 4 {
                    st.p1_units = Point2D::new(args[0], args[1]);
                    st.p2_units = Point2D::new(args[2], args[3]);
                } else if args.len() >= 2 {
                    let dx = st.p2_units.x - st.p1_units.x;
                    let dy = st.p2_units.y - st.p1_units.y;
                    st.p1_units = Point2D::new(args[0], args[1]);
                    st.p2_units = Point2D::new(args[0] + dx, args[1] + dy);
                } else {
                    st.p1_units = Point2D::ZERO;
                    st.p2_units = Point2D::new(10_000.0, 10_000.0);
                }
            }
            b"SC" => {
                st.saw_valid_cmd = true;
                if args.len() >= 4 {
                    st.user_scale = Some((args[0], args[1], args[2], args[3]));
                } else {
                    st.user_scale = None;
                }
            }
            b"SP" => {
                st.saw_valid_cmd = true;
                st.flush_stroke();
                let new_pen = args.first().copied().unwrap_or(0.0).round().clamp(0.0, 8.0) as u8;
                st.pen = new_pen;
                if new_pen == 0 {
                    st.pen_down = false;
                }
            }
            b"PW" => {
                st.saw_valid_cmd = true;
                let w = args.first().copied().unwrap_or(0.35).clamp(0.05, 50.0);
                if let Some(&pen_arg) = args.get(1) {
                    let idx = (pen_arg.round().clamp(1.0, 8.0)) as usize;
                    st.pen_widths_mm[idx] = w;
                } else {
                    st.pen_widths_mm.fill(w);
                }
            }
            b"SI" => {
                st.saw_valid_cmd = true;
                if args.len() >= 2 {
                    // SI width_cm, height_cm
                    st.char_height_mm = (args[1].abs() * 10.0).clamp(1.0, 200.0);
                }
            }
            b"PU" => {
                st.saw_valid_cmd = true;
                st.flush_stroke();
                st.pen_down = false;
                st.move_or_draw_pairs(&args);
            }
            b"PD" => {
                st.saw_valid_cmd = true;
                st.pen_down = true;
                st.move_or_draw_pairs(&args);
            }
            b"PA" => {
                st.saw_valid_cmd = true;
                st.relative = false;
                st.move_or_draw_pairs(&args);
            }
            b"PR" => {
                st.saw_valid_cmd = true;
                st.relative = true;
                st.move_or_draw_pairs(&args);
            }
            b"AA" | b"AR" => {
                st.saw_valid_cmd = true;
                if args.len() >= 3 && st.pen > 0 {
                    let center = if &cmd == b"AA" {
                        st.map_coord_abs(args[0], args[1])
                    } else {
                        st.pos_mm + st.map_delta(args[0], args[1])
                    };
                    let sweep_rad = args[2].to_radians();
                    let radius = st.pos_mm.distance_to(center);
                    if radius > 1e-6 && sweep_rad.abs() > 1e-6 {
                        let a0 = (st.pos_mm.y - center.y).atan2(st.pos_mm.x - center.x);
                        let end_a = a0 + sweep_rad;
                        let end_pt = Point2D::new(
                            center.x + radius * end_a.cos(),
                            center.y + radius * end_a.sin(),
                        );
                        if st.pen_down {
                            if !st.cur_path_has_draw {
                                st.cur_path.move_to(st.pos_mm.to_kurbo());
                            }
                            append_cubic_arc_segments(
                                &mut st.cur_path,
                                center,
                                radius,
                                a0,
                                sweep_rad,
                            );
                            st.cur_path_has_draw = true;
                        }
                        st.pos_mm = end_pt;
                    }
                }
            }
            b"CI" => {
                st.saw_valid_cmd = true;
                if let Some(&r_raw) = args.first() {
                    let r_mm = st.map_delta(r_raw, 0.0).x.abs();
                    if r_mm > 1e-6 && st.pen > 0 {
                        st.flush_stroke();
                        let kb = build_circular_arc_bezpath(
                            st.pos_mm,
                            r_mm,
                            0.0,
                            std::f64::consts::TAU,
                            true,
                        );
                        st.strokes.push(HpglStroke {
                            pen: st.pen,
                            width_mm: st.current_pen_width(),
                            path: BezierPath::from_kurbo(&kb),
                        });
                    }
                }
            }
            b"EA" | b"ER" => {
                st.saw_valid_cmd = true;
                if args.len() >= 2 && st.pen > 0 {
                    let opposite = if &cmd == b"EA" {
                        st.map_coord_abs(args[0], args[1])
                    } else {
                        st.pos_mm + st.map_delta(args[0], args[1])
                    };
                    st.flush_stroke();
                    let rect = Rect2D::from_points(st.pos_mm, opposite);
                    st.strokes.push(HpglStroke {
                        pen: st.pen,
                        width_mm: st.current_pen_width(),
                        path: BezierPath::from_rect(rect),
                    });
                }
            }
            b"VS" | b"LT" | b"RO" | b"IW" | b"PG" | b"EC" | b"OE" | b"OP" | b"SR" | b"DI"
            | b"DR" | b"FS" => {
                st.saw_valid_cmd = true;
            }
            _ => {}
        }
    }
}

fn parse_hpgl_numbers(s: &str) -> Vec<f64> {
    let mut nums = Vec::new();
    for part in s.split(|c: char| c == ',' || c.is_ascii_whitespace()) {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        if let Ok(v) = p.parse::<f64>() {
            if v.is_finite() {
                nums.push(v);
            }
        }
    }
    nums
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{CornerStyle, OutlineStyle, ParametricShape};

    #[test]
    fn dxf_export_contains_header_tables_cutcontour_lwpolyline_spline_line_and_eof() {
        let mut doc = Document::new("CNC Panel", 100.0, 50.0, coree_core::ColorMode::Cmyk, 300.0);
        let page_id = doc.pages()[0].id;
        let cut_layer = doc
            .add_layer(page_id, "CutContour", true, MasterLayerScope::None)
            .unwrap();

        // Sharp rectangle -> LWPOLYLINE
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Plate",
                NodePayload::Path(BezierPath::from_rect(coree_core::Rect2D::from_xywh(
                    10.0, 10.0, 40.0, 20.0,
                ))),
            )
            .with_outline(OutlineStyle::cut_contour()),
            Some(cut_layer),
        )
        .unwrap();

        // Rounded rectangle -> SPLINE + LINE
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Rounded Hole",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 20.0,
                    height: 15.0,
                    corner_radii: [3.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            ),
            Some(cut_layer),
        )
        .unwrap();

        // Single open line -> LINE
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Score",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 25.0),
                    Point2D::new(100.0, 25.0),
                )),
            ),
            None,
        )
        .unwrap();

        let dxf = export_dxf_to_string(&doc, 0).unwrap();
        assert!(dxf.contains("SECTION\n2\nHEADER\n"));
        assert!(dxf.contains("$INSUNITS\n70\n4\n"));
        assert!(dxf.contains("SECTION\n2\nTABLES\n"));
        assert!(dxf.contains("2\nCutContour\n"));
        assert!(dxf.contains("SECTION\n2\nENTITIES\n"));
        assert!(dxf.contains("0\nLWPOLYLINE\n"));
        assert!(dxf.contains("0\nSPLINE\n"));
        assert!(dxf.contains("0\nLINE\n"));
        assert!(dxf.ends_with("0\nEOF\n"));
    }

    #[test]
    fn plt_hpgl_export_uses_40_units_per_mm_and_valid_commands() {
        let mut doc = Document::new(
            "Vinyl Decal",
            100.0,
            50.0,
            coree_core::ColorMode::Cmyk,
            300.0,
        );
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Cut Line",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(10.0, 50.0), // y_up = 0 mm -> 0 hpgl
                    Point2D::new(20.0, 40.0), // x=800, y_up=10mm -> 400 hpgl
                )),
            ),
            None,
        )
        .unwrap();

        let plt = export_plt_to_string(&doc, 0).unwrap();
        assert!(plt.starts_with("IN;SP1;"));
        assert!(plt.contains("PU400,0;PD800,400;PU;"));
        assert!(plt.ends_with("SP0;\n"));
    }

    #[test]
    fn dxf_roundtrip_export_then_import_preserves_layers_and_geometry() {
        let mut doc = Document::new("Laser Jig", 120.0, 60.0, ColorMode::Cmyk, 300.0);
        let page_id = doc.pages()[0].id;
        let cut_layer = doc
            .add_layer(page_id, "CutContour", true, MasterLayerScope::None)
            .unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Outer Cut",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 10.0, 50.0, 30.0,
                ))),
            )
            .with_outline(OutlineStyle::cut_contour()),
            Some(cut_layer),
        )
        .unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Rounded Slot",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 24.0,
                    height: 12.0,
                    corner_radii: [4.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_transform(Affine2D::translate(70.0, 20.0)),
            Some(cut_layer),
        )
        .unwrap();

        let dxf = export_dxf_to_string(&doc, 0).unwrap();
        let imported = import_dxf_bytes(dxf.as_bytes(), "Laser Jig").unwrap();

        assert!((imported.pages()[0].width_mm - 120.0).abs() < 0.05);
        assert!((imported.pages()[0].height_mm - 60.0).abs() < 0.05);

        let cut = imported.pages()[0]
            .layers
            .iter()
            .find(|l| l.name.eq_ignore_ascii_case("CutContour"))
            .expect("CutContour layer preserved");
        assert!(cut.is_cut_layer);
        assert!(!cut.objects.is_empty());

        // Check first imported object is the 50x30 rectangle at (10, 10)
        let first_bb = imported.object_bounds(cut.objects[0]).unwrap();
        assert!((first_bb.min_x - 10.0).abs() < 0.05);
        assert!((first_bb.min_y - 10.0).abs() < 0.05);
        assert!((first_bb.width() - 50.0).abs() < 0.05);
        assert!((first_bb.height() - 30.0).abs() < 0.05);
    }

    #[test]
    fn dxf_import_handles_inches_bulge_arcs_blocks_cycle_guard_and_all_entities() {
        // Inches ($INSUNITS=1 => 25.4 mm/unit), LWPOLYLINE with bulge=1.0 semicircle,
        // CIRCLE, ARC, ELLIPSE, SOLID, MTEXT, BLOCK+INSERT including self-recursive cycle!
        let dxf = "\
0\nSECTION\n2\nHEADER\n9\n$INSUNITS\n70\n1\n0\nENDSEC\n\
0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nLAYER\n\
0\nLAYER\n2\nCutContour\n70\n0\n62\n6\n\
0\nLAYER\n2\nEngrave\n70\n0\n62\n1\n\
0\nENDTAB\n0\nENDSEC\n\
0\nSECTION\n2\nBLOCKS\n\
0\nBLOCK\n2\nBoltHole\n10\n0.0\n20\n0.0\n\
0\nCIRCLE\n8\n0\n10\n0.0\n20\n0.0\n40\n0.5\n\
0\nINSERT\n8\n0\n2\nBoltHole\n10\n0.0\n20\n0.0\n\
0\nENDBLK\n\
0\nENDSEC\n\
0\nSECTION\n2\nENTITIES\n\
0\nLWPOLYLINE\n8\nCutContour\n70\n1\n10\n0.0\n20\n0.0\n42\n1.0\n10\n2.0\n20\n0.0\n\
0\nARC\n8\nEngrave\n10\n1.0\n20\n1.0\n40\n0.5\n50\n0.0\n51\n180.0\n\
0\nELLIPSE\n8\nEngrave\n10\n3.0\n20\n1.0\n11\n1.0\n21\n0.0\n40\n0.5\n41\n0.0\n42\n6.283185\n\
0\nSOLID\n8\nEngrave\n10\n0.0\n20\n2.0\n11\n1.0\n21\n2.0\n12\n0.0\n22\n3.0\n13\n1.0\n23\n3.0\n\
0\nMTEXT\n8\nEngrave\n10\n0.5\n20\n3.5\n40\n0.25\n1\n{\\fArial;PART A\\P%%c10}\n\
0\nINSERT\n8\nCutContour\n2\nBoltHole\n10\n2.0\n20\n2.0\n41\n1.0\n42\n1.0\n50\n0.0\n\
0\nENDSEC\n0\nEOF\n";

        let doc = import_dxf_bytes(dxf.as_bytes(), "Inch Bracket").unwrap();
        // 6 entities (self-recursive INSERT inside BoltHole must terminate safely without duplicating!)
        assert_eq!(doc.object_count(), 6);

        let cut = doc.pages()[0]
            .layers
            .iter()
            .find(|l| l.name == "CutContour")
            .unwrap();
        assert!(cut.is_cut_layer);
        // LWPOLYLINE (2 inches wide = 50.8 mm) + INSERT circle (diameter 1 inch = 25.4 mm)
        assert_eq!(cut.objects.len(), 2);
        let poly_bb = doc.object_bounds(cut.objects[0]).unwrap();
        assert!((poly_bb.width() - 50.8).abs() < 0.2);
        let circle_bb = doc.object_bounds(cut.objects[1]).unwrap();
        assert!((circle_bb.width() - 25.4).abs() < 0.2);

        let text = doc
            .objects()
            .values()
            .find_map(|o| match &o.payload {
                NodePayload::ArtisticText(t) => Some(t.plain_text()),
                _ => None,
            })
            .unwrap();
        assert_eq!(text, "PART A\n∅10");
    }

    #[test]
    fn plt_roundtrip_export_then_import_and_hpgl_primitives() {
        let mut doc = Document::new("Sticker Sheet", 100.0, 50.0, ColorMode::Cmyk, 300.0);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Decal Rect",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 10.0, 40.0, 20.0,
                ))),
            ),
            None,
        )
        .unwrap();

        let plt = export_plt_to_string(&doc, 0).unwrap();
        let imported = import_plt_bytes(plt.as_bytes(), "Sticker Sheet").unwrap();
        assert_eq!(imported.object_count(), 1);
        let (&oid, _) = imported.objects().iter().next().unwrap();
        let bb = imported.object_bounds(oid).unwrap();
        assert!((bb.width() - 40.0).abs() < 0.1);
        assert!((bb.height() - 20.0).abs() < 0.1);

        // Multi-pen HPGL with CI, EA, AA, PR, PW, LB
        let rich_plt =
            "IN;SP1;PU400,400;CI400;EA1200,800;SP2;PW0.5,2;PU1600,400;PD;PR400,0;AA1600,400,90;PU;LBPLOT\x03;SP0;";
        let rich = import_plt_bytes(rich_plt.as_bytes(), "Rich HPGL").unwrap();
        assert_eq!(rich.pages()[0].layers.len(), 2); // CutContour (Pen 1) + Layer 1 (Pen 2)
        assert!(rich.pages()[0].layers.iter().any(|l| l.is_cut_layer));
        assert!(rich.objects().values().any(|o| matches!(
            &o.payload,
            NodePayload::ArtisticText(t) if t.plain_text() == "PLOT"
        )));
    }

    #[test]
    fn cad_adversarial_hpgl_utf8_dxf_inf_angles_dag_bomb_and_aci_min() {
        // 1. HPGL DT/LB with multi-byte UTF-8 terminator & Arabic label must NEVER panic!
        let utf8_plt = "IN;SP1;PU400,400;DT\u{060C};LBمرحبا\u{060C};SP0;";
        let plt_doc = import_plt_bytes(utf8_plt.as_bytes(), "Utf8Plt").unwrap();
        assert!(plt_doc.objects().values().any(|o| matches!(
            &o.payload,
            NodePayload::ArtisticText(t) if t.plain_text() == "مرحبا"
        )));
        // Also test multi-byte char directly after DT without `;`
        let tricky_plt = "IN;SP1;PU0,0;DTéLBcafééSP0;";
        let tricky_doc = import_plt_bytes(tricky_plt.as_bytes(), "Utf8Dt").unwrap();
        assert!(tricky_doc.objects().values().any(|o| matches!(
            &o.payload,
            NodePayload::ArtisticText(t) if t.plain_text() == "caf"
        )));

        // 2. DXF ARC & ELLIPSE with inf / NaN / 1e20 angles AND ACI i32::MIN must finish cleanly!
        let extreme_dxf = "\
0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nLAYER\n\
0\nLAYER\n2\nMinAci\n70\n0\n62\n-2147483648\n\
0\nENDTAB\n0\nENDSEC\n\
0\nSECTION\n2\nENTITIES\n\
0\nARC\n8\nMinAci\n62\n-2147483648\n10\n0\n20\n0\n40\n10\n50\n1e20\n51\n0\n\
0\nARC\n8\nMinAci\n10\n0\n20\n0\n40\n10\n50\ninf\n51\nNaN\n\
0\nELLIPSE\n8\nMinAci\n10\n0\n20\n0\n11\n10\n21\n0\n40\n0.5\n41\n1e20\n42\n0\n\
0\nELLIPSE\n8\nMinAci\n10\n0\n20\n0\n11\n10\n21\n0\n40\n0.5\n41\ninf\n42\n0\n\
0\nENDSEC\n0\nEOF\n";
        let ext_doc = import_dxf_bytes(extreme_dxf.as_bytes(), "ExtremeDxf").unwrap();
        assert_eq!(ext_doc.pages().len(), 1);
        assert_eq!(ext_doc.object_count(), 4);

        // 3. Exponential DXF Block DAG ("Billion Laughs": 22 levels * 2x = 4M entities) -> LimitExceeded!
        let mut dag = String::from("0\nSECTION\n2\nBLOCKS\n");
        dag.push_str(
            "0\nBLOCK\n2\nB0\n10\n0\n20\n0\n0\nLINE\n8\n0\n10\n0\n20\n0\n11\n1\n21\n1\n0\nENDBLK\n",
        );
        for lvl in 1..=22 {
            let prev = lvl - 1;
            let _ = write!(
                dag,
                "0\nBLOCK\n2\nB{lvl}\n10\n0\n20\n0\n\
                 0\nINSERT\n8\n0\n2\nB{prev}\n10\n0\n20\n0\n\
                 0\nINSERT\n8\n0\n2\nB{prev}\n10\n1\n20\n0\n\
                 0\nENDBLK\n"
            );
        }
        dag.push_str("0\nENDSEC\n0\nSECTION\n2\nENTITIES\n0\nINSERT\n8\n0\n2\nB22\n10\n0\n20\n0\n0\nENDSEC\n0\nEOF\n");
        let err = import_dxf_bytes(dag.as_bytes(), "DagBomb").unwrap_err();
        assert!(matches!(err, IoError::LimitExceeded { .. }));
    }

    #[test]
    fn dxf_exports_native_circle_and_arc_entities_with_exact_roundtrip() {
        let mut doc = Document::new("CAD Circle Arc Test", 100.0, 100.0, ColorMode::Cmyk, 300.0);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Bolt Hole",
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: 15.0,
                    ry: 15.0,
                    mode: EllipseMode::Full,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
            )
            .with_transform(Affine2D::translate(50.0, 40.0)),
            None,
        )
        .unwrap();
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Lead Arc",
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: 20.0,
                    ry: 20.0,
                    mode: EllipseMode::Arc,
                    start_deg: 0.0,
                    end_deg: 90.0,
                }),
            )
            .with_transform(Affine2D::translate(30.0, 60.0)),
            None,
        )
        .unwrap();

        let dxf = export_dxf_to_string(&doc, 0).unwrap();
        assert!(dxf.contains("0\nCIRCLE\n"));
        assert!(dxf.contains("0\nARC\n"));

        let imported = import_dxf_bytes(dxf.as_bytes(), "Roundtrip").unwrap();
        assert_eq!(imported.object_count(), 2);
        let hole_bb = imported
            .objects()
            .values()
            .filter_map(|o| imported.object_bounds(o.id))
            .find(|b| (b.width() - 30.0).abs() < 0.2 && (b.height() - 30.0).abs() < 0.2);
        assert!(
            hole_bb.is_some(),
            "imported DXF circle must keep 30x30 mm diameter"
        );
    }
}
