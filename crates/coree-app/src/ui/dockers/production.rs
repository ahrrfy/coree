// SPDX-License-Identifier: MIT OR Apache-2.0

use super::inspector_laser::ui_laser_plugins_studio;
use super::*;

pub(super) fn ui_production_docker(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        ui.selectable_value(
            &mut app.production_state.sub_section,
            5,
            RichText::new(format!(
                "⚡ {}",
                crate::i18n::I18nKey::ToolsPlugins.tr_ui(lang)
            ))
            .color(tokens.accent_secondary),
        );
        ui.selectable_value(
            &mut app.production_state.sub_section,
            0,
            crate::i18n::I18nKey::ProdPreflightTab.tr_ui(lang),
        );
        ui.selectable_value(
            &mut app.production_state.sub_section,
            1,
            crate::i18n::I18nKey::ProdSeparationsTab.tr_ui(lang),
        );
        ui.selectable_value(
            &mut app.production_state.sub_section,
            2,
            crate::i18n::I18nKey::ProdCutDielinesTab.tr_ui(lang),
        );
        ui.selectable_value(
            &mut app.production_state.sub_section,
            3,
            crate::i18n::I18nKey::ProdBarcodeTab.tr_ui(lang),
        );
        ui.selectable_value(
            &mut app.production_state.sub_section,
            4,
            crate::i18n::I18nKey::ProdMergeCollectTab.tr_ui(lang),
        );
    });
    ui.separator();

    match app.production_state.sub_section {
        5 => {
            ui_laser_plugins_studio(ui, app);
        }
        0 => {
            // Preflight Inspector (`S13`): Rule checklist + Severity filter tabs + Selection change hook (`#20e`)
            shell_dir.with_horizontal(ui, |ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.preflight_min_dpi)
                        .range(72.0..=600.0)
                        .prefix(crate::i18n::I18nKey::DockProdMinDpi.tr_ui(lang)),
                );
                if ui
                    .button(crate::i18n::I18nKey::PreflightRunAudit.tr_ui(lang))
                    .clicked()
                {
                    app.run_preflight_active();
                }
            });
            egui::CollapsingHeader::new(crate::i18n::I18nKey::DockProdRulesHdr.tr_ui(lang))
                .default_open(false)
                .show(ui, |ui| {
                    let r = &mut app.production_state.preflight_rules;
                    ui.checkbox(
                        &mut r.check_rgb_in_cmyk,
                        crate::i18n::I18nKey::PreflightRuleRgbInCmyk.tr_ui(lang),
                    );
                    ui.checkbox(
                        &mut r.warn_live_text,
                        crate::i18n::I18nKey::PreflightRuleLiveText.tr_ui(lang),
                    );
                    ui.checkbox(
                        &mut r.check_missing_fonts,
                        crate::i18n::I18nKey::PreflightRuleMissingFont.tr_ui(lang),
                    );
                    ui.checkbox(
                        &mut r.check_bleed,
                        crate::i18n::I18nKey::PreflightRuleBleed.tr_ui(lang),
                    );
                    ui.checkbox(
                        &mut r.check_layer_states,
                        crate::i18n::I18nKey::PreflightRuleLayerStates.tr_ui(lang),
                    );
                    ui.add(
                        DragValue::new(&mut r.min_outline_width_mm)
                            .range(0.01..=5.0)
                            .speed(0.02)
                            .prefix(format!(
                                "{}: ",
                                crate::i18n::I18nKey::PreflightMinOutline.tr_ui(lang)
                            ))
                            .suffix(" mm"),
                    );
                    ui.add(
                        DragValue::new(&mut r.max_tac_pct)
                            .range(180.0..=400.0)
                            .speed(5.0)
                            .prefix(format!(
                                "{}: ",
                                crate::i18n::I18nKey::PreflightMaxTac.tr_ui(lang)
                            ))
                            .suffix(" %"),
                    );
                });
            let report = app.active_tab().and_then(|t| t.preflight_report.clone());
            if let Some(rep) = report {
                shell_dir.with_horizontal_wrapped(ui, |ui| {
                    use crate::state::PreflightSeverityFilter;
                    ui.selectable_value(
                        &mut app.production_state.preflight_severity,
                        PreflightSeverityFilter::All,
                        format!(
                            "{} ({})",
                            crate::i18n::I18nKey::PreflightAllIssues.tr_ui(lang),
                            rep.issues.len()
                        ),
                    );
                    ui.selectable_value(
                        &mut app.production_state.preflight_severity,
                        PreflightSeverityFilter::Errors,
                        format!(
                            "{} ({})",
                            crate::i18n::I18nKey::PreflightErrorsOnly.tr_ui(lang),
                            rep.error_count()
                        ),
                    );
                    ui.selectable_value(
                        &mut app.production_state.preflight_severity,
                        PreflightSeverityFilter::Warnings,
                        format!(
                            "{} ({})",
                            crate::i18n::I18nKey::PreflightWarningsOnly.tr_ui(lang),
                            rep.warning_count()
                        ),
                    );
                    ui.selectable_value(
                        &mut app.production_state.preflight_severity,
                        PreflightSeverityFilter::Info,
                        format!(
                            "{} ({})",
                            crate::i18n::I18nKey::PreflightInfoOnly.tr_ui(lang),
                            rep.info_count()
                        ),
                    );
                });
                let sev_filter = app.production_state.preflight_severity;
                let mut select_obj: Option<ObjectId> = None;
                for issue in &rep.issues {
                    use crate::state::PreflightSeverityFilter;
                    let show = match sev_filter {
                        PreflightSeverityFilter::All => true,
                        PreflightSeverityFilter::Errors => {
                            issue.severity == PreflightSeverity::Error
                        }
                        PreflightSeverityFilter::Warnings => {
                            issue.severity == PreflightSeverity::Warning
                        }
                        PreflightSeverityFilter::Info => issue.severity == PreflightSeverity::Info,
                    };
                    if !show {
                        continue;
                    }
                    let col = match issue.severity {
                        PreflightSeverity::Error => tokens.danger,
                        PreflightSeverity::Warning => tokens.warning,
                        PreflightSeverity::Info => tokens.accent,
                    };
                    shell_dir.with_horizontal_wrapped(ui, |ui| {
                        let resp = ui.add(
                            egui::Button::new(
                                RichText::new(format!("[{:?}] {}", issue.severity, issue.message))
                                    .size(11.0)
                                    .color(col),
                            )
                            .frame(false),
                        );
                        if resp.on_hover_text(&issue.suggestion).clicked() {
                            select_obj = issue.object_id;
                        }
                    });
                }
                if let Some(oid) = select_obj {
                    let before_sel = app
                        .active_tab()
                        .map(|t| t.selection.objects.clone())
                        .unwrap_or_default();
                    if let Some(tab) = app.active_tab_mut() {
                        tab.selection.select_single(&tab.document, oid);
                        tab.metrics_dirty = true;
                        tab.needs_rerender = true;
                    }
                    app.maybe_fire_selection_change(&before_sel);
                }
            }
        }
        1 => {
            // Separations, Overprint & Imposition
            shell_dir.with_horizontal(ui, |ui| {
                if ui
                    .button(crate::i18n::I18nKey::DlgPrintComputeSep.tr_ui(lang))
                    .clicked()
                {
                    app.compute_separations_active();
                }
                if ui
                    .button(crate::i18n::I18nKey::DlgPrintExportPlates.tr_ui(lang))
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("PDF", &["pdf"])
                        .set_file_name("separations_plates.pdf")
                        .save_file()
                    {
                        app.export_separated_plates_pdf_active(&path);
                    }
                }
                if let Some(tab) = app.active_tab_mut() {
                    if ui
                        .checkbox(&mut tab.overprint_preview, "Overprint")
                        .changed()
                    {
                        tab.needs_rerender = true;
                    }
                }
            });
            shell_dir.with_horizontal(ui, |ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.trap_width_mm)
                        .range(0.01..=3.0)
                        .speed(0.01)
                        .prefix("Trap: ")
                        .suffix(" mm"),
                );
                ComboBox::from_id_salt("trap_place_combo")
                    .width(96.0)
                    .selected_text(format!("{:?}", app.production_state.trap_placement))
                    .show_ui(ui, |ui| {
                        for p in [
                            TrapPlacement::Centerline,
                            TrapPlacement::Spread,
                            TrapPlacement::Choke,
                        ] {
                            ui.selectable_value(
                                &mut app.production_state.trap_placement,
                                p,
                                format!("{p:?}"),
                            );
                        }
                    });
                if ui
                    .button(crate::i18n::I18nKey::DlgPrintApplyTrap.tr_ui(lang))
                    .clicked()
                {
                    app.apply_trapping_active();
                }
            });
            shell_dir.with_horizontal(ui, |ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.halftone_lpi)
                        .range(10.0..=300.0)
                        .prefix("LPI: "),
                );
                ComboBox::from_id_salt("halftone_shape_combo")
                    .width(92.0)
                    .selected_text(format!("{:?}", app.production_state.halftone_shape))
                    .show_ui(ui, |ui| {
                        for sh in [
                            HalftoneDotShape::Round,
                            HalftoneDotShape::Elliptical,
                            HalftoneDotShape::Diamond,
                            HalftoneDotShape::Square,
                            HalftoneDotShape::Line,
                        ] {
                            ui.selectable_value(
                                &mut app.production_state.halftone_shape,
                                sh,
                                format!("{sh:?}"),
                            );
                        }
                    });
                if ui
                    .button(crate::i18n::I18nKey::DockProdHalftoneScreen.tr_ui(lang))
                    .clicked()
                {
                    app.apply_halftone_preview_active();
                }
            });
            if let Some(sep) = app
                .active_tab()
                .and_then(|t| t.separations_preview.as_ref())
            {
                ui.label(format!("Max TAC: {:.1}%", sep.max_tac_pct));
                for p in &sep.plates {
                    ui.label(format!(
                        "• {} ({:.1}% avg, {:.1}% peak)",
                        p.name, p.average_coverage_pct, p.max_coverage_pct
                    ));
                }
            }
            ui.separator();
            ui.label(RichText::new(crate::i18n::I18nKey::PrintImpositionMode.tr_ui(lang)).strong());
            shell_dir.with_horizontal_wrapped(ui, |ui| {
                ui.selectable_value(&mut app.production_state.imposition_kind, 0, "N-Up");
                ui.selectable_value(&mut app.production_state.imposition_kind, 1, "Booklet");
                ui.selectable_value(&mut app.production_state.imposition_kind, 2, "Tiling");
                ui.selectable_value(
                    &mut app.production_state.imposition_kind,
                    3,
                    crate::i18n::I18nKey::DockProdPerfectBound.tr_ui(lang),
                );
            });
            if app.production_state.imposition_kind == 3 {
                shell_dir.with_horizontal(ui, |ui| {
                    ui.add(
                        DragValue::new(&mut app.production_state.imposition_signature_pages)
                            .range(4..=64)
                            .prefix("Sig: ")
                            .suffix(" pp"),
                    );
                    ui.add(
                        DragValue::new(&mut app.production_state.spine_thickness_mm)
                            .range(0.0..=80.0)
                            .speed(0.2)
                            .prefix("Spine: ")
                            .suffix(" mm"),
                    );
                });
                shell_dir.with_horizontal(ui, |ui| {
                    ui.add(
                        DragValue::new(&mut app.production_state.creep_mm)
                            .range(0.0..=10.0)
                            .speed(0.1)
                            .prefix("Creep: ")
                            .suffix(" mm"),
                    );
                    ui.add(
                        DragValue::new(&mut app.production_state.milling_allowance_mm)
                            .range(0.0..=10.0)
                            .speed(0.1)
                            .prefix("Mill: ")
                            .suffix(" mm"),
                    );
                });
            }
            shell_dir.with_horizontal(ui, |ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.nup_cols)
                        .range(1..=12)
                        .prefix("Cols: "),
                );
                ui.add(
                    DragValue::new(&mut app.production_state.nup_rows)
                        .range(1..=12)
                        .prefix("Rows: "),
                );
            });
            shell_dir.with_horizontal(ui, |ui| {
                if ui
                    .button(crate::i18n::I18nKey::DockProdImposeNewTab.tr_ui(lang))
                    .clicked()
                {
                    app.run_imposition_active();
                }
                if ui
                    .button(crate::i18n::I18nKey::DockProdAddPrintMarks.tr_ui(lang))
                    .clicked()
                {
                    app.add_print_marks_active();
                }
            });
        }
        2 => {
            // CutContour + Parametric Packaging Dielines + Live Metrics
            ui.label(
                RichText::new(crate::i18n::I18nKey::DockProdOneClickCut.tr_ui(lang))
                    .strong()
                    .color(tokens.cutcontour),
            );
            shell_dir.with_horizontal(ui, |ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.cut_offset_mm)
                        .range(-10.0..=50.0)
                        .speed(0.2)
                        .prefix("Offset: ")
                        .suffix(" mm"),
                );
                ui.text_edit_singleline(&mut app.production_state.cut_spot_name);
            });
            if ui
                .button(crate::i18n::I18nKey::DockProdGenCutContour.tr_ui(lang))
                .clicked()
            {
                app.generate_cut_contour_active();
            }

            ui.separator();
            ui.label(RichText::new(crate::i18n::I18nKey::DockProdDielinesHdr.tr_ui(lang)).strong());
            shell_dir.with_horizontal_wrapped(ui, |ui| {
                ui.selectable_value(
                    &mut app.production_state.dieline_type,
                    0,
                    crate::i18n::I18nKey::DockProdBox.tr_ui(lang),
                );
                ui.selectable_value(
                    &mut app.production_state.dieline_type,
                    1,
                    crate::i18n::I18nKey::DockProdEnvelope.tr_ui(lang),
                );
                ui.selectable_value(
                    &mut app.production_state.dieline_type,
                    2,
                    crate::i18n::I18nKey::DockProdBag.tr_ui(lang),
                );
                ui.selectable_value(
                    &mut app.production_state.dieline_type,
                    3,
                    crate::i18n::I18nKey::DockProdStickers.tr_ui(lang),
                );
                ui.selectable_value(&mut app.production_state.dieline_type, 4, "FEFCO");
            });
            if app.production_state.dieline_type == 4 {
                shell_dir.with_horizontal(ui, |ui| {
                    ComboBox::from_id_salt("fefco_code_combo")
                        .width(120.0)
                        .selected_text(format!("{:?}", app.production_state.fefco_code))
                        .show_ui(ui, |ui| {
                            for code in FefcoCode::ALL {
                                ui.selectable_value(
                                    &mut app.production_state.fefco_code,
                                    code,
                                    format!("{code:?}"),
                                );
                            }
                        });
                    ComboBox::from_id_salt("flute_type_combo")
                        .width(80.0)
                        .selected_text(format!("{:?}", app.production_state.flute_type))
                        .show_ui(ui, |ui| {
                            for f in [
                                FluteType::None,
                                FluteType::E,
                                FluteType::B,
                                FluteType::C,
                                FluteType::Bc,
                            ] {
                                ui.selectable_value(
                                    &mut app.production_state.flute_type,
                                    f,
                                    format!("{f:?}"),
                                );
                            }
                        });
                });
            }
            shell_dir.with_horizontal(ui, |ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.box_l_mm)
                        .range(15.0..=1000.0)
                        .prefix("L: "),
                );
                ui.add(
                    DragValue::new(&mut app.production_state.box_w_mm)
                        .range(10.0..=1000.0)
                        .prefix("W: "),
                );
                ui.add(
                    DragValue::new(&mut app.production_state.box_h_mm)
                        .range(15.0..=1000.0)
                        .prefix("H: "),
                );
            });
            if ui
                .button(crate::i18n::I18nKey::DockProdGenDieline.tr_ui(lang))
                .clicked()
            {
                app.generate_dieline_active();
            }

            if let Some(tab) = app.active_tab_mut() {
                tab.ensure_metrics_fresh();
                let m = &tab.last_metrics;
                ui.separator();
                ui.label(
                    RichText::new(crate::i18n::I18nKey::DockProdLiveMeter.tr_ui(lang)).strong(),
                );
                ui.label(format!(
                    "Cut Length: {:.1} mm ({:.2} m)",
                    m.cut_length_mm, m.cut_length_m
                ));
                ui.label(format!("Crease Length: {:.1} mm", m.crease_length_mm));
                ui.label(format!("Filled Area: {:.1} cm²", m.filled_area_mm2 / 100.0));
                ui.label(format!(
                    "Est. Laser: {:.1}s · Vinyl: {:.1}s",
                    m.estimated_laser_time_sec, m.estimated_vinyl_time_sec
                ));
            }
        }
        3 => {
            // Vector 1D / 2D Barcode & QR Generator
            ui.label(RichText::new(crate::i18n::I18nKey::DockProdBarcodeHdr.tr_ui(lang)).strong());
            ComboBox::from_id_salt("barcode_sym_combo")
                .selected_text(format!("{:?}", app.production_state.barcode_symbology))
                .show_ui(ui, |ui| {
                    for s in [
                        BarcodeSymbology::Ean13,
                        BarcodeSymbology::Ean8,
                        BarcodeSymbology::UpcA,
                        BarcodeSymbology::Code128,
                        BarcodeSymbology::Code39,
                        BarcodeSymbology::Gs1128,
                        BarcodeSymbology::DataMatrix,
                        BarcodeSymbology::Pdf417,
                        BarcodeSymbology::Aztec,
                    ] {
                        ui.selectable_value(
                            &mut app.production_state.barcode_symbology,
                            s,
                            format!("{s:?}"),
                        );
                    }
                });
            shell_dir.with_horizontal(ui, |ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.barcode_bwr_mm)
                        .range(0.0..=0.5)
                        .speed(0.005)
                        .prefix("BWR: ")
                        .suffix(" mm"),
                );
                if matches!(
                    app.production_state.barcode_symbology,
                    BarcodeSymbology::Code128 | BarcodeSymbology::Gs1128
                ) {
                    ComboBox::from_id_salt("code128_mode_combo")
                        .width(88.0)
                        .selected_text(format!("{:?}", app.production_state.code128_mode))
                        .show_ui(ui, |ui| {
                            for m in [
                                Code128Mode::Auto,
                                Code128Mode::SubsetA,
                                Code128Mode::SubsetB,
                                Code128Mode::SubsetC,
                            ] {
                                ui.selectable_value(
                                    &mut app.production_state.code128_mode,
                                    m,
                                    format!("{m:?}"),
                                );
                            }
                        });
                }
            });
            ui.text_edit_singleline(&mut app.production_state.barcode_payload);
            if ui
                .button(crate::i18n::I18nKey::DockProdInsertBarcode.tr_ui(lang))
                .clicked()
            {
                app.insert_barcode_active();
            }

            ui.separator();
            ui.label(RichText::new(crate::i18n::I18nKey::DockProdQrHdr.tr_ui(lang)).strong());
            ui.text_edit_singleline(&mut app.production_state.qr_payload);
            shell_dir.with_horizontal(ui, |ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.qr_size_mm)
                        .range(8.0..=300.0)
                        .prefix("Size: ")
                        .suffix(" mm"),
                );
                ComboBox::from_id_salt("qr_ec_combo")
                    .width(56.0)
                    .selected_text(format!("{:?}", app.production_state.qr_ec))
                    .show_ui(ui, |ui| {
                        for ec in [
                            QrErrorCorrection::L,
                            QrErrorCorrection::M,
                            QrErrorCorrection::Q,
                            QrErrorCorrection::H,
                        ] {
                            ui.selectable_value(
                                &mut app.production_state.qr_ec,
                                ec,
                                format!("{ec:?}"),
                            );
                        }
                    });
            });
            if ui
                .button(crate::i18n::I18nKey::DockProdInsertQr.tr_ui(lang))
                .clicked()
            {
                app.insert_qr_active();
            }
        }
        _ => {
            // Print Merge (CSV) & Collect for Output
            ui.label(
                RichText::new(crate::i18n::I18nKey::DockProdPrintMergeHdr.tr_ui(lang)).strong(),
            );
            ui.add(
                egui::TextEdit::multiline(&mut app.production_state.merge_csv_text)
                    .desired_rows(4)
                    .code_editor(),
            );
            shell_dir.with_horizontal(ui, |ui| {
                if ui
                    .button(crate::i18n::I18nKey::DockProdLoadCsv.tr_ui(lang))
                    .clicked()
                {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("CSV/TSV", &["csv", "tsv", "txt"])
                        .pick_file()
                    {
                        match read_text_file_bounded(&p, MAX_TEXT_READ_BYTES) {
                            Ok(txt) => app.production_state.merge_csv_text = txt,
                            Err(err) => app.push_toast(err, true),
                        }
                    }
                }
                if ui
                    .button(crate::i18n::I18nKey::DockProdRunMerge.tr_ui(lang))
                    .clicked()
                {
                    app.run_print_merge_active();
                }
            });

            ui.separator();
            ui.label(RichText::new(crate::i18n::I18nKey::DockProdCollectHdr.tr_ui(lang)).strong());
            ui.text_edit_singleline(&mut app.production_state.collect_dir_text);
            shell_dir.with_horizontal(ui, |ui| {
                if ui
                    .button(crate::i18n::I18nKey::DockProdPickFolder.tr_ui(lang))
                    .clicked()
                {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        app.production_state.collect_dir_text = dir.to_string_lossy().into_owned();
                    }
                }
                if ui
                    .button(crate::i18n::I18nKey::CollectRunBundle.tr_ui(lang))
                    .clicked()
                {
                    let dir = PathBuf::from(app.production_state.collect_dir_text.trim());
                    app.run_collect_for_output(&dir);
                }
            });
            if let Some(tab) = app.active_tab() {
                let doc = &tab.document;
                let mut fonts: Vec<String> = Vec::new();
                for obj in doc.objects().values() {
                    let runs: Option<&[coree_core::TextRun]> = match &obj.payload {
                        NodePayload::ArtisticText(t) => Some(&t.runs),
                        NodePayload::ParagraphText(p) => Some(&p.runs),
                        NodePayload::TextOnPath(t) => Some(&t.text.runs),
                        _ => None,
                    };
                    if let Some(rlist) = runs {
                        for r in rlist {
                            let fam = r.style.font_family.trim();
                            if !fam.is_empty() && !fonts.iter().any(|f| f == fam) {
                                fonts.push(fam.to_owned());
                            }
                        }
                    }
                }
                let mut spots: Vec<String> = Vec::new();
                for obj in doc.objects().values() {
                    for c in obj.fill.referenced_colors() {
                        if let Color::Spot { name, .. } = c {
                            if !spots.contains(&name) {
                                spots.push(name);
                            }
                        }
                    }
                    if obj.outline.is_visible() {
                        if let Color::Spot { ref name, .. } = obj.outline.color {
                            if !spots.contains(name) {
                                spots.push(name.clone());
                            }
                        }
                    }
                }
                egui::CollapsingHeader::new(
                    crate::i18n::I18nKey::CollectManifestPreview.tr_ui(lang),
                )
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "Pages: {} · {}: {} · {}: {} · {}: {}",
                            doc.pages().len(),
                            crate::i18n::I18nKey::CollectExtractedImages.tr_ui(lang),
                            doc.assets().len(),
                            crate::i18n::I18nKey::CollectUsedFonts.tr_ui(lang),
                            fonts.len(),
                            crate::i18n::I18nKey::CollectSpotInks.tr_ui(lang),
                            spots.len()
                        ))
                        .size(10.5),
                    );
                    if !fonts.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "{}: {}",
                                crate::i18n::I18nKey::CollectRestrictedFonts.tr_ui(lang),
                                fonts.join(", ")
                            ))
                            .size(10.0)
                            .color(tokens.text_muted),
                        );
                    }
                    if !spots.is_empty() {
                        ui.label(
                            RichText::new(format!("Spot Plates: {}", spots.join(", ")))
                                .size(10.0)
                                .color(tokens.cutcontour),
                        );
                    }
                });
            }

            ui.separator();
            ui.label(
                RichText::new(crate::i18n::I18nKey::DockProdLaserStreamHdr.tr_ui(lang)).strong(),
            );
            shell_dir.with_horizontal(ui, |ui| {
                ComboBox::from_id_salt("open_laser_proto_combo")
                    .width(140.0)
                    .selected_text(format!("{:?}", app.production_state.laser_protocol))
                    .show_ui(ui, |ui| {
                        for proto in [
                            OpenLaserProtocol::GrblStream,
                            OpenLaserProtocol::MarlinStream,
                            OpenLaserProtocol::RuidaRdPacket,
                            OpenLaserProtocol::LightBurnLbrn2,
                            OpenLaserProtocol::K40M2NanoEgv,
                            OpenLaserProtocol::HpglStream,
                        ] {
                            ui.selectable_value(
                                &mut app.production_state.laser_protocol,
                                proto,
                                format!("{proto:?}"),
                            );
                        }
                    });
                if ui
                    .button(crate::i18n::I18nKey::DockProdExportLaserStream.tr_ui(lang))
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_file_name("laser_job.stream")
                        .save_file()
                    {
                        app.export_laser_protocol_active(&path);
                    }
                }
            });
        }
    }
}

// =============================================================================
// 9. Find & Replace Docker
// =============================================================================
