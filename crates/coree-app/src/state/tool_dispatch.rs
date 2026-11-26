// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

impl StudioApp {
    /// Checks whether the active creation target layer is writable (visible and unlocked),
    /// returning `Ok(Option<LayerId>)` for `CommandOp::create_object` or an error message.
    pub(crate) fn active_creation_layer_or_err(&self) -> Result<Option<LayerId>, String> {
        let tab = self
            .tabs
            .get(self.active_tab)
            .ok_or_else(|| "no active document".to_owned())?;
        if let Some(lid) = tab.active_layer_id {
            match tab.document.find_layer(lid) {
                Some(l) if l.locked || !l.visible => Err(format!(
                    "{} ({})",
                    crate::i18n::I18nKey::ErrorLayerLocked.tr_ui(self.ui_language),
                    l.name
                )),
                Some(_) => Ok(Some(lid)),
                None => Err("Active layer no longer exists".to_owned()),
            }
        } else if let Some(page) = tab.document.active_page() {
            if page.layers.iter().any(|l| l.visible && !l.locked) {
                Ok(None)
            } else {
                Err(crate::i18n::I18nKey::ErrorLayerLocked.tr_ui(self.ui_language))
            }
        } else {
            Err("No active page".to_owned())
        }
    }

    /// Validates that the active creation target layer is visible and unlocked, pushing an
    /// error toast and returning `None` if creation should be refused.
    pub(crate) fn ensure_active_creation_layer(&mut self) -> Option<Option<LayerId>> {
        match self.active_creation_layer_or_err() {
            Ok(lid) => Some(lid),
            Err(e) => {
                self.push_toast(e, true);
                None
            }
        }
    }

    /// Creates `obj` on the active writable layer via `CommandOp::create_object`, propagating
    /// errors as error toasts and selecting the newly created object only on success.
    pub(crate) fn create_object_on_active_layer(&mut self, obj: SceneObject) -> Option<ObjectId> {
        let layer_id = self.ensure_active_creation_layer()?;
        match self.execute_command(CommandOp::create_object(obj, layer_id)) {
            Ok(()) => {
                let created_id = self
                    .active_tab()
                    .and_then(|t| t.document.objects().keys().last().copied());
                if let (Some(tab), Some(id)) = (self.active_tab_mut(), created_id) {
                    tab.selection.select_single(&tab.document, id);
                    tab.metrics_dirty = true;
                    tab.needs_rerender = true;
                }
                created_id
            }
            Err(e) => {
                self.push_toast(e, true);
                None
            }
        }
    }

    /// Executes `op` through the active tab's [`CommandHistory`] and appends it to SQLite WAL journal.
    pub fn execute_command(&mut self, op: CommandOp) -> Result<(), String> {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return Err("no active document".to_owned());
        };
        let doc_id = tab.document.id().to_string();

        // Ensure an active unclean baseline snapshot exists before journaling commands
        // (including after `save_active_as` marks the previous session cleanly closed).
        if !tab.wal_session_open {
            if let Some(ref db) = self.storage {
                if let Ok((session_id, _)) = db.resolve_or_open_session(&doc_id) {
                    if let Ok(base_id) = db.enqueue_baseline_snapshot(
                        &doc_id,
                        &session_id,
                        tab.document.title(),
                        tab.source_path.as_deref(),
                        tab.document.clone(),
                        tab.journal_seq,
                    ) {
                        tab.wal_session_open = true;
                        tab.wal_session_id = Some(session_id);
                        tab.wal_base_snapshot_id = Some(base_id);
                        tab.wal_delta_count = 0;
                        super::templates_autosave::clear_tab_autosave_dirty(tab);
                    }
                }
            }
        }

        tab.history
            .execute(&mut tab.document, op.clone())
            .map_err(|e| e.to_string())?;
        let pruned_assets = tab.history.take_last_gc_pruned();
        let inv = tab.history.last_invalidation().clone();
        let wal_hints = tab.history.last_delta_wal_hints().clone();
        tab.journal_seq = tab.journal_seq.saturating_add(1);
        let seq = tab.journal_seq;
        tab.mark_modified_with_invalidation(&inv);
        if wal_hints.touches_structure
            || super::templates_autosave::command_op_touches_structure(&op)
        {
            tab.autosave_dirty_structure = true;
        }
        tab.autosave_dirty_palette |= wal_hints.touches_palette;
        for &aid in &wal_hints.upserted_assets {
            tab.autosave_deleted_assets.remove(&aid);
            tab.autosave_upserted_assets.insert(aid);
        }
        for &aid in &wal_hints.deleted_assets {
            tab.autosave_upserted_assets.remove(&aid);
            tab.autosave_deleted_assets.insert(aid);
        }
        if !pruned_assets.is_empty() {
            tab.autosave_dirty_structure = true;
            for &aid in &pruned_assets {
                tab.autosave_upserted_assets.remove(&aid);
                tab.autosave_deleted_assets.insert(aid);
            }
        }
        tab.wal_delta_count = tab.wal_delta_count.saturating_add(1);

