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
    ArrowheadStyle, BuiltinIccProfile, Color, ColorMode, CommandOp, Document, Fill, GradientStop,
    LineCap, LineJoin, OutlineStyle, Point2D, PostScriptFillPreset, RenderingIntent, Unit,
};
use coree_prod::{
    default_plugin_dirs, detect_executable, install_sample_plugin_template, GcodeDialect,
    PluginKind, PluginManifest, PluginRegistry,
};
use egui::{
    pos2, vec2, Align2, Color32, ComboBox, CornerRadius, DragValue, FontId, Frame, Id, Margin,
    Rect, RichText, Sense, Stroke, StrokeKind,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

mod doc_export;
pub(crate) use doc_export::export_laser_dxf_dialog;
mod fill_outline;
mod options;
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

pub(super) const NEW_DOC_PRESETS: [(&str, f64, f64, ColorMode, f32, f64); 8] = [
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

pub(super) const EXPORT_FORMATS: [(&str, &str); 21] = [
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
    ("AutoCAD Binary (.dwg)", "dwg"),
    ("Windows Metafile (.wmf)", "wmf"),
    ("Enhanced Metafile (.emf)", "emf"),
    ("Photoshop PSD (.psd)", "psd"),
];

/// Format index constant for Laser G-Code inside [`EXPORT_FORMATS`].
pub const EXPORT_FMT_GCODE: usize = 11;

/// Renders all open modal studio dialogs.
pub fn show_dialogs(ctx: &egui::Context, app: &mut StudioApp) {
    doc_export::show_new_document_dialog(ctx, app);
    doc_export::show_export_dialog(ctx, app);
    show_print_preview_dialog(ctx, app);
    options::show_options_dialog(ctx, app);
    fill_outline::show_edit_fill_dialog(ctx, app);
    fill_outline::show_outline_pen_dialog(ctx, app);
    show_missing_font_dialog(ctx, app);
    show_cdr_import_codepage_dialog(ctx, app);
    show_about_dialog(ctx, app);
}

fn show_print_preview_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.show_print_preview_dialog {
        return;
    }
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut open = app.show_print_preview_dialog;
    let mut print_now = false;
    let mut refresh_printers = false;
    egui::Window::new(crate::i18n::I18nKey::PrintPreviewTitle.tr_ui(lang))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(560.0, 440.0))
        .show(ctx, |ui| {
            shell_dir.with_vertical(ui, |ui| {
                ui.label(
                    RichText::new(crate::i18n::I18nKey::DlgPrintNativeQueue.tr_ui(lang)).strong(),
                );
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::PrintPrinterLabel.tr_ui(lang));
                    let current_printer =
                        app.print_job_config
                            .printer_name
                            .clone()
                            .unwrap_or_else(|| {
                                crate::i18n::I18nKey::DlgPrintDefaultPrinter.tr_ui(lang)
                            });
                    ComboBox::from_id_salt("print_os_printer_combo")
                        .width(260.0)
                        .selected_text(&current_printer)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(
                                    app.print_job_config.printer_name.is_none(),
                                    crate::i18n::I18nKey::DlgPrintDefaultPrinter.tr_ui(lang),
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
                                        app.print_job_config.printer_name.as_deref()
                                            == Some(&p.name),
                                        label,
                                    )
                                    .clicked()
                                {
                                    app.print_job_config.printer_name = Some(p.name.clone());
                                }
                            }
                        });
                    let refresh_lbl = if app.scanning_printers {
                        crate::i18n::I18nKey::DlgPrintScanning.tr_ui(lang)
                    } else {
                        crate::i18n::I18nKey::DlgPrintRefresh.tr_ui(lang)
                    };
                    if ui
                        .add_enabled(!app.scanning_printers, egui::Button::new(refresh_lbl))
                        .clicked()
                    {
                        refresh_printers = true;
                    }
                });

                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::PrintCopiesLabel.tr_ui(lang));
                    ui.add(DragValue::new(&mut app.print_job_config.copies).range(1..=999));
                    ui.label(crate::i18n::I18nKey::DocPagesLabel.tr_ui(lang));
                    ui.add(
                        egui::TextEdit::singleline(&mut app.print_job_config.page_range)
                            .desired_width(100.0)
                            .hint_text(crate::i18n::I18nKey::DlgPrintAllHint.tr_ui(lang)),
                    );
                });

                shell_dir.with_horizontal_wrapped(ui, |ui| {
                    ui.checkbox(
                        &mut app.print_job_config.force_cmyk,
                        crate::i18n::I18nKey::DlgPrintCmykOutput.tr_ui(lang),
                    );
                    ui.checkbox(
                        &mut app.print_job_config.fit_to_page,
                        crate::i18n::I18nKey::NavigatorZoomFit.tr_ui(lang),
                    );
                    ui.checkbox(
                        &mut app.print_job_config.crop_marks,
                        crate::i18n::I18nKey::DlgExportCropMarks.tr_ui(lang),
                    );
                    ui.checkbox(
                        &mut app.print_job_config.collate,
                        crate::i18n::I18nKey::DlgPrintCollate.tr_ui(lang),
                    );
                });

                shell_dir.with_horizontal(ui, |ui| {
                    if ui
                        .button(
                            RichText::new(crate::i18n::I18nKey::PrintSendToSpooler.tr_ui(lang))
                                .strong(),
                        )
                        .clicked()
                    {
                        print_now = true;
                    }
                    if let Some(ref status) = app.printers_status {
                        ui.label(RichText::new(status).size(11.0).color(tokens.text_muted));
                    }
                });

                ui.separator();
                ui.label(
                    RichText::new(crate::i18n::I18nKey::DlgPrintStudioTitle.tr_ui(lang)).strong(),
                );
                shell_dir.with_horizontal_wrapped(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::PrintImpositionMode.tr_ui(lang));
                    ui.selectable_value(
                        &mut app.production_state.imposition_kind,
                        0,
                        crate::i18n::I18nKey::DlgPrintImposNup.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut app.production_state.imposition_kind,
                        1,
                        crate::i18n::I18nKey::DlgPrintImposBooklet.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut app.production_state.imposition_kind,
                        2,
                        crate::i18n::I18nKey::DlgPrintImposTiling.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut app.production_state.imposition_kind,
                        3,
                        crate::i18n::I18nKey::DlgPrintImposPerfect.tr_ui(lang),
                    );
                });
                shell_dir.with_horizontal_wrapped(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::DlgPrintSheetW.tr_ui(lang));
                    ui.add(
                        DragValue::new(&mut app.production_state.sheet_w_mm)
                            .range(50.0..=5000.0)
                            .suffix(" mm"),
                    );
                    ui.label(crate::i18n::I18nKey::DlgPrintSheetH.tr_ui(lang));
                    ui.add(
                        DragValue::new(&mut app.production_state.sheet_h_mm)
                            .range(50.0..=5000.0)
                            .suffix(" mm"),
                    );
                    ui.label(crate::i18n::I18nKey::PrintScalePercent.tr_ui(lang));
                    if ui
                        .add(
                            DragValue::new(&mut app.production_state.print_scale_pct)
                                .range(10.0..=400.0)
                                .speed(1.0)
                                .suffix("%"),
                        )
                        .changed()
                    {
                        app.print_job_config.scale_pct = app.production_state.print_scale_pct;
                    }
                });
                if app.production_state.imposition_kind == 2 {
                    shell_dir.with_horizontal_wrapped(ui, |ui| {
                        ui.label(crate::i18n::I18nKey::PrintTileWidth.tr_ui(lang));
                        ui.add(
                            DragValue::new(&mut app.production_state.tile_w_mm)
                                .range(30.0..=5000.0)
                                .suffix(" mm"),
                        );
                        ui.label(crate::i18n::I18nKey::PrintTileHeight.tr_ui(lang));
                        ui.add(
                            DragValue::new(&mut app.production_state.tile_h_mm)
                                .range(30.0..=5000.0)
                                .suffix(" mm"),
                        );
                        ui.label(crate::i18n::I18nKey::PrintTileOverlap.tr_ui(lang));
                        let max_overlap = (app
                            .production_state
                            .tile_w_mm
                            .min(app.production_state.tile_h_mm)
                            - 1.0)
                            .max(0.0);
                        app.production_state.tile_overlap_mm =
                            app.production_state.tile_overlap_mm.clamp(0.0, max_overlap);
                        ui.add(
                            DragValue::new(&mut app.production_state.tile_overlap_mm)
                                .range(0.0..=max_overlap)
                                .speed(0.5)
                                .suffix(" mm"),
                        );
                    });
                }
                ui.separator();
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(
                        RichText::new(crate::i18n::I18nKey::DlgPrintLivePreview.tr_ui(lang))
                            .strong(),
                    );
                    ui.with_layout(
                        shell_dir.opposite_horizontal_layout(egui::Align::Center),
                        |ui| {
                            if ui
                                .small_button(crate::i18n::I18nKey::NavigatorResetView.tr_ui(lang))
                                .clicked()
                            {
                                app.print_preview_zoom = 1.0;
                                app.print_preview_pan = [0.0, 0.0];
                            }
                            ui.add(
                                DragValue::new(&mut app.print_preview_zoom)
                                    .range(0.5..=6.0)
                                    .speed(0.05)
                                    .prefix(crate::i18n::I18nKey::DlgPrintZoomPrefix.tr_ui(lang))
                                    .suffix("x"),
                            );
                        },
                    );
                });
                if let Some(sep) = app.active_tab().and_then(|t| t.separations_preview.clone()) {
                    shell_dir.with_horizontal_wrapped(ui, |ui| {
                        ui.label(crate::i18n::I18nKey::DlgPrintPlateLabel.tr_ui(lang));
                        if ui
                            .selectable_label(
                                app.print_preview_plate.is_none(),
                                crate::i18n::I18nKey::PrintSeparationsComposite.tr_ui(lang),
                            )
                            .clicked()
                        {
                            app.print_preview_plate = None;
                        }
                        for (p_idx, plate) in sep.plates.iter().enumerate() {
                            if ui
                                .selectable_label(
                                    app.print_preview_plate == Some(p_idx),
                                    &plate.name,
                                )
                                .clicked()
                            {
                                app.print_preview_plate = Some(p_idx);
                            }
                        }
                    });
                }
            });
            draw_print_preview_thumbnail(ui, app, &tokens);
            shell_dir.with_horizontal_wrapped(ui, |ui| {
                if ui
                    .button(crate::i18n::I18nKey::DlgPrintGenImposed.tr_ui(lang))
                    .clicked()
                {
                    app.run_imposition_active();
                }
                if ui
                    .button(crate::i18n::I18nKey::DlgPrintAddMarks.tr_ui(lang))
                    .clicked()
                {
                    app.add_print_marks_active();
                }
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
                if ui
                    .button(crate::i18n::I18nKey::DlgPrintApplyTrap.tr_ui(lang))
                    .clicked()
                {
                    app.apply_trapping_active();
                }
                if ui
                    .button(crate::i18n::I18nKey::DlgPrintHalftone.tr_ui(lang))
                    .clicked()
                {
                    app.apply_halftone_preview_active();
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
        app.refresh_system_printers_async();
    }
    if print_now {
        app.print_active_document_os();
    }
    app.show_print_preview_dialog = open;
}

fn print_preview_cache_key(
    app: &StudioApp,
    tab: &crate::state::DocumentTab,
    page_w: f32,
    page_h: f32,
    bleed_mm: f32,
) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    let mix = |h: &mut u64, bytes: &[u8]| {
        for &b in bytes {
            *h ^= u64::from(b);
            *h = h.wrapping_mul(0x100_0000_01b3);
        }
    };
    mix(&mut h, &tab.id.as_u128().to_le_bytes());
    mix(&mut h, &tab.document.spatial_revision().to_le_bytes());
    mix(
        &mut h,
        &(tab.document.active_page_index() as u64).to_le_bytes(),
    );
    mix(&mut h, &page_w.to_bits().to_le_bytes());
    mix(&mut h, &page_h.to_bits().to_le_bytes());
    mix(&mut h, &bleed_mm.to_bits().to_le_bytes());
    mix(&mut h, &[u8::from(app.print_job_config.force_cmyk)]);
    mix(&mut h, &[u8::from(app.soft_proof_cmyk)]);
    mix(&mut h, &(app.proof_profile as u8).to_le_bytes());
    mix(&mut h, &(app.proof_intent as u8).to_le_bytes());
    mix(
        &mut h,
        &app.print_preview_plate
            .map_or(u64::MAX, |i| i as u64)
            .to_le_bytes(),
    );
    if let Some(ref sep) = tab.separations_preview {
        mix(&mut h, &(sep.plates.len() as u64).to_le_bytes());
        mix(&mut h, &sep.width.to_le_bytes());
        mix(&mut h, &sep.height.to_le_bytes());
        if let Some(plate) = app.print_preview_plate.and_then(|idx| sep.plates.get(idx)) {
            mix(&mut h, plate.name.as_bytes());
            mix(&mut h, &plate.average_coverage_pct.to_bits().to_le_bytes());
            mix(&mut h, &plate.max_coverage_pct.to_bits().to_le_bytes());
            if let Some(fb) = plate.spot_fallback_cmyk {
                for c in fb {
                    mix(&mut h, &c.to_bits().to_le_bytes());
                }
            }
        }
    } else {
        mix(&mut h, &[0u8]);
    }
    h
}

