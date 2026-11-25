// SPDX-License-Identifier: MIT OR Apache-2.0

use super::find_symbols_fonts::ui_glyphs_browser;
use super::*;

pub(super) fn ui_properties_docker(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    shell_dir.with_horizontal(ui, |ui| {
        ui.selectable_value(
            &mut app.properties_state.active_tab,
            PropertiesSubTab::Fill,
            crate::i18n::I18nKey::PropFillType.tr_ui(lang),
        );
        ui.selectable_value(
            &mut app.properties_state.active_tab,
            PropertiesSubTab::Outline,
            crate::i18n::I18nKey::PropOutlineWidth.tr_ui(lang),
        );
        ui.selectable_value(
            &mut app.properties_state.active_tab,
            PropertiesSubTab::Transparency,
            crate::i18n::I18nKey::DockPropTabBlend.tr_ui(lang),
        );
        ui.selectable_value(
            &mut app.properties_state.active_tab,
            PropertiesSubTab::Text,
            crate::i18n::I18nKey::MenuText.tr_ui(lang),
        );
    });
    ui.separator();

    let sel_id = app
        .active_tab()
        .and_then(|t| t.selection.objects.first().copied());

    match app.properties_state.active_tab {
        PropertiesSubTab::Fill => {
            let cur_fill = sel_id
                .and_then(|id| {
                    app.active_tab()?
                        .document
                        .object(id)
                        .map(|o| o.fill.clone())
                })
                .unwrap_or_else(|| app.default_fill.clone());

            if ui
                .button(crate::i18n::I18nKey::DockPropEditFillBtn.tr_ui(lang))
                .clicked()
            {
                app.open_edit_fill_dialog();
            }

            shell_dir.with_horizontal_wrapped(ui, |ui| {
                if ui
                    .selectable_label(
                        matches!(cur_fill, Fill::None),
                        crate::i18n::I18nKey::PropFillNone.tr_ui(lang),
                    )
                    .clicked()
                {
                    app.apply_fill(Fill::None);
                }
                if ui
                    .selectable_label(
                        matches!(cur_fill, Fill::Solid(_)),
                        crate::i18n::I18nKey::PropFillSolid.tr_ui(lang),
                    )
                    .clicked()
                {
                    let c = cur_fill
                        .referenced_colors()
                        .into_iter()
                        .next()
                        .unwrap_or(Color::CYAN);
                    app.apply_fill(Fill::Solid(c));
                }
                if ui
                    .selectable_label(
                        matches!(cur_fill, Fill::LinearGradient { .. }),
                        crate::i18n::I18nKey::TbPressureLinear.tr_ui(lang),
                    )
                    .clicked()
                {
                    app.apply_fill(Fill::LinearGradient {
                        start: Point2D::ZERO,
                        end: Point2D::new(60.0, 40.0),
                        stops: vec![
                            GradientStop::new(0.0, app.properties_state.grad_start_color.clone()),
                            GradientStop::new(1.0, app.properties_state.grad_end_color.clone()),
                        ],
                    });
                }
                if ui
                    .selectable_label(
                        matches!(cur_fill, Fill::RadialGradient { .. }),
                        crate::i18n::I18nKey::TbRadial.tr_ui(lang),
                    )
                    .clicked()
                {
                    app.apply_fill(Fill::RadialGradient {
                        center: Point2D::new(30.0, 30.0),
                        radius: 40.0,
                        focal: Point2D::new(30.0, 30.0),
                        stops: vec![
                            GradientStop::new(0.0, app.properties_state.grad_start_color.clone()),
                            GradientStop::new(1.0, app.properties_state.grad_end_color.clone()),
                        ],
                    });
                }
                if ui
                    .selectable_label(
                        matches!(cur_fill, Fill::ConicalGradient { .. }),
                        crate::i18n::I18nKey::TbConical.tr_ui(lang),
                    )
                    .clicked()
                {
                    app.apply_fill(Fill::ConicalGradient {
                        center: Point2D::new(30.0, 30.0),
                        angle_deg: 0.0,
                        stops: vec![
                            GradientStop::new(0.0, app.properties_state.grad_start_color.clone()),
                            GradientStop::new(1.0, app.properties_state.grad_end_color.clone()),
                        ],
                    });
                }
                if ui
                    .selectable_label(
                        matches!(cur_fill, Fill::SquareGradient { .. }),
                        crate::i18n::I18nKey::TbSquare.tr_ui(lang),
                    )
                    .clicked()
                {
                    app.apply_fill(Fill::SquareGradient {
                        center: Point2D::new(30.0, 30.0),
                        size: 50.0,
                        angle_deg: 0.0,
                        stops: vec![
                            GradientStop::new(0.0, app.properties_state.grad_start_color.clone()),
                            GradientStop::new(1.0, app.properties_state.grad_end_color.clone()),
                        ],
                    });
                }
                if ui
                    .selectable_label(
                        matches!(cur_fill, Fill::TwoColorPattern { .. }),
                        crate::i18n::I18nKey::PropFillTwoColor.tr_ui(lang),
                    )
                    .clicked()
                {
                    app.apply_fill(Fill::TwoColorPattern {
                        fg: Color::CYAN,
                        bg: Color::WHITE,
                        pattern_name: "Checker".to_owned(),
                        tile_w: 8.0,
                        tile_h: 8.0,
                    });
                }
                if ui
                    .selectable_label(
                        matches!(cur_fill, Fill::VectorPattern { .. }),
                        crate::i18n::I18nKey::DockPropVecPatShort.tr_ui(lang),
                    )
                    .clicked()
                {
                    let sym_id = app
                        .active_tab()
                        .and_then(|t| t.document.symbols().keys().next().copied())
                        .unwrap_or_default();
                    app.apply_fill(Fill::VectorPattern {
                        symbol_id: sym_id,
                        tile_w: 12.0,
                        tile_h: 12.0,
                    });
                }
                if ui
                    .selectable_label(
                        matches!(cur_fill, Fill::BitmapPattern { .. }),
                        crate::i18n::I18nKey::DockPropBmpPatShort.tr_ui(lang),
                    )
                    .clicked()
                {
                    let img_id = app
                        .active_tab()
                        .and_then(|t| t.document.assets().keys().next().copied())
                        .unwrap_or_default();
                    app.apply_fill(Fill::BitmapPattern {
                        image_id: img_id,
                        tile_w: 16.0,
                        tile_h: 16.0,
                    });
                }
                let is_ps_fill = matches!(
                    &cur_fill,
                    Fill::TextureFill { preset, .. }
                        if PostScriptFillPreset::from_preset_str(preset).is_some()
                );
                if ui
                    .selectable_label(
                        matches!(cur_fill, Fill::TextureFill { .. }) && !is_ps_fill,
                        crate::i18n::I18nKey::DockPropTextureShort.tr_ui(lang),
                    )
                    .clicked()
                {
                    app.apply_fill(Fill::TextureFill {
                        preset: "Marble".to_owned(),
                        seed: 7,
                        fg: Color::CYAN,
                        bg: Color::WHITE,
                    });
                }
                if ui
                    .selectable_label(
                        is_ps_fill,
                        crate::i18n::I18nKey::DockPropPostScriptShort.tr_ui(lang),
                    )
                    .clicked()
                {
                    let fg = cur_fill
                        .referenced_colors()
                        .into_iter()
                        .next()
                        .unwrap_or(Color::KEY_BLACK);
                    app.apply_fill(PostScriptFillPreset::Hatch.to_fill(fg, Color::WHITE, 1));
                }
                if ui
                    .selectable_label(
                        matches!(cur_fill, Fill::MeshFill { .. }),
                        crate::i18n::I18nKey::DockPropMeshShort.tr_ui(lang),
                    )
                    .clicked()
                {
                    let local_bb = sel_id
                        .and_then(|id| {
                            app.active_tab()
                                .and_then(|t| StudioApp::object_local_bounds(&t.document, id))
                        })
                        .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 50.0, 50.0));
                    let fill = StudioApp::build_mesh_fill_for_bounds(
                        local_bb,
                        app.properties_state.mesh_rows,
                        app.properties_state.mesh_cols,
                    );
                    app.apply_fill(fill);
                }
            });

            ui.separator();
            // Color Model Editors (CMYK / RGB / HSL / Spot / Hex)
            let mut base_color = cur_fill
                .referenced_colors()
                .into_iter()
                .next()
                .unwrap_or(Color::CYAN);

            shell_dir.with_horizontal(ui, |ui| {
                ui.selectable_value(&mut app.properties_state.color_space_tab, 0, "CMYK");
                ui.selectable_value(&mut app.properties_state.color_space_tab, 1, "RGB");
                ui.selectable_value(&mut app.properties_state.color_space_tab, 2, "HSL");
                ui.selectable_value(
                    &mut app.properties_state.color_space_tab,
                    3,
                    crate::i18n::I18nKey::DockPropSpotTab.tr_ui(lang),
                );
            });

            let mut color_changed = false;
            match app.properties_state.color_space_tab {
                0 => {
                    let [mut c, mut m, mut y, mut k] = base_color.to_cmyk();
                    let (mut cp, mut mp, mut yp, mut kp) =
                        (c * 100.0, m * 100.0, y * 100.0, k * 100.0);
                    color_changed |= ui
                        .add(egui::Slider::new(&mut cp, 0.0..=100.0).text("C %"))
                        .changed();
                    color_changed |= ui
                        .add(egui::Slider::new(&mut mp, 0.0..=100.0).text("M %"))
                        .changed();
                    color_changed |= ui
                        .add(egui::Slider::new(&mut yp, 0.0..=100.0).text("Y %"))
                        .changed();
                    color_changed |= ui
                        .add(egui::Slider::new(&mut kp, 0.0..=100.0).text("K %"))
                        .changed();
                    if color_changed {
                        c = cp / 100.0;
                        m = mp / 100.0;
                        y = yp / 100.0;
                        k = kp / 100.0;
                        base_color = Color::cmyk(c, m, y, k);
                    }
                }
                1 => {
                    let mut c32 = core_color_to_egui(&base_color);
                    if ui.color_edit_button_srgba(&mut c32).changed() {
                        base_color = egui_color_to_core(c32);
                        color_changed = true;
                    }
                    shell_dir.with_horizontal(ui, |ui| {
                        ui.label(crate::i18n::I18nKey::PropFillHexLabel.tr_ui(lang));
                        ui.text_edit_singleline(&mut app.properties_state.hex_input);
                        if ui
                            .small_button(crate::i18n::I18nKey::DialogApply.tr_ui(lang))
                            .clicked()
                        {
                            if let Ok(parsed) = Color::from_hex(&app.properties_state.hex_input) {
                                base_color = parsed;
                                color_changed = true;
                            }
                        }
                    });
                }
                2 => {
                    let [mut h, mut s, mut l] = base_color.to_hsl();
                    color_changed |= ui
                        .add(
                            egui::Slider::new(&mut h, 0.0..=360.0)
                                .text(crate::i18n::I18nKey::DockPropHue.tr_ui(lang)),
                        )
                        .changed();
                    color_changed |= ui
                        .add(
                            egui::Slider::new(&mut s, 0.0..=1.0)
                                .text(crate::i18n::I18nKey::DockPropSat.tr_ui(lang)),
                        )
                        .changed();
                    color_changed |= ui
                        .add(
                            egui::Slider::new(&mut l, 0.0..=1.0)
                                .text(crate::i18n::I18nKey::DockPropLight.tr_ui(lang)),
                        )
                        .changed();
                    if color_changed {
                        base_color = Color::hsl(h, s, l);
                    }
                }
                _ => {
                    shell_dir.with_horizontal(ui, |ui| {
                        ui.label(crate::i18n::I18nKey::PropFillSpotLabel.tr_ui(lang));
                        ui.text_edit_singleline(&mut app.properties_state.spot_name);
                    });
                    ui.add(
                        egui::Slider::new(&mut app.properties_state.spot_tint, 0.0..=1.0)
                            .text(crate::i18n::I18nKey::DockPropTint.tr_ui(lang)),
                    );
                    if ui
                        .button(crate::i18n::I18nKey::DialogApply.tr_ui(lang))
                        .clicked()
                    {
                        let cmyk = base_color.to_cmyk();
                        base_color = Color::spot(
                            app.properties_state.spot_name.clone(),
                            app.properties_state.spot_tint,
                            cmyk,
                        );
                        color_changed = true;
                    }
                }
            }
            if color_changed {
                app.apply_fill_color(base_color);
            }

            // Gradient Stops editor
            if matches!(
                cur_fill,
                Fill::LinearGradient { .. }
                    | Fill::RadialGradient { .. }
                    | Fill::ConicalGradient { .. }
                    | Fill::SquareGradient { .. }
            ) {
                ui.separator();
                ui.label(
                    RichText::new(crate::i18n::I18nKey::DockPropGradColors.tr_ui(lang)).strong(),
                );
                let mut sc = core_color_to_egui(&app.properties_state.grad_start_color);
                let mut ec = core_color_to_egui(&app.properties_state.grad_end_color);
                shell_dir.with_horizontal(ui, |ui| {
                    ui.label(crate::i18n::I18nKey::PropFillStartLabel.tr_ui(lang));
                    let c1 = ui.color_edit_button_srgba(&mut sc).changed();
                    ui.label(crate::i18n::I18nKey::PropFillEndLabel.tr_ui(lang));
                    let c2 = ui.color_edit_button_srgba(&mut ec).changed();
                    if c1 || c2 {
                        app.properties_state.grad_start_color = egui_color_to_core(sc);
                        app.properties_state.grad_end_color = egui_color_to_core(ec);
                        let mut updated = cur_fill.clone();
                        let new_stops = vec![
                            GradientStop::new(0.0, app.properties_state.grad_start_color.clone()),
                            GradientStop::new(1.0, app.properties_state.grad_end_color.clone()),
                        ];
                        match &mut updated {
                            Fill::LinearGradient { stops, .. }
                            | Fill::RadialGradient { stops, .. }
                            | Fill::ConicalGradient { stops, .. }
                            | Fill::SquareGradient { stops, .. } => *stops = new_stops,
                            _ => {}
                        }
                        app.apply_fill(updated);
                    }
                });
            }

            // VectorPattern & BitmapPattern parameter editors (`ADD-12`)
            if let Fill::VectorPattern {
                mut symbol_id,
                mut tile_w,
                mut tile_h,
            } = cur_fill
            {
                ui.separator();
                ui.label(
                    RichText::new(crate::i18n::I18nKey::PropFillVectorPattern.tr_ui(lang))
                        .strong()
                        .color(tokens.accent),
                );
                let syms: Vec<(coree_core::SymbolId, String)> = app
                    .active_tab()
                    .map(|t| {
                        t.document
                            .symbols()
                            .iter()
                            .map(|(&id, s)| (id, s.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut changed = false;
                ComboBox::from_id_salt("prop_vecpat_sym")
                    .selected_text(
                        syms.iter()
                            .find(|(id, _)| *id == symbol_id)
                            .map_or_else(|| format!("Sym #{}", symbol_id.0), |(_, n)| n.clone()),
                    )
                    .show_ui(ui, |ui| {
                        for (sid, sname) in &syms {
                            if ui.selectable_value(&mut symbol_id, *sid, sname).changed() {
                                changed = true;
                            }
                        }
                    });
                shell_dir.with_horizontal(ui, |ui| {
                    changed |= ui
                        .add(
                            DragValue::new(&mut tile_w)
                                .range(1.0..=500.0)
                                .prefix("W: ")
                                .suffix(" mm"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            DragValue::new(&mut tile_h)
                                .range(1.0..=500.0)
                                .prefix("H: ")
                                .suffix(" mm"),
                        )
                        .changed();
                });
                if changed {
                    app.apply_fill(Fill::VectorPattern {
                        symbol_id,
                        tile_w,
                        tile_h,
                    });
                }
            }
            if let Fill::BitmapPattern {
                mut image_id,
                mut tile_w,
                mut tile_h,
            } = cur_fill
            {
                ui.separator();
                ui.label(
                    RichText::new(crate::i18n::I18nKey::PropFillBitmapPattern.tr_ui(lang))
                        .strong()
                        .color(tokens.accent),
                );
                let assets: Vec<coree_core::ImageAssetId> = app
                    .active_tab()
                    .map(|t| t.document.assets().keys().copied().collect())
                    .unwrap_or_default();
                let mut changed = false;
                ComboBox::from_id_salt("prop_bmppat_asset")
                    .selected_text(format!("{image_id}"))
                    .show_ui(ui, |ui| {
                        for aid in &assets {
                            if ui
                                .selectable_value(&mut image_id, *aid, format!("{aid}"))
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                shell_dir.with_horizontal(ui, |ui| {
                    changed |= ui
                        .add(
                            DragValue::new(&mut tile_w)
                                .range(1.0..=500.0)
                                .prefix("W: ")
                                .suffix(" mm"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            DragValue::new(&mut tile_h)
                                .range(1.0..=500.0)
                                .prefix("H: ")
                                .suffix(" mm"),
                        )
                        .changed();
                });
                if changed {
                    app.apply_fill(Fill::BitmapPattern {
                        image_id,
                        tile_w,
                        tile_h,
                    });
                }
            }

            // PostScript Procedural Pattern Fill editor (all 10 clean-room presets)
            if let Fill::TextureFill {
                ref preset,
                mut seed,
                ref fg,
                ref bg,
            } = cur_fill
            {
                if let Some(mut active_ps) = PostScriptFillPreset::from_preset_str(preset) {
                    ui.separator();
                    ui.label(
                        RichText::new(crate::i18n::I18nKey::PropFillTexture.tr_ui(lang))
                            .strong()
                            .color(tokens.accent),
                    );
                    let mut ps_changed = false;
                    let mut next_fg = fg.clone();
                    let mut next_bg = bg.clone();

                    shell_dir.with_horizontal_wrapped(ui, |ui| {
                        for p in PostScriptFillPreset::ALL {
                            if ui
                                .selectable_label(active_ps == p, p.display_name())
                                .clicked()
                            {
                                active_ps = p;
                                ps_changed = true;
                            }
                        }
                    });
                    shell_dir.with_horizontal(ui, |ui| {
                        ui.label(crate::i18n::I18nKey::DlgFillFg.tr_ui(lang));
                        let mut fg32 = core_color_to_egui(&next_fg);
                        if ui.color_edit_button_srgba(&mut fg32).changed() {
                            next_fg = egui_color_to_core(fg32);
                            ps_changed = true;
                        }
                        ui.label(crate::i18n::I18nKey::DlgFillBg.tr_ui(lang));
                        let mut bg32 = core_color_to_egui(&next_bg);
                        if ui.color_edit_button_srgba(&mut bg32).changed() {
                            next_bg = egui_color_to_core(bg32);
                            ps_changed = true;
                        }
                        ui.label(crate::i18n::I18nKey::DockPropSeed.tr_ui(lang));
                        if ui.add(DragValue::new(&mut seed).range(0..=9999)).changed() {
                            ps_changed = true;
                        }
                        if ui
                            .small_button("↻")
                            .on_hover_text(crate::i18n::I18nKey::DockPropNextSeed.tr_ui(lang))
                            .clicked()
                        {
                            seed = seed.wrapping_add(1) % 10_000;
                            ps_changed = true;
                        }
                    });
                    if ps_changed {
                        app.apply_fill(active_ps.to_fill(next_fg, next_bg, seed));
                    }
                }
            }
        }
        PropertiesSubTab::Outline => {
            if ui
                .button(crate::i18n::I18nKey::DockPropOutlineDlgBtn.tr_ui(lang))
                .clicked()
            {
                app.open_outline_pen_dialog();
            }
            let mut outline = sel_id
                .and_then(|id| {
                    app.active_tab()?
                        .document
                        .object(id)
                        .map(|o| o.outline.clone())
                })
                .unwrap_or_else(|| app.default_outline.clone());
            let mut changed = false;

            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DockPropColorLabel.tr_ui(lang));
                let mut c32 = core_color_to_egui(&outline.color);
                if ui.color_edit_button_srgba(&mut c32).changed() {
                    outline.color = egui_color_to_core(c32);
                    changed = true;
                }
                if ui
                    .small_button(crate::i18n::I18nKey::DockPropCutSpotBtn.tr_ui(lang))
                    .clicked()
                {
                    outline = OutlineStyle::cut_contour();
                    changed = true;
                }
            });

            changed |= ui
                .add(
                    egui::Slider::new(&mut outline.width_mm, 0.0..=25.0)
                        .text(crate::i18n::I18nKey::DockPropWidthMm.tr_ui(lang)),
                )
                .changed();
            changed |= ui
                .checkbox(
                    &mut outline.is_hairline,
                    crate::i18n::I18nKey::DockPropHairlineFull.tr_ui(lang),
                )
                .changed();
            changed |= ui
                .checkbox(
                    &mut outline.behind_fill,
                    crate::i18n::I18nKey::PropOutlineBehindFill.tr_ui(lang),
                )
                .changed();
            changed |= ui
                .checkbox(
                    &mut outline.scale_with_object,
                    crate::i18n::I18nKey::PropOutlineScaleWithObj.tr_ui(lang),
                )
                .changed();
            changed |= ui
                .checkbox(&mut outline.overprint, "Overprint Outline")
                .changed();

            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DockPropCapLabel.tr_ui(lang));
                for cap in [
                    LineCap::Butt,
                    LineCap::Round,
                    LineJoinCapHelper::Square.cap(),
                ] {
                    if ui
                        .selectable_label(outline.cap == cap, format!("{cap:?}"))
                        .clicked()
                    {
                        outline.cap = cap;
                        changed = true;
                    }
                }
            });
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DockPropJoinLabel.tr_ui(lang));
                for join in [LineJoin::Miter, LineJoin::Round, LineJoin::Bevel] {
                    if ui
                        .selectable_label(outline.join == join, format!("{join:?}"))
                        .clicked()
                    {
                        outline.join = join;
                        changed = true;
                    }
                }
            });
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DockPropDashLabel.tr_ui(lang));
                if ui
                    .small_button(crate::i18n::I18nKey::DlgOutlineDashSolid.tr_ui(lang))
                    .clicked()
                {
                    outline.dash_pattern.clear();
                    changed = true;
                }
                if ui
                    .small_button(crate::i18n::I18nKey::DlgOutlineDashDashed.tr_ui(lang))
                    .clicked()
                {
                    outline.dash_pattern = vec![3.0, 2.0];
                    changed = true;
                }
                if ui
                    .small_button(crate::i18n::I18nKey::DlgOutlineDashDotted.tr_ui(lang))
                    .clicked()
                {
                    outline.dash_pattern = vec![0.8, 1.5];
                    changed = true;
                }
            });
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DlgOutlineEndArrow.tr_ui(lang));
                ComboBox::from_id_salt("outline_end_arrow")
                    .selected_text(format!("{:?}", outline.end_arrow))
                    .show_ui(ui, |ui| {
                        for a in [
                            ArrowheadStyle::None,
                            ArrowheadStyle::Triangle,
                            ArrowheadStyle::OpenVee,
                            ArrowheadStyle::Diamond,
                            ArrowheadStyle::Circle,
                            ArrowheadStyle::Stealth,
                        ] {
                            if ui
                                .selectable_value(
                                    &mut outline.end_arrow,
                                    a.clone(),
                                    format!("{a:?}"),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
            });

            if changed {
                app.apply_outline(outline);
            }
        }
        PropertiesSubTab::Transparency => {
            if let Some(id) = sel_id {
                let current = app
                    .active_tab()
                    .and_then(|t| t.document.object(id))
                    .map(|o| (o.opacity, o.blend_mode));
                if let Some((mut opacity, mut blend_mode)) = current {
                    let op_changed = ui
                        .add(
                            egui::Slider::new(&mut opacity, 0.0..=1.0)
                                .text(crate::i18n::I18nKey::DockEffectsAlpha.tr_ui(lang)),
                        )
                        .changed();
                    let mut bm_changed = false;
                    ComboBox::from_id_salt("obj_blend_mode")
                        .selected_text(format!("{blend_mode:?}"))
                        .show_ui(ui, |ui| {
                            for bm in [
                                BlendMode::Normal,
                                BlendMode::Multiply,
                                BlendMode::Screen,
                                BlendMode::Overlay,
                                BlendMode::Darken,
                                BlendMode::Lighten,
                                BlendMode::Difference,
                                BlendMode::Exclusion,
                                BlendMode::SoftLight,
                                BlendMode::HardLight,
                                BlendMode::ColorDodge,
                                BlendMode::ColorBurn,
                            ] {
                                if ui
                                    .selectable_value(&mut blend_mode, bm, format!("{bm:?}"))
                                    .changed()
                                {
                                    bm_changed = true;
                                }
                            }
                        });
                    if op_changed {
                        let _ = app.execute_command(CommandOp::SetOpacity {
                            ids: vec![id],
                            opacity,
                        });
                    }
                    if bm_changed {
                        let _ = app.execute_command(CommandOp::SetBlendMode {
                            ids: vec![id],
                            blend_mode,
                        });
                    }
                }
            } else {
                ui.label(
                    RichText::new(crate::i18n::I18nKey::DockPropSelectForOpacity.tr_ui(lang))
                        .color(tokens.text_muted),
                );
            }
        }
        PropertiesSubTab::Text => {
            ui.label(RichText::new(crate::i18n::I18nKey::DockTextHeading.tr_ui(lang)).strong());
            let font_records = app.font_manager_state.indexed_fonts.clone();
            let mut edited_selected_text = false;
            if let Some(id) = sel_id {
                let text_edit_payload = app
                    .active_tab()
                    .and_then(|t| t.document.object(id))
                    .map(|o| o.payload.clone());
                match text_edit_payload {
                    Some(NodePayload::ArtisticText(mut art)) => {
                        let mut changed = false;
                        if let Some(run) = art.runs.first_mut() {
                            changed |= ui.text_edit_multiline(&mut run.text).changed();
                            shell_dir.with_horizontal_wrapped(ui, |ui| {
                                ui.label(
                                    RichText::new(
                                        crate::i18n::I18nKey::DockTextDynamicField.tr_ui(lang),
                                    )
                                    .size(10.5),
                                );
                                for (fld, lbl_key) in [
                                    (None, crate::i18n::I18nKey::DockTextFieldStatic),
                                    (
                                        Some(coree_core::TextField::AutoPageNumber),
                                        crate::i18n::I18nKey::TextInsertPageNumber,
                                    ),
                                    (
                                        Some(coree_core::TextField::PageCount),
                                        crate::i18n::I18nKey::TextInsertPageCount,
                                    ),
                                    (
                                        Some(coree_core::TextField::DocumentTitle),
                                        crate::i18n::I18nKey::TextInsertDocTitle,
                                    ),
                                    (
                                        Some(coree_core::TextField::CurrentDate),
                                        crate::i18n::I18nKey::TextInsertCurrentDate,
                                    ),
                                ] {
                                    if ui
                                        .selectable_label(run.field == fld, lbl_key.tr_ui(lang))
                                        .clicked()
                                    {
                                        run.field = fld;
                                        if matches!(
                                            fld,
                                            Some(coree_core::TextField::AutoPageNumber)
                                        ) && run.text.is_empty()
                                        {
                                            run.text = "Page #".to_owned();
                                        }
                                        changed = true;
                                    }
                                }
                            });
                            changed |= render_docker_text_style_fields(
                                ui,
                                &mut run.style,
                                Some(&mut art.direction),
                                Some(&mut art.align),
                                &font_records,
                                &mut app.properties_state.new_fvar_tag,
                                &mut app.properties_state.new_fvar_val,
                                "art",
                                lang,
                                shell_dir,
                            );
                        }
                        if changed {
                            let _ = app.execute_command(CommandOp::EditText {
                                id,
                                payload: NodePayload::ArtisticText(art),
                            });
                        }
                        edited_selected_text = true;
                    }
                    Some(NodePayload::ParagraphText(mut para)) => {
                        let mut changed = false;
                        if let Some(run) = para.runs.first_mut() {
                            changed |= ui.text_edit_multiline(&mut run.text).changed();
                            shell_dir.with_horizontal_wrapped(ui, |ui| {
                                ui.label(
                                    RichText::new(
                                        crate::i18n::I18nKey::DockTextDynamicField.tr_ui(lang),
                                    )
                                    .size(10.5),
                                );
                                for (fld, lbl_key) in [
                                    (None, crate::i18n::I18nKey::DockTextFieldStatic),
                                    (
                                        Some(coree_core::TextField::AutoPageNumber),
                                        crate::i18n::I18nKey::TextInsertPageNumber,
                                    ),
                                    (
                                        Some(coree_core::TextField::PageCount),
                                        crate::i18n::I18nKey::TextInsertPageCount,
                                    ),
                                    (
                                        Some(coree_core::TextField::DocumentTitle),
                                        crate::i18n::I18nKey::TextInsertDocTitle,
                                    ),
                                    (
                                        Some(coree_core::TextField::CurrentDate),
                                        crate::i18n::I18nKey::TextInsertCurrentDate,
                                    ),
                                ] {
                                    if ui
                                        .selectable_label(run.field == fld, lbl_key.tr_ui(lang))
                                        .clicked()
                                    {
                                        run.field = fld;
                                        changed = true;
                                    }
                                }
                            });
                            changed |= render_docker_text_style_fields(
                                ui,
                                &mut run.style,
                                Some(&mut para.direction),
                                Some(&mut para.align),
                                &font_records,
                                &mut app.properties_state.new_fvar_tag,
                                &mut app.properties_state.new_fvar_val,
                                "para",
                                lang,
                                shell_dir,
                            );
                        }
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.label(crate::i18n::I18nKey::DockTextFrame.tr_ui(lang));
                            changed |= ui
                                .add(
                                    DragValue::new(&mut para.frame_size.x)
                                        .range(10.0..=2000.0)
                                        .prefix("W: ")
                                        .suffix(" mm"),
                                )
                                .changed();
                            changed |= ui
                                .add(
                                    DragValue::new(&mut para.frame_size.y)
                                        .range(10.0..=2000.0)
                                        .prefix("H: ")
                                        .suffix(" mm"),
                                )
                                .changed();
                        });
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.label(crate::i18n::I18nKey::TextColumnCount.tr_ui(lang));
                            changed |= ui
                                .add(DragValue::new(&mut para.columns).range(1..=6))
                                .changed();
                            changed |= ui
                                .add(
                                    DragValue::new(&mut para.column_gutter_mm)
                                        .range(0.0..=50.0)
                                        .prefix(crate::i18n::I18nKey::DockTextGutter.tr_ui(lang))
                                        .suffix(" mm"),
                                )
                                .changed();
                        });
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.label(crate::i18n::I18nKey::DockTextWrap.tr_ui(lang));
                            ComboBox::from_id_salt("para_wrap_combo")
                                .width(120.0)
                                .selected_text(format!("{:?}", para.wrap_mode))
                                .show_ui(ui, |ui| {
                                    for wm in [
                                        coree_core::TextWrapMode::None,
                                        coree_core::TextWrapMode::Straddle,
                                        coree_core::TextWrapMode::ContourLeft,
                                        coree_core::TextWrapMode::ContourRight,
                                        coree_core::TextWrapMode::SquareBoth,
                                        coree_core::TextWrapMode::AboveBelow,
                                    ] {
                                        if ui
                                            .selectable_value(
                                                &mut para.wrap_mode,
                                                wm,
                                                format!("{wm:?}"),
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                        });
                        shell_dir.with_horizontal(ui, |ui| {
                            let mut dc = para.drop_cap.unwrap_or_default();
                            if ui
                                .checkbox(
                                    &mut dc.enabled,
                                    crate::i18n::I18nKey::TextDropCap.tr_ui(lang),
                                )
                                .changed()
                            {
                                para.drop_cap = Some(dc);
                                changed = true;
                            }
                            if dc.enabled
                                && ui
                                    .add(
                                        DragValue::new(&mut dc.lines).range(2..=6).prefix(
                                            crate::i18n::I18nKey::DockTextLines.tr_ui(lang),
                                        ),
                                    )
                                    .changed()
                            {
                                para.drop_cap = Some(dc);
                                changed = true;
                            }
                        });
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.label(crate::i18n::I18nKey::DockTextList.tr_ui(lang));
                            if ui
                                .selectable_label(
                                    para.list_style.is_none()
                                        || matches!(
                                            para.list_style,
                                            Some(coree_core::ParagraphListStyle::None)
                                        ),
                                    "None",
                                )
                                .clicked()
                            {
                                para.list_style = None;
                                changed = true;
                            }
                            if ui
                                .selectable_label(
                                    matches!(
                                        para.list_style,
                                        Some(coree_core::ParagraphListStyle::Bullet(_))
                                    ),
                                    crate::i18n::I18nKey::DockTextListBullet.tr_ui(lang),
                                )
                                .clicked()
                            {
                                para.list_style =
                                    Some(coree_core::ParagraphListStyle::Bullet("•".to_owned()));
                                changed = true;
                            }
                            if ui
                                .selectable_label(
                                    matches!(
                                        para.list_style,
                                        Some(coree_core::ParagraphListStyle::Numbered { .. })
                                    ),
                                    crate::i18n::I18nKey::DockTextListNum.tr_ui(lang),
                                )
                                .clicked()
                            {
                                para.list_style = Some(coree_core::ParagraphListStyle::Numbered {
                                    start: 1,
                                    suffix: ".".to_owned(),
                                });
                                changed = true;
                            }
                        });
                        // Linked Next Frame selector (guards self-links & multi-hop cycles)
                        let link_candidates: Vec<(ObjectId, String)> = app
                            .active_tab()
                            .and_then(|tab| {
                                let page = tab.document.active_page()?;
                                let mut out = Vec::new();
                                for layer in &page.layers {
                                    for &oid in &layer.objects {
                                        if oid == id {
                                            continue;
                                        }
                                        if let Some(obj) = tab.document.object(oid) {
                                            if matches!(obj.payload, NodePayload::ParagraphText(_))
                                                && !would_create_frame_link_cycle(
                                                    &tab.document,
                                                    id,
                                                    oid,
                                                )
                                            {
                                                out.push((
                                                    oid,
                                                    format!("{} (#{})", obj.name, oid.0),
                                                ));
                                            }
                                        }
                                    }
                                }
                                Some(out)
                            })
                            .unwrap_or_default();
                        let cur_link_label = para
                            .linked_next_frame
                            .and_then(|lid| {
                                link_candidates
                                    .iter()
                                    .find(|(oid, _)| *oid == lid)
                                    .map(|(_, lbl)| lbl.clone())
                                    .or_else(|| Some(format!("#{}", lid.0)))
                            })
                            .unwrap_or_else(|| "(None)".to_owned());
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.label(crate::i18n::I18nKey::TextLinkFrame.tr_ui(lang));
                            ComboBox::from_id_salt("para_linked_next_frame")
                                .width(140.0)
                                .selected_text(cur_link_label)
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_value(
                                            &mut para.linked_next_frame,
                                            None,
                                            crate::i18n::I18nKey::TextUnlinkFrame.tr_ui(lang),
                                        )
                                        .changed()
                                    {
                                        changed = true;
                                    }
                                    for (cand_id, label) in &link_candidates {
                                        if ui
                                            .selectable_value(
                                                &mut para.linked_next_frame,
                                                Some(*cand_id),
                                                label,
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                        });
                        // Tab Stops (mm) editor
                        ui.label(
                            RichText::new(crate::i18n::I18nKey::DockTextTabStops.tr_ui(lang))
                                .small(),
                        );
                        let mut remove_tab_idx: Option<usize> = None;
                        for (t_idx, ts) in para.tab_stops_mm.iter_mut().enumerate() {
                            shell_dir.with_horizontal(ui, |ui| {
                                changed |= ui
                                    .add(
                                        DragValue::new(&mut ts.position_mm)
                                            .range(0.0..=1000.0)
                                            .suffix(" mm"),
                                    )
                                    .changed();
                                ComboBox::from_id_salt(("para_tab_align", t_idx))
                                    .width(72.0)
                                    .selected_text(format!("{:?}", ts.align))
                                    .show_ui(ui, |ui| {
                                        for al in [
                                            coree_core::ParagraphTabAlign::Left,
                                            coree_core::ParagraphTabAlign::Center,
                                            coree_core::ParagraphTabAlign::Right,
                                        ] {
                                            if ui
                                                .selectable_value(
                                                    &mut ts.align,
                                                    al,
                                                    format!("{al:?}"),
                                                )
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                        }
                                    });
                                if ui.small_button("×").clicked() {
                                    remove_tab_idx = Some(t_idx);
                                }
                            });
                        }
                        if let Some(ri) = remove_tab_idx {
                            para.tab_stops_mm.remove(ri);
                            changed = true;
                        }
                        shell_dir.with_horizontal(ui, |ui| {
                            ui.add(
                                DragValue::new(&mut app.properties_state.new_tab_stop_mm)
                                    .range(0.0..=1000.0)
                                    .prefix(crate::i18n::I18nKey::DockTextPosPrefix.tr_ui(lang))
                                    .suffix(" mm"),
                            );
                            ComboBox::from_id_salt("para_new_tab_align")
                                .width(72.0)
                                .selected_text(format!(
                                    "{:?}",
                                    app.properties_state.new_tab_stop_align
                                ))
                                .show_ui(ui, |ui| {
                                    for al in [
                                        coree_core::ParagraphTabAlign::Left,
                                        coree_core::ParagraphTabAlign::Center,
                                        coree_core::ParagraphTabAlign::Right,
                                    ] {
                                        ui.selectable_value(
                                            &mut app.properties_state.new_tab_stop_align,
                                            al,
                                            format!("{al:?}"),
                                        );
                                    }
                                });
                            if ui
                                .small_button(crate::i18n::I18nKey::DockTextAddTab.tr_ui(lang))
                                .clicked()
                            {
                                para.tab_stops_mm.push(coree_core::ParagraphTabStop {
                                    position_mm: app.properties_state.new_tab_stop_mm.max(0.0),
                                    align: app.properties_state.new_tab_stop_align,
                                });
                                para.tab_stops_mm.sort_by(|a, b| {
                                    a.position_mm
                                        .partial_cmp(&b.position_mm)
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                });
                                changed = true;
                            }
                        });
                        if changed {
                            let _ = app.execute_command(CommandOp::EditText {
                                id,
                                payload: NodePayload::ParagraphText(para),
                            });
                        }
                        edited_selected_text = true;
                    }
                    Some(NodePayload::TextOnPath(mut top)) => {
                        let mut changed = false;
                        if let Some(run) = top.text.runs.first_mut() {
                            changed |= ui.text_edit_singleline(&mut run.text).changed();
                            changed |= render_docker_text_style_fields(
                                ui,
                                &mut run.style,
                                Some(&mut top.text.direction),
                                Some(&mut top.text.align),
                                &font_records,
                                &mut app.properties_state.new_fvar_tag,
                                &mut app.properties_state.new_fvar_val,
                                "top",
                                lang,
                                shell_dir,
                            );
                        }
                        shell_dir.with_horizontal(ui, |ui| {
                            changed |= ui
                                .add(
                                    DragValue::new(&mut top.offset_mm)
                                        .range(-1000.0..=1000.0)
                                        .prefix("Offset: ")
                                        .suffix(" mm"),
                                )
                                .changed();
                            changed |= ui
                                .add(
                                    DragValue::new(&mut top.distance_mm)
                                        .range(-100.0..=100.0)
                                        .prefix("Dist: ")
                                        .suffix(" mm"),
                                )
                                .changed();
                        });
                        shell_dir.with_horizontal(ui, |ui| {
                            changed |= ui
                                .checkbox(
                                    &mut top.flip_horizontal,
                                    crate::i18n::I18nKey::DockTextFlipH.tr_ui(lang),
                                )
                                .changed();
                            changed |= ui
                                .checkbox(
                                    &mut top.flip_vertical,
                                    crate::i18n::I18nKey::DockTextFlipV.tr_ui(lang),
                                )
                                .changed();
                        });
                        if changed {
                            let _ = app.execute_command(CommandOp::EditText {
                                id,
                                payload: NodePayload::TextOnPath(top),
                            });
                        }
                        edited_selected_text = true;
                    }
                    _ => {}
                }
            }
            if !edited_selected_text {
                ui.label(
                    RichText::new(crate::i18n::I18nKey::DockTextSelectOrDefault.tr_ui(lang))
                        .color(tokens.text_muted),
                );
                ui.text_edit_singleline(&mut app.tool_settings.text_insert_draft);
                let _ = render_docker_text_style_fields(
                    ui,
                    &mut app.default_text_style,
                    None,
                    None,
                    &font_records,
                    &mut app.properties_state.new_fvar_tag,
                    &mut app.properties_state.new_fvar_val,
                    "default_txt",
                    lang,
                    shell_dir,
                );
            }
            ui.separator();
            ui_glyphs_browser(ui, app, "props_text_glyphs");
        }
    }
}

