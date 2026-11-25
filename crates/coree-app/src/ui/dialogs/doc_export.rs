// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

pub(super) fn show_new_document_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.new_doc_dialog.open {
        return;
    }
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut open = app.new_doc_dialog.open;
    let mut create_now = false;

    egui::Window::new(crate::i18n::I18nKey::NewDocument.tr_ui(lang))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::vec2(440.0, 325.0))
        .show(ctx, |ui| {
            shell_dir.with_vertical(ui, |ui| {
                let d = &mut app.new_doc_dialog;
                if d.preset_index >= NEW_DOC_PRESETS.len() {
                    d.preset_index = 0;
                }
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::DocPresetLabel.tr_ui(lang));
                    let prev = d.preset_index;
                    ComboBox::from_id_salt("new_doc_preset")
                        .width(290.0)
                        .selected_text(NEW_DOC_PRESETS[d.preset_index].0)
                        .show_ui(ui, |ui| {
                            for (idx, p) in NEW_DOC_PRESETS.iter().enumerate() {
                                ui.selectable_value(&mut d.preset_index, idx, p.0);
                            }
                        });
                    if d.preset_index != prev {
                        let p = NEW_DOC_PRESETS[d.preset_index];
                        d.width_mm = p.1;
                        d.height_mm = p.2;
                        d.color_mode = p.3;
                        d.dpi = p.4;
                        d.bleed_mm = p.5;
                    }
                });

                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::DocTitleLabel.tr_ui(lang));
                    ui.text_edit_singleline(&mut d.title);
                });
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::PropWidth.tr_ui(lang));
                    ui.add(
                        DragValue::new(&mut d.width_mm)
                            .range(5.0..=45000.0)
                            .suffix(" mm"),
                    );
                    ui.label(crate::i18n::I18nKey::PropHeight.tr_ui(lang));
                    ui.add(
                        DragValue::new(&mut d.height_mm)
                            .range(5.0..=45000.0)
                            .suffix(" mm"),
                    );
                });
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::DocUnitLabel.tr_ui(lang));
                    ComboBox::from_id_salt("new_doc_unit")
                        .selected_text(d.unit.suffix())
                        .show_ui(ui, |ui| {
                            for u in [
                                Unit::Millimeter,
                                Unit::Centimeter,
                                Unit::Meter,
                                Unit::Inch,
                                Unit::Point,
                                Unit::Pixel,
                            ] {
                                ui.selectable_value(&mut d.unit, u, u.suffix());
                            }
                        });
                    ui.label(crate::i18n::I18nKey::DocColorModeLabel.tr_ui(lang));
                    ui.selectable_value(&mut d.color_mode, ColorMode::Cmyk, "CMYK");
                    ui.selectable_value(&mut d.color_mode, ColorMode::Rgb, "RGB");
                });
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::DocResolutionLabel.tr_ui(lang));
                    ui.add(
                        DragValue::new(&mut d.dpi)
                            .range(72.0..=1200.0)
                            .suffix(" DPI"),
                    );
                    ui.label(crate::i18n::I18nKey::DocBleedLabel.tr_ui(lang));
                    ui.add(
                        DragValue::new(&mut d.bleed_mm)
                            .range(0.0..=50.0)
                            .suffix(" mm"),
                    );
                });
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::DocPagesLabel.tr_ui(lang));
                    ui.add(DragValue::new(&mut d.pages).range(1..=200));
                });

                ui.add_space(12.0);
                ui.separator();
                shell_dir.with_horizontal(ui, |ui| {
                    if ui
                        .button(crate::i18n::I18nKey::DialogCancel.tr_ui(lang))
                        .clicked()
                    {
                        d.open = false;
                    }
                    if ui
                        .button(
                            RichText::new(crate::i18n::I18nKey::DocCreateButton.tr_ui(lang))
                                .strong(),
                        )
                        .clicked()
                    {
                        create_now = true;
                    }
                });
            });
        });

    if create_now {
        let d = app.new_doc_dialog.clone();
        let mut doc = Document::new(&d.title, d.width_mm, d.height_mm, d.color_mode, d.dpi);
        doc.set_default_unit(d.unit);
        let _ = doc.set_page_bleed(0, d.bleed_mm);
        for i in 2..=d.pages {
            doc.add_page(format!("Page {i}"), d.width_mm, d.height_mm, d.bleed_mm);
        }
        app.new_document(doc);
        app.new_doc_dialog.open = false;
    } else {
        app.new_doc_dialog.open = open && app.new_doc_dialog.open;
    }
}

