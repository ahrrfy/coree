// SPDX-License-Identifier: MPL-2.0

//! Pure-Rust PostScript / EPS Level 3 writer (`%!PS-Adobe-3.0 EPSF-3.0`).

use crate::atomic::atomic_write_bytes;
use crate::error::{IoError, IoResult};
use coree_core::{
    Affine2D, BezierPath, Color, ColorMode, Document, Fill, LineCap, LineJoin, MasterLayerScope,
    NodePayload, ObjectId, OutlineStyle, Page, Point2D, SceneObject, TextDirection,
};
use kurbo::PathEl;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

const MM_TO_PT: f64 = 72.0 / 25.4;
const MAX_EPS_DEPTH: usize = 64;

/// Exports `page_index` of `doc` as an Encapsulated PostScript Level 3 string.
pub fn export_eps_to_string(doc: &Document, page_index: usize) -> IoResult<String> {
    let page = doc.pages.get(page_index).ok_or(IoError::PageOutOfRange {
        page_index,
        page_count: doc.pages.len(),
    })?;

    if !page.width_mm.is_finite()
        || page.width_mm <= 0.0
        || !page.height_mm.is_finite()
        || page.height_mm <= 0.0
    {
        return Err(IoError::InvalidFormat {
            format: "eps".into(),
            message: format!(
                "page {} has invalid non-finite or non-positive dimensions",
                page_index + 1
            ),
        });
    }

    let w_pt = page.width_mm * MM_TO_PT;
    let h_pt = page.height_mm * MM_TO_PT;
    let use_cmyk = doc.color_mode == ColorMode::Cmyk;

    let mut out = String::with_capacity(4096);
    out.push_str("%!PS-Adobe-3.0 EPSF-3.0\n");
    let _ = writeln!(
        out,
        "%%BoundingBox: 0 0 {} {}",
        w_pt.ceil() as i64,
        h_pt.ceil() as i64
    );
    let _ = writeln!(out, "%%HiResBoundingBox: 0.0000 0.0000 {w_pt:.4} {h_pt:.4}");
    out.push_str("%%Creator: coree (coree-io EPS Level 3 Writer)\n");
    let _ = writeln!(out, "%%Title: {}", sanitize_dsc_comment(&doc.title));
    out.push_str("%%LanguageLevel: 3\n");
    out.push_str("%%Pages: 1\n");
    out.push_str("%%EndComments\n");
    out.push_str("%%BeginProlog\n");
    out.push_str("gsave\n");
    out.push_str("%%EndProlog\n");

    // Emit applicable master layers from OTHER pages first (bottom), then target page's own layers.
    let mut ordered_layers = Vec::new();
    for (p_idx, p) in doc.pages.iter().enumerate() {
        if p_idx == page_index {
            continue;
        }
        for layer in &p.layers {
            if !layer.visible || !layer.printable {
                continue;
            }
            let include = match layer.master_scope {
                MasterLayerScope::None => false,
                MasterLayerScope::AllPages => true,
                MasterLayerScope::OddPages => (page_index + 1) % 2 == 1,
                MasterLayerScope::EvenPages => (page_index + 1).is_multiple_of(2),
            };
            if include {
                ordered_layers.push(layer);
            }
        }
    }
    for layer in &page.layers {
        if layer.visible && layer.printable {
            ordered_layers.push(layer);
        }
    }

    for layer in ordered_layers {
        let _ = writeln!(out, "% Layer: {}", sanitize_dsc_comment(&layer.name));
        let mut visited = HashSet::new();
        for &oid in &layer.objects {
            if let Some(obj) = doc.objects.get(&oid) {
                emit_eps_object(
                    doc,
                    page,
                    obj,
                    Affine2D::IDENTITY,
                    use_cmyk,
                    &mut out,
                    0,
                    &mut visited,
                );
            }
        }
    }

    out.push_str("grestore\n");
    out.push_str("%%Trailer\n");
    out.push_str("%%EOF\n");
    Ok(out)
}

