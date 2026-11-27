// SPDX-License-Identifier: MIT OR Apache-2.0

use super::icons::*;
use super::*;

pub(super) fn show_shape_node_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    let lang = app.ui_language;
    ui.label(
        RichText::new(I18nKey::TbShapeF10Title.tr_ui(lang))
            .color(tokens.accent)
            .strong(),
    );
    let selected_nodes = app
        .active_tab()
        .map_or(0, |t| t.selection.node_selection.nodes.len());
    ui.label(
        RichText::new(format!("{selected_nodes} node(s)"))
            .size(11.0)
            .color(tokens.text_muted),
    );
    ui.separator();

    if ui
        .small_button(I18nKey::TbShapeAdd.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeAddTip.tr_ui(lang))
        .clicked()
    {
        app.shape_add_node();
    }
    if ui
        .small_button(I18nKey::TbShapeDel.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeDelTip.tr_ui(lang))
        .clicked()
    {
        app.shape_delete_selected_nodes();
    }
    if ui
        .small_button(I18nKey::TbShapeJoin.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeJoinTip.tr_ui(lang))
        .clicked()
    {
        app.shape_join_nodes();
    }
    if ui
        .small_button(I18nKey::TbShapeBreak.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeBreakTip.tr_ui(lang))
        .clicked()
    {
        app.shape_break_curve();
    }
    ui.separator();

    if ui
        .small_button(I18nKey::TbShapeToLine.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeToLineTip.tr_ui(lang))
        .clicked()
    {
        app.shape_convert_to_line();
    }
    if ui
        .small_button(I18nKey::TbShapeToCurve.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeToCurveTip.tr_ui(lang))
        .clicked()
    {
        app.shape_convert_to_curve();
    }
    ui.separator();

    if ui
        .small_button(I18nKey::TbShapeCusp.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeCuspTip.tr_ui(lang))
        .clicked()
    {
        app.shape_set_node_smoothness(NodeSmoothness::Cusp);
    }
    if ui
        .small_button(I18nKey::TbShapeSmooth.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeSmoothTip.tr_ui(lang))
        .clicked()
    {
        app.shape_set_node_smoothness(NodeSmoothness::Smooth);
    }
    if ui
        .small_button(I18nKey::TbShapeSymm.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeSymmTip.tr_ui(lang))
        .clicked()
    {
        app.shape_set_node_smoothness(NodeSmoothness::Symmetrical);
    }
    ui.separator();

    if ui
        .small_button(I18nKey::TbShapeReverse.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeReverseTip.tr_ui(lang))
        .clicked()
    {
        app.shape_reverse_subpaths();
    }
    if ui
        .small_button(I18nKey::TbShapeClose.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeCloseTip.tr_ui(lang))
        .clicked()
    {
        app.shape_close_curve();
    }
    if ui
        .small_button(I18nKey::TbShapeSelectAll.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeSelectAllTip.tr_ui(lang))
        .clicked()
    {
        app.shape_select_all_nodes();
    }
    ui.separator();

    ui.add(
        DragValue::new(&mut app.tool_settings.shape_reduce_tolerance_mm)
            .range(0.05..=5.0)
            .speed(0.05)
            .prefix(I18nKey::TbTolPrefix.tr_ui(lang))
            .suffix(" mm"),
    );
    if ui
        .small_button(I18nKey::TbShapeReduceNodes.tr_ui(lang))
        .on_hover_text(I18nKey::TbShapeReduceNodesTip.tr_ui(lang))
        .clicked()
    {
        let tol = app.tool_settings.shape_reduce_tolerance_mm;
        app.shape_reduce_nodes(tol);
    }
}

// -----------------------------------------------------------------------------
// Sculpting Tools Property Bar
// -----------------------------------------------------------------------------

pub(super) fn show_sculpt_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    let lang = app.ui_language;
    ui.label(
        RichText::new(format!(
            "{}:",
            crate::i18n::tool_i18n_key(app.active_tool).tr_ui(lang)
        ))
        .color(tokens.accent)
        .strong(),
    );
    ui.add(
        DragValue::new(&mut app.tool_settings.sculpt_radius_mm)
            .range(1.0..=100.0)
            .speed(0.5)
            .prefix(I18nKey::TbSculptNib.tr_ui(lang))
            .suffix(" mm"),
    );
    ui.add(
        DragValue::new(&mut app.tool_settings.sculpt_strength)
            .range(0.05..=1.0)
            .speed(0.02)
            .prefix(I18nKey::TbSculptRate.tr_ui(lang)),
    );
    if app.active_tool == ToolId::AttractRepel {
        ui.separator();
        ui.selectable_value(
            &mut app.tool_settings.sculpt_sub_mode,
            coree_tools::SculptSubMode::Attract,
            I18nKey::TbSculptAttract.tr_ui(lang),
        );
        ui.selectable_value(
            &mut app.tool_settings.sculpt_sub_mode,
            coree_tools::SculptSubMode::Repel,
            I18nKey::TbSculptRepel.tr_ui(lang),
        );
    }
    if app.active_tool == ToolId::Roughen {
        ui.separator();
        ui.add(
            DragValue::new(&mut app.effects_state.frequency)
                .range(0.5..=12.0)
                .speed(0.2)
                .prefix(I18nKey::TbSculptFreq.tr_ui(lang)),
        );
    }
    ui.separator();
    let hint = match app.active_tool {
        ToolId::Twirl => I18nKey::TbSculptHintTwirl.tr_ui(lang),
        ToolId::AttractRepel => match app.tool_settings.sculpt_sub_mode {
            coree_tools::SculptSubMode::Attract => I18nKey::TbSculptHintAttract.tr_ui(lang),
            coree_tools::SculptSubMode::Repel => I18nKey::TbSculptHintRepel.tr_ui(lang),
        },
        ToolId::Roughen => I18nKey::TbSculptHintRoughen.tr_ui(lang),
        _ => I18nKey::TbSculptHintDefault.tr_ui(lang),
    };
    ui.label(RichText::new(hint).size(11.0).color(tokens.text_muted));
}

