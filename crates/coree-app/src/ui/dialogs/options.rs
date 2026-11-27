// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

/// Classic tabbed Studio Options dialog (`Tools -> Options...`, `Ctrl+J`).
pub(super) fn show_options_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.show_options_dialog && !app.show_preferences_dialog {
        return;
    }
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut open = true;

    egui::Window::new(crate::i18n::I18nKey::DlgOptTitle.tr_ui(lang))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(640.0, 460.0))
        .min_size(egui::vec2(540.0, 380.0))
        .show(ctx, |ui| {
            shell_dir.with_vertical(ui, |ui| {
                shell_dir.with_horizontal_wrapped(ui, |ui| {
                    ui.selectable_value(
                        &mut app.options_tab,
                        0,
                        crate::i18n::I18nKey::DlgOptTab1.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut app.options_tab,
                        1,
                        crate::i18n::I18nKey::DlgOptTab2.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut app.options_tab,
                        2,
                        crate::i18n::I18nKey::DlgOptTab3.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut app.options_tab,
                        3,
                        crate::i18n::I18nKey::DlgOptTab4.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut app.options_tab,
                        4,
                        crate::i18n::I18nKey::DlgOptTabPerf.tr_ui(lang),
                    );
                });
                ui.separator();

                egui::ScrollArea::vertical()
                    .id_salt("options_dialog_scroll")
                    .max_height(355.0)
                    .show(ui, |ui| match app.options_tab {
                        0 => show_options_appearance_workspace(ui, app, &tokens, shell_dir),
                        1 => show_options_general_nudge(ui, app, &tokens, shell_dir),
                        2 => show_options_color_management(ui, app, &tokens, shell_dir),
                        3 => show_options_laser_plugins(ui, app, &tokens, shell_dir),
                        _ => show_options_performance_resources(ui, app, &tokens, shell_dir),
                    });

                ui.separator();
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(
                        RichText::new(crate::i18n::I18nKey::DlgOptPersistNote.tr_ui(lang))
                            .size(11.0)
                            .color(tokens.text_muted),
                    );
                    shell_dir.with_opposite_horizontal(ui, |ui| {
                        if ui
                            .button(
                                RichText::new(crate::i18n::I18nKey::DialogClose.tr_ui(lang))
                                    .strong(),
                            )
                            .clicked()
                        {
                            app.show_options_dialog = false;
                            app.show_preferences_dialog = false;
                        }
                    });
                });
            });
        });

    if !open {
        app.show_options_dialog = false;
        app.show_preferences_dialog = false;
    }
}

