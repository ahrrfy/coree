// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

pub(super) fn ui_cdr_inspector_docker(ui: &mut Ui, app: &mut StudioApp) {
    const CP1256: u16 = 1256;
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    shell_dir.with_horizontal(ui, |ui| {
        if ui
            .button(crate::i18n::I18nKey::DockCdrRefresh.tr_ui(lang))
            .clicked()
        {
            app.refresh_inspector_for_active(true);
        }
        if ui
            .button(crate::i18n::I18nKey::FileOpenCdrWithCodepage.tr_ui(lang))
            .clicked()
        {
            app.cdr_import_dialog.pending_path =
                app.active_tab().and_then(|t| t.source_path.clone());
            if app.cdr_import_dialog.selected_codepage.is_none() {
                app.cdr_import_dialog.selected_codepage = Some(CP1256);
            }
            if app.cdr_import_dialog.preview_summary.is_empty() {
                if let Some(rep) = app.active_tab().and_then(|t| t.inspection_report.as_ref()) {
                    app.cdr_import_dialog.preview_summary =
                        format!("{} · {} chunk(s)", rep.format_era, rep.chunks.len());
                }
            }
            app.cdr_import_dialog.open = true;
        }
    });
    let Some(rep) = app.active_tab().and_then(|t| t.inspection_report.as_ref()) else {
        ui.label(
            RichText::new(crate::i18n::I18nKey::DockCdrEmptyHint.tr_ui(lang))
                .color(tokens.text_muted),
        );
        return;
    };

    ui.label(RichText::new(&rep.format_era).strong().color(tokens.accent));
    ui.label(format!(
        "{} page(s) · {} layer(s) · {} obj(s) · {} bitmap(s)",
        rep.page_count, rep.layer_count, rep.object_count, rep.bitmap_count
    ));
    if !rep.fonts.is_empty() {
        ui.label(format!("Fonts: {}", rep.fonts.join(", ")));
    }
    if !rep.color_models.is_empty() {
        ui.label(format!("Colors: {}", rep.color_models.join(", ")));
    }

    if !rep.unknown_chunks.is_empty() {
        ui.separator();
        ui.label(
            RichText::new(format!(
                "Preserved Unknown Chunks ({})",
                rep.unknown_chunks.len()
            ))
            .strong()
            .color(tokens.warning),
        );
        for unk in &rep.unknown_chunks {
            ui.label(
                RichText::new(format!(
                    "• '{}' [{}] ({} B, v{})",
                    unk.fourcc, unk.scope, unk.byte_len, unk.version_hint
                ))
                .monospace()
                .size(11.0),
            );
        }
    }

    if !rep.zip_entries.is_empty() {
        ui.separator();
        egui::CollapsingHeader::new(format!(
            "{} ({})",
            crate::i18n::I18nKey::CdrZipEntries.tr_ui(lang),
            rep.zip_entries.len()
        ))
        .show(ui, |ui| {
            for ze in &rep.zip_entries {
                ui.label(
                    RichText::new(format!(
                        "{} ({} → {} B)",
                        ze.name, ze.compressed_size, ze.uncompressed_size
                    ))
                    .monospace()
                    .size(10.5),
                );
            }
        });
    }

    if !rep.chunks.is_empty() {
        ui.separator();
        let mut next_selected = app.selected_cdr_chunk_idx;
        egui::CollapsingHeader::new(format!(
            "{} ({})",
            crate::i18n::I18nKey::CdrRiffChunkTree.tr_ui(lang),
            rep.chunks.len()
        ))
        .default_open(true)
        .show(ui, |ui| {
            for (c_idx, ch) in rep.chunks.iter().take(150).enumerate() {
                let indent = "  ".repeat(ch.depth.min(8));
                let tag = if ch.is_list { "LIST " } else { "" };
                let sel = next_selected == Some(c_idx);
                if ui
                    .selectable_label(
                        sel,
                        RichText::new(format!("{indent}{tag}'{}' ({} B)", ch.fourcc, ch.size))
                            .monospace()
                            .size(10.5),
                    )
                    .clicked()
                {
                    next_selected = Some(c_idx);
                }
            }
        });
        if let Some(chunk) = next_selected.and_then(|idx| rep.chunks.get(idx)) {
            super::hex_view(ui, chunk, lang);
        }
        app.selected_cdr_chunk_idx = next_selected;
    }
}

