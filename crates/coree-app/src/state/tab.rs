// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

// =============================================================================
// Multi-Document Tab Model
// =============================================================================

/// Independent document session tab holding scene graph, history, viewport, and render cache.
pub struct DocumentTab {
    pub id: Uuid,
    pub document: Document,
    pub history: CommandHistory,
    pub selection: SelectionState,
    pub viewport: Viewport,
    pub view_mode: ViewMode,
    pub render_options: RenderOptions,
    pub tile_cache: DirtyTileCache,
    pub renderer: SceneRenderer,
    pub snap_options: SnapOptions,
    pub active_guides: Vec<DynamicGuideLine>,
    pub selected_guide_id: Option<u64>,
    pub dirty: bool,
    pub source_path: Option<PathBuf>,
    pub last_autosave_at: Instant,
    pub journal_seq: u64,
    pub wal_session_open: bool,
    pub last_rendered_texture: Option<egui::TextureHandle>,
    pub needs_rerender: bool,
    pub viewport_initialized: bool,
    pub drag_state: CanvasDragState,
    pub poly_points_mm: Vec<Point2D>,
    pub bezier_draft_nodes: Vec<PathNode>,
    pub three_point_curve_chord: Option<(Point2D, Point2D)>,
    pub three_point_shape_chord: Option<(ToolId, Point2D, Point2D)>,
    pub pending_crop_rect: Option<Rect2D>,
    pub inline_text_edit: Option<InlineTextEditState>,
    pub raster_mask_rect_mm: Option<Rect2D>,
    pub selected_mesh_node: Option<(ObjectId, usize)>,
    pub live_sketch_strokes: Vec<Vec<Point2D>>,
    pub cursor_world_mm: Point2D,
    pub inspection_report: Option<FileInspectionReport>,
    pub overprint_preview: bool,
    pub separations_preview: Option<SeparationResult>,
    pub active_separation_channel: Option<usize>,
    pub powerclip_edit_id: Option<ObjectId>,
    pub last_metrics: ProductionMetrics,
    pub metrics_dirty: bool,
    pub preflight_report: Option<PreflightReport>,
    pub nudge_mm: f64,
    pub super_nudge_factor: f64,
    pub micro_nudge_divisor: f64,
    pub duplicate_offset_mm: Vec2D,
    /// Smart step-and-repeat state: `(created_ids, cumulative_delta_xf)` for `Ctrl+D`.
    pub last_duplicate_batch: Option<(Vec<ObjectId>, Affine2D)>,
    pub lock_aspect: bool,
    pub inline_renaming_page: Option<usize>,
    pub inline_page_name_buf: String,
    pub active_layer_id: Option<LayerId>,
}

impl DocumentTab {
    /// Creates a new interactive tab wrapping `document`.
    #[must_use]
    pub fn new(document: Document, source_path: Option<PathBuf>, theme: StudioTheme) -> Self {
        let mut render_options = RenderOptions::interactive();
        render_options.canvas_bg = theme.tokens().canvas_well_core_color();
        let active_layer_id = document
            .active_page()
            .and_then(|p| p.layers.last())
            .map(|l| l.id);
        let metrics = compute_production_metrics(&document, None);
        Self {
            id: document.id,
            document,
            history: CommandHistory::default(),
            selection: SelectionState::new(),
            viewport: Viewport::new(1024, 720),
            view_mode: ViewMode::Enhanced,
            render_options,
            tile_cache: DirtyTileCache::new(1024, 720),
            renderer: SceneRenderer::new(),
            snap_options: SnapOptions::default(),
            active_guides: Vec::new(),
            selected_guide_id: None,
            dirty: false,
            source_path,
            last_autosave_at: Instant::now(),
            journal_seq: 0,
            wal_session_open: false,
            last_rendered_texture: None,
            needs_rerender: true,
            viewport_initialized: false,
            drag_state: CanvasDragState::Idle,
            poly_points_mm: Vec::new(),
            bezier_draft_nodes: Vec::new(),
            three_point_curve_chord: None,
            three_point_shape_chord: None,
            pending_crop_rect: None,
            inline_text_edit: None,
            raster_mask_rect_mm: None,
            selected_mesh_node: None,
            live_sketch_strokes: Vec::new(),
            cursor_world_mm: Point2D::ZERO,
            inspection_report: None,
            overprint_preview: false,
            separations_preview: None,
            active_separation_channel: None,
            powerclip_edit_id: None,
            last_metrics: metrics,
            metrics_dirty: false,
            preflight_report: None,
            nudge_mm: 1.0,
            super_nudge_factor: 2.0,
            micro_nudge_divisor: 2.0,
            duplicate_offset_mm: Vec2D::new(5.0, 5.0),
            last_duplicate_batch: None,
            lock_aspect: true,
            inline_renaming_page: None,
            inline_page_name_buf: String::new(),
            active_layer_id,
        }
    }

