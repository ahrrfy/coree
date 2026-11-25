// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

pub(super) fn ui_find_replace_docker(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let tokens = app.theme.tokens();
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let fr = &mut app.find_replace_state;
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockFindScope.tr_ui(lang));
        ui.selectable_value(
            &mut fr.scope,
            FindScope::CurrentPage,
            crate::i18n::I18nKey::FindScopePage.tr_ui(lang),
        )
        .on_hover_text(crate::i18n::I18nKey::FindScopeSelection.tr_ui(lang));
        ui.selectable_value(
            &mut fr.scope,
            FindScope::WholeDocument,
            crate::i18n::I18nKey::FindScopeDocument.tr_ui(lang),
        );
    });
    ComboBox::from_id_salt("fr_kind")
        .selected_text(
            fr.object_kind
                .map_or(crate::i18n::I18nKey::DockFindAnyObj.tr_ui(lang), |k| {
                    format!("{k:?}")
                }),
        )
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut fr.object_kind,
                None,
                crate::i18n::I18nKey::DockFindAnyObj.tr_ui(lang),
            );
            for k in [
                ObjectKindFilter::Path,
                ObjectKindFilter::Shape,
                ObjectKindFilter::AnyText,
                ObjectKindFilter::Raster,
                ObjectKindFilter::Table,
                ObjectKindFilter::Group,
                ObjectKindFilter::PowerClip,
            ] {
                ui.selectable_value(&mut fr.object_kind, Some(k), format!("{k:?}"));
            }
        });
    ComboBox::from_id_salt("fr_lens")
        .selected_text(
            fr.lens_filter
                .map_or(crate::i18n::I18nKey::DockFindAnyLens.tr_ui(lang), |l| {
                    format!("{l:?}")
                }),
        )
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut fr.lens_filter,
                None,
                crate::i18n::I18nKey::DockFindAnyLens.tr_ui(lang),
            );
            ui.selectable_value(
                &mut fr.lens_filter,
                Some(LensKindFilter::AnyLens),
                crate::i18n::I18nKey::DockFindHasLens.tr_ui(lang),
            );
            ui.selectable_value(
                &mut fr.lens_filter,
                Some(LensKindFilter::Magnify),
                "Magnify",
            );
            ui.selectable_value(
                &mut fr.lens_filter,
                Some(LensKindFilter::FishEye),
                "Fish Eye",
            );
            ui.selectable_value(
                &mut fr.lens_filter,
                Some(LensKindFilter::Invert),
                crate::i18n::I18nKey::TbInvert.tr_ui(lang),
            );
        });
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockFindTextLabel.tr_ui(lang));
        ui.text_edit_singleline(&mut fr.text_query);
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockFindReplaceLabel.tr_ui(lang));
        ui.text_edit_singleline(&mut fr.replace_text);
    });
    ui.checkbox(
        &mut fr.case_sensitive,
        crate::i18n::I18nKey::DockFindCaseSens.tr_ui(lang),
    );

    ui.separator();
    ui.label(
        RichText::new(crate::i18n::I18nKey::DockFindAttrFilters.tr_ui(lang))
            .strong()
            .size(11.0),
    );
    shell_dir.with_horizontal(ui, |ui| {
        ui.checkbox(
            &mut fr.use_fill_color,
            crate::i18n::I18nKey::FindFillColor.tr_ui(lang),
        );
        let mut fc = core_color_to_egui(&fr.find_fill_color);
        if ui.color_edit_button_srgba(&mut fc).changed() {
            fr.find_fill_color = egui_color_to_core(fc);
            fr.use_fill_color = true;
        }
        ui.checkbox(
            &mut fr.use_outline_color,
            crate::i18n::I18nKey::FindOutlineColor.tr_ui(lang),
        );
        let mut oc = core_color_to_egui(&fr.find_outline_color);
        if ui.color_edit_button_srgba(&mut oc).changed() {
            fr.find_outline_color = egui_color_to_core(oc);
            fr.use_outline_color = true;
        }
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.checkbox(
            &mut fr.use_outline_width,
            crate::i18n::I18nKey::PropOutlineWidth.tr_ui(lang),
        );
        ui.add_enabled(
            fr.use_outline_width,
            DragValue::new(&mut fr.outline_width_min_mm)
                .range(0.0..=100.0)
                .speed(0.05)
                .prefix(format!(
                    "{}: ",
                    crate::i18n::I18nKey::FindOutlineWidthMin.tr_ui(lang)
                )),
        );
        ui.add_enabled(
            fr.use_outline_width,
            DragValue::new(&mut fr.outline_width_max_mm)
                .range(0.0..=100.0)
                .speed(0.05)
                .prefix(format!(
                    "{}: ",
                    crate::i18n::I18nKey::FindOutlineWidthMax.tr_ui(lang)
                )),
        );
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::PropOutlineHairline.tr_ui(lang));
        ui.selectable_value(
            &mut fr.filter_hairline,
            None,
            crate::i18n::I18nKey::FindHairlineAny.tr_ui(lang),
        );
        ui.selectable_value(
            &mut fr.filter_hairline,
            Some(true),
            crate::i18n::I18nKey::FindHairlineOnly.tr_ui(lang),
        );
        ui.selectable_value(
            &mut fr.filter_hairline,
            Some(false),
            crate::i18n::I18nKey::FindHairlineNon.tr_ui(lang),
        );
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::FindFontFamily.tr_ui(lang));
        ui.add(egui::TextEdit::singleline(&mut fr.font_query).desired_width(96.0));
        ui.label(crate::i18n::I18nKey::FindLayerFilter.tr_ui(lang));
        ui.add(egui::TextEdit::singleline(&mut fr.layer_filter).desired_width(72.0));
    });

    let mut action_to_run: Option<ReplaceAction> = None;
    let mut find_only = false;
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        if ui
            .button(crate::i18n::I18nKey::FindRunSelect.tr_ui(lang))
            .clicked()
        {
            find_only = true;
        }
        if ui
            .button(crate::i18n::I18nKey::ReplaceRunAction.tr_ui(lang))
            .clicked()
        {
            action_to_run = Some(ReplaceAction::ReplaceText {
                find: fr.text_query.clone(),
                replace_with: fr.replace_text.clone(),
                case_sensitive: fr.case_sensitive,
            });
        }
        if ui
            .button(crate::i18n::I18nKey::ReplaceFillColor.tr_ui(lang))
            .clicked()
        {
            action_to_run = Some(ReplaceAction::ReplaceFillColor {
                color: fr.replace_fill_color.clone(),
            });
        }
        if ui
            .button(crate::i18n::I18nKey::DockFindReplaceOutline.tr_ui(lang))
            .on_hover_text(crate::i18n::I18nKey::ReplaceOutlineColor.tr_ui(lang))
            .clicked()
        {
            action_to_run = Some(ReplaceAction::ReplaceOutlineColor {
                color: fr.replace_outline_color.clone(),
            });
        }
        if ui
            .button(crate::i18n::I18nKey::DockFindReplaceWidth.tr_ui(lang))
            .on_hover_text(crate::i18n::I18nKey::ReplaceOutlineWidth.tr_ui(lang))
            .clicked()
        {
            action_to_run = Some(ReplaceAction::ReplaceOutlineWidth {
                width_mm: fr.replace_outline_width_mm,
                is_hairline: fr.replace_outline_width_mm <= 0.05,
            });
        }
        if ui
            .button(crate::i18n::I18nKey::DockFindReplaceFontBtn.tr_ui(lang))
            .on_hover_text(crate::i18n::I18nKey::ReplaceFontFamily.tr_ui(lang))
            .clicked()
            && !fr.replace_font.trim().is_empty()
        {
            action_to_run = Some(ReplaceAction::ReplaceFontFamily {
                font_family: fr.replace_font.trim().to_owned(),
            });
        }
        if ui
            .button(crate::i18n::I18nKey::DockFindSetOverprint.tr_ui(lang))
            .clicked()
        {
            action_to_run = Some(ReplaceAction::ConvertOverprint { overprint: true });
        }
    });

    let mut rc = core_color_to_egui(&fr.replace_fill_color);
    let mut roc = core_color_to_egui(&fr.replace_outline_color);
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockFindFillLabel.tr_ui(lang));
        if ui.color_edit_button_srgba(&mut rc).changed() {
            fr.replace_fill_color = egui_color_to_core(rc);
        }
        ui.label(crate::i18n::I18nKey::DockFindLineLabel.tr_ui(lang));
        if ui.color_edit_button_srgba(&mut roc).changed() {
            fr.replace_outline_color = egui_color_to_core(roc);
        }
        ui.add(
            DragValue::new(&mut fr.replace_outline_width_mm)
                .range(0.01..=50.0)
                .speed(0.05)
                .suffix(" mm"),
        );
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockFindReplaceFontLbl.tr_ui(lang));
        ui.add(egui::TextEdit::singleline(&mut fr.replace_font).desired_width(140.0));
    });

    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::FindCqlQuery.tr_ui(lang)).strong());
    ui.add(
        egui::TextEdit::singleline(&mut fr.cql_query)
            .desired_width(f32::INFINITY)
            .hint_text("@outline.hairline and @color == 'CutContour'"),
    );
    let mut run_cql = false;
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        for (lbl, preset) in [
            (
                crate::i18n::I18nKey::DockFindCqlHairlines.tr_ui(lang),
                "@outline.hairline",
            ),
            ("CutContour".to_owned(), "@color == 'CutContour'"),
            (
                crate::i18n::I18nKey::CollectSpotInks.tr_ui(lang),
                "@fill.model == 'spot'",
            ),
            (
                crate::i18n::I18nKey::PropOutlineOverprint.tr_ui(lang),
                "@outline.overprint",
            ),
            (
                crate::i18n::I18nKey::DockFindCqlRgbFill.tr_ui(lang),
                "@fill.model == 'rgb'",
            ),
        ] {
            if ui.small_button(lbl).clicked() {
                fr.cql_query = preset.to_owned();
                run_cql = true;
            }
        }
    });
    if ui
        .button(crate::i18n::I18nKey::DockFindSelectCql.tr_ui(lang))
        .clicked()
    {
        run_cql = true;
    }
    if let Some(ref cql_err) = fr.last_cql_error {
        ui.label(RichText::new(cql_err).size(11.0).color(tokens.danger));
    }

    if find_only {
        app.run_find_select();
    } else if let Some(act) = action_to_run {
        app.run_find_and_replace_action(act);
    } else if run_cql {
        if let Err(err) = app.run_cql_select() {
            app.find_replace_state.last_cql_error = Some(err);
        }
    }

    if let Some(ref rep) = app.find_replace_state.last_report {
        ui.separator();
        ui.label(format!(
            "Matched: {} · Modified: {} · Replacements: {}",
            rep.matched_ids.len(),
            rep.modified_ids.len(),
            rep.replacements_applied
        ));
    }
}

