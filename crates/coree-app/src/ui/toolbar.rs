// SPDX-License-Identifier: MIT OR Apache-2.0

//! Crisp Standard Studio Toolbar (with procedural clean-room vector icons and
//! Laser Quick-Send strip) + Authentic Context-Sensitive Property Bar (`36px`)
//! adapting live to active tool and selected object(s).

use crate::i18n::I18nKey;
use crate::shortcuts::ShortcutAction;
use crate::state::{
    DockerTab, LensDockerState, NodeSmoothness, PropertiesSubTab, RasterBrushOp, StudioApp, ToolId,
};
use crate::theme::{core_color_to_egui, egui_color_to_core, ThemeTokens, PROPERTY_BAR_HEIGHT};
use coree_core::{
    Affine2D, AlignOp, ArtisticMediaStyle, ArtisticWidthProfile, BasicShapeKind, BlendMode,
    BooleanOp, CommandOp, ContourMode, CornerStyle, DistortKind, EllipseMode, EnvelopeMode, Fill,
    GradientStop, ImpactStyle, NodePayload, ParametricShape, Point2D, RasterMaskMode, Rect2D,
    TextAlign, TextDirection, Unit, ZOrderOp,
};
use coree_render::{ToneRange, ViewMode};
use egui::{
    pos2, vec2, Color32, ComboBox, CornerRadius, DragValue, Frame, Margin, Rect, RichText, Sense,
    Shape, Stroke, StrokeKind, Ui,
};

mod icons;
mod page_defaults;
#[cfg(test)]
mod tests;
mod tool_bars;
use icons::*;