// -----------------------------------------------------------------------------
// Integrated Bitmap Studio Property Bar (all 8 RasterBrushOps)
// -----------------------------------------------------------------------------

pub(super) fn show_raster_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    let lang = app.ui_language;
    ui.label(
        RichText::new(I18nKey::TbBitmapLabel.tr_ui(lang))
            .color(tokens.accent)
            .strong(),
    );

    ComboBox::from_id_salt("prop_raster_op")
        .width(126.0)
        .selected_text(app.tool_settings.raster_op.label())
        .show_ui(ui, |ui| {
            for op in RasterBrushOp::ALL {
                ui.selectable_value(&mut app.tool_settings.raster_op, op, op.label());
            }
        });

    ui.add(
        DragValue::new(&mut app.tool_settings.brush_radius_px)
            .range(1.0..=200.0)
            .speed(1.0)
            .prefix(I18nKey::TbSizePrefix.tr_ui(lang))
            .suffix(" px"),
    );
    ui.add(
        DragValue::new(&mut app.tool_settings.brush_hardness)
            .range(0.0..=1.0)
            .speed(0.02)
            .prefix(I18nKey::TbHardPrefix.tr_ui(lang)),
    );
    let mut bc = core_color_to_egui(&app.tool_settings.brush_color);
    if ui.color_edit_button_srgba(&mut bc).changed() {
        app.tool_settings.brush_color = egui_color_to_core(bc);
    }

    match app.tool_settings.raster_op {
        RasterBrushOp::Smudge => {
            ui.add(
                DragValue::new(&mut app.tool_settings.smudge_strength)
                    .range(0.05..=1.0)
                    .speed(0.02)
                    .prefix(I18nKey::TbStrengthPrefix.tr_ui(lang)),
            );
        }
        RasterBrushOp::Dodge | RasterBrushOp::Burn => {
            ui.separator();
            ui.label(I18nKey::TbToneLabel.tr_ui(lang));
            for (tone, lbl) in [
                (ToneRange::Shadows, I18nKey::TbToneShadows.tr_ui(lang)),
                (ToneRange::Midtones, I18nKey::TbToneMidtones.tr_ui(lang)),
                (ToneRange::Highlights, I18nKey::TbToneHighlights.tr_ui(lang)),
            ] {
                ui.selectable_value(&mut app.tool_settings.dodge_burn_tone, tone, lbl);
            }
            ui.add(
                DragValue::new(&mut app.tool_settings.dodge_burn_exposure)
                    .range(0.05..=1.0)
                    .speed(0.02)
                    .prefix(I18nKey::TbExpPrefix.tr_ui(lang)),
            );
        }
        RasterBrushOp::FloodFill => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.flood_tolerance)
                    .range(0..=255)
                    .prefix(I18nKey::TbTolerancePrefix.tr_ui(lang)),
            );
        }
        _ => {}
    }

    ComboBox::from_id_salt("prop_raster_pressure_curve")
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

    let has_mask = app
        .active_tab()
        .is_some_and(|t| t.raster_mask.is_some() || t.raster_mask_rect_mm.is_some());
    if app.active_tool == ToolId::MaskSelect || has_mask {
        ui.separator();
        ComboBox::from_id_salt("prop_mask_mode")
            .width(100.0)
            .selected_text(app.tool_settings.mask_mode.label())
            .show_ui(ui, |ui| {
                for m in [
                    RasterMaskMode::Rectangle,
                    RasterMaskMode::Ellipse,
                    RasterMaskMode::Lasso,
                    RasterMaskMode::MagicWand,
                ] {
                    ui.selectable_value(&mut app.tool_settings.mask_mode, m, m.label());
                }
            });
        let feather_changed = ui
            .add(
                DragValue::new(&mut app.tool_settings.mask_feather_px)
                    .range(0.0..=32.0)
                    .speed(0.2)
                    .prefix(I18nKey::TbFeatherPrefix.tr_ui(lang))
                    .suffix("px"),
            )
            .changed();
        if app.tool_settings.mask_mode == RasterMaskMode::MagicWand {
            ui.add(
                DragValue::new(&mut app.tool_settings.magic_wand_tolerance)
                    .range(1.0..=150.0)
                    .prefix(I18nKey::TbTolPrefix.tr_ui(lang)),
            );
            ui.checkbox(
                &mut app.tool_settings.magic_wand_contiguous,
                I18nKey::TbContiguous.tr_ui(lang),
            );
        }
        let inv_changed = ui
            .checkbox(
                &mut app.tool_settings.mask_inverted,
                I18nKey::TbInvert.tr_ui(lang),
            )
            .changed();
        if feather_changed || inv_changed {
            let fpx = app.tool_settings.mask_feather_px.max(0.0);
            let inv = app.tool_settings.mask_inverted;
            if let Some(tab) = app.active_tab_mut() {
                if let Some(ref mut sel) = tab.raster_mask {
                    sel.feather_px = fpx;
                    sel.inverted = inv;
                }
            }
        }
        if let Some(mask) = app.active_tab().and_then(|t| t.raster_mask_rect_mm) {
            ui.label(
                RichText::new(format!("Mask: {:.1}×{:.1} mm", mask.width(), mask.height()))
                    .size(11.0)
                    .color(tokens.text_muted),
            );
        }
        if has_mask {
            if ui
                .small_button(I18nKey::BitmapCutoutLab.tr_ui(lang))
                .clicked()
            {
                app.run_cutout_lab_active();
            }
            if ui.small_button(I18nKey::TbClearMask.tr_ui(lang)).clicked() {
                if let Some(tab) = app.active_tab_mut() {
                    tab.raster_mask = None;
                    tab.raster_mask_rect_mm = None;
                    tab.tool_core.raster_mask_rect_mm = None;
                    tab.needs_rerender = true;
                }
            }
        } else {
            ui.label(
                RichText::new(I18nKey::TbMaskHint.tr_ui(lang))
                    .size(11.0)
                    .color(tokens.text_muted),
            );
        }
    }
}

