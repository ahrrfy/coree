// SPDX-License-Identifier: MIT OR Apache-2.0

//! Welcome & Recovery Hub (`S01`): New Document templates, Studio Workspace presets,
//! Showcase Sample, Open file across all supported vector/CAD/print/raster formats,
//! SQLite Recent Documents list, and Crash Recovery Banner when unclean sessions exist.

use crate::state::{StudioApp, WorkspacePreset};
use coree_core::brand::BrandInfo;
use coree_core::{ColorMode, Document};
use egui::{CornerRadius, Frame, Margin, RichText, Stroke, Ui};
use std::path::PathBuf;

/// Renders the Welcome & Crash Recovery Hub inside the central workspace area.
pub fn show_welcome_hub(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let brand = BrandInfo::current();

    egui::CentralPanel::default()
        .frame(
            Frame::NONE
                .fill(tokens.bg)
                .inner_margin(Margin::same(24)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.set_max_width(1020.0);

                // Header
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(format!("{} Studio v{}", brand.display_name, brand.version))
                                .size(28.0)
                                .strong()
                                .color(tokens.text),
                        );
                        ui.label(
                            RichText::new(format!(
                                "100% Offline Pure-Rust Vector Graphics, Multipage Layout, Laser & Print Production Suite ({})",
                                brand.license
                            ))
                            .size(13.0)
                            .color(tokens.text_muted),
                        );
                        ui.label(
                            RichText::new(
                                "Native & Exchange Formats: CDR X6–2025 · CDT · CMX · CPT · PDF/AI · EPS · DXF · PLT · PSD · SVG · G-Code · coree",
                            )
                            .size(11.5)
                            .color(tokens.accent),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !app.tabs.is_empty() && ui.button("Return to Canvas →").clicked() {
                            app.show_welcome = false;
                        }
                    });
                });

                ui.add_space(16.0);

                // Crash Recovery Banner (when unclean sessions exist in SQLite WAL)
                if !app.unclean_sessions.is_empty() {
                    let sessions = app.unclean_sessions.clone();
                    Frame::NONE
                        .fill(tokens.surface_elevated)
                        .stroke(Stroke::new(1.5, tokens.accent_secondary))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(14))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "⚠ Crash Recovery Available ({} unsaved session(s))",
                                    sessions.len()
                                ))
                                .size(15.0)
                                .strong()
                                .color(tokens.accent_secondary),
                            );
                            ui.label(
                                RichText::new(
                                    "Deterministic snapshots + WAL command logs were preserved locally with SHA-256 verification.",
                                )
                                .size(12.0)
                                .color(tokens.text_muted),
                            );
                            ui.add_space(8.0);
                            for sess in &sessions {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{} ({} journal op(s), updated {})",
                                            sess.display_name,
                                            sess.pending_journal_ops,
                                            sess.updated_at
                                        ))
                                        .color(tokens.text),
                                    );
                                    if ui
                                        .button(
                                            RichText::new("Restore Session").color(tokens.success),
                                        )
                                        .clicked()
                                    {
                                        if let Err(e) = app.recover_unclean_session(
                                            &sess.document_id,
                                            &sess.session_id,
                                        ) {
                                            app.push_toast(e, true);
                                        }
                                    }
                                    if ui
                                        .button(RichText::new("Discard").color(tokens.danger))
                                        .clicked()
                                    {
                                        app.discard_unclean_session(
                                            &sess.document_id,
                                            &sess.session_id,
                                        );
                                    }
                                });
                            }
                        });
                    ui.add_space(16.0);
                }

                // Quick Actions Row
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("＋ New Document...")
                                    .size(14.0)
                                    .color(tokens.on_accent),
                            )
                            .fill(tokens.accent),
                        )
                        .clicked()
                    {
                        app.new_doc_dialog.open = true;
                    }
                    if ui
                        .button(
                            RichText::new("📂 Open File (CDR / PDF / AI / EPS / DXF / PLT / PSD / SVG)")
                                .size(13.5),
                        )
                        .clicked()
                    {
                        app.open_file_dialog();
                    }
                    if ui
                        .button(RichText::new("★ Open Showcase Sample").size(13.5))
                        .clicked()
                    {
                        app.new_sample_document();
                    }
                    if ui
                        .button(RichText::new("⚙ Studio Options (Ctrl+J)").size(13.0))
                        .clicked()
                    {
                        app.show_options_dialog = true;
                    }
                });

                ui.add_space(20.0);

                // Quick Workspace Preset Selector Cards
                ui.label(
                    RichText::new("Studio Workspaces")
                        .size(15.0)
                        .strong()
                        .color(tokens.text),
                );
                ui.label(
                    RichText::new("Pick a classic studio layout tailored to vector illustration, page layout, or laser & sign production:")
                        .size(11.5)
                        .color(tokens.text_muted),
                );
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    for preset in WorkspacePreset::ALL {
                        let active = app.active_workspace_preset == preset;
                        Frame::NONE
                            .fill(if active {
                                tokens.surface_elevated
                            } else {
                                tokens.surface
                            })
                            .stroke(Stroke::new(
                                if active { 1.5 } else { 1.0 },
                                if active { tokens.accent } else { tokens.border },
                            ))
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_min_width(154.0);
                                ui.set_max_width(154.0);
                                ui.label(
                                    RichText::new(preset.label())
                                        .strong()
                                        .color(if active { tokens.accent } else { tokens.text }),
                                );
                                ui.label(
                                    RichText::new(preset.description())
                                        .size(10.5)
                                        .color(tokens.text_muted),
                                );
                                ui.add_space(4.0);
                                if ui
                                    .small_button(if active { "✓ Active" } else { "Select" })
                                    .clicked()
                                {
                                    app.apply_workspace_preset(preset);
                                }
                            });
                    }
                });

                ui.add_space(20.0);

                // Quick Document Templates
                ui.label(
                    RichText::new("Quick Start Templates")
                        .size(15.0)
                        .strong()
                        .color(tokens.text),
                );
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    let templates = [
                        ("A4 Print (CMYK)", 210.0, 297.0, ColorMode::Cmyk, 300.0),
                        ("A3 Poster (CMYK)", 297.0, 420.0, ColorMode::Cmyk, 300.0),
                        ("Business Card", 85.0, 55.0, ColorMode::Cmyk, 300.0),
                        ("Laser & Vinyl Bed", 300.0, 200.0, ColorMode::Rgb, 300.0),
                        ("Packaging Box Sheet", 320.0, 450.0, ColorMode::Cmyk, 300.0),
                        ("Web 1920×1080 (RGB)", 508.0, 285.75, ColorMode::Rgb, 96.0),
                    ];
                    for (name, w, h, mode, dpi) in templates {
                        Frame::NONE
                            .fill(tokens.surface)
                            .stroke(Stroke::new(1.0, tokens.border))
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_min_width(154.0);
                                ui.label(RichText::new(name).strong().color(tokens.text));
                                ui.label(
                                    RichText::new(format!("{w:.0} × {h:.0} mm · {mode:?} {dpi:.0} DPI"))
                                        .size(11.0)
                                        .color(tokens.text_muted),
                                );
                                ui.add_space(6.0);
                                if ui.small_button("Create").clicked() {
                                    app.new_document(Document::new(name, w, h, mode, dpi));
                                }
                            });
                    }
                });

                ui.add_space(20.0);

                // Recent Documents from SQLite
                ui.label(
                    RichText::new("Recent Documents")
                        .size(15.0)
                        .strong()
                        .color(tokens.text),
                );
                ui.add_space(6.0);
                let recents = app.recent_docs.clone();
                if recents.is_empty() {
                    ui.label(
                        RichText::new("No recent local documents yet. Open or save a file to populate this list.")
                            .color(tokens.text_muted),
                    );
                } else {
                    for rec in &recents {
                        Frame::NONE
                            .fill(tokens.surface)
                            .stroke(Stroke::new(1.0, tokens.border))
                            .corner_radius(CornerRadius::same(5))
                            .inner_margin(Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(rec.file_format.to_uppercase())
                                            .size(10.5)
                                            .strong()
                                            .color(tokens.accent),
                                    );
                                    ui.label(
                                        RichText::new(&rec.display_name)
                                            .strong()
                                            .color(tokens.text),
                                    );
                                    ui.label(
                                        RichText::new(&rec.file_path)
                                            .size(11.0)
                                            .color(tokens.text_muted),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("Open").clicked() {
                                                let pb = PathBuf::from(&rec.file_path);
                                                if let Err(err) = app.open_file_path(&pb) {
                                                    app.push_toast(err, true);
                                                }
                                            }
                                        },
                                    );
                                });
                            });
                        ui.add_space(4.0);
                    }
                }
            });
        });
}