fn rasterize_separation_plate_rgba(
    plate: &coree_prod::SeparationPlate,
    doc: &Document,
) -> Option<(u32, u32, Vec<u8>)> {
    let expected = (plate.width as usize).checked_mul(plate.height as usize)?;
    if plate.width == 0 || plate.height == 0 || plate.ink_map.len() != expected {
        return None;
    }
    let ink_color = match &plate.channel {
        coree_prod::SeparationChannel::Cyan => Color::cmyk(1.0, 0.0, 0.0, 0.0),
        coree_prod::SeparationChannel::Magenta => Color::cmyk(0.0, 1.0, 0.0, 0.0),
        coree_prod::SeparationChannel::Yellow => Color::cmyk(0.0, 0.0, 1.0, 0.0),
        coree_prod::SeparationChannel::Black => Color::cmyk(0.0, 0.0, 0.0, 1.0),
        coree_prod::SeparationChannel::Spot(name) => {
            let fb = plate.spot_fallback_cmyk.unwrap_or([0.0, 1.0, 0.0, 0.0]);
            Color::spot(name.clone(), 1.0, fb)
        }
    };
    let [ir, ig, ib, _] = ink_color.to_rgba_f32_icc(&doc.icc_pipeline());
    let ink_r = ir.clamp(0.0, 1.0) * 255.0;
    let ink_g = ig.clamp(0.0, 1.0) * 255.0;
    let ink_b = ib.clamp(0.0, 1.0) * 255.0;
    let mut rgba = Vec::with_capacity(expected * 4);
    for &density in &plate.ink_map {
        let d = f32::from(density) / 255.0;
        rgba.push((255.0 + (ink_r - 255.0) * d).round().clamp(0.0, 255.0) as u8);
        rgba.push((255.0 + (ink_g - 255.0) * d).round().clamp(0.0, 255.0) as u8);
        rgba.push((255.0 + (ink_b - 255.0) * d).round().clamp(0.0, 255.0) as u8);
        rgba.push(255);
    }
    Some((plate.width, plate.height, rgba))
}