// =============================================================================
// 10. Symbols & Object Styles Docker
// =============================================================================

pub(super) fn ui_symbols_styles_docker(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    ui.label(RichText::new(crate::i18n::I18nKey::DockSymHeading.tr_ui(lang)).strong());
    let editing_sym_name = app
        .active_tab()
        .and_then(|t| t.editing_symbol.as_ref().map(|s| s.symbol_name.clone()));
    if let Some(sname) = editing_sym_name {
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(RichText::new(format!("Editing '{sname}'")).strong());
            if ui
                .button(crate::i18n::I18nKey::DockSymFinishEdit.tr_ui(lang))
                .clicked()
            {
                app.finish_edit_symbol(true);
            }
            if ui
                .button(crate::i18n::I18nKey::DialogCancel.tr_ui(lang))
                .clicked()
            {
                app.finish_edit_symbol(false);
            }
        });
    } else if ui
        .button(crate::i18n::I18nKey::DockSymCreate.tr_ui(lang))
        .clicked()
    {
        let new_sym = app.active_tab().and_then(|tab| {
            let objs = tab.document.collect_subtree_objects(&tab.selection.objects);
            if objs.is_empty() {
                None
            } else {
                let n = tab.document.symbols().len() + 1;
                Some(SymbolDef::new(format!("Symbol {n}"), objs))
            }
        });
        if let Some(symbol) = new_sym {
            let _ = app.execute_command(CommandOp::UpsertSymbol { symbol });
        }
    }

    let syms: Vec<_> = app
        .active_tab()
        .map(|t| {
            t.document
                .symbols()
                .values()
                .map(|s| (s.id, s.name.clone(), s.objects.len()))
                .collect()
        })
        .unwrap_or_default();
    for (sid, name, cnt) in syms {
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(format!("{name} ({cnt} obj)"));
            if ui
                .small_button(crate::i18n::I18nKey::DockSymInsert.tr_ui(lang))
                .clicked()
            {
                if let Some(lid) = app.ensure_active_creation_layer() {
                    if let Err(e) = app.execute_command(CommandOp::InstantiateSymbol {
                        symbol_id: sid,
                        layer_id: lid,
                        transform: Affine2D::translate(30.0, 30.0),
                        name: Some(format!("{name} Instance")),
                    }) {
                        app.push_toast(e, true);
                    }
                }
            }
            if ui
                .small_button(crate::i18n::I18nKey::DockSymEditBtn.tr_ui(lang))
                .clicked()
            {
                app.begin_edit_symbol(sid);
            }
            if ui
                .small_button(crate::i18n::I18nKey::DockSymDelBtn.tr_ui(lang))
                .clicked()
            {
                if let Err(e) = app.execute_command(CommandOp::DeleteSymbol { id: sid }) {
                    app.push_toast(e, true);
                }
            }
        });
    }
    let selected_sym_inst = app.active_tab().and_then(|t| {
        t.selection.objects.iter().copied().find(|&id| {
            t.document
                .object(id)
                .is_some_and(|o| matches!(o.payload, NodePayload::SymbolInstance(_)))
        })
    });
    if let Some(inst_id) = selected_sym_inst {
        if ui
            .button(crate::i18n::I18nKey::DockSymUnlinkInst.tr_ui(lang))
            .clicked()
        {
            if let Err(e) = app.execute_command(CommandOp::UnlinkSymbolInstance { id: inst_id }) {
                app.push_toast(e, true);
            }
        }
    }
    let (has_sel_for_clone, has_selected_clone) = app
        .active_tab()
        .map(|t| {
            (
                !t.selection.objects.is_empty(),
                t.selection.objects.iter().any(|id| {
                    t.document
                        .object(*id)
                        .is_some_and(|o| matches!(o.payload, NodePayload::Clone { .. }))
                }),
            )
        })
        .unwrap_or((false, false));
    shell_dir.with_horizontal(ui, |ui| {
        if ui
            .add_enabled(
                has_sel_for_clone,
                egui::Button::new(crate::i18n::I18nKey::DockSymCloneSel.tr_ui(lang)),
            )
            .clicked()
        {
            let _ = app.clone_selection();
        }
        if ui
            .add_enabled(
                has_selected_clone,
                egui::Button::new(crate::i18n::I18nKey::CtxUnlinkClone.tr_ui(lang)),
            )
            .clicked()
        {
            app.unlink_clone_selection();
        }
    });

    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DockColorStylesHdr.tr_ui(lang)).strong());
    if ui
        .button(crate::i18n::I18nKey::DockColorStyleCreate.tr_ui(lang))
        .clicked()
    {
        let new_cs = app.active_tab().and_then(|t| {
            let obj = t
                .selection
                .objects
                .first()
                .and_then(|id| t.document.object(*id))?;
            let col = obj
                .fill
                .referenced_colors()
                .into_iter()
                .next()
                .unwrap_or(Color::CYAN);
            let n = t.document.color_styles().len() + 1;
            Some(coree_core::ColorStyle::new(
                format!("Color {n}"),
                col,
                Some(coree_core::ColorHarmony::Complementary),
            ))
        });
        if let Some(style) = new_cs {
            let _ = app.execute_command(CommandOp::UpsertColorStyle { style });
        }
    }
    let color_styles: Vec<_> = app
        .active_tab()
        .map(|t| t.document.color_styles().to_vec())
        .unwrap_or_default();
    for mut cs in color_styles {
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(&cs.name);
            let mut rgb = cs.base_color.to_rgba8();
            if ui.color_edit_button_srgba_unmultiplied(&mut rgb).changed() {
                cs.set_base_color(Color::rgba8(rgb[0], rgb[1], rgb[2], rgb[3]));
                let _ = app.execute_command(CommandOp::UpsertColorStyle { style: cs.clone() });
            }
            if ui
                .small_button(crate::i18n::I18nKey::DockLinkBtn.tr_ui(lang))
                .clicked()
            {
                let ids = app
                    .active_tab()
                    .map(|t| t.selection.objects.clone())
                    .unwrap_or_default();
                if !ids.is_empty() {
                    let _ = app.execute_command(CommandOp::SetObjectColorStyleLink {
                        ids,
                        color_style_id: Some(cs.id),
                        slot: None,
                    });
                }
            }
            if ui
                .small_button(crate::i18n::I18nKey::DockSymDelBtn.tr_ui(lang))
                .clicked()
            {
                let _ = app.execute_command(CommandOp::DeleteColorStyle { id: cs.id });
            }
        });
    }

    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DockObjStylesHdr.tr_ui(lang)).strong());
    if ui
        .button(crate::i18n::I18nKey::DockObjStyleCreate.tr_ui(lang))
        .clicked()
    {
        let new_style = app.active_tab().and_then(|tab| {
            let obj = tab
                .selection
                .objects
                .first()
                .and_then(|id| tab.document.object(*id))?;
            let mut st =
                ObjectStyle::new(format!("Style {}", tab.document.object_styles().len() + 1));
            st.fill = Some(obj.fill.clone());
            st.outline = Some(obj.outline.clone());
            st.blend_mode = Some(obj.blend_mode);
            st.opacity = Some(obj.opacity);
            st.color_style_id = obj.color_style_id;
            st.color_style_slot = obj.color_style_slot;
            Some(st)
        });
        if let Some(style) = new_style {
            let _ = app.execute_command(CommandOp::UpsertObjectStyle { style });
        }
    }
    if ui
        .small_button(crate::i18n::I18nKey::DockObjStyleUnlink.tr_ui(lang))
        .clicked()
    {
        let ids = app
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if !ids.is_empty() {
            let _ = app.execute_command(CommandOp::SetObjectStyleLink {
                ids,
                style_id: None,
            });
        }
    }
    let styles: Vec<_> = app
        .active_tab()
        .map(|t| t.document.object_styles().to_vec())
        .unwrap_or_default();
    for st in styles {
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(&st.name);
            if ui
                .small_button(crate::i18n::I18nKey::DialogApply.tr_ui(lang))
                .clicked()
            {
                let ids = app
                    .active_tab()
                    .map(|t| t.selection.objects.clone())
                    .unwrap_or_default();
                if ids.is_empty() {
                    app.push_toast("Select an object to apply Object Style", true);
                } else if let Err(err) = app.execute_command(CommandOp::SetObjectStyleLink {
                    ids,
                    style_id: Some(st.id),
                }) {
                    app.push_toast(format!("Apply Object Style failed: {err}"), true);
                }
            }
            if ui
                .small_button(crate::i18n::I18nKey::DockSymDelBtn.tr_ui(lang))
                .clicked()
            {
                let _ = app.execute_command(CommandOp::DeleteObjectStyle { id: st.id });
            }
        });
    }
}

