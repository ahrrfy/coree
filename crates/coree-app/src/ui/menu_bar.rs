// SPDX-License-Identifier: MIT OR Apache-2.0

//! Complete 13-menu Classic Studio Menu Bar (`File`, `Edit`, `View`, `Layout`,
//! `Object`, `Effects`, `Bitmaps`, `Text`, `Table`, `Tools`, `Laser`, `Window`,
//! `Help`) plus the multi-document tab strip.

use crate::shortcuts::{shortcut_label, shortcut_label_for, ShortcutAction, ShortcutProfile};
use crate::state::{DockerTab, LensDockerState, PropertiesSubTab, StudioApp, WorkspacePreset};
use crate::theme::{StudioLanguage, StudioTheme, MENUBAR_HEIGHT};
use crate::ui::dialogs::{
    export_workspace_dialog, import_workspace_dialog, install_sample_plugin_action,
    EXPORT_FMT_GCODE,
};
use coree_core::{AlignOp, BooleanOp, CommandOp, DistributeOp, MasterLayerScope, ZOrderOp};
use coree_prod::{builtin_macros, default_plugin_dirs, export_laser_dxf_r12, plan_laser_toolpath};
use coree_render::ViewMode;
use egui::{CornerRadius, Frame, Margin, MenuBar, RichText, Stroke, Ui};

