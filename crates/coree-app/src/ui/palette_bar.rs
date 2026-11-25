// SPDX-License-Identifier: MIT OR Apache-2.0

//! Right-side vertical **Color Palette Swatch Bar** (`32px` compact or expanded
//! named-swatches list):
//! - **Left-click swatch** = Apply Fill
//! - **Right-click swatch** = Apply Outline
//! - **Top `✕` No-Color swatch** = Remove Fill (`Fill::None`, Left-click) / Remove Outline (`Outline::none`, Right-click)
//! - Quick switcher between `Default CMYK`, `Default RGB`, `Document Palette`,
//!   `Laser & Cut Colors` (Red Cut, Blue Score, Black Hatch, Green Perforate, Magenta CutContour),
//!   and imported `.gpl`/`.ase`/`.pal` palettes.
//! - Spot ink swatches display a crisp corner triangle dot for color-blind accessibility.

use crate::i18n::{I18nKey, ShellLayoutDirections};
use crate::state::StudioApp;
use crate::theme::{core_color_to_egui, ThemeTokens, PALETTE_BAR_WIDTH};
use coree_core::{
    default_cmyk_palette, default_rgb_palette, Color, ColorMode, ColorPalette, ColorSwatch,
};
use egui::{
    pos2, vec2, Color32, CornerRadius, Frame, Margin, RichText, Sense, Shape, Stroke, StrokeKind,
    Ui,
};

/// Builds the combined Laser Cut Colors + Production Spot palette (index 3).
#[must_use]
pub fn laser_and_cut_palette() -> ColorPalette {
    let mut pal = ColorPalette::new("Laser & Cut Colors", ColorMode::Rgb);
    pal.swatches = vec![
        ColorSwatch::new("Laser Cut (Red)", Color::rgba8(255, 0, 0, 255)),
        ColorSwatch::new("Laser Score / Engrave (Blue)", Color::rgba8(0, 0, 255, 255)),
        ColorSwatch::new("Laser Hatch Fill (Black)", Color::rgba8(0, 0, 0, 255)),
        ColorSwatch::new("Laser Perforate (Green)", Color::rgba8(0, 170, 0, 255)),
        ColorSwatch::new("Laser Engrave Alt (Orange)", Color::rgba8(255, 136, 0, 255)),
        ColorSwatch::new("Laser Mark (Cyan)", Color::rgba8(0, 187, 255, 255)),
        ColorSwatch::new(
            "Laser Guide / Ignore (Gray)",
            Color::rgba8(128, 128, 128, 255),
        ),
    ];
    pal.swatches
        .extend(StudioApp::cut_production_palette().swatches);
    pal
}