// =============================================================================
// 11. Local Font Manager Docker (`S07` — backed by `coree-storage` font_index)
// =============================================================================

pub(super) fn ui_font_manager_docker(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut need_refresh = !app.font_manager_state.scanned_once;
    let mut rescan = false;

    shell_dir.with_horizontal(ui, |ui| {
        if ui
            .add(
                egui::TextEdit::singleline(&mut app.font_manager_state.search_query)
                    .hint_text(crate::i18n::I18nKey::DockFontsSearchHint.tr_ui(lang))
                    .desired_width(150.0),
            )
            .changed()
        {
            need_refresh = true;
        }
        if ui
            .small_button(crate::i18n::I18nKey::DockFontsScanBtn.tr_ui(lang))
            .clicked()
        {
            rescan = true;
            need_refresh = true;
        }
    });
    shell_dir.with_horizontal(ui, |ui| {
        if ui
            .checkbox(
                &mut app.font_manager_state.arabic_only,
                crate::i18n::I18nKey::DockFontsArabicRtl.tr_ui(lang),
            )
            .changed()
        {
            need_refresh = true;
        }
        if ui
            .checkbox(&mut app.font_manager_state.kashida_only, "Kashida")
            .changed()
        {
            need_refresh = true;
        }
        if ui
            .checkbox(&mut app.font_manager_state.favorites_only, "★ Favorites")
            .changed()
        {
            need_refresh = true;
        }
    });

    if need_refresh {
        app.refresh_font_manager(rescan);
    }

    let tokens = app.theme.tokens();
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(
            RichText::new(format!(
                "{} local font face(s)",
                app.font_manager_state.indexed_fonts.len()
            ))
            .size(11.0)
            .color(tokens.text_muted),
        );
        let q_len = app.font_manager_state.quarantined_files.len();
        if q_len > 0 {
            let preview: String = app
                .font_manager_state
                .quarantined_files
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let more = if q_len > 12 {
                format!("\n... and {} more", q_len - 12)
            } else {
                String::new()
            };
            ui.label(
                RichText::new(format!("⚠ {q_len} quarantined"))
                    .size(10.5)
                    .color(tokens.warning),
            )
            .on_hover_text(format!(
                "Corrupted/unparseable system font files skipped safely:\n{preview}{more}"
            ));
        }
    });
    ui.separator();

    let fonts = app.font_manager_state.indexed_fonts.clone();
    for f in fonts.iter().take(80) {
        shell_dir.with_horizontal(ui, |ui| {
            let star = if f.favorite { "★" } else { "☆" };
            if ui
                .small_button(star)
                .on_hover_text(crate::i18n::I18nKey::DockFontsToggleFavTip.tr_ui(lang))
                .clicked()
            {
                app.toggle_font_favorite(&f.id, !f.favorite);
            }
            let ar = if f.supports_arabic_rtl { " [AR]" } else { "" };
            let var = if f.is_variable { " [Var]" } else { "" };
            let pan = if f.panose.family_type() != 0 {
                format!(" P:{}", f.panose.family_type())
            } else {
                String::new()
            };
            if ui
                .button(format!(
                    "{} {}{ar}{var}{pan}",
                    f.family_name, f.subfamily_name
                ))
                .clicked()
            {
                app.default_text_style.font_family = f.family_name.clone();
                // Apply to selected text object(s) via undoable, WAL-journaled EditText commands
                let edit_ops: Vec<CommandOp> = app
                    .active_tab()
                    .map(|tab| {
                        tab.selection
                            .objects
                            .iter()
                            .copied()
                            .filter_map(|oid| {
                                let obj = tab.document.object(oid)?;
                                let mut payload = obj.payload.clone();
                                match &mut payload {
                                    NodePayload::ArtisticText(t) => {
                                        for r in &mut t.runs {
                                            r.style.font_family = f.family_name.clone();
                                        }
                                        Some(CommandOp::EditText { id: oid, payload })
                                    }
                                    NodePayload::ParagraphText(p) => {
                                        for r in &mut p.runs {
                                            r.style.font_family = f.family_name.clone();
                                        }
                                        Some(CommandOp::EditText { id: oid, payload })
                                    }
                                    NodePayload::TextOnPath(top) => {
                                        for r in &mut top.text.runs {
                                            r.style.font_family = f.family_name.clone();
                                        }
                                        Some(CommandOp::EditText { id: oid, payload })
                                    }
                                    _ => None,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if edit_ops.len() == 1 {
                    let _ = app.execute_command(edit_ops.into_iter().next().unwrap());
                } else if !edit_ops.is_empty() {
                    let _ = app.execute_command(CommandOp::Batch(edit_ops));
                }
                app.push_toast(format!("Selected font '{}'", f.family_name), false);
            }
        });
    }

    ui.separator();
    egui::CollapsingHeader::new(
        RichText::new(crate::i18n::I18nKey::DockFontsPanoseHdr.tr_ui(lang))
            .strong()
            .color(tokens.accent),
    )
    .id_salt("font_mgr_panose_sub")
    .default_open(false)
    .show(ui, |ui| {
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(crate::i18n::I18nKey::DockFontsMissingLbl.tr_ui(lang));
            ui.add(
                egui::TextEdit::singleline(&mut app.font_manager_state.missing_family_input)
                    .hint_text(crate::i18n::I18nKey::DockFontsMissingHint.tr_ui(lang))
                    .desired_width(110.0),
            );
        });
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(crate::i18n::I18nKey::DockFontsSubLbl.tr_ui(lang));
            ui.add(
                egui::TextEdit::singleline(&mut app.font_manager_state.substitute_family_input)
                    .hint_text(crate::i18n::I18nKey::DockFontsSubHint.tr_ui(lang))
                    .desired_width(110.0),
            );
        });
        shell_dir.with_horizontal(ui, |ui| {
            if ui
                .small_button(crate::i18n::I18nKey::DockFontsPanoseMatch.tr_ui(lang))
                .clicked()
            {
                let target_panose = app
                    .font_manager_state
                    .indexed_fonts
                    .iter()
                    .find(|f| {
                        f.family_name.eq_ignore_ascii_case(
                            app.font_manager_state.missing_family_input.trim(),
                        )
                    })
                    .map_or(
                        coree_storage::PanoseClassification([2, 0, 5, 3, 0, 0, 0, 0, 0, 0]),
                        |f| f.panose,
                    );
                let _ = app.find_best_panose_substitute(
                    &target_panose,
                    400,
                    false,
                    app.font_manager_state.arabic_only,
                );
            }
            if ui
                .small_button(crate::i18n::I18nKey::DockFontsSaveRule.tr_ui(lang))
                .clicked()
            {
                let missing = app.font_manager_state.missing_family_input.clone();
                let sub = app.font_manager_state.substitute_family_input.clone();
                let pan = app
                    .font_manager_state
                    .panose_suggestion
                    .as_ref()
                    .map(|(r, _)| r.panose);
                if let Err(err) = app.set_font_substitution(&missing, &sub, pan.as_ref()) {
                    app.push_toast(format!("Substitution error: {err}"), true);
                }
            }
            if ui
                .small_button(crate::i18n::I18nKey::DockFontsApplyDoc.tr_ui(lang))
                .clicked()
            {
                let missing = app.font_manager_state.missing_family_input.clone();
                let sub = app.font_manager_state.substitute_family_input.clone();
                let _ = app.apply_font_substitution_to_active_document(&missing, &sub);
            }
        });
        if let Some((ref best, score)) = app.font_manager_state.panose_suggestion {
            ui.label(
                RichText::new(format!(
                    "Best PANOSE match: {} (dist {score})",
                    best.family_name
                ))
                .size(10.5)
                .color(tokens.text_muted),
            );
        }
        let saved_subs = app.font_manager_state.substitutions.clone();
        for sub in &saved_subs {
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} → {}",
                        sub.missing_family, sub.substitute_family
                    ))
                    .size(10.5),
                );
                if ui
                    .small_button(crate::i18n::I18nKey::DialogApply.tr_ui(lang))
                    .clicked()
                {
                    let _ = app.apply_font_substitution_to_active_document(
                        &sub.missing_family,
                        &sub.substitute_family,
                    );
                }
                if ui.small_button("✕").clicked() {
                    app.remove_font_substitution(&sub.missing_family);
                }
            });
        }
    });

    ui.separator();
    ui_glyphs_browser(ui, app, "font_mgr_glyphs");
}