// -----------------------------------------------------------------------------
// Lens, Interactive Effects, Fill, Eyedropper, Crop/Cut, Zoom/Pan Property Bars
// -----------------------------------------------------------------------------

pub(super) fn show_lens_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    let lang = app.ui_language;
    ui.label(
        RichText::new(I18nKey::TbLensLabel.tr_ui(lang))
            .color(tokens.accent)
            .strong(),
    );
    ComboBox::from_id_salt("prop_lens_kind")
        .width(136.0)
        .selected_text(LensDockerState::NAMES[app.lens_state.selected_index])
        .show_ui(ui, |ui| {
            for (idx, name) in LensDockerState::NAMES.iter().enumerate() {
                ui.selectable_value(&mut app.lens_state.selected_index, idx, *name);
            }
        });
    ui.checkbox(
        &mut app.lens_state.frozen,
        I18nKey::TbLensFrozen.tr_ui(lang),
    );
    ui.checkbox(
        &mut app.lens_state.use_viewpoint,
        I18nKey::TbLensViewpoint.tr_ui(lang),
    );
    ui.checkbox(
        &mut app.lens_state.remove_face,
        I18nKey::TbLensRemoveFace.tr_ui(lang),
    );
    if ui.small_button(I18nKey::TbApplyLens.tr_ui(lang)).clicked() {
        let cfg = app.lens_state.build_config();
        app.apply_lens_to_selection(Some(cfg));
    }
    if ui.small_button(I18nKey::TbRemove.tr_ui(lang)).clicked() {
        app.apply_lens_to_selection(None);
    }
}

