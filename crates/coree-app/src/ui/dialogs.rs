// SPDX-License-Identifier: MPL-2.0

//! Studio modal dialogs: `NewDocumentDialog`, `ExportDialog`, `PrintPreviewDialog`,
//! `PreferencesDialog`, and `AboutDialog` (powered by `coree_core::brand::BrandInfo`).

use crate::shortcuts::ShortcutProfile;
use crate::state::StudioApp;
use crate::theme::StudioTheme;
use coree_cdr::CdrWriteVersion;
use coree_core::brand::BrandInfo;
use coree_core::{ColorMode, Document, Unit};
use egui::{ComboBox, DragValue, RichText};
use std::path::PathBuf;

const NEW_DOC_PRESETS: [(&str, f64, f64, ColorMode, f32, f64); 7] = [
    (
        "A4 Print (210 × 297 mm)",
        210.0,
        297.0,
        ColorMode::Cmyk,
        300.0,
        3.0,
    ),
    (
        "A3 Poster (297 × 420 mm)",
        297.0,
        420.0,
        ColorMode::Cmyk,
        300.0,
        3.0,
    ),
    (
        "US Letter (215.9 × 279.4 mm)",
        215.9,
        279.4,
        ColorMode::Cmyk,
        300.0,
        3.0,
    ),
    (
        "Business Card (85 × 55 mm)",
        85.0,
        55.0,
        ColorMode::Cmyk,
        300.0,
        2.0,
    ),
    (
        "Packaging Sheet (320 × 450 mm)",
        320.0,
        450.0,
        ColorMode::Cmyk,
        300.0,
        5.0,
    ),
    (
        "Square Sticker (100 × 100 mm)",
        100.0,
        100.0,
        ColorMode::Cmyk,
        300.0,
        2.0,
    ),
    (
        "Web HD 1920×1080 (RGB)",
        508.0,
        285.75,
        ColorMode::Rgb,
        96.0,
        0.0,
    ),
];

const EXPORT_FORMATS: [(&str, &str); 14] = [
    ("PDF Print-Ready (.pdf)", "pdf"),
    ("Native Lossless (.coree)", "coree"),
    ("CDR X6+ Archive (.cdr)", "cdr"),
    ("CDR X4 ZIP (.cdr)", "cdr"),
    ("CDR Classic RIFF v12 (.cdr)", "cdr"),
    ("CDT Template (.cdt)", "cdt"),
    ("CMX Exchange (.cmx)", "cmx"),
    ("SVG 1.1 Vector (.svg)", "svg"),
    ("EPS PostScript (.eps)", "eps"),
    ("AutoCAD DXF (.dxf)", "dxf"),
    ("HPGL Plotter (.plt)", "plt"),
    ("PNG Raster (.png)", "png"),
    ("JPEG Raster (.jpg)", "jpg"),
    ("WebP Raster (.webp)", "webp"),
];

/// Renders all open modal studio dialogs.
pub fn show_dialogs(ctx: &egui::Context, app: &mut StudioApp) {
    show_new_document_dialog(ctx, app);
    show_export_dialog(ctx, app);
    show_print_preview_dialog(ctx, app);
    show_preferences_dialog(ctx, app);
    show_about_dialog(ctx, app);
}