/// Renders the docked right-side Color Palette Swatch Bar.
pub fn show_palette_bar(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(app.ui_language);
    let width = if app.palette_show_names {
        176.0
    } else {
        PALETTE_BAR_WIDTH
    };

    let panel = if shell_dir.palette_on_left {
        egui::Panel::left("coree_palette_bar")
    } else {
        egui::Panel::right("coree_palette_bar")
    };
    panel
        .exact_size(width)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(4, 4))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            let lang = app.ui_language;
            // Palette expand toggle + dropdown in named mode
            shell_dir.with_horizontal(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                if ui
                    .small_button(if app.palette_show_names { "»" } else { "«" })
                    .on_hover_text("Toggle Expanded Swatch Names & Palette Selector")
                    .clicked()
                {
                    app.palette_show_names = !app.palette_show_names;
                }
                if app.palette_show_names {
                    let pal_names = collect_palette_names(app);
                    let cur = pal_names
                        .get(app.active_palette_index)
                        .cloned()
                        .unwrap_or_else(|| "Default CMYK".to_owned());
                    egui::ComboBox::from_id_salt("palette_switcher")
                        .width(126.0)
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
                let (short, active_name) = match app.active_palette_index {
                    0 => ("C", "Default CMYK"),
                    1 => ("R", "Default RGB"),
                    2 => ("D", "Document Palette"),
                    3 => ("L", "Laser & Cut Colors"),
                    idx => (
                        "+",
                        app.custom_palettes
                            .get(idx.saturating_sub(4))
                            .map_or("Custom Palette", |p| p.name.as_str()),
                    ),
                };
                let btn_resp = ui
                    .add_sized(
                        vec2(22.0, 20.0),
                        egui::Button::new(RichText::new(short).size(10.0).strong().color(
                            if app.active_palette_index == 3 {
                                tokens.cutcontour
                            } else {
                                tokens.accent
                            },
                        )),
                    )
                    .on_hover_text(format!(
                        "Active Palette: {active_name}\nLeft-click: Cycle (CMYK / RGB / Document / Laser & Cut / Custom)\nRight-click: {}",
                        I18nKey::PaletteImportButton.tr_ui(lang)
                    ));
                if btn_resp.clicked() {
                    app.active_palette_index = (app.active_palette_index + 1) % total.max(1);
                } else if btn_resp.secondary_clicked() {
                    import_palette_dialog(app);
                }
            } else if ui
                .small_button(I18nKey::PaletteImportButton.tr_ui(lang))
                .clicked()
            {
                import_palette_dialog(app);
            }

            ui.add_space(3.0);

            // Top [X] No-Color swatch with crisp red diagonal slash
            let no_size = if app.palette_show_names {
                vec2(ui.available_width(), 22.0)
            } else {
                vec2(22.0, 22.0)
            };
            let (no_rect, no_resp) = ui.allocate_exact_size(no_size, Sense::click());
            let box_rect = if shell_dir.palette_on_left && app.palette_show_names {
                egui::Rect::from_min_size(
                    pos2(no_rect.right() - 22.0, no_rect.top()),
                    vec2(22.0, 22.0),
                )
            } else {
                egui::Rect::from_min_size(no_rect.left_top(), vec2(22.0, 22.0))
            };
            let no_border = if no_resp.hovered() {
                Stroke::new(1.5, tokens.accent)
            } else {
                Stroke::new(1.0, tokens.border_input)
            };
            ui.painter().rect(
                box_rect,
                CornerRadius::same(2),
                Color32::WHITE,
                no_border,
                StrokeKind::Inside,
            );
            // Crisp classic red diagonal slash across the [X] No-Color well
            let slash_red = Color32::from_rgb(0xd9, 0x26, 0x26);
            ui.painter().line_segment(
                [
                    box_rect.left_bottom() + vec2(2.5, -2.5),
                    box_rect.right_top() + vec2(-2.5, 2.5),
                ],
                Stroke::new(2.2, slash_red),
            );
            ui.painter().line_segment(
                [
                    box_rect.left_top() + vec2(2.5, 2.5),
                    box_rect.right_bottom() + vec2(-2.5, -2.5),
                ],
                Stroke::new(1.0, slash_red.gamma_multiply(0.35)),
            );
            if app.palette_show_names {
                let (txt_pos, txt_align) = if shell_dir.palette_on_left {
                    (
                        pos2(box_rect.left() - 6.0, box_rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                    )
                } else {
                    (
                        pos2(box_rect.right() + 6.0, box_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                    )
                };
                ui.painter().text(
                    txt_pos,
                    txt_align,
                    I18nKey::PaletteNoColorLabel.tr_ui(lang),
                    egui::FontId::proportional(10.5),
                    tokens.text_muted,
                );
            }
            let no_resp = no_resp.on_hover_text(I18nKey::PaletteNoColorLabel.tr_ui(lang));
            if no_resp.clicked() {
                app.remove_fill();
            } else if no_resp.secondary_clicked() {
                app.remove_outline();
            }

            ui.add_space(3.0);

            // Active palette swatches (`Click` = Fill, `Right-click` = Outline, `0.5s Hold` = TintGrid, `Drag` = DragSwatch)
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
                        let (row_rect, resp) =
                            ui.allocate_exact_size(row_size, Sense::click_and_drag());
                        let cell = if shell_dir.palette_on_left && app.palette_show_names {
                            egui::Rect::from_min_size(
                                pos2(row_rect.right() - 22.0, row_rect.top()),
                                vec2(22.0, 22.0),
                            )
                        } else {
                            egui::Rect::from_min_size(row_rect.left_top(), vec2(22.0, 22.0))
                        };
                        let c32 = core_color_to_egui(&sw.color);
                        let stroke = if resp.hovered() {
                            Stroke::new(1.6, tokens.accent)
                        } else {
                            Stroke::new(0.85, tokens.border)
                        };
                        ui.painter().rect(
                            cell,
                            CornerRadius::same(2),
                            c32,
                            stroke,
                            StrokeKind::Inside,
                        );

                        // Inner crisp white outline on hover for dark swatches
                        if resp.hovered() {
                            ui.painter().rect_stroke(
                                cell.shrink(1.5),
                                CornerRadius::same(1),
                                Stroke::new(0.8, Color32::WHITE),
                                StrokeKind::Inside,
                            );
                        }

                        // Spot ink accessibility triangle dot in bottom-right corner
                        if matches!(sw.color, Color::Spot { .. }) {
                            let br = cell.right_bottom() - vec2(1.0, 1.0);
                            ui.painter().add(Shape::convex_polygon(
                                vec![br, br - vec2(6.0, 0.0), br - vec2(0.0, 6.0)],
                                Color32::WHITE,
                                Stroke::new(0.6, Color32::BLACK),
                            ));
                        }

                        if app.palette_show_names {
                            let (txt_pos, txt_align) = if shell_dir.palette_on_left {
                                (
                                    pos2(cell.left() - 6.0, cell.center().y),
                                    egui::Align2::RIGHT_CENTER,
                                )
                            } else {
                                (
                                    pos2(cell.right() + 6.0, cell.center().y),
                                    egui::Align2::LEFT_CENTER,
                                )
                            };
                            ui.painter().text(
                                txt_pos,
                                txt_align,
                                &sw.name,
                                egui::FontId::proportional(10.5),
                                tokens.text,
                            );
                        }

                        let tip = format!(
                            "{}\n{}",
                            format_swatch_tooltip(&sw.name, &sw.color),
                            I18nKey::PaletteDragSwatchHint.tr_ui(lang)
                        );
                        let resp = resp.on_hover_text(tip);

                        // DragSwatch: initiate egui Drag-and-Drop payload (`Color`) onto canvas
                        if resp.drag_started() {
                            app.swatch_hold_state = None;
                            egui::DragAndDrop::set_payload(ui.ctx(), sw.color.clone());
                        }
                        if resp.dragged() {
                            if let Some(pointer) = ui.ctx().pointer_latest_pos() {
                                let preview_rect =
                                    egui::Rect::from_min_size(pointer + vec2(10.0, 10.0), vec2(18.0, 18.0));
                                ui.ctx().layer_painter(egui::LayerId::new(
                                    egui::Order::Tooltip,
                                    egui::Id::unique("drag_swatch_preview"),
                                ))
                                .rect(
                                    preview_rect,
                                    CornerRadius::same(3),
                                    c32,
                                    Stroke::new(1.2, Color32::WHITE),
                                    StrokeKind::Inside,
                                );
                            }
                        }

                        // 0.5s long-press TintGrid trigger
                        if resp.is_pointer_button_down_on()
                            && ui.input(|i| i.pointer.primary_down())
                            && !resp.dragged()
                        {
                            match &app.swatch_hold_state {
                                Some((started_at, held_color, _)) if *held_color == sw.color => {
                                    if started_at.elapsed().as_secs_f32() >= 0.5 {
                                        app.tint_grid_popup =
                                            Some((sw.color.clone(), cell.center()));
                                        app.swatch_hold_state = None;
                                    } else {
                                        ui.ctx().request_repaint();
                                    }
                                }
                                _ => {
                                    app.swatch_hold_state = Some((
                                        std::time::Instant::now(),
                                        sw.color.clone(),
                                        cell.center(),
                                    ));
                                    ui.ctx().request_repaint();
                                }
                            }
                        } else if app
                            .swatch_hold_state
                            .as_ref()
                            .is_some_and(|(_, held_color, _)| *held_color == sw.color)
                        {
                            app.swatch_hold_state = None;
                        }

                        if resp.clicked() {
                            apply_fill = Some(sw.color.clone());
                        } else if resp.secondary_clicked() {
                            apply_outline = Some(sw.color.clone());
                        }
                    }
                });

            // Render floating TintGrid popup (5x5 tints & shades around held swatch)
            show_tint_grid(ui, app, &tokens, shell_dir, &mut apply_fill, &mut apply_outline);

            if let Some(c) = apply_fill {
                app.apply_fill_color(c);
            }
            if let Some(c) = apply_outline {
                app.apply_outline_color(c);
            }
        });
}