// =============================================================================
// 12. Visual Command History Docker (`S17`)
// =============================================================================

pub(super) fn ui_history_docker(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let tokens = app.theme.tokens();
    shell_dir.with_horizontal(ui, |ui| {
        if ui
            .button(crate::i18n::I18nKey::DockHistUndo.tr_ui(lang))
            .clicked()
        {
            let _ = app.undo();
        }
        if ui
            .button(crate::i18n::I18nKey::DockHistRedo.tr_ui(lang))
            .clicked()
        {
            let _ = app.redo();
        }
        if ui
            .button(crate::i18n::I18nKey::DockHistSaveSnap.tr_ui(lang))
            .clicked()
        {
            if let Err(e) = app.perform_autosave_active() {
                app.push_toast(e, true);
            } else {
                app.push_toast("Saved crash-recovery snapshot", false);
            }
        }
    });
    ui.separator();

    if let Some(tab) = app.active_tab() {
        ui.label(RichText::new(crate::i18n::I18nKey::DockHistLogHdr.tr_ui(lang)).strong());
        let undo_labels = tab.history.undo_labels();
        if undo_labels.is_empty() {
            ui.label(
                RichText::new(crate::i18n::I18nKey::DockHistNoCmds.tr_ui(lang))
                    .color(tokens.text_muted),
            );
        }
        for (i, lbl) in undo_labels.iter().enumerate() {
            ui.label(format!("{}. {lbl}", i + 1));
        }

        let redo_labels = tab.history.redo_labels();
        if !redo_labels.is_empty() {
            ui.separator();
            ui.label(
                RichText::new(crate::i18n::I18nKey::DockHistRedoStack.tr_ui(lang))
                    .color(tokens.text_muted),
            );
            for lbl in redo_labels.iter().rev() {
                ui.label(RichText::new(format!("↳ {lbl}")).color(tokens.text_muted));
            }
        }
    }
}