    /// Ensures `active_page_index` and `active_layer_id` always reference an existing page
    /// and a valid layer belonging to that active page.
    pub fn sync_active_layer(&mut self) {
        if self.document.pages.is_empty() {
            self.active_layer_id = None;
            return;
        }
        if self.document.active_page_index >= self.document.pages.len() {
            self.document.active_page_index = self.document.pages.len() - 1;
        }
        let Some(page) = self.document.active_page() else {
            self.active_layer_id = None;
            return;
        };
        let still_valid = self
            .active_layer_id
            .is_some_and(|lid| page.layers.iter().any(|l| l.id == lid));
        if !still_valid {
            self.active_layer_id = page
                .layers
                .iter()
                .rev()
                .find(|l| !l.locked)
                .or_else(|| page.layers.last())
                .map(|l| l.id);
        }
    }

    /// Cancels any active interactive drag gesture and cleanly rolls back live preview mutations
    /// so `Esc` never leaves uncommitted transforms, paths, fills, or effects behind.
    pub fn cancel_active_drag(&mut self) {
        let state = std::mem::take(&mut self.drag_state);
        match state {
            CanvasDragState::MovingSelection {
                accum_dx, accum_dy, ..
            } => {
                if accum_dx.abs() > 1e-6 || accum_dy.abs() > 1e-6 {
                    let inv = Affine2D::translate(-accum_dx, -accum_dy);
                    let editable: Vec<ObjectId> = self
                        .selection
                        .objects
                        .iter()
                        .copied()
                        .filter(|&id| is_object_editable_in_doc(&self.document, id))
                        .collect();
                    for id in editable {
                        if let Some(obj) = self.document.objects.get_mut(&id) {
                            obj.transform = inv.compose(&obj.transform);
                        }
                    }
                    self.tile_cache.mark_all_dirty();
                    self.needs_rerender = true;
                }
            }
            CanvasDragState::ScalingSelection {
                orig_transforms, ..
            }
            | CanvasDragState::RotatingSelection {
                orig_transforms, ..
            }
            | CanvasDragState::SkewingSelection {
                orig_transforms, ..
            }
            | CanvasDragState::FreeTransforming {
                orig_transforms, ..
            } => {
                for (id, orig_xf) in orig_transforms {
                    if let Some(obj) = self.document.objects.get_mut(&id) {
                        obj.transform = orig_xf;
                    }
                }
                self.tile_cache.mark_all_dirty();
                self.needs_rerender = true;
            }
            CanvasDragState::DraggingNode { orig_paths, .. } => {
                for (id, orig_path) in orig_paths {
                    if let Some(obj) = self.document.objects.get_mut(&id) {
                        obj.payload = NodePayload::Path(orig_path);
                    }
                }
                self.tile_cache.mark_all_dirty();
                self.needs_rerender = true;
            }
            CanvasDragState::DraggingCurveSegment {
                object_id,
                orig_path,
                ..
            } => {
                if let Some(obj) = self.document.objects.get_mut(&object_id) {
                    obj.payload = NodePayload::Path(orig_path);
                }
                self.tile_cache.mark_all_dirty();
                self.needs_rerender = true;
            }
            CanvasDragState::DraggingParametricHandle {
                object_id,
                orig_shape,
                ..
            } => {
                if let Some(obj) = self.document.objects.get_mut(&object_id) {
                    obj.payload = NodePayload::Shape(orig_shape);
                }
                self.tile_cache.mark_all_dirty();
                self.needs_rerender = true;
            }
            CanvasDragState::Sculpting {
                object_id,
                orig_payload,
                ..
            } => {
                if let Some(obj) = self.document.objects.get_mut(&object_id) {
                    obj.payload = orig_payload;
                }
                self.tile_cache.mark_all_dirty();
                self.needs_rerender = true;
            }
            CanvasDragState::DraggingGradientHandle {
                object_id,
                orig_fill,
                ..
            }
            | CanvasDragState::DraggingGradientFocal {
                object_id,
                orig_fill,
            }
            | CanvasDragState::DraggingGradientStop {
                object_id,
                orig_fill,
                ..
            }
            | CanvasDragState::DraggingMeshNode {
                object_id,
                orig_fill,
                ..
            } => {
                if let Some(obj) = self.document.objects.get_mut(&object_id) {
                    obj.fill = orig_fill;
                }
                self.tile_cache.mark_all_dirty();
                self.needs_rerender = true;
            }
            CanvasDragState::DraggingTransparencyHandle {
                object_id,
                effect_index,
                orig_effect,
                ..
            }
            | CanvasDragState::DraggingEnvelopeHandle {
                object_id,
                effect_index,
                orig_effect,
                ..
            }
            | CanvasDragState::DraggingEffectHandle {
                object_id,
                effect_index,
                orig_effect,
            } => {
                if let Some(obj) = self.document.objects.get_mut(&object_id) {
                    if let Some(slot) = obj.effects.get_mut(effect_index) {
                        *slot = orig_effect;
                    }
                }
                self.tile_cache.mark_all_dirty();
                self.needs_rerender = true;
            }
            CanvasDragState::DraggingCanvasGuide {
                guide_id,
                orig_orientation,
            } => {
                if let Some(g) = self.document.guides.iter_mut().find(|g| g.id == guide_id) {
                    g.orientation = orig_orientation;
                }
                self.needs_rerender = true;
            }
            _ => {}
        }
        self.viewport.lod_dragging = false;
        self.active_guides.clear();
    }