fn would_create_frame_link_cycle(
    doc: &coree_core::Document,
    src_id: ObjectId,
    target_id: ObjectId,
) -> bool {
    let mut visited = std::collections::HashSet::from([src_id]);
    let mut cur = Some(target_id);
    while let Some(cid) = cur {
        if !visited.insert(cid) {
            return true;
        }
        cur = doc.object(cid).and_then(|o| match &o.payload {
            NodePayload::ParagraphText(p) => p.linked_next_frame,
            _ => None,
        });
    }
    false
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_docker_text_style_fields(
    ui: &mut Ui,
    style: &mut coree_core::TextStyle,
    mut direction: Option<&mut TextDirection>,
    mut align: Option<&mut TextAlign>,
    indexed_fonts: &[coree_storage::FontRecord],
    new_fvar_tag: &mut String,
    new_fvar_val: &mut f32,
    salt: &str,
    lang: crate::i18n::StudioLanguage,
    shell_dir: crate::i18n::ShellLayoutDirections,
) -> bool {
    let mut changed = false;
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockTextFontLabel.tr_ui(lang));
        changed |= ui.text_edit_singleline(&mut style.font_family).changed();
    });
    let mut bidi_buf = style.bidi_font_family.clone().unwrap_or_default();
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockTextRtlFontLabel.tr_ui(lang));
        if ui
            .add(
                egui::TextEdit::singleline(&mut bidi_buf)
                    .hint_text(crate::i18n::I18nKey::DockTextAutoBidiHint.tr_ui(lang)),
            )
            .changed()
        {
            let trimmed = bidi_buf.trim();
            style.bidi_font_family = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            };
            changed = true;
        }
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockTextWeightLabel.tr_ui(lang));
        let mut w = style.effective_weight();
        ComboBox::from_id_salt(("docker_txt_w", salt))
            .width(120.0)
            .selected_text(format!("{w}"))
            .show_ui(ui, |ui| {
                for (wt, lbl) in [
                    (100, "100 Thin"),
                    (200, "200 ExtraLight"),
                    (300, "300 Light"),
                    (400, "400 Regular"),
                    (500, "500 Medium"),
                    (600, "600 SemiBold"),
                    (700, "700 Bold"),
                    (800, "800 ExtraBold"),
                    (900, "900 Black"),
                ] {
                    if ui.selectable_label(w == wt, lbl).clicked() {
                        style.set_weight(wt);
                        changed = true;
                    }
                }
            });
        if ui
            .add(DragValue::new(&mut w).range(100..=900).speed(10.0))
            .changed()
        {
            style.set_weight(w);
            changed = true;
        }
    });
    changed |= ui
        .add(
            egui::Slider::new(&mut style.font_size_pt, 6.0..=200.0)
                .text(crate::i18n::I18nKey::DockTextSizePt.tr_ui(lang)),
        )
        .changed();
    if let Some(ref mut dir_ref) = direction {
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(crate::i18n::I18nKey::DockTextDirectionLabel.tr_ui(lang));
            for dir in [TextDirection::Auto, TextDirection::Ltr, TextDirection::Rtl] {
                if ui
                    .selectable_label(**dir_ref == dir, format!("{dir:?}"))
                    .clicked()
                {
                    **dir_ref = dir;
                    style.direction = dir;
                    changed = true;
                }
            }
        });
    }
    if let Some(ref mut align_ref) = align {
        shell_dir.with_horizontal_wrapped(ui, |ui| {
            ui.label(crate::i18n::I18nKey::DockTextAlignLabel.tr_ui(lang));
            for al in [
                TextAlign::Left,
                TextAlign::Center,
                TextAlign::Right,
                TextAlign::Justify,
                TextAlign::ForceJustifyKashida,
            ] {
                if ui
                    .selectable_label(**align_ref == al, format!("{al:?}"))
                    .clicked()
                {
                    **align_ref = al;
                    style.align = al;
                    changed = true;
                }
            }
        });
    }
    changed |= ui
        .add(
            egui::Slider::new(&mut style.kashida_stretch, 0.0..=2.0)
                .text(crate::i18n::I18nKey::DockTextKashidaStretch.tr_ui(lang)),
        )
        .changed();
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockTextOpenType.tr_ui(lang));
        changed |= ui
            .checkbox(&mut style.opentype_features.liga, "liga")
            .changed();
        changed |= ui
            .checkbox(&mut style.opentype_features.calt, "calt")
            .changed();
        changed |= ui
            .checkbox(&mut style.opentype_features.kern, "kern")
            .changed();
        changed |= ui
            .checkbox(&mut style.opentype_features.dlig, "dlig")
            .changed();
        changed |= ui
            .checkbox(&mut style.opentype_features.smcp, "smcp")
            .changed();
    });
    // OpenType `fvar` Variable Font Axes
    let fvar_meta: Vec<coree_storage::FontVariationAxis> = indexed_fonts
        .iter()
        .find(|rec| {
            rec.is_variable
                && rec
                    .family_name
                    .eq_ignore_ascii_case(style.font_family.trim())
        })
        .and_then(|rec| {
            let p = std::path::Path::new(&rec.file_path);
            let meta = std::fs::metadata(p).ok()?;
            if meta.len() > 32 * 1024 * 1024 {
                return None;
            }
            let bytes = std::fs::read(p).ok()?;
            let faces = coree_storage::inspect_font_bytes(&bytes, p);
            faces
                .into_iter()
                .find(|f| f.face_index == rec.face_index)
                .map(|f| f.variation_axes)
        })
        .unwrap_or_default();
    ui.label(RichText::new(crate::i18n::I18nKey::DockTextFvarLabel.tr_ui(lang)).small());
    let mut remove_axis_idx: Option<usize> = None;
    for (ax_idx, (tag, val)) in style.variation_axes.iter_mut().enumerate() {
        let (min_v, max_v) = fvar_meta
            .iter()
            .find(|a| a.tag == *tag)
            .map(|a| (a.min_value, a.max_value))
            .unwrap_or_else(|| match tag.as_str() {
                "wght" => (100.0, 900.0),
                "wdth" => (50.0, 200.0),
                "slnt" => (-45.0, 45.0),
                _ => (0.0, 2000.0),
            });
        shell_dir.with_horizontal(ui, |ui| {
            ui.label(format!("{tag}:"));
            changed |= ui
                .add(egui::Slider::new(val, min_v..=max_v).step_by(1.0))
                .changed();
            if ui.small_button("×").clicked() {
                remove_axis_idx = Some(ax_idx);
            }
        });
    }
    if let Some(ri) = remove_axis_idx {
        style.variation_axes.remove(ri);
        changed = true;
    }
    // Quick buttons for discovered fvar axes or standard wght/wdth/slnt
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        for axis in &fvar_meta {
            if !style.variation_axes.iter().any(|(t, _)| t == &axis.tag)
                && ui.small_button(format!("+ {}", axis.tag)).clicked()
            {
                style
                    .variation_axes
                    .push((axis.tag.clone(), axis.default_value));
                changed = true;
            }
        }
        for (std_tag, def_val) in [
            ("wght", 600.0_f32),
            ("wdth", 100.0_f32),
            ("slnt", -12.0_f32),
        ] {
            if !fvar_meta.iter().any(|a| a.tag == std_tag)
                && !style.variation_axes.iter().any(|(t, _)| t == std_tag)
                && ui.small_button(format!("+ {std_tag}")).clicked()
            {
                style.variation_axes.push((std_tag.to_owned(), def_val));
                changed = true;
            }
        }
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.add(
            egui::TextEdit::singleline(new_fvar_tag)
                .desired_width(48.0)
                .hint_text("wght"),
        );
        ui.add(DragValue::new(new_fvar_val).range(-1000.0..=2000.0));
        if ui
            .small_button(crate::i18n::I18nKey::DockTextAddAxis.tr_ui(lang))
            .clicked()
        {
            let clean_tag: String = new_fvar_tag.trim().chars().take(4).collect();
            if !clean_tag.is_empty() && new_fvar_val.is_finite() {
                if let Some((_, existing)) = style
                    .variation_axes
                    .iter_mut()
                    .find(|(t, _)| *t == clean_tag)
                {
                    *existing = *new_fvar_val;
                } else {
                    style.variation_axes.push((clean_tag, *new_fvar_val));
                }
                changed = true;
            }
        }
    });
    changed
}

enum LineJoinCapHelper {
    Square,
}
impl LineJoinCapHelper {
    const fn cap(self) -> LineCap {
        match self {
            Self::Square => LineCap::Square,
        }
    }
}

// =============================================================================
// 3. 11-Lens Inspector Docker (`S09`)
// =============================================================================