// =============================================================================
// 13. Sandboxed Rhai Macros Studio Docker (`S17`)
// =============================================================================

pub(super) fn ui_macros_docker(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let tokens = app.theme.tokens();
    let recording = app.active_tab().is_some_and(|t| t.history.recording_macro);

    shell_dir.with_horizontal(ui, |ui| {
        if recording {
            if ui
                .button(
                    RichText::new(crate::i18n::I18nKey::DockMacroStopRhai.tr_ui(lang))
                        .color(tokens.accent_secondary),
                )
                .clicked()
            {
                app.stop_macro_recording();
            }
        } else if ui
            .button(crate::i18n::I18nKey::DockMacroRecord.tr_ui(lang))
            .clicked()
        {
            app.start_macro_recording();
        }
        if ui
            .button(
                RichText::new(crate::i18n::I18nKey::DockMacroRunScript.tr_ui(lang))
                    .color(tokens.success),
            )
            .clicked()
        {
            let src = app.macro_state.script_source.clone();
            let _ = app.run_rhai_script(&src);
        }
    });

    shell_dir.with_horizontal_wrapped(ui, |ui| {
        ui.label(
            RichText::new(crate::i18n::I18nKey::DockMacroBuiltins.tr_ui(lang))
                .size(11.0)
                .color(tokens.text_muted),
        );
        for m in builtin_macros() {
            if ui.small_button(m.id).on_hover_text(m.description).clicked() {
                app.macro_state.script_source = m.script.trim().to_owned();
            }
        }
    });

    ui.add(
        egui::TextEdit::multiline(&mut app.macro_state.script_source)
            .code_editor()
            .desired_rows(10)
            .desired_width(f32::INFINITY),
    );

    if let Some(ref err) = app.macro_state.last_error {
        ui.label(RichText::new(format!("Error: {err}")).color(tokens.danger));
    }
    if !app.macro_state.output_logs.is_empty() {
        ui.separator();
        ui.label(RichText::new(crate::i18n::I18nKey::DockMacroConsole.tr_ui(lang)).strong());
        for line in &app.macro_state.output_logs {
            ui.label(
                RichText::new(line)
                    .monospace()
                    .size(11.0)
                    .color(tokens.success),
            );
        }
    }
}

