// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

pub(super) fn show_canvas_context_menu(canvas_resp: &egui::Response, app: &mut StudioApp) {
    canvas_resp.context_menu(|ui| {
        let has_sel = app.active_tab().is_some_and(|t| !t.selection.is_empty());
        let has_multi = app.active_tab().is_some_and(|t| t.selection.len() >= 2);
        let has_clone = app.active_tab().is_some_and(|t| {
            t.selection.objects.iter().any(|id| {
                t.document
                    .object(*id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Clone { .. }))
            })
        });
        let has_clip = app.has_clipboard_content();
        let lang = app.ui_language;

        if ui
            .add_enabled(
                has_sel,
                egui::Button::new(crate::i18n::I18nKey::CtxCut.tr_ui(lang)),
            )
            .clicked()
        {
            app.cut_selection();
            ui.close();
        }
        if ui
            .add_enabled(
                has_sel,
                egui::Button::new(crate::i18n::I18nKey::CtxCopy.tr_ui(lang)),
            )
            .clicked()
        {
            app.copy_selection();
            ui.close();
        }
        if ui
            .add_enabled(
                has_clip,
                egui::Button::new(crate::i18n::I18nKey::CtxPaste.tr_ui(lang)),
            )
            .clicked()
        {
            app.paste_clipboard();
            ui.close();
        }
        if ui
            .add_enabled(
                has_sel,
                egui::Button::new(crate::i18n::I18nKey::CtxDuplicate.tr_ui(lang)),
            )
            .clicked()
        {
            let _ = app.duplicate_selection();
            ui.close();
        }
        if ui
            .add_enabled(
                has_sel,
                egui::Button::new(crate::i18n::I18nKey::CtxClone.tr_ui(lang)),
            )
            .clicked()
        {
            let _ = app.clone_selection();
            ui.close();
        }
        if has_clone
            && ui
                .button(crate::i18n::I18nKey::CtxUnlinkClone.tr_ui(lang))
                .clicked()
        {
            app.unlink_clone_selection();
            ui.close();
        }
        if ui
            .add_enabled(
                has_sel,
                egui::Button::new(crate::i18n::I18nKey::EditDelete.tr_ui(app.ui_language)),
            )
            .clicked()
        {
            app.delete_selection();
            ui.close();
        }

        ui.separator();

        if ui
            .add_enabled(
                has_sel,
                egui::Button::new(
                    crate::i18n::I18nKey::ObjectConvertToCurves.tr_ui(app.ui_language),
                ),
            )
            .clicked()
        {
            app.convert_selection_to_curves();
            ui.close();
        }
        if ui
            .add_enabled(
                has_sel,
                egui::Button::new(
                    crate::i18n::I18nKey::ObjectConvertOutlineToObject.tr_ui(app.ui_language),
                ),
            )
            .clicked()
        {
            app.convert_outline_to_object_selection();
            ui.close();
        }
        if ui
            .add_enabled(
                has_sel,
                egui::Button::new(crate::i18n::I18nKey::ObjectBreakApart.tr_ui(app.ui_language)),
            )
            .clicked()
        {
            app.break_apart_selection();
            ui.close();
        }
        if ui
            .add_enabled(
                has_multi,
                egui::Button::new(crate::i18n::I18nKey::ObjectCombine.tr_ui(app.ui_language)),
            )
            .clicked()
        {
            app.apply_boolean_op(BooleanOp::Combine);
            ui.close();
        }
        ui.menu_button(crate::i18n::I18nKey::CtxOrderMenu.tr_ui(lang), |ui| {
            if ui
                .button(crate::i18n::I18nKey::ObjectOrderToFront.tr_ui(lang))
                .clicked()
            {
                app.apply_z_order(ZOrderOp::BringToFront);
                ui.close();
            }
            if ui
                .button(crate::i18n::I18nKey::ObjectOrderForwardOne.tr_ui(lang))
                .clicked()
            {
                app.apply_z_order(ZOrderOp::BringForward);
                ui.close();
            }
            if ui
                .button(crate::i18n::I18nKey::ObjectOrderBackOne.tr_ui(lang))
                .clicked()
            {
                app.apply_z_order(ZOrderOp::SendBackward);
                ui.close();
            }
            if ui
                .button(crate::i18n::I18nKey::ObjectOrderToBack.tr_ui(lang))
                .clicked()
            {
                app.apply_z_order(ZOrderOp::SendToBack);
                ui.close();
            }
        });
        if ui
            .add_enabled(
                has_multi,
                egui::Button::new(crate::i18n::I18nKey::ObjectGroup.tr_ui(app.ui_language)),
            )
            .clicked()
        {
            app.group_selection();
            ui.close();
        }
        if ui
            .add_enabled(
                has_sel,
                egui::Button::new(crate::i18n::I18nKey::ObjectUngroup.tr_ui(app.ui_language)),
            )
            .clicked()
        {
            app.ungroup_selection();
            ui.close();
        }
        if ui
            .add_enabled(
                has_multi,
                egui::Button::new(
                    crate::i18n::I18nKey::ObjectPowerClipPlaceInside.tr_ui(app.ui_language),
                ),
            )
            .clicked()
        {
            app.create_powerclip_from_selection();
            ui.close();
        }
        let selected_pc = app.active_tab().and_then(|t| {
            let oid = *t.selection.objects.first()?;
            matches!(
                t.document.object(oid).map(|o| &o.payload),
                Some(NodePayload::PowerClip { .. })
            )
            .then_some(oid)
        });
        if let Some(pc_id) = selected_pc {
            let lang = app.ui_language;
            if ui
                .button(crate::i18n::I18nKey::ObjectPowerClipEdit.tr_ui(lang))
                .clicked()
            {
                app.enter_powerclip_edit(pc_id);
                ui.close();
            }
            if ui
                .button(crate::i18n::I18nKey::ObjectPowerClipFitContents.tr_ui(lang))
                .clicked()
            {
                app.fit_powerclip_selection(coree_core::PowerClipFitMode::FitContents);
                ui.close();
            }
            if ui
                .button(crate::i18n::I18nKey::ObjectPowerClipCenterContents.tr_ui(lang))
                .clicked()
            {
                app.fit_powerclip_selection(coree_core::PowerClipFitMode::CenterContents);
                ui.close();
            }
            if ui
                .button(crate::i18n::I18nKey::ObjectPowerClipStretchToFill.tr_ui(lang))
                .clicked()
            {
                app.fit_powerclip_selection(coree_core::PowerClipFitMode::StretchToFill);
                ui.close();
            }
        }
        if app
            .active_tab()
            .is_some_and(|t| t.powerclip_edit_id.is_some())
            && ui
                .button(crate::i18n::I18nKey::ObjectPowerClipFinish.tr_ui(app.ui_language))
                .clicked()
        {
            app.exit_powerclip_edit();
            ui.close();
        }
        if ui
            .add_enabled(
                has_sel,
                egui::Button::new(crate::i18n::I18nKey::ObjectLock.tr_ui(app.ui_language)),
            )
            .clicked()
        {
            app.toggle_lock_selection();
            ui.close();
        }

        ui.separator();

        if ui
            .button(crate::i18n::I18nKey::CtxAssignCutContour.tr_ui(lang))
            .clicked()
        {
            app.generate_cut_contour_active();
            ui.close();
        }
        if ui
            .button(crate::i18n::I18nKey::LayoutAutofitPage.tr_ui(app.ui_language))
            .clicked()
        {
            app.autofit_active_page(2.0);
            ui.close();
        }
        if ui
            .button(crate::i18n::I18nKey::DockerProperties.tr_ui(app.ui_language))
            .clicked()
        {
            app.focus_docker(DockerTab::Properties);
            ui.close();
        }
    });
}