pub(super) fn show_interactive_effect_property_bar(
    ui: &mut Ui,
    app: &mut StudioApp,
    tokens: &ThemeTokens,
) {
    let lang = app.ui_language;
    let idx = match app.active_tool {
        ToolId::DropShadow => 0,
        ToolId::BlockShadow => 1,
        ToolId::Contour => 2,
        ToolId::Blend => 3,
        ToolId::Distort => 4,
        ToolId::Envelope => 5,
        ToolId::Extrude25D => 6,
        _ => 7,
    };
    app.effects_state.selected_effect_kind = idx;
    ui.label(
        RichText::new(format!(
            "{}:",
            crate::i18n::tool_i18n_key(app.active_tool).tr_ui(lang)
        ))
        .color(tokens.accent)
        .strong(),
    );

    let (copy_target_id, envelope_sources): (
        Option<coree_core::ObjectId>,
        Vec<(coree_core::ObjectId, String)>,
    ) = if app.active_tool == ToolId::Envelope {
        if let Some(tab) = app.active_tab() {
            let target = tab.selection.objects.first().copied();
            let mut srcs = Vec::new();
            if let Some(page) = tab.document.active_page() {
                for layer in &page.layers {
                    for &oid in &layer.objects {
                        if Some(oid) == target {
                            continue;
                        }
                        if let Some(obj) = tab.document.object(oid) {
                            if obj
                                .effects
                                .iter()
                                .any(|e| matches!(e, coree_core::Effect::Envelope { .. }))
                            {
                                srcs.push((oid, format!("{} (#{})", obj.name, oid.0)));
                            }
                        }
                    }
                }
            }
            (target, srcs)
        } else {
            (None, Vec::new())
        }
    } else {
        (None, Vec::new())
    };
    let mut copy_envelope_from: Option<coree_core::ObjectId> = None;

    let es = &mut app.effects_state;
    match app.active_tool {
        ToolId::DropShadow => {
            ui.add(
                DragValue::new(&mut es.dx_mm)
                    .range(-40.0..=40.0)
                    .prefix(I18nKey::PropPositionX.tr_ui(lang))
                    .suffix(" mm"),
            );
            ui.add(
                DragValue::new(&mut es.dy_mm)
                    .range(-40.0..=40.0)
                    .prefix(I18nKey::PropPositionY.tr_ui(lang))
                    .suffix(" mm"),
            );
            ui.add(
                DragValue::new(&mut es.blur_mm)
                    .range(0.0..=25.0)
                    .prefix(I18nKey::TbFeatherPrefix.tr_ui(lang))
                    .suffix(" mm"),
            );
            ui.add(
                DragValue::new(&mut es.opacity)
                    .range(0.0..=1.0)
                    .speed(0.02)
                    .prefix(I18nKey::TbNodeAlphaPrefix.tr_ui(lang)),
            );
        }
        ToolId::BlockShadow => {
            ui.add(
                DragValue::new(&mut es.dx_mm)
                    .range(-50.0..=50.0)
                    .prefix(I18nKey::TbDepthXPrefix.tr_ui(lang))
                    .suffix(" mm"),
            );
            ui.add(
                DragValue::new(&mut es.dy_mm)
                    .range(-50.0..=50.0)
                    .prefix(I18nKey::TbDepthYPrefix.tr_ui(lang))
                    .suffix(" mm"),
            );
        }
        ToolId::Contour => {
            ui.selectable_value(
                &mut es.contour_mode,
                ContourMode::ToCenter,
                I18nKey::ObjPowerClipCenter.tr_ui(lang),
            );
            ui.selectable_value(
                &mut es.contour_mode,
                ContourMode::Inside,
                I18nKey::TbInside.tr_ui(lang),
            );
            ui.selectable_value(
                &mut es.contour_mode,
                ContourMode::Outside,
                I18nKey::TbOutside.tr_ui(lang),
            );
            ui.add(
                DragValue::new(&mut es.steps)
                    .range(1..=30)
                    .prefix(I18nKey::TbStepsPrefix.tr_ui(lang)),
            );
            ui.add(
                DragValue::new(&mut es.offset_mm)
                    .range(0.2..=25.0)
                    .prefix(I18nKey::PropTextOnPathOffset.tr_ui(lang))
                    .suffix(" mm"),
            );
        }
        ToolId::Blend => {
            ui.add(
                DragValue::new(&mut es.steps)
                    .range(1..=60)
                    .prefix(I18nKey::TbBlendStepsPrefix.tr_ui(lang)),
            );
        }
        ToolId::Distort => {
            ui.selectable_value(
                &mut es.distort_kind,
                DistortKind::PushPull,
                I18nKey::TbDistortPushPull.tr_ui(lang),
            );
            ui.selectable_value(
                &mut es.distort_kind,
                DistortKind::Zipper,
                I18nKey::TbDistortZipper.tr_ui(lang),
            );
            ui.selectable_value(
                &mut es.distort_kind,
                DistortKind::Twister,
                I18nKey::TbDistortTwister.tr_ui(lang),
            );
            ui.add(
                DragValue::new(&mut es.amplitude)
                    .range(1.0..=100.0)
                    .prefix(I18nKey::TbAmpPrefix.tr_ui(lang)),
            );
            ui.add(
                DragValue::new(&mut es.frequency)
                    .range(0.5..=12.0)
                    .prefix(I18nKey::TbFreqPrefix.tr_ui(lang)),
            );
        }
        ToolId::Envelope => {
            ui.selectable_value(
                &mut es.envelope_mode,
                EnvelopeMode::Straight,
                I18nKey::TbEnvStraight.tr_ui(lang),
            );
            ui.selectable_value(
                &mut es.envelope_mode,
                EnvelopeMode::SingleArc,
                I18nKey::TbEnvSingleArc.tr_ui(lang),
            );
            ui.selectable_value(
                &mut es.envelope_mode,
                EnvelopeMode::DoubleArc,
                I18nKey::TbEnvDoubleArc.tr_ui(lang),
            );
            ui.selectable_value(
                &mut es.envelope_mode,
                EnvelopeMode::Unconstrained,
                I18nKey::TbEnvUnconstrained.tr_ui(lang),
            );
            if copy_target_id.is_some() && !envelope_sources.is_empty() {
                if es
                    .copy_envelope_source
                    .is_none_or(|s| !envelope_sources.iter().any(|(id, _)| *id == s))
                {
                    es.copy_envelope_source = envelope_sources.first().map(|(id, _)| *id);
                }
                let sel_label = es
                    .copy_envelope_source
                    .and_then(|s| {
                        envelope_sources
                            .iter()
                            .find(|(id, _)| *id == s)
                            .map(|(_, n)| n.as_str())
                    })
                    .unwrap_or("Source");
                ComboBox::from_id_salt("prop_copy_env_src")
                    .width(120.0)
                    .selected_text(sel_label)
                    .show_ui(ui, |ui| {
                        for (oid, label) in &envelope_sources {
                            ui.selectable_value(&mut es.copy_envelope_source, Some(*oid), label);
                        }
                    });
                if ui
                    .small_button(I18nKey::TbCopyEnvelope.tr_ui(lang))
                    .clicked()
                {
                    copy_envelope_from = es.copy_envelope_source;
                }
            }
        }
        ToolId::Extrude25D => {
            ui.add(
                DragValue::new(&mut es.extrude_depth)
                    .range(2.0..=80.0)
                    .prefix(I18nKey::TbDepthPrefix.tr_ui(lang))
                    .suffix(" mm"),
            );
        }
        ToolId::Transparency => {
            ui.add(
                DragValue::new(&mut es.opacity)
                    .range(0.0..=1.0)
                    .speed(0.02)
                    .prefix(I18nKey::TbOpacityPrefix.tr_ui(lang)),
            );
            ComboBox::from_id_salt("prop_trans_blend")
                .width(100.0)
                .selected_text(format!("{:?}", es.blend_mode))
                .show_ui(ui, |ui| {
                    for bm in [
                        BlendMode::Normal,
                        BlendMode::Multiply,
                        BlendMode::Screen,
                        BlendMode::Overlay,
                        BlendMode::Darken,
                        BlendMode::Lighten,
                    ] {
                        ui.selectable_value(&mut es.blend_mode, bm, format!("{bm:?}"));
                    }
                });
        }
        _ => {}
    }

    if let (Some(dst), Some(src)) = (copy_target_id, copy_envelope_from) {
        app.copy_envelope_from_object(dst, src);
    }

    if ui
        .small_button(I18nKey::TbApplyEffect.tr_ui(lang))
        .clicked()
    {
        let eff = app.build_selected_effect();
        app.add_effect_to_selection(eff);
    }
    if ui
        .small_button(I18nKey::TbEffectsDocker.tr_ui(lang))
        .clicked()
    {
        app.focus_docker(DockerTab::Effects);
    }
}

