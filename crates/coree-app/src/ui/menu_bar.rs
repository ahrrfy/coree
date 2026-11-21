// SPDX-License-Identifier: MPL-2.0

//! Complete 12-menu studio Menu Bar (`File`, `Edit`, `View`, `Layout`, `Object`,
//! `Effects`, `Bitmaps`, `Text`, `Table`, `Tools`, `Window`, `Help`) plus the
//! multi-document tab strip.

use crate::shortcuts::{shortcut_label, ShortcutAction, ShortcutProfile};
use crate::state::{DockerTab, LensDockerState, PropertiesSubTab, StudioApp};
use crate::theme::{StudioTheme, MENUBAR_HEIGHT};
use coree_core::{AlignOp, BooleanOp, CommandOp, DistributeOp, MasterLayerScope, ZOrderOp};
use coree_prod::builtin_macros;
use coree_render::ViewMode;
use egui::{CornerRadius, Frame, Margin, MenuBar, RichText, Stroke, Ui};

/// Renders the top 28px studio menu bar and document tab strip.
pub fn show_menu_bar_and_tabs(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let profile = app.shortcut_profile;

    egui::Panel::top("coree_menu_bar")
        .exact_size(MENUBAR_HEIGHT)
        .frame(
            Frame::NONE
                .fill(tokens.bg)
                .inner_margin(Margin::symmetric(6, 2))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            MenuBar::new().ui(ui, |ui| {
                // 1. File
                ui.menu_button("File", |ui| {
                    if menu_item(
                        ui,
                        "New...",
                        shortcut_label(profile, ShortcutAction::NewDocument),
                    ) {
                        app.new_doc_dialog.open = true;
                        ui.close();
                    }
                    if menu_item(ui, "New Showcase Sample", "") {
                        app.new_sample_document();
                        ui.close();
                    }
                    if menu_item(ui, "Welcome & Recovery Hub", "") {
                        app.show_welcome = true;
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(
                        ui,
                        "Open...",
                        shortcut_label(profile, ShortcutAction::OpenDocument),
                    ) {
                        app.open_file_dialog();
                        ui.close();
                    }
                    ui.menu_button("Open Recent", |ui| {
                        let recents = app.recent_docs.clone();
                        if recents.is_empty() {
                            ui.label(RichText::new("No recent files").color(tokens.text_muted));
                        } else {
                            for rec in recents.iter().take(12) {
                                let lbl = format!("{}  [{}]", rec.display_name, rec.file_format);
                                if ui.button(lbl).clicked() {
                                    let path = std::path::PathBuf::from(&rec.file_path);
                                    if let Err(err) = app.open_file_path(&path) {
                                        app.push_toast(err, true);
                                    }
                                    ui.close();
                                }
                            }
                        }
                    });
                    if menu_item(
                        ui,
                        "Import (CDR/CDT/CMX/CPT/SVG/Raster)...",
                        shortcut_label(profile, ShortcutAction::ImportFile),
                    ) {
                        app.import_file_dialog();
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(
                        ui,
                        "Save",
                        shortcut_label(profile, ShortcutAction::SaveDocument),
                    ) {
                        app.handle_shortcut(ShortcutAction::SaveDocument);
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Save As...",
                        shortcut_label(profile, ShortcutAction::SaveDocumentAs),
                    ) {
                        app.save_as_file_dialog();
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Export (CDR/CDT/CMX/SVG/PDF/EPS/DXF/PLT/PNG)...",
                        shortcut_label(profile, ShortcutAction::ExportFile),
                    ) {
                        app.export_dialog.open = true;
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(ui, "Print Preview & Imposition...", "") {
                        app.show_print_preview_dialog = true;
                        ui.close();
                    }
                    if menu_item(ui, "Preflight Audit...", "") {
                        app.run_preflight_active();
                        app.production_state.sub_section = 0;
                        app.focus_docker(DockerTab::Production);
                        ui.close();
                    }
                    if menu_item(ui, "Collect for Output...", "") {
                        app.production_state.sub_section = 4;
                        app.focus_docker(DockerTab::Production);
                        ui.close();
                    }
                });

                // 2. Edit
                ui.menu_button("Edit", |ui| {
                    if menu_item(ui, "Undo", shortcut_label(profile, ShortcutAction::Undo)) {
                        let _ = app.undo();
                        ui.close();
                    }
                    if menu_item(ui, "Redo", shortcut_label(profile, ShortcutAction::Redo)) {
                        let _ = app.redo();
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(
                        ui,
                        "Duplicate",
                        shortcut_label(profile, ShortcutAction::Duplicate),
                    ) {
                        let _ = app.duplicate_selection();
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Delete",
                        shortcut_label(profile, ShortcutAction::DeleteSelection),
                    ) {
                        app.delete_selection();
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Select All",
                        shortcut_label(profile, ShortcutAction::SelectAll),
                    ) {
                        app.select_all();
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(
                        ui,
                        "Find and Replace...",
                        shortcut_label(profile, ShortcutAction::OpenFindReplace),
                    ) {
                        app.focus_docker(DockerTab::FindReplace);
                        ui.close();
                    }
                });

                // 3. View
                ui.menu_button("View", |ui| {
                    if let Some(tab) = app.active_tab_mut() {
                        for (mode, label) in [
                            (ViewMode::Wireframe, "Wireframe"),
                            (ViewMode::Normal, "Normal"),
                            (ViewMode::Enhanced, "Enhanced"),
                            (ViewMode::Pixels, "Pixels"),
                        ] {
                            let selected = tab.view_mode == mode;
                            if ui.selectable_label(selected, label).clicked() {
                                tab.view_mode = mode;
                                tab.needs_rerender = true;
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui
                            .checkbox(&mut tab.document.multipage_view, "Multipage View")
                            .changed()
                        {
                            tab.viewport.multipage_view = tab.document.multipage_view;
                            tab.needs_rerender = true;
                        }
                        if ui
                            .checkbox(&mut tab.document.grid.visible, "Show Document Grid")
                            .changed()
                        {
                            tab.needs_rerender = true;
                        }
                        if ui
                            .checkbox(&mut tab.render_options.show_guidelines, "Show Guidelines")
                            .changed()
                        {
                            tab.needs_rerender = true;
                        }
                        if ui
                            .checkbox(&mut tab.render_options.show_bleed_guides, "Show Bleed Box")
                            .changed()
                        {
                            tab.needs_rerender = true;
                        }
                        if ui
                            .checkbox(
                                &mut tab.document.perspective_grid.enabled,
                                "Perspective Drawing Grid",
                            )
                            .changed()
                        {
                            tab.needs_rerender = true;
                        }
                        ui.separator();
                        if ui
                            .checkbox(&mut tab.overprint_preview, "Simulate Overprints")
                            .changed()
                        {
                            tab.needs_rerender = true;
                        }
                    }
                    if menu_item(ui, "Compute CMYK/Spot Separations Preview", "") {
                        app.compute_separations_active();
                        app.production_state.sub_section = 1;
                        app.focus_docker(DockerTab::Production);
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(
                        ui,
                        "Zoom to All Objects",
                        shortcut_label(profile, ShortcutAction::ZoomToAll),
                    ) {
                        app.handle_shortcut(ShortcutAction::ZoomToAll);
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Zoom to Page",
                        shortcut_label(profile, ShortcutAction::ZoomToPage),
                    ) {
                        app.handle_shortcut(ShortcutAction::ZoomToPage);
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Actual Size (100%)",
                        shortcut_label(profile, ShortcutAction::ZoomActual),
                    ) {
                        app.handle_shortcut(ShortcutAction::ZoomActual);
                        ui.close();
                    }
                });

                // 4. Layout
                ui.menu_button("Layout", |ui| {
                    if menu_item(ui, "Insert Page", "") {
                        let (w, h, b, next_num) = app
                            .active_tab()
                            .and_then(|t| {
                                t.document.active_page().map(|p| {
                                    (
                                        p.width_mm,
                                        p.height_mm,
                                        p.bleed_mm,
                                        t.document.pages.len() + 1,
                                    )
                                })
                            })
                            .unwrap_or((210.0, 297.0, 3.0, 2));
                        let _ = app.execute_command(CommandOp::AddPage {
                            name: format!("Page {next_num}"),
                            width_mm: w,
                            height_mm: h,
                            bleed_mm: b,
                        });
                        ui.close();
                    }
                    if menu_item(ui, "Delete Active Page", "") {
                        if let Some(pid) = app
                            .active_tab()
                            .filter(|t| t.document.pages.len() > 1)
                            .and_then(|t| t.document.active_page())
                            .map(|p| p.id)
                        {
                            let _ = app.execute_command(CommandOp::DeletePage { page_id: pid });
                        } else {
                            app.push_toast("Cannot delete the last remaining page", true);
                        }
                        ui.close();
                    }
                    if menu_item(ui, "Add Master Layer (All Pages)", "") {
                        app.add_layer_active(
                            "Master Layer".to_owned(),
                            false,
                            MasterLayerScope::AllPages,
                        );
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(ui, "Imposition (N-Up / Booklet / Tiling)...", "") {
                        app.production_state.sub_section = 1;
                        app.focus_docker(DockerTab::Production);
                        ui.close();
                    }
                    if menu_item(ui, "Print Merge (CSV / Serial Numbers)...", "") {
                        app.production_state.sub_section = 4;
                        app.focus_docker(DockerTab::Production);
                        ui.close();
                    }
                    if menu_item(ui, "Add Crop & Registration Print Marks", "") {
                        app.add_print_marks_active();
                        ui.close();
                    }
                });

                // 5. Object
                ui.menu_button("Object", |ui| {
                    if menu_item(ui, "Group", shortcut_label(profile, ShortcutAction::Group)) {
                        app.group_selection();
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Ungroup",
                        shortcut_label(profile, ShortcutAction::Ungroup),
                    ) {
                        app.ungroup_selection();
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button("Shaping (Booleans)", |ui| {
                        for (op, label) in [
                            (BooleanOp::Weld, "Weld"),
                            (BooleanOp::Trim, "Trim"),
                            (BooleanOp::Intersect, "Intersect"),
                            (BooleanOp::Simplify, "Simplify"),
                            (BooleanOp::FrontMinusBack, "Front Minus Back"),
                            (BooleanOp::BackMinusFront, "Back Minus Front"),
                            (BooleanOp::CreateBoundary, "Create Boundary"),
                        ] {
                            if ui.button(label).clicked() {
                                app.apply_boolean_op(op);
                                ui.close();
                            }
                        }
                    });
                    if menu_item(
                        ui,
                        "Combine",
                        shortcut_label(profile, ShortcutAction::Combine),
                    ) {
                        app.apply_boolean_op(BooleanOp::Combine);
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Break Apart",
                        shortcut_label(profile, ShortcutAction::BreakApart),
                    ) {
                        app.apply_boolean_op(BooleanOp::BreakApart);
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Convert to Curves",
                        shortcut_label(profile, ShortcutAction::ConvertToCurves),
                    ) {
                        app.convert_selection_to_curves();
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button("PowerClip", |ui| {
                        if ui.button("Place Inside Frame").clicked() {
                            app.create_powerclip_from_selection();
                            ui.close();
                        }
                        if ui.button("Extract Contents").clicked() {
                            app.extract_selected_powerclip();
                            ui.close();
                        }
                    });
                    ui.menu_button("Order", |ui| {
                        for (op, label) in [
                            (ZOrderOp::BringToFront, "To Front of Layer"),
                            (ZOrderOp::BringForward, "Forward One"),
                            (ZOrderOp::SendBackward, "Back One"),
                            (ZOrderOp::SendToBack, "To Back of Layer"),
                        ] {
                            if menu_item(
                                ui,
                                label,
                                shortcut_label(profile, ShortcutAction::ZOrder(op)),
                            ) {
                                app.apply_z_order(op);
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("Align and Distribute", |ui| {
                        for (op, label) in [
                            (AlignOp::Left, "Align Left"),
                            (AlignOp::CenterH, "Align Centers Horizontally"),
                            (AlignOp::Right, "Align Right"),
                            (AlignOp::Top, "Align Top"),
                            (AlignOp::CenterV, "Align Centers Vertically"),
                            (AlignOp::Bottom, "Align Bottom"),
                            (AlignOp::CenterPage, "Center to Page"),
                        ] {
                            if menu_item(
                                ui,
                                label,
                                shortcut_label(profile, ShortcutAction::Align(op)),
                            ) {
                                app.apply_align(op);
                                ui.close();
                            }
                        }
                        ui.separator();
                        for (op, label) in [
                            (DistributeOp::SpacingH, "Distribute Spacing Horizontally"),
                            (DistributeOp::SpacingV, "Distribute Spacing Vertically"),
                        ] {
                            if ui.button(label).clicked() {
                                app.apply_distribute(op);
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    if menu_item(ui, "One-Click CutContour Spot Hairline", "") {
                        app.generate_cut_contour_active();
                        ui.close();
                    }
                    if menu_item(ui, "Parametric Packaging Dielines...", "") {
                        app.production_state.sub_section = 2;
                        app.focus_docker(DockerTab::Production);
                        ui.close();
                    }
                    if menu_item(ui, "Insert Vector Barcode / QR Code...", "") {
                        app.production_state.sub_section = 3;
                        app.focus_docker(DockerTab::Production);
                        ui.close();
                    }
                });

                // 6. Effects
                ui.menu_button("Effects", |ui| {
                    ui.menu_button("Apply Lens (All 11 Lenses)", |ui| {
                        for (idx, name) in LensDockerState::NAMES.iter().enumerate() {
                            if ui.button(*name).clicked() {
                                app.lens_state.selected_index = idx;
                                let cfg = app.lens_state.build_config();
                                app.apply_lens_to_selection(Some(cfg));
                                app.focus_docker(DockerTab::Lens);
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Remove Lens").clicked() {
                            app.apply_lens_to_selection(None);
                            ui.close();
                        }
                    });
                    if menu_item(
                        ui,
                        "Lens Inspector Docker",
                        shortcut_label(profile, ShortcutAction::ToggleDockerLens),
                    ) {
                        app.focus_docker(DockerTab::Lens);
                        ui.close();
                    }
                    ui.separator();
                    for (idx, name) in [
                        "Drop Shadow",
                        "Block Shadow",
                        "Contour",
                        "Blend",
                        "Distort",
                        "Envelope",
                        "Extrude 2.5D",
                        "Transparency",
                        "Perspective",
                        "Symmetry",
                    ]
                    .iter()
                    .enumerate()
                    {
                        if ui.button(format!("Add {name}")).clicked() {
                            app.effects_state.selected_effect_kind = idx;
                            let eff = app.build_selected_effect();
                            app.add_effect_to_selection(eff);
                            app.focus_docker(DockerTab::Effects);
                            ui.close();
                        }
                    }
                });

                // 7. Bitmaps
                ui.menu_button("Bitmaps", |ui| {
                    if menu_item(ui, "Convert Selection to Bitmap...", "") {
                        app.convert_selection_to_bitmap();
                        ui.close();
                    }
                    if menu_item(ui, "Resample & Bitmap Studio Docker", "") {
                        app.focus_docker(DockerTab::BitmapStudio);
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(ui, "Vector Trace (Quick / Outline / Centerline)...", "") {
                        app.focus_docker(DockerTab::VectorTrace);
                        ui.close();
                    }
                    if menu_item(ui, "Quick Trace Selected Bitmap Now", "") {
                        app.trace_state.mode_idx = 0;
                        if let Err(e) = app.trace_selected_raster() {
                            app.push_toast(e, true);
                        }
                        ui.close();
                    }
                });

                // 8. Text
                ui.menu_button("Text", |ui| {
                    if menu_item(ui, "Text & Arabic RTL / Kashida Properties", "") {
                        app.properties_state.active_tab = PropertiesSubTab::Text;
                        app.focus_docker(DockerTab::Properties);
                        ui.close();
                    }
                    if menu_item(ui, "Fit Text to Path", "") {
                        app.fit_selected_text_to_path();
                        ui.close();
                    }
                    if menu_item(ui, "Insert Arabic RTL Sample Heading", "") {
                        app.tool_settings.text_insert_draft = "استوديو كوري للتصميم".to_owned();
                        app.default_text_style.direction = coree_core::TextDirection::Rtl;
                        app.default_text_style.align = coree_core::TextAlign::ForceJustifyKashida;
                        app.default_text_style.kashida_stretch = 0.65;
                        app.insert_artistic_text_at(coree_core::Point2D::new(30.0, 45.0));
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(ui, "Font Manager (Arabic / Kashida Filter)...", "") {
                        app.refresh_font_manager(false);
                        app.focus_docker(DockerTab::FontManager);
                        ui.close();
                    }
                });

                // 9. Table
                ui.menu_button("Table", |ui| {
                    if menu_item(ui, "Insert 3x3 Table", "") {
                        app.insert_table(3, 3);
                        ui.close();
                    }
                    if menu_item(ui, "Insert 5x4 Specification Table", "") {
                        app.insert_table(5, 4);
                        ui.close();
                    }
                    if menu_item(ui, "Convert Selected Table to Curves", "") {
                        app.convert_selection_to_curves();
                        ui.close();
                    }
                });

                // 10. Tools
                ui.menu_button("Tools", |ui| {
                    if menu_item(
                        ui,
                        "Command Palette...",
                        shortcut_label(profile, ShortcutAction::ToggleCommandPalette),
                    ) {
                        app.handle_shortcut(ShortcutAction::ToggleCommandPalette);
                        ui.close();
                    }
                    ui.separator();
                    let recording = app.active_tab().is_some_and(|t| t.history.recording_macro);
                    if recording {
                        if menu_item(ui, "Stop Recording Macro -> Rhai", "") {
                            app.stop_macro_recording();
                            ui.close();
                        }
                    } else if menu_item(ui, "Start Recording Macro (Rhai)", "") {
                        app.start_macro_recording();
                        ui.close();
                    }
                    ui.menu_button("Run Built-in Macro", |ui| {
                        for m in builtin_macros() {
                            if ui.button(m.name).on_hover_text(m.description).clicked() {
                                let _ = app.run_rhai_script(m.script);
                                ui.close();
                            }
                        }
                    });
                    if menu_item(ui, "Macros & Script Studio...", "") {
                        app.focus_docker(DockerTab::Macros);
                        ui.close();
                    }
                    if menu_item(ui, "CDR Binary & Chunk Inspector...", "") {
                        app.refresh_inspector_for_active(false);
                        app.focus_docker(DockerTab::CdrInspector);
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button("Shortcut Profile", |ui| {
                        for prof in [
                            ShortcutProfile::ClassicStudio,
                            ShortcutProfile::StandardVector,
                        ] {
                            if ui
                                .selectable_label(app.shortcut_profile == prof, prof.label())
                                .clicked()
                            {
                                app.set_shortcut_profile(prof);
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("Theme", |ui| {
                        for theme in StudioTheme::ALL {
                            if ui
                                .selectable_label(app.theme == theme, theme.label())
                                .clicked()
                            {
                                app.set_theme(theme);
                                ui.close();
                            }
                        }
                    });
                    if menu_item(ui, "Preferences...", "") {
                        app.show_preferences_dialog = true;
                        ui.close();
                    }
                });

                // 11. Window
                ui.menu_button("Window", |ui| {
                    ui.checkbox(&mut app.show_dockers, "Show Dockers Pane");
                    ui.checkbox(&mut app.toolbox_two_column, "Two-Column Toolbox");
                    ui.separator();
                    for docker in DockerTab::ALL {
                        let present = app.dock_state.find_tab(&docker).is_some();
                        if ui.selectable_label(present, docker.title()).clicked() {
                            app.toggle_docker(docker);
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("Reset Default Workspace Layout").clicked() {
                        app.dock_state = crate::state::default_dock_state();
                        app.show_dockers = true;
                        ui.close();
                    }
                });

                // 12. Help
                ui.menu_button("Help", |ui| {
                    if menu_item(ui, "About coree & Clean-Room Notice", "") {
                        app.show_about_dialog = true;
                        ui.close();
                    }
                });

                // Right-aligned quick Command Palette button + Offline pill
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let cmd_btn = ui.button(
                        RichText::new(format!(
                            "⌘ Commands ({})",
                            shortcut_label(profile, ShortcutAction::ToggleCommandPalette)
                        ))
                        .size(11.0)
                        .color(tokens.accent),
                    );
                    if cmd_btn.clicked() {
                        app.handle_shortcut(ShortcutAction::ToggleCommandPalette);
                    }
                    ui.label(
                        RichText::new("● Offline · Local WAL")
                            .size(11.0)
                            .color(tokens.success),
                    );
                });
            });
        });

    // Multi-Document Tab Strip
    if !app.tabs.is_empty() {
        egui::Panel::top("coree_doc_tabs")
            .exact_size(26.0)
            .frame(
                Frame::NONE
                    .fill(tokens.bg)
                    .inner_margin(Margin::symmetric(6, 2))
                    .stroke(Stroke::new(1.0, tokens.border)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut switch_to = None;
                    let mut close_idx = None;

                    for (idx, tab) in app.tabs.iter().enumerate() {
                        let active = idx == app.active_tab && !app.show_welcome;
                        let bg = if active {
                            tokens.surface_elevated
                        } else {
                            tokens.surface
                        };
                        let stroke = if active {
                            Stroke::new(1.0, tokens.accent)
                        } else {
                            Stroke::new(1.0, tokens.border)
                        };
                        Frame::NONE
                            .fill(bg)
                            .stroke(stroke)
                            .corner_radius(CornerRadius::same(4))
                            .inner_margin(Margin::symmetric(8, 2))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    let lbl = ui.add(
                                        egui::Label::new(
                                            RichText::new(tab.tab_title()).size(11.5).color(
                                                if active {
                                                    tokens.text
                                                } else {
                                                    tokens.text_muted
                                                },
                                            ),
                                        )
                                        .sense(egui::Sense::click()),
                                    );
                                    if lbl.clicked() {
                                        switch_to = Some(idx);
                                    }
                                    if ui
                                        .small_button(
                                            RichText::new("×").size(11.0).color(tokens.text_muted),
                                        )
                                        .clicked()
                                    {
                                        close_idx = Some(idx);
                                    }
                                });
                            });
                    }

                    if ui
                        .small_button(RichText::new("+").color(tokens.accent))
                        .on_hover_text("New Blank A4 Document")
                        .clicked()
                    {
                        let n = app.tabs.len() + 1;
                        app.new_document(coree_core::Document::new_a4(format!("Untitled-{n}")));
                    }

                    if let Some(idx) = switch_to {
                        app.active_tab = idx;
                        app.show_welcome = false;
                    }
                    if let Some(idx) = close_idx {
                        app.close_tab(idx);
                    }
                });
            });
    }
}

fn menu_item(ui: &mut Ui, label: &str, shortcut: &str) -> bool {
    if shortcut.is_empty() {
        ui.button(label).clicked()
    } else {
        ui.add(egui::Button::new(label).shortcut_text(shortcut))
            .clicked()
    }
}