fn draw_print_preview_thumbnail(
    ui: &mut egui::Ui,
    app: &mut StudioApp,
    tokens: &crate::theme::ThemeTokens,
) {
    let sheet_w = app.production_state.sheet_w_mm.max(10.0) as f32;
    let sheet_h = app.production_state.sheet_h_mm.max(10.0) as f32;
    let scale_pct = app.production_state.print_scale_pct.clamp(10.0, 400.0);
    let scale_factor = (scale_pct / 100.0) as f32;

    let (page_w, page_h, bleed_mm, preview_tex_id) = if let Some(tab) = app.active_tab() {
        let p = tab.document.active_page();
        let (pw, ph, bm) = p.map_or((210.0_f32, 297.0_f32, 3.0_f32), |pg| {
            (
                pg.width_mm.max(5.0) as f32,
                pg.height_mm.max(5.0) as f32,
                pg.bleed_mm.max(0.0) as f32,
            )
        });
        let cache_key = print_preview_cache_key(app, tab, pw, ph, bm);
        let mut tex_slot = app.print_preview_tex.borrow_mut();
        let cache_hit = tex_slot.as_ref().is_some_and(|(k, _)| *k == cache_key);
        if !cache_hit {
            let plate_opt = tab
                .separations_preview
                .as_ref()
                .and_then(|sep| app.print_preview_plate.and_then(|idx| sep.plates.get(idx)));
            let rendered_buf = plate_opt
                .and_then(|plate| rasterize_separation_plate_rgba(plate, &tab.document))
                .or_else(|| {
                    use coree_render::{
                        CanvasBackend, RenderOptions, SceneRenderer, TinySkiaBackend, ViewMode,
                        Viewport,
                    };
                    const PREVIEW_MAX_DIM: u32 = 512;
                    let max_mm = f64::from(pw).max(f64::from(ph)).max(1.0);
                    let scale_px_per_mm = f64::from(PREVIEW_MAX_DIM) / max_mm;
                    let target_w = (f64::from(pw) * scale_px_per_mm)
                        .round()
                        .clamp(16.0, f64::from(PREVIEW_MAX_DIM))
                        as u32;
                    let target_h = (f64::from(ph) * scale_px_per_mm)
                        .round()
                        .clamp(16.0, f64::from(PREVIEW_MAX_DIM))
                        as u32;
                    let raw_dpi = scale_px_per_mm * coree_core::math::MM_PER_INCH;
                    let clamped_dpi = raw_dpi.clamp(1.0, 4800.0);
                    let zoom = (raw_dpi / clamped_dpi).clamp(0.01, 256.0);
                    let mut backend = TinySkiaBackend::new(target_w, target_h).ok()?;
                    let mut viewport = Viewport::for_export(target_w, target_h, clamped_dpi);
                    viewport.zoom = zoom;
                    let mut doc_view = tab.document.clone();
                    doc_view.set_multipage_view(false);
                    let mut opts = RenderOptions::export();
                    opts.soft_proof_cmyk = app.print_job_config.force_cmyk || app.soft_proof_cmyk;
                    opts.proof_profile = app.proof_profile;
                    opts.proof_intent = app.proof_intent;
                    let mut renderer = SceneRenderer::new();
                    renderer.render_document(
                        &doc_view,
                        &viewport,
                        ViewMode::Enhanced,
                        &opts,
                        &mut backend,
                    );
                    let buf = backend.into_rgba();
                    Some((buf.width, buf.height, buf.rgba))
                });
            if let Some((w, h, rgba)) = rendered_buf {
                let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                if let Some((ref mut cached_k, ref mut handle)) = *tex_slot {
                    *cached_k = cache_key;
                    handle.set(img, egui::TextureOptions::LINEAR);
                } else {
                    let handle = ui.ctx().load_texture(
                        "coree_print_preview",
                        img,
                        egui::TextureOptions::LINEAR,
                    );
                    *tex_slot = Some((cache_key, handle));
                }
            }
        }
        (pw, ph, bm, tex_slot.as_ref().map(|(_, h)| h.id()))
    } else {
        (210.0, 297.0, 3.0, None)
    };

    let avail_w = ui.available_width().clamp(260.0, 480.0);
    let (resp, painter) = ui.allocate_painter(vec2(avail_w, 190.0), Sense::click_and_drag());
    if resp.dragged() {
        let d = resp.drag_delta();
        app.print_preview_pan[0] = (app.print_preview_pan[0] + d.x).clamp(-300.0, 300.0);
        app.print_preview_pan[1] = (app.print_preview_pan[1] + d.y).clamp(-300.0, 300.0);
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.1 {
            app.print_preview_zoom =
                (app.print_preview_zoom * (1.0 + scroll * 0.002)).clamp(0.5, 6.0);
        }
    }
    painter.rect_filled(resp.rect, CornerRadius::same(4), tokens.canvas_well);
    painter.rect_stroke(
        resp.rect,
        CornerRadius::same(4),
        Stroke::new(1.0, tokens.border),
        StrokeKind::Inside,
    );

    let inner = resp.rect.shrink2(vec2(18.0, 14.0));
    let scale = (inner.width() / sheet_w)
        .min(inner.height() / sheet_h)
        .max(1e-3)
        * app.print_preview_zoom.clamp(0.5, 6.0);
    let sheet_px_size = vec2(sheet_w * scale, sheet_h * scale);
    let sheet_rect = Rect::from_center_size(
        inner.center() + vec2(app.print_preview_pan[0], app.print_preview_pan[1]),
        sheet_px_size,
    );
    let well_painter = painter.with_clip_rect(resp.rect);

    well_painter.rect_filled(
        sheet_rect.translate(vec2(2.5, 2.5)),
        CornerRadius::ZERO,
        Color32::from_black_alpha(70),
    );
    well_painter.rect_filled(
        sheet_rect,
        CornerRadius::ZERO,
        Color32::from_rgb(248, 249, 251),
    );
    well_painter.rect_stroke(
        sheet_rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, Color32::from_rgb(130, 140, 155)),
        StrokeKind::Outside,
    );

    let draw_slot_contents = |slot_rect: Rect| {
        if bleed_mm > 0.01 && page_w > 0.0 && page_h > 0.0 {
            let bx = (bleed_mm / page_w) * slot_rect.width();
            let by = (bleed_mm / page_h) * slot_rect.height();
            let bleed_rect = slot_rect.expand2(vec2(bx.min(8.0), by.min(8.0)));
            well_painter.rect_stroke(
                bleed_rect,
                CornerRadius::ZERO,
                Stroke::new(0.9, Color32::from_rgb(220, 65, 85)),
                StrokeKind::Inside,
            );
        }
        if let Some(tex_id) = preview_tex_id {
            well_painter.image(
                tex_id,
                slot_rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        // Safe inner margin (5mm visual guide)
        let safe_inset = ((5.0 / page_w.max(page_h)) * slot_rect.width().min(slot_rect.height()))
            .clamp(1.5, 10.0);
        if slot_rect.width() > 14.0 && slot_rect.height() > 14.0 {
            well_painter.rect_stroke(
                slot_rect.shrink(safe_inset),
                CornerRadius::ZERO,
                Stroke::new(0.7, Color32::from_rgba_unmultiplied(40, 130, 210, 140)),
                StrokeKind::Inside,
            );
        }
        well_painter.rect_stroke(
            slot_rect,
            CornerRadius::ZERO,
            Stroke::new(1.0, Color32::from_rgb(55, 65, 80)),
            StrokeKind::Inside,
        );
        if app.print_job_config.crop_marks {
            let tick = 5.0;
            let gap = 1.5;
            let mc = Stroke::new(1.0, Color32::from_rgb(15, 25, 40));
            for (cx, cy, sx, sy) in [
                (slot_rect.left(), slot_rect.top(), -1.0_f32, -1.0_f32),
                (slot_rect.right(), slot_rect.top(), 1.0, -1.0),
                (slot_rect.left(), slot_rect.bottom(), -1.0, 1.0),
                (slot_rect.right(), slot_rect.bottom(), 1.0, 1.0),
            ] {
                let p = pos2(cx, cy);
                well_painter.line_segment(
                    [p + vec2(sx * gap, 0.0), p + vec2(sx * (gap + tick), 0.0)],
                    mc,
                );
                well_painter.line_segment(
                    [p + vec2(0.0, sy * gap), p + vec2(0.0, sy * (gap + tick))],
                    mc,
                );
            }
        }
    };

    match app.production_state.imposition_kind {
        1 | 3 => {
            // 2-Up Booklet / Perfect-Bound signature spread
            let margin_px = (6.0 * scale).clamp(3.0, 12.0);
            let work = sheet_rect.shrink(margin_px);
            let half_w = (work.width() * 0.5).max(4.0);
            let s_scale = (half_w / page_w).min(work.height() / page_h).max(1e-3);
            let pw_px = page_w * s_scale * scale_factor;
            let ph_px = page_h * s_scale * scale_factor;
            let top = work.center().y - ph_px * 0.5;
            let left_slot =
                Rect::from_min_size(pos2(work.center().x - pw_px, top), vec2(pw_px, ph_px));
            let right_slot = Rect::from_min_size(pos2(work.center().x, top), vec2(pw_px, ph_px));
            draw_slot_contents(left_slot);
            draw_slot_contents(right_slot);
            well_painter.line_segment(
                [
                    pos2(work.center().x, sheet_rect.top()),
                    pos2(work.center().x, sheet_rect.bottom()),
                ],
                Stroke::new(1.2, Color32::from_rgb(200, 70, 50)),
            );
        }
        2 => {
            // Large-Format Tiling grid
            let margin_px = (4.0 * scale).clamp(2.0, 10.0);
            let work = sheet_rect.shrink(margin_px);
            let scaled_w = page_w * scale_factor;
            let scaled_h = page_h * scale_factor;
            let p_scale = (work.width() / scaled_w)
                .min(work.height() / scaled_h)
                .max(1e-3);
            let art_rect =
                Rect::from_center_size(work.center(), vec2(scaled_w * p_scale, scaled_h * p_scale));
            draw_slot_contents(art_rect);
            let tile_w_mm = app.production_state.tile_w_mm.max(10.0) as f32;
            let tile_h_mm = app.production_state.tile_h_mm.max(10.0) as f32;
            let ov_mm = (app.production_state.tile_overlap_mm.max(0.0) as f32)
                .min((tile_w_mm.min(tile_h_mm) - 1.0).max(0.0));
            let tw = tile_w_mm * p_scale;
            let th = tile_h_mm * p_scale;
            let ov = ov_mm * p_scale;
            let step_x = (tw - ov).max(8.0);
            let step_y = (th - ov).max(8.0);
            let mut x = art_rect.left();
            while x < art_rect.right() - 1.0 {
                let mut y = art_rect.top();
                while y < art_rect.bottom() - 1.0 {
                    let tile = Rect::from_min_size(pos2(x, y), vec2(tw, th)).intersect(art_rect);
                    well_painter.rect_stroke(
                        tile,
                        CornerRadius::ZERO,
                        Stroke::new(1.0, Color32::from_rgb(25, 130, 195)),
                        StrokeKind::Inside,
                    );
                    y += step_y;
                }
                x += step_x;
            }
        }
        _ => {
            // N-Up Grid
            let cols = app.production_state.nup_cols.clamp(1, 16) as usize;
            let rows = app.production_state.nup_rows.clamp(1, 16) as usize;
            let margin_px = (6.0 * scale).clamp(3.0, 12.0);
            let work = sheet_rect.shrink(margin_px);
            let cell_w = work.width() / cols as f32;
            let cell_h = work.height() / rows as f32;
            let fit_scale = ((cell_w - 4.0).max(2.0) / page_w)
                .min((cell_h - 4.0).max(2.0) / page_h)
                .max(1e-3);
            let slot_size = vec2(
                page_w * fit_scale * scale_factor,
                page_h * fit_scale * scale_factor,
            );
            for r in 0..rows {
                for c in 0..cols {
                    let cell = Rect::from_min_size(
                        pos2(
                            work.left() + c as f32 * cell_w,
                            work.top() + r as f32 * cell_h,
                        ),
                        vec2(cell_w, cell_h),
                    );
                    let slot_rect = Rect::from_center_size(cell.center(), slot_size);
                    draw_slot_contents(slot_rect);
                }
            }
        }
    }

    let summary = format!(
        "Sheet {sheet_w:.0}×{sheet_h:.0} mm · Trim {page_w:.0}×{page_h:.0} mm · Scale {scale_pct:.0}% · Bleed {bleed_mm:.1} mm · {}",
        if app.print_job_config.force_cmyk {
            "CMYK Press"
        } else {
            "RGB Output"
        }
    );
    painter.text(
        pos2(resp.rect.center().x, resp.rect.bottom() - 4.0),
        Align2::CENTER_BOTTOM,
        summary,
        FontId::proportional(10.0),
        tokens.text_muted,
    );
}

fn show_about_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.show_about_dialog {
        return;
    }
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut open = app.show_about_dialog;
    let brand = BrandInfo::current();
    egui::Window::new(format!(
        "{} {}",
        crate::i18n::I18nKey::DlgAboutTitlePrefix.tr_ui(lang),
        brand.display_name
    ))
    .open(&mut open)
    .collapsible(false)
    .resizable(false)
    .fixed_size(egui::vec2(480.0, 290.0))
    .show(ctx, |ui| {
        shell_dir.with_vertical(ui, |ui| {
            ui.label(
                RichText::new(format!("{} v{}", brand.display_name, brand.version))
                    .size(20.0)
                    .strong(),
            );
            ui.label(format!(
                "License: {}  |  Repository: {}",
                brand.license, brand.repo_url
            ));
            ui.label(crate::i18n::I18nKey::DlgAboutPurity.tr_ui(lang));
            ui.separator();
            ui.label(RichText::new(crate::i18n::I18nKey::DlgAboutCleanRoom.tr_ui(lang)).strong());
            ui.label(brand.trademark_notice);
        });
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

pub(super) fn rendering_intent_label(intent: RenderingIntent) -> &'static str {
    match intent {
        RenderingIntent::Perceptual => "Perceptual (Photographic)",
        RenderingIntent::RelativeColorimetric => "Relative Colorimetric (Print Standard)",
        RenderingIntent::Saturation => "Saturation (Business Graphics)",
        RenderingIntent::AbsoluteColorimetric => "Absolute Colorimetric (Hard Proof)",
    }
}

pub(super) fn gcode_dialect_label(dialect: GcodeDialect) -> &'static str {
    match dialect {
        GcodeDialect::Grbl11 => "GRBL 1.1+ Laser Mode",
        GcodeDialect::Marlin => "Marlin Laser Firmware",
        GcodeDialect::RuidaGcode => "Ruida G-Code Dialect",
    }
}

pub(super) fn edit_color_compact_sliders(
    ui: &mut egui::Ui,
    color: &mut Color,
    id_salt: &str,
    lang: crate::i18n::StudioLanguage,
) -> bool {
    let mut changed = false;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
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

    shell_dir.with_horizontal(ui, |ui| {
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
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DlgFillRgbLabel.tr_ui(lang));
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
            shell_dir.with_horizontal(ui, |ui| {
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
            shell_dir.with_horizontal(ui, |ui| {
                let is_cut = matches!(color, Color::Spot { name, .. } if name == "CutContour");
                if ui
                    .selectable_label(is_cut, crate::i18n::I18nKey::DlgFillSpotCut.tr_ui(lang))
                    .clicked()
                {
                    *color = Color::spot("CutContour", 1.0, [0.0, 1.0, 0.0, 0.0]);
                    changed = true;
                }
                if ui
                    .small_button(crate::i18n::I18nKey::DlgFillKeyBlack.tr_ui(lang))
                    .clicked()
                {
                    *color = Color::KEY_BLACK;
                    changed = true;
                }
                if ui
                    .small_button(crate::i18n::I18nKey::DlgFillWhite.tr_ui(lang))
                    .clicked()
                {
                    *color = Color::WHITE;
                    changed = true;
                }
                let _ = id_salt;
            });
        });
    });
    changed
}