/// Renders both the Standard Toolbar (Row 1) and the Context-Sensitive Property Bar (Row 2).
pub fn show_toolbars(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);

    // Row 1: Standard Studio Toolbar (compact zero-scrollbar layout fitting >= 960px windows)
    egui::Panel::top("coree_standard_toolbar")
        .exact_size(32.0)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(6, 4))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("std_toolbar_scroll")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    shell_dir.with_horizontal(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;

                        // File operations with crisp procedural vector icons (`ToolbarConfig`)
                        if app.toolbar_config.show_file_ops {
                            if std_icon_btn(
                                ui,
                                &tokens,
                                StdIcon::NewDoc,
                                &I18nKey::FileNew.tr_ui(lang),
                                true,
                                false,
                            ) {
                                app.new_doc_dialog.open = true;
                            }
                            if std_icon_btn(
                                ui,
                                &tokens,
                                StdIcon::OpenDoc,
                                &I18nKey::FileOpen.tr_ui(lang),
                                true,
                                false,
                            ) {
                                app.open_file_dialog();
                            }
                            if std_icon_btn(
                                ui,
                                &tokens,
                                StdIcon::SaveDoc,
                                &I18nKey::FileSave.tr_ui(lang),
                                true,
                                false,
                            ) {
                                app.handle_shortcut(ShortcutAction::SaveDocument);
                            }
                            if std_icon_btn(
                                ui,
                                &tokens,
                                StdIcon::ImportDoc,
                                &I18nKey::FileImport.tr_ui(lang),
                                true,
                                false,
                            ) {
                                app.import_file_dialog();
                            }
                            if std_icon_btn(
                                ui,
                                &tokens,
                                StdIcon::ExportDoc,
                                &I18nKey::FileExport.tr_ui(lang),
                                true,
                                false,
                            ) {
                                app.export_dialog.open = true;
                            }
                            ui.separator();
                        }

                        // History & Clipboard
                        let (can_undo, can_redo, has_sel) =
                            app.active_tab().map_or((false, false, false), |t| {
                                (
                                    t.history.can_undo(),
                                    t.history.can_redo(),
                                    !t.selection.is_empty(),
                                )
                            });
                        if app.toolbar_config.show_undo_redo {
                            if std_icon_btn(
                                ui,
                                &tokens,
                                StdIcon::Undo,
                                &I18nKey::EditUndo.tr_ui(lang),
                                can_undo,
                                false,
                            ) {
                                let _ = app.undo();
                            }
                            if std_icon_btn(
                                ui,
                                &tokens,
                                StdIcon::Redo,
                                &I18nKey::EditRedo.tr_ui(lang),
                                can_redo,
                                false,
                            ) {
                                let _ = app.redo();
                            }
                            ui.separator();
                        }
                        if app.toolbar_config.show_clipboard {
                            if std_icon_btn(
                                ui,
                                &tokens,
                                StdIcon::Cut,
                                &I18nKey::EditCut.tr_ui(lang),
                                has_sel,
                                false,
                            ) {
                                app.cut_selection();
                            }
                            if std_icon_btn(
                                ui,
                                &tokens,
                                StdIcon::Copy,
                                &I18nKey::EditCopy.tr_ui(lang),
                                has_sel,
                                false,
                            ) {
                                app.copy_selection();
                            }
                            if std_icon_btn(
                                ui,
                                &tokens,
                                StdIcon::Paste,
                                &I18nKey::EditPaste.tr_ui(lang),
                                app.has_clipboard_content(),
                                false,
                            ) {
                                app.paste_clipboard();
                            }
                            ui.separator();
                        }

                        // Zoom preset combo + F9 Full-Screen Preview
                        if app.toolbar_config.show_zoom {
                            let zoom_pct = app
                                .active_tab()
                                .map_or(100.0, |t| (t.viewport.zoom * 100.0).round());
                            ComboBox::from_id_salt("std_zoom_preset")
                                .width(68.0)
                                .selected_text(format!("{zoom_pct:.0}%"))
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_label(
                                            false,
                                            I18nKey::TbZoomToSelectedShort.tr_ui(lang),
                                        )
                                        .clicked()
                                    {
                                        app.handle_shortcut(ShortcutAction::ZoomToSelected);
                                    }
                                    if ui
                                        .selectable_label(
                                            false,
                                            I18nKey::TbZoomToPageShort.tr_ui(lang),
                                        )
                                        .clicked()
                                    {
                                        app.handle_shortcut(ShortcutAction::ZoomToPage);
                                    }
                                    if ui
                                        .selectable_label(
                                            false,
                                            I18nKey::TbZoomToAllShort.tr_ui(lang),
                                        )
                                        .clicked()
                                    {
                                        app.handle_shortcut(ShortcutAction::ZoomToAll);
                                    }
                                    ui.separator();
                                    for pct in [25.0, 50.0, 75.0, 100.0, 150.0, 200.0, 400.0] {
                                        if ui
                                            .selectable_label(false, format!("{pct:.0}%"))
                                            .clicked()
                                        {
                                            if let Some(t) = app.active_tab_mut() {
                                                t.viewport.zoom = pct / 100.0;
                                                t.needs_rerender = true;
                                            }
                                        }
                                    }
                                });

                            if std_icon_btn(
                                ui,
                                &tokens,
                                StdIcon::PreviewF9,
                                &I18nKey::ViewFullScreenPreview.tr_ui(lang),
                                true,
                                app.full_screen_preview,
                            ) {
                                app.handle_shortcut(ShortcutAction::ToggleFullScreenPreview);
                            }
                        }

                        let tb_cfg = app.toolbar_config.clone();
                        let mut pending_multipage: Option<bool> = None;
                        let mut pending_facing: Option<(coree_core::PageId, bool)> = None;
                        let mut pending_scale: Option<coree_core::DrawingScale> = None;
                        let mut pending_grid: Option<coree_core::GridConfig> = None;
                        if let Some(tab) = app.active_tab_mut() {
                            if tb_cfg.show_view_mode {
                                ComboBox::from_id_salt("std_view_mode")
                                    .width(92.0)
                                    .selected_text(format!("{:?}", tab.view_mode))
                                    .show_ui(ui, |ui| {
                                        for (mode, label) in [
                                            (
                                                ViewMode::SimpleWireframe,
                                                I18nKey::ViewSimpleWireframe.tr_ui(lang),
                                            ),
                                            (
                                                ViewMode::Wireframe,
                                                I18nKey::ViewWireframe.tr_ui(lang),
                                            ),
                                            (ViewMode::Normal, I18nKey::ViewNormal.tr_ui(lang)),
                                            (ViewMode::Enhanced, I18nKey::ViewEnhanced.tr_ui(lang)),
                                            (ViewMode::Pixels, I18nKey::ViewPixels.tr_ui(lang)),
                                        ] {
                                            if ui
                                                .selectable_value(&mut tab.view_mode, mode, label)
                                                .changed()
                                            {
                                                tab.needs_rerender = true;
                                            }
                                        }
                                    });
                            }

                            if tb_cfg.show_layout_toggles {
                                if ui
                                    .selectable_label(tab.document.multipage_view(), "⊞")
                                    .on_hover_text(I18nKey::ViewMultipage.tr_ui(lang))
                                    .clicked()
                                {
                                    let next = !tab.document.multipage_view();
                                    pending_multipage = Some(next);
                                    tab.viewport.multipage_view = next;
                                    tab.needs_rerender = true;
                                }
                                if let Some(page) = tab.document.active_page() {
                                    if ui
                                        .selectable_label(
                                            page.facing_pages,
                                            I18nKey::TbFacingShort.tr_ui(lang),
                                        )
                                        .on_hover_text(I18nKey::LayoutFacingPages.tr_ui(lang))
                                        .clicked()
                                    {
                                        pending_facing = Some((page.id, !page.facing_pages));
                                        tab.needs_rerender = true;
                                    }
                                }
                            }

                            if tb_cfg.show_drawing_scale {
                                let cur_scale = tab.document.drawing_scale();
                                ComboBox::from_id_salt("std_drawing_scale")
                                    .width(58.0)
                                    .selected_text(cur_scale.label())
                                    .show_ui(ui, |ui| {
                                        for (num, den) in [
                                            (1.0, 1.0),
                                            (1.0, 5.0),
                                            (1.0, 10.0),
                                            (1.0, 20.0),
                                            (1.0, 50.0),
                                            (1.0, 100.0),
                                            (2.0, 1.0),
                                        ] {
                                            let cand = coree_core::DrawingScale::new(num, den);
                                            if ui
                                                .selectable_label(cur_scale == cand, cand.label())
                                                .clicked()
                                            {
                                                pending_scale = Some(cand);
                                                tab.needs_rerender = true;
                                            }
                                        }
                                    });
                            }

                            // Consolidated Snap popover button (replaces 4 sprawling controls)
                            if tb_cfg.show_snap_toggles {
                                ui.separator();
                                let so = &tab.snap_options;
                                let snap_count = [
                                    so.objects,
                                    so.guidelines,
                                    so.grid,
                                    so.nodes,
                                    so.page_edges,
                                    so.tangent,
                                    so.perpendicular,
                                    so.quadrant,
                                    so.intersection,
                                    so.baseline,
                                ]
                                .into_iter()
                                .filter(|&on| on)
                                .count();
                                let snap_btn_lbl = if snap_count > 0 {
                                    RichText::new(format!(
                                        "{} ({snap_count}) ▾",
                                        I18nKey::ViewSnapTo.tr_ui(lang)
                                    ))
                                    .color(tokens.accent)
                                } else {
                                    RichText::new(format!("{} ▾", I18nKey::ViewSnapTo.tr_ui(lang)))
                                        .color(tokens.text_muted)
                                };
                                ui.menu_button(snap_btn_lbl, |ui| {
                                    ui.set_min_width(188.0);
                                    ui.checkbox(
                                        &mut tab.snap_options.objects,
                                        I18nKey::ViewSnapObjects.tr_ui(lang),
                                    );
                                    ui.checkbox(
                                        &mut tab.snap_options.guidelines,
                                        I18nKey::ViewSnapGuidelines.tr_ui(lang),
                                    );
                                    ui.checkbox(
                                        &mut tab.snap_options.grid,
                                        I18nKey::ViewSnapGrid.tr_ui(lang),
                                    );
                                    ui.checkbox(
                                        &mut tab.snap_options.nodes,
                                        I18nKey::ViewSnapNodes.tr_ui(lang),
                                    );
                                    ui.checkbox(
                                        &mut tab.snap_options.page_edges,
                                        I18nKey::ViewSnapPageEdges.tr_ui(lang),
                                    );
                                    ui.separator();
                                    ui.checkbox(
                                        &mut tab.snap_options.tangent,
                                        I18nKey::ViewSnapTangent.tr_ui(lang),
                                    );
                                    ui.checkbox(
                                        &mut tab.snap_options.perpendicular,
                                        I18nKey::ViewSnapPerpendicular.tr_ui(lang),
                                    );
                                    ui.checkbox(
                                        &mut tab.snap_options.quadrant,
                                        I18nKey::ViewSnapQuadrant.tr_ui(lang),
                                    );
                                    ui.checkbox(
                                        &mut tab.snap_options.intersection,
                                        I18nKey::ViewSnapIntersection.tr_ui(lang),
                                    );
                                    ui.checkbox(
                                        &mut tab.snap_options.baseline,
                                        I18nKey::ViewSnapBaseline.tr_ui(lang),
                                    );
                                    ui.separator();
                                    ui.checkbox(
                                        &mut tab.snap_options.dynamic_guides,
                                        I18nKey::ViewDynamicGuides.tr_ui(lang),
                                    );
                                });
                                if tab.snap_options.grid != tab.document.grid().snap {
                                    let mut g = tab.document.grid();
                                    g.snap = tab.snap_options.grid;
                                    pending_grid = Some(g);
                                }
                            }
                        }
                        if pending_multipage.is_some() || pending_scale.is_some() {
                            let _ = app.execute_command(CommandOp::SetDocumentProperties {
                                title: None,
                                color_mode: None,
                                default_unit: None,
                                dpi: None,
                                multipage_view: pending_multipage,
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
                                drawing_scale: pending_scale,
                            });
                        }
                        if let Some((page_id, facing)) = pending_facing {
                            let _ = app.execute_command(CommandOp::SetPageProperties {
                                page_id,
                                name: None,
                                width_mm: None,
                                height_mm: None,
                                bleed_mm: None,
                                facing_pages: Some(facing),
                                background_fill: None,
                                printable_background: None,
                            });
                        }
                        if let Some(config) = pending_grid {
                            let _ = app.execute_command(CommandOp::SetGridConfig { config });
                        }

                        ui.separator();

                        // CMYK Soft-Proof toggle
                        if ui
                            .selectable_label(
                                app.soft_proof_cmyk,
                                I18nKey::ViewSoftProofCmyk.tr_ui(lang),
                            )
                            .on_hover_text(I18nKey::ViewSoftProofCmyk.tr_ui(lang))
                            .clicked()
                        {
                            app.soft_proof_cmyk = !app.soft_proof_cmyk;
                            app.sync_proof_options_to_tabs();
                        }

                        ui.separator();

                        // Consolidated Laser & Plotter Quick-Send Menu + CutContour + Macro Record
                        let laser_lbl =
                            RichText::new(format!("{} ▾", I18nKey::TbLaserPreview.tr_ui(lang)))
                                .color(if app.show_laser_toolpath_overlay {
                                    tokens.accent_secondary
                                } else {
                                    tokens.text
                                });
                        ui.menu_button(laser_lbl, |ui| {
                            ui.set_min_width(184.0);
                            let mut preview_on = app.show_laser_toolpath_overlay;
                            if ui
                                .checkbox(&mut preview_on, I18nKey::TbLaserPreview.tr_ui(lang))
                                .on_hover_text(I18nKey::TbLaserPreviewTip.tr_ui(lang))
                                .changed()
                            {
                                if preview_on {
                                    app.refresh_laser_plan_preview();
                                } else {
                                    app.show_laser_toolpath_overlay = false;
                                }
                            }
                            ui.separator();
                            for (plugin_id, tip) in [
                                ("lightburn-bridge", I18nKey::MenuSendLightBurn.tr_ui(lang)),
                                ("rdworks-ruida", I18nKey::MenuSendRdWorks.tr_ui(lang)),
                                ("ezcad-fiber", I18nKey::MenuSendEzCad.tr_ui(lang)),
                                (
                                    "vinyl-plotter-hpgl",
                                    I18nKey::MenuSendVinylPlotter.tr_ui(lang),
                                ),
                            ] {
                                if ui.button(tip).clicked() {
                                    app.send_to_laser_plugin(plugin_id, true);
                                    ui.close();
                                }
                            }
                        });

                        if ui
                            .button(
                                RichText::new(I18nKey::TbCutContourBtn.tr_ui(lang))
                                    .size(11.0)
                                    .color(tokens.cutcontour),
                            )
                            .on_hover_text(I18nKey::TbCutContourTip.tr_ui(lang))
                            .clicked()
                        {
                            app.generate_cut_contour_active();
                        }

                        let recording = app.active_tab().is_some_and(|t| t.history.recording_macro);
                        if recording {
                            if ui
                                .button(
                                    RichText::new(format!(
                                        "■ {}",
                                        I18nKey::ToolsStopMacro.tr_ui(lang)
                                    ))
                                    .size(11.0)
                                    .color(tokens.accent_secondary),
                                )
                                .clicked()
                            {
                                app.stop_macro_recording();
                            }
                        } else if ui
                            .small_button(format!("● {}", I18nKey::DockerMacros.tr_ui(lang)))
                            .on_hover_text(I18nKey::TbRecordMacroTip.tr_ui(lang))
                            .clicked()
                        {
                            app.start_macro_recording();
                        }
                    });
                });
        });

    // Row 2: Authentic Context-Sensitive Property Bar (36px, smooth hidden scrollbar)
    egui::Panel::top("coree_property_bar")
        .exact_size(PROPERTY_BAR_HEIGHT)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(8, 4))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("prop_bar_scroll")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    shell_dir.with_horizontal(ui, |ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        show_context_property_contents(ui, app);
                    });
                });
        });
}