// =============================================================================
// 15. Laser Cutting, Engraving & Plugins Studio (`Production` -> Laser & Plugins)
// =============================================================================

pub(super) fn ui_laser_plugins_studio(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let tokens = app.theme.tokens();

    // Section A: Built-in & Custom Laser / Plotter Plugins Grid
    ui.label(
        RichText::new(crate::i18n::I18nKey::DockLaserHeading.tr_ui(lang))
            .strong()
            .color(tokens.accent),
    );
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        if ui
            .small_button(crate::i18n::I18nKey::DockLaserInstallSample.tr_ui(lang))
            .clicked()
        {
            install_sample_plugin_action(app);
        }
        if ui
            .small_button(crate::i18n::I18nKey::DockLaserReload.tr_ui(lang))
            .clicked()
        {
            reload_plugins_action(app);
        }
    });
    ui.add_space(4.0);

    let plugins = app.plugin_registry.plugins().to_vec();
    let mut pending_send: Option<(String, bool)> = None;
    let mut pending_run_rhai: Option<coree_prod::PluginManifest> = None;

    for plugin in &plugins {
        Frame::NONE
            .fill(tokens.surface_elevated)
            .stroke(Stroke::new(1.0, tokens.border))
            .corner_radius(CornerRadius::same(5))
            .inner_margin(Margin::same(7))
            .show(ui, |ui| match &plugin.kind {
                PluginKind::ExternalBridge { export_format, .. } => {
                    let override_entry = app
                        .plugin_exec_overrides
                        .entry(plugin.id.clone())
                        .or_default();
                    let detected = cached_detect_executable(ui.ctx(), plugin, override_entry);
                    shell_dir.with_horizontal_wrapped(ui, |ui| {
                        ui.label(RichText::new(&plugin.name).strong());
                        ui.label(
                            RichText::new(format!(
                                "[{}]",
                                export_format.file_extension().to_uppercase()
                            ))
                            .size(10.0)
                            .color(tokens.accent),
                        );
                        if let Some(ref exe) = detected {
                            ui.label(
                                RichText::new(crate::i18n::I18nKey::DockLaserReady.tr_ui(lang))
                                    .size(10.0)
                                    .color(tokens.success),
                            )
                            .on_hover_text(exe.display().to_string());
                        } else {
                            ui.label(
                                RichText::new(
                                    crate::i18n::I18nKey::DockLaserExportOnly.tr_ui(lang),
                                )
                                .size(10.0)
                                .color(tokens.text_muted),
                            )
                            .on_hover_text(crate::i18n::I18nKey::DockLaserNotFoundTip.tr_ui(lang));
                        }
                    });
                    ui.label(
                        RichText::new(&plugin.description)
                            .size(10.5)
                            .color(tokens.text_muted),
                    );
                    shell_dir.with_horizontal(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(override_entry)
                                .hint_text(
                                    crate::i18n::I18nKey::DockLaserCustomPathHint.tr_ui(lang),
                                )
                                .desired_width(168.0),
                        );
                        if ui
                            .small_button("…")
                            .on_hover_text(crate::i18n::I18nKey::DockLaserBrowseExeTip.tr_ui(lang))
                            .clicked()
                        {
                            if let Some(p) = rfd::FileDialog::new()
                                .set_title(format!("Select {} Executable", plugin.name))
                                .pick_file()
                            {
                                *override_entry = p.to_string_lossy().into_owned();
                            }
                        }
                    });
                    shell_dir.with_horizontal(ui, |ui| {
                        if ui
                            .small_button(
                                RichText::new(
                                    crate::i18n::I18nKey::DockLaserSendLaunch.tr_ui(lang),
                                )
                                .color(tokens.accent),
                            )
                            .clicked()
                        {
                            pending_send = Some((plugin.id.clone(), true));
                        }
                        if ui
                            .small_button(crate::i18n::I18nKey::DockLaserExportFileOnly.tr_ui(lang))
                            .clicked()
                        {
                            pending_send = Some((plugin.id.clone(), false));
                        }
                    });
                }
                PluginKind::RhaiScript { entry_label, .. } => {
                    shell_dir.with_horizontal_wrapped(ui, |ui| {
                        ui.label(RichText::new(&plugin.name).strong());
                        ui.label(
                            RichText::new("[Rhai Custom]")
                                .size(10.0)
                                .color(tokens.accent_secondary),
                        );
                    });
                    ui.label(
                        RichText::new(&plugin.description)
                            .size(10.5)
                            .color(tokens.text_muted),
                    );
                    if ui
                        .small_button(
                            RichText::new(format!("▶ {entry_label}")).color(tokens.success),
                        )
                        .clicked()
                    {
                        pending_run_rhai = Some(plugin.clone());
                    }
                }
            });
        ui.add_space(4.0);
    }

    if let Some((pid, launch)) = pending_send {
        app.send_to_laser_plugin(&pid, launch);
    }
    if let Some(plugin) = pending_run_rhai {
        run_rhai_plugin_undoable(app, &plugin);
    }

    if let Some(ref last_path) = app.last_plugin_export_path {
        ui.label(
            RichText::new(format!("Last export: {}", last_path.display()))
                .size(10.0)
                .monospace()
                .color(tokens.text_muted),
        );
    }

    ui.separator();

    // Section B: Color-to-Operation Laser Layers Table
    egui::CollapsingHeader::new(
        RichText::new(format!(
            "Color-to-Operation Laser Layers ({})",
            app.laser_config.rules.len()
        ))
        .strong(),
    )
    .id_salt("laser_layers_table_hdr")
    .default_open(true)
    .show(ui, |ui| {
        shell_dir.with_horizontal(ui, |ui| {
            if ui
                .small_button(crate::i18n::I18nKey::DockLaserAddRule.tr_ui(lang))
                .clicked()
            {
                let n = app.laser_config.rules.len() + 1;
                app.laser_config.rules.push(LaserLayerRule {
                    name: format!("Custom Layer {n}"),
                    match_rgb: [255, 136, 0],
                    color_tolerance: 40,
                    match_cut_layer: false,
                    operation: LaserOperationKind::VectorEngrave,
                    speed_mm_s: 100.0,
                    min_power_pct: 25.0,
                    max_power_pct: 40.0,
                    passes: 1,
                    air_assist: false,
                    kerf_mm: 0.0,
                    z_step_mm: 0.0,
                    frequency_khz: 20.0,
                    hatch_spacing_mm: 0.15,
                    hatch_angle_deg: 45.0,
                    cross_hatch: false,
                    enabled: true,
                });
            }
            if ui
                .small_button(crate::i18n::I18nKey::DockLaserResetDefaults.tr_ui(lang))
                .clicked()
            {
                app.laser_config.rules = default_laser_layer_rules();
            }
        });
        ui.add_space(3.0);

        let mut remove_idx: Option<usize> = None;
        let rule_count = app.laser_config.rules.len();
        for (idx, rule) in app.laser_config.rules.iter_mut().enumerate() {
            Frame::NONE
                .fill(tokens.surface_elevated)
                .stroke(Stroke::new(1.0, tokens.border))
                .corner_radius(CornerRadius::same(4))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    shell_dir.with_horizontal(ui, |ui| {
                        ui.checkbox(&mut rule.enabled, "");
                        let mut c32 = Color32::from_rgb(
                            rule.match_rgb[0],
                            rule.match_rgb[1],
                            rule.match_rgb[2],
                        );
                        if ui.color_edit_button_srgba(&mut c32).changed() {
                            rule.match_rgb = [c32.r(), c32.g(), c32.b()];
                        }
                        ui.add(egui::TextEdit::singleline(&mut rule.name).desired_width(132.0));
                        if rule_count > 1 && ui.small_button("×").clicked() {
                            remove_idx = Some(idx);
                        }
                    });
                    shell_dir.with_horizontal(ui, |ui| {
                        ComboBox::from_id_salt(("laser_op_rule", idx))
                            .width(106.0)
                            .selected_text(laser_op_label(rule.operation))
                            .show_ui(ui, |ui| {
                                for op in [
                                    LaserOperationKind::Cut,
                                    LaserOperationKind::VectorEngrave,
                                    LaserOperationKind::HatchEngrave,
                                    LaserOperationKind::Perforate,
                                    LaserOperationKind::Ignore,
                                ] {
                                    ui.selectable_value(
                                        &mut rule.operation,
                                        op,
                                        laser_op_label(op),
                                    );
                                }
                            });
                        ui.add(
                            DragValue::new(&mut rule.speed_mm_s)
                                .range(0.5..=2000.0)
                                .speed(1.0)
                                .prefix("Spd: ")
                                .suffix(" mm/s"),
                        );
                    });
                    shell_dir.with_horizontal_wrapped(ui, |ui| {
                        ui.add(
                            DragValue::new(&mut rule.max_power_pct)
                                .range(0.0..=100.0)
                                .prefix("Pwr: ")
                                .suffix("%"),
                        );
                        ui.add(
                            DragValue::new(&mut rule.passes)
                                .range(1..=50)
                                .prefix("Pass: "),
                        );
                        ui.add(
                            DragValue::new(&mut rule.kerf_mm)
                                .range(-2.0..=2.0)
                                .speed(0.01)
                                .prefix("Kerf: ")
                                .suffix(" mm"),
                        );
                        ui.checkbox(
                            &mut rule.air_assist,
                            crate::i18n::I18nKey::DockLaserAir.tr_ui(lang),
                        );
                    });
                    if rule.operation == LaserOperationKind::HatchEngrave {
                        shell_dir.with_horizontal_wrapped(ui, |ui| {
                            ui.add(
                                DragValue::new(&mut rule.hatch_spacing_mm)
                                    .range(0.02..=5.0)
                                    .speed(0.01)
                                    .prefix("Hatch: ")
                                    .suffix(" mm"),
                            );
                            ui.add(
                                DragValue::new(&mut rule.hatch_angle_deg)
                                    .range(0.0..=180.0)
                                    .prefix("Ang: ")
                                    .suffix("°"),
                            );
                            ui.checkbox(
                                &mut rule.cross_hatch,
                                crate::i18n::I18nKey::DockLaserCross.tr_ui(lang),
                            );
                        });
                    }
                });
            ui.add_space(3.0);
        }
        if let Some(i) = remove_idx {
            app.laser_config.rules.remove(i);
        }
    });

    ui.separator();

    // Section C: Laser Job Prep & Toolpath Simulator
    ui.label(RichText::new(crate::i18n::I18nKey::DockLaserJobPrepHdr.tr_ui(lang)).strong());
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockLaserOriginLbl.tr_ui(lang));
        ComboBox::from_id_salt("laser_origin_combo")
            .width(105.0)
            .selected_text(format!("{:?}", app.laser_config.origin))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut app.laser_config.origin,
                    LaserOrigin::TopLeft,
                    "TopLeft",
                );
                ui.selectable_value(
                    &mut app.laser_config.origin,
                    LaserOrigin::BottomLeft,
                    "BottomLeft",
                );
                ui.selectable_value(
                    &mut app.laser_config.origin,
                    LaserOrigin::Center,
                    crate::i18n::I18nKey::DockEffectsCenter.tr_ui(lang),
                );
            });
        ComboBox::from_id_salt("laser_dialect_combo")
            .width(96.0)
            .selected_text(match app.laser_config.dialect {
                GcodeDialect::Grbl11 => "GRBL 1.1",
                GcodeDialect::Marlin => "Marlin",
                GcodeDialect::RuidaGcode => "Ruida",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut app.laser_config.dialect,
                    GcodeDialect::Grbl11,
                    "GRBL 1.1",
                );
                ui.selectable_value(
                    &mut app.laser_config.dialect,
                    GcodeDialect::Marlin,
                    "Marlin",
                );
                ui.selectable_value(
                    &mut app.laser_config.dialect,
                    GcodeDialect::RuidaGcode,
                    "Ruida",
                );
            });
    });

    shell_dir.with_horizontal(ui, |ui| {
        ui.add(
            DragValue::new(&mut app.laser_config.dedupe_tolerance_mm)
                .range(0.0..=2.0)
                .speed(0.01)
                .prefix("Dedupe: ")
                .suffix(" mm"),
        );
        ui.add(
            DragValue::new(&mut app.laser_config.join_tolerance_mm)
                .range(0.0..=5.0)
                .speed(0.01)
                .prefix("Join: ")
                .suffix(" mm"),
        );
    });

    shell_dir.with_horizontal_wrapped(ui, |ui| {
        ui.checkbox(
            &mut app.laser_config.inner_first,
            crate::i18n::I18nKey::DockLaserInnerFirst.tr_ui(lang),
        );
        ui.checkbox(
            &mut app.laser_config.optimize_travel,
            crate::i18n::I18nKey::DockLaserOptimizeTravel.tr_ui(lang),
        );
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.checkbox(
            &mut app.laser_config.add_weeding_box,
            crate::i18n::I18nKey::DockLaserWeedingBox.tr_ui(lang),
        );
        if app.laser_config.add_weeding_box {
            ui.add(
                DragValue::new(&mut app.laser_config.weeding_margin_mm)
                    .range(0.5..=50.0)
                    .prefix("Margin: ")
                    .suffix(" mm"),
            );
        }
    });

    ui.add_space(4.0);
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        if ui
            .button(
                RichText::new(crate::i18n::I18nKey::DockLaserComputePreview.tr_ui(lang))
                    .strong()
                    .color(tokens.accent),
            )
            .clicked()
        {
            app.refresh_laser_plan_preview();
        }
        if ui
            .checkbox(
                &mut app.show_laser_toolpath_overlay,
                crate::i18n::I18nKey::DockLaserOverlay.tr_ui(lang),
            )
            .changed()
            && app.show_laser_toolpath_overlay
            && app.laser_plan_preview.is_none()
        {
            app.refresh_laser_plan_preview();
        }
    });

    if let Some(ref plan) = app.laser_plan_preview {
        ui.add_space(4.0);
        Frame::NONE
            .fill(tokens.bg)
            .stroke(Stroke::new(1.0, tokens.border))
            .corner_radius(CornerRadius::same(4))
            .inner_margin(Margin::same(6))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "Cuts: {} · Segments: {} · Est. Time: {:.1}s",
                        plan.cut_count,
                        plan.segments.len(),
                        plan.estimated_seconds
                    ))
                    .strong()
                    .size(11.0)
                    .color(tokens.success),
                );
                ui.label(
                    RichText::new(format!(
                        "Dupes Removed: {} · Curves Joined: {}",
                        plan.duplicate_segments_removed, plan.curves_joined
                    ))
                    .size(10.5)
                    .color(tokens.text_muted),
                );
                ui.label(
                    RichText::new(format!(
                        "Cut: {:.1} mm · Engrave: {:.1} mm · Rapid: {:.1} mm",
                        plan.total_cut_mm, plan.total_engrave_mm, plan.total_rapid_mm
                    ))
                    .size(10.5)
                    .color(tokens.text),
                );
            });
    }

    ui.add_space(6.0);
    ui.label(RichText::new(crate::i18n::I18nKey::DockLaserDirectExport.tr_ui(lang)).strong());
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        if ui
            .small_button(crate::i18n::I18nKey::DockLaserExpGcode.tr_ui(lang))
            .clicked()
        {
            direct_laser_export(app, "gcode");
        }
        if ui
            .small_button(crate::i18n::I18nKey::DockLaserExpDxf.tr_ui(lang))
            .clicked()
        {
            direct_laser_export(app, "dxf");
        }
        if ui
            .small_button(crate::i18n::I18nKey::DockLaserExpPlt.tr_ui(lang))
            .clicked()
        {
            direct_laser_export(app, "plt");
        }
        if ui
            .small_button(crate::i18n::I18nKey::DockLaserExpSvg.tr_ui(lang))
            .clicked()
        {
            direct_laser_export(app, "svg");
        }
    });
}

