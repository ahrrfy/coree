// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

pub(super) fn ui_lens_docker(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let tokens = app.theme.tokens();
    let has_sel = app.active_tab().is_some_and(|t| !t.selection.is_empty());

    if !has_sel {
        ui.label(
            RichText::new(crate::i18n::I18nKey::DockLensEmptyHint.tr_ui(lang))
                .color(tokens.text_muted),
        );
    }

    let ls = &mut app.lens_state;
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::TbLensLabel.tr_ui(lang));
        ComboBox::from_id_salt("docker_lens_picker")
            .width(175.0)
            .selected_text(LensDockerState::NAMES[ls.selected_index])
            .show_ui(ui, |ui| {
                for (idx, name) in LensDockerState::NAMES.iter().enumerate() {
                    ui.selectable_value(&mut ls.selected_index, idx, *name);
                }
            });
    });

    ui.separator();
    match ls.selected_index {
        0 => {
            ui.add(
                egui::Slider::new(&mut ls.rate_pct, -100.0..=100.0)
                    .text(crate::i18n::I18nKey::DockLensBrightenPct.tr_ui(lang)),
            );
        }
        1 | 2 | 9 => {
            ui.add(
                egui::Slider::new(&mut ls.rate_pct, 0.0..=100.0)
                    .text(crate::i18n::I18nKey::DockLensRatePct.tr_ui(lang)),
            );
            let mut c32 = core_color_to_egui(&ls.tint_color);
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DockPropColorLabel.tr_ui(lang));
                if ui.color_edit_button_srgba(&mut c32).changed() {
                    ls.tint_color = egui_color_to_core(c32);
                }
            });
        }
        3 => {
            let mut fc = core_color_to_egui(&ls.from_color);
            let mut tc = core_color_to_egui(&ls.to_color);
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DockLensFrom.tr_ui(lang));
                if ui.color_edit_button_srgba(&mut fc).changed() {
                    ls.from_color = egui_color_to_core(fc);
                }
                ui.label(crate::i18n::I18nKey::DockLensTo.tr_ui(lang));
                if ui.color_edit_button_srgba(&mut tc).changed() {
                    ls.to_color = egui_color_to_core(tc);
                }
            });
            ComboBox::from_id_salt("lens_map_mode")
                .selected_text(format!("{:?}", ls.map_mode))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut ls.map_mode,
                        ColorMapMode::Direct,
                        crate::i18n::I18nKey::DockLensDirect.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut ls.map_mode,
                        ColorMapMode::ForwardRainbow,
                        crate::i18n::I18nKey::DockLensForwardRainbow.tr_ui(lang),
                    );
                    ui.selectable_value(
                        &mut ls.map_mode,
                        ColorMapMode::ReverseRainbow,
                        crate::i18n::I18nKey::DockLensReverseRainbow.tr_ui(lang),
                    );
                });
        }
        4 => {
            ui.add(
                egui::Slider::new(&mut ls.rate_pct, -1000.0..=1000.0)
                    .text(crate::i18n::I18nKey::DockLensFishEyePct.tr_ui(lang)),
            );
        }
        5 => {
            ui.add(
                egui::Slider::new(&mut ls.heat_rotation_deg, 0.0..=360.0)
                    .text(crate::i18n::I18nKey::DockLensHeatRot.tr_ui(lang)),
            );
        }
        6 => {
            ui.label(crate::i18n::I18nKey::DockLensInvertDesc.tr_ui(lang));
        }
        7 => {
            ui.add(
                egui::Slider::new(&mut ls.magnify_amount, 1.0..=20.0)
                    .text(crate::i18n::I18nKey::DockLensMagX.tr_ui(lang)),
            );
        }
        8 => {
            let mut tc = core_color_to_egui(&ls.tint_color);
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DockLensTintColor.tr_ui(lang));
                if ui.color_edit_button_srgba(&mut tc).changed() {
                    ls.tint_color = egui_color_to_core(tc);
                }
            });
        }
        _ => {
            let mut oc = core_color_to_egui(&ls.wireframe_outline);
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DockLensOverrideOutline.tr_ui(lang));
                if ui.color_edit_button_srgba(&mut oc).changed() {
                    ls.wireframe_outline = egui_color_to_core(oc);
                }
            });
            ui.checkbox(
                &mut ls.use_wireframe_fill,
                crate::i18n::I18nKey::DockLensOverrideFill.tr_ui(lang),
            );
            if ls.use_wireframe_fill {
                let mut fc = core_color_to_egui(&ls.wireframe_fill);
                if ui.color_edit_button_srgba(&mut fc).changed() {
                    ls.wireframe_fill = egui_color_to_core(fc);
                }
            }
        }
    }

    ui.separator();
    ui.checkbox(
        &mut ls.frozen,
        crate::i18n::I18nKey::DockLensFrozenLock.tr_ui(lang),
    );
    ui.checkbox(
        &mut ls.remove_face,
        crate::i18n::I18nKey::TbLensRemoveFace.tr_ui(lang),
    );
    ui.checkbox(
        &mut ls.use_viewpoint,
        crate::i18n::I18nKey::DockLensCustomViewpoint.tr_ui(lang),
    );
    if ls.use_viewpoint {
        shell_dir.with_horizontal(ui, |ui| {
            ui.add(
                DragValue::new(&mut ls.viewpoint_x)
                    .prefix("X: ")
                    .suffix(" mm"),
            );
            ui.add(
                DragValue::new(&mut ls.viewpoint_y)
                    .prefix("Y: ")
                    .suffix(" mm"),
            );
        });
    }

    ui.add_space(6.0);
    let cfg = ls.build_config();
    shell_dir.with_horizontal(ui, |ui| {
        if ui
            .button(RichText::new(crate::i18n::I18nKey::TbApplyLens.tr_ui(lang)).strong())
            .clicked()
        {
            app.apply_lens_to_selection(Some(cfg));
        }
        if ui
            .button(crate::i18n::I18nKey::MenuRemoveLens.tr_ui(lang))
            .clicked()
        {
            app.apply_lens_to_selection(None);
        }
    });
}