fn show_new_document_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.new_doc_dialog.open {
        return;
    }
    let mut open = app.new_doc_dialog.open;
    let mut create_now = false;

    egui::Window::new("New Document")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::vec2(430.0, 320.0))
        .show(ctx, |ui| {
            let d = &mut app.new_doc_dialog;
            ui.horizontal(|ui| {
                ui.label("Preset:");
                let prev = d.preset_index;
                ComboBox::from_id_salt("new_doc_preset")
                    .width(280.0)
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

            ui.horizontal(|ui| {
                ui.label("Title:");
                ui.text_edit_singleline(&mut d.title);
            });
            ui.horizontal(|ui| {
                ui.label("Width:");
                ui.add(
                    DragValue::new(&mut d.width_mm)
                        .range(5.0..=45000.0)
                        .suffix(" mm"),
                );
                ui.label("Height:");
                ui.add(
                    DragValue::new(&mut d.height_mm)
                        .range(5.0..=45000.0)
                        .suffix(" mm"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Unit:");
                ComboBox::from_id_salt("new_doc_unit")
                    .selected_text(d.unit.suffix())
                    .show_ui(ui, |ui| {
                        for u in [
                            Unit::Millimeter,
                            Unit::Centimeter,
                            Unit::Inch,
                            Unit::Point,
                            Unit::Pixel,
                        ] {
                            ui.selectable_value(&mut d.unit, u, u.suffix());
                        }
                    });
                ui.label("Color Mode:");
                ui.selectable_value(&mut d.color_mode, ColorMode::Cmyk, "CMYK");
                ui.selectable_value(&mut d.color_mode, ColorMode::Rgb, "RGB");
            });
            ui.horizontal(|ui| {
                ui.label("Resolution:");
                ui.add(
                    DragValue::new(&mut d.dpi)
                        .range(72.0..=1200.0)
                        .suffix(" DPI"),
                );
                ui.label("Bleed:");
                ui.add(
                    DragValue::new(&mut d.bleed_mm)
                        .range(0.0..=50.0)
                        .suffix(" mm"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Pages:");
                ui.add(DragValue::new(&mut d.pages).range(1..=200));
            });

            ui.add_space(12.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    d.open = false;
                }
                if ui
                    .button(RichText::new("Create Document").strong())
                    .clicked()
                {
                    create_now = true;
                }
            });
        });

    if create_now {
        let d = app.new_doc_dialog.clone();
        let mut doc = Document::new(&d.title, d.width_mm, d.height_mm, d.color_mode, d.dpi);
        doc.default_unit = d.unit;
        if let Some(p0) = doc.pages.first_mut() {
            p0.bleed_mm = d.bleed_mm;
        }
        for i in 2..=d.pages {
            doc.add_page(format!("Page {i}"), d.width_mm, d.height_mm, d.bleed_mm);
        }
        app.new_document(doc);
        app.new_doc_dialog.open = false;
    } else {
        app.new_doc_dialog.open = open && app.new_doc_dialog.open;
    }
}

fn show_export_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.export_dialog.open {
        return;
    }
    let mut open = app.export_dialog.open;
    let mut trigger_export = false;
    let mut browse_clicked = false;

    egui::Window::new("Export Production Document")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::vec2(480.0, 360.0))
        .show(ctx, |ui| {
            let ex = &mut app.export_dialog;
            let prev = ex.format_index;
            ui.horizontal(|ui| {
                ui.label("Format:");
                ComboBox::from_id_salt("export_format_combo")
                    .width(300.0)
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

            ui.horizontal(|ui| {
                ui.label("Output:");
                ui.text_edit_singleline(&mut ex.output_path);
                if ui.button("Browse...").clicked() {
                    browse_clicked = true;
                }
            });

            ui.separator();
            ui.label(RichText::new("Format Options").strong());
            match ex.format_index {
                0 => {
                    // PDF
                    ui.checkbox(
                        &mut ex.options.force_cmyk,
                        "Force DeviceCMYK (Press Output)",
                    );
                    ui.checkbox(&mut ex.options.include_bleed, "Include BleedBox");
                    ui.checkbox(&mut ex.options.crop_marks, "Crop Marks");
                    ui.checkbox(&mut ex.options.registration_marks, "Registration Targets");
                    ui.checkbox(
                        &mut ex.options.color_calibration_bars,
                        "CMYK Color Calibration Bars",
                    );
                    ui.checkbox(&mut ex.options.page_info_slug, "Page Info Slug");
                }
                9 | 10 => {
                    // DXF / PLT
                    ui.checkbox(
                        &mut ex.options.cut_layers_only,
                        "Export CutContour / Cut Layers Only",
                    );
                }
                11..=13 => {
                    // Raster (PNG / JPEG / WebP)
                    ui.horizontal(|ui| {
                        ui.label("Resolution:");
                        ui.add(
                            DragValue::new(&mut ex.options.dpi)
                                .range(72.0..=1200.0)
                                .suffix(" DPI"),
                        );
                    });
                    if ex.format_index == 12 {
                        ui.horizontal(|ui| {
                            ui.label("JPEG Quality:");
                            ui.add(DragValue::new(&mut ex.options.jpeg_quality).range(10..=100));
                        });
                    }
                }
                _ => {
                    ui.label("Lossless offline export with atomic writes & safety checks.");
                }
            }

            ui.add_space(12.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    ex.open = false;
                }
                if ui
                    .button(RichText::new("Export Now (Background)").strong())
                    .clicked()
                {
                    trigger_export = true;
                }
            });
        });

    if browse_clicked {
        let ext = EXPORT_FORMATS[app.export_dialog.format_index].1;
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Choose Export Destination")
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
            app.push_toast("Please specify an output file path before exporting", true);
            app.export_dialog.open = open;
        } else {
            let path = PathBuf::from(trimmed);
            let mut opts = app.export_dialog.options.clone();
            opts.page_index = app.active_tab().map_or(0, |t| t.document.active_page_index);
            app.export_dialog.open = false;
            app.export_active_async(path, opts);
        }
    } else {
        app.export_dialog.open = open && app.export_dialog.open;
    }
}