fn direct_laser_export(app: &mut StudioApp, ext: &str) {
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
    let default_name = format!("laser_job.{ext}");
    if let Some(path) = rfd::FileDialog::new()
        .set_title(format!("Export Laser {}", ext.to_uppercase()))
        .set_file_name(&default_name)
        .add_filter(ext.to_uppercase(), &[ext])
        .save_file()
    {
        let payload = match ext {
            "gcode" => export_laser_gcode(&plan, &app.laser_config),
            "dxf" => export_laser_dxf_r12(&plan),
            "plt" => export_laser_plt(&plan),
            _ => export_laser_svg(&plan),
        };
        match coree_io::atomic_write_bytes(&path, payload.as_bytes()) {
            Ok(()) => {
                app.laser_plan_preview = Some(plan);
                app.push_toast(
                    format!(
                        "Exported {} ({} B) -> {}",
                        ext.to_uppercase(),
                        payload.len(),
                        path.display()
                    ),
                    false,
                );
            }
            Err(e) => app.push_toast(format!("Laser export failed: {e}"), true),
        }
    }
}

fn laser_op_label(op: LaserOperationKind) -> &'static str {
    match op {
        LaserOperationKind::Cut => "Cut",
        LaserOperationKind::VectorEngrave => "Vector Engrave",
        LaserOperationKind::HatchEngrave => "Hatch Engrave",
        LaserOperationKind::Perforate => "Perforate",
        LaserOperationKind::Ignore => "Ignore",
    }
}