fn show_options_appearance_workspace(
    ui: &mut egui::Ui,
    app: &mut StudioApp,
    tokens: &crate::theme::ThemeTokens,
    shell_dir: crate::i18n::ShellLayoutDirections,
) {
    let lang = app.ui_language;
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptVisualTheme.tr_ui(lang)).strong());
    shell_dir.with_horizontal(ui, |ui| {
        for theme in StudioTheme::ALL {
            if ui
                .selectable_label(app.theme == theme, theme.label())
                .clicked()
            {
                app.set_theme(theme);
            }
        }
    });

    ui.add_space(8.0);
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptLangHeader.tr_ui(lang)).strong());
    shell_dir.with_horizontal(ui, |ui| {
        for l in StudioLanguage::ALL {
            if ui
                .selectable_label(app.ui_language == l, l.label())
                .clicked()
            {
                app.set_ui_language(l);
            }
        }
    });

    ui.add_space(8.0);
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptActivePreset.tr_ui(lang)).strong());
    for preset in WorkspacePreset::ALL {
        let active = app.active_workspace_preset == preset;
        Frame::NONE
            .fill(if active {
                tokens.surface_elevated
            } else {
                tokens.surface
            })
            .stroke(Stroke::new(
                1.0,
                if active { tokens.accent } else { tokens.border },
            ))
            .inner_margin(Margin::symmetric(8, 5))
            .show(ui, |ui| {
                shell_dir.with_horizontal(ui, |ui| {
                    if ui
                        .selectable_label(
                            active,
                            RichText::new(crate::i18n::workspace_i18n_key(preset).tr_ui(lang))
                                .strong(),
                        )
                        .clicked()
                    {
                        app.apply_workspace_preset(preset);
                    }
                    ui.label(
                        RichText::new(preset.description())
                            .size(11.0)
                            .color(tokens.text_muted),
                    );
                });
            });
        ui.add_space(2.0);
    }

    ui.add_space(8.0);
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptErgonomics.tr_ui(lang)).strong());
    ui.checkbox(
        &mut app.toolbox_two_column,
        crate::i18n::I18nKey::DlgOptTwoColToolbox.tr_ui(lang),
    );
    ui.checkbox(
        &mut app.show_dockers,
        crate::i18n::I18nKey::DlgOptShowDockers.tr_ui(lang),
    );
    ui.checkbox(
        &mut app.palette_show_names,
        crate::i18n::I18nKey::DlgOptShowPalNames.tr_ui(lang),
    );
    ui.checkbox(
        &mut app.treat_all_objects_as_filled,
        crate::i18n::I18nKey::DlgOptTreatFilled.tr_ui(lang),
    );

    ui.add_space(6.0);
    ui.label(RichText::new(crate::i18n::I18nKey::ToolsToolbarConfig.tr_ui(lang)).strong());
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        ui.checkbox(
            &mut app.toolbar_config.show_file_ops,
            crate::i18n::I18nKey::DlgOptGrpFile.tr_ui(lang),
        );
        ui.checkbox(
            &mut app.toolbar_config.show_undo_redo,
            crate::i18n::I18nKey::DlgOptGrpUndo.tr_ui(lang),
        );
        ui.checkbox(
            &mut app.toolbar_config.show_clipboard,
            crate::i18n::I18nKey::DlgOptGrpClip.tr_ui(lang),
        );
        ui.checkbox(
            &mut app.toolbar_config.show_zoom,
            crate::i18n::I18nKey::DlgOptGrpZoom.tr_ui(lang),
        );
        ui.checkbox(
            &mut app.toolbar_config.show_view_mode,
            crate::i18n::I18nKey::DlgOptGrpView.tr_ui(lang),
        );
        ui.checkbox(
            &mut app.toolbar_config.show_snap_toggles,
            crate::i18n::I18nKey::DlgOptGrpSnaps.tr_ui(lang),
        );
        ui.checkbox(
            &mut app.toolbar_config.show_layout_toggles,
            crate::i18n::I18nKey::DlgOptGrpLayout.tr_ui(lang),
        );
        ui.checkbox(
            &mut app.toolbar_config.show_drawing_scale,
            crate::i18n::I18nKey::DlgOptGrpScale.tr_ui(lang),
        );
    });

    ui.add_space(8.0);
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptShortcutProf.tr_ui(lang)).strong());
    shell_dir.with_horizontal(ui, |ui| {
        for prof in [
            ShortcutProfile::ClassicStudio,
            ShortcutProfile::StandardVector,
        ] {
            if ui
                .selectable_label(app.shortcut_profile == prof, prof.label())
                .clicked()
            {
                app.set_shortcut_profile(prof);
            }
        }
    });

    ui.add_space(8.0);
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptCustomShortcuts.tr_ui(lang)).strong());
    let remappable = ShortcutAction::REMAPPABLE_ACTIONS;
    if app.shortcut_editor_idx >= remappable.len() {
        app.shortcut_editor_idx = 0;
    }
    let (sel_action, sel_name) = remappable[app.shortcut_editor_idx];
    let current_binding =
        shortcut_label_for(app.shortcut_profile, &app.custom_shortcuts, sel_action);
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        ComboBox::from_id_salt("opt_custom_shortcut_action")
            .width(230.0)
            .selected_text(format!("{sel_name} [{current_binding}]"))
            .show_ui(ui, |ui| {
                for (i, &(act, name)) in remappable.iter().enumerate() {
                    let lbl = shortcut_label_for(app.shortcut_profile, &app.custom_shortcuts, act);
                    let display = if lbl.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{name} [{lbl}]")
                    };
                    if ui
                        .selectable_value(&mut app.shortcut_editor_idx, i, display)
                        .clicked()
                    {
                        app.shortcut_editor_combo = lbl;
                    }
                }
            });
        ui.add(
            egui::TextEdit::singleline(&mut app.shortcut_editor_combo)
                .desired_width(130.0)
                .hint_text("Ctrl+Alt+B"),
        );
        if ui
            .button(crate::i18n::I18nKey::DlgOptAssign.tr_ui(lang))
            .clicked()
        {
            let combo = app.shortcut_editor_combo.clone();
            match app.custom_shortcuts.set_binding(sel_action, &combo) {
                Ok(norm) => {
                    app.shortcut_editor_combo = norm.clone();
                    app.push_toast(format!("Bound {sel_name} -> {norm}"), false);
                }
                Err(err) => app.push_toast(
                    format!(
                        "{}: {err}",
                        crate::i18n::I18nKey::ErrorInvalidInput.tr_ui(lang)
                    ),
                    true,
                ),
            }
        }
        if ui
            .button(crate::i18n::I18nKey::DlgOptResetKey.tr_ui(lang))
            .on_hover_text(crate::i18n::I18nKey::DialogReset.tr_ui(lang))
            .clicked()
        {
            app.custom_shortcuts.clear_binding(sel_action);
            app.shortcut_editor_combo.clear();
            app.push_toast(format!("Reset {sel_name} to profile default"), false);
        }
        if !app.custom_shortcuts.is_empty()
            && ui
                .button(crate::i18n::I18nKey::DlgOptResetAllCustom.tr_ui(lang))
                .clicked()
        {
            app.custom_shortcuts.bindings.clear();
            app.shortcut_editor_combo.clear();
            app.push_toast(
                crate::i18n::I18nKey::DlgOptClearedAllShortcuts.tr_ui(lang),
                false,
            );
        }
    });

    ui.add_space(8.0);
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptPortableWs.tr_ui(lang)).strong());
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        if ui
            .button(crate::i18n::I18nKey::DlgOptExportWs.tr_ui(lang))
            .clicked()
        {
            export_workspace_dialog(app);
        }
        if ui
            .button(crate::i18n::I18nKey::DlgOptImportWs.tr_ui(lang))
            .clicked()
        {
            import_workspace_dialog(app);
        }
        if ui
            .button(crate::i18n::I18nKey::DlgOptResetWs.tr_ui(lang))
            .clicked()
        {
            app.apply_workspace_preset(WorkspacePreset::DefaultClassic);
        }
    });
}

