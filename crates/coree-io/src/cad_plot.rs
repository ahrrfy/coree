// SPDX-License-Identifier: MPL-2.0

//! CAD & cutting-plotter exporters:
//! - AutoCAD ASCII **DXF** (`AC1015` metric millimeters with `HEADER`, `TABLES`, `CutContour`
//!   layer definitions, `LWPOLYLINE`, `SPLINE`, `LINE`, `EOF`) for CNC routers & laser cutters.
//! - **PLT (HPGL)** (`IN;SP1;PU...;PD...;SP0;` at `1 mm = 40 HPGL units`) for Roland / Graphtec
//!   vinyl cutting plotters.

use crate::atomic::atomic_write_bytes;
use crate::error::{IoError, IoResult};
use coree_core::{
    Affine2D, BezierPath, Document, Layer, MasterLayerScope, NodePayload, ObjectId, Page, Point2D,
    SceneObject,
};
use kurbo::PathSeg;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

/// Standard HPGL plotter resolution (`1 mm = 40 HPGL units`, i.e., `0.025 mm/step`).
pub const HPGL_UNITS_PER_MM: f64 = 40.0;
const MAX_CAD_DEPTH: usize = 64;

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
    let page = doc.pages.get(page_index).ok_or(IoError::PageOutOfRange {
        page_index,
        page_count: doc.pages.len(),
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
        let mut paths = Vec::new();
        let mut visited = HashSet::new();
        for &oid in &layer.objects {
            if let Some(obj) = doc.objects.get(&oid) {
                collect_vector_paths(doc, obj, Affine2D::IDENTITY, &mut paths, 0, &mut visited);
            }
        }
        for path in &paths {
            emit_dxf_path(page, path, &layer_name, opts, &mut out);
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
    let page = doc.pages.get(page_index).ok_or(IoError::PageOutOfRange {
        page_index,
        page_count: doc.pages.len(),
    })?;

    let layers = collect_page_layers(doc, page_index, opts.cut_layers_only);

    let mut out = String::with_capacity(2048);
    out.push_str("IN;");

    let mut pen_idx = 0usize;
    for layer in &layers {
        let mut paths = Vec::new();
        let mut visited = HashSet::new();
        for &oid in &layer.objects {
            if let Some(obj) = doc.objects.get(&oid) {
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
    for (p_idx, p) in doc.pages.iter().enumerate() {
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
    if let Some(page) = doc.pages.get(page_index) {
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
        NodePayload::Table(t) => out.push(t.to_grid_paths().transformed(&world_xf)),
        NodePayload::Connector(c) => {
            let (a, b) = doc.resolve_connector_endpoints(c);
            out.push(c.route_path(a, b));
        }
        NodePayload::PowerClip { container, .. } => out.push(container.transformed(&world_xf)),
        NodePayload::Group(children) => {
            for &cid in children {
                if let Some(child) = doc.objects.get(&cid) {
                    collect_vector_paths(doc, child, world_xf, out, depth + 1, visited);
                }
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = doc.symbols.get(sym_id) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{CornerStyle, OutlineStyle, ParametricShape};

    #[test]
    fn dxf_export_contains_header_tables_cutcontour_lwpolyline_spline_line_and_eof() {
        let mut doc = Document::new("CNC Panel", 100.0, 50.0, coree_core::ColorMode::Cmyk, 300.0);
        let page_id = doc.pages[0].id;
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
}