// =============================================================================
// 14. CDR Binary & Chunk Inspector Docker (`S16`)
// =============================================================================

pub(super) fn ui_glyphs_browser(ui: &mut Ui, app: &mut StudioApp, id_salt: &str) {
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let tokens = app.theme.tokens();
    egui::CollapsingHeader::new(
        RichText::new(crate::i18n::I18nKey::DockGlyphsHdr.tr_ui(lang))
            .strong()
            .color(tokens.accent),
    )
    .id_salt(id_salt)
    .default_open(true)
    .show(ui, |ui| {
        shell_dir.with_horizontal(ui, |ui| {
            ComboBox::from_id_salt((id_salt, "block"))
                .width(138.0)
                .selected_text(app.glyph_block_filter.label())
                .show_ui(ui, |ui| {
                    for block in GlyphBlockFilter::ALL_FILTERS {
                        ui.selectable_value(&mut app.glyph_block_filter, block, block.label());
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut app.glyph_search)
                    .hint_text(crate::i18n::I18nKey::DockGlyphsSearchHint.tr_ui(lang))
                    .desired_width(110.0),
            );
        });

        let active_font = if app.default_text_style.font_family.trim().is_empty() {
            None
        } else {
            Some(app.default_text_style.font_family.as_str())
        };
        let glyphs =
            list_glyphs_for_block(active_font, app.glyph_block_filter, &app.glyph_search, 120);
        ui.label(
            RichText::new(format!(
                "{} glyph(s) · Click any tile to insert",
                glyphs.len()
            ))
            .size(10.5)
            .color(tokens.text_muted),
        );

        let mut insert_glyph: Option<String> = None;
        egui::ScrollArea::vertical()
            .id_salt((id_salt, "grid_scroll"))
            .max_height(185.0)
            .show(ui, |ui| {
                shell_dir.with_horizontal_wrapped(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                    for entry in &glyphs {
                        let glyph_str = entry.ch.to_string();
                        let text_color = if entry.supported_by_font {
                            tokens.text
                        } else {
                            tokens.text_muted
                        };
                        let btn = egui::Button::new(
                            RichText::new(&glyph_str).size(15.0).color(text_color),
                        )
                        .min_size(vec2(28.0, 28.0));
                        let tip = format!(
                            "{}\n{} · {}\nClick to insert",
                            entry.name,
                            entry.codepoint_hex,
                            entry.block.label()
                        );
                        if ui.add(btn).on_hover_text(tip).clicked() {
                            insert_glyph = Some(glyph_str);
                        }
                    }
                });
            });

        if let Some(g) = insert_glyph {
            app.insert_glyph_at_selection(&g);
        }
    });
}