fn show_context_property_contents(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let lang = app.ui_language;

    // 1. Dedicated Tool Property Bars (takes priority regardless of selection count)
    match app.active_tool {
        ToolId::ShapeNode => {
            tool_bars::show_shape_node_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::Smooth | ToolId::Smear | ToolId::Twirl | ToolId::AttractRepel | ToolId::Roughen => {
            tool_bars::show_sculpt_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::BitmapBrush
        | ToolId::BitmapEraser
        | ToolId::CloneStamp
        | ToolId::Smudge
        | ToolId::MaskSelect => {
            tool_bars::show_raster_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::LensTool => {
            tool_bars::show_lens_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::DropShadow
        | ToolId::BlockShadow
        | ToolId::Contour
        | ToolId::Blend
        | ToolId::Distort
        | ToolId::Envelope
        | ToolId::Extrude25D
        | ToolId::Transparency => {
            tool_bars::show_interactive_effect_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::InteractiveFill | ToolId::MeshFill | ToolId::SmartFill => {
            tool_bars::show_fill_tool_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
            tool_bars::show_eyedropper_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::Crop | ToolId::Knife | ToolId::VirtualSegmentDelete | ToolId::Eraser => {
            tool_bars::show_crop_cut_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::Zoom | ToolId::Pan => {
            tool_bars::show_zoom_pan_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::ArtisticText | ToolId::ParagraphText => {
            tool_bars::show_text_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::TwoPointLine => {
            ui.label(
                RichText::new(I18nKey::TbTwoPointLineTitle.tr_ui(lang))
                    .color(tokens.accent)
                    .strong(),
            );
            ui.add(
                DragValue::new(&mut app.default_outline.width_mm)
                    .range(0.0..=50.0)
                    .speed(0.1)
                    .prefix(I18nKey::PropWidth.tr_ui(lang))
                    .suffix(" mm"),
            );
            ui.checkbox(
                &mut app.default_outline.is_hairline,
                I18nKey::PropOutlineHairline.tr_ui(lang),
            );
            ui.label(
                RichText::new(I18nKey::TbTwoPointLineHint.tr_ui(lang))
                    .size(11.0)
                    .color(tokens.text_muted),
            );
            return;
        }
        ToolId::FreeTransform => {
            ui.label(
                RichText::new(I18nKey::TbFreeTransformTitle.tr_ui(lang))
                    .color(tokens.accent)
                    .strong(),
            );
            ui.checkbox(
                &mut app.treat_all_objects_as_filled,
                I18nKey::TbTreatAllFilled.tr_ui(lang),
            );
            if ui.small_button(I18nKey::PropMirrorH.tr_ui(lang)).clicked() {
                if let Some((ids, center)) = app.active_tab().map(|t| {
                    let c = t
                        .selection
                        .bounding_box(&t.document)
                        .map_or(Point2D::ZERO, |b| b.center());
                    (t.selection.objects.clone(), c)
                }) {
                    if !ids.is_empty() {
                        app.apply_world_transform_to_selection(
                            &ids,
                            Affine2D::scale_about(-1.0, 1.0, center),
                        );
                    }
                }
            }
            if ui.small_button(I18nKey::PropMirrorV.tr_ui(lang)).clicked() {
                if let Some((ids, center)) = app.active_tab().map(|t| {
                    let c = t
                        .selection
                        .bounding_box(&t.document)
                        .map_or(Point2D::ZERO, |b| b.center());
                    (t.selection.objects.clone(), c)
                }) {
                    if !ids.is_empty() {
                        app.apply_world_transform_to_selection(
                            &ids,
                            Affine2D::scale_about(1.0, -1.0, center),
                        );
                    }
                }
            }
            if app.active_tab().is_some_and(|t| !t.selection.is_empty()) {
                ui.separator();
            } else {
                return;
            }
        }
        ToolId::Pick | ToolId::FreehandPick
            if app.active_tab().is_some_and(|t| t.selection.is_empty()) =>
        {
            ui.label(
                RichText::new(if app.active_tool == ToolId::FreehandPick {
                    I18nKey::TbFreehandPickTitle.tr_ui(lang)
                } else {
                    I18nKey::TbPickToolTitle.tr_ui(lang)
                })
                .color(tokens.accent)
                .strong(),
            );
            ui.checkbox(
                &mut app.treat_all_objects_as_filled,
                I18nKey::TbTreatAllFilled.tr_ui(lang),
            );
            ui.separator();
        }
        _ => {}
    }

    let sel_ids = app
        .active_tab()
        .map(|t| t.selection.objects.clone())
        .unwrap_or_default();

    // 2. Pick / Drawing Tools with 0 Objects Selected -> Document / Page Defaults Bar
    if sel_ids.is_empty() {
        page_defaults::show_page_defaults_property_bar(ui, app, &tokens);
        return;
    }

    // 3. Pick / Drawing Tools with 1+ Objects Selected -> Full Object Transform + Contextual Bar
    show_selected_objects_property_bar(ui, app, &tokens, &sel_ids);
}

// -----------------------------------------------------------------------------
// F10 Shape / Node Editing Property Bar (all 11 ShapeNode operations)
// -----------------------------------------------------------------------------

fn show_selected_objects_property_bar(
    ui: &mut Ui,
    app: &mut StudioApp,
    tokens: &ThemeTokens,
    sel_ids: &[coree_core::ObjectId],
) {
    let lang = app.ui_language;
    let bounds = app
        .active_tab()
        .and_then(|t| t.selection.bounding_box(&t.document));

    if let Some(bb) = bounds {
        // Interactive 3x3 9-Point Object Origin Anchor Grid
        render_nine_point_origin_grid(ui, &mut app.object_origin_anchor, tokens, lang);
        let anchor_pt = anchor_point_in_rect(bb, app.object_origin_anchor);

        let mut x = anchor_pt.x;
        let mut y = anchor_pt.y;
        let mut w = bb.width().max(0.1);
        let mut h = bb.height().max(0.1);
        let lock = app.active_tab().is_some_and(|t| t.lock_aspect);

        let x_chg = ui
            .add(
                DragValue::new(&mut x)
                    .speed(0.5)
                    .prefix(I18nKey::PropPositionX.tr_ui(lang))
                    .suffix(" mm"),
            )
            .changed();
        let y_chg = ui
            .add(
                DragValue::new(&mut y)
                    .speed(0.5)
                    .prefix(I18nKey::PropPositionY.tr_ui(lang))
                    .suffix(" mm"),
            )
            .changed();
        if x_chg || y_chg {
            let affine = Affine2D::translate(x - anchor_pt.x, y - anchor_pt.y);
            let _ = app.apply_world_transform_to_selection(sel_ids, affine);
        }

        let w_chg = ui
            .add(
                DragValue::new(&mut w)
                    .range(0.1..=5000.0)
                    .speed(0.5)
                    .prefix(I18nKey::PropWidth.tr_ui(lang))
                    .suffix(" mm"),
            )
            .changed();
        let h_chg = ui
            .add(
                DragValue::new(&mut h)
                    .range(0.1..=5000.0)
                    .speed(0.5)
                    .prefix(I18nKey::PropHeight.tr_ui(lang))
                    .suffix(" mm"),
            )
            .changed();
        if let Some(t) = app.active_tab_mut() {
            ui.checkbox(&mut t.lock_aspect, "🔒")
                .on_hover_text(I18nKey::PropLockAspect.tr_ui(lang));
        }
        if w_chg || h_chg {
            let sx = (w / bb.width().max(1e-4)).clamp(0.01, 100.0);
            let sy = if lock && w_chg {
                sx
            } else {
                (h / bb.height().max(1e-4)).clamp(0.01, 100.0)
            };
            let sx_final = if lock && h_chg && !w_chg { sy } else { sx };
            let affine = Affine2D::translate(anchor_pt.x, anchor_pt.y)
                .compose(&Affine2D::scale(sx_final, sy))
                .compose(&Affine2D::translate(-anchor_pt.x, -anchor_pt.y));
            let _ = app.apply_world_transform_to_selection(sel_ids, affine);
        }

        ui.separator();
        // Rotation° DragValue + Mirror H/V
        let mut rot_step = 0.0_f64;
        if ui
            .add(
                DragValue::new(&mut rot_step)
                    .range(-360.0..=360.0)
                    .speed(1.0)
                    .prefix("↻ ")
                    .suffix("°"),
            )
            .on_hover_text(format!(
                "{}: {}",
                I18nKey::PropRotation.tr_ui(lang),
                I18nKey::TbRotateSelectionTip.tr_ui(lang)
            ))
            .changed()
            && rot_step.abs() > 1e-4
        {
            let affine = Affine2D::rotate_about(rot_step.to_radians(), anchor_pt);
            let _ = app.apply_world_transform_to_selection(sel_ids, affine);
        }
        if ui
            .small_button("90°")
            .on_hover_text(I18nKey::TbRotate90Tip.tr_ui(lang))
            .clicked()
        {
            let affine = Affine2D::rotate_about(90.0_f64.to_radians(), anchor_pt);
            let _ = app.apply_world_transform_to_selection(sel_ids, affine);
        }
        if ui
            .small_button("⇄")
            .on_hover_text(I18nKey::PropMirrorH.tr_ui(lang))
            .clicked()
        {
            let affine = Affine2D::translate(anchor_pt.x, anchor_pt.y)
                .compose(&Affine2D::scale(-1.0, 1.0))
                .compose(&Affine2D::translate(-anchor_pt.x, -anchor_pt.y));
            let _ = app.apply_world_transform_to_selection(sel_ids, affine);
        }
        if ui
            .small_button("⇅")
            .on_hover_text(I18nKey::PropMirrorV.tr_ui(lang))
            .clicked()
        {
            let affine = Affine2D::translate(anchor_pt.x, anchor_pt.y)
                .compose(&Affine2D::scale(1.0, -1.0))
                .compose(&Affine2D::translate(-anchor_pt.x, -anchor_pt.y));
            let _ = app.apply_world_transform_to_selection(sel_ids, affine);
        }
    }

    // 2+ Objects Selected -> Instant Classic Shaping Strip + Combine + Group
    if sel_ids.len() >= 2 {
        ui.separator();
        for (op, lbl) in [
            (BooleanOp::Weld, I18nKey::ObjectWeld.tr_ui(lang)),
            (BooleanOp::Trim, I18nKey::ObjectTrim.tr_ui(lang)),
            (BooleanOp::Intersect, I18nKey::ObjectIntersect.tr_ui(lang)),
            (BooleanOp::Simplify, I18nKey::ObjectSimplify.tr_ui(lang)),
            (
                BooleanOp::FrontMinusBack,
                I18nKey::ObjectFrontMinusBack.tr_ui(lang),
            ),
            (
                BooleanOp::BackMinusFront,
                I18nKey::ObjectBackMinusFront.tr_ui(lang),
            ),
            (
                BooleanOp::CreateBoundary,
                I18nKey::ObjectCreateBoundary.tr_ui(lang),
            ),
        ] {
            if ui.small_button(lbl).clicked() {
                app.apply_boolean_op(op);
            }
        }
        ui.separator();
        if ui
            .small_button(I18nKey::ObjectCombine.tr_ui(lang))
            .on_hover_text(I18nKey::TbCombineTip.tr_ui(lang))
            .clicked()
        {
            app.apply_boolean_op(BooleanOp::Combine);
        }
        if ui
            .small_button(I18nKey::ObjectGroup.tr_ui(lang))
            .on_hover_text(I18nKey::TbGroupTip.tr_ui(lang))
            .clicked()
        {
            app.group_selection();
        }
    } else if let Some(&single_id) = sel_ids.first() {
        // Single selection: outline + parametric shape / text controls
        let payload_snapshot = app
            .active_tab()
            .and_then(|t| t.document.object(single_id))
            .map(|o| (o.payload.clone(), o.outline.clone(), o.lens.clone()));

        if let Some((payload, mut outline, lens_opt)) = payload_snapshot {
            ui.separator();
            let mut width = outline.width_mm;
            if ui
                .add(
                    DragValue::new(&mut width)
                        .range(0.0..=50.0)
                        .speed(0.05)
                        .prefix(I18nKey::TbLinePrefix.tr_ui(lang))
                        .suffix(" mm"),
                )
                .changed()
            {
                outline.width_mm = width;
                outline.is_hairline = false;
                app.apply_outline(outline.clone());
            }
            if ui
                .selectable_label(
                    outline.is_hairline,
                    I18nKey::PropOutlineHairline.tr_ui(lang),
                )
                .clicked()
            {
                outline.is_hairline = !outline.is_hairline;
                app.apply_outline(outline);
            }

            match payload {
                NodePayload::Shape(orig_shape) => match orig_shape.clone() {
                    ParametricShape::Rectangle {
                        width: rw,
                        height: rh,
                        mut corner_radii,
                        mut corner_style,
                        mut lock_corners,
                    } => {
                        ui.separator();
                        let max_r = (rw.min(rh) * 0.5).max(0.0);
                        let mut changed = false;
                        if lock_corners {
                            let mut r = corner_radii[0];
                            if ui
                                .add(
                                    DragValue::new(&mut r)
                                        .range(0.0..=max_r)
                                        .speed(0.2)
                                        .prefix(format!(
                                            "{} / {}: ",
                                            I18nKey::PropCornerRadius.tr_ui(lang),
                                            I18nKey::PropCornerStyle.tr_ui(lang)
                                        ))
                                        .suffix(" mm"),
                                )
                                .changed()
                            {
                                corner_radii = [r; 4];
                                changed = true;
                            }
                        } else {
                            for (i, pfx) in ["TL:", "TR:", "BR:", "BL:"].iter().enumerate() {
                                changed |= ui
                                    .add(
                                        DragValue::new(&mut corner_radii[i])
                                            .range(0.0..=max_r)
                                            .speed(0.2)
                                            .prefix(*pfx),
                                    )
                                    .changed();
                            }
                        }
                        if ui
                            .selectable_label(lock_corners, if lock_corners { "🔒" } else { "🔓" })
                            .on_hover_text(I18nKey::TbLockCornersTip.tr_ui(lang))
                            .clicked()
                        {
                            lock_corners = !lock_corners;
                            if lock_corners {
                                corner_radii = [corner_radii[0]; 4];
                            }
                            changed = true;
                        }
                        for (st, lbl) in [
                            (CornerStyle::Rounded, I18nKey::TbCornerRound.tr_ui(lang)),
                            (CornerStyle::Scalloped, I18nKey::TbCornerScallop.tr_ui(lang)),
                            (CornerStyle::Chamfered, I18nKey::TbCornerChamfer.tr_ui(lang)),
                        ] {
                            if ui.selectable_label(corner_style == st, lbl).clicked() {
                                corner_style = st;
                                changed = true;
                            }
                        }
                        if changed {
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::Rectangle {
                                    width: rw,
                                    height: rh,
                                    corner_radii,
                                    corner_style,
                                    lock_corners,
                                },
                            );
                        }
                    }
                    ParametricShape::Ellipse {
                        rx,
                        ry,
                        mut mode,
                        mut start_deg,
                        mut end_deg,
                    } => {
                        ui.separator();
                        let mut changed = false;
                        for (m, lbl) in [
                            (EllipseMode::Full, I18nKey::TbEllipseFull.tr_ui(lang)),
                            (EllipseMode::Pie, I18nKey::TbEllipsePie.tr_ui(lang)),
                            (EllipseMode::Arc, I18nKey::TbEllipseArc.tr_ui(lang)),
                        ] {
                            if ui.selectable_label(mode == m, lbl).clicked() {
                                mode = m;
                                if mode != EllipseMode::Full && (end_deg - start_deg).abs() >= 359.5
                                {
                                    start_deg = 0.0;
                                    end_deg = 270.0;
                                }
                                changed = true;
                            }
                        }
                        changed |= ui
                            .add(
                                DragValue::new(&mut start_deg)
                                    .range(0.0..=360.0)
                                    .prefix(I18nKey::PropFillStartLabel.tr_ui(lang))
                                    .suffix("°"),
                            )
                            .changed();
                        changed |= ui
                            .add(
                                DragValue::new(&mut end_deg)
                                    .range(0.0..=360.0)
                                    .prefix(I18nKey::PropFillEndLabel.tr_ui(lang))
                                    .suffix("°"),
                            )
                            .changed();
                        if changed {
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::Ellipse {
                                    rx,
                                    ry,
                                    mode,
                                    start_deg,
                                    end_deg,
                                },
                            );
                        }
                    }
                    ParametricShape::Polygon {
                        mut sides,
                        radius_x,
                        radius_y,
                    } => {
                        ui.separator();
                        if ui
                            .add(
                                DragValue::new(&mut sides)
                                    .range(3..=64)
                                    .prefix(I18nKey::PropPolygonSides.tr_ui(lang)),
                            )
                            .changed()
                        {
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::Polygon {
                                    sides,
                                    radius_x,
                                    radius_y,
                                },
                            );
                        }
                    }
                    ParametricShape::Star {
                        mut points,
                        outer_radius,
                        mut inner_ratio,
                        mut complex,
                    } => {
                        ui.separator();
                        let p_chg = ui
                            .add(
                                DragValue::new(&mut points)
                                    .range(3..=64)
                                    .prefix(I18nKey::PropStarPoints.tr_ui(lang)),
                            )
                            .changed();
                        let i_chg = ui
                            .add(
                                DragValue::new(&mut inner_ratio)
                                    .range(0.1..=0.9)
                                    .speed(0.02)
                                    .prefix(I18nKey::PropStarSharpness.tr_ui(lang)),
                            )
                            .changed();
                        let c_chg = ui
                            .checkbox(&mut complex, I18nKey::TbStarComplex.tr_ui(lang))
                            .changed();
                        if p_chg || i_chg || c_chg {
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::Star {
                                    points,
                                    outer_radius,
                                    inner_ratio,
                                    complex,
                                },
                            );
                        }
                    }
                    ParametricShape::Spiral {
                        mut revolutions,
                        radius,
                        mut logarithmic,
                        mut expansion,
                    } => {
                        ui.separator();
                        let mut changed = ui
                            .add(
                                DragValue::new(&mut revolutions)
                                    .range(1.0..=30.0)
                                    .speed(0.2)
                                    .prefix(I18nKey::PropSpiralRevolutions.tr_ui(lang)),
                            )
                            .changed();
                        changed |= ui
                            .checkbox(&mut logarithmic, I18nKey::TbSpiralLogShort.tr_ui(lang))
                            .changed();
                        if logarithmic {
                            changed |= ui
                                .add(
                                    DragValue::new(&mut expansion)
                                        .range(1.0..=4.0)
                                        .speed(0.05)
                                        .prefix(I18nKey::TbSpiralExpPrefix.tr_ui(lang)),
                                )
                                .changed();
                        }
                        if changed {
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::Spiral {
                                    revolutions,
                                    radius,
                                    logarithmic,
                                    expansion,
                                },
                            );
                        }
                    }
                    ParametricShape::Impact {
                        mut rays,
                        mut inner_radius,
                        outer_radius,
                        mut style,
                    } => {
                        ui.separator();
                        let mut changed = ui
                            .add(
                                DragValue::new(&mut rays)
                                    .range(4..=120)
                                    .prefix(I18nKey::PropImpactRays.tr_ui(lang)),
                            )
                            .changed();
                        changed |= ui
                            .add(
                                DragValue::new(&mut inner_radius)
                                    .range(1.0..=outer_radius.max(2.0))
                                    .speed(0.5)
                                    .prefix(I18nKey::TbImpactInnerPrefix.tr_ui(lang)),
                            )
                            .changed();
                        if ui
                            .selectable_label(
                                style == ImpactStyle::Radial,
                                I18nKey::TbImpactRadial.tr_ui(lang),
                            )
                            .clicked()
                        {
                            style = ImpactStyle::Radial;
                            changed = true;
                        }
                        if ui
                            .selectable_label(
                                style == ImpactStyle::Parallel,
                                I18nKey::TbImpactParallel.tr_ui(lang),
                            )
                            .clicked()
                        {
                            style = ImpactStyle::Parallel;
                            changed = true;
                        }
                        if changed {
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::Impact {
                                    rays,
                                    inner_radius,
                                    outer_radius,
                                    style,
                                },
                            );
                        }
                    }
                    ParametricShape::GraphPaper {
                        mut cols,
                        mut rows,
                        width,
                        height,
                    } => {
                        ui.separator();
                        let c_chg = ui
                            .add(
                                DragValue::new(&mut cols)
                                    .range(1..=50)
                                    .prefix(I18nKey::PropGraphCols.tr_ui(lang)),
                            )
                            .changed();
                        let r_chg = ui
                            .add(
                                DragValue::new(&mut rows)
                                    .range(1..=50)
                                    .prefix(I18nKey::PropGraphRows.tr_ui(lang)),
                            )
                            .changed();
                        if c_chg || r_chg {
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::GraphPaper {
                                    cols,
                                    rows,
                                    width,
                                    height,
                                },
                            );
                        }
                    }
                    ParametricShape::BasicShape {
                        mut kind,
                        width,
                        height,
                    } => {
                        ui.separator();
                        let mut changed = false;
                        ComboBox::from_id_salt("prop_sel_basic_shape")
                            .width(96.0)
                            .selected_text(format!("{kind:?}"))
                            .show_ui(ui, |ui| {
                                for k in [
                                    BasicShapeKind::ArrowRight,
                                    BasicShapeKind::ArrowLeft,
                                    BasicShapeKind::DoubleArrow,
                                    BasicShapeKind::Banner,
                                    BasicShapeKind::Callout,
                                    BasicShapeKind::Heart,
                                    BasicShapeKind::Cross,
                                    BasicShapeKind::Diamond,
                                    BasicShapeKind::Trapezoid,
                                ] {
                                    if ui
                                        .selectable_value(&mut kind, k, format!("{k:?}"))
                                        .changed()
                                    {
                                        changed = true;
                                    }
                                }
                            });
                        if changed {
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::BasicShape {
                                    kind,
                                    width,
                                    height,
                                },
                            );
                        }
                    }
                },
                NodePayload::Table(mut table) => {
                    ui.separator();
                    ui.label(
                        RichText::new(format!("Table {}×{}", table.rows, table.cols))
                            .size(11.0)
                            .color(tokens.accent),
                    );
                    if ui
                        .add(
                            DragValue::new(&mut table.border_style.width_mm)
                                .range(0.05..=10.0)
                                .speed(0.05)
                                .prefix(I18nKey::PropTableBorderWidth.tr_ui(lang))
                                .suffix(" mm"),
                        )
                        .changed()
                    {
                        let _ = app.execute_command(CommandOp::EditTable {
                            id: single_id,
                            table,
                        });
                    }
                }
                NodePayload::Dimension(mut dim) => {
                    ui.separator();
                    let mut changed = ui
                        .add(
                            DragValue::new(&mut dim.precision)
                                .range(0..=6)
                                .prefix(I18nKey::PropDimensionPrecision.tr_ui(lang)),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            DragValue::new(&mut dim.offset_mm)
                                .range(0.5..=200.0)
                                .speed(0.5)
                                .prefix(I18nKey::PropDimensionOffset.tr_ui(lang))
                                .suffix(" mm"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut dim.prefix)
                                .desired_width(36.0)
                                .hint_text(format!(
                                    "{}: {}",
                                    I18nKey::PropDimensionUnit.tr_ui(lang),
                                    I18nKey::TbDimPfxHint.tr_ui(lang)
                                )),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut dim.suffix)
                                .desired_width(36.0)
                                .hint_text(I18nKey::TbDimSfxHint.tr_ui(lang)),
                        )
                        .changed();
                    if changed {
                        let _ = app.execute_command(CommandOp::EditDimension {
                            id: single_id,
                            dimension: dim,
                        });
                    }
                }
                NodePayload::Connector(conn) => {
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "Connector ({:?}, r={:.1}mm)",
                            conn.style, conn.corner_radius_mm
                        ))
                        .size(11.0)
                        .color(tokens.accent),
                    );
                    if conn.from_obj.is_some() || conn.to_obj.is_some() {
                        ui.label(
                            RichText::new(format!(
                                "{} -> {}",
                                conn.from_obj
                                    .map_or("Free".to_owned(), |id| format!("#{}", id.0)),
                                conn.to_obj
                                    .map_or("Free".to_owned(), |id| format!("#{}", id.0)),
                            ))
                            .size(10.5)
                            .color(tokens.text_muted),
                        );
                    }
                }
                NodePayload::BSpline(mut bs) => {
                    ui.separator();
                    let max_deg = (bs.control_points.len().saturating_sub(1)).clamp(1, 7) as u32;
                    let d_chg = ui
                        .add(
                            DragValue::new(&mut bs.degree)
                                .range(1..=max_deg)
                                .prefix(I18nKey::PropBSplineDegree.tr_ui(lang)),
                        )
                        .changed();
                    let c_chg = ui
                        .checkbox(&mut bs.closed, I18nKey::PropBSplineClosed.tr_ui(lang))
                        .changed();
                    if d_chg || c_chg {
                        let rebuilt = coree_core::BSplineCurve::from_control_points(
                            bs.control_points,
                            bs.degree,
                            bs.closed,
                        );
                        let _ = app.execute_command(CommandOp::EditBSpline {
                            id: single_id,
                            bspline: rebuilt,
                        });
                    }
                }
                NodePayload::TextOnPath(mut top) => {
                    ui.separator();
                    let mut changed = ui
                        .add(
                            DragValue::new(&mut top.offset_mm)
                                .range(0.0..=5000.0)
                                .speed(0.5)
                                .prefix(I18nKey::PropTextOnPathOffset.tr_ui(lang))
                                .suffix(" mm"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            DragValue::new(&mut top.distance_mm)
                                .range(-100.0..=100.0)
                                .speed(0.25)
                                .prefix(I18nKey::PropTextOnPathDistance.tr_ui(lang))
                                .suffix(" mm"),
                        )
                        .changed();
                    changed |= ui
                        .checkbox(&mut top.flip_vertical, I18nKey::TbTextPathFlipV.tr_ui(lang))
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut top.flip_horizontal,
                            I18nKey::TbTextPathFlipH.tr_ui(lang),
                        )
                        .changed();
                    if changed {
                        let _ = app.execute_command(CommandOp::EditText {
                            id: single_id,
                            payload: NodePayload::TextOnPath(top),
                        });
                    }
                }
                NodePayload::PowerClip {
                    contents,
                    mut lock_contents,
                    ..
                } => {
                    ui.separator();
                    ui.label(
                        RichText::new(format!("PowerClip ({} item(s))", contents.len()))
                            .size(11.0)
                            .color(tokens.accent),
                    );
                    if ui
                        .small_button(I18nKey::TbPowerClipEditBtn.tr_ui(lang))
                        .clicked()
                    {
                        app.enter_powerclip_edit(single_id);
                    }
                    if ui
                        .small_button(I18nKey::ObjPowerClipFit.tr_ui(lang))
                        .clicked()
                    {
                        app.fit_powerclip_selection(coree_core::PowerClipFitMode::FitContents);
                    }
                    if ui
                        .small_button(I18nKey::ObjPowerClipCenter.tr_ui(lang))
                        .clicked()
                    {
                        app.fit_powerclip_selection(coree_core::PowerClipFitMode::CenterContents);
                    }
                    if ui
                        .small_button(I18nKey::ObjPowerClipStretch.tr_ui(lang))
                        .clicked()
                    {
                        app.fit_powerclip_selection(coree_core::PowerClipFitMode::StretchToFill);
                    }
                    if ui
                        .checkbox(
                            &mut lock_contents,
                            I18nKey::ObjectPowerClipLockContents.tr_ui(lang),
                        )
                        .changed()
                    {
                        app.set_powerclip_lock_selected(Some(lock_contents));
                    }
                    if ui
                        .small_button(I18nKey::TbPowerClipExtractBtn.tr_ui(lang))
                        .clicked()
                    {
                        app.extract_selected_powerclip();
                    }
                }
                NodePayload::ArtisticText(mut art) => {
                    ui.separator();
                    if let Some(run) = art.runs.first_mut() {
                        if tool_bars::render_text_style_strip(
                            ui,
                            &mut run.style,
                            &mut art.direction,
                            &mut art.align,
                            "prop_sel_art_font",
                            lang,
                        ) {
                            let _ = app.execute_command(CommandOp::EditText {
                                id: single_id,
                                payload: NodePayload::ArtisticText(art),
                            });
                        }
                    }
                }
                NodePayload::ParagraphText(mut para) => {
                    ui.separator();
                    if let Some(run) = para.runs.first_mut() {
                        if tool_bars::render_text_style_strip(
                            ui,
                            &mut run.style,
                            &mut para.direction,
                            &mut para.align,
                            "prop_sel_para_font",
                            lang,
                        ) {
                            let _ = app.execute_command(CommandOp::EditText {
                                id: single_id,
                                payload: NodePayload::ParagraphText(para),
                            });
                        }
                    }
                }
                NodePayload::ArtisticMedia { spine, mut style } => {
                    ui.separator();
                    let mut width_mm = match &style {
                        ArtisticMediaStyle::Preset { max_width_mm, .. } => *max_width_mm,
                        ArtisticMediaStyle::Brush { width_mm, .. }
                        | ArtisticMediaStyle::Calligraphic { width_mm, .. } => *width_mm,
                        ArtisticMediaStyle::Sprayer { size_mm, .. } => *size_mm,
                        ArtisticMediaStyle::Expression { max_width_mm, .. } => *max_width_mm,
                    };
                    let mut changed = ui
                        .add(
                            DragValue::new(&mut width_mm)
                                .range(0.4..=50.0)
                                .speed(0.2)
                                .prefix(I18nKey::DockBmpHeading.tr_ui(lang))
                                .suffix(" mm"),
                        )
                        .changed();
                    if changed {
                        match &mut style {
                            ArtisticMediaStyle::Preset { max_width_mm, .. } => {
                                *max_width_mm = width_mm;
                            }
                            ArtisticMediaStyle::Brush { width_mm: w, .. }
                            | ArtisticMediaStyle::Calligraphic { width_mm: w, .. } => {
                                *w = width_mm;
                            }
                            ArtisticMediaStyle::Sprayer { size_mm, .. } => *size_mm = width_mm,
                            ArtisticMediaStyle::Expression { max_width_mm, .. } => {
                                *max_width_mm = width_mm;
                            }
                        }
                        app.tool_settings.artistic_media_width_mm = width_mm;
                    }
                    if let ArtisticMediaStyle::Preset {
                        ref mut profile, ..
                    } = style
                    {
                        ComboBox::from_id_salt("prop_sel_artistic_profile")
                            .width(110.0)
                            .selected_text(format!("{profile:?}"))
                            .show_ui(ui, |ui| {
                                for prof in [
                                    ArtisticWidthProfile::TaperBoth,
                                    ArtisticWidthProfile::TaperStart,
                                    ArtisticWidthProfile::TaperEnd,
                                    ArtisticWidthProfile::Wave,
                                    ArtisticWidthProfile::Uniform,
                                ] {
                                    if ui
                                        .selectable_value(
                                            profile,
                                            prof.clone(),
                                            format!("{prof:?}"),
                                        )
                                        .changed()
                                    {
                                        app.tool_settings.artistic_media_profile = prof;
                                        changed = true;
                                    }
                                }
                            });
                    }
                    if changed {
                        let _ = app.execute_command(CommandOp::EditArtisticMedia {
                            id: single_id,
                            spine,
                            style,
                        });
                    }
                }
                NodePayload::Clone { .. } => {
                    ui.separator();
                    if ui
                        .small_button(I18nKey::CtxUnlinkClone.tr_ui(lang))
                        .on_hover_text(I18nKey::TbUnlinkCloneTip.tr_ui(lang))
                        .clicked()
                    {
                        app.unlink_clone_selection();
                    }
                }
                _ => {}
            }

            if let Some(lcfg) = lens_opt {
                ui.separator();
                if ui
                    .button(
                        RichText::new(format!("◉ Lens: {}", lcfg.kind.name()))
                            .size(11.0)
                            .color(tokens.accent_secondary),
                    )
                    .clicked()
                {
                    app.lens_state.load_from(&lcfg);
                    app.focus_docker(DockerTab::Lens);
                }
            }
        }
    }

    // Always-present classic quick-action strip for selected object(s)
    ui.separator();
    if ui
        .small_button(I18nKey::TbToCurves.tr_ui(lang))
        .on_hover_text(I18nKey::TbConvertCurvesTip.tr_ui(lang))
        .clicked()
    {
        app.convert_selection_to_curves();
    }
    if ui
        .small_button(I18nKey::TbBreakApartBtn.tr_ui(lang))
        .on_hover_text(I18nKey::TbBreakApartTip.tr_ui(lang))
        .clicked()
    {
        app.break_apart_selection();
    }
    if ui
        .small_button(I18nKey::TbCenterPageBtn.tr_ui(lang))
        .on_hover_text(I18nKey::TbCenterPageTip.tr_ui(lang))
        .clicked()
    {
        app.apply_align(AlignOp::CenterPage);
    }
    if ui
        .small_button(I18nKey::TbFrontBtn.tr_ui(lang))
        .on_hover_text(I18nKey::TbFrontTip.tr_ui(lang))
        .clicked()
    {
        app.apply_z_order(ZOrderOp::BringToFront);
    }
    if ui
        .small_button(I18nKey::TbBackBtn.tr_ui(lang))
        .on_hover_text(I18nKey::TbBackTip.tr_ui(lang))
        .clicked()
    {
        app.apply_z_order(ZOrderOp::SendToBack);
    }
}

