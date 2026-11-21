// SPDX-License-Identifier: MPL-2.0

//! Modal fuzzy-search Command Palette (`Ctrl+Space` / `Ctrl+K`) indexing all menu
//! actions, tools, 11 lenses, 9 boolean ops, dockers, themes, and built-in Rhai macros.

use crate::state::StudioApp;
use egui::{Align2, CornerRadius, Frame, Key, Margin, RichText, Stroke};

/// Renders the Command Palette overlay when `app.command_palette.open` is true.
pub fn show_command_palette(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.command_palette.open {
        return;
    }
    let tokens = app.theme.tokens();
    let matches = app.search_command_palette(&app.command_palette.query);
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
        .fixed_size(egui::vec2(560.0, 420.0))
        .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 68.0))
        .frame(
            Frame::NONE
                .fill(tokens.surface_elevated)
                .stroke(Stroke::new(1.5, tokens.border_input))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::same(12)),
        )
        .show(ctx, |ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut app.command_palette.query)
                    .hint_text("Search tools, lenses, booleans, dockers, menus, macros...")
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
                .max_height(330.0)
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
                            .corner_radius(CornerRadius::same(5))
                            .inner_margin(Margin::symmetric(8, 5))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(&cmd.category)
                                            .size(10.5)
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
        app.execute_palette_command(&id);
    } else if close_now {
        app.command_palette.open = false;
    }
}