/// Renders the floating 5x5 Tint Grid popup for long-pressed swatches (`ADD-09`).
fn show_tint_grid(
    ui: &Ui,
    app: &mut StudioApp,
    tokens: &ThemeTokens,
    shell_dir: ShellLayoutDirections,
    apply_fill: &mut Option<Color>,
    apply_outline: &mut Option<Color>,
) {
    let Some((base_color, anchor)) = app.tint_grid_popup.clone() else {
        return;
    };
    let lang = app.ui_language;
    let mut close_grid = false;
    let popup_pos = if shell_dir.palette_on_left {
        pos2(anchor.x + 28.0, (anchor.y - 68.0).max(32.0))
    } else {
        pos2((anchor.x - 148.0).max(8.0), (anchor.y - 68.0).max(32.0))
    };
    egui::Area::new(egui::Id::unique("tint_grid_popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(popup_pos)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(tokens.surface_elevated)
                .stroke(Stroke::new(1.2, tokens.accent))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    shell_dir.with_horizontal(ui, |ui| {
                        ui.label(
                            RichText::new(I18nKey::PaletteTintGridTitle.tr_ui(lang))
                                .size(11.0)
                                .strong()
                                .color(tokens.text),
                        );
                        if ui.small_button("✕").clicked() {
                            close_grid = true;
                        }
                    });
                    ui.add_space(4.0);
                    let tints = build_tint_grid_colors(&base_color);
                    for row in tints.chunks(5) {
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                            for c in row {
                                let (_, sresp) =
                                    ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
                                let srect = sresp.rect;
                                ui.painter().rect(
                                    srect,
                                    CornerRadius::same(2),
                                    core_color_to_egui(c),
                                    Stroke::new(
                                        if sresp.hovered() { 1.4 } else { 0.8 },
                                        if sresp.hovered() {
                                            tokens.accent
                                        } else {
                                            tokens.border
                                        },
                                    ),
                                    StrokeKind::Inside,
                                );
                                if sresp.clicked() {
                                    *apply_fill = Some(c.clone());
                                    close_grid = true;
                                } else if sresp.secondary_clicked() {
                                    *apply_outline = Some(c.clone());
                                    close_grid = true;
                                }
                            }
                        });
                    }
                });
        });
    if close_grid || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.tint_grid_popup = None;
    }
}