        match &op {
            CommandOp::TransformObjects { ids, affine } if !ids.is_empty() => {
                if let Some((ref batch_ids, ref mut prev_delta)) = tab.last_duplicate_batch {
                    if *batch_ids == *ids {
                        *prev_delta = affine.compose(prev_delta);
                    } else {
                        tab.last_duplicate_batch = None;
                    }
                }
            }
            _ => {
                tab.last_duplicate_batch = None;
            }
        }

        if let Some(ref db) = self.storage {
            if let Some(sess_id) = tab.wal_session_id.clone() {
                let base_id = tab.wal_base_snapshot_id.as_deref();
                let _ = db.enqueue_command_async(&doc_id, &sess_id, base_id, seq, op.clone());
                if !pruned_assets.is_empty() {
                    tab.journal_seq = tab.journal_seq.saturating_add(1);
                    let prune_seq = tab.journal_seq;
                    let _ = db.enqueue_command_async(
                        &doc_id,
                        &sess_id,
                        base_id,
                        prune_seq,
                        CommandOp::PruneAssets { ids: pruned_assets },
                    );
                }
                if tab.wal_delta_count >= coree_storage::WAL_COMPACTION_INTERVAL {
                    if tab.autosave_dirty_all
                        || !tab.wal_session_open
                        || tab.wal_base_snapshot_id.is_none()
                    {
                        if let Ok(snap_id) = db.enqueue_baseline_snapshot(
                            &doc_id,
                            &sess_id,
                            tab.document.title(),
                            tab.source_path.as_deref(),
                            tab.document.clone(),
                            tab.journal_seq,
                        ) {
                            tab.wal_session_open = true;
                            tab.wal_base_snapshot_id = Some(snap_id);
                            tab.wal_delta_count = 0;
                            super::templates_autosave::clear_tab_autosave_dirty(tab);
                        }
                    } else if let Some(latest_id) = tab.wal_base_snapshot_id.clone() {
                        // Flush accumulated dirty objects/structure/assets/palette into a
                        // differential delta at `tab.journal_seq` before compacting, so the
                        // compacted baseline never skips un-materialized `command_journal` edits.
                        let patch = super::templates_autosave::build_differential_patch_from_tab(
                            tab,
                            &doc_id,
                            sess_id.clone(),
                            latest_id,
                        );
                        if let Ok(delta_id) = db.enqueue_differential_autosave(patch) {
                            tab.wal_base_snapshot_id = Some(delta_id.clone());
                            super::templates_autosave::clear_tab_autosave_dirty(tab);
                            if let Ok(new_base) = db.enqueue_compact_baseline(
                                &doc_id,
                                &sess_id,
                                tab.document.title(),
                                tab.source_path.as_deref(),
                                &delta_id,
                                tab.journal_seq,
                            ) {
                                tab.wal_base_snapshot_id = Some(new_base);
                                tab.wal_delta_count = 0;
                            }
                        }
                    }
                }
            } else {
                let _ = db.append_command(&doc_id, seq, &op);
                if !pruned_assets.is_empty() {
                    tab.journal_seq = tab.journal_seq.saturating_add(1);
                    let prune_seq = tab.journal_seq;
                    let _ = db.append_command(
                        &doc_id,
                        prune_seq,
                        &CommandOp::PruneAssets { ids: pruned_assets },
                    );
                }
            }
        }
        self.last_world_transform = match &op {
            CommandOp::TransformObjects { ids, affine } if !ids.is_empty() => {
                let doc = &self.tabs[self.active_tab].document;
                Some(helpers::unconjugate_local_affine_to_world(
                    doc, ids[0], *affine,
                ))
            }
            CommandOp::Batch(ops)
                if !ops.is_empty()
                    && ops.iter().all(|inner| {
                        matches!(inner, CommandOp::TransformObjects { ids, .. } if !ids.is_empty())
                    }) =>
            {
                if let Some(CommandOp::TransformObjects { ids, affine }) = ops.first() {
                    let doc = &self.tabs[self.active_tab].document;
                    Some(helpers::unconjugate_local_affine_to_world(
                        doc, ids[0], *affine,
                    ))
                } else {
                    None
                }
            }
            _ => None,
        };
        self.last_executed_op = Some(op);
        Ok(())
    }

    /// Applies `world_affine` in world coordinates to `ids` via `build_world_transform_command`.
    pub(crate) fn apply_world_transform_to_selection(
        &mut self,
        ids: &[ObjectId],
        world_affine: Affine2D,
    ) -> bool {
        let Some(cmd) = self
            .active_tab()
            .and_then(|t| helpers::build_world_transform_command(&t.document, ids, world_affine))
        else {
            return false;
        };
        self.execute_command(cmd).is_ok()
    }

    /// Undoes the last command on the active tab and streams a targeted zero-clone WAL delta
    /// so a subsequent crash cannot resurrect the undone edit.
    pub fn undo(&mut self) -> Result<String, String> {
        let (label, inv, wal_hints) = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let label = tab
                .history
                .undo(&mut tab.document)
                .map_err(|e| e.to_string())?;
            let inv = tab.history.last_invalidation().clone();
            let wal_hints = tab.history.last_delta_wal_hints().clone();
            tab.last_duplicate_batch = None;
            tab.viewport.multipage_view = tab.document.multipage_view();
            tab.snap_options.grid = tab.document.grid().snap;
            tab.sync_active_layer();
            tab.mark_modified_with_invalidation(&inv);
            tab.autosave_dirty_structure |= wal_hints.touches_structure;
            tab.autosave_dirty_palette |= wal_hints.touches_palette;
            for &aid in &wal_hints.upserted_assets {
                tab.autosave_deleted_assets.remove(&aid);
                tab.autosave_upserted_assets.insert(aid);
            }
            for &aid in &wal_hints.deleted_assets {
                tab.autosave_upserted_assets.remove(&aid);
                tab.autosave_deleted_assets.insert(aid);
            }
            tab.journal_seq = tab.journal_seq.saturating_add(1);
            tab.wal_delta_count = tab.wal_delta_count.saturating_add(1);
            (label, inv, wal_hints)
        };
        self.stream_wal_undo_redo_delta(&inv, &wal_hints);
        self.push_toast(format!("Undo: {label}"), false);
        Ok(label)
    }

    /// Redoes the last undone command on the active tab and streams a targeted zero-clone WAL delta.
    pub fn redo(&mut self) -> Result<String, String> {
        let (label, inv, wal_hints) = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let label = tab
                .history
                .redo(&mut tab.document)
                .map_err(|e| e.to_string())?;
            let inv = tab.history.last_invalidation().clone();
            let wal_hints = tab.history.last_delta_wal_hints().clone();
            tab.last_duplicate_batch = None;
            tab.viewport.multipage_view = tab.document.multipage_view();
            tab.snap_options.grid = tab.document.grid().snap;
            tab.sync_active_layer();
            tab.mark_modified_with_invalidation(&inv);
            tab.autosave_dirty_structure |= wal_hints.touches_structure;
            tab.autosave_dirty_palette |= wal_hints.touches_palette;
            for &aid in &wal_hints.upserted_assets {
                tab.autosave_deleted_assets.remove(&aid);
                tab.autosave_upserted_assets.insert(aid);
            }
            for &aid in &wal_hints.deleted_assets {
                tab.autosave_upserted_assets.remove(&aid);
                tab.autosave_deleted_assets.insert(aid);
            }
            tab.journal_seq = tab.journal_seq.saturating_add(1);
            tab.wal_delta_count = tab.wal_delta_count.saturating_add(1);
            (label, inv, wal_hints)
        };
        self.stream_wal_undo_redo_delta(&inv, &wal_hints);
        self.push_toast(format!("Redo: {label}"), false);
        Ok(label)
    }

    fn stream_wal_undo_redo_delta(
        &mut self,
        inv: &coree_core::DocumentInvalidation,
        wal_hints: &coree_core::DeltaWalHints,
    ) {
        let full_all = matches!(inv, coree_core::DocumentInvalidation::All);
        let needs_baseline = self.tabs.get(self.active_tab).is_some_and(|t| {
            !t.wal_session_open
                || t.wal_session_id.is_none()
                || t.wal_base_snapshot_id.is_none()
                || t.autosave_dirty_all
                || full_all
        });
        if needs_baseline {
            self.checkpoint_wal_snapshot_active();
            return;
        }
        let Some(ref db) = self.storage else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let (Some(session_id), Some(base_id)) =
            (tab.wal_session_id.clone(), tab.wal_base_snapshot_id.clone())
        else {
            return;
        };
        let doc_id = tab.document.id().to_string();
        let struct_dirty = tab.autosave_dirty_structure || wal_hints.touches_structure;
        let mut upserted_objects = Vec::new();
        let mut deleted_object_ids = Vec::new();
        for &oid in &tab.autosave_dirty_objects {
            if let Some(obj) = tab.document.object(oid) {
                upserted_objects.push((oid, obj.clone()));
            } else {
                deleted_object_ids.push(oid);
            }
        }
        let mut merged_upserted: HashSet<ImageAssetId> = tab.autosave_upserted_assets.clone();
        merged_upserted.extend(wal_hints.upserted_assets.iter().copied());
        let mut merged_deleted: HashSet<ImageAssetId> = tab.autosave_deleted_assets.clone();
        merged_deleted.extend(wal_hints.deleted_assets.iter().copied());
        for aid in &merged_deleted {
            merged_upserted.remove(aid);
        }
        let mut upserted_assets = Vec::new();
        for aid in merged_upserted {
            if let Some(asset) = tab.document.asset(aid) {
                upserted_assets.push((aid, asset.clone()));
            }
        }
        let deleted_asset_ids: Vec<ImageAssetId> = merged_deleted.into_iter().collect();
        let pal_dirty = struct_dirty || tab.autosave_dirty_palette || wal_hints.touches_palette;
        let patch = DifferentialSnapshotPatch {
            doc_id: doc_id.clone(),
            session_id: session_id.clone(),
            title: tab.document.title().to_owned(),
            source_path: tab
                .source_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            base_snapshot_id: base_id,
            last_command_seq: tab.journal_seq,
            upserted_objects,
            deleted_object_ids,
            upserted_assets,
            deleted_asset_ids,
            full_assets: None,
            pages: struct_dirty.then(|| tab.document.pages().to_vec()),
            active_page_index: struct_dirty.then(|| tab.document.active_page_index()),
            next_id: Some(tab.document.next_id()),
            color_mode: struct_dirty.then(|| tab.document.color_mode()),
            default_unit: struct_dirty.then(|| tab.document.default_unit()),
            dpi: struct_dirty.then(|| tab.document.dpi()),
            multipage_view: struct_dirty.then(|| tab.document.multipage_view()),
            palettes: pal_dirty.then(|| tab.document.palettes().to_vec()),
            document_palette: pal_dirty.then(|| tab.document.document_palette().clone()),
            guides: struct_dirty.then(|| tab.document.guides().to_vec()),
            grid: struct_dirty.then(|| tab.document.grid()),
            perspective_grid: struct_dirty.then(|| tab.document.perspective_grid().clone()),
            color_styles: struct_dirty.then(|| tab.document.color_styles().to_vec()),
            object_styles: struct_dirty.then(|| tab.document.object_styles().to_vec()),
            symbols: struct_dirty.then(|| tab.document.symbols().clone()),
            metadata: struct_dirty.then(|| tab.document.metadata().clone()),
            custom_icc_profile: struct_dirty
                .then(|| tab.document.custom_icc_profile().map(<[u8]>::to_vec)),
            custom_icc_name: struct_dirty
                .then(|| tab.document.custom_icc_name().map(str::to_owned)),
            rgb_icc_profile: struct_dirty
                .then(|| tab.document.rgb_icc_profile().map(<[u8]>::to_vec)),
            rgb_icc_name: struct_dirty.then(|| tab.document.rgb_icc_name().map(str::to_owned)),
            cmyk_icc_profile: struct_dirty
                .then(|| tab.document.cmyk_icc_profile().map(<[u8]>::to_vec)),
            cmyk_icc_name: struct_dirty.then(|| tab.document.cmyk_icc_name().map(str::to_owned)),
            gray_icc_profile: struct_dirty
                .then(|| tab.document.gray_icc_profile().map(<[u8]>::to_vec)),
            gray_icc_name: struct_dirty.then(|| tab.document.gray_icc_name().map(str::to_owned)),
            rendering_intent: struct_dirty.then(|| tab.document.rendering_intent()),
            black_point_compensation: struct_dirty.then(|| tab.document.black_point_compensation()),
            drawing_scale: struct_dirty.then(|| tab.document.drawing_scale()),
            unknown_doc_chunks: struct_dirty.then(|| tab.document.unknown_doc_chunks().to_vec()),
            unknown_data_files: struct_dirty.then(|| tab.document.unknown_data_files().clone()),
        };
        if let Ok(delta_id) = db.enqueue_differential_autosave(patch) {
            tab.wal_base_snapshot_id = Some(delta_id.clone());
            super::templates_autosave::clear_tab_autosave_dirty(tab);
            if tab.wal_delta_count >= coree_storage::WAL_COMPACTION_INTERVAL {
                if let Ok(new_base) = db.enqueue_compact_baseline(
                    &doc_id,
                    &session_id,
                    tab.document.title(),
                    tab.source_path.as_deref(),
                    &delta_id,
                    tab.journal_seq,
                ) {
                    tab.wal_base_snapshot_id = Some(new_base);
                    tab.wal_delta_count = 0;
                }
            }
        }
    }

    /// Builds an immutable [`ToolSettingsSnapshot`] + active lens config for `ToolCore`.
    #[must_use]
    pub fn current_tool_snapshot(&self) -> (ToolSettingsSnapshot, LensConfig) {
        (
            self.tool_settings
                .to_snapshot(&self.properties_state, &self.effects_state),
            self.lens_state.build_config(),
        )
    }

    /// Dispatches a pointer-down event to the active tab's [`ToolCore`] and applies its output.
    pub fn run_tool_pointer_down(&mut self, event: &PointerEvent) {
        let creation_layer_id = self.ensure_active_creation_layer().flatten();
        let (settings, default_lens) = self.current_tool_snapshot();
        let treat_all = self.treat_all_objects_as_filled;
        let default_fill = self.default_fill.clone();
        let default_outline = self.default_outline.clone();
        let default_text_style = self.default_text_style.clone();
        let active_tool = self.active_tool;

        let out = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return;
            };
            if tab.tool_core.active_tool() != active_tool {
                tab.tool_core.set_active_tool(active_tool);
            }
            tab.cursor_world_mm = event.world_pt_mm;
            // Keep ToolCore's mirror of pending crop/raster/guide/mesh state synced if modified from toolbar.
            tab.tool_core.pending_crop_rect = tab.pending_crop_rect;
            tab.tool_core.raster_mask_rect_mm = tab.raster_mask_rect_mm;
            tab.tool_core.selected_guide_id = tab.selected_guide_id;
            tab.tool_core.selected_mesh_node = tab.selected_mesh_node;
            let mut tool_core = std::mem::replace(&mut tab.tool_core, ToolCore::new(active_tool));
            let ctx = ToolReadContext {
                document: &tab.document,
                selection: &tab.selection,
                snap_engine: &SnapEngine,
                viewport: &tab.viewport,
                snap_options: &tab.snap_options,
                active_layer_id: tab.active_layer_id,
                creation_layer_id,
                powerclip_edit_id: tab.powerclip_edit_id,
                treat_all_objects_as_filled: treat_all,
                default_fill: &default_fill,
                default_outline: &default_outline,
                default_text_style: &default_text_style,
                default_lens: &default_lens,
                settings: &settings,
            };
            let out = tool_core.on_pointer_down(&ctx, event);
            tab.tool_core = tool_core;
            out
        };
        self.apply_tool_output(out);
    }

    /// Dispatches a pointer-move/drag event to the active tab's [`ToolCore`] and applies its output.
    pub fn run_tool_pointer_move(&mut self, event: &PointerEvent) {
        let creation_layer_id = self
            .tabs
            .get(self.active_tab)
            .and_then(|t| t.active_layer_id);
        let (settings, default_lens) = self.current_tool_snapshot();
        let treat_all = self.treat_all_objects_as_filled;
        let default_fill = self.default_fill.clone();
        let default_outline = self.default_outline.clone();
        let default_text_style = self.default_text_style.clone();
        let active_tool = self.active_tool;

        let out = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return;
            };
            if tab.tool_core.active_tool() != active_tool {
                tab.tool_core.set_active_tool(active_tool);
            }
            tab.cursor_world_mm = event.world_pt_mm;
            tab.tool_core.pending_crop_rect = tab.pending_crop_rect;
            tab.tool_core.raster_mask_rect_mm = tab.raster_mask_rect_mm;
            tab.tool_core.selected_guide_id = tab.selected_guide_id;
            tab.tool_core.selected_mesh_node = tab.selected_mesh_node;
            let mut tool_core = std::mem::replace(&mut tab.tool_core, ToolCore::new(active_tool));
            let ctx = ToolReadContext {
                document: &tab.document,
                selection: &tab.selection,
                snap_engine: &SnapEngine,
                viewport: &tab.viewport,
                snap_options: &tab.snap_options,
                active_layer_id: tab.active_layer_id,
                creation_layer_id,
                powerclip_edit_id: tab.powerclip_edit_id,
                treat_all_objects_as_filled: treat_all,
                default_fill: &default_fill,
                default_outline: &default_outline,
                default_text_style: &default_text_style,
                default_lens: &default_lens,
                settings: &settings,
            };
            let out = tool_core.on_pointer_move(&ctx, event);
            tab.tool_core = tool_core;
            out
        };
        self.apply_tool_output(out);
    }

    /// Dispatches a pointer-up event to the active tab's [`ToolCore`] and applies its output.
    pub fn run_tool_pointer_up(&mut self, event: &PointerEvent) {
        let creation_layer_id = self.ensure_active_creation_layer().flatten();
        let (settings, default_lens) = self.current_tool_snapshot();
        let treat_all = self.treat_all_objects_as_filled;
        let default_fill = self.default_fill.clone();
        let default_outline = self.default_outline.clone();
        let default_text_style = self.default_text_style.clone();
        let active_tool = self.active_tool;

        let out = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return;
            };
            if tab.tool_core.active_tool() != active_tool {
                tab.tool_core.set_active_tool(active_tool);
            }
            tab.cursor_world_mm = event.world_pt_mm;
            tab.tool_core.pending_crop_rect = tab.pending_crop_rect;
            tab.tool_core.raster_mask_rect_mm = tab.raster_mask_rect_mm;
            tab.tool_core.selected_guide_id = tab.selected_guide_id;
            tab.tool_core.selected_mesh_node = tab.selected_mesh_node;
            let mut tool_core = std::mem::replace(&mut tab.tool_core, ToolCore::new(active_tool));
            let ctx = ToolReadContext {
                document: &tab.document,
                selection: &tab.selection,
                snap_engine: &SnapEngine,
                viewport: &tab.viewport,
                snap_options: &tab.snap_options,
                active_layer_id: tab.active_layer_id,
                creation_layer_id,
                powerclip_edit_id: tab.powerclip_edit_id,
                treat_all_objects_as_filled: treat_all,
                default_fill: &default_fill,
                default_outline: &default_outline,
                default_text_style: &default_text_style,
                default_lens: &default_lens,
                settings: &settings,
            };
            let out = tool_core.on_pointer_up(&ctx, event);
            tab.tool_core = tool_core;
            out
        };
        self.apply_tool_output(out);
    }

    /// Dispatches a keyboard event to the active tab's [`ToolCore`] and applies its output.
    pub fn run_tool_key_event(&mut self, event: &KeyEvent) {
        let creation_layer_id = self
            .tabs
            .get(self.active_tab)
            .and_then(|t| t.active_layer_id);
        let (settings, default_lens) = self.current_tool_snapshot();
        let treat_all = self.treat_all_objects_as_filled;
        let default_fill = self.default_fill.clone();
        let default_outline = self.default_outline.clone();
        let default_text_style = self.default_text_style.clone();
        let active_tool = self.active_tool;

        let out = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return;
            };
            if tab.tool_core.active_tool() != active_tool {
                tab.tool_core.set_active_tool(active_tool);
            }
            tab.tool_core.pending_crop_rect = tab.pending_crop_rect;
            tab.tool_core.raster_mask_rect_mm = tab.raster_mask_rect_mm;
            tab.tool_core.selected_guide_id = tab.selected_guide_id;
            tab.tool_core.selected_mesh_node = tab.selected_mesh_node;
            let mut tool_core = std::mem::replace(&mut tab.tool_core, ToolCore::new(active_tool));
            let ctx = ToolReadContext {
                document: &tab.document,
                selection: &tab.selection,
                snap_engine: &SnapEngine,
                viewport: &tab.viewport,
                snap_options: &tab.snap_options,
                active_layer_id: tab.active_layer_id,
                creation_layer_id,
                powerclip_edit_id: tab.powerclip_edit_id,
                treat_all_objects_as_filled: treat_all,
                default_fill: &default_fill,
                default_outline: &default_outline,
                default_text_style: &default_text_style,
                default_lens: &default_lens,
                settings: &settings,
            };
            let out = tool_core.on_key(&ctx, event);
            tab.tool_core = tool_core;
            out
        };
        self.apply_tool_output(out);
    }

    /// Applies a [`ToolOutput`] returned by [`ToolCore`] to the active tab and studio shell.
    pub fn apply_tool_output(&mut self, output: ToolOutput) {
        match output {
            ToolOutput::Continue { overlay } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.active_guides = overlay.active_guides.clone();
                    tab.transient_overlay = overlay;
                    tab.viewport.lod_dragging = tab.tool_core.is_transform_dragging();
                }
            }
            ToolOutput::Cancel => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.transient_overlay = TransientOverlay::default();
                    tab.active_guides.clear();
                    tab.viewport.lod_dragging = false;
                    tab.pending_crop_rect = tab.tool_core.pending_crop_rect;
                    tab.raster_mask_rect_mm = tab.tool_core.raster_mask_rect_mm;
                }
            }
            ToolOutput::Commit {
                mut ops,
                clear_overlay,
                post_actions,
            } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.lod_dragging = false;
                    if clear_overlay {
                        tab.transient_overlay = TransientOverlay::default();
                        tab.active_guides.clear();
                    }
                    tab.pending_crop_rect = tab.tool_core.pending_crop_rect;
                    tab.raster_mask_rect_mm = tab.tool_core.raster_mask_rect_mm;
                    tab.selected_guide_id = tab.tool_core.selected_guide_id;
                    tab.selected_mesh_node = tab.tool_core.selected_mesh_node;
                }
                if !ops.is_empty() {
                    let has_create = ops
                        .iter()
                        .any(|op| matches!(op, CommandOp::CreateObject { .. }));
                    let has_explicit_select = post_actions
                        .iter()
                        .any(|a| matches!(a, ToolActionRequest::SelectObjects { .. }));
                    let cmd = if ops.len() == 1 {
                        ops.remove(0)
                    } else {
                        CommandOp::Batch(ops)
                    };
                    if let Err(err) = self.execute_command(cmd) {
                        self.push_toast(err, true);
                        return;
                    }
                    if has_create && !has_explicit_select {
                        let before_sel = self
                            .active_tab()
                            .map(|t| t.selection.objects.clone())
                            .unwrap_or_default();
                        let mut updated = false;
                        if let Some(tab) = self.active_tab_mut() {
                            if let Some(created_id) = tab.document.objects().keys().last().copied()
                            {
                                tab.selection.select_single(&tab.document, created_id);
                                tab.metrics_dirty = true;
                                updated = true;
                            }
                        }
                        if updated {
                            self.maybe_fire_selection_change(&before_sel);
                        }
                    }
                }
                for act in post_actions {
                    self.handle_tool_action(act);
                }
            }
            ToolOutput::Consumed { overlay, actions } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.lod_dragging = tab.tool_core.is_transform_dragging();
                    if let Some(ov) = overlay {
                        tab.active_guides = ov.active_guides.clone();
                        tab.transient_overlay = ov;
                    }
                    tab.pending_crop_rect = tab.tool_core.pending_crop_rect;
                    tab.raster_mask_rect_mm = tab.tool_core.raster_mask_rect_mm;
                    tab.selected_guide_id = tab.tool_core.selected_guide_id;
                    tab.selected_mesh_node = tab.tool_core.selected_mesh_node;
                }
                for act in actions {
                    self.handle_tool_action(act);
                }
            }
        }
    }

    /// Executes a single host-delegated [`ToolActionRequest`] emitted by [`ToolCore`].
    pub fn handle_tool_action(&mut self, action: ToolActionRequest) {
        match action {
            ToolActionRequest::SelectObjects {
                ids,
                additive,
                toggle,
            } => {
                let active_tool = self.active_tool;
                let before_sel = self
                    .active_tab()
                    .map(|t| t.selection.objects.clone())
                    .unwrap_or_default();
                if let Some(tab) = self.active_tab_mut() {
                    if ids.is_empty() {
                        if !additive && !toggle {
                            tab.selection.clear();
                        }
                    } else if additive {
                        for id in ids {
                            if !tab.selection.contains(id) {
                                tab.selection.toggle_object(&tab.document, id);
                            }
                        }
                    } else if toggle {
                        for id in ids {
                            tab.selection.click_object(&tab.document, id, true);
                        }
                    } else if ids.len() == 1 {
                        if active_tool == ToolId::Pick {
                            tab.selection.click_object(&tab.document, ids[0], false);
                        } else {
                            tab.selection.select_single(&tab.document, ids[0]);
                        }
                    } else {
                        tab.selection.select_many(&tab.document, &ids);
                    }
                    tab.metrics_dirty = true;
                    tab.last_duplicate_batch = None;
                }
                self.maybe_fire_selection_change(&before_sel);
            }
            ToolActionRequest::ClearSelection => {
                let before_sel = self
                    .active_tab()
                    .map(|t| t.selection.objects.clone())
                    .unwrap_or_default();
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.clear();
                    tab.metrics_dirty = true;
                    tab.last_duplicate_batch = None;
                }
                self.maybe_fire_selection_change(&before_sel);
            }
            ToolActionRequest::ToggleTransformMode => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.toggle_transform_mode();
                    tab.metrics_dirty = true;
                }
            }
            ToolActionRequest::SelectNodes { nodes, additive } => {
                if let Some(tab) = self.active_tab_mut() {
                    if !additive {
                        tab.selection.node_selection.clear();
                    }
                    for n in nodes {
                        if additive {
                            tab.selection.node_selection.toggle(n);
                        } else if !tab.selection.node_selection.contains(n) {
                            tab.selection.node_selection.nodes.push(n);
                        }
                    }
                }
            }
            ToolActionRequest::ClearNodeSelection => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.node_selection.clear();
                }
            }
            ToolActionRequest::SetPivot { pivot_mm } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.set_pivot(pivot_mm);
                }
            }
            ToolActionRequest::PanTo {
                pan_mm,
                lod_dragging,
            } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.pan_mm = pan_mm;
                    tab.viewport.lod_dragging = lod_dragging;
                    tab.needs_rerender = true;
                }
            }
            ToolActionRequest::ZoomFitRect {
                rect_mm,
                margin_px,
                one_shot_complete,
            } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.fit_rect(rect_mm, margin_px);
                    tab.needs_rerender = true;
                }
                if one_shot_complete {
                    self.complete_one_shot_zoom_if_active();
                }
            }
            ToolActionRequest::ZoomAtScreen {
                screen_px,
                factor,
                one_shot_complete,
            } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.zoom_at_screen_point(screen_px, factor);
                    tab.needs_rerender = true;
                }
                if one_shot_complete {
                    self.complete_one_shot_zoom_if_active();
                }
            }
            ToolActionRequest::SetPendingCropRect(rect) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.pending_crop_rect = rect;
                    tab.tool_core.pending_crop_rect = rect;
                }
            }
            ToolActionRequest::ApplyCropRect(rect) => {
                self.apply_crop_rect(rect);
                if let Some(tab) = self.active_tab_mut() {
                    tab.pending_crop_rect = None;
                    tab.tool_core.pending_crop_rect = None;
                    tab.transient_overlay = TransientOverlay::default();
                }
            }
            ToolActionRequest::SetRasterMaskRect(rect) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.raster_mask_rect_mm = rect;
                    tab.tool_core.raster_mask_rect_mm = rect;
                }
            }
            ToolActionRequest::SetRasterMask(sel) => {
                if let Some(tab) = self.active_tab_mut() {
                    if let Some(ref s) = sel {
                        if let Some(bb) = s.world_bounds() {
                            tab.raster_mask_rect_mm = Some(bb);
                            tab.tool_core.raster_mask_rect_mm = Some(bb);
                        }
                    }
                    tab.raster_mask = sel;
                    tab.needs_rerender = true;
                }
            }
            ToolActionRequest::ClearRasterMask => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.raster_mask = None;
                    tab.raster_mask_rect_mm = None;
                    tab.tool_core.raster_mask_rect_mm = None;
                    tab.needs_rerender = true;
                }
            }
            ToolActionRequest::SetSelectedGuide(gid) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selected_guide_id = gid;
                    tab.tool_core.selected_guide_id = gid;
                }
            }
            ToolActionRequest::SetSelectedMeshNode(node) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selected_mesh_node = node;
                    tab.tool_core.selected_mesh_node = node;
                }
            }
            ToolActionRequest::EnterPowerClipEdit(oid) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.powerclip_edit_id = Some(oid);
                }
            }
            ToolActionRequest::ExitPowerClipEdit => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.powerclip_edit_id = None;
                }
            }
            ToolActionRequest::OpenInlineTextEdit {
                object_id,
                world_pos_mm,
                is_paragraph,
                box_size_mm,
            } => {
                self.begin_inline_text_edit(object_id, world_pos_mm, is_paragraph, box_size_mm);
            }
            ToolActionRequest::CommitInlineTextEdit => {
                let _ = self.commit_inline_text_edit();
            }
            ToolActionRequest::SmartFillAt { pt_mm } => {
                if let Some(lid) = self.ensure_active_creation_layer() {
                    let created = self.active_tab().and_then(|tab| {
                        create_smart_fill_object(
                            &tab.document,
                            tab.document.active_page_index(),
                            pt_mm,
                            self.default_fill.clone(),
                            self.default_outline.clone(),
                        )
                    });
                    if let Some(obj) = created {
                        match self.execute_command(CommandOp::create_object(obj, lid)) {
                            Ok(()) => {
                                if let Some(tab) = self.active_tab_mut() {
                                    if let Some(new_id) =
                                        tab.document.objects().keys().last().copied()
                                    {
                                        tab.selection.select_single(&tab.document, new_id);
                                        tab.metrics_dirty = true;
                                    }
                                }
                                self.push_toast("Created Smart Fill region", false);
                            }
                            Err(e) => self.push_toast(e, true),
                        }
                    } else {
                        self.push_toast("Click inside an enclosed boundary for Smart Fill", false);
                    }
                }
            }
            ToolActionRequest::ApplyEyedropper {
                pt_mm,
                attributes_mode,
                force_sample,
            } => {
                self.eyedropper_click(pt_mm, attributes_mode, force_sample);
            }
            ToolActionRequest::ApplyRasterStroke {
                tool,
                points_mm,
                samples,
            } => {
                let _ = self.apply_raster_tool_stroke_with_stylus(tool, &points_mm, &samples);
            }
            ToolActionRequest::SetCloneSource(pt) => {
                self.tool_settings.clone_source_mm = Some(pt);
                self.push_toast(
                    format!("Set sample source at ({:.1}, {:.1}) mm", pt.x, pt.y),
                    false,
                );
            }
            ToolActionRequest::StampDuplicateDuringDrag { live_world_affine } => {
                let _ = self.stamp_duplicate_with_affine(live_world_affine);
            }
            ToolActionRequest::ApplyLensToObject { object_id } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.select_single(&tab.document, object_id);
                }
                let cfg = self.lens_state.build_config();
                self.apply_lens_to_selection(Some(cfg));
            }
            ToolActionRequest::EnsureEnvelopeOnObject { object_id } => {
                let _ = self.ensure_envelope_on_object(object_id);
            }
            ToolActionRequest::MeshFillInteract {
                start_mm,
                end_mm,
                shift_recolor,
            } => {
                let recolor = if shift_recolor {
                    self.default_fill.referenced_colors().into_iter().next()
                } else {
                    None
                };
                self.mesh_fill_interact(start_mm, end_mm, recolor);
            }
            ToolActionRequest::Toast { message, is_error } => {
                self.push_toast(message, is_error);
            }
        }
    }
}