fn show_options_general_nudge(
    ui: &mut egui::Ui,
    app: &mut StudioApp,
    tokens: &crate::theme::ThemeTokens,
    shell_dir: crate::i18n::ShellLayoutDirections,
) {
    let lang = app.ui_language;
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptUnitsHeading.tr_ui(lang)).strong());
    let mut pending_unit: Option<Unit> = None;
    if let Some(tab) = app.active_tab_mut() {
        let cur_unit = tab.document.default_unit();
        let mut next_unit = cur_unit;
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(crate::i18n::I18nKey::DlgOptActiveUnits.tr_ui(lang));
            ComboBox::from_id_salt("opt_doc_units")
                .width(120.0)
                .selected_text(cur_unit.suffix())
                .show_ui(ui, |ui| {
                    for u in [
                        Unit::Millimeter,
                        Unit::Centimeter,
                        Unit::Meter,
                        Unit::Inch,
                        Unit::Point,
                        Unit::Pixel,
                    ] {
                        ui.selectable_value(&mut next_unit, u, u.suffix());
                    }
                });
        });
        if next_unit != cur_unit {
            pending_unit = Some(next_unit);
        }

        ui.add_space(6.0);
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(crate::i18n::I18nKey::DlgOptNudgeDist.tr_ui(lang));
            ui.add(
                DragValue::new(&mut tab.nudge_mm)
                    .range(0.01..=200.0)
                    .speed(0.1)
                    .suffix(" mm"),
            );
        });
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(crate::i18n::I18nKey::DlgOptSuperNudge.tr_ui(lang));
            ui.add(
                DragValue::new(&mut tab.super_nudge_factor)
                    .range(1.0..=20.0)
                    .speed(0.2)
                    .suffix(" ×"),
            );
        });
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(crate::i18n::I18nKey::DlgOptMicroNudge.tr_ui(lang));
            ui.add(
                DragValue::new(&mut tab.micro_nudge_divisor)
                    .range(1.0..=20.0)
                    .speed(0.2)
                    .prefix("÷ "),
            );
        });

        ui.add_space(6.0);
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(crate::i18n::I18nKey::DlgOptDupOffset.tr_ui(lang));
            ui.add(
                DragValue::new(&mut tab.duplicate_offset_mm.x)
                    .range(-500.0..=500.0)
                    .prefix("X: ")
                    .suffix(" mm"),
            );
            ui.add(
                DragValue::new(&mut tab.duplicate_offset_mm.y)
                    .range(-500.0..=500.0)
                    .prefix("Y: ")
                    .suffix(" mm"),
            );
        });
    } else {
        ui.label(
            RichText::new(crate::i18n::I18nKey::DlgOptOpenDocForUnits.tr_ui(lang))
                .color(tokens.text_muted),
        );
    }
    if let Some(next_unit) = pending_unit {
        let _ = app.execute_command(CommandOp::SetDocumentProperties {
            title: None,
            color_mode: None,
            default_unit: Some(next_unit),
            dpi: None,
            multipage_view: None,
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

    ui.add_space(10.0);
    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptAutoBackup.tr_ui(lang)).strong());
    let wal_status = if app.storage.is_some() {
        crate::i18n::I18nKey::DlgOptWalActive.tr_ui(lang)
    } else {
        crate::i18n::I18nKey::DlgOptWalMemory.tr_ui(lang)
    };
    ui.label(RichText::new(wal_status).color(tokens.success));
    ui.label(
        RichText::new(format!(
            "Autosave Checkpoint Interval: {} seconds · Open Tabs: {}",
            app.autosave_interval.as_secs(),
            app.tabs.len()
        ))
        .size(11.5)
        .color(tokens.text_muted),
    );
    if ui
        .button(crate::i18n::I18nKey::DlgOptSaveSnapNow.tr_ui(lang))
        .clicked()
    {
        match app.perform_autosave_active() {
            Ok(()) => app.push_toast(crate::i18n::I18nKey::DlgOptSavedSnapshot.tr_ui(lang), false),
            Err(e) => app.push_toast(e, true),
        }
    }
}

