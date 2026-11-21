// SPDX-License-Identifier: MPL-2.0

//! Right-side vertical **Color Palette Swatch Bar** (`32px` compact or expanded
//! named-swatches list):
//! - **Left-click swatch** = Apply Fill
//! - **Right-click swatch** = Apply Outline
//! - **Top `✕` swatch** = Remove Fill (Left-click) / Remove Outline (Right-click)
//! - Spot ink swatches display a crisp corner triangle dot for color-blind accessibility.

use crate::state::StudioApp;
use crate::theme::{core_color_to_egui, PALETTE_BAR_WIDTH};
use coree_core::{default_cmyk_palette, default_rgb_palette, Color, ColorPalette};
use egui::{
    vec2, Color32, CornerRadius, Frame, Margin, RichText, Sense, Shape, Stroke, StrokeKind, Ui,
};

/// Renders the docked right-side Color Palette Swatch Bar.
pub fn show_palette_bar(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let width = if app.palette_show_names {
        164.0
    } else {
        PALETTE_BAR_WIDTH
    };

    egui::Panel::right("coree_palette_bar")
        .exact_size(width)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(4, 4))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            // Palette switcher button & expand names toggle
            ui.horizontal(|ui| {
                if ui
                    .small_button(if app.palette_show_names { "»" } else { "«" })
                    .on_hover_text("Toggle Swatch Color Names")
                    .clicked()
                {
                    app.palette_show_names = !app.palette_show_names;
                }
                if app.palette_show_names {
                    let pal_names = collect_palette_names(app);
                    let cur = pal_names
                        .get(app.active_palette_index)
                        .cloned()
                        .unwrap_or_else(|| "CMYK".to_owned());
                    egui::ComboBox::from_id_salt("palette_switcher")
                        .width(112.0)
                        .selected_text(cur)
                        .show_ui(ui, |ui| {
                            for (idx, name) in pal_names.iter().enumerate() {
                                ui.selectable_value(&mut app.active_palette_index, idx, name);
                            }
                        });
                }
            });

            // Quick palette selector cycle button in compact mode
            if !app.palette_show_names {
                let total = 4 + app.custom_palettes.len();
                let short = match app.active_palette_index {
                    0 => "C",
                    1 => "R",
                    2 => "D",
                    3 => "S",
                    _ => "+",
                };
                let btn_resp = ui
                    .add_sized(
                        vec2(22.0, 20.0),
                        egui::Button::new(RichText::new(short).size(10.0)),
                    )
                    .on_hover_text(
                        "Click to cycle palette (CMYK / RGB / Document / CutContour / Custom)\nRight-click to import .gpl/.ase/.pal",
                    );
                if btn_resp.clicked() {
                    app.active_palette_index = (app.active_palette_index + 1) % total.max(1);
                } else if btn_resp.secondary_clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Import Color Palette")
                        .add_filter("Color Palettes", &["gpl", "ase", "pal"])
                        .pick_file()
                    {
                        if let Err(err) = app.import_palette_from_path(&path) {
                            app.push_toast(err, true);
                        }
                    }
                }
            } else if ui
                .small_button("+ Import (.gpl/.ase/.pal)")
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Import Color Palette")
                    .add_filter("Color Palettes", &["gpl", "ase", "pal"])
                    .pick_file()
                {
                    if let Err(err) = app.import_palette_from_path(&path) {
                        app.push_toast(err, true);
                    }
                }
            }

            ui.add_space(2.0);

            // Top "No Fill / No Outline" slash swatch
            let no_size = if app.palette_show_names {
                vec2(ui.available_width(), 22.0)
            } else {
                vec2(22.0, 22.0)
            };
            let (no_rect, no_resp) = ui.allocate_exact_size(no_size, Sense::click());
            let box_rect = egui::Rect::from_min_size(no_rect.left_top(), vec2(22.0, 22.0));
            ui.painter().rect(
                box_rect,
                CornerRadius::same(2),
                Color32::WHITE,
                Stroke::new(1.0, tokens.border_input),
                StrokeKind::Inside,
            );
            ui.painter().line_segment(
                [box_rect.left_bottom() + vec2(2.0, -2.0), box_rect.right_top() + vec2(-2.0, 2.0)],
                Stroke::new(2.0, tokens.danger),
            );
            if app.palette_show_names {
                ui.painter().text(
                    egui::pos2(box_rect.right() + 6.0, box_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "None (L:Fill / R:Stroke)",
                    egui::FontId::proportional(10.5),
                    tokens.text_muted,
                );
            }
            let no_resp = no_resp
                .on_hover_text("No Color: Left-click removes Fill, Right-click removes Outline");
            if no_resp.clicked() {
                app.remove_fill();
            } else if no_resp.secondary_clicked() {
                app.remove_outline();
            }

            ui.add_space(2.0);

            // Active palette swatches
            let palette = resolve_active_palette(app);
            let mut apply_fill: Option<Color> = None;
            let mut apply_outline: Option<Color> = None;

            egui::ScrollArea::vertical()
                .id_salt("palette_swatches_scroll")
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                    for sw in &palette.swatches {
                        let row_size = if app.palette_show_names {
                            vec2(ui.available_width(), 22.0)
                        } else {
                            vec2(22.0, 22.0)
                        };
                        let (row_rect, resp) = ui.allocate_exact_size(row_size, Sense::click());
                        let cell = egui::Rect::from_min_size(row_rect.left_top(), vec2(22.0, 22.0));
                        let c32 = core_color_to_egui(&sw.color);
                        let stroke = if resp.hovered() {
                            Stroke::new(1.5, tokens.text)
                        } else {
                            Stroke::new(0.8, tokens.border)
                        };
                        ui.painter().rect(
                            cell,
                            CornerRadius::same(2),
                            c32,
                            stroke,
                            StrokeKind::Inside,
                        );

                        // Spot ink accessibility triangle dot in bottom-right corner
                        if matches!(sw.color, Color::Spot { .. }) {
                            let br = cell.right_bottom() - vec2(1.0, 1.0);
                            ui.painter().add(Shape::convex_polygon(
                                vec![br, br - vec2(6.0, 0.0), br - vec2(0.0, 6.0)],
                                Color32::WHITE,
                                Stroke::new(0.5, Color32::BLACK),
                            ));
                        }

                        if app.palette_show_names {
                            ui.painter().text(
                                egui::pos2(cell.right() + 6.0, cell.center().y),
                                egui::Align2::LEFT_CENTER,
                                &sw.name,
                                egui::FontId::proportional(10.5),
                                tokens.text,
                            );
                        }

                        let tip = format_swatch_tooltip(&sw.name, &sw.color);
                        let resp = resp.on_hover_text(tip);
                        if resp.clicked() {
                            apply_fill = Some(sw.color.clone());
                        } else if resp.secondary_clicked() {
                            apply_outline = Some(sw.color.clone());
                        }
                    }
                });

            if let Some(c) = apply_fill {
                app.apply_fill_color(c);
            }
            if let Some(c) = apply_outline {
                app.apply_outline_color(c);
            }
        });
}

