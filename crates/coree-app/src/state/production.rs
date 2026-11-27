// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

mod palette_io_macros;
mod prepress_laser;
mod raster_trace_fonts;
pub(super) use raster_trace_fonts::{compute_trace_placed_transform, crop_filtered_trace_asset};

impl StudioApp {
    // -------------------------------------------------------------------------
    // Production, Prepress, Trace, Bitmap Studio, Find & Replace, Macros
    // -------------------------------------------------------------------------

    /// Builds a [`FindQuery`] from [`FindReplaceState`].
    #[must_use]
    pub fn build_find_query(&self) -> FindQuery {
        let fr = &self.find_replace_state;
        let (min_w, max_w) = if fr.use_outline_width {
            let lo = fr
                .outline_width_min_mm
                .min(fr.outline_width_max_mm)
                .max(0.0);
            let hi = fr.outline_width_min_mm.max(fr.outline_width_max_mm).max(lo);
            (Some(lo), Some(hi))
        } else {
            (None, None)
        };
        let layer_trim = fr.layer_filter.trim();
        let parsed_layer_id = layer_trim.parse::<u64>().ok().map(LayerId);
        let layer_name =
            (!layer_trim.is_empty() && parsed_layer_id.is_none()).then(|| layer_trim.to_owned());
        FindQuery {
            scope: fr.scope,
            object_kind: fr.object_kind,
            fill_color: fr
                .use_fill_color
                .then(|| coree_core::ColorMatch::exact(fr.find_fill_color.clone())),
            outline_color: fr
                .use_outline_color
                .then(|| coree_core::ColorMatch::exact(fr.find_outline_color.clone())),
            outline_width_min_mm: min_w,
            outline_width_max_mm: max_w,
            is_hairline: fr.filter_hairline,
            font_family: (!fr.font_query.trim().is_empty())
                .then(|| fr.font_query.trim().to_owned()),
            text_pattern: (!fr.text_query.is_empty()).then(|| {
                (
                    fr.text_query.clone(),
                    TextMatchMode::Contains {
                        case_sensitive: fr.case_sensitive,
                    },
                )
            }),
            lens_kind: fr.lens_filter,
            layer_id: parsed_layer_id,
            layer_name,
        }
    }

