// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

impl StudioApp {
    /// Starts recording subsequent commands into a Rhai macro.
    pub fn start_macro_recording(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.history.start_macro_recording();
            self.push_toast("Recording Rhai Macro...", false);
        }
    }

    /// Stops macro recording and populates the Macro Docker editor with the generated Rhai script.
    pub fn stop_macro_recording(&mut self) -> String {
        let script = self
            .active_tab_mut()
            .map(|tab| tab.history.stop_macro_to_rhai())
            .unwrap_or_default();
        if !script.is_empty() {
            self.macro_state.script_source = script.clone();
            self.focus_docker(DockerTab::Macros);
            self.push_toast("Macro recorded to Rhai script", false);
        }
        script
    }

    /// Executes `script` inside `coree_prod::macros` strict sandbox against the active document.
    pub fn run_rhai_script(&mut self, script: &str) -> Result<usize, String> {
        let planned = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let cfg = MacroSandboxConfig::default();
            if script.contains("// coree recorded macro") {
                plan_recorded_macro_ops(script, &cfg)
            } else {
                plan_macro_ops_with_selection(
                    &tab.document,
                    script,
                    &BTreeMap::new(),
                    &cfg,
                    &tab.selection.objects,
                )
            }
        };
        match planned {
            Ok((exec, ops)) => {
                if !ops.is_empty() {
                    let cmd = if ops.len() == 1 {
                        ops.into_iter().next().expect("len == 1")
                    } else {
                        CommandOp::Batch(ops)
                    };
                    if let Err(err_str) = self.execute_command(cmd) {
                        self.macro_state.last_error = Some(err_str.clone());
                        self.push_toast(format!("Macro error: {err_str}"), true);
                        return Err(err_str);
                    }
                    if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                        tab.sync_active_layer();
                        tab.needs_rerender = true;
                    }
                    self.checkpoint_wal_snapshot_active();
                }
                self.macro_state.output_logs = exec.logs;
                self.macro_state.last_error = None;
                self.push_toast(
                    format!("Macro ran ({} command(s))", exec.commands_executed),
                    false,
                );
                Ok(exec.commands_executed)
            }
            Err(e) => {
                let err_str = e.to_string();
                self.macro_state.last_error = Some(err_str.clone());
                self.push_toast(format!("Macro error: {err_str}"), true);
                Err(err_str)
            }
        }
    }

    /// Imports a `.gpl`, `.ase`, or `.pal` palette file into `custom_palettes`.
    pub fn import_palette_from_path(&mut self, path: &Path) -> Result<String, String> {
        let pal = import_palette_file(path).map_err(|e| format!("palette import failed: {e}"))?;
        let name = pal.name.clone();
        self.custom_palettes.push(pal);
        self.active_palette_index = 3 + self.custom_palettes.len();
        self.push_toast(format!("Imported palette '{name}'"), false);
        Ok(name)
    }

    // -------------------------------------------------------------------------
    // Tool Switching, Dockers, Theme, Shortcuts & Command Palette
    // -------------------------------------------------------------------------

    /// Opens a native OS file picker and opens the chosen document.
    /// Includes both lowercase and uppercase extensions for Linux portal & Windows safety,
    /// plus an `"All Files"` fallback filter so valid files are never hidden.
    pub fn open_file_dialog(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Open Document")
            .add_filter(
                "All Supported Studio Formats",
                &[
                    "coree", "COREE", "cdr", "CDR", "cdt", "CDT", "cmx", "CMX", "cpt", "CPT",
                    "pdf", "PDF", "ai", "AI", "eps", "EPS", "ps", "PS", "dxf", "DXF", "dwg", "DWG",
                    "wmf", "WMF", "emf", "EMF", "plt", "PLT", "psd", "PSD", "psb", "PSB", "svg",
                    "SVG", "svgz", "SVGZ", "png", "PNG", "jpg", "JPG", "jpeg", "JPEG", "webp",
                    "WEBP", "tiff", "TIFF", "tif", "TIF", "bmp", "BMP", "zip", "ZIP",
                ],
            )
            .add_filter("Native coree (.coree)", &["coree", "COREE"])
            .add_filter(
                "CDR / CDT / CMX / CPT (.cdr, .cdr.zip)",
                &[
                    "cdr", "CDR", "cdt", "CDT", "cmx", "CMX", "cpt", "CPT", "zip", "ZIP",
                ],
            )
            .add_filter("PDF & Vector AI (.pdf, .ai)", &["pdf", "PDF", "ai", "AI"])
            .add_filter("PostScript / EPS (.eps, .ps)", &["eps", "EPS", "ps", "PS"])
            .add_filter(
                "CAD / Plotter (.dxf, .dwg, .plt)",
                &["dxf", "DXF", "dwg", "DWG", "plt", "PLT"],
            )
            .add_filter(
                "Windows Metafiles (.wmf, .emf)",
                &["wmf", "WMF", "emf", "EMF"],
            )
            .add_filter("SVG Vector (.svg, .svgz)", &["svg", "SVG", "svgz", "SVGZ"])
            .add_filter(
                "Layered Raster / PSD (.psd, .psb)",
                &["psd", "PSD", "psb", "PSB"],
            )
            .add_filter(
                "Raster Images (.png, .jpg, .webp, .tif, .bmp)",
                &[
                    "png", "PNG", "jpg", "JPG", "jpeg", "JPEG", "webp", "WEBP", "tiff", "TIFF",
                    "tif", "TIF", "bmp", "BMP",
                ],
            )
            .add_filter("All Files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            if let Err(err) = self.open_file_path(&path) {
                self.push_toast(err, true);
            }
        }
    }

    /// Opens a native OS file picker and imports the chosen file into the active page.
    /// Includes all supported vector, CAD, PDF/EPS, PSD, CDR, and raster extensions (lower + upper)
    /// plus `"All Files"` fallback.
    pub fn import_file_dialog(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Import into Active Document")
            .add_filter(
                "All Importable Formats",
                &[
                    "coree", "COREE", "cdr", "CDR", "cdt", "CDT", "cmx", "CMX", "cpt", "CPT",
                    "pdf", "PDF", "ai", "AI", "eps", "EPS", "ps", "PS", "dxf", "DXF", "dwg", "DWG",
                    "wmf", "WMF", "emf", "EMF", "plt", "PLT", "psd", "PSD", "psb", "PSB", "svg",
                    "SVG", "svgz", "SVGZ", "png", "PNG", "jpg", "JPG", "jpeg", "JPEG", "webp",
                    "WEBP", "tiff", "TIFF", "tif", "TIF", "bmp", "BMP", "zip", "ZIP",
                ],
            )
            .add_filter(
                "CDR / CDT / CMX / CPT (.cdr, .cdr.zip)",
                &[
                    "cdr", "CDR", "cdt", "CDT", "cmx", "CMX", "cpt", "CPT", "zip", "ZIP",
                ],
            )
            .add_filter("PDF & Vector AI (.pdf, .ai)", &["pdf", "PDF", "ai", "AI"])
            .add_filter("PostScript / EPS (.eps, .ps)", &["eps", "EPS", "ps", "PS"])
            .add_filter(
                "CAD / Plotter (.dxf, .dwg, .plt)",
                &["dxf", "DXF", "dwg", "DWG", "plt", "PLT"],
            )
            .add_filter(
                "Windows Metafiles (.wmf, .emf)",
                &["wmf", "WMF", "emf", "EMF"],
            )
            .add_filter("SVG Vector (.svg, .svgz)", &["svg", "SVG", "svgz", "SVGZ"])
            .add_filter(
                "Layered Raster / PSD (.psd, .psb)",
                &["psd", "PSD", "psb", "PSB"],
            )
            .add_filter(
                "Raster Images (.png, .jpg, .webp, .tif, .bmp)",
                &[
                    "png", "PNG", "jpg", "JPG", "jpeg", "JPEG", "webp", "WEBP", "tiff", "TIFF",
                    "tif", "TIF", "bmp", "BMP",
                ],
            )
            .add_filter("Native coree (.coree)", &["coree", "COREE"])
            .add_filter("All Files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            if let Err(err) = self.import_file_path(&path) {
                self.push_toast(err, true);
            }
        }
    }

    /// Opens a native OS Save As dialog for `.coree` / `.cdr` / `.cdt` / `.svg`.
    pub fn save_as_file_dialog(&mut self) {
        let default_name = self
            .active_tab()
            .map(|t| format!("{}.coree", t.document.title()))
            .unwrap_or_else(|| "Untitled.coree".to_owned());
        let picked = rfd::FileDialog::new()
            .set_title("Save Document As")
            .set_file_name(&default_name)
            .add_filter("Native coree", &["coree", "COREE"])
            .add_filter("CDR Archive", &["cdr", "CDR"])
            .add_filter("CDT Template", &["cdt", "CDT"])
            .add_filter("SVG Vector", &["svg", "SVG"])
            .add_filter("All Files", &["*"])
            .save_file();
        if let Some(path) = picked {
            if let Err(err) = self.save_active_as(&path) {
                self.push_toast(err, true);
            }
        }
    }

    /// Saves a non-destructive numbered backup copy (`_v1`, `_v2`, ...) beside the active file.
    pub fn save_incremental_copy(&mut self) -> Result<PathBuf, String> {
        let src = {
            let tab = self
                .active_tab()
                .ok_or_else(|| "no active document".to_owned())?;
            tab.source_path
                .clone()
                .ok_or_else(|| "document has not been saved yet; use Save As first".to_owned())?
        };
        let target = next_incremental_path(&src);
        self.save_active_as(&target)?;
        Ok(target)
    }

    // -------------------------------------------------------------------------
    // Command Palette Registry & Fuzzy Search
    // -------------------------------------------------------------------------

    /// Builds the complete list of searchable Command Palette actions.
    #[must_use]
    pub fn all_palette_commands(&self) -> Vec<PaletteCommand> {
        let p = self.shortcut_profile;
        let mut list = vec![
            PaletteCommand {
                id: "file.new".into(),
                title: "New Document...".into(),
                category: "File".into(),
                shortcut: shortcut_label(p, ShortcutAction::NewDocument).into(),
            },
            PaletteCommand {
                id: "file.sample".into(),
                title: "Open Showcase Sample Document".into(),
                category: "File".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "file.open".into(),
                title: "Open File...".into(),
                category: "File".into(),
                shortcut: shortcut_label(p, ShortcutAction::OpenDocument).into(),
            },
            PaletteCommand {
                id: "file.save".into(),
                title: "Save Document".into(),
                category: "File".into(),
                shortcut: shortcut_label(p, ShortcutAction::SaveDocument).into(),
            },
            PaletteCommand {
                id: "file.save_incremental".into(),
                title: "Save Incremental Copy (_vN)".into(),
                category: "File".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "file.export".into(),
                title: "Export Document...".into(),
                category: "File".into(),
                shortcut: shortcut_label_for(p, &self.custom_shortcuts, ShortcutAction::ExportFile),
            },
            PaletteCommand {
                id: "file.print".into(),
                title: "Print Document (Native OS Spooler)...".into(),
                category: "File".into(),
                shortcut: shortcut_label_for(
                    p,
                    &self.custom_shortcuts,
                    ShortcutAction::PrintDocument,
                ),
            },
            PaletteCommand {
                id: "edit.undo".into(),
                title: "Undo".into(),
                category: "Edit".into(),
                shortcut: shortcut_label(p, ShortcutAction::Undo).into(),
            },
            PaletteCommand {
                id: "edit.redo".into(),
                title: "Redo".into(),
                category: "Edit".into(),
                shortcut: shortcut_label(p, ShortcutAction::Redo).into(),
            },
            PaletteCommand {
                id: "edit.duplicate".into(),
                title: "Duplicate Selection".into(),
                category: "Edit".into(),
                shortcut: shortcut_label(p, ShortcutAction::Duplicate).into(),
            },
            PaletteCommand {
                id: "object.group".into(),
                title: "Group Selection".into(),
                category: "Object".into(),
                shortcut: shortcut_label(p, ShortcutAction::Group).into(),
            },
            PaletteCommand {
                id: "object.ungroup".into(),
                title: "Ungroup Selection".into(),
                category: "Object".into(),
                shortcut: shortcut_label(p, ShortcutAction::Ungroup).into(),
            },
            PaletteCommand {
                id: "object.curves".into(),
                title: "Convert to Curves".into(),
                category: "Object".into(),
                shortcut: shortcut_label(p, ShortcutAction::ConvertToCurves).into(),
            },
            PaletteCommand {
                id: "object.convert_outline_to_object".into(),
                title: "Convert Outline to Object".into(),
                category: "Object".into(),
                shortcut: shortcut_label(p, ShortcutAction::ConvertOutlineToObject).into(),
            },
            PaletteCommand {
                id: "object.powerclip.create".into(),
                title: "PowerClip: Place Inside Frame".into(),
                category: "Object".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "object.powerclip.extract".into(),
                title: "PowerClip: Extract Contents".into(),
                category: "Object".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "object.powerclip.lock".into(),
                title: "PowerClip: Toggle Lock Contents".into(),
                category: "Object".into(),
                shortcut: shortcut_label_for(
                    p,
                    &self.custom_shortcuts,
                    ShortcutAction::TogglePowerClipLock,
                ),
            },
            PaletteCommand {
                id: "object.desktop_layer".into(),
                title: "Move Selection to Cross-Page Desktop Layer".into(),
                category: "Object".into(),
                shortcut: shortcut_label_for(
                    p,
                    &self.custom_shortcuts,
                    ShortcutAction::MoveToDesktop,
                ),
            },
            PaletteCommand {
                id: "prod.cutcontour".into(),
                title: "One-Click CutContour Spot Hairline".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "prod.preflight".into(),
                title: "Run Print Preflight Audit".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "prod.separations".into(),
                title: "Compute CMYK & Spot Separations".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "prod.printmarks".into(),
                title: "Generate Crop & Registration Marks".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.multipage".into(),
                title: "Toggle Multipage View".into(),
                category: "View".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.wireframe".into(),
                title: "View Mode: Wireframe".into(),
                category: "View".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.enhanced".into(),
                title: "View Mode: Enhanced".into(),
                category: "View".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.zoom_all".into(),
                title: "Zoom to All Objects".into(),
                category: "View".into(),
                shortcut: shortcut_label(p, ShortcutAction::ZoomToAll).into(),
            },
            PaletteCommand {
                id: "view.zoom_page".into(),
                title: "Zoom to Active Page".into(),
                category: "View".into(),
                shortcut: shortcut_label(p, ShortcutAction::ZoomToPage).into(),
            },
            PaletteCommand {
                id: "text.fit_path".into(),
                title: "Fit Text to Path".into(),
                category: "Text".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "table.insert".into(),
                title: "Insert Vector Table (3x3)".into(),
                category: "Table".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "bitmap.trace".into(),
                title: "Vector Trace Selected Bitmap".into(),
                category: "Bitmaps".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "bitmap.convert".into(),
                title: "Convert Selection to Bitmap".into(),
                category: "Bitmaps".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "theme.dark".into(),
                title: "Theme: Dark Studio".into(),
                category: "Appearance".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "theme.light".into(),
                title: "Theme: Classic Light".into(),
                category: "Appearance".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "theme.contrast".into(),
                title: "Theme: High Contrast".into(),
                category: "Appearance".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "profile.classic".into(),
                title: "Shortcuts: Classic Studio".into(),
                category: "Shortcuts".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "profile.standard".into(),
                title: "Shortcuts: Standard Vector".into(),
                category: "Shortcuts".into(),
                shortcut: String::new(),
            },
        ];

        // All 9 Boolean Shaping operations
        for (id_suffix, title) in [
            ("weld", "Shaping: Weld"),
            ("trim", "Shaping: Trim"),
            ("intersect", "Shaping: Intersect"),
            ("simplify", "Shaping: Simplify"),
            ("front_minus_back", "Shaping: Front Minus Back"),
            ("back_minus_front", "Shaping: Back Minus Front"),
            ("boundary", "Shaping: Create Boundary"),
            ("combine", "Shaping: Combine"),
            ("break_apart", "Shaping: Break Apart"),
        ] {
            list.push(PaletteCommand {
                id: format!("bool.{id_suffix}"),
                title: title.into(),
                category: "Shaping".into(),
                shortcut: String::new(),
            });
        }

        // All 11 Lenses
        for (idx, name) in LensDockerState::NAMES.iter().enumerate() {
            list.push(PaletteCommand {
                id: format!("lens.{idx}"),
                title: format!("Apply Lens: {name}"),
                category: "Lens".into(),
                shortcut: String::new(),
            });
        }

        // All 14 Dockers
        for docker in DockerTab::ALL {
            list.push(PaletteCommand {
                id: format!("docker.{}", docker.title()),
                title: format!("Show Docker: {}", docker.title()),
                category: "Window".into(),
                shortcut: String::new(),
            });
        }

        // All Tools across all 15 Toolbox Flyout groups
        for group in TOOL_GROUPS {
            for &tool in group {
                let sc = crate::state::tool_shortcut_action(tool)
                    .map(|a| shortcut_label(p, a))
                    .unwrap_or("");
                list.push(PaletteCommand {
                    id: format!("tool.{tool:?}"),
                    title: format!("Tool: {}", tool.label()),
                    category: "Toolbox".into(),
                    shortcut: sc.into(),
                });
            }
        }

        // Built-in Rhai Macros
        for m in builtin_macros() {
            list.push(PaletteCommand {
                id: format!("macro.{}", m.id),
                title: format!("Run Macro: {}", m.name),
                category: "Macros".into(),
                shortcut: String::new(),
            });
        }

        list
    }

    /// Fuzzy-filters Command Palette commands ordered by match quality.
    #[must_use]
    pub fn search_command_palette(&self, query: &str) -> Vec<PaletteCommand> {
        let q = query.trim().to_ascii_lowercase();
        let all = self.all_palette_commands();
        if q.is_empty() {
            return all;
        }
        let mut scored: Vec<(i32, PaletteCommand)> = all
            .into_iter()
            .filter_map(|cmd| {
                let hay = format!("{} {}", cmd.title, cmd.category).to_ascii_lowercase();
                fuzzy_score(&hay, &q).map(|s| (s, cmd))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
        scored.into_iter().map(|(_, c)| c).collect()
    }

    /// Executes a Command Palette action by its `id`.
    pub fn execute_palette_command(&mut self, id: &str) -> bool {
        match id {
            "file.new" => self.new_doc_dialog.open = true,
            "file.sample" => self.new_sample_document(),
            "file.open" => self.open_file_dialog(),
            "file.save" => self.handle_shortcut(ShortcutAction::SaveDocument),
            "file.save_incremental" => {
                if let Err(e) = self.save_incremental_copy() {
                    self.push_toast(e, true);
                }
            }
            "file.export" => self.export_dialog.open = true,
            "file.print" => self.handle_shortcut(ShortcutAction::PrintDocument),
            "edit.undo" => {
                let _ = self.undo();
            }
            "edit.redo" => {
                let _ = self.redo();
            }
            "edit.duplicate" => {
                let _ = self.duplicate_selection();
            }
            "object.group" => self.group_selection(),
            "object.ungroup" => self.ungroup_selection(),
            "object.curves" => self.convert_selection_to_curves(),
            "object.convert_outline_to_object" => self.convert_outline_to_object_selection(),
            "object.powerclip.create" => self.create_powerclip_from_selection(),
            "object.powerclip.extract" => self.extract_selected_powerclip(),
            "object.powerclip.lock" => self.set_powerclip_lock_selected(None),
            "object.desktop_layer" => self.move_selection_to_desktop_layer(),
            "prod.cutcontour" => self.generate_cut_contour_active(),
            "prod.preflight" => {
                self.run_preflight_active();
                self.production_state.sub_section = 0;
                self.focus_docker(DockerTab::Production);
            }
            "prod.separations" => {
                self.compute_separations_active();
                self.production_state.sub_section = 1;
                self.focus_docker(DockerTab::Production);
            }
            "prod.printmarks" => self.add_print_marks_active(),
            "view.multipage" => {
                let next = self.active_tab().map(|t| !t.document.multipage_view());
                if let Some(next) = next {
                    let _ = self.execute_command(CommandOp::SetDocumentProperties {
                        title: None,
                        color_mode: None,
                        default_unit: None,
                        dpi: None,
                        multipage_view: Some(next),
                        active_page_index: None,
                        metadata: None,
                        custom_icc_profile: None,
                        clear_custom_icc: false,
                        rgb_icc_profile: None,
                        clear_rgb_icc: false,
                        cmyk_icc_profile: None,
                        clear_cmyk_icc: false,
                        gray_icc_profile: None,
                        clear_gray_icc: false,
                        rendering_intent: None,
                        black_point_compensation: None,
                        drawing_scale: None,
                    });
                    if let Some(t) = self.active_tab_mut() {
                        t.viewport.multipage_view = next;
                        t.needs_rerender = true;
                    }
                }
            }
            "view.wireframe" => {
                if let Some(t) = self.active_tab_mut() {
                    t.view_mode = ViewMode::Wireframe;
                    t.needs_rerender = true;
                }
            }
            "view.enhanced" => {
                if let Some(t) = self.active_tab_mut() {
                    t.view_mode = ViewMode::Enhanced;
                    t.needs_rerender = true;
                }
            }
            "view.zoom_all" => self.handle_shortcut(ShortcutAction::ZoomToAll),
            "view.zoom_page" => self.handle_shortcut(ShortcutAction::ZoomToPage),
            "text.fit_path" => self.fit_selected_text_to_path(),
            "table.insert" => self.insert_table(3, 3),
            "bitmap.trace" => {
                if let Err(e) = self.trace_selected_raster() {
                    self.push_toast(e, true);
                }
            }
            "bitmap.convert" => self.convert_selection_to_bitmap(),
            "theme.dark" => self.set_theme(StudioTheme::DarkStudio),
            "theme.light" => self.set_theme(StudioTheme::ClassicLight),
            "theme.contrast" => self.set_theme(StudioTheme::HighContrast),
            "profile.classic" => self.set_shortcut_profile(ShortcutProfile::ClassicStudio),
            "profile.standard" => self.set_shortcut_profile(ShortcutProfile::StandardVector),
            "bool.weld" => self.apply_boolean_op(BooleanOp::Weld),
            "bool.trim" => self.apply_boolean_op(BooleanOp::Trim),
            "bool.intersect" => self.apply_boolean_op(BooleanOp::Intersect),
            "bool.simplify" => self.apply_boolean_op(BooleanOp::Simplify),
            "bool.front_minus_back" => self.apply_boolean_op(BooleanOp::FrontMinusBack),
            "bool.back_minus_front" => self.apply_boolean_op(BooleanOp::BackMinusFront),
            "bool.boundary" => self.apply_boolean_op(BooleanOp::CreateBoundary),
            "bool.combine" => self.apply_boolean_op(BooleanOp::Combine),
            "bool.break_apart" => self.apply_boolean_op(BooleanOp::BreakApart),
            other => {
                if let Some(idx_str) = other.strip_prefix("lens.") {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if idx < 11 {
                            self.lens_state.selected_index = idx;
                            let cfg = self.lens_state.build_config();
                            self.apply_lens_to_selection(Some(cfg));
                            self.focus_docker(DockerTab::Lens);
                            return true;
                        }
                    }
                }
                if let Some(title) = other.strip_prefix("docker.") {
                    if let Some(d) = DockerTab::ALL.into_iter().find(|d| d.title() == title) {
                        self.focus_docker(d);
                        return true;
                    }
                }
                if let Some(tool_dbg) = other.strip_prefix("tool.") {
                    for group in TOOL_GROUPS {
                        for &tool in group {
                            if format!("{tool:?}") == tool_dbg {
                                self.select_tool(tool);
                                return true;
                            }
                        }
                    }
                }
                if let Some(macro_id) = other.strip_prefix("macro.") {
                    if let Some(m) = coree_prod::find_builtin_macro(macro_id) {
                        let _ = self.run_rhai_script(m.script);
                        return true;
                    }
                }
                return false;
            }
        }
        true
    }
}

fn next_incremental_path(src: &Path) -> PathBuf {
    let dir = src
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let raw_stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Untitled");
    let raw_ext = src
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "coree".to_owned());
    let ext = if matches!(raw_ext.as_str(), "coree" | "cdr" | "cdt" | "svg") {
        raw_ext
    } else {
        "coree".to_owned()
    };

    let (base, start_num) = if let Some((prefix, num_str)) = raw_stem.rsplit_once("_v") {
        if !prefix.is_empty() {
            if let Ok(n) = num_str.parse::<u32>() {
                (prefix.to_owned(), n.saturating_add(1).max(1))
            } else {
                (raw_stem.to_owned(), 1)
            }
        } else {
            (raw_stem.to_owned(), 1)
        }
    } else {
        (raw_stem.to_owned(), 1)
    };

    for v in start_num..start_num.saturating_add(10_000) {
        let candidate = dir.join(format!("{base}_v{v}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{base}_v{start_num}.{ext}"))
}