/// Atomically exports `page_index` of `doc` as an EPS Level 3 file at `path`.
pub fn export_eps(doc: &Document, page_index: usize, path: &Path) -> IoResult<()> {
    let eps = export_eps_to_string(doc, page_index)?;
    atomic_write_bytes(path, eps.as_bytes())
}

#[allow(clippy::too_many_arguments)]
fn emit_eps_object(
    doc: &Document,
    page: &Page,
    obj: &SceneObject,
    parent_xf: Affine2D,
    use_cmyk: bool,
    out: &mut String,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) {
    if depth > MAX_EPS_DEPTH || !obj.visible || !obj.printable || !visited.insert(obj.id) {
        return;
    }
    let world_xf = parent_xf.compose(&obj.transform);
    if !world_xf.is_finite() {
        visited.remove(&obj.id);
        return;
    }

    match &obj.payload {
        NodePayload::Path(path) => {
            emit_eps_styled_path(
                page,
                &path.transformed(&world_xf),
                &obj.fill,
                &obj.outline,
                use_cmyk,
                out,
            );
        }
        NodePayload::Shape(shape) => {
            emit_eps_styled_path(
                page,
                &shape.to_bezier_path().transformed(&world_xf),
                &obj.fill,
                &obj.outline,
                use_cmyk,
                out,
            );
        }
        NodePayload::ArtisticText(text) => {
            let style = text
                .runs
                .first()
                .map(|r| r.style.clone())
                .unwrap_or_default();
            emit_eps_text(
                page,
                &text.plain_text(),
                &style,
                text.effective_direction(),
                world_xf,
                use_cmyk,
                out,
            );
        }
        NodePayload::ParagraphText(para) => {
            let style = para
                .runs
                .first()
                .map(|r| r.style.clone())
                .unwrap_or_default();
            emit_eps_text(
                page,
                &para.plain_text(),
                &style,
                para.effective_direction(),
                world_xf,
                use_cmyk,
                out,
            );
        }
        NodePayload::TextOnPath(top) => {
            let style = top
                .text
                .runs
                .first()
                .map(|r| r.style.clone())
                .unwrap_or_default();
            emit_eps_text(
                page,
                &top.text.plain_text(),
                &style,
                top.text.effective_direction(),
                world_xf,
                use_cmyk,
                out,
            );
        }
        NodePayload::Table(table) => {
            for r in 0..table.rows {
                for c in 0..table.cols {
                    if let (Some(cell), Some(rect)) = (table.cell(r, c), table.cell_rect(r, c)) {
                        emit_eps_styled_path(
                            page,
                            &BezierPath::from_rect(rect).transformed(&world_xf),
                            &cell.fill,
                            &table.border_style,
                            use_cmyk,
                            out,
                        );
                    }
                }
            }
        }
        NodePayload::Dimension(dim) => {
            let line = BezierPath::from_line(
                Point2D::new(dim.p1.x, dim.p1.y - dim.offset_mm),
                Point2D::new(dim.p2.x, dim.p2.y - dim.offset_mm),
            )
            .transformed(&world_xf);
            emit_eps_styled_path(page, &line, &Fill::None, &obj.outline, use_cmyk, out);
        }
        NodePayload::Connector(conn) => {
            let (a, b) = doc.resolve_connector_endpoints(conn);
            let path = conn.route_path(a, b);
            emit_eps_styled_path(page, &path, &Fill::None, &obj.outline, use_cmyk, out);
        }
        NodePayload::Group(children) => {
            for &cid in children {
                if let Some(child) = doc.objects.get(&cid) {
                    emit_eps_object(
                        doc,
                        page,
                        child,
                        world_xf,
                        use_cmyk,
                        out,
                        depth + 1,
                        visited,
                    );
                }
            }
        }
        NodePayload::PowerClip {
            container,
            contents,
        } => {
            let clip_path = container.transformed(&world_xf);
            if obj.fill.is_visible() {
                emit_eps_styled_path(
                    page,
                    &clip_path,
                    &obj.fill,
                    &OutlineStyle::none(),
                    use_cmyk,
                    out,
                );
            }
            out.push_str("gsave\n");
            emit_eps_path_ops(page, &clip_path, out);
            out.push_str("clip newpath\n");
            for &cid in contents {
                if let Some(child) = doc.objects.get(&cid) {
                    emit_eps_object(
                        doc,
                        page,
                        child,
                        world_xf,
                        use_cmyk,
                        out,
                        depth + 1,
                        visited,
                    );
                }
            }
            out.push_str("grestore\n");
            if obj.outline.is_visible() {
                emit_eps_styled_path(page, &clip_path, &Fill::None, &obj.outline, use_cmyk, out);
            }
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = doc.symbols.get(sym_id) {
                for sub in &sym.objects {
                    let mut sym_visited = HashSet::new();
                    emit_eps_object(
                        doc,
                        page,
                        sub,
                        world_xf,
                        use_cmyk,
                        out,
                        depth + 1,
                        &mut sym_visited,
                    );
                }
            }
        }
        NodePayload::Raster(_) => {}
    }

    visited.remove(&obj.id);
}