fn collect_palette_names(app: &StudioApp) -> Vec<String> {
    let mut names = vec![
        "Default CMYK".to_owned(),
        "Default RGB".to_owned(),
        "Document Palette".to_owned(),
        "CutContour Spot".to_owned(),
    ];
    for p in &app.custom_palettes {
        names.push(p.name.clone());
    }
    names
}

fn resolve_active_palette(app: &StudioApp) -> ColorPalette {
    match app.active_palette_index {
        0 => default_cmyk_palette(),
        1 => default_rgb_palette(),
        2 => app
            .active_tab()
            .map(|t| t.document.document_palette.clone())
            .unwrap_or_else(default_cmyk_palette),
        3 => StudioApp::cut_production_palette(),
        idx => app
            .custom_palettes
            .get(idx.saturating_sub(4))
            .cloned()
            .unwrap_or_else(default_cmyk_palette),
    }
}

fn format_swatch_tooltip(name: &str, color: &Color) -> String {
    let detail = match color {
        Color::Cmyk { c, m, y, k, .. } => format!(
            "CMYK {:.0}/{:.0}/{:.0}/{:.0}",
            c * 100.0,
            m * 100.0,
            y * 100.0,
            k * 100.0
        ),
        Color::Spot {
            name: spot, tint, ..
        } => format!("Spot '{spot}' {:.0}%", tint * 100.0),
        _ => {
            let [r, g, b, _] = color.to_rgba8();
            format!("RGB({r}, {g}, {b}) {}", color.to_hex())
        }
    };
    format!("{name}\n{detail}\nLeft-click: Fill · Right-click: Outline")
}