pub(super) fn show_fill_tool_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    let lang = app.ui_language;
    ui.label(
        RichText::new(format!(
            "{}:",
            crate::i18n::tool_i18n_key(app.active_tool).tr_ui(lang)
        ))
        .color(tokens.accent)
        .strong(),
    );
    match app.active_tool {
        ToolId::MeshFill => {
            ui.add(
                DragValue::new(&mut app.properties_state.mesh_rows)
                    .range(2..=8)
                    .prefix(I18nKey::PropGraphRows.tr_ui(lang)),
            );
            ui.add(
                DragValue::new(&mut app.properties_state.mesh_cols)
                    .range(2..=8)
                    .prefix(I18nKey::PropGraphCols.tr_ui(lang)),
            );
            if ui
                .small_button(I18nKey::TbApplyMeshGrid.tr_ui(lang))
                .clicked()
            {
                let bb = app
                    .selected_local_bounds()
                    .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 50.0, 50.0));
                let fill = StudioApp::build_mesh_fill_for_bounds(
                    bb,
                    app.properties_state.mesh_rows,
                    app.properties_state.mesh_cols,
                );
                app.apply_fill(fill);
            }
            let selected_mesh_info = app.active_tab().and_then(|tab| {
                let (oid, n_idx) = tab.selected_mesh_node?;
                let obj = tab.document.object(oid)?;
                if let Fill::MeshFill { nodes, .. } = &obj.fill {
                    nodes
                        .get(n_idx)
                        .map(|n| (oid, n_idx, obj.fill.clone(), n.alpha))
                } else {
                    None
                }
            });
            if let Some((oid, n_idx, mut mesh_fill, mut node_alpha)) = selected_mesh_info {
                if ui
                    .add(
                        DragValue::new(&mut node_alpha)
                            .range(0.0..=1.0)
                            .speed(0.02)
                            .prefix(I18nKey::TbNodeAlphaPrefix.tr_ui(lang)),
                    )
                    .changed()
                {
                    if let Fill::MeshFill { ref mut nodes, .. } = mesh_fill {
                        if let Some(node) = nodes.get_mut(n_idx) {
                            node.alpha = node_alpha.clamp(0.0, 1.0);
                        }
                    }
                    let _ = app.execute_command(CommandOp::SetFill {
                        ids: vec![oid],
                        fill: mesh_fill,
                    });
                }
            }
        }
        ToolId::InteractiveFill => {
            let mut sc = core_color_to_egui(&app.properties_state.grad_start_color);
            let mut ec = core_color_to_egui(&app.properties_state.grad_end_color);
            ui.label(I18nKey::PropFillStartLabel.tr_ui(lang));
            if ui.color_edit_button_srgba(&mut sc).changed() {
                app.properties_state.grad_start_color = egui_color_to_core(sc);
            }
            ui.label(I18nKey::PropFillEndLabel.tr_ui(lang));
            if ui.color_edit_button_srgba(&mut ec).changed() {
                app.properties_state.grad_end_color = egui_color_to_core(ec);
            }
            if ui
                .small_button(I18nKey::TbPressureLinear.tr_ui(lang))
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
            if ui.small_button(I18nKey::TbRadial.tr_ui(lang)).clicked() {
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
            if ui.small_button(I18nKey::TbConical.tr_ui(lang)).clicked() {
                app.apply_fill(Fill::ConicalGradient {
                    center: Point2D::new(30.0, 30.0),
                    angle_deg: 0.0,
                    stops: vec![
                        GradientStop::new(0.0, app.properties_state.grad_start_color.clone()),
                        GradientStop::new(1.0, app.properties_state.grad_end_color.clone()),
                    ],
                });
            }
        }
        ToolId::SmartFill => {
            let mut c32 = core_color_to_egui(
                &app.default_fill
                    .referenced_colors()
                    .into_iter()
                    .next()
                    .unwrap_or(coree_core::Color::CYAN),
            );
            ui.label(I18nKey::TbFillColorLabel.tr_ui(lang));
            if ui.color_edit_button_srgba(&mut c32).changed() {
                app.default_fill = Fill::Solid(egui_color_to_core(c32));
            }
            ui.label(
                RichText::new(I18nKey::TbSmartFillHint.tr_ui(lang))
                    .size(11.0)
                    .color(tokens.text_muted),
            );
        }
        _ => {}
    }
}