/// Interactive `CRLFOM` modal missing-font substitution dialog with `PANOSE` suggestions (`REQ-07`).
fn show_missing_font_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.missing_font_dialog.open {
        return;
    }
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut open = app.missing_font_dialog.open;
    let mut apply_now = false;
    let mut skip_now = false;
    let doc_title = app.missing_font_dialog.document_title.clone();
    egui::Window::new(crate::i18n::I18nKey::MissingFontDialogTitle.tr_ui(lang))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(vec2(520.0, 320.0))
        .show(ctx, |ui| {
            shell_dir.with_vertical(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} ({doc_title})",
                        crate::i18n::I18nKey::MissingFontDialogDesc.tr_ui(lang)
                    ))
                    .color(tokens.text_muted),
                );
                ui.add_space(6.0);
                for (idx, entry) in app.missing_font_dialog.entries.iter_mut().enumerate() {
                    Frame::NONE
                        .fill(tokens.surface)
                        .stroke(Stroke::new(1.0, tokens.border))
                        .corner_radius(CornerRadius::same(4))
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            shell_dir.with_horizontal(ui, |ui| {
                                ui.label(
                                    RichText::new(&entry.missing_family)
                                        .strong()
                                        .color(tokens.warning),
                                );
                                ui.label("->");
                                ui.add(
                                    egui::TextEdit::singleline(&mut entry.selected_family)
                                        .id_salt(("crlfom_sub", idx))
                                        .desired_width(170.0),
                                );
                                let dist_txt = entry.panose_distance.map_or_else(
                                    || crate::i18n::I18nKey::MissingFontSuggestedPanose.tr_ui(lang),
                                    |d| format!("PANOSE Δ={d}"),
                                );
                                ui.label(RichText::new(dist_txt).size(10.5).color(tokens.accent));
                            });
                            shell_dir.with_horizontal_wrapped(ui, |ui| {
                                for cand in [
                                    entry.suggested_family.as_str(),
                                    "Noto Sans",
                                    "Noto Sans Arabic",
                                    "Noto Naskh Arabic",
                                    "DejaVu Sans",
                                ] {
                                    if ui.small_button(cand).clicked() {
                                        entry.selected_family = cand.to_owned();
                                    }
                                }
                            });
                        });
                    ui.add_space(4.0);
                }
                ui.checkbox(
                    &mut app.missing_font_dialog.remember_rules,
                    crate::i18n::I18nKey::MissingFontRememberRule.tr_ui(lang),
                );
                ui.separator();
                shell_dir.with_horizontal(ui, |ui| {
                    if ui
                        .button(
                            RichText::new(crate::i18n::I18nKey::MissingFontApplyAll.tr_ui(lang))
                                .strong(),
                        )
                        .clicked()
                    {
                        apply_now = true;
                    }
                    if ui
                        .button(crate::i18n::I18nKey::MissingFontSkip.tr_ui(lang))
                        .clicked()
                    {
                        skip_now = true;
                    }
                });
            });
        });
    if apply_now {
        app.apply_missing_font_dialog_substitutions();
    } else if skip_now || !open {
        app.missing_font_dialog.open = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CdrCodepage {
    Auto,
    Cp1256,
    Cp1251,
    Cp1252,
    Utf8,
}

impl CdrCodepage {
    const ALL: [Self; 5] = [
        Self::Auto,
        Self::Cp1256,
        Self::Cp1251,
        Self::Cp1252,
        Self::Utf8,
    ];

    const fn codepage_id(self) -> Option<u16> {
        match self {
            Self::Auto => None,
            Self::Cp1256 => Some(1256),
            Self::Cp1251 => Some(1251),
            Self::Cp1252 => Some(1252),
            Self::Utf8 => Some(65001),
        }
    }

    const fn i18n_key(self) -> crate::i18n::I18nKey {
        match self {
            Self::Auto => crate::i18n::I18nKey::CdrCodepageAuto,
            Self::Cp1256 => crate::i18n::I18nKey::CdrCodepageCp1256,
            Self::Cp1251 => crate::i18n::I18nKey::CdrCodepageCp1251,
            Self::Cp1252 => crate::i18n::I18nKey::CdrCodepageCp1252,
            Self::Utf8 => crate::i18n::I18nKey::CdrCodepageUtf8,
        }
    }

    const fn preview_hint(self) -> &'static str {
        match self {
            Self::Auto => "Auto-detect codepage from RIFF font/stlt/txsm chunks",
            Self::Cp1256 => "Preview fallback: Windows-1256 (Arabic) legacy single-byte decoding",
            Self::Cp1251 => "Preview fallback: Windows-1251 (Cyrillic) legacy single-byte decoding",
            Self::Cp1252 => "Preview fallback: Windows-1252 (Western Latin) single-byte decoding",
            Self::Utf8 => "Preview fallback: UTF-8 multi-byte decoding",
        }
    }
}