// -----------------------------------------------------------------------------
// Interactive 3x3 Object Origin Anchor Grid Widget
// -----------------------------------------------------------------------------

pub(super) fn render_nine_point_origin_grid(
    ui: &mut Ui,
    anchor: &mut (u8, u8),
    tokens: &ThemeTokens,
    lang: crate::i18n::StudioLanguage,
) {
    let (rect, resp) = ui.allocate_exact_size(vec2(21.0, 21.0), Sense::click());
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(2),
        tokens.bg,
        Stroke::new(1.0, tokens.border),
        StrokeKind::Inside,
    );

    let cell = 7.0;
    for row in 0..3u8 {
        for col in 0..3u8 {
            let dot_center = pos2(
                rect.left() + (col as f32 + 0.5) * cell,
                rect.top() + (row as f32 + 0.5) * cell,
            );
            let active = anchor.0 == col && anchor.1 == row;
            let dot_rect = Rect::from_center_size(
                dot_center,
                if active {
                    vec2(5.0, 5.0)
                } else {
                    vec2(3.2, 3.2)
                },
            );
            painter.rect_filled(
                dot_rect,
                CornerRadius::same(1),
                if active {
                    tokens.accent
                } else {
                    tokens.text_muted
                },
            );
        }
    }

    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let col = (((pos.x - rect.left()) / cell).floor() as i32).clamp(0, 2) as u8;
            let row = (((pos.y - rect.top()) / cell).floor() as i32).clamp(0, 2) as u8;
            *anchor = (col, row);
        }
    }
    resp.on_hover_text(format!(
        "{}: {}",
        I18nKey::PropOriginAnchor.tr_ui(lang),
        I18nKey::TbOriginAnchorTip.tr_ui(lang)
    ));
}

pub(super) fn anchor_point_in_rect(bb: Rect2D, anchor: (u8, u8)) -> Point2D {
    let fx = f64::from(anchor.0.min(2)) * 0.5;
    let fy = f64::from(anchor.1.min(2)) * 0.5;
    Point2D::new(bb.min_x + bb.width() * fx, bb.min_y + bb.height() * fy)
}

// -----------------------------------------------------------------------------
// Crisp Procedural Vector Toolbar Icon Buttons
// -----------------------------------------------------------------------------
