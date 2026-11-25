// SPDX-License-Identifier: MIT OR Apache-2.0

use super::icons::*;
use super::*;

pub(super) fn show_page_defaults_property_bar(
    ui: &mut Ui,
    app: &mut StudioApp,
    tokens: &ThemeTokens,
) {
    let lang = app.ui_language;
    let mut autofit_clicked = false;
    let mut pending_page_cmd: Option<CommandOp> = None;
    let mut pending_unit_cmd: Option<CommandOp> = None;
    {
        let Some(tab) = app.active_tab_mut() else {
            return;
        };
        if let Some(page) = tab.document.active_page() {
            let page_id = page.id;
            let cur_w = page.width_mm;
            let cur_h = page.height_mm;
            // Page Preset combo
            let active_preset_name = PAGE_PRESETS
                .iter()
                .find(|(_, pw, ph)| {
                    ((cur_w - *pw).abs() < 0.5 && (cur_h - *ph).abs() < 0.5)
                        || ((cur_w - *ph).abs() < 0.5 && (cur_h - *pw).abs() < 0.5)
                })
                .map_or(I18nKey::TbPagePresetCustom.tr_ui(lang), |(n, _, _)| {
                    (*n).to_owned()
                });
            ComboBox::from_id_salt("prop_page_preset")
                .width(90.0)
                .selected_text(&active_preset_name)
                .show_ui(ui, |ui| {
                    for (name, pw, ph) in PAGE_PRESETS {
                        if ui
                            .selectable_label(active_preset_name == *name, name)
                            .clicked()
                        {
                            pending_page_cmd = Some(CommandOp::SetPageProperties {
                                page_id,
                                name: None,
                                width_mm: Some(pw),
                                height_mm: Some(ph),
                                bleed_mm: None,
                                facing_pages: None,
                                background_fill: None,
                                printable_background: None,
                            });
                        }
                    }
                });

            let mut w = cur_w;
            let mut h = cur_h;
            let w_chg = ui
                .add(
                    DragValue::new(&mut w)
                        .range(5.0..=5000.0)
                        .suffix(" mm")
                        .prefix(I18nKey::PropWidth.tr_ui(lang)),
                )
                .changed();
            let h_chg = ui
                .add(
                    DragValue::new(&mut h)
                        .range(5.0..=5000.0)
                        .suffix(" mm")
                        .prefix(I18nKey::PropHeight.tr_ui(lang)),
                )
                .changed();
            if w_chg || h_chg {
                pending_page_cmd = Some(CommandOp::SetPageProperties {
                    page_id,
                    name: None,
                    width_mm: Some(w),
                    height_mm: Some(h),
                    bleed_mm: None,
                    facing_pages: None,
                    background_fill: None,
                    printable_background: None,
                });
            }
            let is_portrait = cur_h >= cur_w;
            if ui
                .selectable_label(is_portrait, "▯")
                .on_hover_text(I18nKey::TbPortraitTip.tr_ui(lang))
                .clicked()
                && !is_portrait
            {
                pending_page_cmd = Some(CommandOp::SetPageProperties {
                    page_id,
                    name: None,
                    width_mm: Some(cur_h),
                    height_mm: Some(cur_w),
                    bleed_mm: None,
                    facing_pages: None,
                    background_fill: None,
                    printable_background: None,
                });
            }
            if ui
                .selectable_label(!is_portrait, "▭")
                .on_hover_text(I18nKey::TbLandscapeTip.tr_ui(lang))
                .clicked()
                && is_portrait
            {
                pending_page_cmd = Some(CommandOp::SetPageProperties {
                    page_id,
                    name: None,
                    width_mm: Some(cur_h),
                    height_mm: Some(cur_w),
                    bleed_mm: None,
                    facing_pages: None,
                    background_fill: None,
                    printable_background: None,
                });
            }
            let mut facing = page.facing_pages;
            if ui
                .checkbox(&mut facing, I18nKey::TbFacingShort.tr_ui(lang))
                .on_hover_text(I18nKey::TbFacingTip.tr_ui(lang))
                .changed()
            {
                pending_page_cmd = Some(CommandOp::SetPageProperties {
                    page_id,
                    name: None,
                    width_mm: None,
                    height_mm: None,
                    bleed_mm: None,
                    facing_pages: Some(facing),
                    background_fill: None,
                    printable_background: None,
                });
            }
            let mut printable_bg = page.printable_background;
            if ui
                .checkbox(&mut printable_bg, I18nKey::TbPrintBgShort.tr_ui(lang))
                .on_hover_text(I18nKey::TbPrintBgTip.tr_ui(lang))
                .changed()
            {
                pending_page_cmd = Some(CommandOp::SetPageProperties {
                    page_id,
                    name: None,
                    width_mm: None,
                    height_mm: None,
                    bleed_mm: None,
                    facing_pages: None,
                    background_fill: Some(page.background_fill.clone()),
                    printable_background: Some(printable_bg),
                });
            }
            let mut bg_c = page
                .background_fill
                .referenced_colors()
                .into_iter()
                .next()
                .map(|c| core_color_to_egui(&c))
                .unwrap_or(Color32::WHITE);
            if ui.color_edit_button_srgba(&mut bg_c).changed() {
                pending_page_cmd = Some(CommandOp::SetPageProperties {
                    page_id,
                    name: None,
                    width_mm: None,
                    height_mm: None,
                    bleed_mm: None,
                    facing_pages: None,
                    background_fill: Some(Fill::Solid(egui_color_to_core(bg_c))),
                    printable_background: None,
                });
            }
        }
        ui.separator();

        // Units
        let cur_unit = tab.document.default_unit();
        let mut next_unit = cur_unit;
        ComboBox::from_id_salt("prop_unit")
            .width(58.0)
            .selected_text(cur_unit.suffix())
            .show_ui(ui, |ui| {
                for u in [
                    Unit::Millimeter,
                    Unit::Centimeter,
                    Unit::Meter,
                    Unit::Inch,
                    Unit::Point,
                    Unit::Pixel,
                ] {
                    ui.selectable_value(&mut next_unit, u, u.suffix());
                }
            });
        if next_unit != cur_unit {
            pending_unit_cmd = Some(CommandOp::SetDocumentProperties {
                title: None,
                color_mode: None,
                default_unit: Some(next_unit),
                dpi: None,
                multipage_view: None,
                active_page_index: None,
                metadata: None,
                custom_icc_profile: None,
                clear_custom_icc: false,
                rgb_icc_profile: None,
                clear_rgb_icc: false,
                cmyk_icc_profile: None,
                clear_cmyk_icc: false,
                gray_icc_profile: None,
                clear_gray_icc: false,
                rendering_intent: None,
                black_point_compensation: None,
                drawing_scale: None,
            });
        }
        ui.separator();

        // Nudge & Duplicate Offsets
        ui.add(
            DragValue::new(&mut tab.nudge_mm)
                .range(0.05..=100.0)
                .speed(0.1)
                .prefix(I18nKey::TbNudgePrefix.tr_ui(lang))
                .suffix(" mm"),
        );
        ui.add(
            DragValue::new(&mut tab.duplicate_offset_mm.x)
                .range(-200.0..=200.0)
                .prefix(I18nKey::TbDupXPrefix.tr_ui(lang))
                .suffix(" mm"),
        );
        ui.add(
            DragValue::new(&mut tab.duplicate_offset_mm.y)
                .range(-200.0..=200.0)
                .prefix(I18nKey::TbDupYPrefix.tr_ui(lang))
                .suffix(" mm"),
        );
    }
    if let Some(cmd) = pending_page_cmd {
        if let Err(err) = app.execute_command(cmd) {
            app.push_toast(format!("Page update failed: {err}"), true);
        } else if let Some(tab) = app.active_tab_mut() {
            tab.needs_rerender = true;
        }
    }
    if let Some(cmd) = pending_unit_cmd {
        let _ = app.execute_command(cmd);
    }

    ui.separator();
    ui.checkbox(
        &mut app.treat_all_objects_as_filled,
        I18nKey::TbTreatAsFilled.tr_ui(lang),
    )
    .on_hover_text(I18nKey::TbTreatAsFilledTip.tr_ui(lang));

    if ui
        .small_button(RichText::new(I18nKey::LayoutAutofitPage.tr_ui(lang)).color(tokens.accent))
        .on_hover_text(I18nKey::TbAutofitPageTip.tr_ui(lang))
        .clicked()
    {
        autofit_clicked = true;
    }

    // Creation-tool parameters when a parametric, curve, dimension, connector, or table tool is active
    match app.active_tool {
        ToolId::Rectangle | ToolId::ThreePointRectangle => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.rect_corner_radius_mm)
                    .range(0.0..=100.0)
                    .speed(0.2)
                    .prefix(format!(
                        "{} / {}: ",
                        I18nKey::PropCornerRadius.tr_ui(lang),
                        I18nKey::PropCornerStyle.tr_ui(lang)
                    ))
                    .suffix(" mm"),
            );
        }
        ToolId::Ellipse | ToolId::ThreePointEllipse => {
            ui.separator();
            ui.selectable_value(
                &mut app.tool_settings.ellipse_mode,
                EllipseMode::Full,
                I18nKey::TbEllipseFull.tr_ui(lang),
            );
            ui.selectable_value(
                &mut app.tool_settings.ellipse_mode,
                EllipseMode::Pie,
                I18nKey::TbEllipsePie.tr_ui(lang),
            );
            ui.selectable_value(
                &mut app.tool_settings.ellipse_mode,
                EllipseMode::Arc,
                I18nKey::TbEllipseArc.tr_ui(lang),
            );
        }
        ToolId::Polygon => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.polygon_sides)
                    .range(3..=64)
                    .prefix(I18nKey::PropPolygonSides.tr_ui(lang)),
            );
        }
        ToolId::Star | ToolId::ComplexStar => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.star_points)
                    .range(3..=64)
                    .prefix(I18nKey::PropStarPoints.tr_ui(lang)),
            );
            ui.add(
                DragValue::new(&mut app.tool_settings.star_inner_ratio)
                    .range(0.1..=0.9)
                    .speed(0.02)
                    .prefix(I18nKey::PropStarSharpness.tr_ui(lang)),
            );
        }
        ToolId::Spiral => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.spiral_revolutions)
                    .range(1.0..=20.0)
                    .speed(0.5)
                    .prefix(I18nKey::PropSpiralRevolutions.tr_ui(lang)),
            );
            ui.checkbox(
                &mut app.tool_settings.spiral_logarithmic,
                I18nKey::PropSpiralLogarithmic.tr_ui(lang),
            );
        }
        ToolId::Impact => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.impact_rays)
                    .range(4..=128)
                    .prefix(format!(
                        "{} / {}: ",
                        I18nKey::PropImpactRays.tr_ui(lang),
                        I18nKey::PropImpactStyle.tr_ui(lang)
                    )),
            );
            ui.selectable_value(
                &mut app.tool_settings.impact_style,
                ImpactStyle::Radial,
                I18nKey::TbImpactRadial.tr_ui(lang),
            );
            ui.selectable_value(
                &mut app.tool_settings.impact_style,
                ImpactStyle::Parallel,
                I18nKey::TbImpactParallel.tr_ui(lang),
            );
        }
        ToolId::BasicShapes => {
            ui.separator();
            ui.label(RichText::new(I18nKey::PropBasicShapeKind.tr_ui(lang)).size(10.5));
            ComboBox::from_id_salt("prop_basic_shape")
                .width(120.0)
                .selected_text(format!("{:?}", app.tool_settings.basic_shape_kind))
                .show_ui(ui, |ui| {
                    for kind in [
                        BasicShapeKind::ArrowRight,
                        BasicShapeKind::ArrowLeft,
                        BasicShapeKind::DoubleArrow,
                        BasicShapeKind::Banner,
                        BasicShapeKind::Callout,
                        BasicShapeKind::Heart,
                        BasicShapeKind::Cross,
                        BasicShapeKind::Diamond,
                        BasicShapeKind::Trapezoid,
                    ] {
                        ui.selectable_value(
                            &mut app.tool_settings.basic_shape_kind,
                            kind,
                            format!("{kind:?}"),
                        );
                    }
                });
        }
        ToolId::ArtisticMedia => {
            ui.separator();
            let width_changed = ui
                .add(
                    DragValue::new(&mut app.tool_settings.artistic_media_width_mm)
                        .range(0.4..=50.0)
                        .speed(0.2)
                        .prefix(I18nKey::DockBmpHeading.tr_ui(lang))
                        .suffix(" mm"),
                )
                .changed();
            let mut profile_changed = false;
            ComboBox::from_id_salt("prop_artistic_profile")
                .width(110.0)
                .selected_text(format!("{:?}", app.tool_settings.artistic_media_profile))
                .show_ui(ui, |ui| {
                    for prof in [
                        ArtisticWidthProfile::TaperBoth,
                        ArtisticWidthProfile::TaperStart,
                        ArtisticWidthProfile::TaperEnd,
                        ArtisticWidthProfile::Wave,
                        ArtisticWidthProfile::Uniform,
                    ] {
                        profile_changed |= ui
                            .selectable_value(
                                &mut app.tool_settings.artistic_media_profile,
                                prof.clone(),
                                format!("{prof:?}"),
                            )
                            .changed();
                    }
                });
            if width_changed || profile_changed {
                let _ = app.edit_selected_artistic_media(
                    width_changed.then_some(app.tool_settings.artistic_media_width_mm),
                    profile_changed.then(|| app.tool_settings.artistic_media_profile.clone()),
                );
            }
            ComboBox::from_id_salt("prop_artistic_pressure_curve")
                .width(86.0)
                .selected_text(pressure_curve_label(
                    &app.tool_settings.pressure_curve_json,
                    lang,
                ))
                .show_ui(ui, |ui| {
                    for (lbl, json) in [
                        (
                            I18nKey::TbPressureLinear.tr_ui(lang),
                            "[[0.0,0.0],[1.0,1.0]]",
                        ),
                        (
                            I18nKey::TbPressureSoft.tr_ui(lang),
                            "[[0.0,0.2],[0.5,0.75],[1.0,1.0]]",
                        ),
                        (
                            I18nKey::TbPressureFirm.tr_ui(lang),
                            "[[0.0,0.0],[0.5,0.25],[1.0,1.0]]",
                        ),
                    ] {
                        ui.selectable_value(
                            &mut app.tool_settings.pressure_curve_json,
                            json.to_owned(),
                            lbl,
                        );
                    }
                });
        }
        ToolId::Freehand
        | ToolId::BSpline
        | ToolId::Polyline
        | ToolId::ThreePointCurve
        | ToolId::SmartDrawing
        | ToolId::LiveSketch
        | ToolId::Bezier
        | ToolId::Pen => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.shape_reduce_tolerance_mm)
                    .range(0.05..=8.0)
                    .speed(0.05)
                    .prefix(I18nKey::TbTolPrefix.tr_ui(lang))
                    .suffix(" mm"),
            );
        }
        ToolId::ParallelDimension
        | ToolId::HvDimension
        | ToolId::AngularDimension
        | ToolId::SegmentDimension
        | ToolId::ThreePointCallout => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.dimension_offset_mm)
                    .range(1.0..=100.0)
                    .speed(0.5)
                    .prefix(I18nKey::PropDimensionOffset.tr_ui(lang))
                    .suffix(" mm"),
            );
        }
        ToolId::StraightConnector | ToolId::RightAngleConnector => {
            ui.separator();
            ui.label(
                RichText::new(format!(
                    "{}: {}",
                    I18nKey::PropConnectorStyle.tr_ui(lang),
                    I18nKey::TbConnectorHint.tr_ui(lang)
                ))
                .size(11.0)
                .color(tokens.text_muted),
            );
        }
        ToolId::GraphPaper => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.graph_cols)
                    .range(1..=30)
                    .prefix(I18nKey::PropGraphCols.tr_ui(lang)),
            );
            ui.add(
                DragValue::new(&mut app.tool_settings.graph_rows)
                    .range(1..=30)
                    .prefix(I18nKey::PropGraphRows.tr_ui(lang)),
            );
        }
        ToolId::Table => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.table_cols)
                    .range(1..=20)
                    .prefix(I18nKey::PropTableCols.tr_ui(lang)),
            );
            ui.add(
                DragValue::new(&mut app.tool_settings.table_rows)
                    .range(1..=20)
                    .prefix(I18nKey::PropTableRows.tr_ui(lang)),
            );
        }
        _ => {}
    }

    if autofit_clicked {
        app.autofit_active_page(5.0);
    }
}

// -----------------------------------------------------------------------------
// Selected Object(s) Property Bar (9-Point Origin Grid + Transform + Contextual)
// -----------------------------------------------------------------------------