    /// Finds matching objects and selects them on the active document.
    pub fn run_find_select(&mut self) -> Vec<ObjectId> {
        let query = self.build_find_query();
        let before_sel = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        let ids = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Vec::new();
            };
            let ids = find_objects(&tab.document, &query);
            tab.selection.select_many(&tab.document, &ids);
            tab.metrics_dirty = true;
            ids
        };
        self.find_replace_state.last_report = Some(FindReplaceReport {
            matched_ids: ids.clone(),
            modified_ids: Vec::new(),
            replacements_applied: 0,
        });
        self.push_toast(format!("Found {} matching object(s)", ids.len()), false);
        self.maybe_fire_selection_change(&before_sel);
        ids
    }

    /// Evaluates the clean-room CQL query in `find_replace_state.cql_query` and selects matches.
    pub fn run_cql_select(&mut self) -> Result<Vec<ObjectId>, String> {
        let scope = self.find_replace_state.scope;
        let query = self.find_replace_state.cql_query.clone();
        let before_sel = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        let res = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("No active document".to_owned());
            };
            let matched =
                evaluate_cql_query(&tab.document, scope, &query).map_err(|e| e.to_string())?;
            tab.selection.select_many(&tab.document, &matched);
            tab.metrics_dirty = true;
            matched
        };
        self.find_replace_state.last_cql_error = None;
        self.find_replace_state.last_report = Some(FindReplaceReport {
            matched_ids: res.clone(),
            modified_ids: Vec::new(),
            replacements_applied: 0,
        });
        self.push_toast(format!("CQL matched {} object(s)", res.len()), false);
        self.maybe_fire_selection_change(&before_sel);
        Ok(res)
    }

    /// Runs `coree_prod::find_and_replace` via [`CommandOp::FindAndReplace`] with explicit `action`.
    pub fn run_find_and_replace_action(&mut self, action: ReplaceAction) -> FindReplaceReport {
        let query = self.build_find_query();
        let before_sel = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        let rep = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return FindReplaceReport::default();
            };
            if matches!(action, ReplaceAction::SelectMatches) {
                let matched_ids = find_objects(&tab.document, &query);
                FindReplaceReport {
                    matched_ids,
                    modified_ids: Vec::new(),
                    replacements_applied: 0,
                }
            } else {
                coree_core::preview_find_and_replace(&tab.document, &query, &action)
            }
        };
        if matches!(action, ReplaceAction::SelectMatches) || rep.modified_ids.is_empty() {
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                if !rep.matched_ids.is_empty() {
                    tab.selection.select_many(&tab.document, &rep.matched_ids);
                    tab.metrics_dirty = true;
                }
            }
        } else {
            if let Err(err) = self.execute_command(CommandOp::FindAndReplace { query, action }) {
                self.push_toast(format!("Find & Replace failed: {err}"), true);
                return FindReplaceReport::default();
            }
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                if !rep.matched_ids.is_empty() {
                    tab.selection.select_many(&tab.document, &rep.matched_ids);
                    tab.metrics_dirty = true;
                }
            }
        }
        self.find_replace_state.last_report = Some(rep.clone());
        self.push_toast(
            format!(
                "Find & Replace: {} matched, {} updated",
                rep.matched_ids.len(),
                rep.replacements_applied
            ),
            false,
        );
        self.maybe_fire_selection_change(&before_sel);
        rep
    }

    /// Activates `tool` and updates its parent flyout group's remembered primary button.
    pub fn select_tool(&mut self, tool: ToolId) {
        if (tool != ToolId::Pick && tool != ToolId::Zoom)
            || (tool == ToolId::Zoom && self.one_shot_zoom_return_tool.is_none())
        {
            self.previous_non_pick_tool = tool;
        }
        if tool != ToolId::Zoom {
            self.one_shot_zoom_return_tool = None;
        }
        // Sync raster sub-mode cleanly when switching between raster toolbox buttons
        match tool {
            ToolId::BitmapEraser => self.tool_settings.raster_op = RasterBrushOp::Eraser,
            ToolId::CloneStamp => self.tool_settings.raster_op = RasterBrushOp::CloneStamp,
            ToolId::Smudge => self.tool_settings.raster_op = RasterBrushOp::Smudge,
            ToolId::BitmapBrush
                if matches!(
                    self.tool_settings.raster_op,
                    RasterBrushOp::Eraser | RasterBrushOp::CloneStamp | RasterBrushOp::Smudge
                ) =>
            {
                self.tool_settings.raster_op = RasterBrushOp::Paint;
            }
            ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
                // Start fresh in Sample mode whenever the user picks an Eyedropper tool
                self.tool_settings.eyedropper_apply_mode = false;
            }
            _ => {}
        }
        let g = tool.group_index();
        self.group_active_tools[g] = tool;
        self.active_tool = tool;
        self.open_flyout_group = None;
        if let Some(tab) = self.active_tab_mut() {
            tab.tool_core.set_active_tool(tool);
            tab.transient_overlay = TransientOverlay::default();
            if tool != ToolId::Crop {
                tab.pending_crop_rect = None;
            }
        }
    }

    /// Selects a specific [`RasterBrushOp`] (Paint, Eraser, CloneStamp, Smudge, Healing, Dodge, Burn, FloodFill)
    /// and activates the corresponding toolbox tool.
    pub fn set_raster_brush_op(&mut self, op: RasterBrushOp) {
        let target_tool = match op {
            RasterBrushOp::Eraser => ToolId::BitmapEraser,
            RasterBrushOp::CloneStamp => ToolId::CloneStamp,
            RasterBrushOp::Smudge => ToolId::Smudge,
            RasterBrushOp::Paint
            | RasterBrushOp::Healing
            | RasterBrushOp::Dodge
            | RasterBrushOp::Burn
            | RasterBrushOp::FloodFill => ToolId::BitmapBrush,
        };
        self.select_tool(target_tool);
        self.tool_settings.raster_op = op;
    }

    /// Activates one-shot Zoom (`F2`): performs a single zoom click/marquee and returns to the previous tool.
    pub fn activate_one_shot_zoom(&mut self) {
        if self.active_tool != ToolId::Zoom {
            self.one_shot_zoom_return_tool = Some(self.active_tool);
        }
        self.active_tool = ToolId::Zoom;
        self.group_active_tools[ToolId::Zoom.group_index()] = ToolId::Zoom;
        self.open_flyout_group = None;
        if let Some(tab) = self.active_tab_mut() {
            tab.tool_core.set_active_tool(ToolId::Zoom);
            tab.transient_overlay = TransientOverlay::default();
        }
    }

    /// Completes a one-shot `F2` zoom operation by restoring the previous tool if armed.
    pub fn complete_one_shot_zoom_if_active(&mut self) {
        if let Some(return_tool) = self.one_shot_zoom_return_tool.take() {
            self.select_tool(return_tool);
        }
    }

    /// Zooms viewport tightly to the currently selected object(s) (`Shift+F2`), falling back to Fit All.
    pub fn zoom_to_selected(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if let Some(bb) = tab.selection.bounding_box(&tab.document) {
                tab.viewport.fit_rect(bb.expand(4.0), 40.0);
                tab.needs_rerender = true;
            } else {
                tab.fit_all_objects();
            }
        }
    }

    /// Applies a built-in [`WorkspacePreset`] layout, configuring dockers and toolbox mode.
    pub fn apply_workspace_preset(&mut self, preset: WorkspacePreset) {
        self.active_workspace_preset = preset;
        match preset {
            WorkspacePreset::DefaultClassic => {
                self.dock_state = default_dock_state();
                self.toolbox_two_column = false;
                self.show_dockers = true;
            }
            WorkspacePreset::Lite => {
                self.dock_state = DockState::new(vec![DockerTab::Properties, DockerTab::Objects]);
                self.toolbox_two_column = false;
                self.show_dockers = true;
            }
            WorkspacePreset::Illustration => {
                let mut state = DockState::new(vec![
                    DockerTab::Properties,
                    DockerTab::Effects,
                    DockerTab::Lens,
                    DockerTab::SymbolsStyles,
                ]);
                state.main_surface_mut().split_below(
                    NodeIndex::root(),
                    0.58,
                    vec![
                        DockerTab::Objects,
                        DockerTab::History,
                        DockerTab::BitmapStudio,
                    ],
                );
                self.dock_state = state;
                self.toolbox_two_column = false;
                self.show_dockers = true;
            }
            WorkspacePreset::Illustrator => {
                let mut state = DockState::new(vec![
                    DockerTab::Properties,
                    DockerTab::Objects,
                    DockerTab::Effects,
                    DockerTab::SymbolsStyles,
                ]);
                state.main_surface_mut().split_below(
                    NodeIndex::root(),
                    0.56,
                    vec![DockerTab::TransformAlignShape, DockerTab::History],
                );
                self.dock_state = state;
                self.toolbox_two_column = true;
                self.show_dockers = true;
                self.set_shortcut_profile(ShortcutProfile::StandardVector);
            }
            WorkspacePreset::PageLayout => {
                let mut state = DockState::new(vec![
                    DockerTab::Objects,
                    DockerTab::Properties,
                    DockerTab::FontManager,
                    DockerTab::FindReplace,
                ]);
                state.main_surface_mut().split_below(
                    NodeIndex::root(),
                    0.55,
                    vec![DockerTab::Production, DockerTab::History],
                );
                self.dock_state = state;
                self.toolbox_two_column = false;
                self.show_dockers = true;
            }
            WorkspacePreset::LaserSignStudio => {
                let mut state = DockState::new(vec![
                    DockerTab::Production,
                    DockerTab::Objects,
                    DockerTab::Properties,
                ]);
                state.main_surface_mut().split_below(
                    NodeIndex::root(),
                    0.56,
                    vec![
                        DockerTab::TransformAlignShape,
                        DockerTab::CdrInspector,
                        DockerTab::Macros,
                    ],
                );
                self.dock_state = state;
                self.toolbox_two_column = false;
                self.show_dockers = true;
            }
            WorkspacePreset::TouchCompact => {
                self.dock_state = DockState::new(vec![
                    DockerTab::Properties,
                    DockerTab::Objects,
                    DockerTab::Production,
                ]);
                self.toolbox_two_column = true;
                self.show_dockers = true;
            }
        }
        self.push_toast(format!("Workspace: {}", preset.label()), false);
    }

    /// Exports the active workspace configuration to `.coreews` JSON (v2 includes full
    /// dock split tree, language, active flyout group tools, and custom shortcuts).
    #[must_use]
    pub fn export_workspace_json(&self) -> String {
        let mut open_dockers = Vec::new();
        for (_, &tab) in self.dock_state.iter_all_tabs() {
            if !open_dockers.contains(&tab) {
                open_dockers.push(tab);
            }
        }
        let bundle = WorkspaceBundle {
            version: 2,
            preset: self.active_workspace_preset,
            theme: self.theme,
            shortcut_profile: self.shortcut_profile,
            toolbox_two_column: self.toolbox_two_column,
            show_dockers: self.show_dockers,
            palette_show_names: self.palette_show_names,
            treat_all_objects_as_filled: self.treat_all_objects_as_filled,
            object_origin_anchor: self.object_origin_anchor,
            open_dockers,
            ui_language: self.ui_language,
            dock_state: Some(sanitize_dock_state_for_serde(&self.dock_state)),
            group_active_tools: Some(self.group_active_tools),
            custom_shortcuts: self.custom_shortcuts.clone(),
            toolbar_config: self.toolbar_config.clone(),
        };
        serde_json::to_string_pretty(&bundle).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Imports and applies a `.coreews` workspace JSON configuration (both v1 and v2).
    pub fn import_workspace_json(&mut self, json: &str) -> Result<(), String> {
        if json.len() > 2 * 1024 * 1024 {
            return Err("workspace file exceeds 2 MiB safety limit".to_owned());
        }
        let bundle: WorkspaceBundle =
            serde_json::from_str(json).map_err(|e| format!("invalid workspace JSON: {e}"))?;
        if bundle.version == 0 || bundle.version > 2 {
            return Err(format!("unsupported workspace version {}", bundle.version));
        }
        self.apply_workspace_preset(bundle.preset);
        if let Some(dock_state) = bundle.dock_state {
            self.dock_state = dock_state;
        } else {
            for &docker in &bundle.open_dockers {
                if self.dock_state.find_tab(&docker).is_none() {
                    self.dock_state.push_to_first_leaf(docker);
                }
            }
        }
        self.toolbox_two_column = bundle.toolbox_two_column;
        self.show_dockers = bundle.show_dockers;
        self.palette_show_names = bundle.palette_show_names;
        self.treat_all_objects_as_filled = bundle.treat_all_objects_as_filled;
        self.object_origin_anchor = (
            bundle.object_origin_anchor.0.min(2),
            bundle.object_origin_anchor.1.min(2),
        );
        if let Some(tools) = bundle.group_active_tools {
            self.group_active_tools = tools;
        }
        self.toolbar_config = bundle.toolbar_config;
        self.custom_shortcuts = bundle.custom_shortcuts;
        self.set_theme(bundle.theme);
        self.set_ui_language(bundle.ui_language);
        self.set_shortcut_profile(bundle.shortcut_profile);
        self.push_toast(
            format!(
                "Imported workspace '{}'",
                self.active_workspace_preset.label()
            ),
            false,
        );
        Ok(())
    }

    fn apply_printer_scan_result(&mut self, res: Result<Vec<PrinterInfo>, String>) {
        match res {
            Ok(printers) => {
                if self.print_job_config.printer_name.is_none() {
                    if let Some(def) = printers.iter().find(|p| p.is_default).or(printers.first()) {
                        self.print_job_config.printer_name = Some(def.name.clone());
                    }
                }
                let count = printers.len();
                self.available_printers = printers;
                self.printers_status = Some(if count == 0 {
                    "No OS printers detected — install CUPS / system printer or use PDF export."
                        .to_owned()
                } else {
                    format!("Detected {count} OS print queue(s)")
                });
            }
            Err(err) => {
                self.available_printers.clear();
                self.printers_status = Some(err);
            }
        }
    }

    /// Refreshes available OS print queues synchronously (headless tests / CLI callers).
    pub fn refresh_system_printers(&mut self) {
        self.apply_printer_scan_result(list_system_printers());
    }

    /// Triggers background OS print queue discovery so opening Print Preview never blocks the UI thread.
    pub fn refresh_system_printers_async(&mut self) {
        if self.scanning_printers {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.scanning_printers = true;
        self.printer_scan_rx = Some(rx);
        self.printers_status = Some("Scanning OS print queues...".to_owned());
        if let Err(err) = std::thread::Builder::new()
            .name("coree-printer-scan".into())
            .spawn(move || {
                let _ = tx.send(list_system_printers());
            })
        {
            self.scanning_printers = false;
            self.printer_scan_rx = None;
            self.printers_status = Some(format!("Failed to start printer scan: {err}"));
        }
    }

    /// Polls any in-flight background OS printer discovery without blocking.
    pub fn poll_system_printers_async(&mut self) {
        let Some(ref rx) = self.printer_scan_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(res) => {
                self.printer_scan_rx = None;
                self.scanning_printers = false;
                self.apply_printer_scan_result(res);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.printer_scan_rx = None;
                self.scanning_printers = false;
                self.printers_status =
                    Some("Printer discovery thread stopped unexpectedly.".into());
            }
        }
    }

    /// Submits the active document to the configured native OS printer spooler (`Ctrl+P`).
    pub fn print_active_document_os(&mut self) {
        let Some(doc) = self.active_tab().map(|t| t.document.clone()) else {
            self.push_toast("No active document to print", true);
            return;
        };
        let mut cfg = self.print_job_config.clone();
        cfg.scale_pct = self.production_state.print_scale_pct.clamp(10.0, 400.0);
        match print_document_via_os_spooler(&doc, &cfg) {
            Ok(msg) => {
                self.printers_status = Some(msg.clone());
                self.push_toast(msg, false);
            }
            Err(err) => {
                self.printers_status = Some(err.clone());
                self.push_toast(format!("Print failed: {err}"), true);
            }
        }
    }

    /// Syncs soft-proofing and gamut-warning flags into every open tab's `RenderOptions`.
    pub fn sync_proof_options_to_tabs(&mut self) {
        for tab in &mut self.tabs {
            if tab.render_options.soft_proof_cmyk != self.soft_proof_cmyk
                || tab.render_options.gamut_warning != self.gamut_warning
                || tab.render_options.proof_profile != self.proof_profile
                || tab.render_options.proof_intent != self.proof_intent
            {
                tab.render_options.soft_proof_cmyk = self.soft_proof_cmyk;
                tab.render_options.gamut_warning = self.gamut_warning;
                tab.render_options.proof_profile = self.proof_profile;
                tab.render_options.proof_intent = self.proof_intent;
                tab.tile_cache.mark_all_dirty();
                tab.needs_rerender = true;
            }
        }
    }

    /// Resizes the active page tightly around its visible content with `margin_mm` padding
    /// via real `TransformObjects` + `SetPageProperties` `CommandOp`s (100% undoable & WAL-journaled).
    pub fn autofit_active_page(&mut self, margin_mm: f64) {
        let plan: Result<(Vec<CommandOp>, f64, f64), String> = (|| {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let doc = &tab.document;
            let page_idx = doc.active_page_index();
            let page = doc
                .page(page_idx)
                .ok_or_else(|| "active page out of range".to_owned())?;
            let page_id = page.id;
            let margin = if margin_mm.is_finite() {
                margin_mm.clamp(0.0, 5_000.0)
            } else {
                0.0
            };

            let mut visible_ids = Vec::new();
            let mut all_page_obj_ids = Vec::new();
            let mut seen = HashSet::new();
            for layer in &page.layers {
                for &oid in &layer.objects {
                    if seen.insert(oid) {
                        all_page_obj_ids.push(oid);
                        if layer.visible {
                            if let Some(obj) = doc.object(oid) {
                                if obj.visible {
                                    visible_ids.push(oid);
                                }
                            }
                        }
                    }
                }
            }

            let bounds = doc
                .combined_bounds(&visible_ids)
                .ok_or_else(|| "page has no visible content to autofit".to_owned())?;
            if !bounds.is_finite() {
                return Err("visible content bounds are non-finite".to_owned());
            }

            let dx = margin - bounds.min_x;
            let dy = margin - bounds.min_y;
            let new_w = (bounds.width() + 2.0 * margin).clamp(1.0, 50_000.0);
            let new_h = (bounds.height() + 2.0 * margin).clamp(1.0, 50_000.0);

            let mut ops = Vec::new();
            if !all_page_obj_ids.is_empty() && (dx.abs() > 1e-9 || dy.abs() > 1e-9) {
                ops.push(CommandOp::TransformObjects {
                    ids: all_page_obj_ids,
                    affine: Affine2D::translate(dx, dy),
                });
            }
            ops.push(CommandOp::SetPageProperties {
                page_id,
                name: None,
                width_mm: Some(new_w),
                height_mm: Some(new_h),
                bleed_mm: None,
                facing_pages: None,
                background_fill: None,
                printable_background: None,
            });
            Ok((ops, new_w, new_h))
        })();

        match plan {
            Ok((ops, w, h)) => {
                let cmd = if ops.len() == 1 {
                    ops.into_iter().next().expect("len == 1")
                } else {
                    CommandOp::Batch(ops)
                };
                if let Err(err) = self.execute_command(cmd) {
                    self.push_toast(format!("Autofit Page failed: {err}"), true);
                    return;
                }
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.fit_active_page();
                }
                self.checkpoint_wal_snapshot_active();
                self.push_toast(
                    format!("Autofit page to {w:.1} × {h:.1} mm (margin {margin_mm:.1} mm)"),
                    false,
                );
            }
            Err(err) => {
                if err != "no active document" {
                    self.push_toast(format!("Autofit Page failed: {err}"), true);
                }
            }
        }
    }

    /// Classic Studio `Space` behavior: toggles cleanly between `Pick` and the previous active tool.
    pub fn toggle_pick_space(&mut self) {
        if self.active_tool == ToolId::Pick {
            let prev = self.previous_non_pick_tool;
            self.select_tool(prev);
        } else {
            self.select_tool(ToolId::Pick);
        }
    }

    /// Focuses `docker` in `egui_dock` (adding it to the main surface if it was closed).
    pub fn focus_docker(&mut self, docker: DockerTab) {
        self.show_dockers = true;
        if let Some(tab_path) = self.dock_state.find_tab(&docker) {
            let _ = self.dock_state.set_active_tab(tab_path);
        } else {
            self.dock_state.push_to_first_leaf(docker);
        }
    }

    /// Toggles `docker` open/closed inside `egui_dock`.
    pub fn toggle_docker(&mut self, docker: DockerTab) {
        if let Some(tab_path) = self.dock_state.find_tab(&docker) {
            self.dock_state.remove_tab(tab_path);
        } else {
            self.show_dockers = true;
            if self.dock_state.main_surface().is_empty() {
                self.dock_state = DockState::new(vec![docker]);
            } else {
                self.dock_state.set_focused_node_and_surface(NodePath {
                    surface: SurfaceIndex::main(),
                    node: NodeIndex::root(),
                });
                self.dock_state.push_to_focused_leaf(docker);
            }
        }
    }

    /// Switches the active studio theme and updates every open tab's pasteboard background.
    pub fn set_theme(&mut self, theme: StudioTheme) {
        self.theme = theme;
        self.theme_dirty = true;
        let bg = theme.tokens().canvas_well_core_color();
        for tab in &mut self.tabs {
            tab.render_options.canvas_bg = bg.clone();
            tab.needs_rerender = true;
            tab.tile_cache.mark_all_dirty();
        }
        if let Some(ref db) = self.storage {
            let _ = db.set_preference("ui.theme", "ui", &theme);
        }
    }

    /// Switches the active keyboard shortcut profile (`ClassicStudio` / `StandardVector`).
    pub fn set_shortcut_profile(&mut self, profile: ShortcutProfile) {
        self.shortcut_profile = profile;
        if let Some(ref db) = self.storage {
            let _ = db.set_preference("ui.shortcut_profile", "ui", &profile);
        }
        self.push_toast(format!("Shortcut profile: {}", profile.label()), false);
    }

    /// Switches UI studio language (`English` / `العربية`) and persists locally.
    pub fn set_ui_language(&mut self, language: StudioLanguage) {
        if self.ui_language != language {
            self.fonts_configured = false;
        }
        self.ui_language = language;
        if let Some(ref db) = self.storage {
            let _ = db.set_preference("ui.language", "ui", &language);
        }
        self.push_toast(
            match language {
                StudioLanguage::English => "UI Language: English",
                StudioLanguage::Arabic => "لغة الواجهة: العربية",
            },
            false,
        );
    }

    /// Opens the classic modal Edit Fill dialog (`F11`) preloaded from the active selection.
    pub fn open_edit_fill_dialog(&mut self) {
        let initial = self
            .active_tab()
            .and_then(|t| {
                let id = t.selection.objects.first()?;
                t.document.object(*id).map(|o| o.fill.clone())
            })
            .unwrap_or_else(|| self.default_fill.clone());
        self.edit_fill_draft = initial;
        self.edit_fill_selected_stop = 0;
        self.show_edit_fill_dialog = true;
    }

    /// Applies the edited fill draft from `Edit Fill (F11)` onto selected objects or default fill.
    pub fn apply_edit_fill_draft(&mut self) {
        let fill = self.edit_fill_draft.clone();
        self.apply_fill(fill);
    }

    /// Opens the classic modal Outline Pen dialog (`F12`) preloaded from the active selection.
    pub fn open_outline_pen_dialog(&mut self) {
        let initial = self
            .active_tab()
            .and_then(|t| {
                let id = t.selection.objects.first()?;
                t.document.object(*id).map(|o| o.outline.clone())
            })
            .unwrap_or_else(|| self.default_outline.clone());
        self.outline_pen_draft = initial;
        self.show_outline_pen_dialog = true;
    }

    /// Applies the edited outline draft from `Outline Pen (F12)` onto selected objects or default outline.
    pub fn apply_outline_pen_draft(&mut self) {
        let outline = self.outline_pen_draft.clone();
        self.apply_outline(outline);
    }

    /// Executes a resolved [`ShortcutAction`] cleanly (used both by live egui input and headless tests).
    pub fn handle_shortcut(&mut self, action: ShortcutAction) {
        match action {
            ShortcutAction::TogglePickSpace => self.toggle_pick_space(),
            ShortcutAction::SelectPick => self.select_tool(ToolId::Pick),
            ShortcutAction::SelectShapeNode => self.select_tool(ToolId::ShapeNode),
            ShortcutAction::SelectZoom => self.select_tool(ToolId::Zoom),
            ShortcutAction::SelectOneShotZoom => self.activate_one_shot_zoom(),
            ShortcutAction::SelectPan => self.select_tool(ToolId::Pan),
            ShortcutAction::SelectFreehand => self.select_tool(ToolId::Freehand),
            ShortcutAction::SelectPen => self.select_tool(ToolId::Pen),
            ShortcutAction::SelectRectangle => self.select_tool(ToolId::Rectangle),
            ShortcutAction::SelectEllipse => self.select_tool(ToolId::Ellipse),
            ShortcutAction::SelectPolygon => self.select_tool(ToolId::Polygon),
            ShortcutAction::SelectSpiral => self.select_tool(ToolId::Spiral),
            ShortcutAction::SelectGraphPaper => self.select_tool(ToolId::GraphPaper),
            ShortcutAction::SelectArtisticText => self.select_tool(ToolId::ArtisticText),
            ShortcutAction::SelectInteractiveFill => self.select_tool(ToolId::InteractiveFill),
            ShortcutAction::SelectMeshFill => self.select_tool(ToolId::MeshFill),
            ShortcutAction::SelectEyedropper => self.select_tool(ToolId::ColorEyedropper),
            ShortcutAction::SelectTool(tool) => self.select_tool(tool),
            ShortcutAction::OpenEditFill => self.open_edit_fill_dialog(),
            ShortcutAction::FocusOutlinePen => self.open_outline_pen_dialog(),
            ShortcutAction::ZoomIn => self.zoom_step(1.25),
            ShortcutAction::ZoomOut => self.zoom_step(0.8),
            ShortcutAction::ZoomToAll => {
                if let Some(t) = self.active_tab_mut() {
                    t.fit_all_objects();
                }
            }
            ShortcutAction::ZoomToPage => {
                if let Some(t) = self.active_tab_mut() {
                    t.fit_active_page();
                }
            }
            ShortcutAction::ZoomToSelected => self.zoom_to_selected(),
            ShortcutAction::ZoomActual => {
                if let Some(t) = self.active_tab_mut() {
                    t.viewport.zoom = 1.0;
                    t.needs_rerender = true;
                }
            }
            ShortcutAction::ToggleFullScreenPreview => {
                self.full_screen_preview = !self.full_screen_preview;
                if self.full_screen_preview {
                    if let Some(tab) = self.active_tab_mut() {
                        tab.cancel_active_drag();
                    }
                }
                self.push_toast(
                    if self.full_screen_preview {
                        "Full-Screen Preview (press Esc or F9 to exit)"
                    } else {
                        "Exited Full-Screen Preview"
                    },
                    false,
                );
            }
            ShortcutAction::PrevPage => {
                if let Some(t) = self.active_tab_mut() {
                    let cur = t.document.active_page_index();
                    if cur > 0 {
                        let _ = t.document.set_active_page_index(cur - 1);
                        t.selection.clear();
                        t.sync_active_layer();
                        t.needs_rerender = true;
                    }
                }
            }
            ShortcutAction::NextPage => {
                if let Some(t) = self.active_tab_mut() {
                    let cur = t.document.active_page_index();
                    if cur + 1 < t.document.pages().len() {
                        let _ = t.document.set_active_page_index(cur + 1);
                        t.selection.clear();
                        t.sync_active_layer();
                        t.needs_rerender = true;
                    }
                }
            }
            ShortcutAction::NewDocument => self.new_doc_dialog.open = true,
            ShortcutAction::OpenDocument => self.open_file_dialog(),
            ShortcutAction::SaveDocument => {
                if self
                    .active_tab()
                    .and_then(|t| t.source_path.as_ref())
                    .is_some()
                {
                    if let Err(e) = self.save_active(None) {
                        self.push_toast(e, true);
                    }
                } else {
                    self.save_as_file_dialog();
                }
            }
            ShortcutAction::SaveDocumentAs => self.save_as_file_dialog(),
            ShortcutAction::ImportFile => self.import_file_dialog(),
            ShortcutAction::ExportFile => self.export_dialog.open = true,
            ShortcutAction::PrintDocument => {
                self.show_print_preview_dialog = true;
                self.refresh_system_printers_async();
            }
            ShortcutAction::ToggleCommandPalette => {
                self.command_palette.open = !self.command_palette.open;
                if self.command_palette.open {
                    self.command_palette.query.clear();
                    self.command_palette.selected_index = 0;
                    self.command_palette.just_opened = true;
                }
            }
            ShortcutAction::OpenFindReplace => self.focus_docker(DockerTab::FindReplace),
            ShortcutAction::OpenOptions => {
                self.show_options_dialog = true;
                self.show_preferences_dialog = true;
            }
            ShortcutAction::ToggleDockerObjects => self.toggle_docker(DockerTab::Objects),
            ShortcutAction::ToggleDockerProperties => self.toggle_docker(DockerTab::Properties),
            ShortcutAction::ToggleDockerLens => self.toggle_docker(DockerTab::Lens),
            ShortcutAction::Undo => {
                let _ = self.undo();
            }
            ShortcutAction::Redo => {
                let _ = self.redo();
            }
            ShortcutAction::Cut => {
                self.cut_selection();
            }
            ShortcutAction::Copy => {
                self.copy_selection();
            }
            ShortcutAction::Paste => {
                self.paste_clipboard();
            }
            ShortcutAction::Duplicate => {
                let _ = self.duplicate_selection();
            }
            ShortcutAction::DuplicateInPlace => {
                let _ = self.duplicate_selection_with_offset(Vec2D::ZERO);
            }
            ShortcutAction::RepeatLastCommand => self.repeat_last_command(),
            ShortcutAction::DeleteSelection => {
                // If actively drafting a Bezier/Pen/BSpline/Polyline path, pop the last node
                let popped_draft = self
                    .active_tab_mut()
                    .is_some_and(|tab| tab.tool_core.pop_last_draft_point());
                if popped_draft {
                    return;
                }
                // In ShapeNode mode with selected Bezier nodes, Delete removes those nodes!
                if self.active_tool == ToolId::ShapeNode
                    && self
                        .active_tab()
                        .is_some_and(|t| !t.selection.node_selection.is_empty())
                {
                    self.shape_delete_selected_nodes();
                } else {
                    self.delete_selection();
                }
            }
            ShortcutAction::SelectAll => {
                if self.active_tool == ToolId::ShapeNode
                    && self.active_tab().is_some_and(|t| !t.selection.is_empty())
                {
                    let n = self.shape_select_all_nodes();
                    if n > 0 {
                        self.push_toast(format!("Selected all {n} nodes"), false);
                        return;
                    }
                }
                self.select_all();
            }
            ShortcutAction::EscapeCancel => {
                self.open_flyout_group = None;
                if self.full_screen_preview {
                    self.full_screen_preview = false;
                } else if self.show_edit_fill_dialog {
                    self.show_edit_fill_dialog = false;
                } else if self.show_outline_pen_dialog {
                    self.show_outline_pen_dialog = false;
                } else if self.command_palette.open {
                    self.command_palette.open = false;
                } else if self.show_options_dialog {
                    self.show_options_dialog = false;
                    self.show_preferences_dialog = false;
                } else if self.new_doc_dialog.open {
                    self.new_doc_dialog.open = false;
                } else if self.export_dialog.open {
                    self.export_dialog.open = false;
                } else if self
                    .active_tab()
                    .is_some_and(|t| t.inline_text_edit.is_some())
                {
                    self.cancel_inline_text_edit();
                } else if let Some(tab) = self.active_tab_mut() {
                    let cleared_draft = tab.tool_core.has_active_draft_or_drag()
                        || tab.pending_crop_rect.is_some()
                        || tab.raster_mask_rect_mm.is_some()
                        || tab.raster_mask.is_some();
                    tab.cancel_active_drag();
                    let _ = tab.tool_core.cancel_all();
                    tab.pending_crop_rect = None;
                    tab.raster_mask_rect_mm = None;
                    tab.raster_mask = None;
                    if !cleared_draft {
                        if !tab.selection.is_empty() {
                            tab.selection.clear();
                            tab.metrics_dirty = true;
                        } else if tab.powerclip_edit_id.is_some() {
                            tab.powerclip_edit_id = None;
                            tab.needs_rerender = true;
                        }
                    }
                }
            }
            ShortcutAction::Group => self.group_selection(),
            ShortcutAction::Ungroup => self.ungroup_selection(),
            ShortcutAction::ConvertToCurves => self.convert_selection_to_curves(),
            ShortcutAction::ConvertOutlineToObject => self.convert_outline_to_object_selection(),
            ShortcutAction::BreakApart => self.break_apart_selection(),
            ShortcutAction::Combine => self.apply_boolean_op(BooleanOp::Combine),
            ShortcutAction::TogglePowerClipLock => self.set_powerclip_lock_selected(None),
            ShortcutAction::MoveToDesktop => self.move_selection_to_desktop_layer(),
            ShortcutAction::ZOrder(op) => self.apply_z_order(op),
            ShortcutAction::Align(op) => self.apply_align(op),
            ShortcutAction::Nudge {
                dx_steps,
                dy_steps,
                mult,
            } => self.nudge_selection(dx_steps, dy_steps, mult),
            ShortcutAction::MicroNudge { dx_steps, dy_steps } => {
                self.micro_nudge_selection(dx_steps, dy_steps);
            }
        }
    }

    /// Zooms the active viewport around its center by `factor`.
    pub fn zoom_step(&mut self, factor: f64) {
        if let Some(tab) = self.active_tab_mut() {
            let center = Point2D::new(
                f64::from(tab.viewport.width_px) * 0.5,
                f64::from(tab.viewport.height_px) * 0.5,
            );
            tab.viewport.zoom_at_screen_point(center, factor);
            tab.needs_rerender = true;
        }
    }

    // -------------------------------------------------------------------------
    // Native `rfd` xdg-portal / Win32 File Dialog Triggers
    // -------------------------------------------------------------------------
}
