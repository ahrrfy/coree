// SPDX-License-Identifier: MIT OR Apache-2.0

//! Unified `ExportDisplayList` (`CdrPsi`) single scene-walk intermediate representation
//! consumed by PDF, EPS, and SVG export pipelines.
//!
//! Resolves master layers, `NodePayload::Clone` (including `Clone -> PowerClip` and
//! inherited raster-composite fills), `NodePayload::SymbolInstance` (`referenced`-filtered
//! children and `ExportSymCtx`), `TextOnPath`, `Connector`, `Table`, `Dimension`,
//! `Symmetry`, `Perspective3x3`, `CutContour` Spot overlays, and isolated raster
//! compositing in a single deterministic scene traversal.

use crate::error::{IoError, IoResult};
use crate::{resolve_export_connector_endpoints, resolve_export_world_path, ExportSymCtx};
use coree_core::{
    Affine2D, ArtisticText, BezierPath, BlendMode, Color, Document, Effect, Fill, ImageAssetId,
    Layer, LayerId, MasterLayerScope, NodePayload, ObjectId, OutlineStyle, Page, Perspective3x3,
    Point2D, Rect2D, SceneObject, SymbolDef, SymbolId, TextOnPath,
};
use coree_render::effects_render::{apply_geometry_effects, symmetry_effect_transforms};
use coree_render::{
    render_isolated_object_to_rgba_with_options, FillRule, ShapedGlyphDraw, ShapedPdfRun,
    TextEngine,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const MAX_DISPLAY_DEPTH: usize = 64;

/// Configuration controlling how [`ExportDisplayList::build`] traverses a document page.
#[derive(Debug, Clone)]
pub struct DisplayListOptions {
    pub include_hidden_layers: bool,
    pub include_non_printable_layers: bool,
    pub include_hidden_objects: bool,
    pub include_non_printable_objects: bool,
    pub raster_dpi: f64,
    /// When `true` (SVG), `TwoColorPattern`, `VectorPattern`, and `BitmapPattern` stay vector.
    pub keep_pattern_fills_vector: bool,
    /// Suppresses Spot `CutContour` strokes inside isolated raster composites (`DynIm`).
    pub suppress_cutcontour_in_raster: bool,
    /// Emits vector Spot `CutContour` overlays after raster-composited objects.
    pub emit_cutcontour_overlays: bool,
    /// Populates `ShapedPdfRun` glyph runs for PDF font subsetting.
    pub shape_pdf_text_runs: bool,
}

impl DisplayListOptions {
    /// Preset for PDF/X-1a and PDF/X-4 export.
    #[must_use]
    pub fn pdf() -> Self {
        Self {
            include_hidden_layers: false,
            include_non_printable_layers: false,
            include_hidden_objects: false,
            include_non_printable_objects: false,
            raster_dpi: 300.0,
            keep_pattern_fills_vector: false,
            suppress_cutcontour_in_raster: true,
            emit_cutcontour_overlays: true,
            shape_pdf_text_runs: true,
        }
    }

    /// Preset for Encapsulated PostScript Level 3 export.
    #[must_use]
    pub fn eps() -> Self {
        Self {
            include_hidden_layers: false,
            include_non_printable_layers: false,
            include_hidden_objects: false,
            include_non_printable_objects: false,
            raster_dpi: 300.0,
            keep_pattern_fills_vector: false,
            suppress_cutcontour_in_raster: true,
            emit_cutcontour_overlays: true,
            shape_pdf_text_runs: false,
        }
    }

    /// Preset for SVG 1.1 export.
    #[must_use]
    pub fn svg() -> Self {
        Self {
            include_hidden_layers: true,
            include_non_printable_layers: true,
            include_hidden_objects: true,
            include_non_printable_objects: true,
            raster_dpi: 150.0,
            keep_pattern_fills_vector: true,
            suppress_cutcontour_in_raster: false,
            emit_cutcontour_overlays: false,
            shape_pdf_text_runs: false,
        }
    }
}

/// Semantic kind of a [`DisplayOp::DrawTextRuns`] operation.
#[derive(Debug, Clone)]
pub enum DisplayTextKind {
    Artistic,
    Paragraph {
        needs_doc_flow: bool,
    },
    OnPath {
        text_on_path: TextOnPath,
        local_host_path: Option<BezierPath>,
    },
    TableCell,
    DimensionLabel,
}

/// Single command in an [`ExportDisplayList`] (`CdrPsi`) stream.
#[derive(Debug, Clone)]
pub enum DisplayOp {
    PushLayer {
        id: LayerId,
        name: String,
        visible: bool,
        printable: bool,
        is_cut_layer: bool,
    },
    PopLayer,
    PushGroup {
        id_suffix: String,
        local_xf: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        visible: bool,
        is_perspective: bool,
    },
    PopGroup,
    PushClip {
        id_suffix: String,
        path: BezierPath,
        local_path: BezierPath,
        fill_rule: FillRule,
        local_xf: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        visible: bool,
        is_perspective: bool,
    },
    PopClip,
    PushTransparencyGroup {
        opacity: f32,
        blend_mode: BlendMode,
        isolated: bool,
        knockout: bool,
    },
    PopTransparencyGroup,
    DrawVector {
        id: ObjectId,
        id_suffix: String,
        path: BezierPath,
        local_path: BezierPath,
        fill: Fill,
        outline: OutlineStyle,
        fill_xf: Affine2D,
        local_xf: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        visible: bool,
        is_perspective: bool,
        is_cut_contour: bool,
        emit_arrowheads: bool,
    },
    DrawTextRuns {
        id: ObjectId,
        id_suffix: String,
        kind: DisplayTextKind,
        artistic: ArtisticText,
        shaped_runs: Vec<ShapedPdfRun>,
        fallback_curves: Vec<ShapedGlyphDraw>,
        svg_fallback_curves: Vec<ShapedGlyphDraw>,
        outline: OutlineStyle,
        world_xf: Affine2D,
        local_xf: Affine2D,
        sym_xf: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        visible: bool,
        is_perspective: bool,
    },
    DrawRaster {
        id: ObjectId,
        id_suffix: String,
        asset_id: Option<ImageAssetId>,
        inline_rgba: Option<Arc<(u32, u32, Vec<u8>)>>,
        rect_mm: Option<Rect2D>,
        bl: Point2D,
        br: Point2D,
        tl: Point2D,
        local_xf: Affine2D,
        world_xf: Affine2D,
        width_mm: f64,
        height_mm: f64,
        opacity: f32,
        blend_mode: BlendMode,
        visible: bool,
    },
}

/// Complete single-page export display list (`CdrPsi`).
#[derive(Debug, Clone)]
pub struct ExportDisplayList {
    pub page_index: usize,
    pub width_mm: f64,
    pub height_mm: f64,
    pub bleed_mm: f64,
    pub has_transparency: bool,
    pub ops: Vec<DisplayOp>,
}

impl ExportDisplayList {
    /// Walks master layers and `page_index` layers once to construct a complete display list.
    pub fn build(
        doc: &Document,
        page_index: usize,
        opts: &DisplayListOptions,
        text_engine: &mut TextEngine,
    ) -> IoResult<Self> {
        build_display_list(doc, page_index, opts, text_engine)
    }

    /// Walks a [`SymbolDef`]'s non-referenced objects to build ops (e.g., for `Fill::VectorPattern`).
    pub fn build_symbol(
        doc: &Document,
        sym: &SymbolDef,
        opts: &DisplayListOptions,
        text_engine: &mut TextEngine,
    ) -> Vec<DisplayOp> {
        let (child_map, referenced) = build_symbol_maps(sym);
        let mut ops = Vec::new();
        let mut has_transparency = false;
        let mut visited = HashSet::new();
        for sub in &sym.objects {
            if sub.id.0 != 0 && referenced.contains(&sub.id) {
                continue;
            }
            emit_object(
                doc,
                0,
                sub,
                Affine2D::IDENTITY,
                None,
                None,
                1.0,
                Some((&child_map, &sym.objects, Affine2D::IDENTITY)),
                opts,
                text_engine,
                &mut ops,
                &mut has_transparency,
                0,
                &mut visited,
            );
        }
        ops
    }
}

/// Builds an [`ExportDisplayList`] for `page_index` of `doc`.
pub fn build_display_list(
    doc: &Document,
    page_index: usize,
    opts: &DisplayListOptions,
    text_engine: &mut TextEngine,
) -> IoResult<ExportDisplayList> {
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
            format: "display_list".into(),
            message: format!(
                "page {} has invalid non-finite or non-positive dimensions",
                page_index + 1
            ),
        });
    }

    let ordered_layers = collect_ordered_layers(doc, page, page_index, opts);
    let mut ops = Vec::with_capacity(64);
    let mut has_transparency = false;

    for layer in ordered_layers {
        ops.push(DisplayOp::PushLayer {
            id: layer.id,
            name: layer.name.clone(),
            visible: layer.visible,
            printable: layer.printable,
            is_cut_layer: layer.is_cut_layer,
        });
        let mut visited = HashSet::new();
        for &oid in &layer.objects {
            if let Some(obj) = doc.object(oid) {
                emit_object(
                    doc,
                    page_index,
                    obj,
                    Affine2D::IDENTITY,
                    None,
                    None,
                    1.0,
                    None,
                    opts,
                    text_engine,
                    &mut ops,
                    &mut has_transparency,
                    0,
                    &mut visited,
                );
            }
        }
        ops.push(DisplayOp::PopLayer);
    }

    Ok(ExportDisplayList {
        page_index,
        width_mm: page.width_mm,
        height_mm: page.height_mm,
        bleed_mm: if page.bleed_mm.is_finite() {
            page.bleed_mm.max(0.0)
        } else {
            0.0
        },
        has_transparency,
        ops,
    })
}