pub(super) fn show_eyedropper_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    let lang = app.ui_language;
    ui.label(
        RichText::new(format!(
            "{}:",
            crate::i18n::tool_i18n_key(app.active_tool).tr_ui(lang)
        ))
        .color(tokens.accent)
        .strong(),
    );
    ui.selectable_value(
        &mut app.tool_settings.eyedropper_apply_mode,
        false,
        I18nKey::TbEyedropperSample.tr_ui(lang),
    );
    ui.selectable_value(
        &mut app.tool_settings.eyedropper_apply_mode,
        true,
        I18nKey::TbEyedropperApply.tr_ui(lang),
    );
    ui.separator();
    let sample_c = app
        .default_fill
        .referenced_colors()
        .into_iter()
        .next()
        .unwrap_or(coree_core::Color::CYAN);
    let (sw_rect, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    ui.painter().rect(
        sw_rect,
        CornerRadius::same(2),
        core_color_to_egui(&sample_c),
        Stroke::new(1.0, tokens.border_input),
        StrokeKind::Inside,
    );
    ui.label(
        RichText::new(I18nKey::TbEyedropperHint.tr_ui(lang))
            .size(11.0)
            .color(tokens.text_muted),
    );
}

pub(super) fn show_crop_cut_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    let lang = app.ui_language;
    ui.label(
        RichText::new(format!(
            "{}:",
            crate::i18n::tool_i18n_key(app.active_tool).tr_ui(lang)
        ))
        .color(tokens.accent)
        .strong(),
    );
    match app.active_tool {
        ToolId::Crop => {
            if let Some(mut rect) = app.active_tab().and_then(|t| t.pending_crop_rect) {
                let mut w = rect.width();
                let mut h = rect.height();
                let mut changed = false;
                changed |= ui
                    .add(
                        DragValue::new(&mut rect.min_x)
                            .prefix(I18nKey::PropPositionX.tr_ui(lang))
                            .suffix(" mm"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        DragValue::new(&mut rect.min_y)
                            .prefix(I18nKey::PropPositionY.tr_ui(lang))
                            .suffix(" mm"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        DragValue::new(&mut w)
                            .range(1.0..=5000.0)
                            .prefix(I18nKey::PropWidth.tr_ui(lang))
                            .suffix(" mm"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        DragValue::new(&mut h)
                            .range(1.0..=5000.0)
                            .prefix(I18nKey::PropHeight.tr_ui(lang))
                            .suffix(" mm"),
                    )
                    .changed();
                if changed {
                    rect.max_x = rect.min_x + w;
                    rect.max_y = rect.min_y + h;
                    if let Some(tab) = app.active_tab_mut() {
                        tab.pending_crop_rect = Some(rect);
                    }
                }
                if ui
                    .small_button(I18nKey::TbCropConfirm.tr_ui(lang))
                    .clicked()
                {
                    app.confirm_active_tool_action();
                }
                if ui.small_button(I18nKey::DialogCancel.tr_ui(lang)).clicked() {
                    if let Some(tab) = app.active_tab_mut() {
                        tab.pending_crop_rect = None;
                    }
                }
            } else {
                ui.label(
                    RichText::new(I18nKey::TbCropHint.tr_ui(lang))
                        .size(11.0)
                        .color(tokens.text_muted),
                );
            }
        }
        ToolId::Eraser => {
            ui.add(
                DragValue::new(&mut app.tool_settings.eraser_radius_mm)
                    .range(0.5..=60.0)
                    .speed(0.2)
                    .prefix(I18nKey::TbEraserRadiusPrefix.tr_ui(lang))
                    .suffix(" mm"),
            );
        }
        _ => {
            ui.label(
                RichText::new(I18nKey::TbKnifeHint.tr_ui(lang))
                    .size(11.0)
                    .color(tokens.text_muted),
            );
        }
    }
}

pub(super) fn show_zoom_pan_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    let lang = app.ui_language;
    ui.label(
        RichText::new(format!(
            "{}:",
            crate::i18n::tool_i18n_key(app.active_tool).tr_ui(lang)
        ))
        .color(tokens.accent)
        .strong(),
    );
    if ui.small_button(I18nKey::TbZoomIn.tr_ui(lang)).clicked() {
        app.zoom_step(1.25);
    }
    if ui.small_button(I18nKey::TbZoomOut.tr_ui(lang)).clicked() {
        app.zoom_step(0.8);
    }
    if ui
        .small_button(I18nKey::TbZoomToSelectedShort.tr_ui(lang))
        .clicked()
    {
        app.handle_shortcut(ShortcutAction::ZoomToSelected);
    }
    if ui
        .small_button(I18nKey::TbZoomToAllShort.tr_ui(lang))
        .clicked()
    {
        app.handle_shortcut(ShortcutAction::ZoomToAll);
    }
    if ui
        .small_button(I18nKey::TbZoomToPageShort.tr_ui(lang))
        .clicked()
    {
        app.handle_shortcut(ShortcutAction::ZoomToPage);
    }
    if ui.small_button("100%").clicked() {
        app.handle_shortcut(ShortcutAction::ZoomActual);
    }
}

// -----------------------------------------------------------------------------
// Text & Arabic RTL / Kashida Property Bar
// -----------------------------------------------------------------------------

pub(super) fn show_text_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    let lang = app.ui_language;
    ui.label(
        RichText::new(I18nKey::TbTextLabel.tr_ui(lang))
            .color(tokens.accent)
            .strong(),
    );

    let selected_text = app.active_tab().and_then(|t| {
        let oid = *t.selection.objects.first()?;
        let obj = t.document.object(oid)?;
        match &obj.payload {
            NodePayload::ArtisticText(_) | NodePayload::ParagraphText(_) => {
                Some((oid, obj.payload.clone()))
            }
            _ => None,
        }
    });

    if let Some((oid, mut payload)) = selected_text {
        let mut changed = false;
        match &mut payload {
            NodePayload::ArtisticText(art) => {
                if let Some(run) = art.runs.first_mut() {
                    changed |= render_text_style_strip(
                        ui,
                        &mut run.style,
                        &mut art.direction,
                        &mut art.align,
                        "prop_txt_font",
                        lang,
                    );
                }
            }
            NodePayload::ParagraphText(para) => {
                if let Some(run) = para.runs.first_mut() {
                    changed |= render_text_style_strip(
                        ui,
                        &mut run.style,
                        &mut para.direction,
                        &mut para.align,
                        "prop_para_font",
                        lang,
                    );
                }
            }
            _ => {}
        }
        if changed {
            let _ = app.execute_command(CommandOp::EditText { id: oid, payload });
        }
    } else {
        let mut dir = app.default_text_style.direction;
        let mut align = app.default_text_style.align;
        let changed = render_text_style_strip(
            ui,
            &mut app.default_text_style,
            &mut dir,
            &mut align,
            "prop_def_font",
            lang,
        );
        if changed {
            app.default_text_style.direction = dir;
            app.default_text_style.align = align;
        }
        ui.separator();
        let hint = I18nKey::TbDraftTextHint.tr_ui(lang);
        crate::theme::bidi_text_edit_singleline(
            ui,
            &mut app.tool_settings.text_insert_draft,
            Some(&hint),
            Some(110.0),
        );
    }

    ui.separator();
    if ui
        .small_button(I18nKey::TbFitToPath.tr_ui(lang))
        .on_hover_text(I18nKey::TextFitToPath.tr_ui(lang))
        .clicked()
    {
        app.fit_selected_text_to_path();
    }
    if ui
        .small_button(I18nKey::TbToCurves.tr_ui(lang))
        .on_hover_text(I18nKey::TbToCurvesTip.tr_ui(lang))
        .clicked()
    {
        app.convert_selection_to_curves();
    }
    if ui
        .small_button(I18nKey::MenuGlyphsShort.tr_ui(lang))
        .on_hover_text(I18nKey::TbGlyphsTip.tr_ui(lang))
        .clicked()
    {
        app.properties_state.active_tab = PropertiesSubTab::Text;
        app.focus_docker(DockerTab::Properties);
    }
}

