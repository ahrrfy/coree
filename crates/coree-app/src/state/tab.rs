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
    pub wal_session_id: Option<String>,
    pub wal_base_snapshot_id: Option<String>,
    pub autosave_dirty_objects: HashSet<ObjectId>,
    pub autosave_dirty_structure: bool,
    pub autosave_dirty_all: bool,
    pub last_rendered_texture: Option<egui::TextureHandle>,
    pub needs_rerender: bool,
    pub viewport_initialized: bool,
    pub tool_core: ToolCore,
    pub transient_overlay: TransientOverlay,
    pub last_canvas_rect: Option<[f32; 4]>,
    pub pending_crop_rect: Option<Rect2D>,
    pub inline_text_edit: Option<InlineTextEditState>,
    pub raster_mask_rect_mm: Option<Rect2D>,
    pub selected_mesh_node: Option<(ObjectId, usize)>,
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
            id: document.id(),
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
            wal_session_id: None,
            wal_base_snapshot_id: None,
            autosave_dirty_objects: HashSet::new(),
            autosave_dirty_structure: false,
            autosave_dirty_all: false,
            last_rendered_texture: None,
            needs_rerender: true,
            viewport_initialized: false,
            tool_core: ToolCore::new(ToolId::Pick),
            transient_overlay: TransientOverlay::default(),
            last_canvas_rect: None,
            pending_crop_rect: None,
            inline_text_edit: None,
            raster_mask_rect_mm: None,
            selected_mesh_node: None,
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
        if self.document.pages().is_empty() {
            self.active_layer_id = None;
            return;
        }
        if self.document.active_page_index() >= self.document.pages().len() {
            let last = self.document.pages().len() - 1;
            let _ = self.document.set_active_page_index(last);
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

    /// Cancels any active interactive drag gesture and clears transient overlay previews.
    /// Since `ToolCore` never mutates `Document` mid-drag, zero rollback mutations or
    /// `mark_all_dirty()` invalidations are needed.
    pub fn cancel_active_drag(&mut self) {
        let _ = self.tool_core.cancel();
        self.transient_overlay = TransientOverlay::default();
        self.viewport.lod_dragging = false;
        self.active_guides.clear();
    }

    /// Marks the canvas cache dirty according to a targeted [`DocumentInvalidation`] from
    /// [`CommandHistory`], avoiding full-tile invalidation whenever only specific objects/bounds
    /// changed.
    pub fn mark_modified_with_invalidation(&mut self, inv: &DocumentInvalidation) {
        self.dirty = true;
        self.needs_rerender = true;
        self.metrics_dirty = true;
        self.separations_preview = None;
        match inv {
            DocumentInvalidation::All => {
                self.autosave_dirty_structure = true;
                self.autosave_dirty_all = true;
                self.tile_cache.mark_all_dirty();
            }
            DocumentInvalidation::Targeted {
                object_ids,
                dirty_rects_mm,
            } => {
                for &id in object_ids {
                    self.autosave_dirty_objects.insert(id);
                }
                for &rect in dirty_rects_mm {
                    self.tile_cache.mark_dirty(rect);
                }
                for &id in object_ids {
                    self.tile_cache.mark_object_dirty(&self.document, id);
                }
            }
        }
        self.selection.prune_missing(&self.document);
        self.sync_active_layer();
    }

    /// Maps a world-space point (mm) into global egui screen coordinates using the last
    /// painted canvas rect, if available.
    #[must_use]
    pub fn world_to_canvas_screen_pos(&self, world: Point2D) -> Option<[f32; 2]> {
        let rect = self.last_canvas_rect?;
        let sp = self.viewport.world_to_screen(world);
        Some([rect[0] + sp.x as f32, rect[1] + sp.y as f32])
    }

    /// Marks the canvas cache and live production metrics dirty after any scene edit.
    pub fn mark_modified(&mut self) {
        self.mark_modified_with_invalidation(&DocumentInvalidation::All);
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
            format!("{} *", self.document.title())
        } else {
            self.document.title().to_owned()
        }
    }

    /// Fits the active page cleanly inside the current viewport.
    pub fn fit_active_page(&mut self) {
        let layouts = compute_page_layouts(
            &self.document,
            self.document.multipage_view(),
            self.render_options.multipage_gap_mm,
        );
        let rect = if self.document.multipage_view() {
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