fn show_options_color_management(
    ui: &mut egui::Ui,
    app: &mut StudioApp,
    tokens: &crate::theme::ThemeTokens,
    shell_dir: crate::i18n::ShellLayoutDirections,
) {
    let lang = app.ui_language;
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptIccHeading.tr_ui(lang)).strong());
    ui.label(
        RichText::new(crate::i18n::I18nKey::DlgOptIccSub.tr_ui(lang))
            .size(11.5)
            .color(tokens.text_muted),
    );
    ui.add_space(8.0);

    let mut changed = false;
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DlgOptProofProfile.tr_ui(lang));
        ComboBox::from_id_salt("opt_icc_profile")
            .width(280.0)
            .selected_text(app.proof_profile.display_name())
            .show_ui(ui, |ui| {
                for prof in BuiltinIccProfile::ALL {
                    if ui
                        .selectable_value(&mut app.proof_profile, prof, prof.display_name())
                        .changed()
                    {
                        changed = true;
                    }
                }
            });
    });

    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DlgOptRenderIntent.tr_ui(lang));
        ComboBox::from_id_salt("opt_icc_intent")
            .width(220.0)
            .selected_text(rendering_intent_label(app.proof_intent))
            .show_ui(ui, |ui| {
                for intent in [
                    RenderingIntent::Perceptual,
                    RenderingIntent::RelativeColorimetric,
                    RenderingIntent::Saturation,
                    RenderingIntent::AbsoluteColorimetric,
                ] {
                    if ui
                        .selectable_value(
                            &mut app.proof_intent,
                            intent,
                            rendering_intent_label(intent),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                }
            });
    });

    ui.add_space(6.0);
    changed |= ui
        .checkbox(
            &mut app.soft_proof_cmyk,
            crate::i18n::I18nKey::DlgOptProofColorsChk.tr_ui(lang),
        )
        .changed();
    changed |= ui
        .checkbox(
            &mut app.gamut_warning,
            crate::i18n::I18nKey::DlgOptGamutWarnChk.tr_ui(lang),
        )
        .changed();

    if changed {
        app.sync_proof_options_to_tabs();
    }

    ui.add_space(8.0);
    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptDocSpaces.tr_ui(lang)).strong());
    let doc_intent = app
        .active_tab()
        .map(|t| t.document.rendering_intent())
        .unwrap_or(RenderingIntent::RelativeColorimetric);
    let mut next_doc_intent = doc_intent;
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DlgOptDocIntent.tr_ui(lang));
        ComboBox::from_id_salt("opt_doc_icc_intent")
            .width(200.0)
            .selected_text(rendering_intent_label(next_doc_intent))
            .show_ui(ui, |ui| {
                for intent in [
                    RenderingIntent::Perceptual,
                    RenderingIntent::RelativeColorimetric,
                    RenderingIntent::Saturation,
                    RenderingIntent::AbsoluteColorimetric,
                ] {
                    ui.selectable_value(
                        &mut next_doc_intent,
                        intent,
                        rendering_intent_label(intent),
                    );
                }
            });
    });
    if next_doc_intent != doc_intent {
        let _ = app.execute_command(CommandOp::SetDocumentProperties {
            title: None,
            color_mode: None,
            default_unit: None,
            dpi: None,
            multipage_view: None,
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
            rendering_intent: Some(next_doc_intent),
            black_point_compensation: None,
            drawing_scale: None,
        });
        if let Some(tab) = app.active_tab_mut() {
            tab.needs_rerender = true;
        }
    }
    let doc_bpc = app
        .active_tab()
        .map(|t| t.document.black_point_compensation())
        .unwrap_or(true);
    let mut next_bpc = doc_bpc;
    if ui
        .checkbox(
            &mut next_bpc,
            crate::i18n::I18nKey::DlgOptBpcChk.tr_ui(lang),
        )
        .changed()
        && next_bpc != doc_bpc
    {
        let _ = app.execute_command(CommandOp::SetDocumentProperties {
            title: None,
            color_mode: None,
            default_unit: None,
            dpi: None,
            multipage_view: None,
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
            black_point_compensation: Some(next_bpc),
            drawing_scale: None,
        });
        if let Some(tab) = app.active_tab_mut() {
            tab.needs_rerender = true;
        }
    }

    let (rgb_name, cmyk_name, gray_name) = app
        .active_tab()
        .map(|t| {
            (
                t.document.rgb_icc_name().map(str::to_owned),
                t.document.cmyk_icc_name().map(str::to_owned),
                t.document.gray_icc_name().map(str::to_owned),
            )
        })
        .unwrap_or((None, None, None));
    ui.label(
        RichText::new(format!(
            "{} RGB: {} · CMYK: {} · Gray: {}",
            crate::i18n::I18nKey::DlgOptSlotsPrefix.tr_ui(lang),
            rgb_name.as_deref().unwrap_or("Built-in sRGB"),
            cmyk_name.as_deref().unwrap_or("Built-in CMYK LUT"),
            gray_name.as_deref().unwrap_or("Built-in Gray"),
        ))
        .size(11.0)
        .color(tokens.text_muted),
    );
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        for (slot_idx, label_key) in [
            (0, crate::i18n::I18nKey::DlgOptLoadRgbIcc),
            (1, crate::i18n::I18nKey::DlgOptLoadCmykIcc),
            (2, crate::i18n::I18nKey::DlgOptLoadGrayIcc),
        ] {
            let label = label_key.tr_ui(lang);
            if ui.button(&label).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title(label)
                    .add_filter("ICC Profiles", &["icc", "ICC", "icm", "ICM"])
                    .pick_file()
                {
                    if let Ok(bytes) = std::fs::read(&path) {
                        if bytes.len() <= 16 * 1024 * 1024 {
                            let mut cmd = CommandOp::SetDocumentProperties {
                                title: None,
                                color_mode: None,
                                default_unit: None,
                                dpi: None,
                                multipage_view: None,
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
                            };
                            if let CommandOp::SetDocumentProperties {
                                rgb_icc_profile,
                                cmyk_icc_profile,
                                gray_icc_profile,
                                ..
                            } = &mut cmd
                            {
                                match slot_idx {
                                    0 => *rgb_icc_profile = Some(bytes),
                                    1 => *cmyk_icc_profile = Some(bytes),
                                    _ => *gray_icc_profile = Some(bytes),
                                }
                            }
                            if let Err(err) = app.execute_command(cmd) {
                                app.push_toast(format!("Invalid ICC profile: {err}"), true);
                            } else if let Some(tab) = app.active_tab_mut() {
                                tab.needs_rerender = true;
                            }
                        }
                    }
                }
            }
        }
        if (rgb_name.is_some() || cmyk_name.is_some() || gray_name.is_some())
            && ui
                .small_button(crate::i18n::I18nKey::DlgOptResetSlots.tr_ui(lang))
                .clicked()
        {
            let _ = app.execute_command(CommandOp::SetDocumentProperties {
                title: None,
                color_mode: None,
                default_unit: None,
                dpi: None,
                multipage_view: None,
                active_page_index: None,
                metadata: None,
                custom_icc_profile: None,
                clear_custom_icc: false,
                rgb_icc_profile: None,
                clear_rgb_icc: true,
                cmyk_icc_profile: None,
                clear_cmyk_icc: true,
                gray_icc_profile: None,
                clear_gray_icc: true,
                rendering_intent: None,
                black_point_compensation: None,
                drawing_scale: None,
            });
            if let Some(tab) = app.active_tab_mut() {
                tab.needs_rerender = true;
            }
        }
    });

    ui.add_space(8.0);
    ui.label(RichText::new(crate::i18n::I18nKey::ViewDisplayIcc.tr_ui(lang)).strong());
    let mut use_disp = app.use_display_icc;
    if ui
        .checkbox(
            &mut use_disp,
            crate::i18n::I18nKey::DlgOptDisplayIccApply.tr_ui(lang),
        )
        .changed()
    {
        app.use_display_icc = use_disp;
        if let Some(tab) = app.active_tab_mut() {
            tab.needs_rerender = true;
        }
    }
    let disp_summary = app.display_icc_info.as_ref().map_or_else(
        || "None (Direct sRGB output)".to_owned(),
        |info| {
            format!(
                "{} ({}, {:.1} KiB)",
                info.description,
                info.source,
                info.bytes.len() as f64 / 1024.0
            )
        },
    );
    ui.label(
        RichText::new(disp_summary)
            .size(11.0)
            .color(tokens.text_muted),
    );
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        if ui
            .button(crate::i18n::I18nKey::DlgOptDetectDispIcc.tr_ui(lang))
            .clicked()
        {
            if !app.detect_os_display_icc() {
                app.push_toast(crate::i18n::I18nKey::DlgOptDetectNoIcc.tr_ui(lang), false);
            } else {
                app.push_toast(
                    crate::i18n::I18nKey::DlgOptDetectLoadedIcc.tr_ui(lang),
                    false,
                );
            }
        }
        if ui
            .button(crate::i18n::I18nKey::DlgOptLoadDispIcc.tr_ui(lang))
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Load Display Monitor ICC Profile")
                .add_filter("ICC Profiles", &["icc", "ICC", "icm", "ICM"])
                .pick_file()
            {
                if let Some(info) = crate::display_icc::load_display_icc_from_path(&path, "File") {
                    app.display_icc_info = Some(info);
                    app.use_display_icc = true;
                    if let Some(tab) = app.active_tab_mut() {
                        tab.needs_rerender = true;
                    }
                } else {
                    app.push_toast(crate::i18n::I18nKey::DlgOptInvalidDispIcc.tr_ui(lang), true);
                }
            }
        }
        if app.display_icc_info.is_some()
            && ui
                .small_button(crate::i18n::I18nKey::DlgOptClearBtn.tr_ui(lang))
                .clicked()
        {
            app.display_icc_info = None;
            app.use_display_icc = false;
            if let Some(tab) = app.active_tab_mut() {
                tab.needs_rerender = true;
            }
        }
    });

    ui.add_space(8.0);
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptStylusHeading.tr_ui(lang)).strong());
    ui.label(
        RichText::new(format!(
            "{} {}",
            crate::i18n::I18nKey::DlgOptDevicePrefix.tr_ui(lang),
            app.stylus_provider.device_summary()
        ))
        .size(11.0)
        .color(tokens.text_muted),
    );
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DlgOptPressureCurve.tr_ui(lang));
        for (lbl_key, json) in [
            (
                crate::i18n::I18nKey::TbPressureLinear,
                "[[0.0,0.0],[1.0,1.0]]",
            ),
            (
                crate::i18n::I18nKey::TbPressureSoft,
                "[[0.0,0.2],[0.5,0.75],[1.0,1.0]]",
            ),
            (
                crate::i18n::I18nKey::TbPressureFirm,
                "[[0.0,0.0],[0.5,0.25],[1.0,1.0]]",
            ),
        ] {
            ui.selectable_value(
                &mut app.tool_settings.pressure_curve_json,
                json.to_owned(),
                lbl_key.tr_ui(lang),
            );
        }
    });

    ui.add_space(6.0);
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptLegacyIcc.tr_ui(lang)).strong());
    let custom_desc = app
        .active_tab()
        .and_then(|t| t.document.custom_icc_name().map(str::to_owned));
    let custom_len = app
        .active_tab()
        .and_then(|t| t.document.custom_icc_profile().map(<[u8]>::len));
    match (custom_desc, custom_len) {
        (Some(name), Some(bytes_len)) => {
            ui.label(
                RichText::new(format!(
                    "{} {name} ({:.1} KiB)",
                    crate::i18n::I18nKey::DlgOptCustomProfilePrefix.tr_ui(lang),
                    bytes_len as f64 / 1024.0
                ))
                .size(11.5)
                .color(tokens.accent),
            );
        }
        _ => {
            ui.label(
                RichText::new(crate::i18n::I18nKey::DlgOptNoCustomIcc.tr_ui(lang))
                    .size(11.5)
                    .color(tokens.text_muted),
            );
        }
    }
    shell_dir.with_horizontal(ui, |ui| {
        if ui
            .button(crate::i18n::I18nKey::DlgOptLoadCustomIcc.tr_ui(lang))
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Load External ICC / ICM Color Profile")
                .add_filter("ICC Profiles", &["icc", "ICC", "icm", "ICM"])
                .add_filter("All Files", &["*"])
                .pick_file()
            {
                const MAX_ICC_BYTES: u64 = 16 * 1024 * 1024;
                let read_res = std::fs::metadata(&path)
                    .map_err(|e| format!("failed to inspect ICC profile: {e}"))
                    .and_then(|meta| {
                        if meta.len() > MAX_ICC_BYTES {
                            Err("ICC profile exceeds 16 MiB safety limit".to_owned())
                        } else {
                            std::fs::read(&path)
                                .map_err(|e| format!("failed to read ICC profile: {e}"))
                        }
                    });
                match read_res {
                    Ok(bytes) => {
                        match app.execute_command(CommandOp::SetDocumentProperties {
                            title: None,
                            color_mode: None,
                            default_unit: None,
                            dpi: None,
                            multipage_view: None,
                            active_page_index: None,
                            metadata: None,
                            custom_icc_profile: Some(bytes),
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
                        }) {
                            Ok(()) => {
                                let desc = app
                                    .active_tab()
                                    .and_then(|t| t.document.custom_icc_name().map(str::to_owned))
                                    .unwrap_or_else(|| "Custom Profile".to_owned());
                                if let Some(tab) = app.active_tab_mut() {
                                    tab.needs_rerender = true;
                                }
                                app.soft_proof_cmyk = true;
                                app.sync_proof_options_to_tabs();
                                app.push_toast(
                                    format!("Attached custom ICC profile: {desc}"),
                                    false,
                                );
                            }
                            Err(err) => {
                                app.push_toast(format!("Invalid ICC profile: {err}"), true);
                            }
                        }
                    }
                    Err(err) => app.push_toast(err, true),
                }
            }
        }
        if custom_len.is_some()
            && ui
                .button(crate::i18n::I18nKey::DlgOptClearCustomIcc.tr_ui(lang))
                .clicked()
        {
            if let Err(err) = app.execute_command(CommandOp::SetDocumentProperties {
                title: None,
                color_mode: None,
                default_unit: None,
                dpi: None,
                multipage_view: None,
                active_page_index: None,
                metadata: None,
                custom_icc_profile: None,
                clear_custom_icc: true,
                rgb_icc_profile: None,
                clear_rgb_icc: false,
                cmyk_icc_profile: None,
                clear_cmyk_icc: false,
                gray_icc_profile: None,
                clear_gray_icc: false,
                rendering_intent: None,
                black_point_compensation: None,
                drawing_scale: None,
            }) {
                app.push_toast(format!("Clear ICC failed: {err}"), true);
            } else {
                if let Some(tab) = app.active_tab_mut() {
                    tab.needs_rerender = true;
                }
                app.sync_proof_options_to_tabs();
                app.push_toast(
                    crate::i18n::I18nKey::DlgOptClearedCustomIcc.tr_ui(lang),
                    false,
                );
            }
        }
    });
}

