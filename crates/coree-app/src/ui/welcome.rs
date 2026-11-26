// SPDX-License-Identifier: MIT OR Apache-2.0

//! Welcome & Recovery Hub (`S01`): New Document templates, Studio Workspace presets,
//! Showcase Sample, Open file across all supported vector/CAD/print/raster formats,
//! SQLite Recent Documents list, and Crash Recovery Banner when unclean sessions exist.

use crate::i18n::{workspace_i18n_key, I18nKey, ShellLayoutDirections};
use crate::state::{StudioApp, WorkspacePreset};
use coree_core::brand::BrandInfo;
use coree_core::{ColorMode, Document};
use egui::{CornerRadius, Frame, Margin, RichText, Stroke, Ui};
use std::path::PathBuf;

/// Renders the Welcome & Crash Recovery Hub inside the central workspace area.
pub fn show_welcome_hub(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let brand = BrandInfo::current();
    let lang = app.ui_language;
    let shell_dir = ShellLayoutDirections::from_language(lang);

    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(tokens.bg).inner_margin(Margin::same(24)))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.set_max_width(1020.0);

                // Header
                shell_dir.with_horizontal(ui, |ui| {
                    shell_dir.with_vertical(ui, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} v{}",
                                I18nKey::AppTitle.tr_ui(lang),
                                brand.version
                            ))
                            .size(28.0)
                            .strong()
                            .color(tokens.text),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} ({})",
                                I18nKey::WelcomeSuiteSummary.tr_ui(lang),
                                brand.license
                            ))
                            .size(13.0)
                            .color(tokens.text_muted),
                        );
                        ui.label(
                            RichText::new(I18nKey::WelcomeSupportedFormats.tr_ui(lang))
                                .size(11.5)
                                .color(tokens.accent),
                        );
                    });
                    shell_dir.with_opposite_horizontal(ui, |ui| {
                        if !app.tabs.is_empty()
                            && ui.button(I18nKey::ReturnToCanvas.tr_ui(lang)).clicked()
                        {
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
                                    "⚠ {} ({})",
                                    I18nKey::CrashRecoveryBanner.tr_ui(lang),
                                    sessions.len()
                                ))
                                .size(15.0)
                                .strong()
                                .color(tokens.accent_secondary),
                            );
                            ui.label(
                                RichText::new(I18nKey::WelcomeRecoveryDesc.tr_ui(lang))
                                    .size(12.0)
                                    .color(tokens.text_muted),
                            );
                            ui.add_space(8.0);
                            for sess in &sessions {
                                shell_dir.with_horizontal(ui, |ui| {
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
                                            RichText::new(I18nKey::RestoreSession.tr_ui(lang))
                                                .color(tokens.success),
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
                                        .button(
                                            RichText::new(I18nKey::DiscardSession.tr_ui(lang))
                                                .color(tokens.danger),
                                        )
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
                shell_dir.with_horizontal_wrapped(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("＋ {}", I18nKey::NewDocument.tr_ui(lang)))
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
                            RichText::new(format!("📂 {}", I18nKey::OpenDocument.tr_ui(lang)))
                                .size(13.5),
                        )
                        .clicked()
                    {
                        app.open_file_dialog();
                    }
                    if ui
                        .button(
                            RichText::new(format!("★ {}", I18nKey::OpenSampleShowcase.tr_ui(lang)))
                                .size(13.5),
                        )
                        .clicked()
                    {
                        app.new_sample_document();
                    }
                    if ui
                        .button(
                            RichText::new(format!("⚙ {}", I18nKey::ToolsOptions.tr_ui(lang)))
                                .size(13.0),
                        )
                        .clicked()
                    {
                        app.show_options_dialog = true;
                    }
                });

                ui.add_space(20.0);

                // Quick Workspace Preset Selector Cards
                ui.label(
                    RichText::new(I18nKey::WindowWorkspaces.tr_ui(lang))
                        .size(15.0)
                        .strong()
                        .color(tokens.text),
                );
                ui.label(
                    RichText::new(I18nKey::AppSubtitle.tr_ui(lang))
                        .size(11.5)
                        .color(tokens.text_muted),
                );
                ui.add_space(6.0);
                shell_dir.with_horizontal_wrapped_align(ui, egui::Align::Min, |ui| {
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
                                    RichText::new(workspace_i18n_key(preset).tr_ui(lang))
                                        .strong()
                                        .color(if active { tokens.accent } else { tokens.text }),
                                );
                                ui.label(
                                    RichText::new(preset.description())
                                        .size(10.5)
                                        .color(tokens.text_muted),
                                );
                                ui.add_space(4.0);
                                let btn_lbl = if active {
                                    format!("✓ {}", I18nKey::WelcomeActivePreset.tr_ui(lang))
                                } else {
                                    I18nKey::WelcomeSelectPreset.tr_ui(lang)
                                };
                                if ui.small_button(btn_lbl).clicked() {
                                    app.apply_workspace_preset(preset);
                                }
                            });
                    }
                });

                ui.add_space(20.0);

                // Quick Document Templates (SQLite `templates` + built-ins + `.cdt`)
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(
                        RichText::new(I18nKey::QuickStartTemplates.tr_ui(lang))
                            .size(15.0)
                            .strong()
                            .color(tokens.text),
                    );
                    ui.add_space(12.0);
                    for (cat, label) in [
                        (None, I18nKey::TemplateCategoryAll.tr_ui(lang)),
                        (Some("print"), I18nKey::TemplateCategoryPrint.tr_ui(lang)),
                        (
                            Some("packaging"),
                            I18nKey::TemplateCategoryPackaging.tr_ui(lang),
                        ),
                        (Some("web-ui"), I18nKey::TemplateCategoryDigital.tr_ui(lang)),
                        (Some("custom"), I18nKey::TemplateCategoryLaser.tr_ui(lang)),
                    ] {
                        let active = app.template_category_filter.as_deref() == cat;
                        if ui.selectable_label(active, label).clicked() {
                            app.template_category_filter = cat.map(str::to_owned);
                        }
                    }
                    if ui.small_button(I18nKey::OpenDocument.tr_ui(lang)).clicked() {
                        app.open_file_dialog();
                    }
                });
                ui.add_space(6.0);
                let visible_templates: Vec<_> = app
                    .templates
                    .iter()
                    .filter(|t| {
                        app.template_category_filter
                            .as_deref()
                            .is_none_or(|c| t.category.eq_ignore_ascii_case(c))
                    })
                    .cloned()
                    .collect();
                shell_dir.with_horizontal_wrapped_align(ui, egui::Align::Min, |ui| {
                    for tpl in &visible_templates {
                        Frame::NONE
                            .fill(tokens.surface)
                            .stroke(Stroke::new(1.0, tokens.border))
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_min_width(172.0);
                                ui.set_max_width(196.0);
                                shell_dir.with_horizontal(ui, |ui| {
                                    ui.label(RichText::new(&tpl.name).strong().color(tokens.text));
                                    if tpl.favorite {
                                        ui.label(RichText::new("★").color(tokens.accent))
                                            .on_hover_text(I18nKey::FavoriteTemplate.tr_ui(lang));
                                    }
                                });
                                ui.label(
                                    RichText::new(format!(
                                        "{:.0} × {:.0} mm · {} · {}p",
                                        tpl.width_mm,
                                        tpl.height_mm,
                                        tpl.color_mode.to_uppercase(),
                                        tpl.page_count
                                    ))
                                    .size(11.0)
                                    .color(tokens.text_muted),
                                );
                                ui.label(
                                    RichText::new(&tpl.description)
                                        .size(10.0)
                                        .color(tokens.text_muted),
                                );
                                ui.add_space(6.0);
                                if ui
                                    .small_button(I18nKey::NewFromTemplate.tr_ui(lang))
                                    .clicked()
                                {
                                    app.create_document_from_template(tpl);
                                }
                            });
                    }
                    let quick_presets = [
                        (
                            I18nKey::WelcomePresetA3Poster.tr_ui(lang),
                            297.0,
                            420.0,
                            ColorMode::Cmyk,
                            300.0,
                        ),
                        (
                            I18nKey::WelcomePresetLaserBed.tr_ui(lang),
                            300.0,
                            200.0,
                            ColorMode::Rgb,
                            300.0,
                        ),
                    ];
                    for (name, w, h, mode, dpi) in quick_presets {
                        Frame::NONE
                            .fill(tokens.surface)
                            .stroke(Stroke::new(1.0, tokens.border))
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_min_width(154.0);
                                ui.label(RichText::new(&name).strong().color(tokens.text));
                                ui.label(
                                    RichText::new(format!(
                                        "{w:.0} × {h:.0} mm · {mode:?} {dpi:.0} DPI"
                                    ))
                                    .size(11.0)
                                    .color(tokens.text_muted),
                                );
                                ui.add_space(6.0);
                                if ui
                                    .small_button(I18nKey::WelcomeCreateButton.tr_ui(lang))
                                    .clicked()
                                {
                                    app.new_document(Document::new(name, w, h, mode, dpi));
                                }
                            });
                    }
                });

                ui.add_space(20.0);

                // Recent Documents from SQLite
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(
                        RichText::new(I18nKey::RecentDocuments.tr_ui(lang))
                            .size(15.0)
                            .strong()
                            .color(tokens.text),
                    );
                    if !app.recent_docs.is_empty()
                        && ui.small_button(I18nKey::ClearRecents.tr_ui(lang)).clicked()
                    {
                        app.clear_recent_docs();
                    }
                });
                ui.add_space(6.0);
                let recents = app.recent_docs.clone();
                if recents.is_empty() {
                    ui.label(
                        RichText::new(I18nKey::WelcomeNoRecents.tr_ui(lang))
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
                                shell_dir.with_horizontal(ui, |ui| {
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
                                    shell_dir.with_opposite_horizontal(ui, |ui| {
                                        if ui
                                            .small_button(I18nKey::WelcomeOpenButton.tr_ui(lang))
                                            .clicked()
                                        {
                                            let pb = PathBuf::from(&rec.file_path);
                                            if let Err(err) = app.open_file_path(&pb) {
                                                app.push_toast(err, true);
                                            }
                                        }
                                    });
                                });
                            });
                        ui.add_space(4.0);
                    }
                }
            });
        });
}