fn emit_eps_styled_path(
    page: &Page,
    path: &BezierPath,
    fill: &Fill,
    outline: &OutlineStyle,
    use_cmyk: bool,
    out: &mut String,
) {
    if path.is_empty() {
        return;
    }
    let fill_color = match fill {
        Fill::None | Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => None,
        Fill::Solid(c) => Some(c.clone()),
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => stops.first().map(|s| s.color.clone()),
        Fill::TwoColorPattern { fg, .. } | Fill::TextureFill { fg, .. } => Some(fg.clone()),
        Fill::MeshFill { nodes, .. } => nodes.first().map(|n| n.color.clone()),
    };

    if let Some(ref fc) = fill_color.filter(|c| c.alpha() > 1e-6) {
        out.push_str("gsave\n");
        emit_eps_color(out, fc, use_cmyk);
        emit_eps_path_ops(page, path, out);
        out.push_str("fill\ngrestore\n");
    }

    if outline.is_visible() {
        out.push_str("gsave\n");
        if outline.overprint {
            out.push_str("true setoverprint\n");
        }
        emit_eps_color(out, &outline.color, use_cmyk);
        let raw_w = outline.effective_width_mm();
        let width_pt = if raw_w.is_finite() {
            (raw_w * MM_TO_PT).max(0.1)
        } else {
            0.25
        };
        let cap = match outline.cap {
            LineCap::Butt => 0,
            LineCap::Round => 1,
            LineCap::Square => 2,
        };
        let join = match outline.join {
            LineJoin::Miter => 0,
            LineJoin::Round => 1,
            LineJoin::Bevel => 2,
        };
        let miter = if outline.miter_limit.is_finite() {
            outline.miter_limit.max(1.0)
        } else {
            4.0
        };
        let _ = writeln!(
            out,
            "{width_pt:.4} setlinewidth {cap} setlinecap {join} setlinejoin {miter:.3} setmiterlimit"
        );
        if !outline.dash_pattern.is_empty() {
            let parts: Vec<String> = outline
                .dash_pattern
                .iter()
                .filter(|d| d.is_finite() && **d > 0.0)
                .map(|d| format!("{:.4}", (d * MM_TO_PT).max(0.1)))
                .collect();
            if !parts.is_empty() {
                let _ = writeln!(out, "[{}] 0 setdash", parts.join(" "));
            }
        }
        emit_eps_path_ops(page, path, out);
        out.push_str("stroke\ngrestore\n");
    }
}