/// Interactive `S15` `.cdr` legacy codepage selector modal (`CP1256` / `CP1251` / `CP1252` / `UTF-8`).
fn show_cdr_import_codepage_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.cdr_import_dialog.open {
        return;
    }
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut open = app.cdr_import_dialog.open;
    let mut open_now = false;
    let mut cancel_now = false;
    egui::Window::new(crate::i18n::I18nKey::CdrImportCodepageTitle.tr_ui(lang))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(vec2(460.0, 240.0))
        .show(ctx, |ui| {
            shell_dir.with_vertical(ui, |ui| {
                let path_txt = app.cdr_import_dialog.pending_path.as_ref().map_or_else(
                    || crate::i18n::I18nKey::DlgCdrSelectFilePlaceholder.tr_ui(lang),
                    |p| p.display().to_string(),
                );
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(RichText::new(path_txt).monospace().size(11.0));
                    if ui
                        .small_button(crate::i18n::I18nKey::ExportBrowseButton.tr_ui(lang))
                        .clicked()
                    {
                        if let Some(p) = rfd::FileDialog::new()
                            .set_title("Choose .cdr File")
                            .add_filter("CDR / CDT / CMX Vector Files", &["cdr", "cdt", "cmx"])
                            .pick_file()
                        {
                            app.cdr_import_dialog.pending_path = Some(p);
                        }
                    }
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(crate::i18n::I18nKey::DlgCdrLegacyEncLabel.tr_ui(lang)).strong(),
                );
                shell_dir.with_horizontal_wrapped(ui, |ui| {
                    for cp_enum in CdrCodepage::ALL {
                        let cp = cp_enum.codepage_id();
                        if ui
                            .selectable_value(
                                &mut app.cdr_import_dialog.selected_codepage,
                                cp,
                                cp_enum.i18n_key().tr_ui(lang),
                            )
                            .clicked()
                            || app.cdr_import_dialog.preview_summary.is_empty()
                                && app.cdr_import_dialog.selected_codepage == cp
                        {
                            app.cdr_import_dialog.preview_summary =
                                cp_enum.preview_hint().to_owned();
                        }
                    }
                });
                if !app.cdr_import_dialog.preview_summary.is_empty() {
                    ui.label(
                        RichText::new(&app.cdr_import_dialog.preview_summary)
                            .size(10.5)
                            .color(tokens.text_muted),
                    );
                }
                ui.separator();
                shell_dir.with_horizontal(ui, |ui| {
                    let has_path = app.cdr_import_dialog.pending_path.is_some();
                    if ui
                        .add_enabled(
                            has_path,
                            egui::Button::new(
                                RichText::new(crate::i18n::I18nKey::WelcomeOpenButton.tr_ui(lang))
                                    .strong(),
                            ),
                        )
                        .clicked()
                    {
                        open_now = true;
                    }
                    if ui
                        .button(crate::i18n::I18nKey::DialogCancel.tr_ui(lang))
                        .clicked()
                    {
                        cancel_now = true;
                    }
                });
            });
        });
    if open_now {
        if let Some(path) = app.cdr_import_dialog.pending_path.clone() {
            let cp = app.cdr_import_dialog.selected_codepage;
            match app.open_file_path_with_codepage(&path, cp) {
                Ok(()) => app.cdr_import_dialog.open = false,
                Err(err) => app.push_toast(err, true),
            }
        }
    } else if cancel_now || !open {
        app.cdr_import_dialog.open = false;
    }
}
