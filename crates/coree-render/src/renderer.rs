// SPDX-License-Identifier: MPL-2.0

//! Full scene graph renderer supporting single-page and Multipage View, Master
//! layers (`AllPages`/`OddPages`/`EvenPages`), all 11 [`Fill`] variants (including
//! Coons-patch [`Fill::MeshFill`], Conical/Square gradients, Patterns & Textures),
//! complete [`OutlineStyle`] (hairlines, dashes, arrowheads, behind-fill,
//! scale-with-object, calligraphy), `PowerClip`, `VectorTable`, `DimensionObject`,
//! `ConnectorObject`, `Group`, `SymbolInstance`, live Lenses, Effects, and Arabic RTL text.

use crate::backend::{
    BackendPaint, CanvasBackend, FillRule, PaintShader, PixelRect, RgbaBuffer, StrokeParams,
    TinySkiaBackend,
};
use crate::effects_render::{
    apply_geometry_effects, apply_transparency_mask, effect_bounds_padding_mm,
    find_transparency_effect, render_overlay_effects, render_underlay_effects,
    symmetry_effect_transforms,
};
use crate::lens_render::{render_lens_object, FrozenLensSnapshot};
use crate::raster_engine::apply_raster_filters;
use crate::text_layout::TextEngine;
use crate::viewport::{ViewMode, Viewport};
use coree_core::math::{sanitize_f32, sanitize_f64};
use coree_core::{
    Affine2D, ArrowheadStyle, ArtisticText, BezierPath, BlendMode, Color, DimensionKind,
    DimensionObject, Document, Fill, GuideOrientation, Layer, MasterLayerScope, MeshNode,
    NodePayload, ObjectId, OutlineStyle, Page, PerspectiveGridMode, Point2D, Rect2D, SceneObject,
    SpatialIndex, Subpath, SymbolDef, Vec2D, VectorTable,
};
use std::collections::{HashMap, HashSet};

/// Maximum scene recursion depth matching `coree-core`.
const MAX_RENDER_DEPTH: usize = 64;

/// Configurable chrome, guides, and surface styling for [`SceneRenderer`].
#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub show_page_shadow: bool,
    pub show_bleed_guides: bool,
    pub show_margin_guides: bool,
    pub margin_mm: f64,
    pub show_grid: bool,
    pub show_guidelines: bool,
    pub show_perspective_grid: bool,
    pub canvas_bg: Color,
    pub page_bg: Color,
    pub wireframe_stroke_color: Color,
    pub multipage_gap_mm: f64,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self::interactive()
    }
}

impl RenderOptions {
    /// Interactive studio viewport options (pasteboard background, page shadow, guides, grid).
    #[must_use]
    pub fn interactive() -> Self {
        Self {
            show_page_shadow: true,
            show_bleed_guides: true,
            show_margin_guides: false,
            margin_mm: 10.0,
            show_grid: true,
            show_guidelines: true,
            show_perspective_grid: true,
            canvas_bg: Color::rgba8(224, 226, 230, 255),
            page_bg: Color::WHITE,
            wireframe_stroke_color: Color::BLACK,
            multipage_gap_mm: 20.0,
        }
    }

    /// Clean headless export options (no pasteboard shadow, guides, grid, or UI chrome).
    #[must_use]
    pub fn export() -> Self {
        Self {
            show_page_shadow: false,
            show_bleed_guides: false,
            show_margin_guides: false,
            margin_mm: 0.0,
            show_grid: false,
            show_guidelines: false,
            show_perspective_grid: false,
            canvas_bg: Color::WHITE,
            page_bg: Color::WHITE,
            wireframe_stroke_color: Color::BLACK,
            multipage_gap_mm: 20.0,
        }
    }
}

/// Positioned page slot in canvas world millimeters (for single-page or Multipage View).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageLayout {
    pub page_index: usize,
    pub origin_mm: Point2D,
    pub width_mm: f64,
    pub height_mm: f64,
}

impl PageLayout {
    /// World-space bounding rectangle of this page.
    #[must_use]
    pub fn rect_mm(&self) -> Rect2D {
        Rect2D::from_xywh(
            self.origin_mm.x,
            self.origin_mm.y,
            self.width_mm,
            self.height_mm,
        )
    }
}

/// Computes world-space page placements for single-page or side-by-side Multipage View.
#[must_use]
pub fn compute_page_layouts(doc: &Document, multipage_view: bool, gap_mm: f64) -> Vec<PageLayout> {
    if doc.pages.is_empty() {
        return Vec::new();
    }
    if !multipage_view {
        let idx = doc.active_page_index.min(doc.pages.len() - 1);
        let p = &doc.pages[idx];
        return vec![PageLayout {
            page_index: idx,
            origin_mm: Point2D::ZERO,
            width_mm: p.width_mm,
            height_mm: p.height_mm,
        }];
    }

    let gap = sanitize_f64(gap_mm, 20.0).max(0.0);
    let max_h = doc
        .pages
        .iter()
        .map(|p| p.height_mm)
        .fold(0.0_f64, f64::max);
    let mut cursor_x = 0.0_f64;
    let mut layouts = Vec::with_capacity(doc.pages.len());
    for (idx, p) in doc.pages.iter().enumerate() {
        let y = (max_h - p.height_mm) * 0.5;
        layouts.push(PageLayout {
            page_index: idx,
            origin_mm: Point2D::new(cursor_x, y),
            width_mm: p.width_mm,
            height_mm: p.height_mm,
        });
        cursor_x += p.width_mm + gap;
    }
    layouts
}

/// Stateful Pure-Rust 2D scene renderer caching typography and frozen lens snapshots.
pub struct SceneRenderer {
    text_engine: TextEngine,
    frozen_lenses: HashMap<ObjectId, FrozenLensSnapshot>,
}

impl Default for SceneRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneRenderer {
    /// Creates a new scene renderer with initialized system font discovery.
    #[must_use]
    pub fn new() -> Self {
        Self {
            text_engine: TextEngine::new(),
            frozen_lenses: HashMap::new(),
        }
    }

    /// Mutable access to the internal [`TextEngine`].
    pub fn text_engine_mut(&mut self) -> &mut TextEngine {
        &mut self.text_engine
    }

    /// Returns a cached frozen lens snapshot if one exists for `id`.
    #[must_use]
    pub fn get_frozen_lens(&self, id: ObjectId) -> Option<&FrozenLensSnapshot> {
        self.frozen_lenses.get(&id)
    }

    /// Stores a frozen lens snapshot for `id`.
    pub fn store_frozen_lens(&mut self, id: ObjectId, snap: FrozenLensSnapshot) {
        self.frozen_lenses.insert(id, snap);
    }

    /// Removes any cached frozen lens snapshot for `id`.
    pub fn remove_frozen_lens(&mut self, id: ObjectId) {
        self.frozen_lenses.remove(&id);
    }

    /// Clears all cached frozen lens snapshots.
    pub fn clear_frozen_lenses(&mut self) {
        self.frozen_lenses.clear();
    }

    /// Renders `doc` into `backend` according to `viewport`, `view_mode`, and `options`.
    pub fn render_document(
        &mut self,
        doc: &Document,
        viewport: &Viewport,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
    ) {
        self.render_document_with_transform(
            doc,
            viewport,
            viewport.world_to_screen_transform(),
            view_mode,
            options,
            backend,
            None,
            None,
        );
    }

    /// Renders `doc` with an explicit `world_to_surface` transform and optional spatial culling
    /// (used internally by [`crate::tile_cache::DirtyTileCache`] for 256×256 tile rendering).
    #[allow(clippy::too_many_arguments)]
    pub fn render_document_with_transform(
        &mut self,
        doc: &Document,
        viewport: &Viewport,
        world_to_surface: Affine2D,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        cull_world_rect: Option<Rect2D>,
        page_spatial_indices: Option<&HashMap<usize, SpatialIndex>>,
    ) {
        backend.set_flatten_tolerance(viewport.lod_flatten_tolerance_mm());
        backend.clear(options.canvas_bg.clone());

        let multipage = viewport.multipage_view || doc.multipage_view;
        let layouts = compute_page_layouts(doc, multipage, options.multipage_gap_mm);
        let px_per_mm = viewport.px_per_mm();

        for layout in &layouts {
            let Some(page) = doc.pages.get(layout.page_index) else {
                continue;
            };
            let page_to_surface = world_to_surface
                .compose(&Affine2D::translate(layout.origin_mm.x, layout.origin_mm.y));

            // 1. Page drop shadow & paper surface
            render_page_surface(page, page_to_surface, px_per_mm, options, backend);

            // 2. Collect ordered layers: applicable master layers + page's own layers
            let layers = collect_effective_layers(doc, layout.page_index);

            // Optional R-Tree frustum culling candidate set (expanded by max effect padding)
            let culled_set: Option<HashSet<ObjectId>> = cull_world_rect.and_then(|cull_world| {
                let index = page_spatial_indices?.get(&layout.page_index)?;
                let local_cull =
                    cull_world.translate(Vec2D::new(-layout.origin_mm.x, -layout.origin_mm.y));
                let max_pad = compute_page_max_effect_padding(doc, &layers);
                let expanded = local_cull.expand(max_pad + 4.0);
                let mut set: HashSet<ObjectId> = index.query_rect(expanded).into_iter().collect();
                // Always include master-layer objects & objects with Blend/Lens/Viewpoint/Symmetry
                for layer in &layers {
                    let is_master = layer.master_scope != MasterLayerScope::None;
                    for &oid in &layer.objects {
                        if is_master {
                            set.insert(oid);
                        } else if let Some(obj) = doc.objects.get(&oid) {
                            if obj.lens.is_some()
                                || obj.effects.iter().any(|e| {
                                    matches!(
                                        e,
                                        coree_core::Effect::Blend { .. }
                                            | coree_core::Effect::Symmetry { .. }
                                            | coree_core::Effect::Perspective { .. }
                                            | coree_core::Effect::Envelope { .. }
                                    )
                                })
                            {
                                set.insert(oid);
                            }
                        }
                    }
                }
                Some(set)
            });

            // 3. Render objects in strict back-to-front z-order, tracking underlying objects for Lenses
            let mut rendered_so_far: Vec<ObjectId> = Vec::new();
            for layer in &layers {
                if !layer.visible {
                    continue;
                }
                for &oid in &layer.objects {
                    let should_draw = culled_set
                        .as_ref()
                        .is_none_or(|allowed| allowed.contains(&oid));
                    if should_draw {
                        self.render_object_by_id(
                            doc,
                            Some(page),
                            oid,
                            page_to_surface,
                            page_to_surface,
                            view_mode,
                            options,
                            backend,
                            &rendered_so_far,
                            0,
                        );
                    }
                    rendered_so_far.push(oid);
                }
            }

            // 4. Page overlays: Grid, Bleed/Margin guides, Perspective Grid, Guidelines
            render_page_overlays(doc, page, page_to_surface, px_per_mm, options, backend);
        }

        backend.set_flatten_tolerance(None);
    }