pub(super) fn render_text_style_strip(
    ui: &mut Ui,
    style: &mut coree_core::TextStyle,
    direction: &mut TextDirection,
    align: &mut TextAlign,
    combo_salt: &str,
    lang: crate::i18n::StudioLanguage,
) -> bool {
    let mut changed = false;
    ComboBox::from_id_salt(combo_salt)
        .width(116.0)
        .selected_text(&style.font_family)
        .show_ui(ui, |ui| {
            for f in COMMON_FONTS {
                if ui.selectable_label(style.font_family == f, f).clicked() {
                    style.font_family = f.to_owned();
                    changed = true;
                }
            }
        });
    changed |= ui
        .add(
            DragValue::new(&mut style.font_size_pt)
                .range(4.0..=400.0)
                .speed(0.5)
                .suffix(" pt"),
        )
        .changed();
    if ui
        .selectable_label(style.bold, RichText::new("B").strong())
        .clicked()
    {
        style.set_weight(if style.bold { 400 } else { 700 });
        changed = true;
    }
    let cur_w = style.effective_weight();
    ComboBox::from_id_salt((combo_salt, "weight"))
        .width(64.0)
        .selected_text(format!("w{cur_w}"))
        .show_ui(ui, |ui| {
            for (w, lbl) in [
                (100, "100 Thin"),
                (300, "300 Light"),
                (400, "400 Regular"),
                (500, "500 Medium"),
                (600, "600 SemiBold"),
                (700, "700 Bold"),
                (800, "800 ExtraBold"),
                (900, "900 Black"),
            ] {
                if ui.selectable_label(cur_w == w, lbl).clicked() {
                    style.set_weight(w);
                    changed = true;
                }
            }
        });
    if ui
        .selectable_label(style.italic, RichText::new("I").italics())
        .clicked()
    {
        style.italic = !style.italic;
        changed = true;
    }
    if ui
        .selectable_label(style.underline, RichText::new("U").underline())
        .clicked()
    {
        style.underline = !style.underline;
        changed = true;
    }
    let bidi_cur = style.bidi_font_family.as_deref().unwrap_or("Auto");
    ComboBox::from_id_salt((combo_salt, "bidi_font"))
        .width(96.0)
        .selected_text(format!("RTL:{bidi_cur}"))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(
                    style.bidi_font_family.is_none(),
                    I18nKey::TbAutoPrimary.tr_ui(lang),
                )
                .clicked()
            {
                style.bidi_font_family = None;
                changed = true;
            }
            for f in [
                "Noto Sans Arabic",
                "Noto Naskh Arabic",
                "Amiri",
                "Cairo",
                "Tahoma",
                "Arial",
            ] {
                if ui
                    .selectable_label(style.bidi_font_family.as_deref() == Some(f), f)
                    .clicked()
                {
                    style.bidi_font_family = Some(f.to_owned());
                    changed = true;
                }
            }
        });
    if ui
        .selectable_label(style.opentype_features.liga, "liga")
        .on_hover_text(I18nKey::TbOtLigaTip.tr_ui(lang))
        .clicked()
    {
        style.opentype_features.liga = !style.opentype_features.liga;
        changed = true;
    }
    if ui
        .selectable_label(style.opentype_features.calt, "calt")
        .on_hover_text(I18nKey::TbOtCaltTip.tr_ui(lang))
        .clicked()
    {
        style.opentype_features.calt = !style.opentype_features.calt;
        changed = true;
    }
    if ui
        .selectable_label(style.opentype_features.kern, "kern")
        .on_hover_text(I18nKey::TbOtKernTip.tr_ui(lang))
        .clicked()
    {
        style.opentype_features.kern = !style.opentype_features.kern;
        changed = true;
    }
    ui.separator();
    for (al, lbl) in [
        (TextAlign::Left, "L"),
        (TextAlign::Center, "C"),
        (TextAlign::Right, "R"),
        (TextAlign::Justify, "J"),
    ] {
        if ui.selectable_label(*align == al, lbl).clicked() {
            *align = al;
            style.align = al;
            changed = true;
        }
    }
    ui.separator();
    let is_rtl = *direction == TextDirection::Rtl;
    if ui
        .selectable_label(is_rtl, "RTL")
        .on_hover_text(if is_rtl {
            I18nKey::TextLtrDirection.tr_ui(lang)
        } else {
            I18nKey::TextRtlDirection.tr_ui(lang)
        })
        .clicked()
    {
        let next = if is_rtl {
            TextDirection::Ltr
        } else {
            TextDirection::Rtl
        };
        *direction = next;
        style.direction = next;
        changed = true;
    }
    let is_kashida = *align == TextAlign::ForceJustifyKashida;
    if ui
        .selectable_label(is_kashida, "Kashida")
        .on_hover_text(I18nKey::TextKashidaJustify.tr_ui(lang))
        .clicked()
    {
        let next = if is_kashida {
            TextAlign::Right
        } else {
            TextAlign::ForceJustifyKashida
        };
        *align = next;
        style.align = next;
        if next == TextAlign::ForceJustifyKashida && style.kashida_stretch < 0.1 {
            style.kashida_stretch = 0.65;
        }
        changed = true;
    }
    changed |= ui
        .add(
            DragValue::new(&mut style.kashida_stretch)
                .range(0.0..=2.0)
                .speed(0.05)
                .prefix("K: "),
        )
        .on_hover_text(I18nKey::TbKashidaStretchTip.tr_ui(lang))
        .changed();
    changed
}

pub(super) fn render_raster_trace_quick_strip(
    ui: &mut Ui,
    app: &mut StudioApp,
    lang: crate::i18n::StudioLanguage,
    tokens: &ThemeTokens,
) {
    ui.separator();
    if ui
        .button(
            RichText::new(I18nKey::BitmapTraceVector.tr_ui(lang))
                .size(11.0)
                .strong()
                .color(tokens.accent),
        )
        .clicked()
    {
        app.trace_selected_raster_interactive();
    }
    if ui
        .small_button(I18nKey::DockerVectorTrace.tr_ui(lang))
        .clicked()
    {
        app.trace_state.split_preview = true;
        app.focus_docker(DockerTab::VectorTrace);
    }
}

// -----------------------------------------------------------------------------
// Document / Page Defaults Property Bar (0 Objects Selected)
// -----------------------------------------------------------------------------