// =============================================================================
// 4. Interactive Vector Effects & PowerClip Docker (`S08`)
// =============================================================================

pub(super) fn ui_effects_docker(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    const EFFECT_NAMES: [&str; 13] = [
        "Drop Shadow",
        "Block Shadow",
        "Contour",
        "Blend",
        "Distort",
        "Envelope",
        "Extrude 2.5D",
        "Transparency",
        "Perspective",
        "Symmetry",
        "Inner Shadow",
        "Bevel",
        "Extrude 3D",
    ];

    ComboBox::from_id_salt("effects_kind_combo")
        .width(200.0)
        .selected_text(EFFECT_NAMES[app.effects_state.selected_effect_kind.min(12)])
        .show_ui(ui, |ui| {
            for (i, n) in EFFECT_NAMES.iter().enumerate() {
                ui.selectable_value(&mut app.effects_state.selected_effect_kind, i, *n);
            }
        });

    let (copy_target_id, envelope_sources): (Option<ObjectId>, Vec<(ObjectId, String)>) =
        if app.effects_state.selected_effect_kind == 5 {
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
    let mut copy_envelope_from: Option<ObjectId> = None;

    let es = &mut app.effects_state;
    match es.selected_effect_kind {
        0 => {
            ui.add(
                egui::Slider::new(&mut es.dx_mm, -30.0..=30.0)
                    .text(crate::i18n::I18nKey::DockEffectsOffsetX.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.dy_mm, -30.0..=30.0)
                    .text(crate::i18n::I18nKey::DockEffectsOffsetY.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.blur_mm, 0.0..=20.0)
                    .text(crate::i18n::I18nKey::DockEffectsFeatherMm.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.opacity, 0.0..=1.0)
                    .text(crate::i18n::I18nKey::DockEffectsAlpha.tr_ui(lang)),
            );
        }
        1 => {
            ui.add(
                egui::Slider::new(&mut es.dx_mm, -40.0..=40.0)
                    .text(crate::i18n::I18nKey::DockEffectsDepthX.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.dy_mm, -40.0..=40.0)
                    .text(crate::i18n::I18nKey::DockEffectsDepthY.tr_ui(lang)),
            );
            ui.checkbox(
                &mut es.block_shadow_simplify,
                crate::i18n::I18nKey::DockEffectsSimplify.tr_ui(lang),
            );
        }
        2 => {
            shell_dir.with_horizontal(ui, |ui| {
                ui.selectable_value(
                    &mut es.contour_mode,
                    ContourMode::ToCenter,
                    crate::i18n::I18nKey::DockEffectsCenter.tr_ui(lang),
                );
                ui.selectable_value(
                    &mut es.contour_mode,
                    ContourMode::Inside,
                    crate::i18n::I18nKey::TbInside.tr_ui(lang),
                );
                ui.selectable_value(
                    &mut es.contour_mode,
                    ContourMode::Outside,
                    crate::i18n::I18nKey::TbOutside.tr_ui(lang),
                );
            });
            ui.add(
                egui::Slider::new(&mut es.steps, 1..=512)
                    .text(crate::i18n::I18nKey::DockEffectsSteps.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.offset_mm, 0.5..=20.0)
                    .text(crate::i18n::I18nKey::DockEffectsOffsetMm.tr_ui(lang)),
            );
        }
        3 => {
            ui.add(
                egui::Slider::new(&mut es.steps, 1..=200)
                    .text(crate::i18n::I18nKey::DockEffectsBlendSteps.tr_ui(lang)),
            );
            ui.checkbox(
                &mut es.rotate_along_path,
                crate::i18n::I18nKey::DockEffectsRotateTangent.tr_ui(lang),
            );
        }
        4 => {
            shell_dir.with_horizontal(ui, |ui| {
                ui.selectable_value(
                    &mut es.distort_kind,
                    DistortKind::PushPull,
                    crate::i18n::I18nKey::TbDistortPushPull.tr_ui(lang),
                );
                ui.selectable_value(
                    &mut es.distort_kind,
                    DistortKind::Zipper,
                    crate::i18n::I18nKey::TbDistortZipper.tr_ui(lang),
                );
                ui.selectable_value(
                    &mut es.distort_kind,
                    DistortKind::Twister,
                    crate::i18n::I18nKey::TbDistortTwister.tr_ui(lang),
                );
            });
            ui.add(
                egui::Slider::new(&mut es.amplitude, 1.0..=90.0)
                    .text(crate::i18n::I18nKey::DockEffectsAmplitude.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.frequency, 0.5..=10.0)
                    .text(crate::i18n::I18nKey::DockEffectsFrequency.tr_ui(lang)),
            );
        }
        5 => {
            shell_dir.with_horizontal_wrapped(ui, |ui| {
                ui.selectable_value(
                    &mut es.envelope_mode,
                    EnvelopeMode::Straight,
                    crate::i18n::I18nKey::TbEnvStraight.tr_ui(lang),
                );
                ui.selectable_value(
                    &mut es.envelope_mode,
                    EnvelopeMode::SingleArc,
                    crate::i18n::I18nKey::TbEnvSingleArc.tr_ui(lang),
                );
                ui.selectable_value(
                    &mut es.envelope_mode,
                    EnvelopeMode::DoubleArc,
                    crate::i18n::I18nKey::TbEnvDoubleArc.tr_ui(lang),
                );
                ui.selectable_value(
                    &mut es.envelope_mode,
                    EnvelopeMode::Unconstrained,
                    "Unconstrained",
                );
            });
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
                shell_dir.with_horizontal(ui, |ui| {
                    ComboBox::from_id_salt("effects_copy_env_src")
                        .width(120.0)
                        .selected_text(sel_label)
                        .show_ui(ui, |ui| {
                            for (oid, label) in &envelope_sources {
                                ui.selectable_value(
                                    &mut es.copy_envelope_source,
                                    Some(*oid),
                                    label,
                                );
                            }
                        });
                    if ui
                        .button(crate::i18n::I18nKey::TbCopyEnvelope.tr_ui(lang))
                        .clicked()
                    {
                        copy_envelope_from = es.copy_envelope_source;
                    }
                });
            }
        }
        6 => {
            ui.add(
                egui::Slider::new(&mut es.extrude_depth, 2.0..=60.0)
                    .text(crate::i18n::I18nKey::DockEffectsDepthMm.tr_ui(lang)),
            );
        }
        7 => {
            const TRANSP_LABELS: [&str; 6] = [
                "Uniform",
                "Fountain Linear",
                "Pattern (Checker)",
                "Texture (Marble)",
                "Conical",
                "Square",
            ];
            ComboBox::from_id_salt("effects_transp_mode")
                .width(180.0)
                .selected_text(TRANSP_LABELS[es.transp_kind.min(5)])
                .show_ui(ui, |ui| {
                    for (idx, lbl) in TRANSP_LABELS.iter().enumerate() {
                        ui.selectable_value(&mut es.transp_kind, idx, *lbl);
                    }
                });
            ui.add(
                egui::Slider::new(&mut es.opacity, 0.0..=1.0)
                    .text(crate::i18n::I18nKey::DockEffectsAlpha.tr_ui(lang)),
            );
        }
        9 => {
            ui.add(
                egui::Slider::new(&mut es.symmetry_axes, 2..=16)
                    .text(crate::i18n::I18nKey::DockEffectsAxes.tr_ui(lang)),
            );
            ui.checkbox(
                &mut es.symmetry_mirror,
                crate::i18n::I18nKey::DockEffectsMirrorRefl.tr_ui(lang),
            );
        }
        10 => {
            ui.add(
                egui::Slider::new(&mut es.dx_mm, -30.0..=30.0)
                    .text(crate::i18n::I18nKey::DockEffectsOffsetX.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.dy_mm, -30.0..=30.0)
                    .text(crate::i18n::I18nKey::DockEffectsOffsetY.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.blur_mm, 0.0..=20.0)
                    .text(crate::i18n::I18nKey::DockEffectsBlurMm.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.opacity, 0.0..=1.0)
                    .text(crate::i18n::I18nKey::DockEffectsAlpha.tr_ui(lang)),
            );
        }
        11 => {
            shell_dir.with_horizontal(ui, |ui| {
                ui.selectable_value(
                    &mut es.bevel_style,
                    coree_core::BevelStyle::SoftEdge,
                    crate::i18n::I18nKey::DockEffectsBevelSoft.tr_ui(lang),
                );
                ui.selectable_value(
                    &mut es.bevel_style,
                    coree_core::BevelStyle::Emboss,
                    crate::i18n::I18nKey::DockEffectsBevelEmboss.tr_ui(lang),
                );
            });
            ui.add(
                egui::Slider::new(&mut es.bevel_distance_mm, 0.5..=20.0)
                    .text(crate::i18n::I18nKey::DockEffectsDistanceMm.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.bevel_softness_mm, 0.0..=10.0)
                    .text(crate::i18n::I18nKey::DockEffectsSoftnessMm.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.bevel_light_angle_deg, 0.0..=360.0)
                    .text(crate::i18n::I18nKey::DockEffectsAngle.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.bevel_intensity, 0.0..=1.0)
                    .text(crate::i18n::I18nKey::DockEffectsIntensity.tr_ui(lang)),
            );
        }
        12 => {
            ui.add(
                egui::Slider::new(&mut es.extrude_depth, 2.0..=60.0)
                    .text(crate::i18n::I18nKey::DockEffectsDepthMm.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.extrude3d_rotation[0], -85.0..=85.0)
                    .text(crate::i18n::I18nKey::DockEffectsRotX.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.extrude3d_rotation[1], -85.0..=85.0)
                    .text(crate::i18n::I18nKey::DockEffectsRotY.tr_ui(lang)),
            );
            ui.add(
                egui::Slider::new(&mut es.extrude3d_light_intensity, 0.0..=2.0)
                    .text(crate::i18n::I18nKey::DockEffectsLight.tr_ui(lang)),
            );
        }
        _ => {}
    }

    if let (Some(dst), Some(src)) = (copy_target_id, copy_envelope_from) {
        app.copy_envelope_from_object(dst, src);
    }

    if ui
        .button(crate::i18n::I18nKey::DockEffectsAddBtn.tr_ui(lang))
        .clicked()
    {
        let eff = app.build_selected_effect();
        app.add_effect_to_selection(eff);
    }

    // Active effects stack on selected object
    let active_stack = app.active_tab().and_then(|t| {
        let oid = *t.selection.objects.first()?;
        let obj = t.document.object(oid)?;
        Some((oid, obj.effects.clone()))
    });
    if let Some((oid, stack)) = active_stack {
        if !stack.is_empty() {
            ui.separator();
            ui.label(RichText::new(format!("Active Stack ({})", stack.len())).strong());
            let mut remove_idx = None;
            for (idx, eff) in stack.iter().enumerate() {
                shell_dir.with_horizontal(ui, |ui| {
                    let name = format!("{eff:?}");
                    let short = name.split(['{', '(']).next().unwrap_or("Effect");
                    ui.label(format!("{}. {short}", idx + 1));
                    if ui.small_button("×").clicked() {
                        remove_idx = Some(idx);
                    }
                });
            }
            if let Some(i) = remove_idx {
                app.remove_effect_on(oid, i);
            }
        }
    }

    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DockEffectsPowerClipHdr.tr_ui(lang)).strong());
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        if ui
            .button(crate::i18n::I18nKey::DockEffectsPlaceInside.tr_ui(lang))
            .clicked()
        {
            app.create_powerclip_from_selection();
        }
        if ui
            .button(crate::i18n::I18nKey::ObjectPowerClipExtract.tr_ui(lang))
            .clicked()
        {
            app.extract_selected_powerclip();
        }
        if ui
            .button(crate::i18n::I18nKey::ObjectPowerClipFitContents.tr_ui(lang))
            .clicked()
        {
            app.fit_powerclip_selection(coree_core::PowerClipFitMode::FitContents);
        }
        if ui
            .button(crate::i18n::I18nKey::ObjectPowerClipCenterContents.tr_ui(lang))
            .clicked()
        {
            app.fit_powerclip_selection(coree_core::PowerClipFitMode::CenterContents);
        }
        if ui
            .button(crate::i18n::I18nKey::ObjectPowerClipStretchToFill.tr_ui(lang))
            .clicked()
        {
            app.fit_powerclip_selection(coree_core::PowerClipFitMode::StretchToFill);
        }
        if ui
            .button(crate::i18n::I18nKey::ObjectMoveToDesktop.tr_ui(lang))
            .clicked()
        {
            app.move_selection_to_desktop_layer();
        }
    });
    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DockEffectsDrawingModes.tr_ui(lang)).strong());
    ui.checkbox(
        &mut app.tool_settings.symmetry_enabled,
        crate::i18n::I18nKey::DockEffectsLiveSymmetry.tr_ui(lang),
    );
    if app.tool_settings.symmetry_enabled {
        shell_dir.with_horizontal(ui, |ui| {
            ui.add(
                egui::Slider::new(&mut app.tool_settings.symmetry_folds, 2..=16)
                    .text(crate::i18n::I18nKey::DockEffectsFolds.tr_ui(lang)),
            );
            ui.checkbox(
                &mut app.tool_settings.symmetry_mirror,
                crate::i18n::I18nKey::DockEffectsMirror.tr_ui(lang),
            );
        });
    }
    ui.checkbox(
        &mut app.tool_settings.draw_in_perspective,
        crate::i18n::I18nKey::DockEffectsDrawPerspective.tr_ui(lang),
    );
    let selected_pc_lock = app.active_tab().and_then(|t| {
        t.selection.objects.iter().find_map(|id| {
            let obj = t.document.object(*id)?;
            if let NodePayload::PowerClip { lock_contents, .. } = &obj.payload {
                Some(*lock_contents)
            } else {
                None
            }
        })
    });
    if let Some(mut locked) = selected_pc_lock {
        if ui
            .checkbox(&mut locked, "Lock Contents to PowerClip")
            .changed()
        {
            app.set_powerclip_lock_selected(Some(locked));
        }
    }
}