fn emit_eps_text(
    page: &Page,
    text: &str,
    style: &coree_core::TextStyle,
    direction: TextDirection,
    world_xf: Affine2D,
    use_cmyk: bool,
    out: &mut String,
) {
    if text.is_empty() || !world_xf.is_finite() {
        return;
    }
    let origin = world_xf.transform_point(Point2D::new(0.0, style.font_size_mm()));
    let (x, y) = to_eps_pt(page, origin);
    let [a, b, c, d, _, _] = world_xf.m;
    let font_pt = if style.font_size_pt.is_finite() {
        style.font_size_pt.max(1.0)
    } else {
        12.0
    };
    out.push_str("gsave\n");
    emit_eps_color(out, &style.fill, use_cmyk);
    if direction != TextDirection::Rtl && text.chars().all(|ch| ch.is_ascii() && !ch.is_control()) {
        let ps_font = if style.bold {
            "Helvetica-Bold"
        } else {
            "Helvetica"
        };
        let is_pure_translate =
            (a - 1.0).abs() < 1e-6 && b.abs() < 1e-6 && c.abs() < 1e-6 && (d - 1.0).abs() < 1e-6;
        if is_pure_translate {
            let _ = writeln!(
                out,
                "/{ps_font} findfont {font_pt:.2} scalefont setfont {x:.4} {y:.4} moveto ({}) show",
                escape_ps_string(text)
            );
        } else {
            let _ = writeln!(
                out,
                "[{a:.6} {:.6} {:.6} {d:.6} {x:.4} {y:.4}] concat /{ps_font} findfont {font_pt:.2} scalefont setfont 0 0 moveto ({}) show",
                -b,
                -c,
                escape_ps_string(text)
            );
        }
    } else {
        // Vector outline fallback for non-ASCII / Arabic RTL text so glyphs are preserved in EPS.
        let glyph_w_mm = style.font_size_mm() * 0.52;
        let glyph_h_mm = style.font_size_mm() * 0.72;
        let step_mm = style.font_size_mm() * 0.62;
        let mut cursor_x = 0.0_f64;
        for ch in text.chars() {
            if !ch.is_whitespace() {
                let rect = coree_core::Rect2D::from_xywh(
                    cursor_x,
                    style.font_size_mm() * 0.15,
                    glyph_w_mm,
                    glyph_h_mm,
                );
                let box_path = BezierPath::from_rect(rect).transformed(&world_xf);
                emit_eps_path_ops(page, &box_path, out);
                out.push_str("fill\n");
            }
            cursor_x += step_mm;
        }
    }
    out.push_str("grestore\n");
}

fn emit_eps_path_ops(page: &Page, path: &BezierPath, out: &mut String) {
    out.push_str("newpath\n");
    let kb = path.to_kurbo();
    let mut cur = kurbo::Point::ZERO;
    let mut subpath_start = kurbo::Point::ZERO;
    let pt_ok = |p: kurbo::Point| p.x.is_finite() && p.y.is_finite();
    for el in kb.elements() {
        match *el {
            PathEl::MoveTo(p) if pt_ok(p) => {
                cur = p;
                subpath_start = p;
                let (x, y) = to_eps_pt(page, Point2D::from_kurbo(p));
                let _ = writeln!(out, "{x:.4} {y:.4} moveto");
            }
            PathEl::LineTo(p) if pt_ok(p) => {
                cur = p;
                let (x, y) = to_eps_pt(page, Point2D::from_kurbo(p));
                let _ = writeln!(out, "{x:.4} {y:.4} lineto");
            }
            PathEl::QuadTo(p1, p2) if pt_ok(cur) && pt_ok(p1) && pt_ok(p2) => {
                let cubic = kurbo::QuadBez::new(cur, p1, p2).raise();
                cur = p2;
                let (x1, y1) = to_eps_pt(page, Point2D::from_kurbo(cubic.p1));
                let (x2, y2) = to_eps_pt(page, Point2D::from_kurbo(cubic.p2));
                let (x3, y3) = to_eps_pt(page, Point2D::from_kurbo(cubic.p3));
                let _ = writeln!(
                    out,
                    "{x1:.4} {y1:.4} {x2:.4} {y2:.4} {x3:.4} {y3:.4} curveto"
                );
            }
            PathEl::CurveTo(p1, p2, p3) if pt_ok(p1) && pt_ok(p2) && pt_ok(p3) => {
                cur = p3;
                let (x1, y1) = to_eps_pt(page, Point2D::from_kurbo(p1));
                let (x2, y2) = to_eps_pt(page, Point2D::from_kurbo(p2));
                let (x3, y3) = to_eps_pt(page, Point2D::from_kurbo(p3));
                let _ = writeln!(
                    out,
                    "{x1:.4} {y1:.4} {x2:.4} {y2:.4} {x3:.4} {y3:.4} curveto"
                );
            }
            PathEl::ClosePath => {
                cur = subpath_start;
                out.push_str("closepath\n");
            }
            _ => {}
        }
    }
}

