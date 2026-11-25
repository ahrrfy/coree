// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust PostScript / EPS Level 3 writer (`%!PS-Adobe-3.0 EPSF-3.0`) and
//! hardened EPS / PostScript vector & text importer.

use crate::atomic::atomic_write_bytes;
use crate::error::{IoError, IoResult, DEFAULT_MAX_SVG_BYTES};
use crate::{resolve_export_connector_endpoints, resolve_export_world_path, ExportSymCtx};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, Color, ColorMode, Document, Effect, Fill, GradientStop,
    LineCap, LineJoin, MasterLayerScope, NodePayload, ObjectId, OutlineStyle, Page, Perspective3x3,
    Point2D, SceneObject, SymbolDef, TextDirection, TextStyle,
};
use coree_render::effects_render::{apply_geometry_effects, symmetry_effect_transforms};
use coree_render::{outline_arrowhead_geometries, render_isolated_object_to_rgba, TextEngine};
use kurbo::{BezPath, PathEl};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

const MM_TO_PT: f64 = 72.0 / 25.4;
const PT_TO_MM: f64 = 25.4 / 72.0;
const MAX_EPS_DEPTH: usize = 64;
const MAX_PS_GSTATE_DEPTH: usize = 128;
const MAX_PS_CALL_DEPTH: usize = 32;
const MAX_PS_STEPS: usize = 2_000_000;
const MAX_PS_TOKENS: usize = 2_000_000;
const MAX_PS_STACK_LEN: usize = 4_096;
const MAX_PS_STACK_NODES: usize = 100_000;
const MAX_EPS_INPUT_BYTES: usize = DEFAULT_MAX_SVG_BYTES * 2; // 128 MiB

