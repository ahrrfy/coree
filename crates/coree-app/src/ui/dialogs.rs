// SPDX-License-Identifier: MIT OR Apache-2.0

//! Studio modal dialogs: `NewDocumentDialog`, `ExportDialog`, `PrintPreviewDialog`,
//! classic tabbed `OptionsDialog (Ctrl+J)`, and `AboutDialog` (powered by
//! `coree_core::brand::BrandInfo`).

use crate::io_bridge::{read_text_file_bounded, MAX_TEXT_READ_BYTES};
use crate::shortcuts::{shortcut_label_for, ShortcutAction, ShortcutProfile};
use crate::state::{StudioApp, WorkspacePreset};
use crate::theme::{core_color_to_egui, StudioLanguage, StudioTheme};
use coree_cdr::CdrWriteVersion;
use coree_core::brand::BrandInfo;
use coree_core::{
    ArrowheadStyle, BuiltinIccProfile, Color, ColorMode, Document, Fill, GradientStop, LineCap,
    LineJoin, OutlineStyle, Point2D, PostScriptFillPreset, RenderingIntent, Unit,
};
use coree_prod::{
    default_plugin_dirs, detect_executable, install_sample_plugin_template, GcodeDialect,
    PluginKind, PluginManifest, PluginRegistry,
};
use egui::{Color32, ComboBox, DragValue, Frame, Id, Margin, RichText, Stroke};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static PLUGIN_DETECT_CACHE_GEN: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
struct PluginDetectCache {
    generation: u64,
    entries: BTreeMap<(String, String), Option<PathBuf>>,
}

/// Invalidates all cached plugin executable probe results (called on Reload / Install).
pub fn invalidate_plugin_detect_cache() {
    PLUGIN_DETECT_CACHE_GEN.fetch_add(1, Ordering::Relaxed);
}

/// Probes `detect_executable` at most once per `(plugin.id, trimmed_override)` and caches the
/// result inside `ctx`'s temporary data store so 60 Hz UI repaints never scan `$PATH` per frame.
pub fn cached_detect_executable(
    ctx: &egui::Context,
    plugin: &PluginManifest,
    override_raw: &str,
) -> Option<PathBuf> {
    let current_gen = PLUGIN_DETECT_CACHE_GEN.load(Ordering::Relaxed);
    let key = (plugin.id.clone(), override_raw.trim().to_owned());
    let cache_id = Id::unique("coree_plugin_detect_cache");

    let cached = ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<PluginDetectCache>(cache_id);
        if cache.generation != current_gen {
            cache.entries.clear();
            cache.generation = current_gen;
        }
        cache.entries.get(&key).cloned()
    });
    if let Some(hit) = cached {
        return hit;
    }

    let override_opt = (!key.1.is_empty()).then(|| Path::new(key.1.as_str()));
    let probed = detect_executable(plugin, override_opt);
    ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<PluginDetectCache>(cache_id);
        if cache.generation != current_gen {
            cache.entries.clear();
            cache.generation = current_gen;
        }
        cache.entries.insert(key, probed.clone());
    });
    probed
}