fn show_options_laser_plugins(
    ui: &mut egui::Ui,
    app: &mut StudioApp,
    tokens: &crate::theme::ThemeTokens,
    shell_dir: crate::i18n::ShellLayoutDirections,
) {
    let lang = app.ui_language;
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptLaserHeading.tr_ui(lang)).strong());

    let plugin_dirs = default_plugin_dirs();
    if let Some(primary_dir) = plugin_dirs.first() {
        ui.label(
            RichText::new(format!(
                "{} {}",
                crate::i18n::I18nKey::DlgOptUserPluginsPrefix.tr_ui(lang),
                primary_dir.display()
            ))
            .monospace()
            .size(11.0)
            .color(tokens.text_muted),
        );
    }

    shell_dir.with_horizontal_wrapped(ui, |ui| {
        if ui
            .button(crate::i18n::I18nKey::MenuInstallSamplePlugin.tr_ui(lang))
            .clicked()
        {
            install_sample_plugin_action(app);
        }
        if ui
            .button(crate::i18n::I18nKey::DlgOptReloadPlugins.tr_ui(lang))
            .clicked()
        {
            reload_plugins_action(app);
        }
    });

    ui.add_space(6.0);
    ui.separator();

    let plugins = app.plugin_registry.plugins().to_vec();
    for plugin in &plugins {
        if let PluginKind::ExternalBridge { export_format, .. } = plugin.kind {
            let override_entry = app
                .plugin_exec_overrides
                .entry(plugin.id.clone())
                .or_default();
            let detected = cached_detect_executable(ui.ctx(), plugin, override_entry);
            Frame::NONE
                .fill(tokens.surface)
                .stroke(Stroke::new(1.0, tokens.border))
                .inner_margin(Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    shell_dir.with_horizontal(ui, |ui| {
                        ui.label(RichText::new(&plugin.name).strong());
                        ui.label(
                            RichText::new(format!(
                                "[{}]",
                                export_format.file_extension().to_uppercase()
                            ))
                            .size(10.5)
                            .color(tokens.accent),
                        );
                        if let Some(ref exe) = detected {
                            ui.label(
                                RichText::new(format!("● Detected ({})", exe.display()))
                                    .size(10.5)
                                    .color(tokens.success),
                            );
                        } else {
                            ui.label(
                                RichText::new(crate::i18n::I18nKey::DlgOptNotDetected.tr_ui(lang))
                                    .size(10.5)
                                    .color(tokens.text_muted),
                            );
                        }
                    });
                    shell_dir.with_horizontal(ui, |ui| {
                        ui.label(crate::i18n::I18nKey::DlgOptExecutable.tr_ui(lang));
                        ui.add(
                            egui::TextEdit::singleline(override_entry)
                                .hint_text(crate::i18n::I18nKey::DlgOptExecHint.tr_ui(lang))
                                .desired_width(340.0),
                        );
                        if ui
                            .small_button(crate::i18n::I18nKey::ExportBrowseButton.tr_ui(lang))
                            .clicked()
                        {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_title(format!("Select {} Executable", plugin.name))
                                .pick_file()
                            {
                                *override_entry = path.to_string_lossy().into_owned();
                            }
                        }
                    });
                });
            ui.add_space(3.0);
        }
    }
}