pub(super) fn show_export_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.export_dialog.open {
        return;
    }
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut open = app.export_dialog.open;
    let mut trigger_export = false;
    let mut browse_clicked = false;

    egui::Window::new(crate::i18n::I18nKey::ExportDialogTitle.tr_ui(lang))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::vec2(490.0, 380.0))
        .show(ctx, |ui| {
            shell_dir.with_vertical(ui, |ui| {
                let ex = &mut app.export_dialog;
                if ex.format_index >= EXPORT_FORMATS.len() {
                    ex.format_index = 0;
                }
                let prev = ex.format_index;
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::ExportFormatLabel.tr_ui(lang));
                    ComboBox::from_id_salt("export_format_combo")
                        .width(310.0)
                        .selected_text(EXPORT_FORMATS[ex.format_index].0)
                        .show_ui(ui, |ui| {
                            for (idx, f) in EXPORT_FORMATS.iter().enumerate() {
                                ui.selectable_value(&mut ex.format_index, idx, f.0);
                            }
                        });
                });
                if ex.format_index != prev {
                    let ext = EXPORT_FORMATS[ex.format_index].1;
                    let trimmed = ex.output_path.trim();
                    let mut target = if trimmed.is_empty() {
                        PathBuf::from("export")
                    } else {
                        PathBuf::from(trimmed)
                    };
                    if target.file_stem().is_none() {
                        target = target.join("export");
                    }
                    target.set_extension(ext);
                    ex.output_path = target.to_string_lossy().into_owned();
                    ex.options.cdr_version = match ex.format_index {
                        3 => CdrWriteVersion::ZipX4,
                        4 => CdrWriteVersion::RiffV12,
                        _ => CdrWriteVersion::ZipX6,
                    };
                }

                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::ExportOutputLabel.tr_ui(lang));
                    ui.text_edit_singleline(&mut ex.output_path);
                    if ui
                        .button(crate::i18n::I18nKey::ExportBrowseButton.tr_ui(lang))
                        .clicked()
                    {
                        browse_clicked = true;
                    }
                });

                ui.separator();
                ui.label(
                    RichText::new(crate::i18n::I18nKey::DlgExportFormatOptions.tr_ui(lang))
                        .strong(),
                );
                match ex.format_index {
                    0 => {
                        // PDF
                        ui.checkbox(
                            &mut ex.options.force_cmyk,
                            crate::i18n::I18nKey::DlgExportForceCmyk.tr_ui(lang),
                        );
                        ui.checkbox(
                            &mut ex.options.include_bleed,
                            crate::i18n::I18nKey::DlgExportIncludeBleed.tr_ui(lang),
                        );
                        ui.checkbox(
                            &mut ex.options.crop_marks,
                            crate::i18n::I18nKey::DlgExportCropMarks.tr_ui(lang),
                        );
                        ui.checkbox(
                            &mut ex.options.registration_marks,
                            crate::i18n::I18nKey::DlgExportRegTargets.tr_ui(lang),
                        );
                        ui.checkbox(
                            &mut ex.options.color_calibration_bars,
                            crate::i18n::I18nKey::DlgExportCalBars.tr_ui(lang),
                        );
                        ui.checkbox(
                            &mut ex.options.page_info_slug,
                            crate::i18n::I18nKey::DlgExportPageSlug.tr_ui(lang),
                        );
                    }
                    9 | 10 => {
                        // DXF / PLT
                        ui.checkbox(
                            &mut ex.options.cut_layers_only,
                            crate::i18n::I18nKey::DlgExportCutOnly.tr_ui(lang),
                        );
                    }
                    EXPORT_FMT_GCODE => {
                        // Laser G-Code (.gcode)
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.label(crate::i18n::I18nKey::DlgExportDialect.tr_ui(lang));
                            ComboBox::from_id_salt("export_gcode_dialect")
                                .width(190.0)
                                .selected_text(gcode_dialect_label(ex.options.gcode_dialect))
                                .show_ui(ui, |ui| {
                                    for d in [
                                        GcodeDialect::Grbl11,
                                        GcodeDialect::Marlin,
                                        GcodeDialect::RuidaGcode,
                                    ] {
                                        ui.selectable_value(
                                            &mut ex.options.gcode_dialect,
                                            d,
                                            gcode_dialect_label(d),
                                        );
                                    }
                                });
                        });
                        ui.checkbox(
                            &mut ex.options.cut_layers_only,
                            crate::i18n::I18nKey::DlgExportCutOnly.tr_ui(lang),
                        );
                        ui.label(crate::i18n::I18nKey::DlgExportGcodeNote.tr_ui(lang));
                    }
                    12..=16 => {
                        // Raster (PNG / JPEG / WebP / TIFF / BMP)
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.label(crate::i18n::I18nKey::DocResolutionLabel.tr_ui(lang));
                            ui.add(
                                DragValue::new(&mut ex.options.dpi)
                                    .range(72.0..=1200.0)
                                    .suffix(" DPI"),
                            );
                        });
                        if ex.format_index == 13 {
                            shell_dir.with_horizontal(ui, |ui| {
                                ui.label(crate::i18n::I18nKey::DlgExportJpegQuality.tr_ui(lang));
                                ui.add(
                                    DragValue::new(&mut ex.options.jpeg_quality).range(10..=100),
                                );
                            });
                        }
                    }
                    _ => {
                        ui.label(crate::i18n::I18nKey::DlgExportLosslessNote.tr_ui(lang));
                    }
                }

                ui.add_space(12.0);
                ui.separator();
                shell_dir.with_horizontal(ui, |ui| {
                    if ui
                        .button(crate::i18n::I18nKey::DialogCancel.tr_ui(lang))
                        .clicked()
                    {
                        ex.open = false;
                    }
                    if ui
                        .button(
                            RichText::new(crate::i18n::I18nKey::ExportRunBackground.tr_ui(lang))
                                .strong(),
                        )
                        .clicked()
                    {
                        trigger_export = true;
                    }
                });
            });
        });

    if browse_clicked {
        let ext = EXPORT_FORMATS[app.export_dialog.format_index].1;
        if let Some(p) = rfd::FileDialog::new()
            .set_title(crate::i18n::I18nKey::DlgExportDestTitle.tr_ui(lang))
            .set_file_name(&app.export_dialog.output_path)
            .add_filter("Export Format", &[ext])
            .save_file()
        {
            app.export_dialog.output_path = p.to_string_lossy().into_owned();
        }
    }

    if trigger_export {
        let trimmed = app.export_dialog.output_path.trim();
        if trimmed.is_empty() {
            app.push_toast(crate::i18n::I18nKey::DlgOptNeedExportPath.tr_ui(lang), true);
            app.export_dialog.open = open;
        } else {
            let path = PathBuf::from(trimmed);
            let mut opts = app.export_dialog.options.clone();
            opts.page_index = app
                .active_tab()
                .map_or(0, |t| t.document.active_page_index());
            app.export_dialog.open = false;
            app.export_active_async(path, opts);
        }
    } else {
        app.export_dialog.open = open && app.export_dialog.open;
    }
}

pub(crate) fn export_laser_dxf_dialog(app: &mut StudioApp) {
    let lang = app.ui_language;
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
    let plan =
        match coree_prod::plan_laser_toolpath(&tab.document, page_idx, sel, &app.laser_config) {
            Ok(p) => p,
            Err(e) => {
                app.push_toast(format!("Laser plan failed: {e}"), true);
                return;
            }
        };
    if let Some(path) = rfd::FileDialog::new()
        .set_title(crate::i18n::I18nKey::DialogExportLaserDxfTitle.tr_ui(lang))
        .set_file_name("laser_job_r12.dxf")
        .add_filter("AutoCAD R12 DXF", &["dxf"])
        .save_file()
    {
        let dxf = coree_prod::export_laser_dxf_r12(&plan);
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