fn show_print_preview_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.show_print_preview_dialog {
        return;
    }
    let mut open = app.show_print_preview_dialog;
    egui::Window::new("Print Preview, Imposition & Separations")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(520.0, 340.0))
        .show(ctx, |ui| {
            ui.label(RichText::new("Prepress Sheet & Separation Studio").strong());
            ui.horizontal(|ui| {
                ui.selectable_value(&mut app.production_state.imposition_kind, 0, "N-Up Grid");
                ui.selectable_value(
                    &mut app.production_state.imposition_kind,
                    1,
                    "Booklet Saddle-Stitch",
                );
                ui.selectable_value(
                    &mut app.production_state.imposition_kind,
                    2,
                    "Large-Format Tiling",
                );
            });
            ui.horizontal(|ui| {
                ui.label("Sheet W:");
                ui.add(
                    DragValue::new(&mut app.production_state.sheet_w_mm)
                        .range(50.0..=5000.0)
                        .suffix(" mm"),
                );
                ui.label("H:");
                ui.add(
                    DragValue::new(&mut app.production_state.sheet_h_mm)
                        .range(50.0..=5000.0)
                        .suffix(" mm"),
                );
            });
            ui.horizontal(|ui| {
                if ui.button("Generate Imposed Press Sheets").clicked() {
                    app.run_imposition_active();
                }
                if ui.button("Add Crop & Registration Marks").clicked() {
                    app.add_print_marks_active();
                }
                if ui.button("Compute Separations").clicked() {
                    app.compute_separations_active();
                }
            });
            if let Some(sep) = app
                .active_tab()
                .and_then(|t| t.separations_preview.as_ref())
            {
                ui.separator();
                ui.label(format!(
                    "Separations ({} plates) · Max TAC: {:.1}% · Over 300%: {:.2}%",
                    sep.plates.len(),
                    sep.max_tac_pct,
                    sep.over_tac_300_pct
                ));
                for plate in &sep.plates {
                    ui.label(format!(
                        "• {} — Mean {:.1}%, Peak {:.1}%, Inked {:.1}%",
                        plate.name,
                        plate.average_coverage_pct,
                        plate.max_coverage_pct,
                        plate.inked_area_pct
                    ));
                }
            }
        });
    app.show_print_preview_dialog = open;
}

fn show_preferences_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.show_preferences_dialog {
        return;
    }
    let mut open = app.show_preferences_dialog;
    egui::Window::new("Preferences & Workspace")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::vec2(420.0, 260.0))
        .show(ctx, |ui| {
            ui.label(RichText::new("Visual Theme").strong());
            ui.horizontal(|ui| {
                for theme in StudioTheme::ALL {
                    if ui
                        .selectable_label(app.theme == theme, theme.label())
                        .clicked()
                    {
                        app.set_theme(theme);
                    }
                }
            });
            ui.separator();
            ui.label(RichText::new("Keyboard Shortcut Profile").strong());
            ui.horizontal(|ui| {
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
            ui.separator();
            ui.checkbox(&mut app.toolbox_two_column, "Two-Column Toolbox Rail");
            ui.label("Storage: 100% Offline SQLite WAL + Atomic Crash Recovery");
        });
    app.show_preferences_dialog = open;
}

fn show_about_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.show_about_dialog {
        return;
    }
    let mut open = app.show_about_dialog;
    let brand = BrandInfo::current();
    egui::Window::new(format!("About {}", brand.display_name))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::vec2(480.0, 280.0))
        .show(ctx, |ui| {
            ui.label(
                RichText::new(format!("{} v{}", brand.display_name, brand.version))
                    .size(20.0)
                    .strong(),
            );
            ui.label(format!(
                "License: {}  |  Repository: {}",
                brand.license, brand.repo_url
            ));
            ui.label("Pure-Rust (#![forbid(unsafe_code)]) · 100% Offline · Zero Telemetry");
            ui.separator();
            ui.label(RichText::new("Clean-Room Independence Notice").strong());
            ui.label(brand.trademark_notice);
        });
    app.show_about_dialog = open;
}