/// Exports `page_index` of `doc` as an Encapsulated PostScript Level 3 string.
pub fn export_eps_to_string(doc: &Document, page_index: usize) -> IoResult<String> {
    let page = doc.page(page_index).ok_or(IoError::PageOutOfRange {
        page_index,
        page_count: doc.pages().len(),
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
    let use_cmyk = doc.color_mode() == ColorMode::Cmyk;

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
    let _ = writeln!(out, "%%Title: {}", sanitize_dsc_comment(doc.title()));
    out.push_str("%%LanguageLevel: 3\n");
    out.push_str("%%Pages: 1\n");
    out.push_str("%%EndComments\n");
    out.push_str("%%BeginProlog\n");
    out.push_str("gsave\n");
    out.push_str("%%EndProlog\n");

    // Emit applicable master layers from OTHER pages first (bottom), then target page's own layers.
    let mut ordered_layers = Vec::new();
    for (p_idx, p) in doc.pages().iter().enumerate() {
        if p_idx == page_index {
            continue;
        }
        for layer in &p.layers {
            if !layer.visible || !layer.printable {
                continue;
            }
            let include = layer.is_desktop_layer
                || match layer.master_scope {
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

    let mut text_engine = TextEngine::new();
    for layer in ordered_layers {
        let _ = writeln!(out, "% Layer: {}", sanitize_dsc_comment(&layer.name));
        let mut visited = HashSet::new();
        for &oid in &layer.objects {
            if let Some(obj) = doc.object(oid) {
                emit_eps_object(
                    doc,
                    page,
                    page_index,
                    obj,
                    Affine2D::IDENTITY,
                    None,
                    None,
                    use_cmyk,
                    &mut text_engine,
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

fn eps_requires_raster_composite(doc: &Document, obj: &SceneObject) -> bool {
    let mut visited_objs = HashSet::new();
    let mut visited_syms = HashSet::new();
    eps_requires_raster_composite_inner(
        doc,
        obj,
        None,
        false,
        &mut visited_objs,
        &mut visited_syms,
        0,
    )
}

fn eps_requires_raster_composite_inner(
    doc: &Document,
    obj: &SceneObject,
    sym_map: Option<&HashMap<ObjectId, &SceneObject>>,
    in_forced_vector_ancestor: bool,
    visited_objs: &mut HashSet<ObjectId>,
    visited_syms: &mut HashSet<coree_core::SymbolId>,
    depth: usize,
) -> bool {
    if depth > MAX_EPS_DEPTH
        || (depth > 0 && (!obj.visible || !obj.printable))
        || (obj.id.0 != 0 && !visited_objs.insert(obj.id))
    {
        return false;
    }
    let has_raster_fx = obj.effects.iter().any(|e| {
        matches!(
            e,
            Effect::DropShadow { .. }
                | Effect::BlockShadow { .. }
                | Effect::Contour { .. }
                | Effect::Extrude25D { .. }
                | Effect::Blend { .. }
                | Effect::Transparency { .. }
                | Effect::InnerShadow { .. }
                | Effect::Bevel { .. }
                | Effect::Extrude3D { .. }
        )
    });
    let result = if obj.lens.is_some() || has_raster_fx {
        true
    } else {
        match &obj.payload {
            NodePayload::Group(children) => {
                eps_has_geometry_effect(&obj.effects)
                    || ((in_forced_vector_ancestor || obj.perspective.is_some())
                        && children.iter().any(|cid| {
                            sym_map
                                .and_then(|m| m.get(cid).copied())
                                .or_else(|| doc.object(*cid))
                                .is_some_and(|child| {
                                    eps_requires_raster_composite_inner(
                                        doc,
                                        child,
                                        sym_map,
                                        true,
                                        visited_objs,
                                        visited_syms,
                                        depth + 1,
                                    )
                                })
                        }))
            }
            NodePayload::SymbolInstance(sym_id) => {
                eps_has_geometry_effect(&obj.effects)
                    || (visited_syms.insert(*sym_id) && {
                        let hit = doc.symbol(*sym_id).is_some_and(|sym| {
                            let (child_map, referenced) = build_eps_symbol_maps(sym);
                            let mut sym_visited = HashSet::new();
                            sym.objects.iter().any(|sub| {
                                !(sub.id.0 != 0 && referenced.contains(&sub.id))
                                    && eps_requires_raster_composite_inner(
                                        doc,
                                        sub,
                                        Some(&child_map),
                                        true,
                                        &mut sym_visited,
                                        visited_syms,
                                        depth + 1,
                                    )
                            })
                        });
                        visited_syms.remove(sym_id);
                        hit
                    })
            }
            NodePayload::PowerClip { contents, .. } => {
                let own_fill = matches!(
                    obj.fill,
                    Fill::ConicalGradient { .. }
                        | Fill::SquareGradient { .. }
                        | Fill::MeshFill { .. }
                        | Fill::TwoColorPattern { .. }
                        | Fill::VectorPattern { .. }
                        | Fill::BitmapPattern { .. }
                        | Fill::TextureFill { .. }
                );
                own_fill
                    || ((in_forced_vector_ancestor || obj.perspective.is_some())
                        && contents.iter().any(|cid| {
                            sym_map
                                .and_then(|m| m.get(cid).copied())
                                .or_else(|| doc.object(*cid))
                                .is_some_and(|child| {
                                    eps_requires_raster_composite_inner(
                                        doc,
                                        child,
                                        sym_map,
                                        true,
                                        visited_objs,
                                        visited_syms,
                                        depth + 1,
                                    )
                                })
                        }))
            }
            _ => matches!(
                obj.fill,
                Fill::ConicalGradient { .. }
                    | Fill::SquareGradient { .. }
                    | Fill::MeshFill { .. }
                    | Fill::TwoColorPattern { .. }
                    | Fill::VectorPattern { .. }
                    | Fill::BitmapPattern { .. }
                    | Fill::TextureFill { .. }
            ),
        }
    };
    if obj.id.0 != 0 {
        visited_objs.remove(&obj.id);
    }
    result
}

fn eps_has_geometry_effect(effects: &[Effect]) -> bool {
    effects.iter().any(|e| {
        matches!(
            e,
            Effect::Distort { .. } | Effect::Envelope { .. } | Effect::Perspective { .. }
        )
    })
}

fn compose_eps_parent_persp(
    parent_persp: Option<Perspective3x3>,
    local_xf: Affine2D,
    world_xf: Affine2D,
    own_persp: Option<Perspective3x3>,
) -> Option<Perspective3x3> {
    match (parent_persp, own_persp) {
        (Some(pp), Some(op)) => Some(
            pp.compose(&Perspective3x3::from_affine(local_xf))
                .compose(&op),
        ),
        (Some(pp), None) => Some(pp.compose(&Perspective3x3::from_affine(local_xf))),
        (None, Some(op)) => Some(Perspective3x3::from_affine(world_xf).compose(&op)),
        (None, None) => None,
    }
}

fn transform_eps_local_path(
    local_deformed: &BezierPath,
    local_xf: Affine2D,
    world_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
) -> BezierPath {
    if let Some(pp) = parent_persp {
        pp.compose(&Perspective3x3::from_affine(local_xf))
            .transform_bezier_path(local_deformed)
    } else {
        local_deformed.transformed(&world_xf)
    }
}

fn build_eps_symbol_maps(sym: &SymbolDef) -> (HashMap<ObjectId, &SceneObject>, HashSet<ObjectId>) {
    let mut child_map: HashMap<ObjectId, &SceneObject> = HashMap::new();
    let mut referenced: HashSet<ObjectId> = HashSet::new();
    for sub in &sym.objects {
        if sub.id.0 != 0 {
            child_map.insert(sub.id, sub);
        }
        match &sub.payload {
            NodePayload::Group(c) => referenced.extend(c.iter().copied()),
            NodePayload::PowerClip { contents, .. } => referenced.extend(contents.iter().copied()),
            _ => {}
        }
    }
    (child_map, referenced)
}

#[allow(clippy::too_many_arguments)]
fn emit_eps_rgba_image(
    page: &Page,
    width: u32,
    height: u32,
    rgba_bytes: &[u8],
    bl: Point2D,
    br: Point2D,
    tl: Point2D,
    use_cmyk: bool,
    out: &mut String,
) {
    if width == 0 || height == 0 || rgba_bytes.len() < (width as usize) * (height as usize) * 4 {
        return;
    }
    let (x0, y0) = to_eps_pt(page, bl);
    let (x1, y1) = to_eps_pt(page, br);
    let (x2, y2) = to_eps_pt(page, tl);

    out.push_str("gsave\n");
    let _ = writeln!(
        out,
        "[{:.4} {:.4} {:.4} {:.4} {:.4} {:.4}] concat",
        x1 - x0,
        y1 - y0,
        x2 - x0,
        y2 - y0,
        x0,
        y0
    );
    if use_cmyk {
        out.push_str("/DeviceCMYK setcolorspace\n");
    } else {
        out.push_str("/DeviceRGB setcolorspace\n");
    }
    let decode = if use_cmyk {
        "[0 1 0 1 0 1 0 1]"
    } else {
        "[0 1 0 1 0 1]"
    };
    let _ = writeln!(
        out,
        "<<\n  /ImageType 1\n  /Width {width}\n  /Height {height}\n  /BitsPerComponent 8\n  /Decode {decode}\n  /ImageMatrix [{width} 0 0 -{height} 0 {height}]\n  /DataSource currentfile /ASCIIHexDecode filter\n>> image"
    );

    let mut hex_line_len = 0usize;
    let total_px = (width as usize) * (height as usize);
    for px in rgba_bytes[..total_px * 4].as_chunks::<4>().0 {
        // Composite over opaque paper white since PostScript has no native alpha channel
        let a = f32::from(px[3]) / 255.0;
        let r = (f32::from(px[0]) * a + 255.0 * (1.0 - a))
            .round()
            .clamp(0.0, 255.0) as u8;
        let g = (f32::from(px[1]) * a + 255.0 * (1.0 - a))
            .round()
            .clamp(0.0, 255.0) as u8;
        let b = (f32::from(px[2]) * a + 255.0 * (1.0 - a))
            .round()
            .clamp(0.0, 255.0) as u8;
        if use_cmyk {
            let c = Color::rgba8(r, g, b, 255).to_cmyk();
            for ch in c {
                let byte = (ch * 255.0).round().clamp(0.0, 255.0) as u8;
                let _ = write!(out, "{byte:02X}");
                hex_line_len += 2;
                if hex_line_len >= 76 {
                    out.push('\n');
                    hex_line_len = 0;
                }
            }
        } else {
            for byte in [r, g, b] {
                let _ = write!(out, "{byte:02X}");
                hex_line_len += 2;
                if hex_line_len >= 76 {
                    out.push('\n');
                    hex_line_len = 0;
                }
            }
        }
    }
    if hex_line_len > 0 {
        out.push('\n');
    }
    out.push_str(">\ngrestore\n");
}

#[allow(clippy::too_many_arguments)]
fn emit_eps_object(
    doc: &Document,
    page: &Page,
    page_index: usize,
    obj: &SceneObject,
    parent_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
    sym_ctx: Option<ExportSymCtx<'_>>,
    use_cmyk: bool,
    text_engine: &mut TextEngine,
    out: &mut String,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) {
    if depth > MAX_EPS_DEPTH
        || !obj.visible
        || !obj.printable
        || (obj.id.0 != 0 && !visited.insert(obj.id))
    {
        return;
    }
    if !parent_xf.is_finite() || !obj.transform.is_finite() {
        if obj.id.0 != 0 {
            visited.remove(&obj.id);
        }
        return;
    }

    if eps_requires_raster_composite(doc, obj)
        && sym_ctx.is_none()
        && obj.id.0 != 0
        && parent_persp.is_none()
    {
        if let Ok(Some((rect_mm, rgba_buf))) =
            render_isolated_object_to_rgba(doc, page_index, obj.id, parent_xf, 300.0)
        {
            emit_eps_rgba_image(
                page,
                rgba_buf.width,
                rgba_buf.height,
                &rgba_buf.rgba,
                Point2D::new(rect_mm.min_x, rect_mm.max_y),
                Point2D::new(rect_mm.max_x, rect_mm.max_y),
                Point2D::new(rect_mm.min_x, rect_mm.min_y),
                use_cmyk,
                out,
            );
            visited.remove(&obj.id);
            return;
        }
    }

    let sym_transforms = symmetry_effect_transforms(&obj.effects);
    for sym_xf in sym_transforms {
        let local_xf = sym_xf.compose(&obj.transform);
        let effective_parent = parent_xf.compose(&sym_xf);
        let world_xf = effective_parent.compose(&obj.transform);
        if !world_xf.is_finite() {
            continue;
        }

        match &obj.payload {
            NodePayload::Path(path) => {
                let bp = if let Some(persp) = obj.perspective {
                    persp.transform_bezier_path(path)
                } else {
                    path.clone()
                };
                let deformed = apply_geometry_effects(&bp, &obj.effects);
                let out_path =
                    transform_eps_local_path(&deformed, local_xf, world_xf, parent_persp);
                emit_eps_styled_path(
                    page,
                    &out_path,
                    &obj.fill,
                    &obj.outline,
                    world_xf,
                    use_cmyk,
                    out,
                );
            }
            NodePayload::Shape(shape) => {
                let bp = if let Some(persp) = obj.perspective {
                    persp.transform_bezier_path(&shape.to_bezier_path())
                } else {
                    shape.to_bezier_path()
                };
                let deformed = apply_geometry_effects(&bp, &obj.effects);
                let out_path =
                    transform_eps_local_path(&deformed, local_xf, world_xf, parent_persp);
                emit_eps_styled_path(
                    page,
                    &out_path,
                    &obj.fill,
                    &obj.outline,
                    world_xf,
                    use_cmyk,
                    out,
                );
            }
            NodePayload::BSpline(bs) => {
                let bp = if let Some(persp) = obj.perspective {
                    persp.transform_bezier_path(&bs.to_bezier_path())
                } else {
                    bs.to_bezier_path()
                };
                let deformed = apply_geometry_effects(&bp, &obj.effects);
                let out_path =
                    transform_eps_local_path(&deformed, local_xf, world_xf, parent_persp);
                emit_eps_styled_path(
                    page,
                    &out_path,
                    &obj.fill,
                    &obj.outline,
                    world_xf,
                    use_cmyk,
                    out,
                );
            }
            NodePayload::ArtisticMedia { spine, style } => {
                let raw = coree_core::ArtisticMedia::expand_stroke(spine, style);
                let bp = if let Some(persp) = obj.perspective {
                    persp.transform_bezier_path(&raw)
                } else {
                    raw
                };
                let deformed = apply_geometry_effects(&bp, &obj.effects);
                let out_path =
                    transform_eps_local_path(&deformed, local_xf, world_xf, parent_persp);
                emit_eps_styled_path(
                    page,
                    &out_path,
                    &obj.fill,
                    &obj.outline,
                    world_xf,
                    use_cmyk,
                    out,
                );
            }
            NodePayload::Clone { .. } => {
                if let Some((p, f, o)) = doc.resolve_clone_payload_and_style(obj) {
                    let bp = match &p {
                        NodePayload::Path(path) => Some(path.clone()),
                        NodePayload::Shape(s) => Some(s.to_bezier_path()),
                        NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
                        NodePayload::ArtisticMedia { spine, style } => {
                            Some(coree_core::ArtisticMedia::expand_stroke(spine, style))
                        }
                        _ => None,
                    };
                    if let Some(raw) = bp {
                        let wp = if let Some(persp) = obj.perspective {
                            persp.transform_bezier_path(&raw)
                        } else {
                            raw
                        };
                        let deformed = apply_geometry_effects(&wp, &obj.effects);
                        let out_path =
                            transform_eps_local_path(&deformed, local_xf, world_xf, parent_persp);
                        emit_eps_styled_path(page, &out_path, &f, &o, world_xf, use_cmyk, out);
                    }
                }
            }
            NodePayload::ArtisticText(text) => {
                if parent_persp.is_some() || obj.perspective.is_some() {
                    let pieces = text_engine.layout_artistic_pieces(text);
                    for piece in &pieces {
                        let bp = if let Some(persp) = obj.perspective {
                            persp.transform_bezier_path(&piece.path)
                        } else {
                            piece.path.clone()
                        };
                        let deformed = apply_geometry_effects(&bp, &obj.effects);
                        let out_path =
                            transform_eps_local_path(&deformed, local_xf, world_xf, parent_persp);
                        emit_eps_styled_path(
                            page,
                            &out_path,
                            &Fill::Solid(piece.fill.clone()),
                            &obj.outline,
                            world_xf,
                            use_cmyk,
                            out,
                        );
                    }
                } else {
                    emit_eps_artistic_text(
                        page,
                        text,
                        &obj.outline,
                        &obj.effects,
                        world_xf,
                        use_cmyk,
                        text_engine,
                        out,
                    );
                }
            }
            NodePayload::ParagraphText(para) => {
                let pieces = text_engine.layout_paragraph_in_doc(doc, obj.id, para);
                if parent_persp.is_some() || obj.perspective.is_some() {
                    for piece in &pieces {
                        let bp = if let Some(persp) = obj.perspective {
                            persp.transform_bezier_path(&piece.path)
                        } else {
                            piece.path.clone()
                        };
                        let deformed = apply_geometry_effects(&bp, &obj.effects);
                        let out_path =
                            transform_eps_local_path(&deformed, local_xf, world_xf, parent_persp);
                        emit_eps_styled_path(
                            page,
                            &out_path,
                            &Fill::Solid(piece.fill.clone()),
                            &obj.outline,
                            world_xf,
                            use_cmyk,
                            out,
                        );
                    }
                } else {
                    emit_eps_shaped_pieces(
                        page,
                        &pieces,
                        &obj.outline,
                        &obj.effects,
                        world_xf,
                        use_cmyk,
                        out,
                    );
                }
            }
            NodePayload::TextOnPath(top) => {
                if let Some(host_path) = resolve_export_world_path(doc, top.path_id, sym_ctx) {
                    let inv_parent = parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                    let mut pieces = text_engine.layout_text_on_path_pieces(top, &host_path);
                    if inv_parent != Affine2D::IDENTITY {
                        for piece in &mut pieces {
                            piece.path = piece.path.transformed(&inv_parent);
                        }
                    }
                    if parent_persp.is_some() || obj.perspective.is_some() {
                        for piece in &pieces {
                            let bp = if let Some(persp) = obj.perspective {
                                persp.transform_bezier_path(&piece.path)
                            } else {
                                piece.path.clone()
                            };
                            let deformed = apply_geometry_effects(&bp, &obj.effects);
                            let out_path = transform_eps_local_path(
                                &deformed,
                                sym_xf,
                                effective_parent,
                                parent_persp,
                            );
                            emit_eps_styled_path(
                                page,
                                &out_path,
                                &Fill::Solid(piece.fill.clone()),
                                &obj.outline,
                                effective_parent,
                                use_cmyk,
                                out,
                            );
                        }
                    } else {
                        emit_eps_shaped_pieces(
                            page,
                            &pieces,
                            &obj.outline,
                            &obj.effects,
                            effective_parent,
                            use_cmyk,
                            out,
                        );
                    }
                } else if parent_persp.is_some() || obj.perspective.is_some() {
                    let pieces = text_engine.layout_artistic_pieces(&top.text);
                    for piece in &pieces {
                        let bp = if let Some(persp) = obj.perspective {
                            persp.transform_bezier_path(&piece.path)
                        } else {
                            piece.path.clone()
                        };
                        let deformed = apply_geometry_effects(&bp, &obj.effects);
                        let out_path =
                            transform_eps_local_path(&deformed, local_xf, world_xf, parent_persp);
                        emit_eps_styled_path(
                            page,
                            &out_path,
                            &Fill::Solid(piece.fill.clone()),
                            &obj.outline,
                            world_xf,
                            use_cmyk,
                            out,
                        );
                    }
                } else {
                    emit_eps_artistic_text(
                        page,
                        &top.text,
                        &obj.outline,
                        &obj.effects,
                        world_xf,
                        use_cmyk,
                        text_engine,
                        out,
                    );
                }
            }
            NodePayload::Table(table) => {
                for r in 0..table.rows {
                    for c in 0..table.cols {
                        if let (Some(cell), Some(rect)) = (table.cell(r, c), table.cell_rect(r, c))
                        {
                            let cell_path = transform_eps_local_path(
                                &BezierPath::from_rect(rect),
                                local_xf,
                                world_xf,
                                parent_persp,
                            );
                            emit_eps_styled_path(
                                page,
                                &cell_path,
                                &cell.fill,
                                &table.border_style,
                                world_xf,
                                use_cmyk,
                                out,
                            );
                            if !cell.text.is_empty() {
                                let tx = Affine2D::translate(
                                    rect.min_x + cell.padding_mm,
                                    rect.min_y + cell.padding_mm,
                                );
                                let at = ArtisticText::from_plain(
                                    cell.text.clone(),
                                    cell.text_style.clone(),
                                );
                                if parent_persp.is_some() {
                                    for piece in text_engine.layout_artistic_pieces(&at) {
                                        let p = transform_eps_local_path(
                                            &piece.path,
                                            local_xf.compose(&tx),
                                            world_xf.compose(&tx),
                                            parent_persp,
                                        );
                                        emit_eps_styled_path(
                                            page,
                                            &p,
                                            &Fill::Solid(piece.fill),
                                            &OutlineStyle::none(),
                                            world_xf.compose(&tx),
                                            use_cmyk,
                                            out,
                                        );
                                    }
                                } else {
                                    emit_eps_artistic_text(
                                        page,
                                        &at,
                                        &OutlineStyle::none(),
                                        &[],
                                        world_xf.compose(&tx),
                                        use_cmyk,
                                        text_engine,
                                        out,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            NodePayload::Dimension(dim) => {
                let line = transform_eps_local_path(
                    &BezierPath::from_line(
                        Point2D::new(dim.p1.x, dim.p1.y - dim.offset_mm),
                        Point2D::new(dim.p2.x, dim.p2.y - dim.offset_mm),
                    ),
                    local_xf,
                    world_xf,
                    parent_persp,
                );
                emit_eps_styled_path(
                    page,
                    &line,
                    &Fill::None,
                    &obj.outline,
                    world_xf,
                    use_cmyk,
                    out,
                );
                let label =
                    dim.compute_measurement_with_dpi(doc.default_unit(), f64::from(doc.dpi()));
                let mid = dim.p1.midpoint(dim.p2);
                let at = ArtisticText::from_plain(label, dim.text_style.clone());
                let tx = Affine2D::translate(mid.x, mid.y - dim.offset_mm - 2.0);
                if parent_persp.is_some() {
                    for piece in text_engine.layout_artistic_pieces(&at) {
                        let p = transform_eps_local_path(
                            &piece.path,
                            local_xf.compose(&tx),
                            world_xf.compose(&tx),
                            parent_persp,
                        );
                        emit_eps_styled_path(
                            page,
                            &p,
                            &Fill::Solid(piece.fill),
                            &OutlineStyle::none(),
                            world_xf.compose(&tx),
                            use_cmyk,
                            out,
                        );
                    }
                } else {
                    emit_eps_artistic_text(
                        page,
                        &at,
                        &OutlineStyle::none(),
                        &[],
                        world_xf.compose(&tx),
                        use_cmyk,
                        text_engine,
                        out,
                    );
                }
            }
            NodePayload::Connector(conn) => {
                let (a, b) = resolve_export_connector_endpoints(doc, conn, &world_xf, sym_ctx);
                let path = conn.route_path(a, b);
                let out_path = if let Some(pp) = parent_persp {
                    let local_path = parent_xf
                        .inverse()
                        .map_or_else(|| path.clone(), |inv| path.transformed(&inv));
                    pp.transform_bezier_path(&local_path)
                } else {
                    path
                };
                emit_eps_styled_path(
                    page,
                    &out_path,
                    &Fill::None,
                    &obj.outline,
                    world_xf,
                    use_cmyk,
                    out,
                );
            }
            NodePayload::Group(children) => {
                let child_persp =
                    compose_eps_parent_persp(parent_persp, local_xf, world_xf, obj.perspective);
                for &cid in children {
                    let child_opt = sym_ctx
                        .and_then(|(m, _, _)| m.get(&cid).copied())
                        .or_else(|| doc.object(cid));
                    if let Some(child) = child_opt {
                        emit_eps_object(
                            doc,
                            page,
                            page_index,
                            child,
                            world_xf,
                            child_persp,
                            sym_ctx,
                            use_cmyk,
                            text_engine,
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
                ..
            } => {
                let container_bp = if let Some(persp) = obj.perspective {
                    persp.transform_bezier_path(container)
                } else {
                    (**container).clone()
                };
                let clip_path = transform_eps_local_path(
                    &apply_geometry_effects(&container_bp, &obj.effects),
                    local_xf,
                    world_xf,
                    parent_persp,
                );
                let child_persp =
                    compose_eps_parent_persp(parent_persp, local_xf, world_xf, obj.perspective);
                if obj.fill.is_visible() {
                    emit_eps_styled_path(
                        page,
                        &clip_path,
                        &obj.fill,
                        &OutlineStyle::none(),
                        world_xf,
                        use_cmyk,
                        out,
                    );
                }
                out.push_str("gsave\n");
                emit_eps_path_ops(page, &clip_path, out);
                out.push_str("clip newpath\n");
                for &cid in contents {
                    let child_opt = sym_ctx
                        .and_then(|(m, _, _)| m.get(&cid).copied())
                        .or_else(|| doc.object(cid));
                    if let Some(child) = child_opt {
                        emit_eps_object(
                            doc,
                            page,
                            page_index,
                            child,
                            world_xf,
                            child_persp,
                            sym_ctx,
                            use_cmyk,
                            text_engine,
                            out,
                            depth + 1,
                            visited,
                        );
                    }
                }
                out.push_str("grestore\n");
                if obj.outline.is_visible() {
                    emit_eps_styled_path(
                        page,
                        &clip_path,
                        &Fill::None,
                        &obj.outline,
                        world_xf,
                        use_cmyk,
                        out,
                    );
                }
            }
            NodePayload::SymbolInstance(sym_id) => {
                if let Some(sym) = doc.symbol(*sym_id) {
                    let (child_map, referenced) = build_eps_symbol_maps(sym);
                    let child_persp =
                        compose_eps_parent_persp(parent_persp, local_xf, world_xf, obj.perspective);
                    let mut sym_visited = HashSet::new();
                    for sub in &sym.objects {
                        if sub.id.0 != 0 && referenced.contains(&sub.id) {
                            continue;
                        }
                        emit_eps_object(
                            doc,
                            page,
                            page_index,
                            sub,
                            world_xf,
                            child_persp,
                            Some((&child_map, &sym.objects, world_xf)),
                            use_cmyk,
                            text_engine,
                            out,
                            depth + 1,
                            &mut sym_visited,
                        );
                    }
                }
            }
            NodePayload::Raster(raster) => {
                if let Some(asset) = doc.asset(raster.asset_id) {
                    let bl = world_xf.transform_point(Point2D::new(0.0, raster.height_mm));
                    let br =
                        world_xf.transform_point(Point2D::new(raster.width_mm, raster.height_mm));
                    let tl = world_xf.transform_point(Point2D::new(0.0, 0.0));
                    let rgba8 = asset.to_rgba8_vec();
                    emit_eps_rgba_image(
                        page,
                        asset.width,
                        asset.height,
                        &rgba8,
                        bl,
                        br,
                        tl,
                        use_cmyk,
                        out,
                    );
                }
            }
        }
    }

    visited.remove(&obj.id);
}

fn format_eps_color_comps(c: &Color, use_cmyk: bool) -> String {
    match c {
        Color::Cmyk { .. } | Color::Spot { .. } => {
            let [cc, m, y, k] = c.to_cmyk().map(sanitize_unit_f32);
            format!("{cc:.4} {m:.4} {y:.4} {k:.4}")
        }
        _ if use_cmyk => {
            let [cc, m, y, k] = c.to_cmyk().map(sanitize_unit_f32);
            format!("{cc:.4} {m:.4} {y:.4} {k:.4}")
        }
        _ => {
            let [r, g, b, _] = c.to_rgba_f32().map(sanitize_unit_f32);
            format!("{r:.4} {g:.4} {b:.4}")
        }
    }
}

fn build_eps_gradient_function_dict(stops: &[GradientStop], use_cmyk: bool) -> Option<String> {
    if stops.is_empty() {
        return None;
    }
    let mut sorted = stops.to_vec();
    sorted.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    if sorted.len() == 1 {
        let c = format_eps_color_comps(&sorted[0].color, use_cmyk);
        return Some(format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [{c}] /C1 [{c}] /N 1 >>"
        ));
    }
    if sorted.len() == 2 {
        let c0 = format_eps_color_comps(&sorted[0].color, use_cmyk);
        let c1 = format_eps_color_comps(&sorted[1].color, use_cmyk);
        return Some(format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >>"
        ));
    }
    let mut segments: Vec<(f32, f32, &Color, &Color)> = Vec::new();
    if sorted[0].offset > 1e-4 {
        segments.push((0.0, sorted[0].offset, &sorted[0].color, &sorted[0].color));
    }
    for win in sorted.windows(2) {
        if win[1].offset > win[0].offset + 1e-5 {
            segments.push((win[0].offset, win[1].offset, &win[0].color, &win[1].color));
        }
    }
    let last = sorted.last()?;
    if last.offset < 1.0 - 1e-4 {
        segments.push((last.offset, 1.0, &last.color, &last.color));
    }
    if segments.is_empty() {
        let c0 = format_eps_color_comps(&sorted[0].color, use_cmyk);
        let c1 = format_eps_color_comps(&last.color, use_cmyk);
        return Some(format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >>"
        ));
    }
    let mut fns = Vec::new();
    let mut bounds = Vec::new();
    let mut encode = Vec::new();
    for (i, &(_t0, t1, c0_col, c1_col)) in segments.iter().enumerate() {
        let c0 = format_eps_color_comps(c0_col, use_cmyk);
        let c1 = format_eps_color_comps(c1_col, use_cmyk);
        fns.push(format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [{c1}] /N 1 >>"
        ));
        if i + 1 < segments.len() {
            bounds.push(format!("{t1:.4}"));
        }
        encode.push("0 1".to_string());
    }
    Some(format!(
        "<< /FunctionType 3 /Domain [0 1] /Functions [{}] /Bounds [{}] /Encode [{}] >>",
        fns.join(" "),
        bounds.join(" "),
        encode.join(" ")
    ))
}

fn emit_eps_gradient_fill(
    page: &Page,
    path: &BezierPath,
    fill: &Fill,
    fill_xf: Affine2D,
    use_cmyk: bool,
    out: &mut String,
) -> bool {
    let cs = if use_cmyk {
        "/DeviceCMYK"
    } else {
        "/DeviceRGB"
    };
    match fill {
        Fill::LinearGradient { start, end, stops } if !stops.is_empty() => {
            let Some(func_dict) = build_eps_gradient_function_dict(stops, use_cmyk) else {
                return false;
            };
            let (x0, y0) = to_eps_pt(page, fill_xf.transform_point(*start));
            let (mut x1, mut y1) = to_eps_pt(page, fill_xf.transform_point(*end));
            if (x1 - x0).hypot(y1 - y0) < 1e-3 {
                x1 = x0 + 1.0;
                y1 = y0;
            }
            out.push_str("gsave\n");
            emit_eps_path_ops(page, path, out);
            out.push_str("eoclip newpath\n");
            let _ = writeln!(
                out,
                "<< /ShadingType 2 /ColorSpace {cs} /Coords [{x0:.4} {y0:.4} {x1:.4} {y1:.4}] \
                 /Function {func_dict} /Extend [true true] >> shfill"
            );
            out.push_str("grestore\n");
            true
        }
        Fill::RadialGradient {
            center,
            radius,
            focal,
            stops,
        } if !stops.is_empty() => {
            let Some(func_dict) = build_eps_gradient_function_dict(stops, use_cmyk) else {
                return false;
            };
            let (cx, cy) = to_eps_pt(page, fill_xf.transform_point(*center));
            let (fx, fy) = to_eps_pt(page, fill_xf.transform_point(*focal));
            let (ex, ey) = to_eps_pt(
                page,
                fill_xf.transform_point(Point2D::new(center.x + radius.max(0.1), center.y)),
            );
            let r_pt = (ex - cx).hypot(ey - cy).max(0.5);
            out.push_str("gsave\n");
            emit_eps_path_ops(page, path, out);
            out.push_str("eoclip newpath\n");
            let _ = writeln!(
                out,
                "<< /ShadingType 3 /ColorSpace {cs} /Coords [{fx:.4} {fy:.4} 0.0000 {cx:.4} {cy:.4} {r_pt:.4}] \
                 /Function {func_dict} /Extend [true true] >> shfill"
            );
            out.push_str("grestore\n");
            true
        }
        _ => false,
    }
}

fn emit_eps_styled_path(
    page: &Page,
    path: &BezierPath,
    fill: &Fill,
    outline: &OutlineStyle,
    fill_xf: Affine2D,
    use_cmyk: bool,
    out: &mut String,
) {
    if path.is_empty() {
        return;
    }

    let emit_stroke_block = |out: &mut String| {
        if !outline.is_visible() {
            return;
        }
        out.push_str("gsave\n");
        if outline.overprint {
            out.push_str("true setoverprint\n");
        }
        emit_eps_color(out, &outline.color, use_cmyk);
        let raw_w = outline.scaled_width_mm_for_transform(&fill_xf);
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
        out.push_str("stroke\n");
        for (geom, filled) in outline_arrowhead_geometries(path, outline) {
            emit_eps_path_ops(page, &geom, out);
            out.push_str(if filled { "fill\n" } else { "stroke\n" });
        }
        out.push_str("grestore\n");
    };

    if outline.behind_fill {
        emit_stroke_block(out);
    }

    if !emit_eps_gradient_fill(page, path, fill, fill_xf, use_cmyk, out) {
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
            out.push_str("eofill\ngrestore\n");
        }
    }

    if !outline.behind_fill {
        emit_stroke_block(out);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_eps_artistic_text(
    page: &Page,
    text: &ArtisticText,
    outline: &OutlineStyle,
    effects: &[Effect],
    world_xf: Affine2D,
    use_cmyk: bool,
    text_engine: &mut TextEngine,
    out: &mut String,
) {
    let plain = text.plain_text();
    if plain.is_empty() || !world_xf.is_finite() {
        return;
    }
    let style = text
        .runs
        .first()
        .map(|r| r.style.clone())
        .unwrap_or_default();
    let is_simple_ascii_helvetica = !eps_has_geometry_effect(effects)
        && (!outline.is_visible() || *outline == OutlineStyle::default())
        && text.runs.len() == 1
        && text.effective_direction() != TextDirection::Rtl
        && plain.chars().all(|ch| ch.is_ascii() && !ch.is_control())
        && !style.italic
        && !style.underline
        && !style.strikethrough
        && (style.font_family.trim().is_empty()
            || style.font_family.eq_ignore_ascii_case("Noto Sans")
            || style.font_family.eq_ignore_ascii_case("Helvetica")
            || style.font_family.eq_ignore_ascii_case("Arial"));

    if is_simple_ascii_helvetica {
        emit_eps_ascii_type1_text(page, &plain, &style, world_xf, use_cmyk, out);
        return;
    }

    let pieces = text_engine.layout_artistic_pieces(text);
    emit_eps_shaped_pieces(page, &pieces, outline, effects, world_xf, use_cmyk, out);
}

#[allow(clippy::too_many_arguments)]
fn emit_eps_shaped_pieces(
    page: &Page,
    pieces: &[coree_render::ShapedGlyphDraw],
    outline: &OutlineStyle,
    effects: &[Effect],
    world_xf: Affine2D,
    use_cmyk: bool,
    out: &mut String,
) {
    let has_geom = eps_has_geometry_effect(effects);
    for piece in pieces {
        if piece.path.is_empty() {
            continue;
        }
        let local = if has_geom {
            apply_geometry_effects(&piece.path, effects)
        } else {
            piece.path.clone()
        };
        let world_path = local.transformed(&world_xf);
        let fill = if piece.fill.alpha() > 1e-6 {
            Fill::Solid(piece.fill.clone())
        } else {
            Fill::None
        };
        emit_eps_styled_path(page, &world_path, &fill, outline, world_xf, use_cmyk, out);
    }
}

fn emit_eps_ascii_type1_text(
    page: &Page,
    text: &str,
    style: &TextStyle,
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

// =============================================================================
// EPS / PostScript Importer
// =============================================================================

/// Imports an Encapsulated PostScript (`.eps`) or PostScript file from `path`.
pub fn import_eps(path: &Path) -> IoResult<Document> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((MAX_EPS_INPUT_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_EPS_INPUT_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_eps_bytes={MAX_EPS_INPUT_BYTES}"),
            actual: format!("file exceeds {MAX_EPS_INPUT_BYTES} bytes"),
        });
    }
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Imported EPS");
    import_eps_bytes(&bytes, title)
}

/// Imports an EPS (ASCII or DOS binary `C5 D0 D3 C6`) or PostScript byte stream into a [`Document`].
pub fn import_eps_bytes(bytes: &[u8], title: &str) -> IoResult<Document> {
    if bytes.is_empty() {
        return Err(IoError::InvalidFormat {
            format: "eps".into(),
            message: "empty EPS input".into(),
        });
    }
    if bytes.len() > MAX_EPS_INPUT_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_eps_bytes={MAX_EPS_INPUT_BYTES}"),
            actual: format!("{} bytes", bytes.len()),
        });
    }

    let ps_slice = unwrap_dos_binary_eps(bytes)?;
    let dsc = parse_eps_dsc(ps_slice);

    // Validate that the payload actually looks like PostScript/EPS
    let trimmed_start = ps_slice
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(ps_slice.len());
    let head = &ps_slice[trimmed_start..];
    let has_ps_header = head.starts_with(b"%!")
        || head
            .windows(4)
            .take(1024)
            .any(|w| w == b"%!PS" || w == b"EPSF");
    if !has_ps_header && dsc.bbox.is_none() {
        // Also allow bare PostScript snippets that contain known drawing operators
        let preview = String::from_utf8_lossy(&head[..head.len().min(4096)]);
        let looks_like_ps = [
            "moveto",
            "lineto",
            "curveto",
            "rectfill",
            "rectstroke",
            "show",
        ]
        .iter()
        .any(|kw| preview.contains(kw));
        if !looks_like_ps {
            return Err(IoError::InvalidFormat {
                format: "eps".into(),
                message: "missing '%!PS' PostScript header or bounding box".into(),
            });
        }
    }

    let tokens = tokenize_ps(ps_slice)?;
    let mut interp = PsInterpreter::new();
    interp.exec_tokens(&tokens, 0)?;

    // Determine page bounds in EPS points
    let bbox = dsc.bbox.or_else(|| {
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for item in &interp.emitted {
            if let Some(bb) = item.eps_bounds() {
                min_x = min_x.min(bb.min_x);
                min_y = min_y.min(bb.min_y);
                max_x = max_x.max(bb.max_x);
                max_y = max_y.max(bb.max_y);
            }
        }
        if min_x.is_finite() && max_x > min_x && max_y > min_y {
            let pad = 10.0 * MM_TO_PT;
            Some([min_x - pad, min_y - pad, max_x + pad, max_y + pad])
        } else {
            None
        }
    });

    let [llx, lly, urx, ury] = bbox.unwrap_or([0.0, 0.0, 210.0 * MM_TO_PT, 297.0 * MM_TO_PT]);
    let w_pt = (urx - llx).abs().max(1.0);
    let h_pt = (ury - lly).abs().max(1.0);
    let min_x = llx.min(urx);
    let max_y = lly.max(ury);
    let page_w_mm = (w_pt * PT_TO_MM).max(1.0);
    let page_h_mm = (h_pt * PT_TO_MM).max(1.0);

    let eps_to_mm = Affine2D::new(
        PT_TO_MM,
        0.0,
        0.0,
        -PT_TO_MM,
        -min_x * PT_TO_MM,
        max_y * PT_TO_MM,
    );

    let color_mode = if interp.saw_cmyk && !interp.saw_rgb {
        ColorMode::Cmyk
    } else {
        ColorMode::Rgb
    };
    let doc_title = dsc
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| {
            if title.trim().is_empty() {
                "Imported EPS".to_owned()
            } else {
                title.to_owned()
            }
        });

    let mut doc = Document::new(doc_title, page_w_mm, page_h_mm, color_mode, 300.0);
    doc.metadata_mut().source_format = Some("eps".to_owned());

    for (idx, item) in interp.emitted.into_iter().enumerate() {
        match item {
            EpsEmitted::Path {
                eps_path,
                fill,
                outline,
            } => {
                let mut bez = BezierPath::from_kurbo(&eps_path);
                bez.transform(&eps_to_mm);
                if bez.is_empty() {
                    continue;
                }
                let obj = SceneObject::new(
                    ObjectId(0),
                    format!("EPS Path {}", idx + 1),
                    NodePayload::Path(bez),
                )
                .with_fill(fill)
                .with_outline(outline);
                doc.add_object(obj, None)?;
            }
            EpsEmitted::Text {
                text,
                font_family,
                font_size_pt,
                bold,
                italic,
                color,
                ctm,
                origin_eps,
            } => {
                if text.is_empty() {
                    continue;
                }
                let world_xf = eps_to_mm.compose(&ctm);
                let text_local_to_eps = Affine2D::new(
                    MM_TO_PT,
                    0.0,
                    0.0,
                    -MM_TO_PT,
                    origin_eps.x,
                    origin_eps.y + font_size_pt,
                );
                let text_xf = world_xf.compose(&text_local_to_eps);
                if !text_xf.is_finite() {
                    continue;
                }
                let style = TextStyle {
                    font_family,
                    font_size_pt: font_size_pt.max(1.0),
                    bold,
                    italic,
                    fill: color,
                    direction: TextDirection::Auto,
                    ..TextStyle::default()
                };
                let obj = SceneObject::new(
                    ObjectId(0),
                    format!("EPS Text {}", idx + 1),
                    NodePayload::ArtisticText(ArtisticText::from_plain(text, style)),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none())
                .with_transform(text_xf);
                doc.add_object(obj, None)?;
            }
            EpsEmitted::Raster {
                width,
                height,
                rgba_bytes,
                ctm,
            } => {
                if let Ok(asset) =
                    coree_core::RasterAsset::new(width, height, rgba_bytes, 300.0, doc.color_mode())
                {
                    let asset_id = doc.register_asset(asset);
                    // PostScript image maps [0,1]x[0,1] bottom-to-top in CTM -> convert corners via `eps_to_mm * ctm`
                    let world_xf = eps_to_mm.compose(&ctm);
                    let tl_mm = world_xf.transform_point(Point2D::new(0.0, 1.0));
                    let br_x_mm = world_xf.transform_point(Point2D::new(1.0, 1.0));
                    let bl_mm = world_xf.transform_point(Point2D::new(0.0, 0.0));
                    let w_mm = (br_x_mm - tl_mm).length().max(0.1);
                    let h_mm = (bl_mm - tl_mm).length().max(0.1);
                    let raster_xf = Affine2D::translate(tl_mm.x, tl_mm.y);
                    let obj = SceneObject::new(
                        ObjectId(0),
                        format!("EPS Raster {}", idx + 1),
                        NodePayload::Raster(coree_core::RasterLayerData {
                            asset_id,
                            width_mm: w_mm,
                            height_mm: h_mm,
                            filters: Vec::new(),
                            crop_rect: None,
                        }),
                    )
                    .with_fill(Fill::None)
                    .with_outline(OutlineStyle::none())
                    .with_transform(raster_xf);
                    doc.add_object(obj, None)?;
                }
            }
        }
    }

    doc.refresh_document_palette();
    Ok(doc)
}

/// Strips a DOS Binary EPS header (`0xC5 0xD0 0xD3 0xC6`) when present, returning the
/// embedded PostScript slice; otherwise returns `bytes` unchanged.
fn unwrap_dos_binary_eps(bytes: &[u8]) -> IoResult<&[u8]> {
    if bytes.len() >= 4 && bytes[0..4] == [0xC5, 0xD0, 0xD3, 0xC6] {
        if bytes.len() < 30 {
            return Err(IoError::InvalidFormat {
                format: "eps".into(),
                message: "truncated DOS binary EPS header".into(),
            });
        }
        let ps_offset = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let ps_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        let ps_end = ps_offset
            .checked_add(ps_len)
            .ok_or_else(|| IoError::InvalidFormat {
                format: "eps".into(),
                message: "DOS binary EPS offset overflow".into(),
            })?;
        if ps_len == 0 || ps_end > bytes.len() {
            return Err(IoError::InvalidFormat {
                format: "eps".into(),
                message: format!(
                    "DOS binary EPS PostScript slice ({ps_offset}..{ps_end}) out of bounds ({})",
                    bytes.len()
                ),
            });
        }
        return Ok(&bytes[ps_offset..ps_end]);
    }
    Ok(bytes)
}

#[derive(Default)]
struct EpsDscHeader {
    bbox: Option<[f64; 4]>,
    title: Option<String>,
}

fn parse_eps_dsc(bytes: &[u8]) -> EpsDscHeader {
    let mut dsc = EpsDscHeader::default();
    let mut hires_bbox: Option<[f64; 4]> = None;
    let mut std_bbox: Option<[f64; 4]> = None;

    for raw_line in bytes.split(|&b| b == b'\n' || b == b'\r') {
        if !raw_line.starts_with(b"%%") {
            continue;
        }
        let line = String::from_utf8_lossy(raw_line);
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("%%HiResBoundingBox:") {
            if let Some(bb) = parse_four_floats(rest) {
                hires_bbox = Some(bb);
            }
        } else if let Some(rest) = trimmed.strip_prefix("%%BoundingBox:") {
            if let Some(bb) = parse_four_floats(rest) {
                std_bbox = Some(bb);
            }
        } else if let Some(rest) = trimmed.strip_prefix("%%Title:") {
            let t = rest.trim().trim_matches(|c| c == '(' || c == ')').trim();
            if !t.is_empty() && dsc.title.is_none() {
                dsc.title = Some(t.to_owned());
            }
        }
    }
    dsc.bbox = hires_bbox.or(std_bbox);
    dsc
}

fn parse_four_floats(s: &str) -> Option<[f64; 4]> {
    let mut iter = s.split_whitespace();
    let a = iter.next()?.parse::<f64>().ok()?;
    let b = iter.next()?.parse::<f64>().ok()?;
    let c = iter.next()?.parse::<f64>().ok()?;
    let d = iter.next()?.parse::<f64>().ok()?;
    if a.is_finite() && b.is_finite() && c.is_finite() && d.is_finite() {
        Some([a, b, c, d])
    } else {
        None
    }
}

#[derive(Debug, Clone)]
enum PsToken {
    Number(f64),
    LiteralName(String),
    ExecName(String),
    Str(Vec<u8>),
    Bool(bool),
    Null,
    ArrayStart,
    ArrayEnd,
    Proc(Vec<PsToken>),
}

#[derive(Debug, Clone)]
enum PsVal {
    Number(f64),
    Name(String),
    Str(Vec<u8>),
    Bool(bool),
    Null,
    Mark,
    Array(Vec<PsVal>),
    Dict(Vec<(String, PsVal)>),
    Proc(Vec<PsToken>),
    Font {
        family: String,
        size_pt: f64,
        bold: bool,
        italic: bool,
    },
}

impl PsVal {
    fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::Number(v) if v.is_finite() => Some(v),
            _ => None,
        }
    }

    fn node_count(&self, limit: usize) -> usize {
        match self {
            Self::Array(items) => {
                let mut total = 1usize;
                for item in items {
                    total = total.saturating_add(item.node_count(limit.saturating_sub(total)));
                    if total > limit {
                        return total;
                    }
                }
                total
            }
            Self::Dict(entries) => {
                let mut total = 1usize;
                for (_, val) in entries {
                    total = total.saturating_add(val.node_count(limit.saturating_sub(total)));
                    if total > limit {
                        return total;
                    }
                }
                total
            }
            Self::Proc(toks) => 1usize.saturating_add(toks.len()),
            Self::Str(b) => 1usize.saturating_add(b.len() / 64),
            _ => 1,
        }
    }
}

fn is_ps_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn tokenize_ps(bytes: &[u8]) -> IoResult<Vec<PsToken>> {
    let mut pos = 0usize;
    let mut total_tokens = 0usize;
    tokenize_ps_inner(bytes, &mut pos, false, 0, &mut total_tokens)
}

fn tokenize_ps_inner(
    bytes: &[u8],
    pos: &mut usize,
    stop_on_rbrace: bool,
    depth: usize,
    total_tokens: &mut usize,
) -> IoResult<Vec<PsToken>> {
    if depth > MAX_EPS_DEPTH {
        return Err(IoError::LimitExceeded {
            limit: format!("max_ps_proc_depth={MAX_EPS_DEPTH}"),
            actual: format!("depth={depth}"),
        });
    }
    let mut tokens = Vec::new();
    let len = bytes.len();
    let mut saw_asciihex_datasource = false;

    while *pos < len {
        if *total_tokens >= MAX_PS_TOKENS {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_tokens={MAX_PS_TOKENS}"),
                actual: format!("exceeded {MAX_PS_TOKENS} tokens"),
            });
        }
        let b = bytes[*pos];
        if b.is_ascii_whitespace() || b == 0 {
            *pos += 1;
            continue;
        }
        if b == b'%' {
            while *pos < len && bytes[*pos] != b'\n' && bytes[*pos] != b'\r' {
                *pos += 1;
            }
            continue;
        }
        if b == b'}' {
            *pos += 1;
            if stop_on_rbrace {
                return Ok(tokens);
            }
            continue;
        }
        if b == b'{' {
            *pos += 1;
            *total_tokens += 1;
            let proc_tokens = tokenize_ps_inner(bytes, pos, true, depth + 1, total_tokens)?;
            tokens.push(PsToken::Proc(proc_tokens));
            continue;
        }
        if b == b'[' {
            *pos += 1;
            *total_tokens += 1;
            tokens.push(PsToken::ArrayStart);
            continue;
        }
        if b == b']' {
            *pos += 1;
            *total_tokens += 1;
            tokens.push(PsToken::ArrayEnd);
            continue;
        }
        if b == b'(' {
            let s = read_ps_literal_string(bytes, pos);
            *total_tokens += 1;
            tokens.push(PsToken::Str(s));
            continue;
        }
        if b == b')' {
            // Unmatched closing paren — consume 1 byte cleanly to guarantee forward progress.
            *pos += 1;
            continue;
        }
        if b == b'<' {
            if *pos + 1 < len && bytes[*pos + 1] == b'<' {
                *pos += 2;
                *total_tokens += 1;
                tokens.push(PsToken::ExecName("<<".to_owned()));
            } else if *pos + 1 < len && bytes[*pos + 1] == b'~' {
                // Skip ASCII85 string literal `<~ ... ~>`
                *pos += 2;
                while *pos + 1 < len && !(bytes[*pos] == b'~' && bytes[*pos + 1] == b'>') {
                    *pos += 1;
                }
                *pos = (*pos + 2).min(len);
                *total_tokens += 1;
                tokens.push(PsToken::Str(Vec::new()));
            } else {
                let s = read_ps_hex_string(bytes, pos);
                *total_tokens += 1;
                tokens.push(PsToken::Str(s));
            }
            continue;
        }
        if b == b'>' {
            if *pos + 1 < len && bytes[*pos + 1] == b'>' {
                *pos += 2;
                *total_tokens += 1;
                tokens.push(PsToken::ExecName(">>".to_owned()));
            } else {
                *pos += 1;
            }
            continue;
        }
        if b == b'/' {
            *pos += 1;
            // Support immediately evaluated `//name` as name too
            if *pos < len && bytes[*pos] == b'/' {
                *pos += 1;
            }
            let start = *pos;
            while *pos < len && !bytes[*pos].is_ascii_whitespace() && !is_ps_delim(bytes[*pos]) {
                *pos += 1;
            }
            let name = String::from_utf8_lossy(&bytes[start..*pos]).into_owned();
            if name == "ASCIIHexDecode" {
                saw_asciihex_datasource = true;
            }
            *total_tokens += 1;
            tokens.push(PsToken::LiteralName(name));
            continue;
        }

        // Regular token (number, boolean, null, or executable name)
        let start = *pos;
        while *pos < len && !bytes[*pos].is_ascii_whitespace() && !is_ps_delim(bytes[*pos]) {
            *pos += 1;
        }
        if *pos == start {
            // Defensive progress guarantee: never spin on an unhandled delimiter byte
            *pos += 1;
            continue;
        }
        let word = String::from_utf8_lossy(&bytes[start..*pos]);
        *total_tokens += 1;
        if word == "image" && saw_asciihex_datasource {
            saw_asciihex_datasource = false;
            let raw_img = read_ps_inline_hex_stream(bytes, pos);
            tokens.push(PsToken::Str(raw_img));
            tokens.push(PsToken::ExecName("image".to_owned()));
        } else if let Some(num) = parse_ps_number(&word) {
            tokens.push(PsToken::Number(num));
        } else if word == "true" {
            tokens.push(PsToken::Bool(true));
        } else if word == "false" {
            tokens.push(PsToken::Bool(false));
        } else if word == "null" {
            tokens.push(PsToken::Null);
        } else {
            if word == "shfill" || word == "colorimage" {
                saw_asciihex_datasource = false;
            }
            tokens.push(PsToken::ExecName(word.into_owned()));
        }
    }

    Ok(tokens)
}

fn read_ps_literal_string(bytes: &[u8], pos: &mut usize) -> Vec<u8> {
    let mut out = Vec::new();
    let len = bytes.len();
    *pos += 1; // skip '('
    let mut paren_depth = 1u32;

    while *pos < len && paren_depth > 0 {
        let b = bytes[*pos];
        *pos += 1;
        match b {
            b'(' => {
                paren_depth += 1;
                out.push(b'(');
            }
            b')' => {
                paren_depth -= 1;
                if paren_depth > 0 {
                    out.push(b')');
                }
            }
            b'\\' => {
                if *pos >= len {
                    break;
                }
                let esc = bytes[*pos];
                *pos += 1;
                match esc {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b'\n' => {}
                    b'\r' => {
                        if *pos < len && bytes[*pos] == b'\n' {
                            *pos += 1;
                        }
                    }
                    b'0'..=b'7' => {
                        let mut val = u32::from(esc - b'0');
                        for _ in 0..2 {
                            if *pos < len && matches!(bytes[*pos], b'0'..=b'7') {
                                val = val * 8 + u32::from(bytes[*pos] - b'0');
                                *pos += 1;
                            } else {
                                break;
                            }
                        }
                        out.push((val & 0xFF) as u8);
                    }
                    other => out.push(other),
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn read_ps_hex_string(bytes: &[u8], pos: &mut usize) -> Vec<u8> {
    *pos += 1; // skip '<'
    read_ps_inline_hex_stream(bytes, pos)
}

fn read_ps_inline_hex_stream(bytes: &[u8], pos: &mut usize) -> Vec<u8> {
    let mut out = Vec::new();
    let len = bytes.len();
    let mut hi: Option<u8> = None;
    while *pos < len {
        let b = bytes[*pos];
        *pos += 1;
        if b == b'>' {
            break;
        }
        let nib = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue,
        };
        if let Some(h) = hi.take() {
            out.push((h << 4) | nib);
        } else {
            hi = Some(nib);
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    out
}

fn parse_ps_number(word: &str) -> Option<f64> {
    // Support PostScript radix integers like `16#FF`
    if let Some((base_s, digits)) = word.split_once('#') {
        if let Ok(base) = base_s.parse::<u32>() {
            if (2..=36).contains(&base) {
                if let Ok(val) = u64::from_str_radix(digits, base) {
                    return Some(val as f64);
                }
            }
        }
        return None;
    }
    let first = word.as_bytes().first().copied()?;
    if !matches!(first, b'0'..=b'9' | b'+' | b'-' | b'.') {
        return None;
    }
    word.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[derive(Debug, Clone)]
struct PsGState {
    ctm: Affine2D,
    fill_color: Color,
    stroke_color: Color,
    line_width_pt: f64,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: f64,
    dash_pattern_pt: Vec<f64>,
    overprint: bool,
    font_family: String,
    font_size_pt: f64,
    font_bold: bool,
    font_italic: bool,
}

impl Default for PsGState {
    fn default() -> Self {
        Self {
            ctm: Affine2D::IDENTITY,
            fill_color: Color::BLACK,
            stroke_color: Color::BLACK,
            line_width_pt: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash_pattern_pt: Vec::new(),
            overprint: false,
            font_family: "Helvetica".to_owned(),
            font_size_pt: 12.0,
            font_bold: false,
            font_italic: false,
        }
    }
}

enum EpsEmitted {
    Path {
        eps_path: BezPath,
        fill: Fill,
        outline: OutlineStyle,
    },
    Text {
        text: String,
        font_family: String,
        font_size_pt: f64,
        bold: bool,
        italic: bool,
        color: Color,
        ctm: Affine2D,
        origin_eps: Point2D,
    },
    Raster {
        width: u32,
        height: u32,
        rgba_bytes: Vec<u8>,
        ctm: Affine2D,
    },
}

impl EpsEmitted {
    fn eps_bounds(&self) -> Option<coree_core::Rect2D> {
        match self {
            Self::Raster { ctm, .. } => {
                let p0 = ctm.transform_point(Point2D::new(0.0, 0.0));
                let p1 = ctm.transform_point(Point2D::new(1.0, 1.0));
                let r = coree_core::Rect2D::from_points(p0, p1);
                r.is_finite().then_some(r)
            }
            Self::Path { eps_path, .. } => {
                if eps_path.is_empty() {
                    None
                } else {
                    use kurbo::Shape;
                    let r = coree_core::Rect2D::from_kurbo(eps_path.bounding_box());
                    r.is_finite().then_some(r)
                }
            }
            Self::Text {
                ctm,
                origin_eps,
                font_size_pt,
                text,
                ..
            } => {
                let p0 = ctm.transform_point(*origin_eps);
                let p1 = ctm.transform_point(Point2D::new(
                    origin_eps.x + (text.chars().count().max(1) as f64) * font_size_pt * 0.6,
                    origin_eps.y + font_size_pt,
                ));
                let r = coree_core::Rect2D::from_points(p0, p1);
                r.is_finite().then_some(r)
            }
        }
    }
}

struct PsInterpreter {
    stack: Vec<PsVal>,
    stack_nodes: usize,
    dict: HashMap<String, PsVal>,
    gstate: PsGState,
    gstack: Vec<PsGState>,
    path: BezPath,
    cur_pt: Option<Point2D>,
    subpath_start: Option<Point2D>,
    emitted: Vec<EpsEmitted>,
    steps: usize,
    saw_cmyk: bool,
    saw_rgb: bool,
}

impl PsInterpreter {
    fn new() -> Self {
        Self {
            stack: Vec::new(),
            stack_nodes: 0,
            dict: HashMap::new(),
            gstate: PsGState::default(),
            gstack: Vec::new(),
            path: BezPath::new(),
            cur_pt: None,
            subpath_start: None,
            emitted: Vec::new(),
            steps: 0,
            saw_cmyk: false,
            saw_rgb: false,
        }
    }

    fn charge_step(&mut self) -> IoResult<()> {
        self.steps += 1;
        if self.steps > MAX_PS_STEPS {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_steps={MAX_PS_STEPS}"),
                actual: format!("exceeded {MAX_PS_STEPS} PostScript operations"),
            });
        }
        Ok(())
    }

    fn check_push_budget(&self, incoming_nodes: usize) -> IoResult<()> {
        if self.stack.len() >= MAX_PS_STACK_LEN {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_stack_len={MAX_PS_STACK_LEN}"),
                actual: format!("stack depth exceeded {MAX_PS_STACK_LEN}"),
            });
        }
        if self.stack_nodes.saturating_add(incoming_nodes) > MAX_PS_STACK_NODES {
            return Err(IoError::LimitExceeded {
                limit: format!("max_ps_stack_nodes={MAX_PS_STACK_NODES}"),
                actual: format!("stack value nodes exceeded {MAX_PS_STACK_NODES}"),
            });
        }
        Ok(())
    }

    fn push(&mut self, val: PsVal) -> IoResult<()> {
        let cost = val.node_count(MAX_PS_STACK_NODES);
        self.check_push_budget(cost)?;
        self.stack_nodes = self.stack_nodes.saturating_add(cost);
        self.stack.push(val);
        Ok(())
    }

    fn pop(&mut self) -> Option<PsVal> {
        let val = self.stack.pop()?;
        let cost = val.node_count(MAX_PS_STACK_NODES);
        self.stack_nodes = self.stack_nodes.saturating_sub(cost);
        Some(val)
    }

    fn pop_f64(&mut self) -> Option<f64> {
        self.pop().and_then(|v| v.as_f64())
    }

    fn exec_tokens(&mut self, tokens: &[PsToken], depth: usize) -> IoResult<()> {
        if depth > MAX_PS_CALL_DEPTH {
            return Ok(());
        }
        for tok in tokens {
            self.charge_step()?;
            match tok {
                PsToken::Number(n) => self.push(PsVal::Number(*n))?,
                PsToken::LiteralName(s) => self.push(PsVal::Name(s.clone()))?,
                PsToken::Str(b) => self.push(PsVal::Str(b.clone()))?,
                PsToken::Bool(b) => self.push(PsVal::Bool(*b))?,
                PsToken::Null => self.push(PsVal::Null)?,
                PsToken::ArrayStart => self.push(PsVal::Mark)?,
                PsToken::ArrayEnd => {
                    let mut items = Vec::new();
                    while let Some(v) = self.pop() {
                        if matches!(v, PsVal::Mark) {
                            break;
                        }
                        items.push(v);
                    }
                    items.reverse();
                    self.push(PsVal::Array(items))?;
                }
                PsToken::Proc(p) => self.push(PsVal::Proc(p.clone()))?,
                PsToken::ExecName(name) => {
                    if let Some(bound_ref) = self.dict.get(name) {
                        let cost = bound_ref.node_count(MAX_PS_STACK_NODES);
                        if !matches!(bound_ref, PsVal::Proc(_)) {
                            self.check_push_budget(cost)?;
                        }
                        let bound = bound_ref.clone();
                        match bound {
                            PsVal::Proc(proc_toks) => {
                                self.exec_tokens(&proc_toks, depth + 1)?;
                            }
                            other => self.push(other)?,
                        }
                    } else {
                        self.exec_builtin(name, depth)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn exec_builtin(&mut self, op: &str, depth: usize) -> IoResult<()> {
        match op {
            // Stack & dictionary primitives
            "pop" => {
                let _ = self.pop();
            }
            "dup" => {
                if let Some(top_ref) = self.stack.last() {
                    let cost = top_ref.node_count(MAX_PS_STACK_NODES);
                    self.check_push_budget(cost)?;
                    let top = top_ref.clone();
                    self.push(top)?;
                }
            }
            "exch" => {
                let len = self.stack.len();
                if len >= 2 {
                    self.stack.swap(len - 1, len - 2);
                }
            }
            "clear" => {
                self.stack.clear();
                self.stack_nodes = 0;
            }
            "mark" | "<<" => self.push(PsVal::Mark)?,
            "cleartomark" => {
                while let Some(v) = self.pop() {
                    if matches!(v, PsVal::Mark) {
                        break;
                    }
                }
            }
            ">>" => {
                let mut pairs = Vec::new();
                let mut collected = Vec::new();
                while let Some(v) = self.pop() {
                    if matches!(v, PsVal::Mark) {
                        break;
                    }
                    collected.push(v);
                }
                collected.reverse();
                for chunk in collected.as_chunks::<2>().0 {
                    if let PsVal::Name(k) = &chunk[0] {
                        pairs.push((k.clone(), chunk[1].clone()));
                    }
                }
                self.push(PsVal::Dict(pairs))?;
            }
            "count" => {
                let n = self.stack.len() as f64;
                self.push(PsVal::Number(n))?;
            }
            "index" => {
                if let Some(n) = self.pop_f64() {
                    let idx = n.max(0.0) as usize;
                    if idx < self.stack.len() {
                        let target_ref = &self.stack[self.stack.len() - 1 - idx];
                        let cost = target_ref.node_count(MAX_PS_STACK_NODES);
                        self.check_push_budget(cost)?;
                        let val = target_ref.clone();
                        self.push(val)?;
                    }
                }
            }
            "roll" => {
                if let (Some(j), Some(n)) = (self.pop_f64(), self.pop_f64()) {
                    let n = n.max(0.0) as usize;
                    if n > 0 && n <= self.stack.len() {
                        let start = self.stack.len() - n;
                        let shift = (j as isize).rem_euclid(n as isize) as usize;
                        self.stack[start..].rotate_right(shift);
                    }
                }
            }
            "def" => {
                if let (Some(val), Some(PsVal::Name(k))) = (self.pop(), self.pop()) {
                    if self.dict.len() < 4096 && val.node_count(MAX_PS_STACK_NODES) <= 10_000 {
                        self.dict.insert(k, val);
                    }
                }
            }
            "bind" | "begin" | "end" | "currentdict" | "showpage" => {}
            "dict" => {
                let _ = self.pop();
                self.push(PsVal::Null)?;
            }
            "load" => {
                if let Some(PsVal::Name(k)) = self.pop() {
                    if let Some(v_ref) = self.dict.get(&k) {
                        let cost = v_ref.node_count(MAX_PS_STACK_NODES);
                        self.check_push_budget(cost)?;
                        let v = v_ref.clone();
                        self.push(v)?;
                    } else {
                        self.push(PsVal::Proc(vec![PsToken::ExecName(k)]))?;
                    }
                }
            }
            "where" => {
                let _ = self.pop();
                self.push(PsVal::Bool(false))?;
            }
            "exec" => {
                if let Some(PsVal::Proc(toks)) = self.pop() {
                    self.exec_tokens(&toks, depth + 1)?;
                }
            }
            "repeat" => {
                if let (Some(PsVal::Proc(toks)), Some(n)) = (self.pop(), self.pop_f64()) {
                    let count = (n.max(0.0) as usize).min(10_000);
                    for _ in 0..count {
                        self.charge_step()?;
                        self.exec_tokens(&toks, depth + 1)?;
                    }
                }
            }

            // Arithmetic
            "add" => {
                if let (Some(b), Some(a)) = (self.pop_f64(), self.pop_f64()) {
                    self.push(PsVal::Number(a + b))?;
                }
            }
            "sub" => {
                if let (Some(b), Some(a)) = (self.pop_f64(), self.pop_f64()) {
                    self.push(PsVal::Number(a - b))?;
                }
            }
            "mul" => {
                if let (Some(b), Some(a)) = (self.pop_f64(), self.pop_f64()) {
                    self.push(PsVal::Number(a * b))?;
                }
            }
            "div" => {
                if let (Some(b), Some(a)) = (self.pop_f64(), self.pop_f64()) {
                    self.push(PsVal::Number(if b.abs() > 1e-12 { a / b } else { 0.0 }))?;
                }
            }
            "neg" => {
                if let Some(a) = self.pop_f64() {
                    self.push(PsVal::Number(-a))?;
                }
            }
            "abs" => {
                if let Some(a) = self.pop_f64() {
                    self.push(PsVal::Number(a.abs()))?;
                }
            }
            "sin" => {
                if let Some(a) = self.pop_f64() {
                    self.push(PsVal::Number(a.to_radians().sin()))?;
                }
            }
            "cos" => {
                if let Some(a) = self.pop_f64() {
                    self.push(PsVal::Number(a.to_radians().cos()))?;
                }
            }
            "sqrt" => {
                if let Some(a) = self.pop_f64() {
                    self.push(PsVal::Number(a.max(0.0).sqrt()))?;
                }
            }

            // Graphics state
            "gsave" | "q" => {
                if self.gstack.len() < MAX_PS_GSTATE_DEPTH {
                    self.gstack.push(self.gstate.clone());
                }
            }
            "grestore" | "Q" => {
                if let Some(prev) = self.gstack.pop() {
                    self.gstate = prev;
                }
            }
            "initgraphics" => {
                self.gstate = PsGState::default();
                self.clear_path();
            }

            // Transforms
            "translate" => {
                if let (Some(ty), Some(tx)) = (self.pop_f64(), self.pop_f64()) {
                    self.gstate.ctm = self.gstate.ctm.compose(&Affine2D::translate(tx, ty));
                }
            }
            "scale" => {
                if let (Some(sy), Some(sx)) = (self.pop_f64(), self.pop_f64()) {
                    self.gstate.ctm = self.gstate.ctm.compose(&Affine2D::scale(sx, sy));
                }
            }
            "rotate" => {
                if let Some(deg) = self.pop_f64() {
                    self.gstate.ctm = self.gstate.ctm.compose(&Affine2D::rotate_deg(deg));
                }
            }
            "concat" => {
                if let Some(PsVal::Array(arr)) = self.pop() {
                    if arr.len() >= 6 {
                        let nums: Vec<f64> = arr.iter().take(6).filter_map(PsVal::as_f64).collect();
                        if nums.len() == 6 {
                            let m =
                                Affine2D::new(nums[0], nums[1], nums[2], nums[3], nums[4], nums[5]);
                            self.gstate.ctm = self.gstate.ctm.compose(&m);
                        }
                    }
                }
            }

            // Colors
            "setrgbcolor" => {
                if let (Some(b), Some(g), Some(r)) =
                    (self.pop_f64(), self.pop_f64(), self.pop_f64())
                {
                    let c = Color::rgb(r as f32, g as f32, b as f32);
                    self.gstate.fill_color = c.clone();
                    self.gstate.stroke_color = c;
                    self.saw_rgb = true;
                }
            }
            "setcmykcolor" => {
                if let (Some(k), Some(y), Some(m), Some(c)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    let col = Color::cmyk(c as f32, m as f32, y as f32, k as f32);
                    self.gstate.fill_color = col.clone();
                    self.gstate.stroke_color = col;
                    self.saw_cmyk = true;
                }
            }
            "setgray" => {
                if let Some(g) = self.pop_f64() {
                    let col = Color::gray(g as f32);
                    self.gstate.fill_color = col.clone();
                    self.gstate.stroke_color = col;
                }
            }
            // Adobe Illustrator EPS shorthand colors
            "k" => {
                if let (Some(k), Some(y), Some(m), Some(c)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.gstate.fill_color = Color::cmyk(c as f32, m as f32, y as f32, k as f32);
                    self.saw_cmyk = true;
                }
            }
            "K" => {
                if let (Some(k), Some(y), Some(m), Some(c)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.gstate.stroke_color = Color::cmyk(c as f32, m as f32, y as f32, k as f32);
                    self.saw_cmyk = true;
                }
            }
            "g" => {
                if let Some(g) = self.pop_f64() {
                    self.gstate.fill_color = Color::gray(g as f32);
                }
            }
            "G" => {
                if let Some(g) = self.pop_f64() {
                    self.gstate.stroke_color = Color::gray(g as f32);
                }
            }
            "setoverprint" => {
                if let Some(PsVal::Bool(b)) = self.pop() {
                    self.gstate.overprint = b;
                }
            }

            // Stroke styling
            "setlinewidth" | "w" => {
                if let Some(w) = self.pop_f64() {
                    self.gstate.line_width_pt = w.max(0.0);
                }
            }
            "setlinecap" | "J" => {
                if let Some(c) = self.pop_f64() {
                    self.gstate.line_cap = match c.round() as i32 {
                        1 => LineCap::Round,
                        2 => LineCap::Square,
                        _ => LineCap::Butt,
                    };
                }
            }
            "setlinejoin" | "j" => {
                if let Some(j) = self.pop_f64() {
                    self.gstate.line_join = match j.round() as i32 {
                        1 => LineJoin::Round,
                        2 => LineJoin::Bevel,
                        _ => LineJoin::Miter,
                    };
                }
            }
            "setmiterlimit" | "M" => {
                if let Some(m) = self.pop_f64() {
                    self.gstate.miter_limit = m.max(1.0);
                }
            }
            "setdash" | "d" => {
                let _phase = self.pop_f64();
                if let Some(PsVal::Array(arr)) = self.pop() {
                    self.gstate.dash_pattern_pt = arr
                        .iter()
                        .filter_map(PsVal::as_f64)
                        .filter(|v| *v > 0.0)
                        .collect();
                }
            }

            // Path construction
            "newpath" | "n" | "N" => self.clear_path(),
            "moveto" | "m" => {
                if let (Some(y), Some(x)) = (self.pop_f64(), self.pop_f64()) {
                    let p = self.gstate.ctm.transform_point(Point2D::new(x, y));
                    self.path.move_to(p.to_kurbo());
                    self.cur_pt = Some(p);
                    self.subpath_start = Some(p);
                }
            }
            "rmoveto" => {
                if let (Some(dy), Some(dx)) = (self.pop_f64(), self.pop_f64()) {
                    let base = self.cur_pt.unwrap_or(Point2D::ZERO);
                    let delta = self
                        .gstate
                        .ctm
                        .transform_vec(coree_core::Vec2D::new(dx, dy));
                    let p = base + delta;
                    self.path.move_to(p.to_kurbo());
                    self.cur_pt = Some(p);
                    self.subpath_start = Some(p);
                }
            }
            "lineto" | "l" | "L" => {
                if let (Some(y), Some(x)) = (self.pop_f64(), self.pop_f64()) {
                    let p = self.gstate.ctm.transform_point(Point2D::new(x, y));
                    if self.cur_pt.is_none() {
                        self.path.move_to(p.to_kurbo());
                        self.subpath_start = Some(p);
                    } else {
                        self.path.line_to(p.to_kurbo());
                    }
                    self.cur_pt = Some(p);
                }
            }
            "rlineto" => {
                if let (Some(dy), Some(dx)) = (self.pop_f64(), self.pop_f64()) {
                    let base = self.cur_pt.unwrap_or(Point2D::ZERO);
                    let delta = self
                        .gstate
                        .ctm
                        .transform_vec(coree_core::Vec2D::new(dx, dy));
                    let p = base + delta;
                    if self.cur_pt.is_none() {
                        self.path.move_to(p.to_kurbo());
                        self.subpath_start = Some(p);
                    } else {
                        self.path.line_to(p.to_kurbo());
                    }
                    self.cur_pt = Some(p);
                }
            }
            "curveto" | "c" | "C" => {
                if let (Some(y3), Some(x3), Some(y2), Some(x2), Some(y1), Some(x1)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    let p1 = self.gstate.ctm.transform_point(Point2D::new(x1, y1));
                    let p2 = self.gstate.ctm.transform_point(Point2D::new(x2, y2));
                    let p3 = self.gstate.ctm.transform_point(Point2D::new(x3, y3));
                    if self.cur_pt.is_none() {
                        self.path.move_to(p1.to_kurbo());
                        self.subpath_start = Some(p1);
                    }
                    self.path
                        .curve_to(p1.to_kurbo(), p2.to_kurbo(), p3.to_kurbo());
                    self.cur_pt = Some(p3);
                }
            }
            "rcurveto" => {
                if let (Some(dy3), Some(dx3), Some(dy2), Some(dx2), Some(dy1), Some(dx1)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    let base = self.cur_pt.unwrap_or(Point2D::ZERO);
                    let p1 = base
                        + self
                            .gstate
                            .ctm
                            .transform_vec(coree_core::Vec2D::new(dx1, dy1));
                    let p2 = base
                        + self
                            .gstate
                            .ctm
                            .transform_vec(coree_core::Vec2D::new(dx2, dy2));
                    let p3 = base
                        + self
                            .gstate
                            .ctm
                            .transform_vec(coree_core::Vec2D::new(dx3, dy3));
                    if self.cur_pt.is_none() {
                        self.path.move_to(base.to_kurbo());
                        self.subpath_start = Some(base);
                    }
                    self.path
                        .curve_to(p1.to_kurbo(), p2.to_kurbo(), p3.to_kurbo());
                    self.cur_pt = Some(p3);
                }
            }
            "v" | "V" => {
                if let (Some(y3), Some(x3), Some(y2), Some(x2)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    let p1 = self.cur_pt.unwrap_or(Point2D::ZERO);
                    let p2 = self.gstate.ctm.transform_point(Point2D::new(x2, y2));
                    let p3 = self.gstate.ctm.transform_point(Point2D::new(x3, y3));
                    self.path
                        .curve_to(p1.to_kurbo(), p2.to_kurbo(), p3.to_kurbo());
                    self.cur_pt = Some(p3);
                }
            }
            "y" | "Y" => {
                if let (Some(y3), Some(x3), Some(y1), Some(x1)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    let p1 = self.gstate.ctm.transform_point(Point2D::new(x1, y1));
                    let p3 = self.gstate.ctm.transform_point(Point2D::new(x3, y3));
                    self.path
                        .curve_to(p1.to_kurbo(), p3.to_kurbo(), p3.to_kurbo());
                    self.cur_pt = Some(p3);
                }
            }
            "arc" => {
                if let (Some(a2), Some(a1), Some(r), Some(yc), Some(xc)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.append_ps_arc(xc, yc, r, a1, a2, false);
                }
            }
            "arcn" => {
                if let (Some(a2), Some(a1), Some(r), Some(yc), Some(xc)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    self.append_ps_arc(xc, yc, r, a1, a2, true);
                }
            }
            "closepath" => {
                if self.cur_pt.is_some() {
                    self.path.close_path();
                    self.cur_pt = self.subpath_start;
                }
            }
            "rectfill" => {
                if let (Some(h), Some(w), Some(y), Some(x)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    let rp = self.build_rect_path(x, y, w, h);
                    self.emitted.push(EpsEmitted::Path {
                        eps_path: rp,
                        fill: Fill::Solid(self.gstate.fill_color.clone()),
                        outline: OutlineStyle::none(),
                    });
                }
            }
            "rectstroke" => {
                if let (Some(h), Some(w), Some(y), Some(x)) = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                ) {
                    let rp = self.build_rect_path(x, y, w, h);
                    let outline = self.current_outline();
                    self.emitted.push(EpsEmitted::Path {
                        eps_path: rp,
                        fill: Fill::None,
                        outline,
                    });
                }
            }
            "rectclip" => {
                let _ = (
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                    self.pop_f64(),
                );
            }
            "clip" | "eoclip" => {}

            // Painting
            "fill" | "eofill" => self.emit_current_path(true, false, false),
            "f" => self.emit_current_path(true, false, true),
            "F" => self.emit_current_path(true, false, false),
            "stroke" | "S" => self.emit_current_path(false, true, false),
            "s" => self.emit_current_path(false, true, true),
            "b" => self.emit_current_path(true, true, true),
            "B" => self.emit_current_path(true, true, false),

            // Fonts & Text
            "findfont" => {
                if let Some(PsVal::Name(fname)) = self.pop() {
                    let (family, bold, italic) = parse_ps_font_name(&fname);
                    self.push(PsVal::Font {
                        family,
                        size_pt: 1.0,
                        bold,
                        italic,
                    })?;
                }
            }
            "scalefont" => {
                if let (
                    Some(scale),
                    Some(PsVal::Font {
                        family,
                        size_pt,
                        bold,
                        italic,
                    }),
                ) = (self.pop_f64(), self.pop())
                {
                    self.push(PsVal::Font {
                        family,
                        size_pt: (size_pt * scale.abs()).max(1.0),
                        bold,
                        italic,
                    })?;
                }
            }
            "makefont" => {
                if let (
                    Some(PsVal::Array(mat)),
                    Some(PsVal::Font {
                        family,
                        size_pt,
                        bold,
                        italic,
                    }),
                ) = (self.pop(), self.pop())
                {
                    let sy = mat
                        .get(3)
                        .and_then(PsVal::as_f64)
                        .map(f64::abs)
                        .unwrap_or(1.0);
                    self.push(PsVal::Font {
                        family,
                        size_pt: (size_pt * sy).max(1.0),
                        bold,
                        italic,
                    })?;
                }
            }
            "setfont" => {
                if let Some(PsVal::Font {
                    family,
                    size_pt,
                    bold,
                    italic,
                }) = self.pop()
                {
                    self.gstate.font_family = family;
                    self.gstate.font_size_pt = size_pt.max(1.0);
                    self.gstate.font_bold = bold;
                    self.gstate.font_italic = italic;
                }
            }
            "selectfont" => {
                if let (Some(scale), Some(PsVal::Name(fname))) = (self.pop_f64(), self.pop()) {
                    let (family, bold, italic) = parse_ps_font_name(&fname);
                    self.gstate.font_family = family;
                    self.gstate.font_size_pt = scale.abs().max(1.0);
                    self.gstate.font_bold = bold;
                    self.gstate.font_italic = italic;
                }
            }
            "show" => {
                if let Some(PsVal::Str(raw)) = self.pop() {
                    self.emit_text(&raw);
                }
            }
            "ashow" => {
                if let (Some(PsVal::Str(raw)), Some(_ay), Some(_ax)) =
                    (self.pop(), self.pop_f64(), self.pop_f64())
                {
                    self.emit_text(&raw);
                }
            }
            "widthshow" => {
                if let (Some(PsVal::Str(raw)), Some(_ch), Some(_cy), Some(_cx)) =
                    (self.pop(), self.pop_f64(), self.pop_f64(), self.pop_f64())
                {
                    self.emit_text(&raw);
                }
            }
            "kshow" => {
                if let (Some(PsVal::Str(raw)), Some(_proc)) = (self.pop(), self.pop()) {
                    self.emit_text(&raw);
                }
            }
            "shfill" => {
                let _ = self.pop();
            }
            "image" => {
                let top = self.pop();
                let (raw_opt, dict_opt) = match top {
                    Some(PsVal::Str(raw)) => (Some(raw), self.pop()),
                    other => (None, other),
                };
                if let (Some(raw), Some(PsVal::Dict(entries))) = (raw_opt, dict_opt) {
                    let find_num = |key: &str| {
                        entries
                            .iter()
                            .rev()
                            .find(|(k, _)| k == key)
                            .and_then(|(_, v)| v.as_f64())
                    };
                    if let (Some(w_f), Some(h_f)) = (find_num("Width"), find_num("Height")) {
                        let w = (w_f.round() as u32).clamp(1, 4096);
                        let h = (h_f.round() as u32).clamp(1, 4096);
                        let px_cnt = (w as usize).saturating_mul(h as usize);
                        let decode_len = entries
                            .iter()
                            .rev()
                            .find(|(k, _)| k == "Decode")
                            .and_then(|(_, v)| match v {
                                PsVal::Array(a) => Some(a.len()),
                                _ => None,
                            })
                            .unwrap_or(6);
                        let is_cmyk = decode_len >= 8 || raw.len() == px_cnt * 4;
                        let stride = if is_cmyk { 4 } else { 3 };
                        if raw.len() >= px_cnt * stride {
                            let mut rgba = Vec::with_capacity(px_cnt * 4);
                            for i in 0..px_cnt {
                                let base = i * stride;
                                if is_cmyk {
                                    let c = f32::from(raw[base]) / 255.0;
                                    let m = f32::from(raw[base + 1]) / 255.0;
                                    let y = f32::from(raw[base + 2]) / 255.0;
                                    let k = f32::from(raw[base + 3]) / 255.0;
                                    let rgb = Color::cmyk(c, m, y, k).to_rgba8();
                                    rgba.extend_from_slice(&rgb);
                                } else {
                                    rgba.extend_from_slice(&[
                                        raw[base],
                                        raw[base + 1],
                                        raw[base + 2],
                                        255,
                                    ]);
                                }
                            }
                            self.emitted.push(EpsEmitted::Raster {
                                width: w,
                                height: h,
                                rgba_bytes: rgba,
                                ctm: self.gstate.ctm,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn clear_path(&mut self) {
        self.path = BezPath::new();
        self.cur_pt = None;
        self.subpath_start = None;
    }

    fn build_rect_path(&self, x: f64, y: f64, w: f64, h: f64) -> BezPath {
        let p0 = self.gstate.ctm.transform_point(Point2D::new(x, y));
        let p1 = self.gstate.ctm.transform_point(Point2D::new(x + w, y));
        let p2 = self.gstate.ctm.transform_point(Point2D::new(x + w, y + h));
        let p3 = self.gstate.ctm.transform_point(Point2D::new(x, y + h));
        let mut p = BezPath::new();
        p.move_to(p0.to_kurbo());
        p.line_to(p1.to_kurbo());
        p.line_to(p2.to_kurbo());
        p.line_to(p3.to_kurbo());
        p.close_path();
        p
    }

    fn append_ps_arc(&mut self, xc: f64, yc: f64, r: f64, ang1_deg: f64, ang2_deg: f64, cw: bool) {
        if !(xc.is_finite()
            && yc.is_finite()
            && r.is_finite()
            && ang1_deg.is_finite()
            && ang2_deg.is_finite())
            || r <= 0.0
        {
            return;
        }
        // O(1) modular angle normalization (never loops on extreme angles like 1e20)
        let mut a1 = ang1_deg.rem_euclid(360.0).to_radians();
        let total_sweep = if cw {
            let mut delta_cw = (ang1_deg - ang2_deg).rem_euclid(360.0);
            if delta_cw <= 1e-9 && (ang1_deg - ang2_deg).abs() >= 1e-9 {
                delta_cw = 360.0;
            }
            -delta_cw.to_radians()
        } else {
            let mut delta_ccw = (ang2_deg - ang1_deg).rem_euclid(360.0);
            if delta_ccw <= 1e-9 && (ang2_deg - ang1_deg).abs() >= 1e-9 {
                delta_ccw = 360.0;
            }
            delta_ccw.to_radians()
        };

        let start_u = Point2D::new(xc + r * a1.cos(), yc + r * a1.sin());
        let start_d = self.gstate.ctm.transform_point(start_u);
        if self.cur_pt.is_none() {
            self.path.move_to(start_d.to_kurbo());
            self.subpath_start = Some(start_d);
        } else {
            self.path.line_to(start_d.to_kurbo());
        }
        self.cur_pt = Some(start_d);

        let segs = ((total_sweep.abs() / std::f64::consts::FRAC_PI_2).ceil() as usize).clamp(1, 32);
        let step = total_sweep / (segs as f64);
        let kappa = (4.0 / 3.0) * (step / 4.0).tan();

        for _ in 0..segs {
            let next_a = a1 + step;
            let (s0, c0) = a1.sin_cos();
            let (s1, c1) = next_a.sin_cos();

            let u1 = Point2D::new(xc + r * (c0 - kappa * s0), yc + r * (s0 + kappa * c0));
            let u2 = Point2D::new(xc + r * (c1 + kappa * s1), yc + r * (s1 - kappa * c1));
            let u3 = Point2D::new(xc + r * c1, yc + r * s1);

            let d1 = self.gstate.ctm.transform_point(u1);
            let d2 = self.gstate.ctm.transform_point(u2);
            let d3 = self.gstate.ctm.transform_point(u3);
            self.path
                .curve_to(d1.to_kurbo(), d2.to_kurbo(), d3.to_kurbo());
            self.cur_pt = Some(d3);
            a1 = next_a;
        }
    }

    fn current_outline(&self) -> OutlineStyle {
        let scale = effective_affine_scale(&self.gstate.ctm);
        let width_mm = (self.gstate.line_width_pt * scale * PT_TO_MM).max(0.05);
        let mut out = OutlineStyle::solid(self.gstate.stroke_color.clone(), width_mm);
        out.cap = self.gstate.line_cap;
        out.join = self.gstate.line_join;
        out.miter_limit = self.gstate.miter_limit;
        out.overprint = self.gstate.overprint;
        out.dash_pattern = self
            .gstate
            .dash_pattern_pt
            .iter()
            .map(|d| d * scale * PT_TO_MM)
            .filter(|d| d.is_finite() && *d > 0.0)
            .collect();
        out
    }

    fn emit_current_path(&mut self, do_fill: bool, do_stroke: bool, close: bool) {
        if close && self.cur_pt.is_some() {
            self.path.close_path();
        }
        let path = std::mem::take(&mut self.path);
        self.cur_pt = None;
        self.subpath_start = None;
        if path.is_empty() {
            return;
        }
        let fill = if do_fill {
            Fill::Solid(self.gstate.fill_color.clone())
        } else {
            Fill::None
        };
        let outline = if do_stroke {
            self.current_outline()
        } else {
            OutlineStyle::none()
        };
        self.emitted.push(EpsEmitted::Path {
            eps_path: path,
            fill,
            outline,
        });
    }

    fn emit_text(&mut self, raw: &[u8]) {
        let text: String = raw.iter().map(|&b| char::from(b)).collect();
        if text.is_empty() {
            return;
        }
        // Recover current point in user space so `ctm` + `origin_eps` compose cleanly
        let cur_dev = self.cur_pt.unwrap_or(Point2D::ZERO);
        let origin_user = self
            .gstate
            .ctm
            .inverse()
            .map_or(cur_dev, |inv| inv.transform_point(cur_dev));
        self.emitted.push(EpsEmitted::Text {
            text,
            font_family: self.gstate.font_family.clone(),
            font_size_pt: self.gstate.font_size_pt,
            bold: self.gstate.font_bold,
            italic: self.gstate.font_italic,
            color: self.gstate.fill_color.clone(),
            ctm: self.gstate.ctm,
            origin_eps: origin_user,
        });
    }
}

fn parse_ps_font_name(raw: &str) -> (String, bool, bool) {
    let lower = raw.to_ascii_lowercase();
    let bold = lower.contains("bold") || lower.contains("black") || lower.contains("heavy");
    let italic = lower.contains("italic") || lower.contains("oblique");
    let family = raw.split('-').next().unwrap_or(raw).trim();
    let clean = if family.is_empty() {
        "Helvetica".to_owned()
    } else {
        family.to_owned()
    };
    (clean, bold, italic)
}

fn effective_affine_scale(xf: &Affine2D) -> f64 {
    let sx = xf.m[0].hypot(xf.m[1]);
    let sy = xf.m[2].hypot(xf.m[3]);
    let avg = 0.5 * (sx + sy);
    if avg.is_finite() && avg > 1e-6 {
        avg
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{CornerStyle, ParametricShape, Rect2D};

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

    #[test]
    fn eps_roundtrip_export_then_import_preserves_dimensions_geometry_colors_and_text() {
        let mut doc = Document::new("Brand Card", 120.0, 80.0, ColorMode::Rgb, 300.0);
        let mut outline = OutlineStyle::solid(Color::rgb(0.1, 0.3, 0.9), 1.2);
        outline.cap = LineCap::Round;
        outline.join = LineJoin::Bevel;
        outline.dash_pattern = vec![3.0, 1.5];

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Card Box",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    15.0, 10.0, 60.0, 35.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::rgb(0.85, 0.25, 0.15)))
            .with_outline(outline),
            None,
        )
        .unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Heading",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "Studio (EPS) 2026",
                    TextStyle {
                        font_family: "Helvetica".to_owned(),
                        font_size_pt: 18.0,
                        bold: true,
                        fill: Color::rgb(0.05, 0.05, 0.05),
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(20.0, 52.0)),
            None,
        )
        .unwrap();

        let eps_str = export_eps_to_string(&doc, 0).unwrap();
        let imported = import_eps_bytes(eps_str.as_bytes(), "fallback").unwrap();

        assert_eq!(imported.title(), "Brand Card");
        assert!((imported.pages()[0].width_mm - 120.0).abs() < 0.05);
        assert!((imported.pages()[0].height_mm - 80.0).abs() < 0.05);

        // Expect filled rect path + stroked rect path + ArtisticText
        assert_eq!(imported.object_count(), 3);

        let mut saw_fill = false;
        let mut saw_stroke = false;
        let mut saw_text = false;
        for (&id, obj) in imported.objects() {
            match &obj.payload {
                NodePayload::Path(_) => {
                    let bb = imported.object_bounds(id).unwrap();
                    assert!((bb.min_x - 15.0).abs() < 0.05);
                    assert!((bb.min_y - 10.0).abs() < 0.05);
                    assert!((bb.width() - 60.0).abs() < 0.05);
                    assert!((bb.height() - 35.0).abs() < 0.05);
                    if let Fill::Solid(c) = &obj.fill {
                        let [r, g, b, _] = c.to_rgba_f32();
                        assert!((r - 0.85).abs() < 0.01);
                        assert!((g - 0.25).abs() < 0.01);
                        assert!((b - 0.15).abs() < 0.01);
                        saw_fill = true;
                    }
                    if obj.outline.is_visible() {
                        assert!((obj.outline.width_mm - 1.2).abs() < 0.05);
                        assert_eq!(obj.outline.cap, LineCap::Round);
                        assert_eq!(obj.outline.join, LineJoin::Bevel);
                        assert_eq!(obj.outline.dash_pattern.len(), 2);
                        saw_stroke = true;
                    }
                }
                NodePayload::ArtisticText(t) => {
                    assert_eq!(t.plain_text(), "Studio (EPS) 2026");
                    assert!(t.runs[0].style.bold);
                    assert!((t.runs[0].style.font_size_pt - 18.0).abs() < 0.1);
                    assert!((obj.transform.m[4] - 20.0).abs() < 0.1);
                    assert!((obj.transform.m[5] - 52.0).abs() < 0.1);
                    saw_text = true;
                }
                _ => {}
            }
        }
        assert!(saw_fill && saw_stroke && saw_text);
    }

    #[test]
    fn eps_dos_binary_wrapper_arcs_rectfill_and_illustrator_shorthand() {
        let ps = b"%!PS-Adobe-3.0 EPSF-3.0\n\
%%BoundingBox: 0 0 200 100\n\
gsave\n\
10 10 40 20 rectfill\n\
0 1 0 0 k\n\
0 0 0 1 K\n\
100 50 25 0 360 arc closepath B\n\
10 80 m 50 80 l S\n\
/Courier findfont 14 scalefont setfont 10 20 moveto (A\\050B\\051) show\n\
grestore\n\
%%EOF\n";

        // Wrap in a 30-byte DOS binary EPS header (C5 D0 D3 C6)
        let mut dos_eps = vec![0u8; 30];
        dos_eps[0..4].copy_from_slice(&[0xC5, 0xD0, 0xD3, 0xC6]);
        dos_eps[4..8].copy_from_slice(&30u32.to_le_bytes());
        dos_eps[8..12].copy_from_slice(&(ps.len() as u32).to_le_bytes());
        dos_eps.extend_from_slice(ps);
        // Trailing TIFF preview bytes should be ignored safely
        dos_eps.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00, 0xDE, 0xAD]);

        let doc = import_eps_bytes(&dos_eps, "DOS Binary").unwrap();
        assert_eq!(doc.color_mode(), ColorMode::Cmyk);
        assert_eq!(doc.object_count(), 4); // rectfill, arc B, line S, text show
        let text_obj = doc
            .objects()
            .values()
            .find_map(|o| match &o.payload {
                NodePayload::ArtisticText(t) => Some(t.plain_text()),
                _ => None,
            })
            .unwrap();
        assert_eq!(text_obj, "A(B)");
    }

    #[test]
    fn eps_adversarial_unmatched_paren_extreme_arc_empty_repeat_and_array_bomb() {
        // 1. Unmatched `)` must terminate immediately without infinite loop or OOM
        let doc = import_eps_bytes(b"%!PS\n)\n", "UnmatchedParen").unwrap();
        assert_eq!(doc.pages().len(), 1);

        // 2. Extreme finite arc angles (1e20) must finish in O(1) without hanging
        let extreme_arc =
            b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n50 50 20 1e20 0 arc stroke\n50 50 20 0 1e20 arcn stroke\n%%EOF\n";
        let arc_doc = import_eps_bytes(extreme_arc, "ExtremeArc").unwrap();
        assert_eq!(arc_doc.object_count(), 2);

        // 3. Nested empty repeat loop bomb must trip MAX_PS_STEPS immediately
        let empty_loop_bomb = b"%!PS\n10000 { 10000 { } repeat } repeat\n";
        let err1 = import_eps_bytes(empty_loop_bomb, "LoopBomb").unwrap_err();
        assert!(matches!(err1, IoError::LimitExceeded { .. }));

        // 4. Exponential array doubling bomb must trip MAX_PS_STACK_NODES without OOM
        let array_bomb = b"%!PS\n[1 2] 30 { dup [ exch dup ] } repeat\n";
        let err2 = import_eps_bytes(array_bomb, "ArrayBomb").unwrap_err();
        assert!(matches!(err2, IoError::LimitExceeded { .. }));
    }

    #[test]
    fn eps_arabic_curves_raster_image_roundtrip_and_shfill() {
        let mut doc = Document::new("EPS Full", 120.0, 90.0, ColorMode::Rgb, 300.0);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Arabic",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "كوري",
                    TextStyle {
                        font_size_pt: 24.0,
                        direction: TextDirection::Rtl,
                        ..TextStyle::default()
                    },
                )),
            )
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(20.0, 20.0)),
            None,
        )
        .unwrap();

        let asset = coree_core::RasterAsset::new(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
            ],
            300.0,
            ColorMode::Rgb,
        )
        .unwrap();
        let asset_id = doc.register_asset(asset);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Bitmap",
                NodePayload::Raster(coree_core::RasterLayerData {
                    asset_id,
                    width_mm: 20.0,
                    height_mm: 20.0,
                    filters: Vec::new(),
                    crop_rect: None,
                }),
            )
            .with_transform(Affine2D::translate(50.0, 30.0)),
            None,
        )
        .unwrap();

        let eps = export_eps_to_string(&doc, 0).unwrap();
        assert!(eps.contains("curveto"));
        assert!(eps.contains("/ImageType 1"));
        assert!(eps.contains("FF0000"));

        let imported = import_eps_bytes(eps.as_bytes(), "EPS Full").unwrap();
        assert!(!imported.assets().is_empty());
        assert!(imported
            .objects()
            .values()
            .any(|o| matches!(o.payload, NodePayload::Raster(_))));
    }
}