pub(super) fn show_inline_text_editor_overlay(
    ui: &egui::Ui,
    app: &mut StudioApp,
    canvas_rect: Rect,
) {
    let tokens = app.theme.tokens();
    let editor_geom = app.active_tab().and_then(|tab| {
        let edit = tab.inline_text_edit.as_ref()?;
        let pos = world_to_screen(canvas_rect, tab, edit.world_pos_mm);
        let raw_w = (edit.box_size_mm.x * tab.viewport.logical_px_per_mm()) as f32;
        let raw_h = (edit.box_size_mm.y * tab.viewport.logical_px_per_mm()) as f32;
        let w_px = if raw_w.is_finite() { raw_w } else { 180.0 };
        let h_px = if raw_h.is_finite() { raw_h } else { 48.0 };
        let avail_w = (canvas_rect.width() - 24.0).max(24.0);
        let avail_h = (canvas_rect.height() - 44.0).max(24.0);
        let max_w = avail_w.min(520.0);
        let min_w = max_w.min(180.0);
        let max_h = avail_h.min(260.0);
        let min_h = max_h.min(48.0);
        let width_px = w_px.clamp(min_w, max_w);
        let height_px = h_px.clamp(min_h, max_h);
        let min_x = canvas_rect.left() + 6.0;
        let max_x = (canvas_rect.right() - (width_px + 14.0)).max(min_x);
        let min_y = canvas_rect.top() + 6.0;
        let max_y = (canvas_rect.bottom() - (height_px + 38.0)).max(min_y);
        let px = if pos.x.is_finite() { pos.x } else { min_x };
        let py = if pos.y.is_finite() { pos.y } else { min_y };
        Some((
            pos2(px.clamp(min_x, max_x), py.clamp(min_y, max_y)),
            width_px,
            height_px,
            edit.is_paragraph,
        ))
    });
    let Some((anchor_pos, width_px, height_px, is_para)) = editor_geom else {
        return;
    };

    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut commit_req = false;
    let mut cancel_req = false;

    egui::Area::new(ui.scope_id().with("coree_inline_text_edit_area"))
        .fixed_pos(anchor_pos)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(tokens.surface_elevated)
                .stroke(Stroke::new(1.5, tokens.accent))
                .corner_radius(CornerRadius::same(4))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    if let Some(tab) = app.active_tab_mut() {
                        if let Some(ref mut edit) = tab.inline_text_edit {
                            let resp = if is_para {
                                ui.add_sized(
                                    vec2(width_px, height_px),
                                    egui::TextEdit::multiline(&mut edit.buffer).hint_text(
                                        crate::i18n::I18nKey::InlineTextParaHint.tr_ui(lang),
                                    ),
                                )
                            } else {
                                ui.add_sized(
                                    vec2(width_px, 28.0),
                                    egui::TextEdit::singleline(&mut edit.buffer).hint_text(
                                        crate::i18n::I18nKey::InlineTextArtHint.tr_ui(lang),
                                    ),
                                )
                            };
                            if edit.just_opened {
                                resp.request_focus();
                                edit.just_opened = false;
                            }
                            let (ctrl_enter, plain_enter, esc) = ui.input(|i| {
                                (
                                    i.key_pressed(egui::Key::Enter)
                                        && (i.modifiers.ctrl || i.modifiers.command),
                                    i.key_pressed(egui::Key::Enter) && !i.modifiers.any(),
                                    i.key_pressed(egui::Key::Escape),
                                )
                            });
                            if esc {
                                cancel_req = true;
                            } else if ctrl_enter || (!is_para && plain_enter) {
                                commit_req = true;
                            }
                        }
                    }
                    shell_dir.with_horizontal(ui, |ui| {
                        if ui
                            .small_button(crate::i18n::I18nKey::InlineTextApplyBtn.tr_ui(lang))
                            .clicked()
                        {
                            commit_req = true;
                        }
                        if ui
                            .small_button(crate::i18n::I18nKey::InlineTextCancelBtn.tr_ui(lang))
                            .clicked()
                        {
                            cancel_req = true;
                        }
                        ui.label(
                            RichText::new(if is_para {
                                crate::i18n::I18nKey::InlineTextParaHelp.tr_ui(lang)
                            } else {
                                crate::i18n::I18nKey::InlineTextArtHelp.tr_ui(lang)
                            })
                            .size(10.0)
                            .color(tokens.text_muted),
                        );
                    });
                });
        });

    if cancel_req {
        app.cancel_inline_text_edit();
    } else if commit_req {
        app.commit_inline_text_edit();
    }
}

// =============================================================================
// Interactive Overlays (Laser Toolpaths, Handles, Nodes, Effects, Crop, Cursors)
// =============================================================================