fn collect_ordered_layers<'a>(
    doc: &'a Document,
    page: &'a Page,
    page_index: usize,
    opts: &DisplayListOptions,
) -> Vec<&'a Layer> {
    let mut ordered = Vec::new();
    for (p_idx, p) in doc.pages().iter().enumerate() {
        if p_idx == page_index {
            continue;
        }
        for layer in &p.layers {
            if (!opts.include_hidden_layers && !layer.visible)
                || (!opts.include_non_printable_layers && !layer.printable)
            {
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
                ordered.push(layer);
            }
        }
    }
    for layer in &page.layers {
        if (opts.include_hidden_layers || layer.visible)
            && (opts.include_non_printable_layers || layer.printable)
        {
            ordered.push(layer);
        }
    }
    ordered
}

/// Returns `true` when `color` is a `CutContour` Spot color.
#[must_use]
pub fn is_cutcontour_color(color: &Color) -> bool {
    matches!(
        color,
        Color::Spot { name, .. } if name.trim().eq_ignore_ascii_case("CutContour")
    )
}

fn build_symbol_maps(sym: &SymbolDef) -> (HashMap<ObjectId, &SceneObject>, HashSet<ObjectId>) {
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

fn has_geometry_effect(effects: &[Effect]) -> bool {
    effects.iter().any(|e| {
        matches!(
            e,
            Effect::Distort { .. } | Effect::Envelope { .. } | Effect::Perspective { .. }
        )
    })
}

fn fill_requires_raster_composite(fill: &Fill, keep_pattern_fills_vector: bool) -> bool {
    match fill {
        Fill::ConicalGradient { .. }
        | Fill::SquareGradient { .. }
        | Fill::MeshFill { .. }
        | Fill::TextureFill { .. } => true,
        Fill::TwoColorPattern { .. } | Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => {
            !keep_pattern_fills_vector
        }
        _ => false,
    }
}

/// Resolves `NodePayload::Clone` via [`Document::resolve_clone_payload_and_style_with_sym`],
/// checking `sym_map` first (for `SymbolDef`-local clones) before falling back to `doc.objects`.
pub(crate) fn resolve_display_list_clone(
    doc: &Document,
    obj: &SceneObject,
    sym_map: Option<&HashMap<ObjectId, &SceneObject>>,
) -> Option<(NodePayload, Fill, OutlineStyle)> {
    doc.resolve_clone_payload_and_style_with_sym(obj, sym_map)
}

/// Returns `true` if `obj` (including resolved `Clone` targets!) requires isolated rasterization.
#[must_use]
pub fn object_requires_raster_composite(
    doc: &Document,
    obj: &SceneObject,
    opts: &DisplayListOptions,
) -> bool {
    let mut visited_objs = HashSet::new();
    let mut visited_syms = HashSet::new();
    object_requires_raster_composite_inner(
        doc,
        obj,
        None,
        false,
        opts,
        &mut visited_objs,
        &mut visited_syms,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn object_requires_raster_composite_inner(
    doc: &Document,
    obj: &SceneObject,
    sym_map: Option<&HashMap<ObjectId, &SceneObject>>,
    in_forced_vector_ancestor: bool,
    opts: &DisplayListOptions,
    visited_objs: &mut HashSet<ObjectId>,
    visited_syms: &mut HashSet<SymbolId>,
    depth: usize,
) -> bool {
    if depth > MAX_DISPLAY_DEPTH
        || (depth > 0
            && ((!opts.include_hidden_objects && !obj.visible)
                || (!opts.include_non_printable_objects && !obj.printable)))
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
            NodePayload::Clone { .. } => {
                if let Some((eff_p, eff_f, eff_o)) = resolve_display_list_clone(doc, obj, sym_map) {
                    let mut transient = obj.clone();
                    transient.id = ObjectId(0);
                    transient.payload = eff_p;
                    transient.fill = eff_f;
                    transient.outline = eff_o;
                    object_requires_raster_composite_inner(
                        doc,
                        &transient,
                        sym_map,
                        in_forced_vector_ancestor,
                        opts,
                        visited_objs,
                        visited_syms,
                        depth + 1,
                    )
                } else {
                    false
                }
            }
            NodePayload::Group(children) => {
                has_geometry_effect(&obj.effects)
                    || ((in_forced_vector_ancestor || obj.perspective.is_some())
                        && children.iter().any(|cid| {
                            sym_map
                                .and_then(|m| m.get(cid).copied())
                                .or_else(|| doc.object(*cid))
                                .is_some_and(|child| {
                                    object_requires_raster_composite_inner(
                                        doc,
                                        child,
                                        sym_map,
                                        true,
                                        opts,
                                        visited_objs,
                                        visited_syms,
                                        depth + 1,
                                    )
                                })
                        }))
            }
            NodePayload::SymbolInstance(sym_id) => {
                has_geometry_effect(&obj.effects)
                    || (visited_syms.insert(*sym_id) && {
                        let hit = doc.symbol(*sym_id).is_some_and(|sym| {
                            let (child_map, referenced) = build_symbol_maps(sym);
                            let mut sym_visited = HashSet::new();
                            sym.objects.iter().any(|sub| {
                                !(sub.id.0 != 0 && referenced.contains(&sub.id))
                                    && object_requires_raster_composite_inner(
                                        doc,
                                        sub,
                                        Some(&child_map),
                                        true,
                                        opts,
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
                fill_requires_raster_composite(&obj.fill, opts.keep_pattern_fills_vector)
                    || ((in_forced_vector_ancestor || obj.perspective.is_some())
                        && contents.iter().any(|cid| {
                            sym_map
                                .and_then(|m| m.get(cid).copied())
                                .or_else(|| doc.object(*cid))
                                .is_some_and(|child| {
                                    object_requires_raster_composite_inner(
                                        doc,
                                        child,
                                        sym_map,
                                        true,
                                        opts,
                                        visited_objs,
                                        visited_syms,
                                        depth + 1,
                                    )
                                })
                        }))
            }
            _ => fill_requires_raster_composite(&obj.fill, opts.keep_pattern_fills_vector),
        }
    };
    if obj.id.0 != 0 {
        visited_objs.remove(&obj.id);
    }
    result
}

fn compose_world_persp(
    parent_persp: Option<Perspective3x3>,
    effective_local_xf: Affine2D,
    world_xf: Affine2D,
    own_persp: Option<Perspective3x3>,
) -> Option<Perspective3x3> {
    match (parent_persp, own_persp) {
        (Some(pp), Some(op)) => Some(
            pp.compose(&Perspective3x3::from_affine(effective_local_xf))
                .compose(&op),
        ),
        (Some(pp), None) => Some(pp.compose(&Perspective3x3::from_affine(effective_local_xf))),
        (None, Some(op)) => Some(Perspective3x3::from_affine(world_xf).compose(&op)),
        (None, None) => None,
    }
}

fn compose_svg_persp(
    parent_persp: Option<Perspective3x3>,
    effective_local_xf: Affine2D,
    own_persp: Option<Perspective3x3>,
) -> Option<Perspective3x3> {
    match (parent_persp, own_persp) {
        (Some(pp), Some(op)) => Some(
            pp.compose(&Perspective3x3::from_affine(effective_local_xf))
                .compose(&op),
        ),
        (Some(pp), None) => Some(pp.compose(&Perspective3x3::from_affine(effective_local_xf))),
        (None, Some(op)) => Some(Perspective3x3::from_affine(effective_local_xf).compose(&op)),
        (None, None) => None,
    }
}

fn transform_world_path(
    local_deformed: &BezierPath,
    effective_local_xf: Affine2D,
    world_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
) -> BezierPath {
    if let Some(pp) = parent_persp {
        pp.compose(&Perspective3x3::from_affine(effective_local_xf))
            .transform_bezier_path(local_deformed)
    } else {
        local_deformed.transformed(&world_xf)
    }
}

fn transform_svg_local_path(
    local_deformed: &BezierPath,
    effective_local_xf: Affine2D,
    parent_persp: Option<Perspective3x3>,
) -> BezierPath {
    if let Some(pp) = parent_persp {
        pp.compose(&Perspective3x3::from_affine(effective_local_xf))
            .transform_bezier_path(local_deformed)
    } else {
        local_deformed.clone()
    }
}

fn color_has_partial_alpha(c: &Color) -> bool {
    let a = c.alpha();
    a.is_finite() && a < 1.0 - 1e-4
}

fn fill_has_alpha(fill: &Fill) -> bool {
    match fill {
        Fill::None => false,
        Fill::Solid(c) => color_has_partial_alpha(c),
        Fill::LinearGradient { stops, .. }
        | Fill::RadialGradient { stops, .. }
        | Fill::ConicalGradient { stops, .. }
        | Fill::SquareGradient { stops, .. } => {
            stops.iter().any(|s| color_has_partial_alpha(&s.color))
        }
        Fill::TwoColorPattern { fg, bg, .. } => {
            color_has_partial_alpha(fg) || color_has_partial_alpha(bg)
        }
        _ => false,
    }
}

fn mark_vector_transparency(
    fill: &Fill,
    outline: &OutlineStyle,
    opacity: f32,
    blend_mode: BlendMode,
    has_transparency: &mut bool,
) {
    if opacity < 1.0 - 1e-4
        || blend_mode != BlendMode::Normal
        || fill_has_alpha(fill)
        || (outline.is_visible() && color_has_partial_alpha(&outline.color))
    {
        *has_transparency = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_cutcontour_overlays(
    doc: &Document,
    obj: &SceneObject,
    parent_xf: Affine2D,
    parent_world_persp: Option<Perspective3x3>,
    parent_svg_persp: Option<Perspective3x3>,
    parent_opacity: f32,
    sym_map: Option<&HashMap<ObjectId, &SceneObject>>,
    opts: &DisplayListOptions,
    ops: &mut Vec<DisplayOp>,
    has_transparency: &mut bool,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) {
    if depth > MAX_DISPLAY_DEPTH
        || (!opts.include_hidden_objects && !obj.visible)
        || (!opts.include_non_printable_objects && !obj.printable)
    {
        return;
    }
    if obj.id.0 != 0 && !visited.insert(obj.id) {
        return;
    }
    if !parent_xf.is_finite() || !obj.transform.is_finite() {
        if obj.id.0 != 0 {
            visited.remove(&obj.id);
        }
        return;
    }

    let resolved_clone = if matches!(obj.payload, NodePayload::Clone { .. }) {
        resolve_display_list_clone(doc, obj, sym_map)
    } else {
        None
    };
    let (eff_payload, eff_outline): (&NodePayload, &OutlineStyle) = match &resolved_clone {
        Some((p, _, o)) => (p, o),
        None => (&obj.payload, &obj.outline),
    };

    let opacity = (parent_opacity * obj.opacity).clamp(0.0, 1.0);
    let sym_transforms = symmetry_effect_transforms(&obj.effects);
    for (sym_idx, sym_xf) in sym_transforms.into_iter().enumerate() {
        let local_xf = sym_xf.compose(&obj.transform);
        let world_xf = parent_xf.compose(&local_xf);
        if !world_xf.is_finite() {
            continue;
        }
        let id_suffix = if sym_idx == 0 {
            format!("obj-{}-cut", obj.id.0)
        } else {
            format!("obj-{}-cut-sym{sym_idx}", obj.id.0)
        };
        match eff_payload {
            NodePayload::Group(children) => {
                let child_world_persp =
                    compose_world_persp(parent_world_persp, local_xf, world_xf, obj.perspective);
                let child_svg_persp =
                    compose_svg_persp(parent_svg_persp, local_xf, obj.perspective);
                for &cid in children {
                    let child_opt = sym_map
                        .and_then(|m| m.get(&cid).copied())
                        .or_else(|| doc.object(cid));
                    if let Some(child) = child_opt {
                        emit_cutcontour_overlays(
                            doc,
                            child,
                            world_xf,
                            child_world_persp,
                            child_svg_persp,
                            opacity,
                            sym_map,
                            opts,
                            ops,
                            has_transparency,
                            depth + 1,
                            visited,
                        );
                    }
                }
            }
            NodePayload::SymbolInstance(sym_id) => {
                if let Some(sym) = doc.symbol(*sym_id) {
                    let (child_map, referenced) = build_symbol_maps(sym);
                    let child_world_persp = compose_world_persp(
                        parent_world_persp,
                        local_xf,
                        world_xf,
                        obj.perspective,
                    );
                    let child_svg_persp =
                        compose_svg_persp(parent_svg_persp, local_xf, obj.perspective);
                    let mut sym_visited = HashSet::new();
                    for sub in &sym.objects {
                        if sub.id.0 != 0 && referenced.contains(&sub.id) {
                            continue;
                        }
                        emit_cutcontour_overlays(
                            doc,
                            sub,
                            world_xf,
                            child_world_persp,
                            child_svg_persp,
                            opacity,
                            Some(&child_map),
                            opts,
                            ops,
                            has_transparency,
                            depth + 1,
                            &mut sym_visited,
                        );
                    }
                }
            }
            NodePayload::PowerClip {
                container,
                contents,
                ..
            } => {
                let bp = if let Some(persp) = obj.perspective {
                    persp.transform_bezier_path(container)
                } else {
                    (**container).clone()
                };
                let raw_def = apply_geometry_effects(&bp, &obj.effects);
                let clip_path =
                    transform_world_path(&raw_def, local_xf, world_xf, parent_world_persp);
                let local_clip = transform_svg_local_path(&raw_def, local_xf, parent_svg_persp);
                let child_world_persp =
                    compose_world_persp(parent_world_persp, local_xf, world_xf, obj.perspective);
                let child_svg_persp =
                    compose_svg_persp(parent_svg_persp, local_xf, obj.perspective);
                ops.push(DisplayOp::PushClip {
                    id_suffix: format!("{id_suffix}-clip"),
                    path: clip_path.clone(),
                    local_path: local_clip.clone(),
                    fill_rule: FillRule::Winding,
                    local_xf,
                    opacity,
                    blend_mode: obj.blend_mode,
                    visible: obj.visible,
                    is_perspective: parent_world_persp.is_some() || obj.perspective.is_some(),
                });
                for &cid in contents {
                    let child_opt = sym_map
                        .and_then(|m| m.get(&cid).copied())
                        .or_else(|| doc.object(cid));
                    if let Some(child) = child_opt {
                        emit_cutcontour_overlays(
                            doc,
                            child,
                            world_xf,
                            child_world_persp,
                            child_svg_persp,
                            opacity,
                            sym_map,
                            opts,
                            ops,
                            has_transparency,
                            depth + 1,
                            visited,
                        );
                    }
                }
                ops.push(DisplayOp::PopClip);
                if matches!(eff_outline.color, Color::Spot { .. }) && eff_outline.is_visible() {
                    mark_vector_transparency(
                        &Fill::None,
                        eff_outline,
                        opacity,
                        obj.blend_mode,
                        has_transparency,
                    );
                    ops.push(DisplayOp::DrawVector {
                        id: obj.id,
                        id_suffix,
                        path: clip_path,
                        local_path: local_clip,
                        fill: Fill::None,
                        outline: eff_outline.clone(),
                        fill_xf: world_xf,
                        local_xf,
                        opacity,
                        blend_mode: obj.blend_mode,
                        visible: obj.visible,
                        is_perspective: parent_world_persp.is_some() || obj.perspective.is_some(),
                        is_cut_contour: is_cutcontour_color(&eff_outline.color),
                        emit_arrowheads: true,
                    });
                }
            }
            _ => {
                if matches!(eff_outline.color, Color::Spot { .. }) && eff_outline.is_visible() {
                    let raw = match eff_payload {
                        NodePayload::Path(path) => Some(path.clone()),
                        NodePayload::Shape(shape) => Some(shape.to_bezier_path()),
                        NodePayload::BSpline(bs) => Some(bs.to_bezier_path()),
                        NodePayload::ArtisticMedia { spine, style } => {
                            Some(coree_core::ArtisticMedia::expand_stroke(spine, style))
                        }
                        _ => None,
                    };
                    if let Some(raw) = raw {
                        let bp = if let Some(persp) = obj.perspective {
                            persp.transform_bezier_path(&raw)
                        } else {
                            raw
                        };
                        let local_def = apply_geometry_effects(&bp, &obj.effects);
                        let page_path = transform_world_path(
                            &local_def,
                            local_xf,
                            world_xf,
                            parent_world_persp,
                        );
                        let svg_local =
                            transform_svg_local_path(&local_def, local_xf, parent_svg_persp);
                        mark_vector_transparency(
                            &Fill::None,
                            eff_outline,
                            opacity,
                            obj.blend_mode,
                            has_transparency,
                        );
                        ops.push(DisplayOp::DrawVector {
                            id: obj.id,
                            id_suffix,
                            path: page_path,
                            local_path: svg_local,
                            fill: Fill::None,
                            outline: eff_outline.clone(),
                            fill_xf: world_xf,
                            local_xf,
                            opacity,
                            blend_mode: obj.blend_mode,
                            visible: obj.visible,
                            is_perspective: parent_world_persp.is_some()
                                || obj.perspective.is_some(),
                            is_cut_contour: is_cutcontour_color(&eff_outline.color),
                            emit_arrowheads: true,
                        });
                    }
                }
            }
        }
    }
    if obj.id.0 != 0 {
        visited.remove(&obj.id);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_object(
    doc: &Document,
    page_index: usize,
    obj: &SceneObject,
    parent_xf: Affine2D,
    parent_world_persp: Option<Perspective3x3>,
    parent_svg_persp: Option<Perspective3x3>,
    parent_opacity: f32,
    sym_ctx: Option<ExportSymCtx<'_>>,
    opts: &DisplayListOptions,
    text_engine: &mut TextEngine,
    ops: &mut Vec<DisplayOp>,
    has_transparency: &mut bool,
    depth: usize,
    visited: &mut HashSet<ObjectId>,
) {
    if depth > MAX_DISPLAY_DEPTH
        || (!opts.include_hidden_objects && !obj.visible)
        || (!opts.include_non_printable_objects && !obj.printable)
        || (obj.id.0 != 0 && !visited.insert(obj.id))
    {
        return;
    }
    let opacity = (parent_opacity
        * if obj.opacity.is_finite() {
            obj.opacity.clamp(0.0, 1.0)
        } else {
            1.0
        })
    .clamp(0.0, 1.0);
    if !parent_xf.is_finite() || !obj.transform.is_finite() {
        if obj.id.0 != 0 {
            visited.remove(&obj.id);
        }
        return;
    }

    if object_requires_raster_composite(doc, obj, opts)
        && sym_ctx.is_none()
        && obj.id.0 != 0
        && parent_world_persp.is_none()
    {
        if let Ok(Some((rect_mm, rgba_buf))) = render_isolated_object_to_rgba_with_options(
            doc,
            page_index,
            obj.id,
            parent_xf,
            opts.raster_dpi,
            opts.suppress_cutcontour_in_raster,
        ) {
            let rgba_has_alpha = rgba_buf
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[3] < 255);
            if rgba_has_alpha || parent_opacity < 1.0 - 1e-4 || obj.blend_mode != BlendMode::Normal
            {
                *has_transparency = true;
            }
            let bl = Point2D::new(rect_mm.min_x, rect_mm.max_y);
            let br = Point2D::new(rect_mm.max_x, rect_mm.max_y);
            let tl = Point2D::new(rect_mm.min_x, rect_mm.min_y);
            ops.push(DisplayOp::DrawRaster {
                id: obj.id,
                id_suffix: format!("obj-{}", obj.id.0),
                asset_id: None,
                inline_rgba: Some(Arc::new((rgba_buf.width, rgba_buf.height, rgba_buf.rgba))),
                rect_mm: Some(rect_mm),
                bl,
                br,
                tl,
                local_xf: parent_xf.inverse().unwrap_or(Affine2D::IDENTITY),
                world_xf: Affine2D::IDENTITY,
                width_mm: rect_mm.width(),
                height_mm: rect_mm.height(),
                opacity: parent_opacity,
                blend_mode: obj.blend_mode,
                visible: obj.visible,
            });
            if opts.emit_cutcontour_overlays {
                let mut cut_visited = HashSet::new();
                emit_cutcontour_overlays(
                    doc,
                    obj,
                    parent_xf,
                    parent_world_persp,
                    parent_svg_persp,
                    parent_opacity,
                    sym_ctx.map(|(m, _, _)| m),
                    opts,
                    ops,
                    has_transparency,
                    depth,
                    &mut cut_visited,
                );
            }
            visited.remove(&obj.id);
            return;
        }
    }

    let resolved_clone = if matches!(obj.payload, NodePayload::Clone { .. }) {
        resolve_display_list_clone(doc, obj, sym_ctx.map(|(m, _, _)| m))
    } else {
        None
    };
    let (eff_payload, eff_fill, eff_outline): (&NodePayload, &Fill, &OutlineStyle) =
        match &resolved_clone {
            Some((p, f, o)) => (p, f, o),
            None => (&obj.payload, &obj.fill, &obj.outline),
        };

    let sym_transforms = symmetry_effect_transforms(&obj.effects);
    for (sym_idx, sym_xf) in sym_transforms.into_iter().enumerate() {
        let local_xf = sym_xf.compose(&obj.transform);
        let effective_parent = parent_xf.compose(&sym_xf);
        let world_xf = effective_parent.compose(&obj.transform);
        if !world_xf.is_finite() {
            continue;
        }
        let id_suffix = if sym_idx == 0 {
            format!("obj-{}", obj.id.0)
        } else {
            format!("obj-{}-sym{sym_idx}", obj.id.0)
        };
        let is_persp = parent_world_persp.is_some() || obj.perspective.is_some();

        match eff_payload {
            NodePayload::Path(path) => {
                emit_vector_payload(
                    obj,
                    &id_suffix,
                    path,
                    eff_fill,
                    eff_outline,
                    local_xf,
                    world_xf,
                    parent_world_persp,
                    parent_svg_persp,
                    opacity,
                    is_persp,
                    true,
                    ops,
                    has_transparency,
                );
            }
            NodePayload::Shape(shape) => {
                let path = shape.to_bezier_path();
                emit_vector_payload(
                    obj,
                    &id_suffix,
                    &path,
                    eff_fill,
                    eff_outline,
                    local_xf,
                    world_xf,
                    parent_world_persp,
                    parent_svg_persp,
                    opacity,
                    is_persp,
                    true,
                    ops,
                    has_transparency,
                );
            }
            NodePayload::BSpline(bs) => {
                let path = bs.to_bezier_path();
                emit_vector_payload(
                    obj,
                    &id_suffix,
                    &path,
                    eff_fill,
                    eff_outline,
                    local_xf,
                    world_xf,
                    parent_world_persp,
                    parent_svg_persp,
                    opacity,
                    is_persp,
                    true,
                    ops,
                    has_transparency,
                );
            }
            NodePayload::ArtisticMedia { spine, style } => {
                let path = coree_core::ArtisticMedia::expand_stroke(spine, style);
                emit_vector_payload(
                    obj,
                    &id_suffix,
                    &path,
                    eff_fill,
                    eff_outline,
                    local_xf,
                    world_xf,
                    parent_world_persp,
                    parent_svg_persp,
                    opacity,
                    is_persp,
                    false,
                    ops,
                    has_transparency,
                );
            }
            NodePayload::Clone { .. } => {}
            NodePayload::ArtisticText(text) => {
                let (shaped_runs, fallback_curves, svg_fallback_curves) = if is_persp {
                    let base = text_engine.layout_artistic_pieces(text);
                    let mut world_pieces = base.clone();
                    transform_pieces_with_effects(
                        &mut world_pieces,
                        obj,
                        local_xf,
                        world_xf,
                        parent_world_persp,
                    );
                    let mut svg_pieces = base;
                    transform_svg_pieces_with_effects(
                        &mut svg_pieces,
                        obj,
                        local_xf,
                        parent_svg_persp,
                    );
                    (Vec::new(), world_pieces, svg_pieces)
                } else if opts.shape_pdf_text_runs {
                    let (r, c) = text_engine.layout_artistic_pdf_runs(text);
                    (r, c, Vec::new())
                } else {
                    (
                        Vec::new(),
                        text_engine.layout_artistic_pieces(text),
                        Vec::new(),
                    )
                };
                mark_text_transparency(
                    text,
                    eff_outline,
                    opacity,
                    obj.blend_mode,
                    has_transparency,
                );
                ops.push(DisplayOp::DrawTextRuns {
                    id: obj.id,
                    id_suffix,
                    kind: DisplayTextKind::Artistic,
                    artistic: text.clone(),
                    shaped_runs,
                    fallback_curves,
                    svg_fallback_curves,
                    outline: eff_outline.clone(),
                    world_xf,
                    local_xf,
                    sym_xf,
                    opacity,
                    blend_mode: obj.blend_mode,
                    visible: obj.visible,
                    is_perspective: is_persp,
                });
            }
            NodePayload::ParagraphText(para) => {
                let is_linked_target = doc.objects().values().any(|other| {
                    matches!(
                        &other.payload,
                        NodePayload::ParagraphText(p) if p.linked_next_frame == Some(obj.id)
                    )
                });
                let needs_doc_flow = is_persp
                    || para.linked_next_frame.is_some()
                    || is_linked_target
                    || para.wrap_mode != coree_core::TextWrapMode::None
                    || para.drop_cap.as_ref().is_some_and(|d| d.enabled)
                    || para
                        .list_style
                        .as_ref()
                        .is_some_and(|l| !matches!(l, coree_core::ParagraphListStyle::None))
                    || !para.tab_stops_mm.is_empty()
                    || para.runs.iter().any(|r| !r.style.variation_axes.is_empty());
                let (shaped_runs, fallback_curves, svg_fallback_curves) = if is_persp {
                    let base = text_engine.layout_paragraph_in_doc(doc, obj.id, para);
                    let mut world_pieces = base.clone();
                    transform_pieces_with_effects(
                        &mut world_pieces,
                        obj,
                        local_xf,
                        world_xf,
                        parent_world_persp,
                    );
                    let mut svg_pieces = base;
                    transform_svg_pieces_with_effects(
                        &mut svg_pieces,
                        obj,
                        local_xf,
                        parent_svg_persp,
                    );
                    (Vec::new(), world_pieces, svg_pieces)
                } else if opts.shape_pdf_text_runs {
                    let (r, c) = text_engine.layout_paragraph_pdf_runs(doc, obj.id, para);
                    (r, c, Vec::new())
                } else {
                    (
                        Vec::new(),
                        text_engine.layout_paragraph_in_doc(doc, obj.id, para),
                        Vec::new(),
                    )
                };
                let artistic = ArtisticText {
                    runs: para.runs.clone(),
                    direction: para.direction,
                    align: para.align,
                };
                mark_text_transparency(
                    &artistic,
                    eff_outline,
                    opacity,
                    obj.blend_mode,
                    has_transparency,
                );
                ops.push(DisplayOp::DrawTextRuns {
                    id: obj.id,
                    id_suffix,
                    kind: DisplayTextKind::Paragraph { needs_doc_flow },
                    artistic,
                    shaped_runs,
                    fallback_curves,
                    svg_fallback_curves,
                    outline: eff_outline.clone(),
                    world_xf,
                    local_xf,
                    sym_xf,
                    opacity,
                    blend_mode: obj.blend_mode,
                    visible: obj.visible,
                    is_perspective: is_persp,
                });
            }
            NodePayload::TextOnPath(top) => {
                if let Some(world_host_path) = resolve_export_world_path(doc, top.path_id, sym_ctx)
                {
                    let inv_parent = parent_xf.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_host = world_host_path.transformed(&inv_parent);
                    let mut pieces = text_engine.layout_text_on_path_pieces(top, &world_host_path);
                    if inv_parent != Affine2D::IDENTITY {
                        for piece in &mut pieces {
                            piece.path = piece.path.transformed(&inv_parent);
                        }
                    }
                    let (world_pieces, svg_pieces) = if is_persp {
                        let mut wp = pieces.clone();
                        transform_pieces_with_effects(
                            &mut wp,
                            obj,
                            sym_xf,
                            effective_parent,
                            parent_world_persp,
                        );
                        let mut sp = pieces;
                        transform_svg_pieces_with_effects(&mut sp, obj, sym_xf, parent_svg_persp);
                        (wp, sp)
                    } else {
                        (pieces, Vec::new())
                    };
                    mark_text_transparency(
                        &top.text,
                        eff_outline,
                        opacity,
                        obj.blend_mode,
                        has_transparency,
                    );
                    ops.push(DisplayOp::DrawTextRuns {
                        id: obj.id,
                        id_suffix,
                        kind: DisplayTextKind::OnPath {
                            text_on_path: top.clone(),
                            local_host_path: Some(local_host),
                        },
                        artistic: top.text.clone(),
                        shaped_runs: Vec::new(),
                        fallback_curves: world_pieces,
                        svg_fallback_curves: svg_pieces,
                        outline: eff_outline.clone(),
                        world_xf: effective_parent,
                        local_xf: sym_xf,
                        sym_xf,
                        opacity,
                        blend_mode: obj.blend_mode,
                        visible: obj.visible,
                        is_perspective: is_persp,
                    });
                } else {
                    let (shaped_runs, fallback_curves, svg_fallback_curves) = if is_persp {
                        let base = text_engine.layout_artistic_pieces(&top.text);
                        let mut wp = base.clone();
                        transform_pieces_with_effects(
                            &mut wp,
                            obj,
                            local_xf,
                            world_xf,
                            parent_world_persp,
                        );
                        let mut sp = base;
                        transform_svg_pieces_with_effects(&mut sp, obj, local_xf, parent_svg_persp);
                        (Vec::new(), wp, sp)
                    } else if opts.shape_pdf_text_runs {
                        let (r, c) = text_engine.layout_artistic_pdf_runs(&top.text);
                        (r, c, Vec::new())
                    } else {
                        (
                            Vec::new(),
                            text_engine.layout_artistic_pieces(&top.text),
                            Vec::new(),
                        )
                    };
                    mark_text_transparency(
                        &top.text,
                        eff_outline,
                        opacity,
                        obj.blend_mode,
                        has_transparency,
                    );
                    ops.push(DisplayOp::DrawTextRuns {
                        id: obj.id,
                        id_suffix,
                        kind: DisplayTextKind::OnPath {
                            text_on_path: top.clone(),
                            local_host_path: None,
                        },
                        artistic: top.text.clone(),
                        shaped_runs,
                        fallback_curves,
                        svg_fallback_curves,
                        outline: eff_outline.clone(),
                        world_xf,
                        local_xf,
                        sym_xf,
                        opacity,
                        blend_mode: obj.blend_mode,
                        visible: obj.visible,
                        is_perspective: is_persp,
                    });
                }
            }
            NodePayload::Raster(raster) => {
                if let Some(asset) = doc.asset(raster.asset_id) {
                    if opacity < 1.0 - 1e-4
                        || obj.blend_mode != BlendMode::Normal
                        || asset_has_alpha(asset)
                    {
                        *has_transparency = true;
                    }
                    let bl = world_xf.transform_point(Point2D::new(0.0, raster.height_mm));
                    let br =
                        world_xf.transform_point(Point2D::new(raster.width_mm, raster.height_mm));
                    let tl = world_xf.transform_point(Point2D::new(0.0, 0.0));
                    ops.push(DisplayOp::DrawRaster {
                        id: obj.id,
                        id_suffix,
                        asset_id: Some(raster.asset_id),
                        inline_rgba: None,
                        rect_mm: None,
                        bl,
                        br,
                        tl,
                        local_xf,
                        world_xf,
                        width_mm: raster.width_mm,
                        height_mm: raster.height_mm,
                        opacity,
                        blend_mode: obj.blend_mode,
                        visible: obj.visible,
                    });
                }
            }
            NodePayload::Table(table) => {
                ops.push(DisplayOp::PushGroup {
                    id_suffix: id_suffix.clone(),
                    local_xf,
                    opacity,
                    blend_mode: obj.blend_mode,
                    visible: obj.visible,
                    is_perspective: is_persp,
                });
                for r in 0..table.rows {
                    for c in 0..table.cols {
                        if let (Some(cell), Some(rect)) = (table.cell(r, c), table.cell_rect(r, c))
                        {
                            let cell_local = BezierPath::from_rect(rect);
                            let cell_world = transform_world_path(
                                &cell_local,
                                local_xf,
                                world_xf,
                                parent_world_persp,
                            );
                            let cell_svg =
                                transform_svg_local_path(&cell_local, local_xf, parent_svg_persp);
                            mark_vector_transparency(
                                &cell.fill,
                                &table.border_style,
                                opacity,
                                obj.blend_mode,
                                has_transparency,
                            );
                            ops.push(DisplayOp::DrawVector {
                                id: obj.id,
                                id_suffix: format!("{id_suffix}-r{r}c{c}"),
                                path: cell_world,
                                local_path: cell_svg,
                                fill: cell.fill.clone(),
                                outline: table.border_style.clone(),
                                fill_xf: world_xf,
                                local_xf: Affine2D::IDENTITY,
                                opacity,
                                blend_mode: obj.blend_mode,
                                visible: obj.visible,
                                is_perspective: is_persp,
                                is_cut_contour: is_cutcontour_color(&table.border_style.color),
                                emit_arrowheads: false,
                            });
                            if !cell.text.is_empty() {
                                let tx = Affine2D::translate(
                                    rect.min_x + cell.padding_mm,
                                    rect.min_y + cell.padding_mm,
                                );
                                let at = ArtisticText::from_plain(
                                    cell.text.clone(),
                                    cell.text_style.clone(),
                                );
                                let (shaped_runs, fallback_curves, svg_fallback_curves) =
                                    if is_persp {
                                        let base = text_engine.layout_artistic_pieces(&at);
                                        let cell_local_xf = local_xf.compose(&tx);
                                        let cell_world_xf = world_xf.compose(&tx);
                                        let mut pieces = base.clone();
                                        for piece in &mut pieces {
                                            piece.path = transform_world_path(
                                                &piece.path,
                                                cell_local_xf,
                                                cell_world_xf,
                                                parent_world_persp,
                                            );
                                        }
                                        let mut svg_pieces = base;
                                        for piece in &mut svg_pieces {
                                            piece.path = transform_svg_local_path(
                                                &piece.path,
                                                cell_local_xf,
                                                parent_svg_persp,
                                            );
                                        }
                                        (Vec::new(), pieces, svg_pieces)
                                    } else if opts.shape_pdf_text_runs {
                                        let (r, c) = text_engine.layout_artistic_pdf_runs(&at);
                                        (r, c, Vec::new())
                                    } else {
                                        (
                                            Vec::new(),
                                            text_engine.layout_artistic_pieces(&at),
                                            Vec::new(),
                                        )
                                    };
                                ops.push(DisplayOp::DrawTextRuns {
                                    id: obj.id,
                                    id_suffix: format!("{id_suffix}-r{r}c{c}-txt"),
                                    kind: DisplayTextKind::TableCell,
                                    artistic: at,
                                    shaped_runs,
                                    fallback_curves,
                                    svg_fallback_curves,
                                    outline: OutlineStyle::none(),
                                    world_xf: world_xf.compose(&tx),
                                    local_xf: tx,
                                    sym_xf: Affine2D::IDENTITY,
                                    opacity,
                                    blend_mode: obj.blend_mode,
                                    visible: obj.visible,
                                    is_perspective: is_persp,
                                });
                            }
                        }
                    }
                }
                ops.push(DisplayOp::PopGroup);
            }
            NodePayload::Dimension(dim) => {
                ops.push(DisplayOp::PushGroup {
                    id_suffix: id_suffix.clone(),
                    local_xf,
                    opacity,
                    blend_mode: obj.blend_mode,
                    visible: obj.visible,
                    is_perspective: is_persp,
                });
                let line_local = BezierPath::from_line(
                    Point2D::new(dim.p1.x, dim.p1.y - dim.offset_mm),
                    Point2D::new(dim.p2.x, dim.p2.y - dim.offset_mm),
                );
                let line_world =
                    transform_world_path(&line_local, local_xf, world_xf, parent_world_persp);
                let line_svg = transform_svg_local_path(&line_local, local_xf, parent_svg_persp);
                mark_vector_transparency(
                    &Fill::None,
                    eff_outline,
                    opacity,
                    obj.blend_mode,
                    has_transparency,
                );
                ops.push(DisplayOp::DrawVector {
                    id: obj.id,
                    id_suffix: format!("{id_suffix}-line"),
                    path: line_world,
                    local_path: line_svg,
                    fill: Fill::None,
                    outline: eff_outline.clone(),
                    fill_xf: world_xf,
                    local_xf: Affine2D::IDENTITY,
                    opacity,
                    blend_mode: obj.blend_mode,
                    visible: obj.visible,
                    is_perspective: is_persp,
                    is_cut_contour: is_cutcontour_color(&eff_outline.color),
                    emit_arrowheads: false,
                });
                let label =
                    dim.compute_measurement_with_dpi(doc.default_unit(), f64::from(doc.dpi()));
                let mid = dim.p1.midpoint(dim.p2);
                let at = ArtisticText::from_plain(label, dim.text_style.clone());
                let tx = Affine2D::translate(mid.x, mid.y - dim.offset_mm - 2.0);
                let (shaped_runs, fallback_curves, svg_fallback_curves) = if is_persp {
                    let base = text_engine.layout_artistic_pieces(&at);
                    let dim_local_xf = local_xf.compose(&tx);
                    let dim_world_xf = world_xf.compose(&tx);
                    let mut pieces = base.clone();
                    for piece in &mut pieces {
                        piece.path = transform_world_path(
                            &piece.path,
                            dim_local_xf,
                            dim_world_xf,
                            parent_world_persp,
                        );
                    }
                    let mut svg_pieces = base;
                    for piece in &mut svg_pieces {
                        piece.path =
                            transform_svg_local_path(&piece.path, dim_local_xf, parent_svg_persp);
                    }
                    (Vec::new(), pieces, svg_pieces)
                } else if opts.shape_pdf_text_runs {
                    let (r, c) = text_engine.layout_artistic_pdf_runs(&at);
                    (r, c, Vec::new())
                } else {
                    (
                        Vec::new(),
                        text_engine.layout_artistic_pieces(&at),
                        Vec::new(),
                    )
                };
                ops.push(DisplayOp::DrawTextRuns {
                    id: obj.id,
                    id_suffix: format!("{id_suffix}-lbl"),
                    kind: DisplayTextKind::DimensionLabel,
                    artistic: at,
                    shaped_runs,
                    fallback_curves,
                    svg_fallback_curves,
                    outline: OutlineStyle::none(),
                    world_xf: world_xf.compose(&tx),
                    local_xf: tx,
                    sym_xf: Affine2D::IDENTITY,
                    opacity,
                    blend_mode: obj.blend_mode,
                    visible: obj.visible,
                    is_perspective: is_persp,
                });
                ops.push(DisplayOp::PopGroup);
            }
            NodePayload::Connector(conn) => {
                let (a, b) = resolve_export_connector_endpoints(doc, conn, &world_xf, sym_ctx);
                let world_path = conn.route_path(a, b);
                let local_path = parent_xf
                    .inverse()
                    .map_or_else(|| world_path.clone(), |inv| world_path.transformed(&inv));
                let out_world = if let Some(pp) = parent_world_persp {
                    pp.transform_bezier_path(&local_path)
                } else {
                    world_path
                };
                let out_local = if let Some(pp) = parent_svg_persp {
                    pp.transform_bezier_path(&local_path)
                } else {
                    local_path
                };
                mark_vector_transparency(
                    &Fill::None,
                    eff_outline,
                    opacity,
                    obj.blend_mode,
                    has_transparency,
                );
                ops.push(DisplayOp::DrawVector {
                    id: obj.id,
                    id_suffix,
                    path: out_world,
                    local_path: out_local,
                    fill: Fill::None,
                    outline: eff_outline.clone(),
                    fill_xf: world_xf,
                    local_xf: Affine2D::IDENTITY,
                    opacity,
                    blend_mode: obj.blend_mode,
                    visible: obj.visible,
                    is_perspective: is_persp,
                    is_cut_contour: is_cutcontour_color(&eff_outline.color),
                    emit_arrowheads: true,
                });
            }
            NodePayload::Group(children) => {
                let child_world_persp =
                    compose_world_persp(parent_world_persp, local_xf, world_xf, obj.perspective);
                let child_svg_persp =
                    compose_svg_persp(parent_svg_persp, local_xf, obj.perspective);
                let need_transp = obj.opacity < 1.0 - 1e-4 || obj.blend_mode != BlendMode::Normal;
                if need_transp {
                    *has_transparency = true;
                    ops.push(DisplayOp::PushTransparencyGroup {
                        opacity,
                        blend_mode: obj.blend_mode,
                        isolated: true,
                        knockout: false,
                    });
                }
                ops.push(DisplayOp::PushGroup {
                    id_suffix,
                    local_xf,
                    opacity,
                    blend_mode: obj.blend_mode,
                    visible: obj.visible,
                    is_perspective: child_world_persp.is_some(),
                });
                for &cid in children {
                    let child_opt = sym_ctx
                        .and_then(|(m, _, _)| m.get(&cid).copied())
                        .or_else(|| doc.object(cid));
                    if let Some(child) = child_opt {
                        emit_object(
                            doc,
                            page_index,
                            child,
                            world_xf,
                            child_world_persp,
                            child_svg_persp,
                            opacity,
                            sym_ctx,
                            opts,
                            text_engine,
                            ops,
                            has_transparency,
                            depth + 1,
                            visited,
                        );
                    }
                }
                ops.push(DisplayOp::PopGroup);
                if need_transp {
                    ops.push(DisplayOp::PopTransparencyGroup);
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
                let raw_container = apply_geometry_effects(&container_bp, &obj.effects);
                let clip_world =
                    transform_world_path(&raw_container, local_xf, world_xf, parent_world_persp);
                let clip_svg = if parent_svg_persp.is_some() {
                    transform_svg_local_path(&raw_container, local_xf, parent_svg_persp)
                } else if obj.perspective.is_some() {
                    raw_container.transformed(&local_xf)
                } else {
                    raw_container
                };
                let child_world_persp =
                    compose_world_persp(parent_world_persp, local_xf, world_xf, obj.perspective);
                let child_svg_persp =
                    compose_svg_persp(parent_svg_persp, local_xf, obj.perspective);
                let need_transp = obj.opacity < 1.0 - 1e-4 || obj.blend_mode != BlendMode::Normal;
                if need_transp {
                    *has_transparency = true;
                    ops.push(DisplayOp::PushTransparencyGroup {
                        opacity,
                        blend_mode: obj.blend_mode,
                        isolated: true,
                        knockout: false,
                    });
                }
                if eff_fill.is_visible() {
                    mark_vector_transparency(
                        eff_fill,
                        &OutlineStyle::none(),
                        opacity,
                        obj.blend_mode,
                        has_transparency,
                    );
                    ops.push(DisplayOp::DrawVector {
                        id: obj.id,
                        id_suffix: format!("{id_suffix}-bg"),
                        path: clip_world.clone(),
                        local_path: clip_svg.clone(),
                        fill: eff_fill.clone(),
                        outline: OutlineStyle::none(),
                        fill_xf: world_xf,
                        local_xf,
                        opacity,
                        blend_mode: obj.blend_mode,
                        visible: obj.visible,
                        is_perspective: is_persp,
                        is_cut_contour: false,
                        emit_arrowheads: false,
                    });
                }
                ops.push(DisplayOp::PushClip {
                    id_suffix: id_suffix.clone(),
                    path: clip_world.clone(),
                    local_path: clip_svg.clone(),
                    fill_rule: FillRule::Winding,
                    local_xf,
                    opacity,
                    blend_mode: obj.blend_mode,
                    visible: obj.visible,
                    is_perspective: child_world_persp.is_some(),
                });
                for &cid in contents {
                    let child_opt = sym_ctx
                        .and_then(|(m, _, _)| m.get(&cid).copied())
                        .or_else(|| doc.object(cid));
                    if let Some(child) = child_opt {
                        emit_object(
                            doc,
                            page_index,
                            child,
                            world_xf,
                            child_world_persp,
                            child_svg_persp,
                            opacity,
                            sym_ctx,
                            opts,
                            text_engine,
                            ops,
                            has_transparency,
                            depth + 1,
                            visited,
                        );
                    }
                }
                ops.push(DisplayOp::PopClip);
                if eff_outline.is_visible() {
                    mark_vector_transparency(
                        &Fill::None,
                        eff_outline,
                        opacity,
                        obj.blend_mode,
                        has_transparency,
                    );
                    ops.push(DisplayOp::DrawVector {
                        id: obj.id,
                        id_suffix,
                        path: clip_world,
                        local_path: clip_svg,
                        fill: Fill::None,
                        outline: eff_outline.clone(),
                        fill_xf: world_xf,
                        local_xf,
                        opacity,
                        blend_mode: obj.blend_mode,
                        visible: obj.visible,
                        is_perspective: is_persp,
                        is_cut_contour: is_cutcontour_color(&eff_outline.color),
                        emit_arrowheads: false,
                    });
                }
                if need_transp {
                    ops.push(DisplayOp::PopTransparencyGroup);
                }
            }
            NodePayload::SymbolInstance(sym_id) => {
                if let Some(sym) = doc.symbol(*sym_id) {
                    let (child_map, referenced) = build_symbol_maps(sym);
                    let child_world_persp = compose_world_persp(
                        parent_world_persp,
                        local_xf,
                        world_xf,
                        obj.perspective,
                    );
                    let child_svg_persp =
                        compose_svg_persp(parent_svg_persp, local_xf, obj.perspective);
                    let need_transp =
                        obj.opacity < 1.0 - 1e-4 || obj.blend_mode != BlendMode::Normal;
                    if need_transp {
                        *has_transparency = true;
                        ops.push(DisplayOp::PushTransparencyGroup {
                            opacity,
                            blend_mode: obj.blend_mode,
                            isolated: true,
                            knockout: false,
                        });
                    }
                    ops.push(DisplayOp::PushGroup {
                        id_suffix,
                        local_xf,
                        opacity,
                        blend_mode: obj.blend_mode,
                        visible: obj.visible,
                        is_perspective: child_world_persp.is_some(),
                    });
                    let mut sym_visited = HashSet::new();
                    for sub in &sym.objects {
                        if sub.id.0 != 0 && referenced.contains(&sub.id) {
                            continue;
                        }
                        emit_object(
                            doc,
                            page_index,
                            sub,
                            world_xf,
                            child_world_persp,
                            child_svg_persp,
                            opacity,
                            Some((&child_map, &sym.objects, world_xf)),
                            opts,
                            text_engine,
                            ops,
                            has_transparency,
                            depth + 1,
                            &mut sym_visited,
                        );
                    }
                    ops.push(DisplayOp::PopGroup);
                    if need_transp {
                        ops.push(DisplayOp::PopTransparencyGroup);
                    }
                }
            }
        }
    }

    if obj.id.0 != 0 {
        visited.remove(&obj.id);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_vector_payload(
    obj: &SceneObject,
    id_suffix: &str,
    raw_path: &BezierPath,
    fill: &Fill,
    outline: &OutlineStyle,
    local_xf: Affine2D,
    world_xf: Affine2D,
    parent_world_persp: Option<Perspective3x3>,
    parent_svg_persp: Option<Perspective3x3>,
    opacity: f32,
    is_persp: bool,
    emit_arrowheads: bool,
    ops: &mut Vec<DisplayOp>,
    has_transparency: &mut bool,
) {
    let bp = if let Some(persp) = obj.perspective {
        persp.transform_bezier_path(raw_path)
    } else {
        raw_path.clone()
    };
    let deformed = apply_geometry_effects(&bp, &obj.effects);
    let world_path = transform_world_path(&deformed, local_xf, world_xf, parent_world_persp);
    let local_path = transform_svg_local_path(&deformed, local_xf, parent_svg_persp);
    mark_vector_transparency(fill, outline, opacity, obj.blend_mode, has_transparency);
    ops.push(DisplayOp::DrawVector {
        id: obj.id,
        id_suffix: id_suffix.to_owned(),
        path: world_path,
        local_path,
        fill: fill.clone(),
        outline: outline.clone(),
        fill_xf: world_xf,
        local_xf,
        opacity,
        blend_mode: obj.blend_mode,
        visible: obj.visible,
        is_perspective: is_persp,
        is_cut_contour: outline.is_visible() && is_cutcontour_color(&outline.color),
        emit_arrowheads,
    });
}

fn transform_pieces_with_effects(
    pieces: &mut [ShapedGlyphDraw],
    obj: &SceneObject,
    local_xf: Affine2D,
    world_xf: Affine2D,
    parent_world_persp: Option<Perspective3x3>,
) {
    for piece in pieces {
        let bp = if let Some(persp) = obj.perspective {
            persp.transform_bezier_path(&piece.path)
        } else {
            piece.path.clone()
        };
        let deformed = apply_geometry_effects(&bp, &obj.effects);
        piece.path = transform_world_path(&deformed, local_xf, world_xf, parent_world_persp);
    }
}

fn transform_svg_pieces_with_effects(
    pieces: &mut [ShapedGlyphDraw],
    obj: &SceneObject,
    local_xf: Affine2D,
    parent_svg_persp: Option<Perspective3x3>,
) {
    for piece in pieces {
        let bp = if let Some(persp) = obj.perspective {
            persp.transform_bezier_path(&piece.path)
        } else {
            piece.path.clone()
        };
        let deformed = apply_geometry_effects(&bp, &obj.effects);
        piece.path = transform_svg_local_path(&deformed, local_xf, parent_svg_persp);
    }
}

fn asset_has_alpha(asset: &coree_core::RasterAsset) -> bool {
    match asset.pixel_depth() {
        coree_core::PixelDepth::Bit8 => asset
            .tiles
            .iter()
            .any(|t| t.data.as_chunks::<4>().0.iter().any(|px| px[3] < 255)),
        coree_core::PixelDepth::Bit16 => asset.tiles.iter().any(|t| {
            t.data
                .as_chunks::<8>()
                .0
                .iter()
                .any(|px| u16::from_le_bytes([px[6], px[7]]) < 65535)
        }),
    }
}

fn mark_text_transparency(
    text: &ArtisticText,
    outline: &OutlineStyle,
    opacity: f32,
    blend_mode: BlendMode,
    has_transparency: &mut bool,
) {
    if opacity < 1.0 - 1e-4
        || blend_mode != BlendMode::Normal
        || (outline.is_visible() && color_has_partial_alpha(&outline.color))
        || text
            .runs
            .iter()
            .any(|r| color_has_partial_alpha(&r.style.fill))
    {
        *has_transparency = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{CornerStyle, FeatherDirection, MeshNode, ParametricShape};

    #[test]
    fn display_list_resolves_clone_powerclip_and_inherited_mesh_composite() {
        let mut doc = Document::new_a4("DisplayList Clone");
        let mut master = SceneObject::new(
            ObjectId(0),
            "Master Mesh",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 40.0,
                height: 20.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        );
        master.fill = Fill::MeshFill {
            rows: 1,
            cols: 1,
            nodes: vec![
                MeshNode::new(0, 0, Point2D::new(0.0, 0.0), Color::CYAN),
                MeshNode::new(0, 1, Point2D::new(40.0, 0.0), Color::MAGENTA),
                MeshNode::new(1, 0, Point2D::new(0.0, 20.0), Color::YELLOW),
                MeshNode::new(1, 1, Point2D::new(40.0, 20.0), Color::BLACK),
            ],
        };
        let master_id = doc.add_object(master, None).unwrap();

        let clone_obj = SceneObject::new(
            ObjectId(0),
            "Clone Mesh",
            NodePayload::Clone {
                master_id,
                link_fill: true,
                link_outline: true,
                link_shape: true,
            },
        )
        .with_transform(Affine2D::translate(50.0, 10.0));
        let clone_id = doc.add_object(clone_obj, None).unwrap();

        let inner = SceneObject::new(
            ObjectId(0),
            "Inner",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 30.0,
                height: 30.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::CYAN));
        let inner_id = doc.add_object(inner, None).unwrap();
        let mut pc = SceneObject::new(
            ObjectId(0),
            "PC Master",
            NodePayload::PowerClip {
                container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                    0.0, 0.0, 25.0, 25.0,
                ))),
                container_parametric: None,
                contents: vec![inner_id],
                lock_contents: false,
            },
        );
        pc.outline = OutlineStyle::solid(Color::spot("CutContour", 1.0, [0.0, 1.0, 0.0, 0.0]), 0.2);
        pc.effects.push(Effect::InnerShadow {
            dx_mm: 1.0,
            dy_mm: 1.0,
            blur_mm: 1.5,
            opacity: 0.5,
            color: Color::BLACK,
            feather_direction: FeatherDirection::Middle,
        });
        let pc_id = doc.add_object(pc, None).unwrap();
        let pc_clone = SceneObject::new(
            ObjectId(0),
            "PC Clone",
            NodePayload::Clone {
                master_id: pc_id,
                link_fill: true,
                link_outline: true,
                link_shape: true,
            },
        )
        .with_transform(Affine2D::translate(10.0, 60.0));
        doc.add_object(pc_clone, None).unwrap();

        let opts = DisplayListOptions::pdf();
        assert!(
            object_requires_raster_composite(&doc, doc.object(clone_id).unwrap(), &opts),
            "Clone of MeshFill master must require raster composite"
        );
        let mut te = TextEngine::new();
        let dl = ExportDisplayList::build(&doc, 0, &opts, &mut te).unwrap();
        assert!(dl
            .ops
            .iter()
            .any(|op| matches!(op, DisplayOp::DrawRaster { .. })));
        let cut_count = dl
            .ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    DisplayOp::DrawVector {
                        is_cut_contour: true,
                        ..
                    }
                )
            })
            .count();
        assert!(
            cut_count >= 2,
            "expected CutContour overlay for both PowerClip master and Clone->PowerClip, got {cut_count}"
        );
    }

    #[test]
    fn display_list_resolves_clone_inside_symbol_def_across_svg_pdf_eps_wmf_and_cutcontour() {
        use coree_core::{EllipseMode, SymbolDef, SymbolId, TextOnPath};

        let mut doc = Document::new_a4("SymbolCloneDisplayList");
        let sym_id = SymbolId::new();
        let red = Color::rgb(1.0, 0.0, 0.0);
        let blue = Color::rgb(0.0, 0.0, 1.0);

        let sym_master = SceneObject::new(
            ObjectId(201),
            "SymMaster",
            NodePayload::Shape(ParametricShape::Ellipse {
                rx: 8.0,
                ry: 8.0,
                mode: EllipseMode::Full,
                start_deg: 0.0,
                end_deg: 360.0,
            }),
        )
        .with_fill(Fill::Solid(red.clone()))
        .with_outline(OutlineStyle::solid(Color::BLACK, 0.4))
        .with_transform(Affine2D::translate(15.0, 15.0));

        // Direct clone inheriting master fill/outline
        let sym_clone = SceneObject::new(
            ObjectId(202),
            "SymClone",
            NodePayload::Clone {
                master_id: ObjectId(201),
                link_fill: true,
                link_outline: true,
                link_shape: true,
            },
        )
        .with_fill(Fill::None)
        .with_transform(Affine2D::translate(45.0, 15.0));

        // Chained clone overriding fill (link_fill=false) through sym_clone -> sym_master
        let sym_chained = SceneObject::new(
            ObjectId(203),
            "SymChainedClone",
            NodePayload::Clone {
                master_id: ObjectId(202),
                link_fill: false,
                link_outline: true,
                link_shape: true,
            },
        )
        .with_fill(Fill::Solid(blue.clone()))
        .with_transform(Affine2D::translate(75.0, 15.0));

        // Unlinked shape clone (link_shape=false) must be ignored cleanly
        let sym_unlinked = SceneObject::new(
            ObjectId(204),
            "SymUnlinked",
            NodePayload::Clone {
                master_id: ObjectId(201),
                link_fill: true,
                link_outline: true,
                link_shape: false,
            },
        )
        .with_transform(Affine2D::translate(105.0, 15.0));

        // TextOnPath inside SymbolDef targeting a symbol-local Clone
        let sym_top = SceneObject::new(
            ObjectId(205),
            "SymTopOnClone",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain("SYM", Default::default()),
                path_id: ObjectId(202),
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        );

        doc.upsert_symbol(SymbolDef {
            id: sym_id,
            name: "SymWithClones".into(),
            objects: vec![sym_master, sym_clone, sym_chained, sym_unlinked, sym_top],
        });

        let inst = SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
            .with_transform(Affine2D::translate(10.0, 10.0));
        doc.add_object(inst, None).unwrap();

        let mut te = TextEngine::new();
        let dl = ExportDisplayList::build(&doc, 0, &DisplayListOptions::svg(), &mut te).unwrap();
        let vec_ids: Vec<ObjectId> = dl
            .ops
            .iter()
            .filter_map(|op| match op {
                DisplayOp::DrawVector { id, .. } => Some(*id),
                _ => None,
            })
            .collect();
        assert!(vec_ids.contains(&ObjectId(201)));
        assert!(vec_ids.contains(&ObjectId(202)));
        assert!(vec_ids.contains(&ObjectId(203)));
        assert!(!vec_ids.contains(&ObjectId(204)));

        // Chained clone must preserve outer fill=blue while inheriting outline
        let chained_op = dl.ops.iter().find_map(|op| match op {
            DisplayOp::DrawVector {
                id, fill, outline, ..
            } if *id == ObjectId(203) => Some((fill, outline)),
            _ => None,
        });
        let (chained_fill, chained_outline) = chained_op.expect("chained clone vector op");
        assert_eq!(*chained_fill, Fill::Solid(blue));
        assert!(chained_outline.is_visible());

        // TextOnPath targeting symbol-local Clone must resolve local_host_path via resolve_export_world_path
        let top_resolved = dl.ops.iter().any(|op| {
            matches!(
                op,
                DisplayOp::DrawTextRuns {
                    id,
                    kind: DisplayTextKind::OnPath {
                        local_host_path: Some(_),
                        ..
                    },
                    ..
                } if *id == ObjectId(205)
            )
        });
        assert!(
            top_resolved,
            "TextOnPath inside SymbolDef targeting Clone must resolve local_host_path"
        );

        // Cross-format verification: SVG, PDF, EPS, WMF
        let svg = crate::svg::export_svg_to_string(&doc, 0).unwrap();
        assert!(svg.contains("id=\"obj-201\""), "SVG missing SymMaster");
        assert!(svg.contains("id=\"obj-202\""), "SVG missing SymClone");
        assert!(
            svg.contains("id=\"obj-203\""),
            "SVG missing SymChainedClone"
        );
        assert!(
            !svg.contains("id=\"obj-204\""),
            "SVG should skip link_shape=false"
        );

        let pdf_bytes =
            crate::pdf::export_pdf_to_bytes(&doc, &crate::pdf::PdfExportOptions::default())
                .unwrap();
        let pdf_rt = crate::pdf_import::import_pdf_bytes(&pdf_bytes, "PdfRt").unwrap();
        let pdf_paths = pdf_rt
            .objects()
            .values()
            .filter(|o| matches!(o.payload, NodePayload::Path(_)))
            .count();
        assert!(
            pdf_paths >= 3,
            "PDF round-trip should contain master + 2 clones (>=3 paths), got {pdf_paths}"
        );

        let eps_str = crate::eps::export_eps_to_string(&doc, 0).unwrap();
        let eps_rt = crate::eps::import_eps_bytes(eps_str.as_bytes(), "EpsRt").unwrap();
        let eps_centers: Vec<f64> = eps_rt
            .objects()
            .keys()
            .filter_map(|&oid| eps_rt.world_object_bounds(oid).map(|bb| bb.center().x))
            .collect();
        assert!(
            eps_centers.iter().any(|&x| (x - 25.0).abs() < 1.5),
            "EPS missing SymMaster at x=25mm: {eps_centers:?}"
        );
        assert!(
            eps_centers.iter().any(|&x| (x - 55.0).abs() < 1.5),
            "EPS missing SymClone at x=55mm: {eps_centers:?}"
        );
        assert!(
            eps_centers.iter().any(|&x| (x - 85.0).abs() < 1.5),
            "EPS missing SymChainedClone at x=85mm: {eps_centers:?}"
        );

        let wmf_bytes = crate::wmf_emf::export_wmf_to_bytes(&doc, 0).unwrap();
        let wmf_rt = crate::wmf_emf::import_wmf_bytes(&wmf_bytes, "WmfRt").unwrap();
        let wmf_centers: Vec<f64> = wmf_rt
            .objects()
            .keys()
            .filter_map(|&oid| wmf_rt.world_object_bounds(oid).map(|bb| bb.center().x))
            .collect();
        assert!(
            wmf_centers.iter().any(|&x| (x - 25.0).abs() < 1.5),
            "WMF missing SymMaster at x=25mm: {wmf_centers:?}"
        );
        assert!(
            wmf_centers.iter().any(|&x| (x - 55.0).abs() < 1.5),
            "WMF missing SymClone at x=55mm: {wmf_centers:?}"
        );
        assert!(
            wmf_centers.iter().any(|&x| (x - 85.0).abs() < 1.5),
            "WMF missing SymChainedClone at x=85mm: {wmf_centers:?}"
        );

        // CutContour + raster-composite case inside SymbolDef
        let mut cut_doc = Document::new_a4("SymbolCloneCutContour");
        let cut_sym_id = SymbolId::new();
        let mut cut_master = SceneObject::new(
            ObjectId(301),
            "CutMaster",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 20.0,
                height: 20.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::solid(
            Color::spot("CutContour", 1.0, [0.0, 1.0, 0.0, 0.0]),
            0.25,
        ));
        cut_master.effects.push(Effect::InnerShadow {
            dx_mm: 1.0,
            dy_mm: 1.0,
            blur_mm: 1.0,
            opacity: 0.5,
            color: Color::BLACK,
            feather_direction: FeatherDirection::Middle,
        });
        let cut_clone = SceneObject::new(
            ObjectId(302),
            "CutClone",
            NodePayload::Clone {
                master_id: ObjectId(301),
                link_fill: true,
                link_outline: true,
                link_shape: true,
            },
        )
        .with_transform(Affine2D::translate(30.0, 0.0));
        cut_doc.upsert_symbol(SymbolDef {
            id: cut_sym_id,
            name: "CutSym".into(),
            objects: vec![cut_master, cut_clone],
        });
        let cut_inst_id = cut_doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "CutSymInst",
                    NodePayload::SymbolInstance(cut_sym_id),
                ),
                None,
            )
            .unwrap();
        let pdf_opts = DisplayListOptions::pdf();
        assert!(object_requires_raster_composite(
            &cut_doc,
            cut_doc.object(cut_inst_id).unwrap(),
            &pdf_opts
        ));
        let cut_dl = ExportDisplayList::build(&cut_doc, 0, &pdf_opts, &mut te).unwrap();
        let cut_ids: Vec<ObjectId> = cut_dl
            .ops
            .iter()
            .filter_map(|op| match op {
                DisplayOp::DrawVector {
                    id,
                    is_cut_contour: true,
                    ..
                } => Some(*id),
                _ => None,
            })
            .collect();
        assert!(
            cut_ids.contains(&ObjectId(301)) && cut_ids.contains(&ObjectId(302)),
            "expected CutContour overlay for both SymMaster (301) and SymClone (302), got {cut_ids:?}"
        );
        let (rw, rh, rgba, rect) = cut_dl
            .ops
            .iter()
            .find_map(|op| match op {
                DisplayOp::DrawRaster {
                    inline_rgba: Some(arc),
                    rect_mm: Some(r),
                    ..
                } => Some((arc.0, arc.1, &arc.2, *r)),
                _ => None,
            })
            .expect("CutSymInst must emit DrawRaster");
        assert!(
            rect.min_x <= 0.5 && rect.max_x >= 49.5,
            "DrawRaster rect_mm must span CutMaster and CutClone, got {rect:?}"
        );
        let sample_px = |x_mm: f64, y_mm: f64| -> [u8; 4] {
            let px = (((x_mm - rect.min_x) / rect.width()) * f64::from(rw))
                .floor()
                .clamp(0.0, f64::from(rw - 1)) as u32;
            let py = (((y_mm - rect.min_y) / rect.height()) * f64::from(rh))
                .floor()
                .clamp(0.0, f64::from(rh - 1)) as u32;
            let idx = ((py * rw + px) * 4) as usize;
            [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]]
        };
        let master_px = sample_px(10.0, 7.5);
        let clone_px = sample_px(40.0, 7.5);
        assert!(
            master_px[2] > 150 && master_px[1] > 150 && master_px[0] < 100 && master_px[3] > 200,
            "CutMaster raster pixel should be cyan, got {master_px:?}"
        );
        assert!(
            clone_px[2] > 150 && clone_px[1] > 150 && clone_px[0] < 100 && clone_px[3] > 200,
            "CutClone raster pixel should be cyan, got {clone_px:?}"
        );
    }
}