// =============================================================================
// 5. Transform, Align & Shaping Booleans Docker
// =============================================================================

pub(super) fn ui_transform_align_shape_docker(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    ui.label(RichText::new(crate::i18n::I18nKey::DockTransformHeading.tr_ui(lang)).strong());
    let ts = &mut app.transform_state;
    shell_dir.with_horizontal(ui, |ui| {
        ui.add(
            DragValue::new(&mut ts.move_dx_mm)
                .prefix("ΔX: ")
                .suffix(" mm"),
        );
        ui.add(
            DragValue::new(&mut ts.move_dy_mm)
                .prefix("ΔY: ")
                .suffix(" mm"),
        );
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.add(
            DragValue::new(&mut ts.rotate_deg)
                .prefix(crate::i18n::I18nKey::DockTransformRotPrefix.tr_ui(lang))
                .suffix("°"),
        );
        ui.add(
            DragValue::new(&mut ts.copies)
                .range(0..=50)
                .prefix(crate::i18n::I18nKey::DockTransformCopiesPrefix.tr_ui(lang)),
        );
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.add(
            DragValue::new(&mut ts.scale_x_pct)
                .range(5.0..=500.0)
                .prefix(format!(
                    "{}: ",
                    crate::i18n::I18nKey::PropScaleX.tr_ui(lang)
                ))
                .suffix("%"),
        );
        ui.add(
            DragValue::new(&mut ts.scale_y_pct)
                .range(5.0..=500.0)
                .prefix(format!(
                    "{}: ",
                    crate::i18n::I18nKey::PropScaleY.tr_ui(lang)
                ))
                .suffix("%"),
        );
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.add(
            DragValue::new(&mut ts.skew_x_deg)
                .range(-60.0..=60.0)
                .prefix(format!("{}: ", crate::i18n::I18nKey::PropSkewX.tr_ui(lang)))
                .suffix("°"),
        );
        ui.add(
            DragValue::new(&mut ts.skew_y_deg)
                .range(-60.0..=60.0)
                .prefix(format!("{}: ", crate::i18n::I18nKey::PropSkewY.tr_ui(lang)))
                .suffix("°"),
        );
    });

    if ui
        .button(crate::i18n::I18nKey::DockTransformApplyBtn.tr_ui(lang))
        .clicked()
    {
        let ts_c = app.transform_state.clone();
        let pivot = app
            .active_tab()
            .and_then(|t| t.selection.bounding_box(&t.document))
            .map_or(Point2D::ZERO, |b| b.center());
        let step_xf = Affine2D::translate(ts_c.move_dx_mm, ts_c.move_dy_mm)
            .compose(&Affine2D::translate(pivot.x, pivot.y))
            .compose(&Affine2D::rotate_deg(ts_c.rotate_deg))
            .compose(&Affine2D::skew(
                ts_c.skew_x_deg.to_radians().tan(),
                ts_c.skew_y_deg.to_radians().tan(),
            ))
            .compose(&Affine2D::scale(
                ts_c.scale_x_pct / 100.0,
                ts_c.scale_y_pct / 100.0,
            ))
            .compose(&Affine2D::translate(-pivot.x, -pivot.y));

        if ts_c.copies == 0 {
            let ids = app
                .active_tab()
                .map(|t| t.selection.objects.clone())
                .unwrap_or_default();
            if !ids.is_empty() {
                let _ = app.apply_world_transform_to_selection(&ids, step_xf);
            }
        } else {
            // Apply to Duplicate N copies sequentially with zero default offset so numeric step_xf is exact
            for _ in 0..ts_c.copies {
                if let Ok(dups) = app.duplicate_selection_with_offset(Vec2D::ZERO) {
                    if !dups.is_empty() {
                        let _ = app.apply_world_transform_to_selection(&dups, step_xf);
                    }
                }
            }
        }
    }

    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DockTransformAlignDistHdr.tr_ui(lang)).strong());
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        for (op, lbl) in [
            (AlignOp::Left, "Left"),
            (AlignOp::CenterH, "Center H"),
            (AlignOp::Right, "Right"),
            (AlignOp::Top, "Top"),
            (AlignOp::CenterV, "Center V"),
            (AlignOp::Bottom, "Bottom"),
            (AlignOp::CenterPage, "Center Page"),
        ] {
            if ui.small_button(lbl).clicked() {
                app.apply_align(op);
            }
        }
    });
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        if ui
            .small_button(crate::i18n::I18nKey::DockTransformDistributeH.tr_ui(lang))
            .clicked()
        {
            app.apply_distribute(DistributeOp::SpacingH);
        }
        if ui
            .small_button(crate::i18n::I18nKey::DockTransformDistributeV.tr_ui(lang))
            .clicked()
        {
            app.apply_distribute(DistributeOp::SpacingV);
        }
    });

    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DockTransformShapingHdr.tr_ui(lang)).strong());
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        for (op, lbl) in [
            (BooleanOp::Weld, "Weld"),
            (BooleanOp::Trim, "Trim"),
            (BooleanOp::Intersect, "Intersect"),
            (BooleanOp::Simplify, "Simplify"),
            (BooleanOp::FrontMinusBack, "Front−Back"),
            (BooleanOp::BackMinusFront, "Back−Front"),
            (BooleanOp::CreateBoundary, "Boundary"),
            (BooleanOp::Combine, "Combine"),
            (BooleanOp::BreakApart, "Break Apart"),
        ] {
            if ui.small_button(lbl).clicked() {
                app.apply_boolean_op(op);
            }
        }
    });
}

// =============================================================================
// 6. Vector Trace Docker (`S11`)
// =============================================================================