fn show_options_performance_resources(
    ui: &mut egui::Ui,
    app: &mut StudioApp,
    tokens: &crate::theme::ThemeTokens,
    shell_dir: crate::i18n::ShellLayoutDirections,
) {
    use coree_core::ResourceGovernorMode;
    let lang = app.ui_language;

    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptPerfHeader.tr_ui(lang)).strong());
    ui.add_space(4.0);

    let modes = [
        (
            ResourceGovernorMode::Auto,
            crate::i18n::I18nKey::DlgOptPerfModeAuto,
        ),
        (
            ResourceGovernorMode::EcoLowMemory,
            crate::i18n::I18nKey::DlgOptPerfModeEco,
        ),
        (
            ResourceGovernorMode::Balanced,
            crate::i18n::I18nKey::DlgOptPerfModeBalanced,
        ),
        (
            ResourceGovernorMode::MaxPerformance,
            crate::i18n::I18nKey::DlgOptPerfModeMax,
        ),
    ];
    let mut chosen_mode = None;
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        for (mode, key) in modes {
            if ui
                .selectable_label(app.resource_mode == mode, key.tr_ui(lang))
                .clicked()
            {
                chosen_mode = Some(mode);
            }
        }
    });
    if let Some(m) = chosen_mode {
        app.set_resource_governor_mode(m);
    }

    if app.resource_budget.under_memory_pressure {
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!(
                "⚠ {}",
                crate::i18n::I18nKey::DlgOptPerfPressureWarn.tr_ui(lang)
            ))
            .color(tokens.warning)
            .strong(),
        );
    }

    ui.add_space(8.0);
    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DlgOptPerfDetectedHdr.tr_ui(lang)).strong());
    ui.add_space(4.0);

    let prof = app.host_resources;
    let budget = app.resource_budget;
    let (cached_cpu_tiles, cached_gpu_tiles) = app.active_tab().map_or((0, 0), |t| {
        (
            t.tile_cache.cached_world_tile_count(),
            t.gpu_tile_textures.len(),
        )
    });

    Frame::NONE
        .fill(tokens.surface)
        .stroke(Stroke::new(1.0, tokens.border))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DlgOptPerfCpuWorkers.tr_ui(lang));
                ui.label(
                    RichText::new(format!(
                        "{} threads  →  {} workers",
                        prof.logical_cpus, budget.worker_threads
                    ))
                    .monospace()
                    .strong()
                    .color(tokens.accent),
                );
            });
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DlgOptPerfSystemRam.tr_ui(lang));
                ui.label(
                    RichText::new(format!(
                        "{:.1} GiB / {:.1} GiB",
                        prof.available_ram_gib(),
                        prof.total_ram_gib()
                    ))
                    .monospace()
                    .color(tokens.text),
                );
            });
            if let Some(rss_mib) = prof.process_rss_mib() {
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::DlgOptPerfProcessRss.tr_ui(lang));
                    ui.label(
                        RichText::new(format!("{rss_mib:.1} MiB"))
                            .monospace()
                            .color(tokens.text),
                    );
                });
            }
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DlgOptPerfTileBudget.tr_ui(lang));
                ui.label(
                    RichText::new(format!(
                        "{cached_cpu_tiles} / {} tiles ({:.0} MiB max)",
                        budget.max_cached_world_tiles,
                        budget.max_tile_cache_mib()
                    ))
                    .monospace()
                    .color(tokens.text),
                );
            });
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DlgOptPerfGpuBudget.tr_ui(lang));
                ui.label(
                    RichText::new(format!(
                        "{cached_gpu_tiles} / {} GPU 256×256 textures",
                        budget.max_gpu_tile_textures
                    ))
                    .monospace()
                    .color(tokens.success),
                );
            });
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DlgOptPerfUndoBudget.tr_ui(lang));
                ui.label(
                    RichText::new(format!("{} steps", budget.undo_history_limit))
                        .monospace()
                        .color(tokens.text),
                );
            });
        });

    ui.add_space(8.0);
    if ui
        .button(crate::i18n::I18nKey::DlgOptPerfPurgeBtn.tr_ui(lang))
        .clicked()
    {
        let _ = app.purge_unused_caches();
        let msg = crate::i18n::I18nKey::DlgOptPerfPurgeToast.tr_ui(lang);
        app.push_toast(msg, false);
    }
}