/// Renders the top 28px studio menu bar and multi-document tab strip.
pub fn show_menu_bar_and_tabs(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let profile = app.shortcut_profile;
    let lang = app.ui_language;

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
                ui.menu_button(lang.tr("File", "ملف"), |ui| {
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
                        "Import (CDR/PDF/AI/EPS/DXF/PLT/PSD/SVG/Raster)...",
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
                    if menu_item(ui, "Save Incremental Copy (_vN)", "") {
                        let has_path = app.active_tab().is_some_and(|t| t.source_path.is_some());
                        if has_path {
                            if let Err(err) = app.save_incremental_copy() {
                                app.push_toast(err, true);
                            }
                        } else {
                            app.save_as_file_dialog();
                        }
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Export (PDF/CDR/SVG/EPS/DXF/PLT/G-Code/PNG)...",
                        shortcut_label(profile, ShortcutAction::ExportFile),
                    ) {
                        app.export_dialog.open = true;
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(
                        ui,
                        "Print...",
                        &shortcut_label_for(
                            profile,
                            &app.custom_shortcuts,
                            ShortcutAction::PrintDocument,
                        ),
                    ) {
                        app.handle_shortcut(ShortcutAction::PrintDocument);
                        ui.close();
                    }
                    if menu_item(ui, "Print Preview & Imposition...", "") {
                        app.show_print_preview_dialog = true;
                        app.refresh_system_printers_async();
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
                ui.menu_button(lang.tr("Edit", "تحرير"), |ui| {
                    if menu_item(ui, "Undo", shortcut_label(profile, ShortcutAction::Undo)) {
                        let _ = app.undo();
                        ui.close();
                    }
                    if menu_item(ui, "Redo", shortcut_label(profile, ShortcutAction::Redo)) {
                        let _ = app.redo();
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Repeat Last Command",
                        shortcut_label(profile, ShortcutAction::RepeatLastCommand),
                    ) {
                        app.repeat_last_command();
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(ui, "Cut", shortcut_label(profile, ShortcutAction::Cut)) {
                        app.cut_selection();
                        ui.close();
                    }
                    if menu_item(ui, "Copy", shortcut_label(profile, ShortcutAction::Copy)) {
                        app.copy_selection();
                        ui.close();
                    }
                    if menu_item(ui, "Paste", shortcut_label(profile, ShortcutAction::Paste)) {
                        app.paste_clipboard();
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
                        "Duplicate In-Place",
                        shortcut_label(profile, ShortcutAction::DuplicateInPlace),
                    ) {
                        let _ = app.duplicate_selection_with_offset(coree_core::Vec2D::ZERO);
                        ui.close();
                    }
                    let has_sel = app
                        .active_tab()
                        .is_some_and(|t| !t.selection.objects.is_empty());
                    let has_clone = app.active_tab().is_some_and(|t| {
                        t.selection.objects.iter().any(|id| {
                            t.document.object(*id).is_some_and(|o| {
                                matches!(o.payload, coree_core::NodePayload::Clone { .. })
                            })
                        })
                    });
                    if ui
                        .add_enabled(has_sel, egui::Button::new("Clone"))
                        .clicked()
                    {
                        let _ = app.clone_selection();
                        ui.close();
                    }
                    if ui
                        .add_enabled(has_clone, egui::Button::new("Unlink Clone"))
                        .clicked()
                    {
                        app.unlink_clone_selection();
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
                ui.menu_button(lang.tr("View", "عرض"), |ui| {
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
                    }
                    ui.separator();
                    if menu_item(
                        ui,
                        if app.full_screen_preview {
                            "✓ Full-Screen Preview"
                        } else {
                            "Full-Screen Preview"
                        },
                        shortcut_label(profile, ShortcutAction::ToggleFullScreenPreview),
                    ) {
                        app.handle_shortcut(ShortcutAction::ToggleFullScreenPreview);
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .checkbox(&mut app.soft_proof_cmyk, "Proof Colors (CMYK Soft Proof)")
                        .changed()
                    {
                        app.sync_proof_options_to_tabs();
                    }
                    if ui
                        .checkbox(&mut app.gamut_warning, "Gamut Warning")
                        .changed()
                    {
                        app.sync_proof_options_to_tabs();
                    }
                    if ui
                        .checkbox(
                            &mut app.show_laser_toolpath_overlay,
                            "Laser Toolpath Overlay",
                        )
                        .changed()
                        && app.show_laser_toolpath_overlay
                        && app.laser_plan_preview.is_none()
                    {
                        app.refresh_laser_plan_preview();
                    }
                    ui.separator();
                    let mut pending_multipage: Option<bool> = None;
                    let mut pending_grid: Option<coree_core::GridConfig> = None;
                    let mut pending_pgrid: Option<coree_core::PerspectiveGridConfig> = None;
                    let mut pending_draw_persp: Option<bool> = None;
                    if let Some(tab) = app.active_tab_mut() {
                        let mut multipage = tab.document.multipage_view();
                        if ui.checkbox(&mut multipage, "Multipage View").changed() {
                            pending_multipage = Some(multipage);
                            tab.viewport.multipage_view = multipage;
                            tab.needs_rerender = true;
                        }
                        let mut grid = tab.document.grid();
                        if ui
                            .checkbox(&mut grid.visible, "Show Document Grid")
                            .changed()
                        {
                            pending_grid = Some(grid);
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
                        let mut pgrid = tab.document.perspective_grid().clone();
                        if ui
                            .checkbox(&mut pgrid.enabled, "Perspective Drawing Grid")
                            .changed()
                        {
                            pending_pgrid = Some(pgrid.clone());
                            tab.needs_rerender = true;
                        }
                        ui.menu_button("Perspective Plane", |ui| {
                            for (plane, label) in [
                                (
                                    coree_core::PerspectivePlane::Orthographic,
                                    "Orthographic (Off)",
                                ),
                                (coree_core::PerspectivePlane::Top, "Top Plane"),
                                (coree_core::PerspectivePlane::Left, "Left Plane"),
                                (coree_core::PerspectivePlane::Right, "Right Plane"),
                                (coree_core::PerspectivePlane::Front, "Front Plane"),
                            ] {
                                if ui
                                    .selectable_label(pgrid.active_plane == plane, label)
                                    .clicked()
                                {
                                    let mut next = pgrid.clone();
                                    next.active_plane = plane;
                                    if plane != coree_core::PerspectivePlane::Orthographic {
                                        next.enabled = true;
                                        pending_draw_persp = Some(true);
                                    }
                                    pending_pgrid = Some(next);
                                    tab.needs_rerender = true;
                                    ui.close();
                                }
                            }
                        });
                        if ui
                            .checkbox(&mut tab.overprint_preview, "Simulate Overprints")
                            .changed()
                        {
                            tab.needs_rerender = true;
                        }
                        ui.menu_button("Snap To", |ui| {
                            ui.checkbox(&mut tab.snap_options.objects, "Snap to Objects");
                            ui.checkbox(&mut tab.snap_options.guidelines, "Snap to Guidelines");
                            if ui
                                .checkbox(&mut tab.snap_options.grid, "Snap to Document Grid")
                                .changed()
                            {
                                let mut g = pending_grid.unwrap_or_else(|| tab.document.grid());
                                g.snap = tab.snap_options.grid;
                                pending_grid = Some(g);
                            }
                        });
                    }
                    if let Some(next) = pending_multipage {
                        let _ = app.execute_command(CommandOp::SetDocumentProperties {
                            title: None,
                            color_mode: None,
                            default_unit: None,
                            dpi: None,
                            multipage_view: Some(next),
                            active_page_index: None,
                            metadata: None,
                            custom_icc_profile: None,
                            clear_custom_icc: false,
                        });
                    }
                    if let Some(config) = pending_grid {
                        let _ = app.execute_command(CommandOp::SetGridConfig { config });
                    }
                    if let Some(config) = pending_pgrid {
                        let _ = app.execute_command(CommandOp::SetPerspectiveGrid { config });
                    }
                    if let Some(dp) = pending_draw_persp {
                        app.tool_settings.draw_in_perspective = dp;
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
                        "Zoom to Selected",
                        shortcut_label(profile, ShortcutAction::ZoomToSelected),
                    ) {
                        app.handle_shortcut(ShortcutAction::ZoomToSelected);
                        ui.close();
                    }
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
                ui.menu_button(lang.tr("Layout", "تخطيط"), |ui| {
                    if menu_item(ui, "Insert Page", "") {
                        let (w, h, b, next_num) = app
                            .active_tab()
                            .and_then(|t| {
                                t.document.active_page().map(|p| {
                                    (
                                        p.width_mm,
                                        p.height_mm,
                                        p.bleed_mm,
                                        t.document.pages().len() + 1,
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
                    if menu_item(ui, "Duplicate Active Page", "") {
                        if let Some(idx) = app.active_tab().map(|t| t.document.active_page_index())
                        {
                            app.duplicate_page(idx);
                        }
                        ui.close();
                    }
                    if menu_item(ui, "Rename Active Page", "") {
                        if let Some(tab) = app.active_tab_mut() {
                            let idx = tab.document.active_page_index();
                            if let Some(p) = tab.document.page(idx) {
                                tab.inline_page_name_buf = p.name.clone();
                                tab.inline_renaming_page = Some(idx);
                            }
                        }
                        ui.close();
                    }
                    if menu_item(ui, "Delete Active Page", "") {
                        if let Some(pid) = app
                            .active_tab()
                            .filter(|t| t.document.pages().len() > 1)
                            .and_then(|t| t.document.active_page())
                            .map(|p| p.id)
                        {
                            let _ = app.execute_command(CommandOp::DeletePage { page_id: pid });
                        } else {
                            app.push_toast("Cannot delete the last remaining page", true);
                        }
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(ui, "Switch Page Orientation (Portrait / Landscape)", "") {
                        let dims = app
                            .active_tab()
                            .and_then(|t| t.document.active_page())
                            .map(|p| (p.id, p.width_mm, p.height_mm));
                        if let Some((page_id, w, h)) = dims {
                            let _ = app.execute_command(CommandOp::SetPageProperties {
                                page_id,
                                name: None,
                                width_mm: Some(h),
                                height_mm: Some(w),
                                bleed_mm: None,
                                facing_pages: None,
                            });
                            if let Some(tab) = app.active_tab_mut() {
                                tab.needs_rerender = true;
                            }
                        }
                        ui.close();
                    }
                    if menu_item(ui, "Autofit Page to Content (5 mm margin)", "") {
                        app.autofit_active_page(5.0);
                        ui.close();
                    }
                    if menu_item(ui, "Page Setup & Units...", "") {
                        app.options_tab = 1;
                        app.show_options_dialog = true;
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(ui, "Add Master Layer (All Pages)", "") {
                        app.add_layer_active(
                            "Master Layer".to_owned(),
                            false,
                            MasterLayerScope::AllPages,
                        );
                        ui.close();
                    }
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
                ui.menu_button(lang.tr("Object", "كائن"), |ui| {
                    if menu_item(
                        ui,
                        lang.tr("Edit Fill...", "تحرير التعبئة..."),
                        shortcut_label(profile, ShortcutAction::OpenEditFill),
                    ) {
                        app.open_edit_fill_dialog();
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        lang.tr("Outline Pen...", "قلم الحدود..."),
                        shortcut_label(profile, ShortcutAction::FocusOutlinePen),
                    ) {
                        app.open_outline_pen_dialog();
                        ui.close();
                    }
                    ui.separator();
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
                        if menu_item(
                            ui,
                            "Center to Page",
                            shortcut_label(profile, ShortcutAction::Align(AlignOp::CenterPage)),
                        ) {
                            app.apply_align(AlignOp::CenterPage);
                            ui.close();
                        }
                        ui.separator();
                        for (op, label) in [
                            (AlignOp::Left, "Align Left"),
                            (AlignOp::Right, "Align Right"),
                            (AlignOp::Top, "Align Top"),
                            (AlignOp::Bottom, "Align Bottom"),
                            (AlignOp::CenterH, "Align Centers Horizontally"),
                            (AlignOp::CenterV, "Align Centers Vertically"),
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
                    ui.menu_button("Shaping", |ui| {
                        for (op, label) in [
                            (BooleanOp::Weld, "Weld"),
                            (BooleanOp::Trim, "Trim"),
                            (BooleanOp::Intersect, "Intersect"),
                            (BooleanOp::Simplify, "Simplify"),
                            (BooleanOp::FrontMinusBack, "Front Minus Back"),
                            (BooleanOp::BackMinusFront, "Back Minus Front"),
                            (BooleanOp::CreateBoundary, "Boundary"),
                        ] {
                            if ui.button(label).clicked() {
                                app.apply_boolean_op(op);
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
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
                        app.break_apart_selection();
                        ui.close();
                    }
                    ui.separator();
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
                    if menu_item(
                        ui,
                        "Convert to Curves",
                        shortcut_label(profile, ShortcutAction::ConvertToCurves),
                    ) {
                        app.convert_selection_to_curves();
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        "Convert Outline to Object",
                        &shortcut_label_for(
                            profile,
                            &app.custom_shortcuts,
                            ShortcutAction::ConvertOutlineToObject,
                        ),
                    ) {
                        app.convert_outline_to_object_selection();
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button("PowerClip", |ui| {
                        if ui.button("Place Inside Frame").clicked() {
                            app.create_powerclip_from_selection();
                            ui.close();
                        }
                        let selected_pc = app.active_tab().and_then(|t| {
                            let oid = *t.selection.objects.first()?;
                            matches!(
                                t.document.object(oid).map(|o| &o.payload),
                                Some(coree_core::NodePayload::PowerClip { .. })
                            )
                            .then_some(oid)
                        });
                        let in_pc_edit = app
                            .active_tab()
                            .is_some_and(|t| t.powerclip_edit_id.is_some());
                        if ui
                            .add_enabled(
                                selected_pc.is_some(),
                                egui::Button::new("Edit PowerClip Contents (Ctrl+Click)"),
                            )
                            .clicked()
                        {
                            if let Some(pc_id) = selected_pc {
                                app.enter_powerclip_edit(pc_id);
                            }
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                in_pc_edit,
                                egui::Button::new("Finish Editing PowerClip (Esc)"),
                            )
                            .clicked()
                        {
                            app.exit_powerclip_edit();
                            ui.close();
                        }
                        if ui.button("Extract Contents").clicked() {
                            app.extract_selected_powerclip();
                            ui.close();
                        }
                        if ui.button("Fit Contents Proportionally").clicked() {
                            app.fit_powerclip_selection(coree_core::PowerClipFitMode::FitContents);
                            ui.close();
                        }
                        if ui.button("Center Contents").clicked() {
                            app.fit_powerclip_selection(
                                coree_core::PowerClipFitMode::CenterContents,
                            );
                            ui.close();
                        }
                        if ui.button("Stretch to Fill").clicked() {
                            app.fit_powerclip_selection(
                                coree_core::PowerClipFitMode::StretchToFill,
                            );
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            "Lock / Unlock Contents to PowerClip",
                            &shortcut_label_for(
                                profile,
                                &app.custom_shortcuts,
                                ShortcutAction::TogglePowerClipLock,
                            ),
                        ) {
                            app.set_powerclip_lock_selected(None);
                            ui.close();
                        }
                    });
                    if menu_item(
                        ui,
                        "Move to Cross-Page Desktop Layer",
                        &shortcut_label_for(
                            profile,
                            &app.custom_shortcuts,
                            ShortcutAction::MoveToDesktop,
                        ),
                    ) {
                        app.move_selection_to_desktop_layer();
                        ui.close();
                    }
                    if menu_item(ui, "Lock / Unlock Selection", "") {
                        app.toggle_lock_selection();
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(ui, "Assign CutContour Spot Hairline", "") {
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
                ui.menu_button(lang.tr("Effects", "تأثيرات"), |ui| {
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
                        "Inner Shadow",
                        "Bevel",
                        "Extrude 3D",
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
                ui.menu_button(lang.tr("Bitmaps", "صور نقطية"), |ui| {
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
                ui.menu_button(lang.tr("Text", "نص"), |ui| {
                    if menu_item(ui, "Text & Arabic RTL / Kashida Properties", "") {
                        app.properties_state.active_tab = PropertiesSubTab::Text;
                        app.focus_docker(DockerTab::Properties);
                        ui.close();
                    }
                    if menu_item(ui, "Glyphs Browser (Arabic / Symbols / Math)...", "") {
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
                ui.menu_button(lang.tr("Table", "جدول"), |ui| {
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
                ui.menu_button(lang.tr("Tools", "أدوات"), |ui| {
                    if menu_item(
                        ui,
                        "Options...",
                        shortcut_label(profile, ShortcutAction::OpenOptions),
                    ) {
                        app.options_tab = 0;
                        app.show_options_dialog = true;
                        ui.close();
                    }
                    if menu_item(ui, "Color Management (ICC)...", "") {
                        app.options_tab = 2;
                        app.show_options_dialog = true;
                        ui.close();
                    }
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
                    if menu_item(ui, "Macros & Scripts...", "") {
                        app.focus_docker(DockerTab::Macros);
                        ui.close();
                    }
                    if menu_item(ui, "Plugins Folder...", "") {
                        let dirs = default_plugin_dirs();
                        if let Some(dir) = dirs.first() {
                            let _ = std::fs::create_dir_all(dir);
                            app.push_toast(format!("Plugins folder: {}", dir.display()), false);
                        }
                        app.options_tab = 3;
                        app.show_options_dialog = true;
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
                });

                // 11. Laser (Dedicated Laser Cutting, Engraving & Plotter Plugins Menu!)
                ui.menu_button(lang.tr("Laser", "ليزر"), |ui| {
                    if menu_item(ui, "Laser & Plugins Studio Docker", "") {
                        app.production_state.sub_section = 5;
                        app.focus_docker(DockerTab::Production);
                        ui.close();
                    }
                    if menu_item(
                        ui,
                        if app.show_laser_toolpath_overlay {
                            "✓ Hide Toolpath Preview Overlay"
                        } else {
                            "Toggle Toolpath Preview Overlay"
                        },
                        "",
                    ) {
                        if app.show_laser_toolpath_overlay {
                            app.show_laser_toolpath_overlay = false;
                        } else {
                            app.refresh_laser_plan_preview();
                        }
                        ui.close();
                    }
                    ui.separator();
                    for (plugin_id, label) in [
                        ("lightburn-bridge", "Send to LightBurn"),
                        ("rdworks-ruida", "Send to RDWorks (Ruida)"),
                        ("ezcad-fiber", "Send to EZCad (Fiber)"),
                        ("laserdrw-m2nano", "Send to LaserDRW / M2 Nano"),
                        ("lasergrbl-bridge", "Send to LaserGRBL"),
                        ("inkcut-visicut", "Send to InkCut / VisiCut"),
                        ("vinyl-plotter-hpgl", "Send to Vinyl Plotter (HPGL)"),
                    ] {
                        if menu_item(ui, label, "") {
                            app.send_to_laser_plugin(plugin_id, true);
                            ui.close();
                        }
                    }
                    ui.separator();
                    if menu_item(ui, "Export Laser G-Code (.gcode)...", "") {
                        app.export_dialog.format_index = EXPORT_FMT_GCODE;
                        app.export_dialog.output_path = "laser_job.gcode".to_owned();
                        app.export_dialog.options.gcode_dialect = app.laser_config.dialect;
                        app.export_dialog.open = true;
                        ui.close();
                    }
                    if menu_item(ui, "Export Laser DXF R12...", "") {
                        export_laser_dxf_dialog(app);
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(ui, "Remove Duplicate Lines", "") {
                        app.laser_config.dedupe_tolerance_mm =
                            app.laser_config.dedupe_tolerance_mm.max(0.05);
                        app.refresh_laser_plan_preview();
                        if let Some(ref plan) = app.laser_plan_preview {
                            app.push_toast(
                                format!(
                                    "Laser Prep: removed {} duplicate segment(s) (tol {:.2} mm)",
                                    plan.duplicate_segments_removed,
                                    app.laser_config.dedupe_tolerance_mm
                                ),
                                false,
                            );
                        }
                        ui.close();
                    }
                    if menu_item(ui, "Join Open Curves", "") {
                        app.laser_config.join_tolerance_mm =
                            app.laser_config.join_tolerance_mm.max(0.05);
                        app.refresh_laser_plan_preview();
                        if let Some(ref plan) = app.laser_plan_preview {
                            app.push_toast(
                                format!(
                                    "Laser Prep: joined {} open curve endpoint(s) (tol {:.2} mm)",
                                    plan.curves_joined, app.laser_config.join_tolerance_mm
                                ),
                                false,
                            );
                        }
                        ui.close();
                    }
                    ui.separator();
                    if menu_item(ui, "Install Sample Custom Plugin", "") {
                        install_sample_plugin_action(app);
                        ui.close();
                    }
                });

                // 12. Window
                ui.menu_button(lang.tr("Window", "نافذة"), |ui| {
                    ui.menu_button("Workspaces", |ui| {
                        for (preset, label) in [
                            (WorkspacePreset::DefaultClassic, "Default (Classic Studio)"),
                            (WorkspacePreset::Lite, "Lite"),
                            (WorkspacePreset::Illustration, "Illustration"),
                            (WorkspacePreset::PageLayout, "Page Layout"),
                            (WorkspacePreset::LaserSignStudio, "Laser & Sign Studio"),
                            (WorkspacePreset::TouchCompact, "Touch / Compact"),
                        ] {
                            let active = app.active_workspace_preset == preset;
                            if ui.selectable_label(active, label).clicked() {
                                app.apply_workspace_preset(preset);
                                ui.close();
                            }
                        }
                        ui.separator();
                        if menu_item(ui, "Export Workspace (.coreews)...", "") {
                            export_workspace_dialog(app);
                            ui.close();
                        }
                        if menu_item(ui, "Import Workspace (.coreews)...", "") {
                            import_workspace_dialog(app);
                            ui.close();
                        }
                        if menu_item(ui, "Reset Workspace to Default", "") {
                            app.apply_workspace_preset(WorkspacePreset::DefaultClassic);
                            ui.close();
                        }
                    });
                    ui.menu_button("Dockers", |ui| {
                        ui.checkbox(&mut app.show_dockers, "Show Dockers Pane");
                        ui.checkbox(&mut app.toolbox_two_column, "Two-Column Toolbox");
                        ui.separator();
                        let laser_focused = app.production_state.sub_section == 5
                            && app.dock_state.find_tab(&DockerTab::Production).is_some();
                        if ui
                            .selectable_label(laser_focused, "⚡ Laser & Plugins")
                            .clicked()
                        {
                            app.production_state.sub_section = 5;
                            app.focus_docker(DockerTab::Production);
                            ui.close();
                        }
                        let glyphs_focused = app.properties_state.active_tab
                            == PropertiesSubTab::Text
                            && app.dock_state.find_tab(&DockerTab::Properties).is_some();
                        if ui.selectable_label(glyphs_focused, "¶ Glyphs").clicked() {
                            app.properties_state.active_tab = PropertiesSubTab::Text;
                            app.focus_docker(DockerTab::Properties);
                            ui.close();
                        }
                        ui.separator();
                        for docker in DockerTab::ALL {
                            let present = app.dock_state.find_tab(&docker).is_some();
                            if ui.selectable_label(present, docker.title()).clicked() {
                                app.toggle_docker(docker);
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("Color Palettes", |ui| {
                        for (idx, name) in [
                            (0usize, "Default CMYK"),
                            (1, "Default RGB"),
                            (2, "Document Palette"),
                            (3, "Laser Cut Colors"),
                        ] {
                            if ui
                                .selectable_label(app.active_palette_index == idx, name)
                                .clicked()
                            {
                                app.active_palette_index = idx;
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
                    ui.menu_button(
                        lang.tr("Language / اللغة", "اللغة / Language"),
                        |ui| {
                            for l in StudioLanguage::ALL {
                                if ui
                                    .selectable_label(app.ui_language == l, l.label())
                                    .clicked()
                                {
                                    app.set_ui_language(l);
                                    ui.close();
                                }
                            }
                        },
                    );
                });

                // 13. Help
                ui.menu_button(lang.tr("Help", "مساعدة"), |ui| {
                    if menu_item(ui, "About coree & Clean-Room Notice", "") {
                        app.show_about_dialog = true;
                        ui.close();
                    }
                });

                // Right-aligned quick Command Palette + Language toggle + Proof/Offline indicators
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let next_lang = match app.ui_language {
                        StudioLanguage::English => StudioLanguage::Arabic,
                        StudioLanguage::Arabic => StudioLanguage::English,
                    };
                    let lang_pill = match app.ui_language {
                        StudioLanguage::English => "EN | ع",
                        StudioLanguage::Arabic => "عربية | EN",
                    };
                    if ui
                        .small_button(lang_pill)
                        .on_hover_text("Toggle English / العربية interface labels")
                        .clicked()
                    {
                        app.set_ui_language(next_lang);
                    }
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
                    if app.soft_proof_cmyk {
                        ui.label(
                            RichText::new("● CMYK Proof")
                                .size(11.0)
                                .color(tokens.warning),
                        );
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

fn export_laser_dxf_dialog(app: &mut StudioApp) {
    let Some(tab) = app.active_tab() else {
        app.push_toast("No active document to export", true);
        return;
    };
    let page_idx = tab.document.active_page_index();
    let sel = if tab.selection.is_empty() {
        None
    } else {
        Some(tab.selection.objects.as_slice())
    };
    let plan = match plan_laser_toolpath(&tab.document, page_idx, sel, &app.laser_config) {
        Ok(p) => p,
        Err(e) => {
            app.push_toast(format!("Laser plan failed: {e}"), true);
            return;
        }
    };
    if let Some(path) = rfd::FileDialog::new()
        .set_title("Export Laser AutoCAD R12 DXF")
        .set_file_name("laser_job_r12.dxf")
        .add_filter("AutoCAD R12 DXF", &["dxf"])
        .save_file()
    {
        let dxf = export_laser_dxf_r12(&plan);
        match coree_io::atomic_write_bytes(&path, dxf.as_bytes()) {
            Ok(()) => app.push_toast(
                format!(
                    "Exported Laser DXF R12 ({} seg(s)) -> {}",
                    plan.segments.len(),
                    path.display()
                ),
                false,
            ),
            Err(e) => app.push_toast(format!("Laser DXF export failed: {e}"), true),
        }
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
