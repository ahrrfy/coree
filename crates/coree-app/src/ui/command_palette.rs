// SPDX-License-Identifier: MIT OR Apache-2.0

//! Modal fuzzy-search Command Palette (`Ctrl+Space` / `Ctrl+Shift+P`) indexing all
//! menu actions, tools, 11 lenses, 9 boolean ops, dockers, themes, built-in Rhai
//! macros, Laser & Plugins bridges, Autofit Page, CMYK Soft-Proofing, Options,
//! and Studio Workspaces.

use crate::shortcuts::{shortcut_label, ShortcutAction};
use crate::state::{DockerTab, PaletteCommand, StudioApp, WorkspacePreset};
use egui::{Align2, CornerRadius, Frame, Key, Margin, RichText, Stroke};

/// Renders the Command Palette overlay when `app.command_palette.open` is true.
pub fn show_command_palette(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.command_palette.open {
        return;
    }
    let tokens = app.theme.tokens();
    let matches = search_combined_commands(app, &app.command_palette.query);
    let visible_count = matches.len().min(60);

    if visible_count == 0 {
        app.command_palette.selected_index = 0;
    } else if app.command_palette.selected_index >= visible_count {
        app.command_palette.selected_index = visible_count - 1;
    }

    // Keyboard navigation inside the palette (clamped to the 60 rendered rows)
    let mut execute_id: Option<String> = None;
    let mut close_now = false;
    ctx.input(|i| {
        if i.key_pressed(Key::Escape) {
            close_now = true;
        }
        if visible_count > 0 {
            if i.key_pressed(Key::ArrowDown) {
                app.command_palette.selected_index =
                    (app.command_palette.selected_index + 1) % visible_count;
            }
            if i.key_pressed(Key::ArrowUp) {
                app.command_palette.selected_index = if app.command_palette.selected_index == 0 {
                    visible_count - 1
                } else {
                    app.command_palette.selected_index - 1
                };
            }
            if i.key_pressed(Key::Enter) {
                if let Some(cmd) = matches.get(app.command_palette.selected_index) {
                    execute_id = Some(cmd.id.clone());
                }
            }
        }
    });

    egui::Window::new("Command Palette")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .fixed_size(egui::vec2(580.0, 440.0))
        .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 64.0))
        .frame(
            Frame::NONE
                .fill(tokens.surface_elevated)
                .stroke(Stroke::new(1.5, tokens.border_input))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(12)),
        )
        .show(ctx, |ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut app.command_palette.query)
                    .hint_text("Search tools, laser plugins, workspaces, autofit, soft-proof, options, macros...")
                    .desired_width(f32::INFINITY),
            );
            if app.command_palette.just_opened {
                resp.request_focus();
                app.command_palette.just_opened = false;
            }
            if resp.changed() {
                app.command_palette.selected_index = 0;
            }

            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("{} command(s) matching", matches.len()))
                    .size(11.0)
                    .color(tokens.text_muted),
            );
            ui.separator();

            egui::ScrollArea::vertical()
                .max_height(346.0)
                .show(ui, |ui| {
                    if matches.is_empty() {
                        ui.label(
                            RichText::new("No commands matched your search.")
                                .color(tokens.text_muted),
                        );
                    }
                    for (idx, cmd) in matches.iter().take(60).enumerate() {
                        let is_sel = idx == app.command_palette.selected_index;
                        let bg = if is_sel {
                            tokens.surface
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        let stroke = if is_sel {
                            Stroke::new(1.0, tokens.accent)
                        } else {
                            Stroke::NONE
                        };
                        let row = Frame::NONE
                            .fill(bg)
                            .stroke(stroke)
                            .corner_radius(CornerRadius::same(4))
                            .inner_margin(Margin::symmetric(8, 5))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(&cmd.category)
                                            .size(10.5)
                                            .strong()
                                            .color(tokens.accent),
                                    );
                                    ui.label(RichText::new(&cmd.title).color(tokens.text));
                                    if !cmd.shortcut.is_empty() {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(
                                                    RichText::new(&cmd.shortcut)
                                                        .monospace()
                                                        .size(11.0)
                                                        .color(tokens.text_muted),
                                                );
                                            },
                                        );
                                    }
                                });
                            });
                        if row.response.interact(egui::Sense::click()).clicked() {
                            execute_id = Some(cmd.id.clone());
                        }
                    }
                });
        });

    if let Some(id) = execute_id {
        app.command_palette.open = false;
        if !execute_extra_ui_command(app, &id) {
            app.execute_palette_command(&id);
        }
    } else if close_now {
        app.command_palette.open = false;
    }
}