// =============================================================================
// 16. Interactive Glyphs Browser (Embedded in Text Properties & Font Manager)
// =============================================================================

/// Runs a sandboxed `PluginKind::RhaiScript` plugin via real [`CommandOp`] execution so
/// `Ctrl+Z` / `Ctrl+Shift+Z`, `CommandHistory::replay_log`, and SQLite WAL journaling work
/// atomically (and 0-op or failing scripts push zero undo entries).
pub(crate) fn run_rhai_plugin_undoable(app: &mut StudioApp, plugin: &PluginManifest) {
    let planned = app.active_tab().map(|tab| {
        plan_rhai_plugin_ops(
            &tab.document,
            plugin,
            &BTreeMap::new(),
            &MacroSandboxConfig::default(),
        )
    });

    match planned {
        Some(Ok((exec, ops))) => {
            if !ops.is_empty() {
                let cmd = if ops.len() == 1 {
                    ops.into_iter().next().expect("len == 1")
                } else {
                    CommandOp::Batch(ops)
                };
                if let Err(err) = app.execute_command(cmd) {
                    app.push_toast(format!("Plugin failed: {err}"), true);
                    return;
                }
                if let Some(tab) = app.active_tab_mut() {
                    tab.sync_active_layer();
                    tab.needs_rerender = true;
                }
                app.checkpoint_wal_snapshot_active();
            }
            let summary = exec
                .logs
                .last()
                .cloned()
                .unwrap_or_else(|| format!("Plugin '{}' completed", plugin.name));
            app.push_toast(summary, false);
        }
        Some(Err(e)) => app.push_toast(format!("Plugin failed: {e}"), true),
        None => app.push_toast("No active document for plugin", true),
    }
}
