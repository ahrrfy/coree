// SPDX-License-Identifier: MIT OR Apache-2.0

//! Complete 13-menu Classic Studio Menu Bar (`File`, `Edit`, `View`, `Layout`,
//! `Object`, `Effects`, `Bitmaps`, `Text`, `Table`, `Tools`, `Laser`, `Window`,
//! `Help`) plus the multi-document tab strip.

use crate::i18n::I18nKey;
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
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);

    egui::Panel::top("coree_menu_bar")
        .exact_size(MENUBAR_HEIGHT)
        .frame(
            Frame::NONE
                .fill(tokens.bg)
                .inner_margin(Margin::symmetric(6, 2))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            shell_dir.with_horizontal(ui, |ui| {
                MenuBar::new().ui(ui, |ui| {
                    // 1. File
                    ui.menu_button(I18nKey::MenuFile.tr(lang), |ui| {
                        if menu_item(
                            ui,
                            I18nKey::FileNew.tr(lang),
                            shortcut_label(profile, ShortcutAction::NewDocument),
                        ) {
                            app.new_doc_dialog.open = true;
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::OpenSampleShowcase.tr(lang), "") {
                            app.new_sample_document();
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::ReturnToCanvas.tr(lang), "") {
                            app.show_welcome = true;
                            ui.close();
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            I18nKey::FileOpen.tr(lang),
                            shortcut_label(profile, ShortcutAction::OpenDocument),
                        ) {
                            app.open_file_dialog();
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::FileOpenCdrWithCodepage.tr(lang), "") {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_title("Open CDR with Legacy Codepage")
                                .add_filter("CDR / CDT / CMX Vector Files", &["cdr", "cdt", "cmx"])
                                .pick_file()
                            {
                                app.cdr_import_dialog.pending_path = Some(path);
                                if app.cdr_import_dialog.selected_codepage.is_none() {
                                    app.cdr_import_dialog.selected_codepage = Some(1256);
                                }
                                app.cdr_import_dialog.open = true;
                            } else {
                                app.cdr_import_dialog.open = true;
                            }
                            ui.close();
                        }
                        ui.menu_button(I18nKey::RecentDocuments.tr(lang), |ui| {
                            let recents = app.recent_docs.clone();
                            if recents.is_empty() {
                                ui.label(
                                    RichText::new(I18nKey::WelcomeNoRecents.tr(lang))
                                        .color(tokens.text_muted),
                                );
                            } else {
                                for rec in recents.iter().take(12) {
                                    let lbl =
                                        format!("{}  [{}]", rec.display_name, rec.file_format);
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
                            I18nKey::FileImport.tr(lang),
                            shortcut_label(profile, ShortcutAction::ImportFile),
                        ) {
                            app.import_file_dialog();
                            ui.close();
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            I18nKey::FileSave.tr(lang),
                            shortcut_label(profile, ShortcutAction::SaveDocument),
                        ) {
                            app.handle_shortcut(ShortcutAction::SaveDocument);
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            I18nKey::FileSaveAs.tr(lang),
                            shortcut_label(profile, ShortcutAction::SaveDocumentAs),
                        ) {
                            app.save_as_file_dialog();
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::SaveAsTemplateCdt.tr(lang), "") {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_title("Save Document as Template (.cdt)")
                                .add_filter("Template", &["cdt"])
                                .set_file_name("template.cdt")
                                .save_file()
                            {
                                if let Err(err) =
                                    app.save_active_as_cdt_template(&path, "custom", "")
                                {
                                    app.push_toast(err, true);
                                }
                            }
                            ui.close();
                        }
                        if menu_item(ui, "Save Incremental Copy (_vN)", "") {
                            let has_path =
                                app.active_tab().is_some_and(|t| t.source_path.is_some());
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
                            I18nKey::FileExport.tr(lang),
                            shortcut_label(profile, ShortcutAction::ExportFile),
                        ) {
                            app.export_dialog.open = true;
                            ui.close();
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            I18nKey::PrintSendButton.tr(lang),
                            &shortcut_label_for(
                                profile,
                                &app.custom_shortcuts,
                                ShortcutAction::PrintDocument,
                            ),
                        ) {
                            app.handle_shortcut(ShortcutAction::PrintDocument);
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::FilePrintPreview.tr(lang), "") {
                            app.show_print_preview_dialog = true;
                            app.refresh_system_printers_async();
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::FilePreflight.tr(lang), "") {
                            app.run_preflight_active();
                            app.production_state.sub_section = 0;
                            app.focus_docker(DockerTab::Production);
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::FileCollectForOutput.tr(lang), "") {
                            app.production_state.sub_section = 4;
                            app.focus_docker(DockerTab::Production);
                            ui.close();
                        }
                    });

                    // 2. Edit
                    ui.menu_button(I18nKey::MenuEdit.tr(lang), |ui| {
                        if menu_item(
                            ui,
                            I18nKey::EditUndo.tr(lang),
                            shortcut_label(profile, ShortcutAction::Undo),
                        ) {
                            let _ = app.undo();
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            I18nKey::EditRedo.tr(lang),
                            shortcut_label(profile, ShortcutAction::Redo),
                        ) {
                            let _ = app.redo();
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            I18nKey::EditRepeatLast.tr(lang),
                            shortcut_label(profile, ShortcutAction::RepeatLastCommand),
                        ) {
                            app.repeat_last_command();
                            ui.close();
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            I18nKey::EditCut.tr(lang),
                            shortcut_label(profile, ShortcutAction::Cut),
                        ) {
                            app.cut_selection();
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            I18nKey::EditCopy.tr(lang),
                            shortcut_label(profile, ShortcutAction::Copy),
                        ) {
                            app.copy_selection();
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            I18nKey::EditPaste.tr(lang),
                            shortcut_label(profile, ShortcutAction::Paste),
                        ) {
                            app.paste_clipboard();
                            ui.close();
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            I18nKey::EditDuplicate.tr(lang),
                            shortcut_label(profile, ShortcutAction::Duplicate),
                        ) {
                            let _ = app.duplicate_selection();
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            I18nKey::EditDuplicateInPlace.tr(lang),
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
                            .add_enabled(has_sel, egui::Button::new(I18nKey::EditClone.tr(lang)))
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
                            I18nKey::EditDelete.tr(lang),
                            shortcut_label(profile, ShortcutAction::DeleteSelection),
                        ) {
                            app.delete_selection();
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            I18nKey::EditSelectAll.tr(lang),
                            shortcut_label(profile, ShortcutAction::SelectAll),
                        ) {
                            app.select_all();
                            ui.close();
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            I18nKey::EditFindAndReplace.tr(lang),
                            shortcut_label(profile, ShortcutAction::OpenFindReplace),
                        ) {
                            app.focus_docker(DockerTab::FindReplace);
                            ui.close();
                        }
                    });

                    // 3. View
                    ui.menu_button(I18nKey::MenuView.tr(lang), |ui| {
                        if let Some(tab) = app.active_tab_mut() {
                            for (mode, label) in [
                                (
                                    ViewMode::SimpleWireframe,
                                    I18nKey::ViewSimpleWireframe.tr(lang),
                                ),
                                (ViewMode::Wireframe, I18nKey::ViewWireframe.tr(lang)),
                                (ViewMode::Normal, I18nKey::ViewNormal.tr(lang)),
                                (ViewMode::Enhanced, I18nKey::ViewEnhanced.tr(lang)),
                                (ViewMode::Pixels, I18nKey::ViewPixels.tr(lang)),
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
                        ui.checkbox(
                            &mut app.show_navigator_popup,
                            I18nKey::ViewNavigator.tr(lang),
                        );
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
                            if ui
                                .checkbox(&mut multipage, I18nKey::ViewMultipage.tr(lang))
                                .changed()
                            {
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
                                .checkbox(
                                    &mut tab.render_options.show_guidelines,
                                    "Show Guidelines",
                                )
                                .changed()
                            {
                                tab.needs_rerender = true;
                            }
                            if ui
                                .checkbox(
                                    &mut tab.render_options.show_bleed_guides,
                                    "Show Bleed Box",
                                )
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
                            let (pw, ph) = tab
                                .document
                                .active_page()
                                .map_or((210.0, 297.0), |p| (p.width_mm, p.height_mm));
                            ui.menu_button(I18nKey::ViewPerspectiveMode.tr(lang), |ui| {
                                for (mode, label) in [
                                    (
                                        coree_core::PerspectiveGridMode::OnePoint,
                                        I18nKey::ViewPerspectiveOnePoint.tr(lang),
                                    ),
                                    (
                                        coree_core::PerspectiveGridMode::TwoPoint,
                                        I18nKey::ViewPerspectiveTwoPoint.tr(lang),
                                    ),
                                    (
                                        coree_core::PerspectiveGridMode::ThreePoint,
                                        I18nKey::ViewPerspectiveThreePoint.tr(lang),
                                    ),
                                    (coree_core::PerspectiveGridMode::Isometric, "Isometric"),
                                ] {
                                    if ui.selectable_label(pgrid.mode == mode, label).clicked() {
                                        let mut next = pgrid.clone();
                                        next.mode = mode;
                                        next.enabled = true;
                                        let hy = next.horizon_y_mm;
                                        next.vanishing_points = match mode {
                                            coree_core::PerspectiveGridMode::OnePoint => {
                                                vec![coree_core::Point2D::new(pw * 0.5, hy)]
                                            }
                                            coree_core::PerspectiveGridMode::TwoPoint
                                            | coree_core::PerspectiveGridMode::Isometric => vec![
                                                coree_core::Point2D::new(-40.0, hy),
                                                coree_core::Point2D::new(pw + 40.0, hy),
                                            ],
                                            coree_core::PerspectiveGridMode::ThreePoint => vec![
                                                coree_core::Point2D::new(-40.0, hy),
                                                coree_core::Point2D::new(pw + 40.0, hy),
                                                coree_core::Point2D::new(pw * 0.5, ph + 140.0),
                                            ],
                                        };
                                        pending_pgrid = Some(next);
                                        tab.needs_rerender = true;
                                        ui.close();
                                    }
                                }
                            });
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
                            ui.menu_button(I18nKey::ViewSnapTo.tr(lang), |ui| {
                                ui.checkbox(
                                    &mut tab.snap_options.objects,
                                    I18nKey::ViewSnapObjects.tr(lang),
                                );
                                ui.checkbox(
                                    &mut tab.snap_options.nodes,
                                    I18nKey::ViewSnapNodes.tr(lang),
                                );
                                ui.checkbox(
                                    &mut tab.snap_options.guidelines,
                                    I18nKey::ViewSnapGuidelines.tr(lang),
                                );
                                ui.checkbox(
                                    &mut tab.snap_options.page_edges,
                                    I18nKey::ViewSnapPageEdges.tr(lang),
                                );
                                if ui
                                    .checkbox(
                                        &mut tab.snap_options.grid,
                                        I18nKey::ViewSnapGrid.tr(lang),
                                    )
                                    .changed()
                                {
                                    let mut g = pending_grid.unwrap_or_else(|| tab.document.grid());
                                    g.snap = tab.snap_options.grid;
                                    pending_grid = Some(g);
                                }
                                ui.separator();
                                ui.checkbox(
                                    &mut tab.snap_options.tangent,
                                    I18nKey::ViewSnapTangent.tr(lang),
                                );
                                ui.checkbox(
                                    &mut tab.snap_options.perpendicular,
                                    I18nKey::ViewSnapPerpendicular.tr(lang),
                                );
                                ui.checkbox(
                                    &mut tab.snap_options.quadrant,
                                    I18nKey::ViewSnapQuadrant.tr(lang),
                                );
                                ui.checkbox(
                                    &mut tab.snap_options.intersection,
                                    I18nKey::ViewSnapIntersection.tr(lang),
                                );
                                ui.checkbox(
                                    &mut tab.snap_options.baseline,
                                    I18nKey::ViewSnapBaseline.tr(lang),
                                );
                                ui.checkbox(
                                    &mut tab.snap_options.dynamic_guides,
                                    I18nKey::ViewDynamicGuides.tr(lang),
                                );
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
                    ui.menu_button(I18nKey::MenuLayout.tr(lang), |ui| {
                        if menu_item(ui, I18nKey::LayoutInsertPage.tr(lang), "") {
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
                        if menu_item(ui, I18nKey::LayoutDuplicatePage.tr(lang), "") {
                            if let Some(idx) =
                                app.active_tab().map(|t| t.document.active_page_index())
                            {
                                app.duplicate_page(idx);
                            }
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::LayoutRenamePage.tr(lang), "") {
                            if let Some(tab) = app.active_tab_mut() {
                                let idx = tab.document.active_page_index();
                                if let Some(p) = tab.document.page(idx) {
                                    tab.inline_page_name_buf = p.name.clone();
                                    tab.inline_renaming_page = Some(idx);
                                }
                            }
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::LayoutDeletePage.tr(lang), "") {
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
                        if menu_item(ui, I18nKey::LayoutSwitchOrientation.tr(lang), "") {
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
                                    background_fill: None,
                                    printable_background: None,
                                });
                                if let Some(tab) = app.active_tab_mut() {
                                    tab.needs_rerender = true;
                                }
                            }
                            ui.close();
                        }
                        if let Some((pid, mut facing, mut printable_bg)) = app
                            .active_tab()
                            .and_then(|t| t.document.active_page())
                            .map(|p| (p.id, p.facing_pages, p.printable_background))
                        {
                            if ui
                                .checkbox(&mut facing, I18nKey::LayoutFacingPages.tr(lang))
                                .changed()
                            {
                                let _ = app.execute_command(CommandOp::SetPageProperties {
                                    page_id: pid,
                                    name: None,
                                    width_mm: None,
                                    height_mm: None,
                                    bleed_mm: None,
                                    facing_pages: Some(facing),
                                    background_fill: None,
                                    printable_background: None,
                                });
                                if let Some(tab) = app.active_tab_mut() {
                                    tab.needs_rerender = true;
                                }
                            }
                            if ui
                                .checkbox(
                                    &mut printable_bg,
                                    I18nKey::LayoutPrintableBackground.tr(lang),
                                )
                                .changed()
                            {
                                let bg_fill = app
                                    .active_tab()
                                    .and_then(|t| t.document.active_page())
                                    .map(|p| p.background_fill.clone())
                                    .unwrap_or_else(|| {
                                        coree_core::Fill::Solid(coree_core::Color::WHITE)
                                    });
                                let _ = app.execute_command(CommandOp::SetPageProperties {
                                    page_id: pid,
                                    name: None,
                                    width_mm: None,
                                    height_mm: None,
                                    bleed_mm: None,
                                    facing_pages: None,
                                    background_fill: Some(bg_fill),
                                    printable_background: Some(printable_bg),
                                });
                                if let Some(tab) = app.active_tab_mut() {
                                    tab.needs_rerender = true;
                                }
                            }
                        }
                        if menu_item(ui, I18nKey::LayoutAutofitPage.tr(lang), "") {
                            app.autofit_active_page(5.0);
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::LayoutPageSetup.tr(lang), "") {
                            app.options_tab = 1;
                            app.show_options_dialog = true;
                            ui.close();
                        }
                        ui.separator();
                        if menu_item(ui, I18nKey::LayoutAddMasterAll.tr(lang), "") {
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
                    ui.menu_button(I18nKey::MenuObject.tr(lang), |ui| {
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
                                (ZOrderOp::BringToFront, I18nKey::ObjectOrderToFront.tr(lang)),
                                (
                                    ZOrderOp::BringForward,
                                    I18nKey::ObjectOrderForwardOne.tr(lang),
                                ),
                                (ZOrderOp::SendBackward, I18nKey::ObjectOrderBackOne.tr(lang)),
                                (ZOrderOp::SendToBack, I18nKey::ObjectOrderToBack.tr(lang)),
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
                                I18nKey::ObjectAlignCenterPage.tr(lang),
                                shortcut_label(profile, ShortcutAction::Align(AlignOp::CenterPage)),
                            ) {
                                app.apply_align(AlignOp::CenterPage);
                                ui.close();
                            }
                            ui.separator();
                            for (op, label) in [
                                (AlignOp::Left, I18nKey::ObjectAlignLeft.tr(lang)),
                                (AlignOp::Right, I18nKey::ObjectAlignRight.tr(lang)),
                                (AlignOp::Top, I18nKey::ObjectAlignTop.tr(lang)),
                                (AlignOp::Bottom, I18nKey::ObjectAlignBottom.tr(lang)),
                                (AlignOp::CenterH, I18nKey::ObjectAlignCenterH.tr(lang)),
                                (AlignOp::CenterV, I18nKey::ObjectAlignCenterV.tr(lang)),
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
                                (DistributeOp::SpacingH, I18nKey::ObjectDistributeH.tr(lang)),
                                (DistributeOp::SpacingV, I18nKey::ObjectDistributeV.tr(lang)),
                            ] {
                                if ui.button(label).clicked() {
                                    app.apply_distribute(op);
                                    ui.close();
                                }
                            }
                        });
                        ui.menu_button("Shaping", |ui| {
                            for (op, label) in [
                                (BooleanOp::Weld, I18nKey::ObjectWeld.tr(lang)),
                                (BooleanOp::Trim, I18nKey::ObjectTrim.tr(lang)),
                                (BooleanOp::Intersect, I18nKey::ObjectIntersect.tr(lang)),
                                (BooleanOp::Simplify, I18nKey::ObjectSimplify.tr(lang)),
                                (
                                    BooleanOp::FrontMinusBack,
                                    I18nKey::ObjectFrontMinusBack.tr(lang),
                                ),
                                (
                                    BooleanOp::BackMinusFront,
                                    I18nKey::ObjectBackMinusFront.tr(lang),
                                ),
                                (
                                    BooleanOp::CreateBoundary,
                                    I18nKey::ObjectCreateBoundary.tr(lang),
                                ),
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
                            I18nKey::ObjectCombine.tr(lang),
                            shortcut_label(profile, ShortcutAction::Combine),
                        ) {
                            app.apply_boolean_op(BooleanOp::Combine);
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            I18nKey::ObjectBreakApart.tr(lang),
                            shortcut_label(profile, ShortcutAction::BreakApart),
                        ) {
                            app.break_apart_selection();
                            ui.close();
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            I18nKey::ObjectGroup.tr(lang),
                            shortcut_label(profile, ShortcutAction::Group),
                        ) {
                            app.group_selection();
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            I18nKey::ObjectUngroup.tr(lang),
                            shortcut_label(profile, ShortcutAction::Ungroup),
                        ) {
                            app.ungroup_selection();
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            I18nKey::ObjectConvertToCurves.tr(lang),
                            shortcut_label(profile, ShortcutAction::ConvertToCurves),
                        ) {
                            app.convert_selection_to_curves();
                            ui.close();
                        }
                        if menu_item(
                            ui,
                            I18nKey::ObjectConvertOutlineToObject.tr(lang),
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
                            if ui
                                .button(I18nKey::ObjectPowerClipPlaceInside.tr(lang))
                                .clicked()
                            {
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
                                    egui::Button::new(I18nKey::ObjectPowerClipEdit.tr(lang)),
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
                                    egui::Button::new(I18nKey::ObjectPowerClipFinish.tr(lang)),
                                )
                                .clicked()
                            {
                                app.exit_powerclip_edit();
                                ui.close();
                            }
                            if ui
                                .button(I18nKey::ObjectPowerClipExtract.tr(lang))
                                .clicked()
                            {
                                app.extract_selected_powerclip();
                                ui.close();
                            }
                            if ui
                                .button(I18nKey::ObjectPowerClipFitContents.tr(lang))
                                .clicked()
                            {
                                app.fit_powerclip_selection(
                                    coree_core::PowerClipFitMode::FitContents,
                                );
                                ui.close();
                            }
                            if ui
                                .button(I18nKey::ObjectPowerClipCenterContents.tr(lang))
                                .clicked()
                            {
                                app.fit_powerclip_selection(
                                    coree_core::PowerClipFitMode::CenterContents,
                                );
                                ui.close();
                            }
                            if ui
                                .button(I18nKey::ObjectPowerClipStretchToFill.tr(lang))
                                .clicked()
                            {
                                app.fit_powerclip_selection(
                                    coree_core::PowerClipFitMode::StretchToFill,
                                );
                                ui.close();
                            }
                            if menu_item(
                                ui,
                                I18nKey::ObjectPowerClipLockContents.tr(lang),
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
                            I18nKey::ObjectMoveToDesktop.tr(lang),
                            &shortcut_label_for(
                                profile,
                                &app.custom_shortcuts,
                                ShortcutAction::MoveToDesktop,
                            ),
                        ) {
                            app.move_selection_to_desktop_layer();
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::ObjectLock.tr(lang), "") {
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
                    ui.menu_button(I18nKey::MenuEffects.tr(lang), |ui| {
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
                            I18nKey::DockerLens.tr(lang),
                            shortcut_label(profile, ShortcutAction::ToggleDockerLens),
                        ) {
                            app.focus_docker(DockerTab::Lens);
                            ui.close();
                        }
                        ui.separator();
                        for (idx, name) in [
                            I18nKey::EffectDropShadow.tr(lang),
                            I18nKey::EffectBlockShadow.tr(lang),
                            I18nKey::EffectContour.tr(lang),
                            I18nKey::EffectBlend.tr(lang),
                            I18nKey::EffectDistort.tr(lang),
                            I18nKey::EffectEnvelope.tr(lang),
                            I18nKey::EffectExtrude25D.tr(lang),
                            "Transparency",
                            I18nKey::EffectPerspective.tr(lang),
                            I18nKey::EffectSymmetry.tr(lang),
                            I18nKey::EffectInnerShadow.tr(lang),
                            I18nKey::EffectBevel.tr(lang),
                            I18nKey::EffectExtrude3D.tr(lang),
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
                    ui.menu_button(I18nKey::MenuBitmaps.tr(lang), |ui| {
                        if menu_item(ui, I18nKey::BitmapConvertToBitmap.tr(lang), "") {
                            app.convert_selection_to_bitmap();
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::DockerBitmapStudio.tr(lang), "") {
                            app.focus_docker(DockerTab::BitmapStudio);
                            ui.close();
                        }
                        ui.separator();
                        if menu_item(ui, I18nKey::DockerVectorTrace.tr(lang), "") {
                            app.focus_docker(DockerTab::VectorTrace);
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::BitmapTraceVector.tr(lang), "") {
                            app.trace_state.mode_idx = 0;
                            if let Err(e) = app.trace_selected_raster() {
                                app.push_toast(e, true);
                            }
                            ui.close();
                        }
                    });

                    // 8. Text
                    ui.menu_button(I18nKey::MenuText.tr(lang), |ui| {
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
                        if menu_item(ui, I18nKey::TextFitToPath.tr(lang), "") {
                            app.fit_selected_text_to_path();
                            ui.close();
                        }
                        if menu_item(ui, "Insert Arabic RTL Sample Heading", "") {
                            app.tool_settings.text_insert_draft = "استوديو كوري للتصميم".to_owned();
                            app.default_text_style.direction = coree_core::TextDirection::Rtl;
                            app.default_text_style.align =
                                coree_core::TextAlign::ForceJustifyKashida;
                            app.default_text_style.kashida_stretch = 0.65;
                            app.insert_artistic_text_at(coree_core::Point2D::new(30.0, 45.0));
                            ui.close();
                        }
                        ui.separator();
                        if menu_item(ui, I18nKey::DockerFontManager.tr(lang), "") {
                            app.refresh_font_manager(false);
                            app.focus_docker(DockerTab::FontManager);
                            ui.close();
                        }
                    });

                    // 9. Table
                    ui.menu_button(I18nKey::MenuTable.tr(lang), |ui| {
                        if menu_item(ui, "Insert 3x3 Table", "") {
                            app.insert_table(3, 3);
                            ui.close();
                        }
                        if menu_item(ui, "Insert 5x4 Specification Table", "") {
                            app.insert_table(5, 4);
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::ObjectConvertToCurves.tr(lang), "") {
                            app.convert_selection_to_curves();
                            ui.close();
                        }
                    });

                    // 10. Tools
                    ui.menu_button(I18nKey::MenuTools.tr(lang), |ui| {
                        if menu_item(
                            ui,
                            I18nKey::ToolsOptions.tr(lang),
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
                            I18nKey::ToolsCommandPalette.tr(lang),
                            shortcut_label(profile, ShortcutAction::ToggleCommandPalette),
                        ) {
                            app.handle_shortcut(ShortcutAction::ToggleCommandPalette);
                            ui.close();
                        }
                        ui.separator();
                        let recording = app.active_tab().is_some_and(|t| t.history.recording_macro);
                        if recording {
                            if menu_item(ui, I18nKey::ToolsStopMacro.tr(lang), "") {
                                app.stop_macro_recording();
                                ui.close();
                            }
                        } else if menu_item(ui, I18nKey::ToolsRecordMacro.tr(lang), "") {
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
                        if menu_item(ui, I18nKey::ToolsMacros.tr(lang), "") {
                            app.focus_docker(DockerTab::Macros);
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::ToolsPlugins.tr(lang), "") {
                            let dirs = default_plugin_dirs();
                            if let Some(dir) = dirs.first() {
                                let _ = std::fs::create_dir_all(dir);
                                app.push_toast(format!("Plugins folder: {}", dir.display()), false);
                            }
                            app.options_tab = 3;
                            app.show_options_dialog = true;
                            ui.close();
                        }
                        if menu_item(ui, I18nKey::DockerCdrInspector.tr(lang), "") {
                            app.refresh_inspector_for_active(false);
                            app.focus_docker(DockerTab::CdrInspector);
                            ui.close();
                        }
                        ui.separator();
                        ui.menu_button(I18nKey::ToolsShortcuts.tr(lang), |ui| {
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
                    ui.menu_button(I18nKey::MenuProduction.tr(lang), |ui| {
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
                    ui.menu_button(I18nKey::MenuWindow.tr(lang), |ui| {
                        ui.menu_button(I18nKey::WindowWorkspaces.tr(lang), |ui| {
                            for preset in WorkspacePreset::ALL {
                                let active = app.active_workspace_preset == preset;
                                if ui
                                    .selectable_label(
                                        active,
                                        crate::i18n::workspace_i18n_key(preset).tr_ui(lang),
                                    )
                                    .clicked()
                                {
                                    app.apply_workspace_preset(preset);
                                    ui.close();
                                }
                            }
                            ui.separator();
                            if menu_item(ui, I18nKey::WindowExportWorkspace.tr(lang), "") {
                                export_workspace_dialog(app);
                                ui.close();
                            }
                            if menu_item(ui, I18nKey::WindowImportWorkspace.tr(lang), "") {
                                import_workspace_dialog(app);
                                ui.close();
                            }
                            if menu_item(ui, I18nKey::WindowResetWorkspace.tr(lang), "") {
                                app.apply_workspace_preset(WorkspacePreset::DefaultClassic);
                                ui.close();
                            }
                        });
                        ui.menu_button(I18nKey::WindowShowDockers.tr(lang), |ui| {
                            ui.checkbox(&mut app.show_dockers, I18nKey::WindowShowDockers.tr(lang));
                            ui.checkbox(
                                &mut app.toolbox_two_column,
                                I18nKey::WindowTwoColumnToolbox.tr(lang),
                            );
                            ui.separator();
                            let laser_focused = app.production_state.sub_section == 5
                                && app.dock_state.find_tab(&DockerTab::Production).is_some();
                            if ui
                                .selectable_label(
                                    laser_focused,
                                    format!("⚡ {}", I18nKey::ToolsPlugins.tr_ui(lang)),
                                )
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
                                if ui
                                    .selectable_label(
                                        present,
                                        crate::i18n::docker_i18n_key(docker).tr_ui(lang),
                                    )
                                    .clicked()
                                {
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
                        ui.menu_button(I18nKey::WindowTheme.tr(lang), |ui| {
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
                        ui.menu_button(I18nKey::WindowLanguage.tr(lang), |ui| {
                            for l in StudioLanguage::ALL {
                                if ui
                                    .selectable_label(app.ui_language == l, l.label())
                                    .clicked()
                                {
                                    app.set_ui_language(l);
                                    ui.close();
                                }
                            }
                        });
                    });

                    // 13. Help
                    ui.menu_button(I18nKey::MenuHelp.tr(lang), |ui| {
                        if menu_item(ui, "About coree & Clean-Room Notice", "") {
                            app.show_about_dialog = true;
                            ui.close();
                        }
                    });

                    // Opposite-aligned quick Command Palette + Language toggle + Proof/Offline indicators
                    ui.with_layout(
                        shell_dir.opposite_horizontal_layout(egui::Align::Center),
                        |ui| {
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
                                    "⌘ {} ({})",
                                    I18nKey::CommandPaletteTitle.tr_ui(lang),
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
                                    RichText::new(format!(
                                        "● {}",
                                        I18nKey::ViewSoftProofCmyk.tr_ui(lang)
                                    ))
                                    .size(11.0)
                                    .color(tokens.warning),
                                );
                            }
                            ui.label(
                                RichText::new("● Offline · Local WAL")
                                    .size(11.0)
                                    .color(tokens.success),
                            );
                        },
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
                shell_dir.with_horizontal(ui, |ui| {
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
                        .on_hover_text(I18nKey::FileNew.tr_ui(lang))
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