fn import_palette_dialog(app: &mut StudioApp) {
    if let Some(path) = rfd::FileDialog::new()
        .set_title("Import Color Palette")
        .add_filter("Color Palettes", &["gpl", "ase", "pal", "acb"])
        .pick_file()
    {
        if let Err(err) = app.import_palette_from_path(&path) {
            app.push_toast(err, true);
        }
    }
}

fn collect_palette_names(app: &StudioApp) -> Vec<String> {
    let mut names = vec![
        "Default CMYK".to_owned(),
        "Default RGB".to_owned(),
        "Document Palette".to_owned(),
        "Laser & Cut Colors".to_owned(),
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
            .map(|t| t.document.document_palette().clone())
            .unwrap_or_else(default_cmyk_palette),
        3 => laser_and_cut_palette(),
        idx => app
            .custom_palettes
            .get(idx.saturating_sub(4))
            .cloned()
            .unwrap_or_else(default_cmyk_palette),
    }
}

fn format_swatch_tooltip(name: &str, color: &Color) -> String {
    let [r, g, b, _] = color.to_rgba8();
    let [c, m, y, k] = color.to_cmyk();
    let cmyk_str = format!(
        "CMYK {:.0}/{:.0}/{:.0}/{:.0}",
        c * 100.0,
        m * 100.0,
        y * 100.0,
        k * 100.0
    );
    let rgb_str = format!("RGB({r}, {g}, {b}) {}", color.to_hex());
    let detail = match color {
        Color::Spot {
            name: spot, tint, ..
        } => format!("Spot '{spot}' {:.0}% · {cmyk_str}\n{rgb_str}", tint * 100.0),
        Color::Cmyk { .. } => format!("{cmyk_str}\n{rgb_str}"),
        _ => format!("{rgb_str}\n{cmyk_str}"),
    };
    format!("{name}\n{detail}\nLeft-click: Apply Fill · Right-click: Apply Outline")
}

/// Builds a 5x5 shade/tint grid around `base` for the long-press `TintGrid` popup (`ADD-09`).
#[must_use]
pub fn build_tint_grid_colors(base: &Color) -> Vec<Color> {
    let mut out = Vec::with_capacity(25);
    for row in 0..5 {
        let light_t = (row as f32 - 2.0) * 0.28;
        let row_color = if light_t < 0.0 {
            base.lerp(&Color::WHITE, (-light_t).clamp(0.0, 0.85))
        } else {
            base.lerp(&Color::BLACK, light_t.clamp(0.0, 0.85))
        };
        for col in 0..5 {
            let sat_t = ((col as f32) - 2.0) * 0.14;
            let [r, g, b, a] = row_color.to_rgba_f32();
            let luma = 0.299 * r + 0.587 * g + 0.114 * b;
            let s_color = Color::rgba(
                (r + (r - luma) * sat_t).clamp(0.0, 1.0),
                (g + (g - luma) * sat_t).clamp(0.0, 1.0),
                (b + (b - luma) * sat_t).clamp(0.0, 1.0),
                a,
            );
            out.push(s_color);
        }
    }
    out
}