    /// Renders a single scene object cleanly (used by `convert_object_to_bitmap`).
    pub fn render_single_object(
        &mut self,
        doc: &Document,
        obj_id: ObjectId,
        viewport: &Viewport,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
    ) {
        let xf = viewport.world_to_screen_transform();
        self.render_object_by_id(
            doc,
            None,
            obj_id,
            xf,
            xf,
            view_mode,
            options,
            backend,
            &[],
            0,
        );
    }

    /// Recursive scene-graph object renderer.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_object_by_id(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        id: ObjectId,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying_ids: &[ObjectId],
        depth: usize,
    ) {
        if depth > MAX_RENDER_DEPTH {
            return;
        }
        let Some(obj) = doc.objects.get(&id) else {
            return;
        };
        self.render_scene_object(
            doc,
            page,
            obj,
            page_to_surface,
            parent_to_screen,
            view_mode,
            options,
            backend,
            underlying_ids,
            depth,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn render_scene_object(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        obj: &SceneObject,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying_ids: &[ObjectId],
        depth: usize,
    ) {
        if !obj.visible || obj.opacity <= 1e-6 || depth > MAX_RENDER_DEPTH {
            return;
        }
        let px_per_mm = estimate_scale(&parent_to_screen);
        let sym_transforms = symmetry_effect_transforms(&obj.effects);

        for sym_xf in sym_transforms {
            let effective_parent = parent_to_screen.compose(&sym_xf);

            // Check if this object uses an Effect::Transparency alpha mask
            if view_mode != ViewMode::Wireframe {
                if let Some((trans_mode, trans_blend)) = find_transparency_effect(&obj.effects) {
                    self.render_with_transparency_effect(
                        doc,
                        page,
                        obj,
                        page_to_surface,
                        effective_parent,
                        view_mode,
                        options,
                        backend,
                        underlying_ids,
                        depth,
                        trans_mode,
                        trans_blend,
                    );
                    continue;
                }
            }

            self.render_scene_object_core(
                doc,
                page,
                obj,
                page_to_surface,
                effective_parent,
                px_per_mm,
                view_mode,
                options,
                backend,
                underlying_ids,
                depth,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_with_transparency_effect(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        obj: &SceneObject,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying_ids: &[ObjectId],
        depth: usize,
        trans_mode: &coree_core::TransparencyMode,
        trans_blend: BlendMode,
    ) {
        let Some(world_bounds) = doc.object_bounds(obj.id) else {
            return;
        };
        let pad = effect_bounds_padding_mm(&obj.effects) + 2.0;
        let sb = parent_to_screen.transform_rect(world_bounds.expand(pad));
        let Some(clamped) =
            PixelRect::from_f64_rect_clamped(sb, 0, backend.width(), backend.height())
        else {
            return;
        };
        let Ok(mut sub) = TinySkiaBackend::new(clamped.width, clamped.height) else {
            return;
        };
        sub.clear(Color::TRANSPARENT);
        let shift = Affine2D::translate(-f64::from(clamped.x), -f64::from(clamped.y));
        let local_page = shift.compose(&page_to_surface);
        let local_parent = shift.compose(&parent_to_screen);
        let px_per_mm = estimate_scale(&parent_to_screen);
        self.render_scene_object_core(
            doc,
            page,
            obj,
            local_page,
            local_parent,
            px_per_mm,
            view_mode,
            options,
            &mut sub,
            underlying_ids,
            depth,
        );

        let mut buf = sub.into_rgba();
        let screen_to_world = parent_to_screen.inverse().unwrap_or(Affine2D::IDENTITY);
        apply_transparency_mask(&mut buf, clamped, screen_to_world, trans_mode);
        backend.composite_region(clamped, &buf, 1.0, trans_blend);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_scene_object_core(
        &mut self,
        doc: &Document,
        page: Option<&Page>,
        obj: &SceneObject,
        page_to_surface: Affine2D,
        parent_to_screen: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        underlying_ids: &[ObjectId],
        depth: usize,
    ) {
        let anti_alias = view_mode != ViewMode::Pixels;
        let obj_to_screen = parent_to_screen.compose(&obj.transform);

        // Handle Lens objects first (all 11 optical & color lenses)
        if view_mode != ViewMode::Wireframe {
            if let Some(lens_cfg) = &obj.lens {
                if let Some(local_path) = match &obj.payload {
                    NodePayload::Path(p) => Some(p.clone()),
                    NodePayload::Shape(s) => Some(s.to_bezier_path()),
                    NodePayload::PowerClip { container, .. } => Some((**container).clone()),
                    _ => None,
                } {
                    let deformed = apply_geometry_effects(&local_path, &obj.effects);
                    render_lens_object(
                        self,
                        doc,
                        page,
                        obj,
                        lens_cfg,
                        &deformed,
                        obj_to_screen,
                        page_to_surface,
                        view_mode,
                        options,
                        backend,
                        underlying_ids,
                        depth,
                    );
                    if obj.outline.is_visible() {
                        draw_outline_styled(
                            &deformed,
                            obj.transform,
                            parent_to_screen,
                            &obj.outline,
                            obj.opacity,
                            obj.blend_mode,
                            px_per_mm,
                            anti_alias,
                            backend,
                        );
                    }
                    return;
                }
            }
        }

        match &obj.payload {
            NodePayload::Path(path) => {
                self.render_vector_geometry(
                    doc,
                    obj,
                    path,
                    parent_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    depth,
                );
            }
            NodePayload::Shape(shape) => {
                let path = shape.to_bezier_path();
                self.render_vector_geometry(
                    doc,
                    obj,
                    &path,
                    parent_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    depth,
                );
            }
            NodePayload::ArtisticText(text) => {
                self.text_engine.render_artistic_text(
                    backend,
                    text,
                    obj_to_screen,
                    &obj.outline,
                    obj.opacity,
                    obj.blend_mode,
                    view_mode,
                    px_per_mm,
                );
            }
            NodePayload::ParagraphText(para) => {
                self.text_engine.render_paragraph_text(
                    backend,
                    para,
                    obj_to_screen,
                    &obj.outline,
                    obj.opacity,
                    obj.blend_mode,
                    view_mode,
                    px_per_mm,
                );
            }
            NodePayload::TextOnPath(top) => {
                if let Some(host_path) = doc.world_path_of(top.path_id) {
                    self.text_engine.render_text_on_path(
                        backend,
                        top,
                        &host_path,
                        parent_to_screen,
                        &obj.outline,
                        obj.opacity,
                        obj.blend_mode,
                        view_mode,
                        px_per_mm,
                    );
                } else {
                    self.text_engine.render_artistic_text(
                        backend,
                        &top.text,
                        obj_to_screen,
                        &obj.outline,
                        obj.opacity,
                        obj.blend_mode,
                        view_mode,
                        px_per_mm,
                    );
                }
            }
            NodePayload::Raster(raster) => {
                render_raster_object(
                    doc,
                    obj,
                    raster,
                    obj_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                );
            }
            NodePayload::Table(table) => {
                self.render_table_object(
                    doc,
                    obj,
                    table,
                    obj_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                    depth,
                );
            }
            NodePayload::Dimension(dim) => {
                self.render_dimension_object(
                    doc,
                    obj,
                    dim,
                    obj_to_screen,
                    px_per_mm,
                    view_mode,
                    options,
                    backend,
                );
            }
            NodePayload::Connector(conn) => {
                let (a, b) = doc.resolve_connector_endpoints(conn);
                let route = conn.route_path(a, b);
                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(&route, parent_to_screen, px_per_mm, options, backend);
                } else if obj.outline.is_visible() {
                    draw_outline_styled(
                        &route,
                        Affine2D::IDENTITY,
                        parent_to_screen,
                        &obj.outline,
                        obj.opacity,
                        obj.blend_mode,
                        px_per_mm,
                        anti_alias,
                        backend,
                    );
                }
            }
            NodePayload::Group(children) => {
                let needs_isolation = view_mode != ViewMode::Wireframe
                    && (obj.opacity < 0.999 || obj.blend_mode != BlendMode::Normal);
                if needs_isolation {
                    if let Some(wb) = doc.object_bounds(obj.id) {
                        let sb = parent_to_screen.transform_rect(wb.expand(4.0));
                        if let Some(clamped) = PixelRect::from_f64_rect_clamped(
                            sb,
                            0,
                            backend.width(),
                            backend.height(),
                        ) {
                            if let Ok(mut sub) = TinySkiaBackend::new(clamped.width, clamped.height)
                            {
                                sub.clear(Color::TRANSPARENT);
                                let shift = Affine2D::translate(
                                    -f64::from(clamped.x),
                                    -f64::from(clamped.y),
                                );
                                let sub_page = shift.compose(&page_to_surface);
                                let sub_xf = shift.compose(&obj_to_screen);
                                for &cid in children {
                                    self.render_object_by_id(
                                        doc,
                                        page,
                                        cid,
                                        sub_page,
                                        sub_xf,
                                        view_mode,
                                        options,
                                        &mut sub,
                                        underlying_ids,
                                        depth + 1,
                                    );
                                }
                                let buf = sub.into_rgba();
                                backend.composite_region(
                                    clamped,
                                    &buf,
                                    obj.opacity,
                                    obj.blend_mode,
                                );
                                return;
                            }
                        }
                    }
                }
                for &cid in children {
                    self.render_object_by_id(
                        doc,
                        page,
                        cid,
                        page_to_surface,
                        obj_to_screen,
                        view_mode,
                        options,
                        backend,
                        underlying_ids,
                        depth + 1,
                    );
                }
            }
            NodePayload::PowerClip {
                container,
                contents,
            } => {
                let deformed_container = apply_geometry_effects(container, &obj.effects);
                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(
                        &deformed_container,
                        obj_to_screen,
                        px_per_mm,
                        options,
                        backend,
                    );
                    for &cid in contents {
                        self.render_object_by_id(
                            doc,
                            page,
                            cid,
                            page_to_surface,
                            obj_to_screen,
                            view_mode,
                            options,
                            backend,
                            underlying_ids,
                            depth + 1,
                        );
                    }
                    return;
                }

                // Underlay effects on PowerClip container
                if view_mode == ViewMode::Enhanced && !obj.effects.is_empty() {
                    render_underlay_effects(
                        doc,
                        obj,
                        &deformed_container,
                        obj.transform,
                        parent_to_screen,
                        px_per_mm,
                        anti_alias,
                        backend,
                    );
                }

                // Behind-fill outline of container
                if obj.outline.behind_fill && obj.outline.is_visible() {
                    draw_outline_styled(
                        &deformed_container,
                        obj.transform,
                        parent_to_screen,
                        &obj.outline,
                        obj.opacity,
                        obj.blend_mode,
                        px_per_mm,
                        anti_alias,
                        backend,
                    );
                }

                // Container background fill
                if obj.fill.is_visible() {
                    self.draw_fill_styled(
                        doc,
                        &deformed_container,
                        &obj.fill,
                        obj_to_screen,
                        obj.opacity,
                        obj.blend_mode,
                        anti_alias,
                        backend,
                        depth,
                    );
                }

                // Clip contents strictly to container contour!
                backend.push_clip(&deformed_container, FillRule::Winding, obj_to_screen);
                for &cid in contents {
                    self.render_object_by_id(
                        doc,
                        page,
                        cid,
                        page_to_surface,
                        obj_to_screen,
                        view_mode,
                        options,
                        backend,
                        underlying_ids,
                        depth + 1,
                    );
                }
                backend.pop_clip();

                // Foreground container outline
                if !obj.outline.behind_fill && obj.outline.is_visible() {
                    draw_outline_styled(
                        &deformed_container,
                        obj.transform,
                        parent_to_screen,
                        &obj.outline,
                        obj.opacity,
                        obj.blend_mode,
                        px_per_mm,
                        anti_alias,
                        backend,
                    );
                }
            }
            NodePayload::SymbolInstance(sym_id) => {
                if let Some(sym) = doc.symbols.get(sym_id) {
                    for sub in &sym.objects {
                        self.render_scene_object(
                            doc,
                            page,
                            sub,
                            page_to_surface,
                            obj_to_screen,
                            view_mode,
                            options,
                            backend,
                            underlying_ids,
                            depth + 1,
                        );
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_vector_geometry(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        local_path: &BezierPath,
        parent_to_screen: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        depth: usize,
    ) {
        let deformed = apply_geometry_effects(local_path, &obj.effects);
        let obj_to_screen = parent_to_screen.compose(&obj.transform);
        let anti_alias = view_mode != ViewMode::Pixels;

        if view_mode == ViewMode::Wireframe {
            stroke_wireframe(&deformed, obj_to_screen, px_per_mm, options, backend);
            return;
        }

        // Underlay live effects (DropShadow, BlockShadow, Contour::Outside, Extrude25D, Blend)
        if !obj.effects.is_empty() {
            render_underlay_effects(
                doc,
                obj,
                &deformed,
                obj.transform,
                parent_to_screen,
                px_per_mm,
                anti_alias,
                backend,
            );
        }

        // Behind-fill outline
        if obj.outline.behind_fill && obj.outline.is_visible() {
            draw_outline_styled(
                &deformed,
                obj.transform,
                parent_to_screen,
                &obj.outline,
                obj.opacity,
                obj.blend_mode,
                px_per_mm,
                anti_alias,
                backend,
            );
        }

        // Fill (all 11 Fill variants!)
        if obj.fill.is_visible() {
            self.draw_fill_styled(
                doc,
                &deformed,
                &obj.fill,
                obj_to_screen,
                obj.opacity,
                obj.blend_mode,
                anti_alias,
                backend,
                depth,
            );
        }

        // Overlay effects (Contour::Inside / ToCenter)
        if !obj.effects.is_empty() {
            render_overlay_effects(
                obj,
                &deformed,
                obj.transform,
                parent_to_screen,
                px_per_mm,
                anti_alias,
                backend,
            );
        }

        // Foreground outline
        if !obj.outline.behind_fill && obj.outline.is_visible() {
            draw_outline_styled(
                &deformed,
                obj.transform,
                parent_to_screen,
                &obj.outline,
                obj.opacity,
                obj.blend_mode,
                px_per_mm,
                anti_alias,
                backend,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_table_object(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        table: &VectorTable,
        obj_to_screen: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
        depth: usize,
    ) {
        let anti_alias = view_mode != ViewMode::Pixels;
        if view_mode == ViewMode::Wireframe {
            stroke_wireframe(
                &table.to_grid_paths(),
                obj_to_screen,
                px_per_mm,
                options,
                backend,
            );
            return;
        }

        // Render cell backgrounds, borders, and cell text
        for r in 0..table.rows {
            for c in 0..table.cols {
                let Some(cell) = table.cell(r, c) else {
                    continue;
                };
                let Some(rect) = table.cell_rect(r, c) else {
                    continue;
                };
                let cell_path = BezierPath::from_rect(rect);
                if cell.fill.is_visible() {
                    self.draw_fill_styled(
                        doc,
                        &cell_path,
                        &cell.fill,
                        obj_to_screen,
                        obj.opacity,
                        obj.blend_mode,
                        anti_alias,
                        backend,
                        depth,
                    );
                } else if obj.fill.is_visible() {
                    self.draw_fill_styled(
                        doc,
                        &cell_path,
                        &obj.fill,
                        obj_to_screen,
                        obj.opacity,
                        obj.blend_mode,
                        anti_alias,
                        backend,
                        depth,
                    );
                }

                if table.border_style.is_visible() {
                    let w = if table.border_style.is_hairline {
                        table
                            .border_style
                            .effective_width_mm()
                            .max(1.0 / px_per_mm.max(1e-3)) as f32
                    } else {
                        table.border_style.effective_width_mm() as f32
                    };
                    let st = StrokeParams::from_outline(
                        &table.border_style,
                        w,
                        obj.opacity,
                        obj.blend_mode,
                        anti_alias,
                    );
                    backend.stroke_path(&cell_path, &st, obj_to_screen);
                }

                if !cell.text.trim().is_empty() {
                    let pad = sanitize_f64(cell.padding_mm, 1.5).max(0.0);
                    let text_xf = obj_to_screen
                        .compose(&Affine2D::translate(rect.min_x + pad, rect.min_y + pad));
                    let at = ArtisticText::from_plain(cell.text.clone(), cell.text_style.clone());
                    backend.push_clip(&cell_path, FillRule::Winding, obj_to_screen);
                    self.text_engine.render_artistic_text(
                        backend,
                        &at,
                        text_xf,
                        &OutlineStyle::none(),
                        obj.opacity,
                        obj.blend_mode,
                        view_mode,
                        px_per_mm,
                    );
                    backend.pop_clip();
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_dimension_object(
        &mut self,
        doc: &Document,
        obj: &SceneObject,
        dim: &DimensionObject,
        obj_to_screen: Affine2D,
        px_per_mm: f64,
        view_mode: ViewMode,
        options: &RenderOptions,
        backend: &mut dyn CanvasBackend,
    ) {
        let anti_alias = view_mode != ViewMode::Pixels;
        let dim_outline = if obj.outline.is_visible() {
            obj.outline.clone()
        } else {
            OutlineStyle::solid(Color::BLACK, 0.25)
        };
        let stroke_w = (dim_outline.effective_width_mm().max(0.2)) as f32;
        let stroke = StrokeParams::from_outline(
            &dim_outline,
            stroke_w,
            obj.opacity,
            obj.blend_mode,
            anti_alias,
        );

        let label_text = dim.compute_measurement_with_dpi(doc.default_unit, f64::from(doc.dpi));
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

                // Extension lines with slight overshoot
                let overshoot = ext_dir * 1.5;
                let ext1 = BezierPath::from_line(dim.p1, d1 + overshoot);
                let ext2 = BezierPath::from_line(dim.p2, d2 + overshoot);
                let main_line = BezierPath::from_line(d1, d2);

                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(&ext1, obj_to_screen, px_per_mm, options, backend);
                    stroke_wireframe(&ext2, obj_to_screen, px_per_mm, options, backend);
                    stroke_wireframe(&main_line, obj_to_screen, px_per_mm, options, backend);
                } else {
                    backend.stroke_path(&ext1, &stroke, obj_to_screen);
                    backend.stroke_path(&ext2, &stroke, obj_to_screen);
                    backend.stroke_path(&main_line, &stroke, obj_to_screen);

                    // Dimension end arrowheads
                    if let Some(dir) = (d2 - d1).normalized() {
                        draw_arrowhead(
                            d1,
                            -dir,
                            &ArrowheadStyle::Triangle,
                            &dim_outline.color,
                            stroke_w.max(0.35),
                            obj.opacity,
                            obj.blend_mode,
                            obj_to_screen,
                            anti_alias,
                            backend,
                        );
                        draw_arrowhead(
                            d2,
                            dir,
                            &ArrowheadStyle::Triangle,
                            &dim_outline.color,
                            stroke_w.max(0.35),
                            obj.opacity,
                            obj.blend_mode,
                            obj_to_screen,
                            anti_alias,
                            backend,
                        );
                    }
                }
                d1.midpoint(d2) + ext_dir * 1.2
            }
            DimensionKind::Angular => {
                let v = dim.p1;
                let r1 = (dim.p2 - v).normalized().unwrap_or(Vec2D::new(1.0, 0.0));
                let r2 = dim
                    .p3
                    .map_or(Vec2D::new(1.0, 0.0), |p| (p - v).normalize_or_zero());
                let rad = dim.offset_mm.abs().max(5.0);
                let ray1 = BezierPath::from_line(v, v + r1 * (rad + 2.0));
                let ray2 = BezierPath::from_line(v, v + r2 * (rad + 2.0));

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
                let arc_path = BezierPath::from_polygon(arc_pts, false);

                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(&ray1, obj_to_screen, px_per_mm, options, backend);
                    stroke_wireframe(&ray2, obj_to_screen, px_per_mm, options, backend);
                    stroke_wireframe(&arc_path, obj_to_screen, px_per_mm, options, backend);
                } else {
                    backend.stroke_path(&ray1, &stroke, obj_to_screen);
                    backend.stroke_path(&ray2, &stroke, obj_to_screen);
                    backend.stroke_path(&arc_path, &stroke, obj_to_screen);
                }
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
                let leader = BezierPath::from_polygon(pts, false);
                if view_mode == ViewMode::Wireframe {
                    stroke_wireframe(&leader, obj_to_screen, px_per_mm, options, backend);
                } else {
                    backend.stroke_path(&leader, &stroke, obj_to_screen);
                }
                anchor
            }
        };

        // Draw formatted measurement label
        if !label_text.trim().is_empty() {
            let at = ArtisticText::from_plain(label_text, dim.text_style.clone());
            let label_xf = obj_to_screen.compose(&Affine2D::translate(
                label_anchor.x,
                label_anchor.y - dim.text_style.font_size_mm(),
            ));
            self.text_engine.render_artistic_text(
                backend,
                &at,
                label_xf,
                &OutlineStyle::none(),
                obj.opacity,
                obj.blend_mode,
                view_mode,
                px_per_mm,
            );
        }
    }

    /// Renders all 11 [`Fill`] variants onto `backend`, reusing `self` for `VectorPattern`
    /// symbol tile rasterization with recursion depth tracking.
    #[allow(clippy::too_many_arguments)]
    fn draw_fill_styled(
        &mut self,
        doc: &Document,
        local_path: &BezierPath,
        fill: &Fill,
        local_to_screen: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        anti_alias: bool,
        backend: &mut dyn CanvasBackend,
        depth: usize,
    ) {
        let op = sanitize_f32(opacity, 1.0).clamp(0.0, 1.0);
        if op <= 1e-6 || local_path.is_empty() {
            return;
        }

        match fill {
            Fill::None => {}
            Fill::Solid(color) => {
                let paint = BackendPaint {
                    shader: PaintShader::Solid(color.clone()),
                    opacity: op,
                    blend_mode,
                    anti_alias,
                };
                backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
            }
            Fill::LinearGradient { start, end, stops } => {
                let paint = BackendPaint {
                    shader: PaintShader::LinearGradient {
                        start: *start,
                        end: *end,
                        stops: stops.clone(),
                    },
                    opacity: op,
                    blend_mode,
                    anti_alias,
                };
                backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
            }
            Fill::RadialGradient {
                center,
                radius,
                focal,
                stops,
            } => {
                let paint = BackendPaint {
                    shader: PaintShader::RadialGradient {
                        focal: *focal,
                        center: *center,
                        radius: *radius,
                        stops: stops.clone(),
                    },
                    opacity: op,
                    blend_mode,
                    anti_alias,
                };
                backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
            }
            Fill::ConicalGradient {
                center,
                angle_deg,
                stops,
            } => {
                render_pixel_shader_fill(
                    local_path,
                    local_to_screen,
                    op,
                    blend_mode,
                    backend,
                    |local_pt| {
                        let dx = local_pt.x - center.x;
                        let dy = local_pt.y - center.y;
                        let deg = dy.atan2(dx).to_degrees();
                        let t = (deg - sanitize_f64(*angle_deg, 0.0)).rem_euclid(360.0) / 360.0;
                        Fill::sample_stops(stops, t as f32).to_rgba8()
                    },
                );
            }
            Fill::SquareGradient {
                center,
                size,
                angle_deg,
                stops,
            } => {
                let half = (sanitize_f64(*size, 20.0).abs() * 0.5).max(1e-4);
                let rot = -sanitize_f64(*angle_deg, 0.0).to_radians();
                let (s, c) = rot.sin_cos();
                render_pixel_shader_fill(
                    local_path,
                    local_to_screen,
                    op,
                    blend_mode,
                    backend,
                    |local_pt| {
                        let dx = local_pt.x - center.x;
                        let dy = local_pt.y - center.y;
                        let rx = dx * c - dy * s;
                        let ry = dx * s + dy * c;
                        let cheb = (rx.abs().max(ry.abs()) / half).clamp(0.0, 1.0);
                        Fill::sample_stops(stops, cheb as f32).to_rgba8()
                    },
                );
            }
            Fill::TwoColorPattern {
                fg,
                bg,
                pattern_name,
                tile_w,
                tile_h,
            } => {
                let tw = sanitize_f64(*tile_w, 10.0).max(0.5);
                let th = sanitize_f64(*tile_h, 10.0).max(0.5);
                let tile_px = 64u32;
                let tile = generate_two_color_pattern_tile(pattern_name, fg, bg, tile_px);
                let tile_xf = Affine2D::scale(tw / f64::from(tile_px), th / f64::from(tile_px));
                let paint = BackendPaint {
                    shader: PaintShader::PixmapPattern {
                        tile,
                        tile_transform: tile_xf,
                    },
                    opacity: op,
                    blend_mode,
                    anti_alias,
                };
                backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
            }
            Fill::VectorPattern {
                symbol_id,
                tile_w,
                tile_h,
            } => {
                let tw = sanitize_f64(*tile_w, 15.0).max(0.5);
                let th = sanitize_f64(*tile_h, 15.0).max(0.5);
                let tile_px = 64u32;
                if let Some(sym) = doc.symbols.get(symbol_id) {
                    let tile = self.rasterize_symbol_tile(doc, sym, tw, th, tile_px, depth + 1);
                    let tile_xf = Affine2D::scale(tw / f64::from(tile_px), th / f64::from(tile_px));
                    let paint = BackendPaint {
                        shader: PaintShader::PixmapPattern {
                            tile,
                            tile_transform: tile_xf,
                        },
                        opacity: op,
                        blend_mode,
                        anti_alias,
                    };
                    backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
                }
            }
            Fill::BitmapPattern {
                image_id,
                tile_w,
                tile_h,
            } => {
                if let Some(asset) = doc.assets.get(image_id) {
                    if let Ok(tile) =
                        RgbaBuffer::from_vec(asset.width, asset.height, asset.rgba_bytes.clone())
                    {
                        let tw = sanitize_f64(*tile_w, 20.0).max(0.5);
                        let th = sanitize_f64(*tile_h, 20.0).max(0.5);
                        let tile_xf = Affine2D::scale(
                            tw / f64::from(asset.width.max(1)),
                            th / f64::from(asset.height.max(1)),
                        );
                        let paint = BackendPaint {
                            shader: PaintShader::PixmapPattern {
                                tile,
                                tile_transform: tile_xf,
                            },
                            opacity: op,
                            blend_mode,
                            anti_alias,
                        };
                        backend.fill_path(local_path, &paint, FillRule::Winding, local_to_screen);
                    }
                }
            }
            Fill::TextureFill {
                preset,
                seed,
                fg,
                bg,
            } => {
                let fg_rgba = fg.to_rgba_f32();
                let bg_rgba = bg.to_rgba_f32();
                let preset_lc = preset.to_ascii_lowercase();
                let seed_val = *seed;
                render_pixel_shader_fill(
                    local_path,
                    local_to_screen,
                    op,
                    blend_mode,
                    backend,
                    |local_pt| {
                        let n =
                            eval_procedural_texture(&preset_lc, seed_val, local_pt.x, local_pt.y);
                        [
                            ((bg_rgba[0] + (fg_rgba[0] - bg_rgba[0]) * n) * 255.0).round() as u8,
                            ((bg_rgba[1] + (fg_rgba[1] - bg_rgba[1]) * n) * 255.0).round() as u8,
                            ((bg_rgba[2] + (fg_rgba[2] - bg_rgba[2]) * n) * 255.0).round() as u8,
                            ((bg_rgba[3] + (fg_rgba[3] - bg_rgba[3]) * n) * 255.0).round() as u8,
                        ]
                    },
                );
            }
            Fill::MeshFill { rows, cols, nodes } => {
                render_mesh_fill(
                    local_path,
                    *rows,
                    *cols,
                    nodes,
                    local_to_screen,
                    op,
                    blend_mode,
                    backend,
                );
            }
        }
    }

    fn rasterize_symbol_tile(
        &mut self,
        doc: &Document,
        sym: &SymbolDef,
        tile_w: f64,
        tile_h: f64,
        tile_px: u32,
        depth: usize,
    ) -> RgbaBuffer {
        if depth > MAX_RENDER_DEPTH {
            return RgbaBuffer::new_transparent(tile_px, tile_px);
        }
        let Ok(mut sub) = TinySkiaBackend::new(tile_px, tile_px) else {
            return RgbaBuffer::new_transparent(tile_px, tile_px);
        };
        sub.clear(Color::TRANSPARENT);
        let scale = Affine2D::scale(
            f64::from(tile_px) / tile_w.max(0.1),
            f64::from(tile_px) / tile_h.max(0.1),
        );
        let opts = RenderOptions::export();
        for obj in &sym.objects {
            self.render_scene_object(
                doc,
                None,
                obj,
                scale,
                scale,
                ViewMode::Enhanced,
                &opts,
                &mut sub,
                &[],
                depth + 1,
            );
        }
        sub.into_rgba()
    }
}

/// Evaluates an arbitrary per-pixel local-space shader clipped cleanly to `local_path`.
fn render_pixel_shader_fill<F>(
    local_path: &BezierPath,
    local_to_screen: Affine2D,
    opacity: f32,
    blend_mode: BlendMode,
    backend: &mut dyn CanvasBackend,
    shader: F,
) where
    F: Fn(Point2D) -> [u8; 4],
{
    let Some(lb) = local_path.bounding_box() else {
        return;
    };
    let Some(screen_to_local) = local_to_screen.inverse() else {
        return;
    };
    let sb = local_to_screen.transform_rect(lb);
    let Some(clamped) = PixelRect::from_f64_rect_clamped(sb, 0, backend.width(), backend.height())
    else {
        return;
    };

    let mut buf = RgbaBuffer::new_transparent(clamped.width, clamped.height);
    for ly in 0..clamped.height {
        let sy = f64::from(clamped.y) + f64::from(ly) + 0.5;
        for lx in 0..clamped.width {
            let sx = f64::from(clamped.x) + f64::from(lx) + 0.5;
            let lp = screen_to_local.transform_point(Point2D::new(sx, sy));
            buf.set_pixel(lx, ly, shader(lp));
        }
    }

    backend.push_clip(local_path, FillRule::Winding, local_to_screen);
    backend.composite_region(clamped, &buf, opacity, blend_mode);
    backend.pop_clip();
}

/// True Coons-patch / bilinear-bicubic subdivided MeshFill rasterizer!
///
/// Builds a `(rows x cols)` grid of control nodes, subdivides each quad patch into
/// fine smooth micro-triangles with bilinear/smoothstep color interpolation across all
/// 4 corner colors, rasterizes into an `RgbaBuffer`, and composites clipped to `local_path`.
#[allow(clippy::too_many_arguments)]
fn render_mesh_fill(
    local_path: &BezierPath,
    rows: usize,
    cols: usize,
    nodes: &[MeshNode],
    local_to_screen: Affine2D,
    opacity: f32,
    blend_mode: BlendMode,
    backend: &mut dyn CanvasBackend,
) {
    let Some(lb) = local_path.bounding_box() else {
        return;
    };
    let sb = local_to_screen.transform_rect(lb);
    let Some(clamped) = PixelRect::from_f64_rect_clamped(sb, 0, backend.width(), backend.height())
    else {
        return;
    };

    let r_cnt = rows.clamp(2, 32);
    let c_cnt = cols.clamp(2, 32);

    // Build complete (r_cnt x c_cnt) lookup grid with automatic fallback placement if sparse
    let mut grid_pos = vec![vec![Point2D::ZERO; c_cnt]; r_cnt];
    let mut grid_col = vec![vec![[1.0_f32, 1.0, 1.0, 1.0]; c_cnt]; r_cnt];
    for (r, pos_row) in grid_pos.iter_mut().enumerate() {
        let v = r as f64 / (r_cnt - 1) as f64;
        for (c, slot) in pos_row.iter_mut().enumerate() {
            let u = c as f64 / (c_cnt - 1) as f64;
            *slot = Point2D::new(lb.min_x + u * lb.width(), lb.min_y + v * lb.height());
        }
    }
    for node in nodes {
        if node.row < r_cnt && node.col < c_cnt {
            if node.pos.is_finite() {
                grid_pos[node.row][node.col] = node.pos;
            }
            grid_col[node.row][node.col] = node.color.to_rgba_f32();
        }
    }

    let mut buf = RgbaBuffer::new_transparent(clamped.width, clamped.height);
    let sub_divs = 8usize; // 8x8 bicubic/Coons subdivisions per mesh cell

    // Project all mesh control points into local buffer pixel space
    let to_buf_px = |p: Point2D| -> Point2D {
        let s = local_to_screen.transform_point(p);
        Point2D::new(s.x - f64::from(clamped.x), s.y - f64::from(clamped.y))
    };

    for r in 0..(r_cnt - 1) {
        for c in 0..(c_cnt - 1) {
            let p00 = to_buf_px(grid_pos[r][c]);
            let p10 = to_buf_px(grid_pos[r][c + 1]);
            let p01 = to_buf_px(grid_pos[r + 1][c]);
            let p11 = to_buf_px(grid_pos[r + 1][c + 1]);

            let c00 = grid_col[r][c];
            let c10 = grid_col[r][c + 1];
            let c01 = grid_col[r + 1][c];
            let c11 = grid_col[r + 1][c + 1];

            let eval_pos = |u: f64, v: f64| -> Point2D {
                let top = p00.lerp(p10, u);
                let bot = p01.lerp(p11, u);
                top.lerp(bot, v)
            };
            // Smooth cubic Hermite (bicubic smoothstep) color interpolation for silky mesh blends
            let eval_col = |u: f64, v: f64| -> [f32; 4] {
                let su = (u * u * (3.0 - 2.0 * u)) as f32;
                let sv = (v * v * (3.0 - 2.0 * v)) as f32;
                let mut out = [0.0_f32; 4];
                for ch in 0..4 {
                    let top = c00[ch] + (c10[ch] - c00[ch]) * su;
                    let bot = c01[ch] + (c11[ch] - c01[ch]) * su;
                    out[ch] = (top + (bot - top) * sv).clamp(0.0, 1.0);
                }
                out
            };

            for sv in 0..sub_divs {
                let v0 = sv as f64 / sub_divs as f64;
                let v1 = (sv + 1) as f64 / sub_divs as f64;
                for su in 0..sub_divs {
                    let u0 = su as f64 / sub_divs as f64;
                    let u1 = (su + 1) as f64 / sub_divs as f64;

                    let a = (eval_pos(u0, v0), eval_col(u0, v0));
                    let b = (eval_pos(u1, v0), eval_col(u1, v0));
                    let d = (eval_pos(u1, v1), eval_col(u1, v1));
                    let e = (eval_pos(u0, v1), eval_col(u0, v1));

                    rasterize_gouraud_triangle(&mut buf, a, b, d);
                    rasterize_gouraud_triangle(&mut buf, a, d, e);
                }
            }
        }
    }

    backend.push_clip(local_path, FillRule::Winding, local_to_screen);
    backend.composite_region(clamped, &buf, opacity, blend_mode);
    backend.pop_clip();
}

/// Rasterizes a Gouraud-shaded triangle with barycentric RGBA color interpolation.
fn rasterize_gouraud_triangle(
    buf: &mut RgbaBuffer,
    v0: (Point2D, [f32; 4]),
    v1: (Point2D, [f32; 4]),
    v2: (Point2D, [f32; 4]),
) {
    let (p0, c0) = v0;
    let (p1, c1) = v1;
    let (p2, c2) = v2;
    let area = (p1.x - p0.x) * (p2.y - p0.y) - (p2.x - p0.x) * (p1.y - p0.y);
    if area.abs() <= 1e-6 {
        return;
    }
    let inv_area = 1.0 / area;

    let bw = f64::from(buf.width);
    let bh = f64::from(buf.height);
    let min_x = p0.x.min(p1.x).min(p2.x).floor().clamp(0.0, bw) as u32;
    let max_x = p0.x.max(p1.x).max(p2.x).ceil().clamp(0.0, bw) as u32;
    let min_y = p0.y.min(p1.y).min(p2.y).floor().clamp(0.0, bh) as u32;
    let max_y = p0.y.max(p1.y).max(p2.y).ceil().clamp(0.0, bh) as u32;

    for y in min_y..max_y {
        let py = f64::from(y) + 0.5;
        for x in min_x..max_x {
            let px = f64::from(x) + 0.5;
            let w0 = ((p1.x - px) * (p2.y - py) - (p2.x - px) * (p1.y - py)) * inv_area;
            let w1 = ((p2.x - px) * (p0.y - py) - (p0.x - px) * (p2.y - py)) * inv_area;
            let w2 = 1.0 - w0 - w1;
            if w0 >= -1e-4 && w1 >= -1e-4 && w2 >= -1e-4 {
                let w0f = w0.clamp(0.0, 1.0) as f32;
                let w1f = w1.clamp(0.0, 1.0) as f32;
                let w2f = w2.clamp(0.0, 1.0) as f32;
                let r = ((c0[0] * w0f + c1[0] * w1f + c2[0] * w2f) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                let g = ((c0[1] * w0f + c1[1] * w1f + c2[1] * w2f) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                let b = ((c0[2] * w0f + c1[2] * w1f + c2[2] * w2f) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                let a = ((c0[3] * w0f + c1[3] * w1f + c2[3] * w2f) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                buf.set_pixel(x, y, [r, g, b, a]);
            }
        }
    }
}

/// Renders an [`OutlineStyle`] honoring `is_hairline`, `scale_with_object`, `dash_pattern`,
/// `calligraphy_stretch`/`calligraphy_angle_deg`, and `start_arrow`/`end_arrow` arrowheads!
#[allow(clippy::too_many_arguments)]
fn draw_outline_styled(
    local_path: &BezierPath,
    obj_xf: Affine2D,
    parent_to_screen: Affine2D,
    outline: &OutlineStyle,
    opacity: f32,
    blend_mode: BlendMode,
    px_per_mm: f64,
    anti_alias: bool,
    backend: &mut dyn CanvasBackend,
) {
    if !outline.is_visible() || opacity <= 1e-6 || local_path.is_empty() {
        return;
    }

    // Choose whether stroke width scales with object transform or stays constant in page mm
    let (draw_path, draw_xf, effective_scale) = if outline.scale_with_object {
        let composed = parent_to_screen.compose(&obj_xf);
        (local_path.clone(), composed, estimate_scale(&composed))
    } else {
        (local_path.transformed(&obj_xf), parent_to_screen, px_per_mm)
    };

    let width_mm = if outline.is_hairline {
        outline
            .effective_width_mm()
            .max(1.0 / effective_scale.max(1e-3))
    } else {
        outline.effective_width_mm()
    };

    // Calligraphic elliptical nib stroke when `calligraphy_stretch != 1.0`
    let stretch = sanitize_f64(outline.calligraphy_stretch, 1.0).clamp(0.05, 1.0);
    if (stretch - 1.0).abs() > 0.03 {
        draw_calligraphic_outline(
            &draw_path, draw_xf, outline, width_mm, stretch, opacity, blend_mode, anti_alias,
            backend,
        );
    } else {
        let params =
            StrokeParams::from_outline(outline, width_mm as f32, opacity, blend_mode, anti_alias);
        backend.stroke_path(&draw_path, &params, draw_xf);
    }

    // Start & end arrowheads on open subpaths
    if outline.start_arrow != ArrowheadStyle::None || outline.end_arrow != ArrowheadStyle::None {
        for sp in &draw_path.subpaths {
            if sp.closed || sp.nodes.len() < 2 {
                continue;
            }
            let single = BezierPath::from_subpath(sp.clone());
            let total_len = single.path_length_mm();
            if total_len <= 1e-6 {
                continue;
            }
            if outline.start_arrow != ArrowheadStyle::None {
                if let Some((pt, tangent)) = single.point_and_tangent_at(0.0) {
                    draw_arrowhead(
                        pt,
                        -tangent,
                        &outline.start_arrow,
                        &outline.color,
                        width_mm as f32,
                        opacity,
                        blend_mode,
                        draw_xf,
                        anti_alias,
                        backend,
                    );
                }
            }
            if outline.end_arrow != ArrowheadStyle::None {
                if let Some((pt, tangent)) = single.point_and_tangent_at(total_len) {
                    draw_arrowhead(
                        pt,
                        tangent,
                        &outline.end_arrow,
                        &outline.color,
                        width_mm as f32,
                        opacity,
                        blend_mode,
                        draw_xf,
                        anti_alias,
                        backend,
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_calligraphic_outline(
    path: &BezierPath,
    transform: Affine2D,
    outline: &OutlineStyle,
    width_mm: f64,
    stretch: f64,
    opacity: f32,
    blend_mode: BlendMode,
    anti_alias: bool,
    backend: &mut dyn CanvasBackend,
) {
    let angle = sanitize_f64(outline.calligraphy_angle_deg, 45.0).to_radians();
    let nib_major = Vec2D::from_angle(angle) * (0.5 * width_mm);
    let nib_minor = nib_major.perp() * stretch;
    let paint = BackendPaint {
        shader: PaintShader::Solid(outline.color.clone()),
        opacity,
        blend_mode,
        anti_alias,
    };

    for sp in &path.subpaths {
        let single = BezierPath::from_subpath(sp.clone());
        let len = single.path_length_mm();
        if len <= 1e-6 {
            continue;
        }
        let steps = ((len / 0.5).ceil() as usize).clamp(8, 256);
        let mut left = Vec::with_capacity(steps + 1);
        let mut right = Vec::with_capacity(steps + 1);
        for i in 0..=steps {
            let d = (i as f64 / steps as f64) * len;
            let Some((pt, tangent)) = single.point_and_tangent_at(d) else {
                continue;
            };
            let n = tangent.perp();
            let proj = (nib_major.dot(n).hypot(nib_minor.dot(n))).max(width_mm * 0.08);
            left.push(pt + n * proj);
            right.push(pt - n * proj);
        }
        if left.len() >= 2 {
            right.reverse();
            left.extend(right);
            let ribbon = BezierPath::from_polygon(left, true);
            backend.fill_path(&ribbon, &paint, FillRule::Winding, transform);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_arrowhead(
    tip: Point2D,
    forward_dir: Vec2D,
    style: &ArrowheadStyle,
    color: &Color,
    stroke_w_mm: f32,
    opacity: f32,
    blend_mode: BlendMode,
    transform: Affine2D,
    anti_alias: bool,
    backend: &mut dyn CanvasBackend,
) {
    if *style == ArrowheadStyle::None {
        return;
    }
    let dir = forward_dir.normalized().unwrap_or(Vec2D::new(1.0, 0.0));
    let perp = dir.perp();
    let size = (f64::from(stroke_w_mm) * 4.5).max(1.5);
    let half_w = size * 0.45;

    let paint = BackendPaint {
        shader: PaintShader::Solid(color.clone()),
        opacity,
        blend_mode,
        anti_alias,
    };

    match style {
        ArrowheadStyle::None => {}
        ArrowheadStyle::Triangle | ArrowheadStyle::Custom(_) => {
            let base = tip - dir * size;
            let tri =
                BezierPath::from_polygon([tip, base + perp * half_w, base - perp * half_w], true);
            backend.fill_path(&tri, &paint, FillRule::Winding, transform);
        }
        ArrowheadStyle::Stealth => {
            let base = tip - dir * size;
            let notch = tip - dir * (size * 0.65);
            let poly = BezierPath::from_polygon(
                [tip, base + perp * half_w, notch, base - perp * half_w],
                true,
            );
            backend.fill_path(&poly, &paint, FillRule::Winding, transform);
        }
        ArrowheadStyle::OpenVee => {
            let base = tip - dir * size;
            let vee =
                BezierPath::from_polygon([base + perp * half_w, tip, base - perp * half_w], false);
            let st = StrokeParams {
                color: color.clone(),
                width: stroke_w_mm.max(0.2),
                cap: coree_core::LineCap::Round,
                join: coree_core::LineJoin::Miter,
                miter_limit: 4.0,
                dash_pattern: Vec::new(),
                dash_offset: 0.0,
                opacity,
                blend_mode,
                anti_alias,
            };
            backend.stroke_path(&vee, &st, transform);
        }
        ArrowheadStyle::Diamond => {
            let mid = tip - dir * (size * 0.5);
            let back = tip - dir * size;
            let dia = BezierPath::from_polygon(
                [tip, mid + perp * half_w, back, mid - perp * half_w],
                true,
            );
            backend.fill_path(&dia, &paint, FillRule::Winding, transform);
        }
        ArrowheadStyle::Circle => {
            let center = tip - dir * (half_w);
            let mut circle = coree_core::ParametricShape::Ellipse {
                rx: half_w,
                ry: half_w,
                mode: coree_core::EllipseMode::Full,
                start_deg: 0.0,
                end_deg: 360.0,
            }
            .to_bezier_path();
            circle.transform(&Affine2D::translate(center.x, center.y));
            backend.fill_path(&circle, &paint, FillRule::Winding, transform);
        }
        ArrowheadStyle::Square => {
            let back = tip - dir * (half_w * 2.0);
            let sq = BezierPath::from_polygon(
                [
                    tip + perp * half_w,
                    tip - perp * half_w,
                    back - perp * half_w,
                    back + perp * half_w,
                ],
                true,
            );
            backend.fill_path(&sq, &paint, FillRule::Winding, transform);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_raster_object(
    doc: &Document,
    obj: &SceneObject,
    raster: &coree_core::RasterLayerData,
    obj_to_screen: Affine2D,
    px_per_mm: f64,
    view_mode: ViewMode,
    options: &RenderOptions,
    backend: &mut dyn CanvasBackend,
) {
    let w_mm = sanitize_f64(raster.width_mm, 50.0).max(0.1);
    let h_mm = sanitize_f64(raster.height_mm, 50.0).max(0.1);
    let full_rect = Rect2D::from_xywh(0.0, 0.0, w_mm, h_mm);
    let clip_rect = raster
        .crop_rect
        .filter(|r| r.is_finite())
        .unwrap_or(full_rect);

    if view_mode == ViewMode::Wireframe {
        let mut frame = BezierPath::from_rect(clip_rect);
        // Diagonal X indicator for wireframe raster placeholders
        frame.subpaths.push(Subpath::from_points(
            [
                Point2D::new(clip_rect.min_x, clip_rect.min_y),
                Point2D::new(clip_rect.max_x, clip_rect.max_y),
            ],
            false,
        ));
        frame.subpaths.push(Subpath::from_points(
            [
                Point2D::new(clip_rect.max_x, clip_rect.min_y),
                Point2D::new(clip_rect.min_x, clip_rect.max_y),
            ],
            false,
        ));
        stroke_wireframe(&frame, obj_to_screen, px_per_mm, options, backend);
        return;
    }

    let Some(asset) = doc.assets.get(&raster.asset_id) else {
        return;
    };
    let filtered = if raster.filters.is_empty() {
        None
    } else {
        Some(apply_raster_filters(asset, &raster.filters))
    };
    let active_asset = filtered.as_ref().unwrap_or(asset);
    let Ok(buf) = RgbaBuffer::from_vec(
        active_asset.width,
        active_asset.height,
        active_asset.rgba_bytes.clone(),
    ) else {
        return;
    };

    let sx = w_mm / f64::from(active_asset.width.max(1));
    let sy = h_mm / f64::from(active_asset.height.max(1));
    let img_xf = obj_to_screen.compose(&Affine2D::scale(sx, sy));
    let smooth = view_mode != ViewMode::Pixels;

    if raster.crop_rect.is_some() {
        let crop_path = BezierPath::from_rect(clip_rect);
        backend.push_clip(&crop_path, FillRule::Winding, obj_to_screen);
        backend.draw_pixmap(&buf, img_xf, obj.opacity, obj.blend_mode, smooth);
        backend.pop_clip();
    } else {
        backend.draw_pixmap(&buf, img_xf, obj.opacity, obj.blend_mode, smooth);
    }
}

pub(crate) fn render_page_surface(
    page: &Page,
    page_to_surface: Affine2D,
    px_per_mm: f64,
    options: &RenderOptions,
    backend: &mut dyn CanvasBackend,
) {
    let page_rect = page.trim_rect();
    let page_path = BezierPath::from_rect(page_rect);

    if options.show_page_shadow {
        let shadow_off = (3.5 / px_per_mm.max(0.2)).clamp(1.0, 5.0);
        let shadow_rect =
            BezierPath::from_rect(page_rect.translate(Vec2D::new(shadow_off, shadow_off)));
        let shadow_paint = BackendPaint::solid(Color::rgba8(40, 45, 55, 65));
        backend.fill_path(
            &shadow_rect,
            &shadow_paint,
            FillRule::Winding,
            page_to_surface,
        );
    }

    if options.page_bg.alpha() > 1e-6 {
        let page_paint = BackendPaint::solid(options.page_bg.clone());
        backend.fill_path(&page_path, &page_paint, FillRule::Winding, page_to_surface);
    }

    if options.show_page_shadow {
        let border = StrokeParams {
            color: Color::rgba8(140, 145, 155, 255),
            width: (1.0 / px_per_mm.max(1e-3)) as f32,
            cap: coree_core::LineCap::Butt,
            join: coree_core::LineJoin::Miter,
            miter_limit: 4.0,
            dash_pattern: Vec::new(),
            dash_offset: 0.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: false,
        };
        backend.stroke_path(&page_path, &border, page_to_surface);
    }
}

fn render_page_overlays(
    doc: &Document,
    page: &Page,
    page_to_surface: Affine2D,
    px_per_mm: f64,
    options: &RenderOptions,
    backend: &mut dyn CanvasBackend,
) {
    let hairline_mm = (1.0 / px_per_mm.max(1e-3)) as f32;

    // 1. Document Grid
    if options.show_grid && doc.grid.visible {
        let sx = sanitize_f64(doc.grid.spacing_x_mm, 10.0).max(0.5);
        let sy = sanitize_f64(doc.grid.spacing_y_mm, 10.0).max(0.5);
        let subs = doc.grid.subdivisions.clamp(1, 10);
        let step_x = sx / f64::from(subs);
        let step_y = sy / f64::from(subs);

        let grid_stroke = StrokeParams {
            color: Color::rgba8(180, 195, 220, 120),
            width: hairline_mm,
            cap: coree_core::LineCap::Butt,
            join: coree_core::LineJoin::Miter,
            miter_limit: 4.0,
            dash_pattern: Vec::new(),
            dash_offset: 0.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: false,
        };
        if doc.grid.show_dots {
            let dot_paint = BackendPaint::solid(Color::rgba8(130, 150, 185, 180));
            let r = f64::from(hairline_mm) * 0.8;
            let mut y = 0.0;
            let mut guard_y = 0usize;
            while y <= page.height_mm + 1e-6 && guard_y < 400 {
                let mut x = 0.0;
                let mut guard_x = 0usize;
                while x <= page.width_mm + 1e-6 && guard_x < 400 {
                    let dot = BezierPath::from_rect(Rect2D::from_center_size(
                        Point2D::new(x, y),
                        r * 2.0,
                        r * 2.0,
                    ));
                    backend.fill_path(&dot, &dot_paint, FillRule::Winding, page_to_surface);
                    x += step_x;
                    guard_x += 1;
                }
                y += step_y;
                guard_y += 1;
            }
        } else {
            let mut subpaths = Vec::new();
            let mut x = 0.0;
            let mut gx = 0usize;
            while x <= page.width_mm + 1e-6 && gx < 500 {
                subpaths.push(Subpath::from_points(
                    [Point2D::new(x, 0.0), Point2D::new(x, page.height_mm)],
                    false,
                ));
                x += step_x;
                gx += 1;
            }
            let mut y = 0.0;
            let mut gy = 0usize;
            while y <= page.height_mm + 1e-6 && gy < 500 {
                subpaths.push(Subpath::from_points(
                    [Point2D::new(0.0, y), Point2D::new(page.width_mm, y)],
                    false,
                ));
                y += step_y;
                gy += 1;
            }
            backend.stroke_path(&BezierPath { subpaths }, &grid_stroke, page_to_surface);
        }
    }

    // 2. Bleed & Margin Guides
    if options.show_bleed_guides && page.bleed_mm > 1e-4 {
        let bleed_path = BezierPath::from_rect(page.bleed_rect());
        let st = StrokeParams {
            color: Color::rgba8(220, 40, 90, 180),
            width: hairline_mm,
            cap: coree_core::LineCap::Butt,
            join: coree_core::LineJoin::Miter,
            miter_limit: 4.0,
            dash_pattern: vec![hairline_mm * 5.0, hairline_mm * 3.0],
            dash_offset: 0.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: false,
        };
        backend.stroke_path(&bleed_path, &st, page_to_surface);
    }

    if options.show_margin_guides && options.margin_mm > 1e-4 {
        let m = options.margin_mm;
        if page.width_mm > 2.0 * m && page.height_mm > 2.0 * m {
            let margin_path = BezierPath::from_rect(Rect2D::from_xywh(
                m,
                m,
                page.width_mm - 2.0 * m,
                page.height_mm - 2.0 * m,
            ));
            let st = StrokeParams {
                color: Color::rgba8(60, 140, 230, 160),
                width: hairline_mm,
                cap: coree_core::LineCap::Butt,
                join: coree_core::LineJoin::Miter,
                miter_limit: 4.0,
                dash_pattern: vec![hairline_mm * 4.0, hairline_mm * 4.0],
                dash_offset: 0.0,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                anti_alias: false,
            };
            backend.stroke_path(&margin_path, &st, page_to_surface);
        }
    }

    // 3. Perspective Grid
    if options.show_perspective_grid && doc.perspective_grid.enabled {
        render_perspective_grid(
            &doc.perspective_grid,
            page,
            page_to_surface,
            hairline_mm,
            backend,
        );
    }

    // 4. Guidelines
    if options.show_guidelines && !doc.guides.is_empty() {
        let guide_stroke = StrokeParams {
            color: Color::rgba8(0, 165, 225, 200),
            width: hairline_mm,
            cap: coree_core::LineCap::Butt,
            join: coree_core::LineJoin::Miter,
            miter_limit: 4.0,
            dash_pattern: vec![hairline_mm * 6.0, hairline_mm * 3.0],
            dash_offset: 0.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: false,
        };
        let extent = page.width_mm.max(page.height_mm) * 4.0;
        let mut subpaths = Vec::with_capacity(doc.guides.len());
        for guide in &doc.guides {
            match guide.orientation {
                GuideOrientation::Horizontal(y) => {
                    subpaths.push(Subpath::from_points(
                        [Point2D::new(-extent, y), Point2D::new(extent, y)],
                        false,
                    ));
                }
                GuideOrientation::Vertical(x) => {
                    subpaths.push(Subpath::from_points(
                        [Point2D::new(x, -extent), Point2D::new(x, extent)],
                        false,
                    ));
                }
                GuideOrientation::Angled { origin, angle_deg } => {
                    let dir = Vec2D::from_angle(angle_deg.to_radians()) * extent;
                    subpaths.push(Subpath::from_points([origin - dir, origin + dir], false));
                }
            }
        }
        backend.stroke_path(&BezierPath { subpaths }, &guide_stroke, page_to_surface);
    }
}

fn render_perspective_grid(
    pg: &coree_core::PerspectiveGridConfig,
    page: &Page,
    page_to_surface: Affine2D,
    hairline_mm: f32,
    backend: &mut dyn CanvasBackend,
) {
    let op = sanitize_f32(pg.opacity, 0.35).clamp(0.05, 1.0);
    let st = StrokeParams {
        color: Color::rgba(0.2, 0.45, 0.85, op),
        width: hairline_mm,
        cap: coree_core::LineCap::Butt,
        join: coree_core::LineJoin::Miter,
        miter_limit: 4.0,
        dash_pattern: Vec::new(),
        dash_offset: 0.0,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        anti_alias: true,
    };
    let density = pg.density.clamp(4, 60);
    let mut subpaths = Vec::new();
    let max_dim = page.width_mm.max(page.height_mm) * 2.0;

    match pg.mode {
        PerspectiveGridMode::Isometric => {
            let step = (page.width_mm / f64::from(density)).max(5.0);
            for deg in [30.0_f64, 150.0, 90.0] {
                let dir = Vec2D::from_angle(deg.to_radians());
                let perp = dir.perp();
                let count = (max_dim / step) as i32;
                for i in -count..=count {
                    let anchor = Point2D::new(page.width_mm * 0.5, page.height_mm * 0.5)
                        + perp * (f64::from(i) * step);
                    subpaths.push(Subpath::from_points(
                        [anchor - dir * max_dim, anchor + dir * max_dim],
                        false,
                    ));
                }
            }
        }
        PerspectiveGridMode::OnePoint
        | PerspectiveGridMode::TwoPoint
        | PerspectiveGridMode::ThreePoint => {
            // Horizon line
            subpaths.push(Subpath::from_points(
                [
                    Point2D::new(-page.width_mm, pg.horizon_y_mm),
                    Point2D::new(page.width_mm * 2.0, pg.horizon_y_mm),
                ],
                false,
            ));
            // Radiating rays from every vanishing point
            for &vp in &pg.vanishing_points {
                if !vp.is_finite() {
                    continue;
                }
                for i in 0..density {
                    let angle = (f64::from(i) / f64::from(density)) * std::f64::consts::TAU;
                    let end = vp + Vec2D::from_angle(angle) * max_dim;
                    subpaths.push(Subpath::from_points([vp, end], false));
                }
            }
        }
    }

    let clip = BezierPath::from_rect(page.trim_rect());
    backend.push_clip(&clip, FillRule::Winding, page_to_surface);
    backend.stroke_path(&BezierPath { subpaths }, &st, page_to_surface);
    backend.pop_clip();
}

fn stroke_wireframe(
    path: &BezierPath,
    transform: Affine2D,
    px_per_mm: f64,
    options: &RenderOptions,
    backend: &mut dyn CanvasBackend,
) {
    let wire = StrokeParams {
        color: options.wireframe_stroke_color.clone(),
        width: (1.0 / px_per_mm.max(1e-3)) as f32,
        cap: coree_core::LineCap::Butt,
        join: coree_core::LineJoin::Miter,
        miter_limit: 4.0,
        dash_pattern: Vec::new(),
        dash_offset: 0.0,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        anti_alias: true,
    };
    backend.stroke_path(path, &wire, transform);
}

/// Collects all layers visible on `page_index`, including applicable master layers across the document.
pub(crate) fn collect_effective_layers(doc: &Document, page_index: usize) -> Vec<&Layer> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let is_odd = page_index.is_multiple_of(2); // 1-based Page 1 = index 0 = Odd

    for page in &doc.pages {
        for layer in &page.layers {
            let applies = match layer.master_scope {
                MasterLayerScope::None => false,
                MasterLayerScope::AllPages => true,
                MasterLayerScope::OddPages => is_odd,
                MasterLayerScope::EvenPages => !is_odd,
            };
            if applies && seen.insert(layer.id) {
                out.push(layer);
            }
        }
    }
    if let Some(page) = doc.pages.get(page_index) {
        for layer in &page.layers {
            if layer.master_scope == MasterLayerScope::None && seen.insert(layer.id) {
                out.push(layer);
            }
        }
    }
    out
}

fn compute_page_max_effect_padding(doc: &Document, layers: &[&Layer]) -> f64 {
    let mut max_pad = 0.0_f64;
    for layer in layers {
        if !layer.visible {
            continue;
        }
        for &oid in &layer.objects {
            if let Some(obj) = doc.objects.get(&oid) {
                max_pad = max_pad.max(effect_bounds_padding_mm(&obj.effects));
                if obj.outline.is_visible() {
                    max_pad = max_pad.max(obj.outline.effective_width_mm() * 4.0);
                }
            }
        }
    }
    max_pad
}

fn estimate_scale(xf: &Affine2D) -> f64 {
    let sx = xf.m[0].hypot(xf.m[1]);
    let sy = xf.m[2].hypot(xf.m[3]);
    (0.5 * (sx + sy)).max(1e-4)
}

fn generate_two_color_pattern_tile(name: &str, fg: &Color, bg: &Color, size: u32) -> RgbaBuffer {
    let fg_px = fg.to_rgba8();
    let bg_px = bg.to_rgba8();
    let mut buf = RgbaBuffer::new_filled(size, size, bg_px);
    let kind = name.to_ascii_lowercase();
    let half = size / 2;

    for y in 0..size {
        for x in 0..size {
            let is_fg = if kind.contains("stripe") || kind.contains("line") {
                y % (size / 4).max(2) < (size / 8).max(1)
            } else if kind.contains("dot") || kind.contains("polka") {
                let dx = x as f64 - f64::from(half);
                let dy = y as f64 - f64::from(half);
                dx.hypot(dy) <= f64::from(size) * 0.25
            } else if kind.contains("cross") || kind.contains("hatch") || kind.contains("grid") {
                x < 4 || y < 4
            } else if kind.contains("diag") {
                ((x + y) % (size / 2).max(2)) < (size / 6).max(1)
            } else if kind.contains("brick") {
                let row_h = (size / 2).max(2);
                let offset = if (y / row_h) % 2 == 1 { half } else { 0 };
                y % row_h < 3 || ((x + offset) % size) < 3
            } else {
                // Default checkerboard
                (x < half) ^ (y < half)
            };
            if is_fg {
                buf.set_pixel(x, y, fg_px);
            }
        }
    }
    buf
}

/// Deterministic 2D value-noise FBM procedural texture evaluator returning `0.0..=1.0`.
fn eval_procedural_texture(preset: &str, seed: u64, x: f64, y: f64) -> f32 {
    let fx = x * 0.15;
    let fy = y * 0.15;
    let n = fbm2d(fx, fy, seed, 4);
    if preset.contains("marble") {
        (0.5 + 0.5 * (fx * 2.0 + n * 6.0).sin()).clamp(0.0, 1.0) as f32
    } else if preset.contains("wood") {
        let rings = ((fx * 0.6).hypot(fy * 0.6) * 4.0 + n * 2.5).fract();
        rings.clamp(0.0, 1.0) as f32
    } else if preset.contains("wave") {
        (0.5 + 0.5 * (fx * 1.5 + (fy * 1.5).cos() + n * 2.0).sin()).clamp(0.0, 1.0) as f32
    } else if preset.contains("granite") {
        fbm2d(x * 0.45, y * 0.45, seed, 3).clamp(0.0, 1.0) as f32
    } else {
        // Clouds / default smooth FBM
        n.clamp(0.0, 1.0) as f32
    }
}

fn fbm2d(x: f64, y: f64, seed: u64, octaves: u32) -> f64 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut freq = 1.0;
    let mut norm = 0.0;
    for o in 0..octaves {
        sum += amp * value_noise_2d(x * freq, y * freq, seed.wrapping_add(u64::from(o) * 1013));
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    if norm > 0.0 {
        sum / norm
    } else {
        0.5
    }
}

fn value_noise_2d(x: f64, y: f64, seed: u64) -> f64 {
    let ix = x.floor() as i64;
    let iy = y.floor() as i64;
    let fx = x - ix as f64;
    let fy = y - iy as f64;
    let ux = fx * fx * (3.0 - 2.0 * fx);
    let uy = fy * fy * (3.0 - 2.0 * fy);

    let v00 = hash_grid(ix, iy, seed);
    let v10 = hash_grid(ix + 1, iy, seed);
    let v01 = hash_grid(ix, iy + 1, seed);
    let v11 = hash_grid(ix + 1, iy + 1, seed);

    let top = v00 + (v10 - v00) * ux;
    let bot = v01 + (v11 - v01) * ux;
    top + (bot - top) * uy
}

fn hash_grid(ix: i64, iy: i64, seed: u64) -> f64 {
    let mut z = (ix as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (iy as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9)
        ^ seed.wrapping_mul(0x94d0_49bb_1331_11eb);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z as f64) / (u64::MAX as f64)
}