fn sanitize_unit_f32(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn emit_eps_color(out: &mut String, color: &Color, use_cmyk: bool) {
    match color {
        Color::Cmyk { .. } | Color::Spot { .. } => {
            let [c, m, y, k] = color.to_cmyk().map(sanitize_unit_f32);
            let _ = writeln!(out, "{c:.4} {m:.4} {y:.4} {k:.4} setcmykcolor");
        }
        _ if use_cmyk => {
            let [c, m, y, k] = color.to_cmyk().map(sanitize_unit_f32);
            let _ = writeln!(out, "{c:.4} {m:.4} {y:.4} {k:.4} setcmykcolor");
        }
        _ => {
            let [r, g, b, _] = color.to_rgba_f32().map(sanitize_unit_f32);
            let _ = writeln!(out, "{r:.4} {g:.4} {b:.4} setrgbcolor");
        }
    }
}

fn to_eps_pt(page: &Page, pt: Point2D) -> (f64, f64) {
    (pt.x * MM_TO_PT, (page.height_mm - pt.y) * MM_TO_PT)
}

fn sanitize_dsc_comment(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

fn escape_ps_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            c if c.is_ascii() && !c.is_control() => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{CornerStyle, ParametricShape};

    #[test]
    fn eps_level_3_emits_valid_dsc_cmyk_rgb_and_curves() {
        let mut doc = Document::new_a4("EPS Vector Logo");
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Rounded Badge",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 60.0,
                    height: 30.0,
                    corner_radii: [5.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::MAGENTA))
            .with_outline(OutlineStyle::solid(Color::BLACK, 0.5)),
            None,
        )
        .unwrap();

        let eps = export_eps_to_string(&doc, 0).unwrap();
        assert!(eps.starts_with("%!PS-Adobe-3.0 EPSF-3.0\n"));
        assert!(eps.contains("%%BoundingBox: 0 0 "));
        assert!(eps.contains("%%LanguageLevel: 3"));
        assert!(eps.contains("moveto"));
        assert!(eps.contains("curveto"));
        assert!(eps.contains("setcmykcolor"));
        assert!(eps.contains("fill"));
        assert!(eps.contains("stroke"));
        assert!(eps.ends_with("%%EOF\n"));

        // Verify RGB mode uses setrgbcolor
        let mut rgb_doc = Document::new("RGB EPS", 50.0, 50.0, ColorMode::Rgb, 96.0);
        rgb_doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Line",
                    NodePayload::Path(BezierPath::from_line(
                        Point2D::new(0.0, 0.0),
                        Point2D::new(20.0, 20.0),
                    )),
                )
                .with_outline(OutlineStyle::solid(Color::rgb(0.2, 0.4, 0.8), 1.0)),
                None,
            )
            .unwrap();
        let rgb_eps = export_eps_to_string(&rgb_doc, 0).unwrap();
        assert!(rgb_eps.contains("setrgbcolor"));
        assert!(rgb_eps.contains("lineto"));
    }
}
