// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

/// Classic Studio Modal **Edit Fill Dialog (`F11`)**.
pub(super) fn show_edit_fill_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.show_edit_fill_dialog {
        return;
    }
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut open = true;
    let mut commit_close = false;
    let mut apply_only = false;
    let mut cancel_clicked = false;

    egui::Window::new(crate::i18n::I18nKey::DlgFillWindowTitle.tr_ui(lang))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(520.0, 440.0))
        .show(ctx, |ui| {
            shell_dir.with_vertical(ui, |ui| {
                shell_dir.with_horizontal_wrapped(ui, |ui| {
                    if ui
                        .selectable_label(
                            matches!(app.edit_fill_draft, Fill::None),
                            crate::i18n::I18nKey::PropFillNone.tr_ui(lang),
                        )
                        .clicked()
                    {
                        app.edit_fill_draft = Fill::None;
                    }
                    if ui
                        .selectable_label(
                            matches!(app.edit_fill_draft, Fill::Solid(_)),
                            crate::i18n::I18nKey::PropFillSolid.tr_ui(lang),
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
                        .selectable_label(
                            is_fountain,
                            crate::i18n::I18nKey::PropFillFountain.tr_ui(lang),
                        )
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
                            crate::i18n::I18nKey::PropFillTwoColor.tr_ui(lang),
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
                            crate::i18n::I18nKey::PropFillTexture.tr_ui(lang),
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
                    if ui
                        .selectable_label(
                            matches!(app.edit_fill_draft, Fill::VectorPattern { .. }),
                            crate::i18n::I18nKey::PropFillVectorPattern.tr_ui(lang),
                        )
                        .clicked()
                    {
                        let sym_id = app
                            .active_tab()
                            .and_then(|t| t.document.symbols().keys().next().copied())
                            .unwrap_or_default();
                        app.edit_fill_draft = Fill::VectorPattern {
                            symbol_id: sym_id,
                            tile_w: 12.0,
                            tile_h: 12.0,
                        };
                    }
                    if ui
                        .selectable_label(
                            matches!(app.edit_fill_draft, Fill::BitmapPattern { .. }),
                            crate::i18n::I18nKey::PropFillBitmapPattern.tr_ui(lang),
                        )
                        .clicked()
                    {
                        let image_id = app
                            .active_tab()
                            .and_then(|t| t.document.assets().keys().next().copied())
                            .unwrap_or_default();
                        app.edit_fill_draft = Fill::BitmapPattern {
                            image_id,
                            tile_w: 16.0,
                            tile_h: 16.0,
                        };
                    }
                });
                ui.separator();

                match &mut app.edit_fill_draft {
                    Fill::None => {
                        ui.add_space(16.0);
                        ui.label(
                            RichText::new(crate::i18n::I18nKey::DlgFillEmptyNote.tr_ui(lang))
                                .color(tokens.text_muted),
                        );
                    }
                    Fill::Solid(c) => {
                        ui.label(
                            RichText::new(crate::i18n::I18nKey::DlgFillUniformHeading.tr_ui(lang))
                                .strong(),
                        );
                        ui.add_space(4.0);
                        edit_color_compact_sliders(ui, c, "f11_uniform", lang);
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
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.label(crate::i18n::I18nKey::DlgFillTypeLabel.tr_ui(lang));
                            ui.selectable_value(
                                &mut next_sub,
                                0,
                                crate::i18n::I18nKey::TbPressureLinear.tr_ui(lang),
                            );
                            ui.selectable_value(
                                &mut next_sub,
                                1,
                                crate::i18n::I18nKey::TbRadial.tr_ui(lang),
                            );
                            ui.selectable_value(
                                &mut next_sub,
                                2,
                                crate::i18n::I18nKey::TbConical.tr_ui(lang),
                            );
                            ui.selectable_value(
                                &mut next_sub,
                                3,
                                crate::i18n::I18nKey::TbSquare.tr_ui(lang),
                            );
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
                                egui::pos2(
                                    bar_rect.left() + bar_rect.width() * t1,
                                    bar_rect.bottom(),
                                ),
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
                                egui::vec2(
                                    if sel { 10.0 } else { 7.0 },
                                    if sel { 10.0 } else { 7.0 },
                                ),
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
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.label(format!(
                                "{} {}/{}:",
                                crate::i18n::I18nKey::DlgFillStopWord.tr_ui(lang),
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
                            if ui
                                .button(crate::i18n::I18nKey::DlgFillAddStop.tr_ui(lang))
                                .clicked()
                                && stops_mut.len() < 16
                            {
                                let c = sample_fountain_stop(stops_mut, 0.5);
                                stops_mut.push(GradientStop::new(0.5, c));
                                app.edit_fill_selected_stop = stops_mut.len() - 1;
                            }
                            if ui
                                .button(crate::i18n::I18nKey::DlgFillRemoveStop.tr_ui(lang))
                                .clicked()
                                && stops_mut.len() > 2
                            {
                                stops_mut.remove(app.edit_fill_selected_stop);
                                app.edit_fill_selected_stop =
                                    app.edit_fill_selected_stop.min(stops_mut.len() - 1);
                            }
                            if ui
                                .button(crate::i18n::I18nKey::DlgFillReverseStops.tr_ui(lang))
                                .clicked()
                            {
                                for st in stops_mut.iter_mut() {
                                    st.offset = (1.0 - st.offset).clamp(0.0, 1.0);
                                }
                                stops_mut.reverse();
                            }
                        });
                        if let Some(st) = stops_mut.get_mut(app.edit_fill_selected_stop) {
                            let mut pct = st.offset * 100.0;
                            shell_dir.with_horizontal(ui, |ui| {
                                ui.label(crate::i18n::I18nKey::DlgFillNodePos.tr_ui(lang));
                                if ui
                                    .add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix("%"))
                                    .changed()
                                {
                                    st.offset = (pct / 100.0).clamp(0.0, 1.0);
                                }
                            });
                            edit_color_compact_sliders(ui, &mut st.color, "f11_stop", lang);
                        }
                        ui.separator();
                        match &mut app.edit_fill_draft {
                            Fill::LinearGradient { start, end, .. } => {
                                shell_dir.with_horizontal(ui, |ui| {
                                    ui.label(crate::i18n::I18nKey::DlgFillStartEndMm.tr_ui(lang));
                                    ui.add(DragValue::new(&mut start.x).prefix("X1 "));
                                    ui.add(DragValue::new(&mut start.y).prefix("Y1 "));
                                    ui.add(DragValue::new(&mut end.x).prefix("X2 "));
                                    ui.add(DragValue::new(&mut end.y).prefix("Y2 "));
                                });
                            }
                            Fill::RadialGradient { center, radius, .. } => {
                                shell_dir.with_horizontal(ui, |ui| {
                                    ui.add(DragValue::new(&mut center.x).prefix("CX "));
                                    ui.add(DragValue::new(&mut center.y).prefix("CY "));
                                    ui.add(DragValue::new(radius).prefix("R ").range(0.5..=2000.0));
                                });
                            }
                            Fill::ConicalGradient {
                                center, angle_deg, ..
                            } => {
                                shell_dir.with_horizontal(ui, |ui| {
                                    ui.add(DragValue::new(&mut center.x).prefix("CX "));
                                    ui.add(DragValue::new(&mut center.y).prefix("CY "));
                                    ui.add(
                                        egui::Slider::new(angle_deg, -180.0..=180.0).suffix("°"),
                                    );
                                });
                            }
                            Fill::SquareGradient {
                                center,
                                size,
                                angle_deg,
                                ..
                            } => {
                                shell_dir.with_horizontal(ui, |ui| {
                                    ui.add(DragValue::new(&mut center.x).prefix("CX "));
                                    ui.add(DragValue::new(&mut center.y).prefix("CY "));
                                    ui.add(
                                        DragValue::new(size).prefix("Size ").range(1.0..=2000.0),
                                    );
                                    ui.add(
                                        egui::Slider::new(angle_deg, -180.0..=180.0).suffix("°"),
                                    );
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
                        shell_dir.with_horizontal(ui, |ui| {
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
                        ui.label(crate::i18n::I18nKey::DlgFillFgColor.tr_ui(lang));
                        edit_color_compact_sliders(ui, fg, "f11_pat_fg", lang);
                        ui.label(crate::i18n::I18nKey::DlgFillBgColor.tr_ui(lang));
                        edit_color_compact_sliders(ui, bg, "f11_pat_bg", lang);
                    }
                    Fill::TextureFill { preset, fg, bg, .. } => {
                        shell_dir.with_horizontal_wrapped(ui, |ui| {
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
                        ui.label(crate::i18n::I18nKey::DlgFillFg.tr_ui(lang));
                        edit_color_compact_sliders(ui, fg, "f11_tex_fg", lang);
                        ui.label(crate::i18n::I18nKey::DlgFillBg.tr_ui(lang));
                        edit_color_compact_sliders(ui, bg, "f11_tex_bg", lang);
                    }
                    Fill::VectorPattern {
                        symbol_id,
                        tile_w,
                        tile_h,
                    } => {
                        ui.label(
                            RichText::new(format!(
                                "{} ({symbol_id})",
                                crate::i18n::I18nKey::DlgFillVectorPatternFmt.tr_ui(lang)
                            ))
                            .strong(),
                        );
                        shell_dir.with_horizontal(ui, |ui| {
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
                    }
                    Fill::BitmapPattern {
                        image_id,
                        tile_w,
                        tile_h,
                    } => {
                        ui.label(
                            RichText::new(format!(
                                "{} (Asset {image_id})",
                                crate::i18n::I18nKey::DlgFillBitmapPatternFmt.tr_ui(lang)
                            ))
                            .strong(),
                        );
                        shell_dir.with_horizontal(ui, |ui| {
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
                    }
                    _ => {
                        ui.label(format!(
                            "{}: {}",
                            crate::i18n::I18nKey::PropFillMesh.tr_ui(lang),
                            crate::i18n::I18nKey::DlgFillMeshHint.tr_ui(lang)
                        ));
                    }
                }

                ui.separator();
                shell_dir.with_horizontal(ui, |ui| {
                    shell_dir.with_opposite_horizontal(ui, |ui| {
                        if ui
                            .button(crate::i18n::I18nKey::DialogCancel.tr_ui(lang))
                            .clicked()
                        {
                            cancel_clicked = true;
                        }
                        if ui
                            .button(
                                RichText::new(crate::i18n::I18nKey::DialogOk.tr_ui(lang)).strong(),
                            )
                            .clicked()
                        {
                            commit_close = true;
                        }
                        if ui
                            .button(crate::i18n::I18nKey::DialogApply.tr_ui(lang))
                            .clicked()
                        {
                            apply_only = true;
                        }
                    });
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
pub(super) fn show_outline_pen_dialog(ctx: &egui::Context, app: &mut StudioApp) {
    if !app.show_outline_pen_dialog {
        return;
    }
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let mut open = true;
    let mut commit_close = false;
    let mut apply_only = false;
    let mut cancel_clicked = false;

    egui::Window::new(crate::i18n::I18nKey::DlgOutlineWindowTitle.tr_ui(lang))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(510.0, 480.0))
        .show(ctx, |ui| {
            shell_dir.with_vertical(ui, |ui| {
                let o = &mut app.outline_pen_draft;

                ui.label(
                    RichText::new(crate::i18n::I18nKey::PropOutlineColor.tr_ui(lang)).strong(),
                );
                edit_color_compact_sliders(ui, &mut o.color, "f12_color", lang);
                ui.separator();

                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::PropOutlineWidth.tr_ui(lang));
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
                        .selectable_label(
                            !o.is_visible(),
                            crate::i18n::I18nKey::StatusNoneLabel.tr_ui(lang),
                        )
                        .clicked()
                    {
                        o.width_mm = 0.0;
                        o.is_hairline = false;
                    }
                    if ui
                        .selectable_label(
                            o.is_hairline,
                            crate::i18n::I18nKey::PropOutlineHairline.tr_ui(lang),
                        )
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

                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::DlgOutlineDashLabel.tr_ui(lang));
                    if ui
                        .selectable_label(
                            o.dash_pattern.is_empty(),
                            crate::i18n::I18nKey::DlgOutlineDashSolid.tr_ui(lang),
                        )
                        .clicked()
                    {
                        o.dash_pattern.clear();
                    }
                    if ui
                        .selectable_label(
                            o.dash_pattern == vec![4.0, 2.0],
                            crate::i18n::I18nKey::DlgOutlineDashDashed.tr_ui(lang),
                        )
                        .clicked()
                    {
                        o.dash_pattern = vec![4.0, 2.0];
                    }
                    if ui
                        .selectable_label(
                            o.dash_pattern == vec![0.8, 1.5],
                            crate::i18n::I18nKey::DlgOutlineDashDotted.tr_ui(lang),
                        )
                        .clicked()
                    {
                        o.dash_pattern = vec![0.8, 1.5];
                    }
                    if ui
                        .selectable_label(
                            o.dash_pattern == vec![4.0, 1.5, 1.0, 1.5],
                            crate::i18n::I18nKey::DlgOutlineDashDot.tr_ui(lang),
                        )
                        .clicked()
                    {
                        o.dash_pattern = vec![4.0, 1.5, 1.0, 1.5];
                    }
                });

                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::DlgOutlineCornersLabel.tr_ui(lang));
                    ui.selectable_value(
                        &mut o.join,
                        LineJoin::Miter,
                        crate::i18n::I18nKey::DlgOutlineCornerMiter.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut o.join,
                        LineJoin::Round,
                        crate::i18n::I18nKey::TbCornerRound.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut o.join,
                        LineJoin::Bevel,
                        crate::i18n::I18nKey::DlgOutlineCornerBevel.tr_ui(lang),
                    );
                    ui.label(crate::i18n::I18nKey::DlgOutlineMiterLimit.tr_ui(lang));
                    ui.add(
                        DragValue::new(&mut o.miter_limit)
                            .speed(0.5)
                            .range(1.0..=90.0),
                    );
                });

                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::DlgOutlineCapsLabel.tr_ui(lang));
                    ui.selectable_value(
                        &mut o.cap,
                        LineCap::Butt,
                        crate::i18n::I18nKey::DlgOutlineCapButt.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut o.cap,
                        LineCap::Round,
                        crate::i18n::I18nKey::TbCornerRound.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut o.cap,
                        LineCap::Square,
                        crate::i18n::I18nKey::DlgOutlineCapSquare.tr_ui(lang),
                    );
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
                    ArrowheadStyle::None => crate::i18n::I18nKey::StatusNoneLabel.tr_ui(lang),
                    ArrowheadStyle::Triangle => {
                        crate::i18n::I18nKey::DlgOutlineArrowTriangle.tr_ui(lang)
                    }
                    ArrowheadStyle::Stealth => {
                        crate::i18n::I18nKey::DlgOutlineArrowStealth.tr_ui(lang)
                    }
                    ArrowheadStyle::OpenVee => {
                        crate::i18n::I18nKey::DlgOutlineArrowOpenVee.tr_ui(lang)
                    }
                    ArrowheadStyle::Diamond => {
                        crate::i18n::I18nKey::DlgOutlineArrowDiamond.tr_ui(lang)
                    }
                    ArrowheadStyle::Circle => {
                        crate::i18n::I18nKey::DlgOutlineArrowCircle.tr_ui(lang)
                    }
                    ArrowheadStyle::Square => {
                        crate::i18n::I18nKey::DlgOutlineArrowSquare.tr_ui(lang)
                    }
                    ArrowheadStyle::Custom(_) => {
                        crate::i18n::I18nKey::DlgOutlineArrowCustom.tr_ui(lang)
                    }
                };
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::DlgOutlineStartArrow.tr_ui(lang));
                    ComboBox::from_id_salt("f12_start_arrow")
                        .selected_text(arrow_lbl(&o.start_arrow))
                        .show_ui(ui, |ui| {
                            for a in &arrows {
                                ui.selectable_value(&mut o.start_arrow, a.clone(), arrow_lbl(a));
                            }
                        });
                    ui.label(crate::i18n::I18nKey::DlgOutlineEndArrow.tr_ui(lang));
                    ComboBox::from_id_salt("f12_end_arrow")
                        .selected_text(arrow_lbl(&o.end_arrow))
                        .show_ui(ui, |ui| {
                            for a in &arrows {
                                ui.selectable_value(&mut o.end_arrow, a.clone(), arrow_lbl(a));
                            }
                        });
                    if ui
                        .small_button(crate::i18n::I18nKey::DlgOutlineSwapArrows.tr_ui(lang))
                        .clicked()
                    {
                        std::mem::swap(&mut o.start_arrow, &mut o.end_arrow);
                    }
                });

                ui.separator();
                ui.label(
                    RichText::new(crate::i18n::I18nKey::DlgOutlineCalligraphyHdr.tr_ui(lang))
                        .strong(),
                );
                shell_dir.with_horizontal(ui, |ui| {
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
                    let ry =
                        (16.0 / o.calligraphy_stretch.clamp(0.1, 10.0) as f32).clamp(2.5, 18.0);
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
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.label(crate::i18n::I18nKey::DlgOutlineNibAngle.tr_ui(lang));
                            ui.add(
                                egui::Slider::new(&mut o.calligraphy_angle_deg, -180.0..=180.0)
                                    .suffix("°"),
                            );
                        });
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.label(crate::i18n::I18nKey::DlgOutlineStretch.tr_ui(lang));
                            ui.add(egui::Slider::new(&mut o.calligraphy_stretch, 0.2..=5.0));
                        });
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.checkbox(
                                &mut o.behind_fill,
                                crate::i18n::I18nKey::PropOutlineBehindFill.tr_ui(lang),
                            );
                            ui.checkbox(
                                &mut o.scale_with_object,
                                crate::i18n::I18nKey::PropOutlineScaleWithObj.tr_ui(lang),
                            );
                            ui.checkbox(
                                &mut o.overprint,
                                crate::i18n::I18nKey::PropOutlineOverprint.tr_ui(lang),
                            );
                        });
                    });
                });

                ui.separator();
                shell_dir.with_horizontal(ui, |ui| {
                    if ui
                        .button(crate::i18n::I18nKey::DlgOutlineRemove.tr_ui(lang))
                        .clicked()
                    {
                        app.outline_pen_draft = OutlineStyle::none();
                        apply_only = true;
                    }
                    shell_dir.with_opposite_horizontal(ui, |ui| {
                        if ui
                            .button(crate::i18n::I18nKey::DialogCancel.tr_ui(lang))
                            .clicked()
                        {
                            cancel_clicked = true;
                        }
                        if ui
                            .button(
                                RichText::new(crate::i18n::I18nKey::DialogOk.tr_ui(lang)).strong(),
                            )
                            .clicked()
                        {
                            commit_close = true;
                        }
                        if ui
                            .button(crate::i18n::I18nKey::DialogApply.tr_ui(lang))
                            .clicked()
                        {
                            apply_only = true;
                        }
                    });
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