const NEW_DOC_PRESETS: [(&str, f64, f64, ColorMode, f32, f64); 8] = [
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
        "Laser / Vinyl Bed (300 × 200 mm)",
        300.0,
        200.0,
        ColorMode::Rgb,
        300.0,
        0.0,
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

const EXPORT_FORMATS: [(&str, &str); 17] = [
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
    ("Laser G-Code (.gcode)", "gcode"),
    ("PNG Raster (.png)", "png"),
    ("JPEG Raster (.jpg)", "jpg"),
    ("WebP Raster (.webp)", "webp"),
    ("TIFF Raster (.tiff)", "tiff"),
    ("BMP Raster (.bmp)", "bmp"),
];

/// Format index constant for Laser G-Code inside [`EXPORT_FORMATS`].
pub const EXPORT_FMT_GCODE: usize = 11;

/// Renders all open modal studio dialogs.
pub fn show_dialogs(ctx: &egui::Context, app: &mut StudioApp) {
    show_new_document_dialog(ctx, app);
    show_export_dialog(ctx, app);
    show_print_preview_dialog(ctx, app);
    show_options_dialog(ctx, app);
    show_edit_fill_dialog(ctx, app);
    show_outline_pen_dialog(ctx, app);
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
        .fixed_size(egui::vec2(440.0, 325.0))
        .show(ctx, |ui| {
            let d = &mut app.new_doc_dialog;
            if d.preset_index >= NEW_DOC_PRESETS.len() {
                d.preset_index = 0;
            }
            ui.horizontal(|ui| {
                ui.label("Preset:");
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
                            Unit::Meter,
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
        .fixed_size(egui::vec2(490.0, 380.0))
        .show(ctx, |ui| {
            let ex = &mut app.export_dialog;
            if ex.format_index >= EXPORT_FORMATS.len() {
                ex.format_index = 0;
            }
            let prev = ex.format_index;
            ui.horizontal(|ui| {
                ui.label("Format:");
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
                EXPORT_FMT_GCODE => {
                    // Laser G-Code (.gcode)
                    ui.horizontal(|ui| {
                        ui.label("Firmware Dialect:");
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
                        "Export CutContour / Cut Layers Only",
                    );
                    ui.label("Applies duplicate line removal, open curve joining & inner-loops-first ordering.");
                }
                12..=16 => {
                    // Raster (PNG / JPEG / WebP / TIFF / BMP)
                    ui.horizontal(|ui| {
                        ui.label("Resolution:");
                        ui.add(
                            DragValue::new(&mut ex.options.dpi)
                                .range(72.0..=1200.0)
                                .suffix(" DPI"),
                        );
                    });
                    if ex.format_index == 13 {
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
    let tokens = app.theme.tokens();
    let mut open = app.show_print_preview_dialog;
    let mut print_now = false;
    let mut refresh_printers = false;
    egui::Window::new("Print Studio, Native OS Spooler & Imposition (Ctrl+P)")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(560.0, 440.0))
        .show(ctx, |ui| {
            ui.label(RichText::new("Native OS Print Queue (CUPS / Windows Spooler)").strong());
            ui.horizontal(|ui| {
                ui.label("Printer:");
                let current_printer = app
                    .print_job_config
                    .printer_name
                    .clone()
                    .unwrap_or_else(|| "System Default Printer".to_owned());
                ComboBox::from_id_salt("print_os_printer_combo")
                    .width(260.0)
                    .selected_text(&current_printer)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(
                                app.print_job_config.printer_name.is_none(),
                                "System Default Printer",
                            )
                            .clicked()
                        {
                            app.print_job_config.printer_name = None;
                        }
                        for p in &app.available_printers {
                            let label = if p.is_default {
                                format!("{} (Default · {})", p.name, p.status)
                            } else {
                                format!("{} ({})", p.name, p.status)
                            };
                            if ui
                                .selectable_label(
                                    app.print_job_config.printer_name.as_deref() == Some(&p.name),
                                    label,
                                )
                                .clicked()
                            {
                                app.print_job_config.printer_name = Some(p.name.clone());
                            }
                        }
                    });
                if ui.button("Refresh").clicked() {
                    refresh_printers = true;
                }
            });

            ui.horizontal(|ui| {
                ui.label("Copies:");
                ui.add(DragValue::new(&mut app.print_job_config.copies).range(1..=999));
                ui.label("Pages:");
                ui.add(
                    egui::TextEdit::singleline(&mut app.print_job_config.page_range)
                        .desired_width(100.0)
                        .hint_text("all or 1,3-5"),
                );
            });

            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut app.print_job_config.force_cmyk, "CMYK Press Output");
                ui.checkbox(&mut app.print_job_config.fit_to_page, "Fit to Page");
                ui.checkbox(&mut app.print_job_config.crop_marks, "Crop Marks");
                ui.checkbox(&mut app.print_job_config.collate, "Collate");
            });

            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Print Document Now").strong())
                    .clicked()
                {
                    print_now = true;
                }
                if let Some(ref status) = app.printers_status {
                    ui.label(RichText::new(status).size(11.0).color(tokens.text_muted));
                }
            });

            ui.separator();
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
    if refresh_printers {
        app.refresh_system_printers();
    }
    if print_now {
        app.print_active_document_os();
    }
    app.show_print_preview_dialog = open;
}

/// Classic tabbed Studio Options dialog (`Tools -> Options...`, `Ctrl+J`).
fn show_options_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.show_options_dialog && !app.show_preferences_dialog {
        return;
    }
    let tokens = app.theme.tokens();
    let mut open = true;

    egui::Window::new("Options — Workspace, Preferences, Color & Laser Plugins (Ctrl+J)")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(640.0, 460.0))
        .min_size(egui::vec2(540.0, 380.0))
        .show(ctx, |ui| {
            // Left-to-right classic Options tab strip
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut app.options_tab, 0, "1. Appearance & Workspace");
                ui.selectable_value(&mut app.options_tab, 1, "2. General & Nudge");
                ui.selectable_value(&mut app.options_tab, 2, "3. Color Management (ICC)");
                ui.selectable_value(&mut app.options_tab, 3, "4. Laser & Plugins");
            });
            ui.separator();

            egui::ScrollArea::vertical()
                .id_salt("options_dialog_scroll")
                .max_height(355.0)
                .show(ui, |ui| match app.options_tab {
                    0 => show_options_appearance_workspace(ui, app, &tokens),
                    1 => show_options_general_nudge(ui, app, &tokens),
                    2 => show_options_color_management(ui, app, &tokens),
                    _ => show_options_laser_plugins(ui, app, &tokens),
                });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Changes apply live and persist locally via offline SQLite WAL.")
                        .size(11.0)
                        .color(tokens.text_muted),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(RichText::new("Close").strong()).clicked() {
                        app.show_options_dialog = false;
                        app.show_preferences_dialog = false;
                    }
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
) {
    ui.label(RichText::new(app.ui_language.tr("Visual Theme", "المظهر البصري")).strong());
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

    ui.add_space(8.0);
    ui.label(
        RichText::new(app.ui_language.tr(
            "Interface Language / لغة الواجهة",
            "لغة الواجهة (Interface Language)",
        ))
        .strong(),
    );
    ui.horizontal(|ui| {
        for lang in StudioLanguage::ALL {
            if ui
                .selectable_label(app.ui_language == lang, lang.label())
                .clicked()
            {
                app.set_ui_language(lang);
            }
        }
    });

    ui.add_space(8.0);
    ui.label(RichText::new("Active Studio Workspace Preset").strong());
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
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(active, RichText::new(preset.label()).strong())
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
    ui.label(RichText::new("Toolbox & Selection Ergonomics").strong());
    ui.checkbox(&mut app.toolbox_two_column, "Two-Column Toolbox Rail");
    ui.checkbox(&mut app.show_dockers, "Show Dockers Host Panel");
    ui.checkbox(
        &mut app.palette_show_names,
        "Show Color Names in Swatch Palette Bar",
    );
    ui.checkbox(
        &mut app.treat_all_objects_as_filled,
        "Treat All Objects as Filled (Click anywhere inside hollow shapes to select)",
    );

    ui.add_space(8.0);
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

    ui.add_space(8.0);
    ui.label(RichText::new("Custom Shortcut Overrides").strong());
    let remappable = ShortcutAction::REMAPPABLE_ACTIONS;
    if app.shortcut_editor_idx >= remappable.len() {
        app.shortcut_editor_idx = 0;
    }
    let (sel_action, sel_name) = remappable[app.shortcut_editor_idx];
    let current_binding =
        shortcut_label_for(app.shortcut_profile, &app.custom_shortcuts, sel_action);
    ui.horizontal_wrapped(|ui| {
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
        if ui.button("Assign").clicked() {
            let combo = app.shortcut_editor_combo.clone();
            match app.custom_shortcuts.set_binding(sel_action, &combo) {
                Ok(norm) => {
                    app.shortcut_editor_combo = norm.clone();
                    app.push_toast(format!("Bound {sel_name} -> {norm}"), false);
                }
                Err(err) => app.push_toast(format!("Invalid shortcut: {err}"), true),
            }
        }
        if ui.button("Reset Key").clicked() {
            app.custom_shortcuts.clear_binding(sel_action);
            app.shortcut_editor_combo.clear();
            app.push_toast(format!("Reset {sel_name} to profile default"), false);
        }
        if !app.custom_shortcuts.is_empty() && ui.button("Reset All Custom").clicked() {
            app.custom_shortcuts.bindings.clear();
            app.shortcut_editor_combo.clear();
            app.push_toast("Cleared all custom shortcut overrides", false);
        }
    });

    ui.add_space(8.0);
    ui.label(RichText::new("Portable Workspace Bundle (.coreews)").strong());
    ui.horizontal_wrapped(|ui| {
        if ui.button("Export Workspace (.coreews)...").clicked() {
            export_workspace_dialog(app);
        }
        if ui.button("Import Workspace (.coreews)...").clicked() {
            import_workspace_dialog(app);
        }
        if ui.button("Reset Workspace to Default").clicked() {
            app.apply_workspace_preset(WorkspacePreset::DefaultClassic);
        }
    });
}

fn show_options_general_nudge(
    ui: &mut egui::Ui,
    app: &mut StudioApp,
    tokens: &crate::theme::ThemeTokens,
) {
    ui.label(RichText::new("Units, Nudge & Duplicate Offsets").strong());
    if let Some(tab) = app.active_tab_mut() {
        ui.horizontal(|ui| {
            ui.label("Active Document Units:");
            ComboBox::from_id_salt("opt_doc_units")
                .width(120.0)
                .selected_text(tab.document.default_unit.suffix())
                .show_ui(ui, |ui| {
                    for u in [
                        Unit::Millimeter,
                        Unit::Centimeter,
                        Unit::Meter,
                        Unit::Inch,
                        Unit::Point,
                        Unit::Pixel,
                    ] {
                        ui.selectable_value(&mut tab.document.default_unit, u, u.suffix());
                    }
                });
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Nudge Distance (Arrow keys):");
            ui.add(
                DragValue::new(&mut tab.nudge_mm)
                    .range(0.01..=200.0)
                    .speed(0.1)
                    .suffix(" mm"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Super-Nudge Multiplier (Shift+Arrow):");
            ui.add(
                DragValue::new(&mut tab.super_nudge_factor)
                    .range(1.0..=20.0)
                    .speed(0.2)
                    .suffix(" ×"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Micro-Nudge Divisor (Ctrl+Arrow):");
            ui.add(
                DragValue::new(&mut tab.micro_nudge_divisor)
                    .range(1.0..=20.0)
                    .speed(0.2)
                    .prefix("÷ "),
            );
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Duplicate Offset (Ctrl+D):");
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
            RichText::new(
                "Open or create a document to adjust per-document units & nudge settings.",
            )
            .color(tokens.text_muted),
        );
    }

    ui.add_space(10.0);
    ui.separator();
    ui.label(RichText::new("Auto-Backup & Crash Recovery (Local SQLite WAL)").strong());
    let wal_status = if app.storage.is_some() {
        "Active — 100% Offline SQLite WAL with SHA-256 Snapshot Verification"
    } else {
        "In-Memory Session (Disk storage unavailable)"
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
    if ui.button("Save Recovery Snapshot Now").clicked() {
        match app.perform_autosave_active() {
            Ok(()) => app.push_toast("Saved manual crash-recovery snapshot", false),
            Err(e) => app.push_toast(e, true),
        }
    }
}

fn show_options_color_management(
    ui: &mut egui::Ui,
    app: &mut StudioApp,
    tokens: &crate::theme::ThemeTokens,
) {
    ui.label(RichText::new("Clean-Room ICC Color Management & Soft Proofing").strong());
    ui.label(
        RichText::new(
            "Simulate CMYK press output on screen using built-in CIE D50 / Bradford ICC profiles.",
        )
        .size(11.5)
        .color(tokens.text_muted),
    );
    ui.add_space(8.0);

    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Proofing Profile:");
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

    ui.horizontal(|ui| {
        ui.label("Rendering Intent:");
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
            "Proof Colors (Enable CMYK Soft Proofing on Canvas)",
        )
        .changed();
    changed |= ui
        .checkbox(
            &mut app.gamut_warning,
            "Gamut Warning (Highlight Out-of-CMYK-Gamut RGB Pixels)",
        )
        .changed();

    if changed {
        app.sync_proof_options_to_tabs();
    }

    ui.add_space(8.0);
    ui.separator();
    ui.label(RichText::new("Document Embedded / Custom ICC Profile (.icc / .icm)").strong());
    let custom_desc = app
        .active_tab()
        .and_then(|t| t.document.custom_icc_name.clone());
    let custom_len = app
        .active_tab()
        .and_then(|t| t.document.custom_icc_profile.as_ref().map(Vec::len));
    match (custom_desc, custom_len) {
        (Some(name), Some(bytes_len)) => {
            ui.label(
                RichText::new(format!(
                    "Active custom profile: {name} ({:.1} KiB)",
                    bytes_len as f64 / 1024.0
                ))
                .size(11.5)
                .color(tokens.accent),
            );
        }
        _ => {
            ui.label(
                RichText::new("No external .icc/.icm profile attached to active document (using built-in D50 LUT).")
                    .size(11.5)
                    .color(tokens.text_muted),
            );
        }
    }
    ui.horizontal(|ui| {
        if ui.button("Load Custom .icc / .icm Profile...").clicked() {
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
                        let attached = app.active_tab_mut().map(|tab| {
                            let res = tab.document.set_custom_icc_profile(bytes);
                            if res.is_ok() {
                                tab.dirty = true;
                                tab.needs_rerender = true;
                            }
                            res
                        });
                        match attached {
                            Some(Ok(prof)) => {
                                app.soft_proof_cmyk = true;
                                app.sync_proof_options_to_tabs();
                                app.push_toast(
                                    format!("Attached custom ICC profile: {}", prof.description),
                                    false,
                                );
                            }
                            Some(Err(err)) => {
                                app.push_toast(format!("Invalid ICC profile: {err}"), true);
                            }
                            None => app.push_toast("No active document", true),
                        }
                    }
                    Err(err) => app.push_toast(err, true),
                }
            }
        }
        if custom_len.is_some() && ui.button("Clear Custom ICC").clicked() {
            if let Some(tab) = app.active_tab_mut() {
                tab.document.custom_icc_profile = None;
                tab.document.custom_icc_name = None;
                tab.document.touch();
                tab.dirty = true;
                tab.needs_rerender = true;
            }
            app.sync_proof_options_to_tabs();
            app.push_toast("Cleared custom document ICC profile", false);
        }
    });
}

fn show_options_laser_plugins(
    ui: &mut egui::Ui,
    app: &mut StudioApp,
    tokens: &crate::theme::ThemeTokens,
) {
    ui.label(RichText::new("Laser Cutting, Engraving & Plotter External Bridges").strong());

    let plugin_dirs = default_plugin_dirs();
    if let Some(primary_dir) = plugin_dirs.first() {
        ui.label(
            RichText::new(format!("User Plugins Folder: {}", primary_dir.display()))
                .monospace()
                .size(11.0)
                .color(tokens.text_muted),
        );
    }

    ui.horizontal_wrapped(|ui| {
        if ui.button("Install Sample Custom Plugin").clicked() {
            install_sample_plugin_action(app);
        }
        if ui.button("Reload Plugins Registry").clicked() {
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
                    ui.horizontal(|ui| {
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
                                RichText::new("○ Not detected (file export ready)")
                                    .size(10.5)
                                    .color(tokens.text_muted),
                            );
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Executable:");
                        ui.add(
                            egui::TextEdit::singleline(override_entry)
                                .hint_text("Auto-detect or enter custom executable path...")
                                .desired_width(340.0),
                        );
                        if ui.small_button("Browse...").clicked() {
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
        .fixed_size(egui::vec2(480.0, 290.0))
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

/// Shared helper to export active workspace as `.coreews` JSON via file dialog.
pub fn export_workspace_dialog(app: &mut StudioApp) {
    if let Some(path) = rfd::FileDialog::new()
        .set_title("Export Studio Workspace (.coreews)")
        .set_file_name("coree_workspace.coreews")
        .add_filter("coree Workspace", &["coreews", "json"])
        .save_file()
    {
        let json = app.export_workspace_json();
        match coree_io::atomic_write_bytes(&path, json.as_bytes()) {
            Ok(()) => app.push_toast(format!("Exported workspace to {}", path.display()), false),
            Err(e) => app.push_toast(format!("Workspace export failed: {e}"), true),
        }
    }
}

/// Shared helper to import `.coreews` JSON workspace via file dialog.
pub fn import_workspace_dialog(app: &mut StudioApp) {
    if let Some(path) = rfd::FileDialog::new()
        .set_title("Import Studio Workspace (.coreews)")
        .add_filter("coree Workspace", &["coreews", "json"])
        .pick_file()
    {
        match read_text_file_bounded(&path, MAX_TEXT_READ_BYTES) {
            Ok(json) => {
                if let Err(err) = app.import_workspace_json(&json) {
                    app.push_toast(err, true);
                }
            }
            Err(err) => app.push_toast(err, true),
        }
    }
}

/// Shared helper to install the sample custom Rhai laser plugin and reload registry.
pub fn install_sample_plugin_action(app: &mut StudioApp) {
    let dirs = default_plugin_dirs();
    let target_dir = dirs
        .first()
        .cloned()
        .unwrap_or_else(|| std::env::temp_dir().join("coree_plugins"));
    match install_sample_plugin_template(&target_dir) {
        Ok(created) => {
            let mut reg = PluginRegistry::with_builtins();
            let loaded = reg.load_discovered(&dirs);
            app.plugin_registry = reg;
            invalidate_plugin_detect_cache();
            app.push_toast(
                format!(
                    "Installed sample custom plugin at {} ({loaded} custom plugin(s) loaded)",
                    created.display()
                ),
                false,
            );
        }
        Err(e) => app.push_toast(format!("Install sample plugin failed: {e}"), true),
    }
}

/// Shared helper to reload built-in + discovered custom plugins from user directories.
pub fn reload_plugins_action(app: &mut StudioApp) {
    let dirs = default_plugin_dirs();
    let mut reg = PluginRegistry::with_builtins();
    let loaded = reg.load_discovered(&dirs);
    app.plugin_registry = reg;
    invalidate_plugin_detect_cache();
    app.push_toast(
        format!(
            "Reloaded plugins: {} built-in + {loaded} custom",
            PluginRegistry::builtin_presets().len()
        ),
        false,
    );
}

fn rendering_intent_label(intent: RenderingIntent) -> &'static str {
    match intent {
        RenderingIntent::Perceptual => "Perceptual (Photographic)",
        RenderingIntent::RelativeColorimetric => "Relative Colorimetric (Print Standard)",
        RenderingIntent::Saturation => "Saturation (Business Graphics)",
        RenderingIntent::AbsoluteColorimetric => "Absolute Colorimetric (Hard Proof)",
    }
}

fn gcode_dialect_label(dialect: GcodeDialect) -> &'static str {
    match dialect {
        GcodeDialect::Grbl11 => "GRBL 1.1+ Laser Mode",
        GcodeDialect::Marlin => "Marlin Laser Firmware",
        GcodeDialect::RuidaGcode => "Ruida G-Code Dialect",
    }
}

fn edit_color_compact_sliders(ui: &mut egui::Ui, color: &mut Color, id_salt: &str) -> bool {
    let mut changed = false;
    let tokens_surface = ui.visuals().extreme_bg_color;
    let [r, g, b, a] = color.to_rgba_f32();
    let mut rgb_u8 = [
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    ];
    let mut cmyk = color.to_cmyk();
    let mut c_pct = cmyk[0] * 100.0;
    let mut m_pct = cmyk[1] * 100.0;
    let mut y_pct = cmyk[2] * 100.0;
    let mut k_pct = cmyk[3] * 100.0;

    ui.horizontal(|ui| {
        let (sw_rect, _) = ui.allocate_exact_size(egui::vec2(38.0, 38.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(sw_rect, egui::CornerRadius::same(4), tokens_surface);
        ui.painter().rect(
            sw_rect,
            egui::CornerRadius::same(4),
            core_color_to_egui(color),
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.fg_stroke.color),
            egui::StrokeKind::Inside,
        );
        ui.vertical(|ui| {
            let mut eg_col = egui::Color32::from_rgba_unmultiplied(
                rgb_u8[0],
                rgb_u8[1],
                rgb_u8[2],
                (a * 255.0).round() as u8,
            );
            ui.horizontal(|ui| {
                ui.label("RGB:");
                if ui.color_edit_button_srgba(&mut eg_col).changed() {
                    *color = Color::rgba8(eg_col.r(), eg_col.g(), eg_col.b(), eg_col.a());
                    return;
                }
                if ui
                    .add(DragValue::new(&mut rgb_u8[0]).prefix("R ").range(0..=255))
                    .changed()
                    | ui.add(DragValue::new(&mut rgb_u8[1]).prefix("G ").range(0..=255))
                        .changed()
                    | ui.add(DragValue::new(&mut rgb_u8[2]).prefix("B ").range(0..=255))
                        .changed()
                {
                    *color =
                        Color::rgba8(rgb_u8[0], rgb_u8[1], rgb_u8[2], (a * 255.0).round() as u8);
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("CMYK%:");
                let dc = ui
                    .add(
                        DragValue::new(&mut c_pct)
                            .prefix("C ")
                            .speed(1.0)
                            .range(0.0..=100.0),
                    )
                    .changed();
                let dm = ui
                    .add(
                        DragValue::new(&mut m_pct)
                            .prefix("M ")
                            .speed(1.0)
                            .range(0.0..=100.0),
                    )
                    .changed();
                let dy = ui
                    .add(
                        DragValue::new(&mut y_pct)
                            .prefix("Y ")
                            .speed(1.0)
                            .range(0.0..=100.0),
                    )
                    .changed();
                let dk = ui
                    .add(
                        DragValue::new(&mut k_pct)
                            .prefix("K ")
                            .speed(1.0)
                            .range(0.0..=100.0),
                    )
                    .changed();
                if dc || dm || dy || dk {
                    cmyk = [c_pct / 100.0, m_pct / 100.0, y_pct / 100.0, k_pct / 100.0];
                    *color = Color::Cmyk {
                        c: cmyk[0],
                        m: cmyk[1],
                        y: cmyk[2],
                        k: cmyk[3],
                        a,
                    };
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                let is_cut = matches!(color, Color::Spot { name, .. } if name == "CutContour");
                if ui.selectable_label(is_cut, "Spot CutContour").clicked() {
                    *color = Color::spot("CutContour", 1.0, [0.0, 1.0, 0.0, 0.0]);
                    changed = true;
                }
                if ui.small_button("Key Black").clicked() {
                    *color = Color::KEY_BLACK;
                    changed = true;
                }
                if ui.small_button("White").clicked() {
                    *color = Color::WHITE;
                    changed = true;
                }
                let _ = id_salt;
            });
        });
    });
    changed
}

/// Classic Studio Modal **Edit Fill Dialog (`F11`)**.
fn show_edit_fill_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.show_edit_fill_dialog {
        return;
    }
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let mut open = true;
    let mut commit_close = false;
    let mut apply_only = false;
    let mut cancel_clicked = false;

    egui::Window::new(lang.tr("Edit Fill (F11)", "تحرير التعبئة (F11)"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(520.0, 440.0))
        .show(ctx, |ui| {
            // Fill type tabs for Edit Fill (`F11`) bar
            ui.horizontal_wrapped(|ui| {
                if ui
                    .selectable_label(
                        matches!(app.edit_fill_draft, Fill::None),
                        lang.tr("No Fill", "بدون تعبئة"),
                    )
                    .clicked()
                {
                    app.edit_fill_draft = Fill::None;
                }
                if ui
                    .selectable_label(
                        matches!(app.edit_fill_draft, Fill::Solid(_)),
                        lang.tr("Uniform Fill", "تعبئة موحدة"),
                    )
                    .clicked()
                {
                    let c = app
                        .edit_fill_draft
                        .referenced_colors()
                        .into_iter()
                        .next()
                        .unwrap_or(Color::CYAN);
                    app.edit_fill_draft = Fill::Solid(c);
                }
                let is_fountain = matches!(
                    app.edit_fill_draft,
                    Fill::LinearGradient { .. }
                        | Fill::RadialGradient { .. }
                        | Fill::ConicalGradient { .. }
                        | Fill::SquareGradient { .. }
                );
                if ui
                    .selectable_label(is_fountain, lang.tr("Fountain Fill", "تدرج لوني"))
                    .clicked()
                {
                    let cols = app.edit_fill_draft.referenced_colors();
                    let c0 = cols.first().cloned().unwrap_or(Color::CYAN);
                    let c1 = cols.get(1).cloned().unwrap_or(Color::WHITE);
                    app.edit_fill_draft = Fill::LinearGradient {
                        start: Point2D::new(0.0, 20.0),
                        end: Point2D::new(80.0, 20.0),
                        stops: vec![GradientStop::new(0.0, c0), GradientStop::new(1.0, c1)],
                    };
                    app.edit_fill_selected_stop = 0;
                }
                if ui
                    .selectable_label(
                        matches!(app.edit_fill_draft, Fill::TwoColorPattern { .. }),
                        lang.tr("2-Color Pattern", "نمط ثنائي"),
                    )
                    .clicked()
                {
                    app.edit_fill_draft = Fill::TwoColorPattern {
                        fg: Color::KEY_BLACK,
                        bg: Color::WHITE,
                        pattern_name: "Checker".to_owned(),
                        tile_w: 8.0,
                        tile_h: 8.0,
                    };
                }
                if ui
                    .selectable_label(
                        matches!(app.edit_fill_draft, Fill::TextureFill { .. }),
                        lang.tr("PostScript / Texture", "بوست سكريبت / نسيج"),
                    )
                    .clicked()
                {
                    app.edit_fill_draft = Fill::TextureFill {
                        preset: PostScriptFillPreset::Hatch.preset_str(),
                        seed: 1,
                        fg: Color::KEY_BLACK,
                        bg: Color::WHITE,
                    };
                }
            });
            ui.separator();

            match &mut app.edit_fill_draft {
                Fill::None => {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(lang.tr(
                            "Object interior is unfilled (transparent).",
                            "داخل الكائن فارغ (شفاف بدون لون).",
                        ))
                        .color(tokens.text_muted),
                    );
                }
                Fill::Solid(c) => {
                    ui.label(
                        RichText::new(lang.tr("Uniform Ink / Color", "لون التعبئة الموحد"))
                            .strong(),
                    );
                    ui.add_space(4.0);
                    edit_color_compact_sliders(ui, c, "f11_uniform");
                }
                Fill::LinearGradient { .. }
                | Fill::RadialGradient { .. }
                | Fill::ConicalGradient { .. }
                | Fill::SquareGradient { .. } => {
                    // Sub-type switcher (Linear / Radial / Conical / Square)
                    let cur_sub = match app.edit_fill_draft {
                        Fill::LinearGradient { .. } => 0,
                        Fill::RadialGradient { .. } => 1,
                        Fill::ConicalGradient { .. } => 2,
                        _ => 3,
                    };
                    let mut next_sub = cur_sub;
                    ui.horizontal(|ui| {
                        ui.label(lang.tr("Type:", "النوع:"));
                        ui.selectable_value(&mut next_sub, 0, lang.tr("Linear", "خطي"));
                        ui.selectable_value(&mut next_sub, 1, lang.tr("Radial", "شعاعي"));
                        ui.selectable_value(&mut next_sub, 2, lang.tr("Conical", "مخروطي"));
                        ui.selectable_value(&mut next_sub, 3, lang.tr("Square", "مربع"));
                    });
                    if next_sub != cur_sub {
                        let stops = match &app.edit_fill_draft {
                            Fill::LinearGradient { stops, .. }
                            | Fill::RadialGradient { stops, .. }
                            | Fill::ConicalGradient { stops, .. }
                            | Fill::SquareGradient { stops, .. } => stops.clone(),
                            _ => vec![],
                        };
                        app.edit_fill_draft = match next_sub {
                            1 => Fill::RadialGradient {
                                center: Point2D::new(40.0, 40.0),
                                radius: 45.0,
                                focal: Point2D::new(40.0, 40.0),
                                stops,
                            },
                            2 => Fill::ConicalGradient {
                                center: Point2D::new(40.0, 40.0),
                                angle_deg: 0.0,
                                stops,
                            },
                            3 => Fill::SquareGradient {
                                center: Point2D::new(40.0, 40.0),
                                size: 50.0,
                                angle_deg: 0.0,
                                stops,
                            },
                            _ => Fill::LinearGradient {
                                start: Point2D::new(0.0, 20.0),
                                end: Point2D::new(80.0, 20.0),
                                stops,
                            },
                        };
                    }

                    // Edit stops & geometry parameters
                    let stops_mut = match &mut app.edit_fill_draft {
                        Fill::LinearGradient { stops, .. }
                        | Fill::RadialGradient { stops, .. }
                        | Fill::ConicalGradient { stops, .. }
                        | Fill::SquareGradient { stops, .. } => stops,
                        _ => unreachable!(),
                    };
                    if stops_mut.is_empty() {
                        stops_mut.push(GradientStop::new(0.0, Color::CYAN));
                        stops_mut.push(GradientStop::new(1.0, Color::WHITE));
                    }
                    app.edit_fill_selected_stop = app
                        .edit_fill_selected_stop
                        .min(stops_mut.len().saturating_sub(1));

                    // Live Fountain Gradient Preview Strip + Stop Markers
                    let (bar_rect, bar_resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width().max(240.0), 26.0),
                        egui::Sense::click(),
                    );
                    let steps = 40usize;
                    for i in 0..steps {
                        let t0 = (i as f32) / (steps as f32);
                        let t1 = ((i + 1) as f32) / (steps as f32);
                        let col = sample_fountain_stop(stops_mut, (t0 + t1) * 0.5);
                        let seg_r = egui::Rect::from_min_max(
                            egui::pos2(bar_rect.left() + bar_rect.width() * t0, bar_rect.top()),
                            egui::pos2(bar_rect.left() + bar_rect.width() * t1, bar_rect.bottom()),
                        );
                        ui.painter().rect_filled(
                            seg_r,
                            egui::CornerRadius::ZERO,
                            core_color_to_egui(&col),
                        );
                    }
                    ui.painter().rect_stroke(
                        bar_rect,
                        egui::CornerRadius::same(2),
                        Stroke::new(1.0, tokens.border_input),
                        egui::StrokeKind::Inside,
                    );
                    for (idx, st) in stops_mut.iter().enumerate() {
                        let mx = bar_rect.left() + bar_rect.width() * st.offset;
                        let sel = idx == app.edit_fill_selected_stop;
                        let marker_r = egui::Rect::from_center_size(
                            egui::pos2(mx, bar_rect.bottom()),
                            egui::vec2(if sel { 10.0 } else { 7.0 }, if sel { 10.0 } else { 7.0 }),
                        );
                        ui.painter().rect(
                            marker_r,
                            egui::CornerRadius::same(1),
                            core_color_to_egui(&st.color),
                            Stroke::new(
                                if sel { 2.0 } else { 1.0 },
                                if sel { tokens.accent } else { Color32::WHITE },
                            ),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if bar_resp.clicked() {
                        if let Some(p) = bar_resp.interact_pointer_pos() {
                            let click_t = ((p.x - bar_rect.left()) / bar_rect.width().max(1.0))
                                .clamp(0.0, 1.0);
                            if let Some((nearest, _)) =
                                stops_mut.iter().enumerate().min_by(|(_, a), (_, b)| {
                                    (a.offset - click_t)
                                        .abs()
                                        .total_cmp(&(b.offset - click_t).abs())
                                })
                            {
                                app.edit_fill_selected_stop = nearest;
                            }
                        }
                    }

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "Stop {}/{}:",
                            app.edit_fill_selected_stop + 1,
                            stops_mut.len()
                        ));
                        for idx in 0..stops_mut.len() {
                            if ui
                                .selectable_label(
                                    app.edit_fill_selected_stop == idx,
                                    format!("#{}", idx + 1),
                                )
                                .clicked()
                            {
                                app.edit_fill_selected_stop = idx;
                            }
                        }
                        if ui.button("+ Stop").clicked() && stops_mut.len() < 16 {
                            let c = sample_fountain_stop(stops_mut, 0.5);
                            stops_mut.push(GradientStop::new(0.5, c));
                            app.edit_fill_selected_stop = stops_mut.len() - 1;
                        }
                        if ui.button("- Stop").clicked() && stops_mut.len() > 2 {
                            stops_mut.remove(app.edit_fill_selected_stop);
                            app.edit_fill_selected_stop =
                                app.edit_fill_selected_stop.min(stops_mut.len() - 1);
                        }
                        if ui.button("Reverse").clicked() {
                            for st in stops_mut.iter_mut() {
                                st.offset = (1.0 - st.offset).clamp(0.0, 1.0);
                            }
                            stops_mut.reverse();
                        }
                    });
                    if let Some(st) = stops_mut.get_mut(app.edit_fill_selected_stop) {
                        let mut pct = st.offset * 100.0;
                        ui.horizontal(|ui| {
                            ui.label(lang.tr("Node Position:", "موضع العقدة:"));
                            if ui
                                .add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix("%"))
                                .changed()
                            {
                                st.offset = (pct / 100.0).clamp(0.0, 1.0);
                            }
                        });
                        edit_color_compact_sliders(ui, &mut st.color, "f11_stop");
                    }
                    ui.separator();
                    match &mut app.edit_fill_draft {
                        Fill::LinearGradient { start, end, .. } => {
                            ui.horizontal(|ui| {
                                ui.label("Start/End mm:");
                                ui.add(DragValue::new(&mut start.x).prefix("X1 "));
                                ui.add(DragValue::new(&mut start.y).prefix("Y1 "));
                                ui.add(DragValue::new(&mut end.x).prefix("X2 "));
                                ui.add(DragValue::new(&mut end.y).prefix("Y2 "));
                            });
                        }
                        Fill::RadialGradient { center, radius, .. } => {
                            ui.horizontal(|ui| {
                                ui.add(DragValue::new(&mut center.x).prefix("CX "));
                                ui.add(DragValue::new(&mut center.y).prefix("CY "));
                                ui.add(DragValue::new(radius).prefix("R ").range(0.5..=2000.0));
                            });
                        }
                        Fill::ConicalGradient {
                            center, angle_deg, ..
                        } => {
                            ui.horizontal(|ui| {
                                ui.add(DragValue::new(&mut center.x).prefix("CX "));
                                ui.add(DragValue::new(&mut center.y).prefix("CY "));
                                ui.add(egui::Slider::new(angle_deg, -180.0..=180.0).suffix("°"));
                            });
                        }
                        Fill::SquareGradient {
                            center,
                            size,
                            angle_deg,
                            ..
                        } => {
                            ui.horizontal(|ui| {
                                ui.add(DragValue::new(&mut center.x).prefix("CX "));
                                ui.add(DragValue::new(&mut center.y).prefix("CY "));
                                ui.add(DragValue::new(size).prefix("Size ").range(1.0..=2000.0));
                                ui.add(egui::Slider::new(angle_deg, -180.0..=180.0).suffix("°"));
                            });
                        }
                        _ => {}
                    }
                }
                Fill::TwoColorPattern {
                    fg,
                    bg,
                    tile_w,
                    tile_h,
                    ..
                } => {
                    ui.horizontal(|ui| {
                        ui.add(
                            DragValue::new(tile_w)
                                .prefix("Tile W ")
                                .suffix(" mm")
                                .range(1.0..=500.0),
                        );
                        ui.add(
                            DragValue::new(tile_h)
                                .prefix("Tile H ")
                                .suffix(" mm")
                                .range(1.0..=500.0),
                        );
                    });
                    ui.label("Foreground Color:");
                    edit_color_compact_sliders(ui, fg, "f11_pat_fg");
                    ui.label("Background Color:");
                    edit_color_compact_sliders(ui, bg, "f11_pat_bg");
                }
                Fill::TextureFill { preset, fg, bg, .. } => {
                    ui.horizontal_wrapped(|ui| {
                        for p in PostScriptFillPreset::ALL {
                            let pid = p.preset_str();
                            if ui
                                .selectable_label(*preset == pid, p.display_name())
                                .clicked()
                            {
                                *preset = pid;
                            }
                        }
                    });
                    ui.label("Foreground:");
                    edit_color_compact_sliders(ui, fg, "f11_tex_fg");
                    ui.label("Background:");
                    edit_color_compact_sliders(ui, bg, "f11_tex_bg");
                }
                _ => {
                    ui.label("Vector/Bitmap/Mesh fills can be edited directly via canvas handles.");
                }
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(lang.tr("Cancel", "إلغاء")).clicked() {
                        cancel_clicked = true;
                    }
                    if ui
                        .button(RichText::new(lang.tr("OK", "موافق")).strong())
                        .clicked()
                    {
                        commit_close = true;
                    }
                    if ui.button(lang.tr("Apply", "تطبيق")).clicked() {
                        apply_only = true;
                    }
                });
            });
        });

    if apply_only {
        app.apply_edit_fill_draft();
    }
    if commit_close {
        app.apply_edit_fill_draft();
        app.show_edit_fill_dialog = false;
    } else if !open || cancel_clicked {
        app.show_edit_fill_dialog = false;
    }
}

fn sample_fountain_stop(stops: &[GradientStop], t: f32) -> Color {
    if stops.is_empty() {
        return Color::BLACK;
    }
    let mut sorted = stops.to_vec();
    sorted.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    if t <= sorted[0].offset {
        return sorted[0].color.clone();
    }
    if let Some(last) = sorted.last() {
        if t >= last.offset {
            return last.color.clone();
        }
    }
    for win in sorted.windows(2) {
        if t >= win[0].offset && t <= win[1].offset {
            let span = (win[1].offset - win[0].offset).max(1e-5);
            return win[0].color.lerp(&win[1].color, (t - win[0].offset) / span);
        }
    }
    sorted[0].color.clone()
}

/// Classic Studio Modal **Outline Pen Dialog (`F12`)**.
fn show_outline_pen_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.show_outline_pen_dialog {
        return;
    }
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let mut open = true;
    let mut commit_close = false;
    let mut apply_only = false;
    let mut cancel_clicked = false;

    egui::Window::new(lang.tr("Outline Pen (F12)", "قلم الحدود (F12)"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(510.0, 480.0))
        .show(ctx, |ui| {
            let o = &mut app.outline_pen_draft;

            ui.label(RichText::new(lang.tr("Outline Color", "لون الحد")).strong());
            edit_color_compact_sliders(ui, &mut o.color, "f12_color");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label(lang.tr("Width:", "السماكة:"));
                ui.add(
                    DragValue::new(&mut o.width_mm)
                        .speed(0.05)
                        .suffix(" mm")
                        .range(0.0..=100.0),
                );
                if o.width_mm > 0.0 && o.is_hairline && o.width_mm > 0.1 {
                    o.is_hairline = false;
                }
                if ui
                    .selectable_label(!o.is_visible(), lang.tr("None", "بلا"))
                    .clicked()
                {
                    o.width_mm = 0.0;
                    o.is_hairline = false;
                }
                if ui
                    .selectable_label(o.is_hairline, lang.tr("Hairline", "خط شعري"))
                    .clicked()
                {
                    o.is_hairline = true;
                    o.width_mm = 0.0762;
                }
                for (lbl, mm) in [
                    ("0.5pt", 0.176),
                    ("1pt", 0.353),
                    ("2pt", 0.706),
                    ("4pt", 1.411),
                    ("2mm", 2.0),
                ] {
                    if ui.small_button(lbl).clicked() {
                        o.is_hairline = false;
                        o.width_mm = mm;
                    }
                }
            });

            ui.horizontal(|ui| {
                ui.label(lang.tr("Dash Style:", "الخط المتقطع:"));
                if ui
                    .selectable_label(o.dash_pattern.is_empty(), "Solid")
                    .clicked()
                {
                    o.dash_pattern.clear();
                }
                if ui
                    .selectable_label(o.dash_pattern == vec![4.0, 2.0], "Dashed")
                    .clicked()
                {
                    o.dash_pattern = vec![4.0, 2.0];
                }
                if ui
                    .selectable_label(o.dash_pattern == vec![0.8, 1.5], "Dotted")
                    .clicked()
                {
                    o.dash_pattern = vec![0.8, 1.5];
                }
                if ui
                    .selectable_label(o.dash_pattern == vec![4.0, 1.5, 1.0, 1.5], "Dash-Dot")
                    .clicked()
                {
                    o.dash_pattern = vec![4.0, 1.5, 1.0, 1.5];
                }
            });

            ui.horizontal(|ui| {
                ui.label(lang.tr("Corners:", "الزوايا:"));
                ui.selectable_value(&mut o.join, LineJoin::Miter, "Miter");
                ui.selectable_value(&mut o.join, LineJoin::Round, "Round");
                ui.selectable_value(&mut o.join, LineJoin::Bevel, "Bevel");
                ui.label("Miter Limit:");
                ui.add(
                    DragValue::new(&mut o.miter_limit)
                        .speed(0.5)
                        .range(1.0..=90.0),
                );
            });

            ui.horizontal(|ui| {
                ui.label(lang.tr("Line Caps:", "نهايات الخط:"));
                ui.selectable_value(&mut o.cap, LineCap::Butt, "Butt");
                ui.selectable_value(&mut o.cap, LineCap::Round, "Round");
                ui.selectable_value(&mut o.cap, LineCap::Square, "Square");
            });

            let arrows = [
                ArrowheadStyle::None,
                ArrowheadStyle::Triangle,
                ArrowheadStyle::Stealth,
                ArrowheadStyle::OpenVee,
                ArrowheadStyle::Diamond,
                ArrowheadStyle::Circle,
                ArrowheadStyle::Square,
            ];
            let arrow_lbl = |a: &ArrowheadStyle| match a {
                ArrowheadStyle::None => "None",
                ArrowheadStyle::Triangle => "Triangle",
                ArrowheadStyle::Stealth => "Stealth",
                ArrowheadStyle::OpenVee => "Open Vee",
                ArrowheadStyle::Diamond => "Diamond",
                ArrowheadStyle::Circle => "Circle",
                ArrowheadStyle::Square => "Square",
                ArrowheadStyle::Custom(_) => "Custom",
            };
            ui.horizontal(|ui| {
                ui.label(lang.tr("Start Arrow:", "بداية السهم:"));
                ComboBox::from_id_salt("f12_start_arrow")
                    .selected_text(arrow_lbl(&o.start_arrow))
                    .show_ui(ui, |ui| {
                        for a in &arrows {
                            ui.selectable_value(&mut o.start_arrow, a.clone(), arrow_lbl(a));
                        }
                    });
                ui.label(lang.tr("End Arrow:", "نهاية السهم:"));
                ComboBox::from_id_salt("f12_end_arrow")
                    .selected_text(arrow_lbl(&o.end_arrow))
                    .show_ui(ui, |ui| {
                        for a in &arrows {
                            ui.selectable_value(&mut o.end_arrow, a.clone(), arrow_lbl(a));
                        }
                    });
                if ui.small_button("Swap").clicked() {
                    std::mem::swap(&mut o.start_arrow, &mut o.end_arrow);
                }
            });

            ui.separator();
            ui.label(
                RichText::new(
                    lang.tr("Calligraphy Nib & Print Flags", "ريشة الخط وخيارات الطباعة"),
                )
                .strong(),
            );
            ui.horizontal(|ui| {
                let (nib_rect, _) =
                    ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::hover());
                ui.painter().rect_stroke(
                    nib_rect,
                    egui::CornerRadius::same(4),
                    Stroke::new(1.0, tokens.border),
                    egui::StrokeKind::Inside,
                );
                let nc = nib_rect.center();
                let rad = o.calligraphy_angle_deg.to_radians() as f32;
                let rx = 16.0_f32;
                let ry = (16.0 / o.calligraphy_stretch.clamp(0.1, 10.0) as f32).clamp(2.5, 18.0);
                let mut pts = Vec::with_capacity(24);
                for i in 0..24 {
                    let th = (i as f32) * std::f32::consts::TAU / 24.0;
                    let lx = rx * th.cos();
                    let ly = ry * th.sin();
                    let px = nc.x + lx * rad.cos() - ly * rad.sin();
                    let py = nc.y + lx * rad.sin() + ly * rad.cos();
                    pts.push(egui::pos2(px, py));
                }
                ui.painter().add(egui::Shape::convex_polygon(
                    pts,
                    tokens.accent.gamma_multiply(0.35),
                    Stroke::new(1.5, tokens.accent),
                ));

                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Nib Angle:");
                        ui.add(
                            egui::Slider::new(&mut o.calligraphy_angle_deg, -180.0..=180.0)
                                .suffix("°"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Stretch:");
                        ui.add(egui::Slider::new(&mut o.calligraphy_stretch, 0.2..=5.0));
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut o.behind_fill, lang.tr("Behind Fill", "خلف التعبئة"));
                        ui.checkbox(
                            &mut o.scale_with_object,
                            lang.tr("Scale with Object", "تحجيم مع الكائن"),
                        );
                        ui.checkbox(&mut o.overprint, lang.tr("Overprint", "طباعة فوقية"));
                    });
                });
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(lang.tr("Remove Outline", "إزالة الحد")).clicked() {
                    app.outline_pen_draft = OutlineStyle::none();
                    apply_only = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(lang.tr("Cancel", "إلغاء")).clicked() {
                        cancel_clicked = true;
                    }
                    if ui
                        .button(RichText::new(lang.tr("OK", "موافق")).strong())
                        .clicked()
                    {
                        commit_close = true;
                    }
                    if ui.button(lang.tr("Apply", "تطبيق")).clicked() {
                        apply_only = true;
                    }
                });
            });
        });

    if apply_only {
        app.apply_outline_pen_draft();
    }
    if commit_close {
        app.apply_outline_pen_draft();
        app.show_outline_pen_dialog = false;
    } else if !open || cancel_clicked {
        app.show_outline_pen_dialog = false;
    }
}