fn extra_ui_commands(app: &StudioApp) -> Vec<PaletteCommand> {
    let p = app.shortcut_profile;
    let mut extra = vec![
        PaletteCommand {
            id: "ui.laser.studio".into(),
            title: "Open Laser & Plugins Studio Docker".into(),
            category: "Laser".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.laser.preview".into(),
            title: "Laser: Compute & Toggle Toolpath Preview Overlay".into(),
            category: "Laser".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.laser.send.lightburn-bridge".into(),
            title: "Laser: Send to LightBurn".into(),
            category: "Laser".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.laser.send.rdworks-ruida".into(),
            title: "Laser: Send to RDWorks (Ruida)".into(),
            category: "Laser".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.laser.send.ezcad-fiber".into(),
            title: "Laser: Send to EZCad (Fiber)".into(),
            category: "Laser".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.laser.send.laserdrw-m2nano".into(),
            title: "Laser: Send to LaserDRW / M2 Nano".into(),
            category: "Laser".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.laser.send.lasergrbl-bridge".into(),
            title: "Laser: Send to LaserGRBL".into(),
            category: "Laser".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.laser.send.inkcut-visicut".into(),
            title: "Laser: Send to InkCut / VisiCut".into(),
            category: "Laser".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.laser.send.vinyl-plotter-hpgl".into(),
            title: "Laser: Send to Vinyl Plotter (HPGL)".into(),
            category: "Laser".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.layout.autofit".into(),
            title: "Autofit Page to Content (5 mm Margin)".into(),
            category: "Layout".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.view.soft_proof".into(),
            title: "Toggle Proof Colors (CMYK Soft Proof)".into(),
            category: "View".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.view.gamut_warning".into(),
            title: "Toggle Gamut Warning Overlay".into(),
            category: "View".into(),
            shortcut: String::new(),
        },
        PaletteCommand {
            id: "ui.view.fullscreen".into(),
            title: "Toggle Full-Screen Preview".into(),
            category: "View".into(),
            shortcut: shortcut_label(p, ShortcutAction::ToggleFullScreenPreview).into(),
        },
        PaletteCommand {
            id: "ui.tools.options".into(),
            title: "Open Studio Options & Preferences...".into(),
            category: "Tools".into(),
            shortcut: shortcut_label(p, ShortcutAction::OpenOptions).into(),
        },
        PaletteCommand {
            id: "ui.tools.icc".into(),
            title: "Color Management (ICC Profiles)...".into(),
            category: "Tools".into(),
            shortcut: String::new(),
        },
    ];

    for (idx, preset) in WorkspacePreset::ALL.iter().enumerate() {
        extra.push(PaletteCommand {
            id: format!("ui.workspace.{idx}"),
            title: format!("Workspace: {}", preset.label()),
            category: "Workspace".into(),
            shortcut: String::new(),
        });
    }

    extra
}

fn search_combined_commands(app: &StudioApp, query: &str) -> Vec<PaletteCommand> {
    let q = query.trim().to_ascii_lowercase();
    let mut results = app.search_command_palette(query);
    let extra = extra_ui_commands(app);

    if q.is_empty() {
        let mut combined = extra;
        combined.extend(results);
        return combined;
    }

    let mut matched_extra: Vec<(i32, PaletteCommand)> = extra
        .into_iter()
        .filter_map(|cmd| {
            let hay = format!("{} {}", cmd.title, cmd.category).to_ascii_lowercase();
            local_fuzzy_score(&hay, &q).map(|score| (score, cmd))
        })
        .collect();
    matched_extra.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
    let mut combined: Vec<PaletteCommand> = matched_extra.into_iter().map(|(_, c)| c).collect();
    combined.append(&mut results);
    combined
}

fn local_fuzzy_score(haystack: &str, needle: &str) -> Option<i32> {
    if haystack.contains(needle) {
        return Some(100 - (haystack.len().saturating_sub(needle.len()) as i32).min(50));
    }
    let mut h_chars = haystack.chars();
    let mut matched = 0i32;
    for nc in needle.chars() {
        if h_chars.any(|hc| hc == nc) {
            matched += 1;
        } else {
            return None;
        }
    }
    Some(matched * 4)
}

fn execute_extra_ui_command(app: &mut StudioApp, id: &str) -> bool {
    match id {
        "ui.laser.studio" => {
            app.production_state.sub_section = 5;
            app.focus_docker(DockerTab::Production);
        }
        "ui.laser.preview" => {
            if app.show_laser_toolpath_overlay {
                app.show_laser_toolpath_overlay = false;
            } else {
                app.refresh_laser_plan_preview();
            }
        }
        "ui.layout.autofit" => {
            app.autofit_active_page(5.0);
        }
        "ui.view.soft_proof" => {
            app.soft_proof_cmyk = !app.soft_proof_cmyk;
            app.sync_proof_options_to_tabs();
        }
        "ui.view.gamut_warning" => {
            app.gamut_warning = !app.gamut_warning;
            app.sync_proof_options_to_tabs();
        }
        "ui.view.fullscreen" => {
            app.handle_shortcut(ShortcutAction::ToggleFullScreenPreview);
        }
        "ui.tools.options" => {
            app.options_tab = 0;
            app.show_options_dialog = true;
        }
        "ui.tools.icc" => {
            app.options_tab = 2;
            app.show_options_dialog = true;
        }
        other => {
            if let Some(plugin_id) = other.strip_prefix("ui.laser.send.") {
                app.send_to_laser_plugin(plugin_id, true);
                return true;
            }
            if let Some(idx_str) = other.strip_prefix("ui.workspace.") {
                if let Ok(idx) = idx_str.parse::<usize>() {
                    if let Some(&preset) = WorkspacePreset::ALL.get(idx) {
                        app.apply_workspace_preset(preset);
                        return true;
                    }
                }
            }
            return false;
        }
    }
    true
}
