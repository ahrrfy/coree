// SPDX-License-Identifier: MIT OR Apache-2.0

//! Studio modal dialogs: `NewDocumentDialog`, `ExportDialog`, `PrintPreviewDialog`,
//! classic tabbed `OptionsDialog (Ctrl+J)`, and `AboutDialog` (powered by
//! `coree_core::brand::BrandInfo`).

use crate::io_bridge::{read_text_file_bounded, MAX_TEXT_READ_BYTES};
use crate::shortcuts::ShortcutProfile;
use crate::state::{StudioApp, WorkspacePreset};
use crate::theme::StudioTheme;
use coree_cdr::CdrWriteVersion;
use coree_core::brand::BrandInfo;
use coree_core::{BuiltinIccProfile, ColorMode, Document, RenderingIntent, Unit};
use coree_prod::{
    default_plugin_dirs, detect_executable, install_sample_plugin_template, GcodeDialect,
    PluginKind, PluginManifest, PluginRegistry,
};
use egui::{ComboBox, DragValue, Frame, Id, Margin, RichText, Stroke};
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