    /// Marks the canvas cache and live production metrics dirty after any scene edit.
    pub fn mark_modified(&mut self) {
        self.dirty = true;
        self.needs_rerender = true;
        self.metrics_dirty = true;
        self.separations_preview = None;
        self.tile_cache.mark_all_dirty();
        self.selection.prune_missing(&self.document);
        self.sync_active_layer();
    }

    /// Refreshes live path length & area metrics if dirty.
    pub fn ensure_metrics_fresh(&mut self) {
        if self.metrics_dirty {
            let sel = if self.selection.is_empty() {
                None
            } else {
                Some(self.selection.objects.as_slice())
            };
            self.last_metrics = compute_production_metrics(&self.document, sel);
            self.metrics_dirty = false;
        }
    }

    /// Tab label shown in the top document tab strip.
    #[must_use]
    pub fn tab_title(&self) -> String {
        if self.dirty {
            format!("{} *", self.document.title)
        } else {
            self.document.title.clone()
        }
    }

    /// Fits the active page cleanly inside the current viewport.
    pub fn fit_active_page(&mut self) {
        let layouts = compute_page_layouts(
            &self.document,
            self.document.multipage_view,
            self.render_options.multipage_gap_mm,
        );
        let rect = if self.document.multipage_view {
            let mut acc: Option<Rect2D> = None;
            for l in &layouts {
                let r = l.rect_mm();
                acc = Some(match acc {
                    Some(prev) => prev.union(r),
                    None => r,
                });
            }
            acc.unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0))
        } else {
            self.document.active_page().map_or_else(
                || Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0),
                |p| p.trim_rect(),
            )
        };
        self.viewport.fit_rect(rect, 36.0);
        self.needs_rerender = true;
    }

    /// Fits all visible objects on the active page (or the active page if empty).
    pub fn fit_all_objects(&mut self) {
        let all_ids: Vec<ObjectId> = self
            .document
            .active_page()
            .map(|p| {
                p.layers
                    .iter()
                    .filter(|l| l.visible)
                    .flat_map(|l| l.objects.iter().copied())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(bb) = self.document.combined_bounds(&all_ids) {
            self.viewport.fit_rect(bb.expand(5.0), 40.0);
            self.needs_rerender = true;
        } else {
            self.fit_active_page();
        }
    }
}
