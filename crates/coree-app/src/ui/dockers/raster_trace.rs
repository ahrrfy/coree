// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

pub(super) fn ui_vector_trace_docker(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let tokens = app.theme.tokens();
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    ui.label(
        RichText::new(crate::i18n::I18nKey::DockTraceHeading.tr_ui(lang))
            .color(tokens.text)
            .strong(),
    );

    // Primary high-contrast CTA button right at the top so the user never has to scroll to run Trace
    let cta_btn = egui::Button::new(
        RichText::new(crate::i18n::I18nKey::TraceRunVectorize.tr_ui(lang))
            .color(tokens.on_accent)
            .strong(),
    )
    .fill(tokens.accent)
    .min_size(vec2(ui.available_width().max(140.0), 26.0));
    if ui.add(cta_btn).clicked() {
        app.trace_selected_raster_interactive();
    }
    ui.add_space(2.0);

    let tr = &mut app.trace_state;
    shell_dir.with_horizontal_wrapped(ui, |ui| {
        ui.selectable_value(
            &mut tr.mode_idx,
            0,
            crate::i18n::I18nKey::TraceModeQuick.tr_ui(lang),
        );
        ui.selectable_value(
            &mut tr.mode_idx,
            1,
            crate::i18n::I18nKey::TraceModeOutline.tr_ui(lang),
        );
        ui.selectable_value(
            &mut tr.mode_idx,
            2,
            crate::i18n::I18nKey::TraceModeCenterline.tr_ui(lang),
        );
    });

    if tr.mode_idx == 1 {
        let prev_preset = tr.preset;
        ComboBox::from_id_salt("trace_preset")
            .width(170.0)
            .selected_text(format!("{:?}", tr.preset))
            .show_ui(ui, |ui| {
                for p in [
                    OutlinePreset::LineArt,
                    OutlinePreset::Logo,
                    OutlinePreset::DetailedLogo,
                    OutlinePreset::Clipart,
                    OutlinePreset::LowQualityImage,
                    OutlinePreset::HighQualityImage,
                ] {
                    ui.selectable_value(&mut tr.preset, p, format!("{p:?}"));
                }
            });
        if tr.preset != prev_preset {
            tr.apply_preset_defaults();
        }
    }

    ui.spacing_mut().slider_width = 105.0;
    ui.add(
        egui::Slider::new(&mut tr.max_colors, 2..=64)
            .text(crate::i18n::I18nKey::TraceMaxColors.tr_ui(lang)),
    );
    ui.add(
        egui::Slider::new(&mut tr.detail, 0.1..=1.0)
            .text(crate::i18n::I18nKey::TraceDetail.tr_ui(lang)),
    );
    ui.add(
        egui::Slider::new(&mut tr.smoothing, 0.0..=1.0)
            .text(crate::i18n::I18nKey::TraceSmoothing.tr_ui(lang)),
    );
    ui.add(
        egui::Slider::new(&mut tr.corner_threshold, 10.0..=160.0)
            .text(crate::i18n::I18nKey::TraceCorners.tr_ui(lang)),
    );
    ui.add(
        DragValue::new(&mut tr.speckle_size)
            .range(0..=1024)
            .prefix(format!(
                "{}: ",
                crate::i18n::I18nKey::TraceSpeckles.tr_ui(lang)
            ))
            .suffix(" px"),
    );
    ui.checkbox(
        &mut tr.remove_background,
        crate::i18n::I18nKey::TraceRemoveBackground.tr_ui(lang),
    );
    ui.checkbox(
        &mut tr.keep_original,
        crate::i18n::I18nKey::TraceKeepOriginal.tr_ui(lang),
    );
    if tr.keep_original {
        ui.checkbox(
            &mut tr.place_side_by_side,
            crate::i18n::I18nKey::TracePlaceBeside.tr_ui(lang),
        );
    }
    super::render_trace_split_preview(ui, app);
}

// =============================================================================
// 7. Integrated Bitmap Studio Docker (`S10` — all 19 RasterFilters + Brush/Resample)
// =============================================================================

pub(super) fn ui_bitmap_studio_docker(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    ui.label(RichText::new(crate::i18n::I18nKey::DockBmpHeading.tr_ui(lang)).strong());
    shell_dir.with_horizontal(ui, |ui| {
        ui.add(
            DragValue::new(&mut app.bitmap_state.convert_dpi)
                .range(72.0..=600.0)
                .suffix(" DPI"),
        );
        if ui
            .button(crate::i18n::I18nKey::DockBmpConvert.tr_ui(lang))
            .clicked()
        {
            app.convert_selection_to_bitmap();
        }
    });

    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DockBmpBrushHeading.tr_ui(lang)).strong());
    ComboBox::from_id_salt("bitmap_docker_raster_op")
        .width(190.0)
        .selected_text(app.tool_settings.raster_op.label())
        .show_ui(ui, |ui| {
            for op in RasterBrushOp::ALL {
                ui.selectable_value(&mut app.tool_settings.raster_op, op, op.label());
            }
        });
    ui.add(
        egui::Slider::new(&mut app.tool_settings.brush_radius_px, 1.0..=150.0)
            .text(crate::i18n::I18nKey::DockBmpRadiusPx.tr_ui(lang)),
    );
    ui.add(
        egui::Slider::new(&mut app.tool_settings.brush_hardness, 0.0..=1.0)
            .text(crate::i18n::I18nKey::DockBmpHardness.tr_ui(lang)),
    );
    let mut bc = core_color_to_egui(&app.tool_settings.brush_color);
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockBmpBrushColor.tr_ui(lang));
        if ui.color_edit_button_srgba(&mut bc).changed() {
            app.tool_settings.brush_color = egui_color_to_core(bc);
        }
    });
    match app.tool_settings.raster_op {
        RasterBrushOp::Dodge | RasterBrushOp::Burn => {
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(crate::i18n::I18nKey::DockBmpTone.tr_ui(lang));
                ui.selectable_value(
                    &mut app.tool_settings.dodge_burn_tone,
                    ToneRange::Shadows,
                    "Shadows",
                );
                ui.selectable_value(
                    &mut app.tool_settings.dodge_burn_tone,
                    ToneRange::Midtones,
                    "Midtones",
                );
                ui.selectable_value(
                    &mut app.tool_settings.dodge_burn_tone,
                    ToneRange::Highlights,
                    "Highlights",
                );
            });
            ui.add(
                egui::Slider::new(&mut app.tool_settings.dodge_burn_exposure, 0.05..=1.0)
                    .text(crate::i18n::I18nKey::DockBmpExposure.tr_ui(lang)),
            );
        }
        RasterBrushOp::FloodFill => {
            ui.add(
                egui::Slider::new(&mut app.tool_settings.flood_tolerance, 0..=255)
                    .text(crate::i18n::I18nKey::DockBmpTolerance.tr_ui(lang)),
            );
        }
        RasterBrushOp::Smudge => {
            ui.add(
                egui::Slider::new(&mut app.tool_settings.smudge_strength, 0.05..=1.0)
                    .text(crate::i18n::I18nKey::DockBmpSmudge.tr_ui(lang)),
            );
        }
        _ => {}
    }

    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DockBmpFiltersHeading.tr_ui(lang)).strong());
    let bm = &mut app.bitmap_state;
    ComboBox::from_id_salt("bitmap_filter_select")
        .width(210.0)
        .selected_text(BitmapDockerState::FILTER_NAMES[bm.filter_idx])
        .show_ui(ui, |ui| {
            for (idx, name) in BitmapDockerState::FILTER_NAMES.iter().enumerate() {
                ui.selectable_value(&mut bm.filter_idx, idx, *name);
            }
        });
    if bm.filter_idx == 1 {
        // ToneCurve parameters also editable numerically
        ui.label(RichText::new(crate::i18n::I18nKey::BmpToneCurveEditor.tr_ui(lang)).size(10.5));
    } else if bm.filter_idx == 19 {
        ui.checkbox(
            &mut bm.mixer_mono,
            crate::i18n::I18nKey::BmpChannelMonochrome.tr_ui(lang),
        );
        for (lbl, row) in [
            (
                crate::i18n::I18nKey::DockBmpRedOut.tr_ui(lang),
                &mut bm.mixer_red,
            ),
            (
                crate::i18n::I18nKey::DockBmpGreenOut.tr_ui(lang),
                &mut bm.mixer_green,
            ),
            (
                crate::i18n::I18nKey::DockBmpBlueOut.tr_ui(lang),
                &mut bm.mixer_blue,
            ),
        ] {
            shell_dir.with_horizontal(ui, |ui| {
                ui.label(RichText::new(lbl).size(10.0))
                    .on_hover_text(crate::i18n::I18nKey::BmpChannelMixer.tr_ui(lang));
                ui.add(
                    DragValue::new(&mut row[0])
                        .range(-2.0..=2.0)
                        .speed(0.05)
                        .prefix("R:"),
                );
                ui.add(
                    DragValue::new(&mut row[1])
                        .range(-2.0..=2.0)
                        .speed(0.05)
                        .prefix("G:"),
                );
                ui.add(
                    DragValue::new(&mut row[2])
                        .range(-2.0..=2.0)
                        .speed(0.05)
                        .prefix("B:"),
                );
            });
        }
    } else {
        ui.add(
            egui::Slider::new(&mut bm.param_a, -1.0..=1.0)
                .text(crate::i18n::I18nKey::DockBmpParamA.tr_ui(lang)),
        );
        ui.add(
            egui::Slider::new(&mut bm.param_b, -1.0..=1.0)
                .text(crate::i18n::I18nKey::DockBmpParamB.tr_ui(lang)),
        );
        ui.add(
            egui::Slider::new(&mut bm.param_c, 0.1..=10.0)
                .text(crate::i18n::I18nKey::DockBmpStrengthRadius.tr_ui(lang)),
        );
    }
    let filter = bm.build_filter();
    if ui
        .button(crate::i18n::I18nKey::BmpAddFilter.tr_ui(lang))
        .clicked()
    {
        app.apply_raster_filter_to_selection(filter);
    }

    // Non-destructive Filter Stack Inspector (Toggle / Reorder / Remove) (`S10` `#51`)
    let active_raster = app.active_tab().and_then(|tab| {
        let oid = tab.selection.objects.iter().copied().find(|id| {
            tab.document
                .object(*id)
                .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
        })?;
        match &tab.document.object(oid)?.payload {
            NodePayload::Raster(r) => Some((oid, r.clone())),
            _ => None,
        }
    });
    if let Some((raster_oid, mut rdata)) = active_raster {
        if !rdata.filters.is_empty() {
            ui.separator();
            ui.label(
                RichText::new(format!(
                    "{} ({})",
                    crate::i18n::I18nKey::BmpFilterStack.tr_ui(lang),
                    rdata.filters.len()
                ))
                .strong()
                .color(app.theme.tokens().accent),
            );
            let mut stack_dirty = false;
            let len = rdata.filters.len();
            if rdata.filter_enabled.len() < len {
                rdata.filter_enabled.resize(len, true);
            }
            let mut remove_idx: Option<usize> = None;
            let mut move_up: Option<usize> = None;
            let mut move_down: Option<usize> = None;
            for i in 0..len {
                let mut enabled = rdata.filter_enabled[i];
                let fname = format!("{:?}", rdata.filters[i]);
                let short_name: String = fname.chars().take(24).collect();
                shell_dir.with_horizontal(ui, |ui| {
                    if ui
                        .checkbox(&mut enabled, "")
                        .on_hover_text(crate::i18n::I18nKey::BmpToggleFilterEnabled.tr_ui(lang))
                        .changed()
                    {
                        rdata.filter_enabled[i] = enabled;
                        stack_dirty = true;
                    }
                    ui.label(RichText::new(format!("{}. {short_name}", i + 1)).size(10.5));
                    if i > 0
                        && ui
                            .small_button("↑")
                            .on_hover_text(crate::i18n::I18nKey::BmpMoveFilterUp.tr_ui(lang))
                            .clicked()
                    {
                        move_up = Some(i);
                    }
                    if i + 1 < len
                        && ui
                            .small_button("↓")
                            .on_hover_text(crate::i18n::I18nKey::BmpMoveFilterDown.tr_ui(lang))
                            .clicked()
                    {
                        move_down = Some(i);
                    }
                    if ui
                        .small_button("✕")
                        .on_hover_text(crate::i18n::I18nKey::BmpRemoveFilter.tr_ui(lang))
                        .clicked()
                    {
                        remove_idx = Some(i);
                    }
                });
            }
            if let Some(idx) = move_up {
                rdata.filters.swap(idx, idx - 1);
                rdata.filter_enabled.swap(idx, idx - 1);
                stack_dirty = true;
            }
            if let Some(idx) = move_down {
                rdata.filters.swap(idx, idx + 1);
                rdata.filter_enabled.swap(idx, idx + 1);
                stack_dirty = true;
            }
            if let Some(idx) = remove_idx {
                rdata.filters.remove(idx);
                rdata.filter_enabled.remove(idx);
                stack_dirty = true;
            }
            if stack_dirty {
                let _ = app.execute_command(CommandOp::UpdateRaster {
                    id: raster_oid,
                    data: rdata,
                });
            }
        }
    }

    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DockBmpResampleHdr.tr_ui(lang)).strong());
    shell_dir.with_horizontal(ui, |ui| {
        ui.add(
            DragValue::new(&mut app.bitmap_state.resample_w)
                .range(16..=8192)
                .prefix("W: "),
        );
        ui.add(
            DragValue::new(&mut app.bitmap_state.resample_h)
                .range(16..=8192)
                .prefix("H: "),
        );
        if ui
            .button(crate::i18n::I18nKey::BmpResampleApply.tr_ui(lang))
            .clicked()
        {
            let (w, h) = (app.bitmap_state.resample_w, app.bitmap_state.resample_h);
            app.resample_selected_bitmap(w, h);
        }
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::DockBmpKernelLabel.tr_ui(lang));
        ComboBox::from_id_salt("bitmap_resample_algo")
            .width(120.0)
            .selected_text(format!("{:?}", app.bitmap_state.resample_algo))
            .show_ui(ui, |ui| {
                for alg in [
                    coree_render::ResampleAlgorithm::Nearest,
                    coree_render::ResampleAlgorithm::Bilinear,
                    coree_render::ResampleAlgorithm::Bicubic,
                    coree_render::ResampleAlgorithm::Lanczos3,
                ] {
                    ui.selectable_value(
                        &mut app.bitmap_state.resample_algo,
                        alg,
                        format!("{alg:?}"),
                    );
                }
            });
    });

    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DockBmpDepthModeHdr.tr_ui(lang)).strong());
    shell_dir.with_horizontal(ui, |ui| {
        if ui
            .button(crate::i18n::I18nKey::BmpDepth16Bit.tr_ui(lang))
            .clicked()
        {
            app.convert_selected_bitmap_depth(coree_core::PixelDepth::Bit16);
        }
        if ui
            .button(crate::i18n::I18nKey::BmpDepth8Bit.tr_ui(lang))
            .clicked()
        {
            app.convert_selected_bitmap_depth(coree_core::PixelDepth::Bit8);
        }
        if ui
            .button(crate::i18n::I18nKey::DockBmpToCmyk.tr_ui(lang))
            .clicked()
        {
            app.convert_selected_bitmap_color_mode(coree_core::ColorMode::Cmyk);
        }
        if ui
            .button(crate::i18n::I18nKey::DockBmpToRgb.tr_ui(lang))
            .clicked()
        {
            app.convert_selected_bitmap_color_mode(coree_core::ColorMode::Rgb);
        }
    });
    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::DockBmpCutoutHdr.tr_ui(lang)).strong());
    shell_dir.with_horizontal(ui, |ui| {
        ui.add(
            DragValue::new(&mut app.bitmap_state.cutout_bg_threshold)
                .range(1.0..=150.0)
                .prefix(crate::i18n::I18nKey::DockBmpBgTolPrefix.tr_ui(lang)),
        );
        ui.add(
            DragValue::new(&mut app.bitmap_state.cutout_feather_px)
                .range(0.0..=24.0)
                .speed(0.1)
                .prefix("Feather: ")
                .suffix("px"),
        );
        if ui
            .button(crate::i18n::I18nKey::DockBmpCutoutBtn.tr_ui(lang))
            .clicked()
        {
            app.run_cutout_lab_active();
        }
    });
    let hist_opt = app.selected_bitmap_histogram(64);
    ui.separator();
    ui.label(RichText::new(crate::i18n::I18nKey::BmpHistogramLive.tr_ui(lang)).strong());
    let (curve_rect, curve_resp) =
        ui.allocate_exact_size(egui::vec2(224.0, 96.0), egui::Sense::click_and_drag());
    let tokens = app.theme.tokens();
    let painter = ui.painter_at(curve_rect);
    painter.rect_filled(curve_rect, CornerRadius::same(3), tokens.bg);
    painter.rect_stroke(
        curve_rect,
        CornerRadius::same(3),
        Stroke::new(1.0, tokens.border),
        egui::StrokeKind::Inside,
    );

    if let Some(ref hist) = hist_opt {
        let peak = hist.luma.iter().copied().max().unwrap_or(1).max(1) as f32;
        let bar_w = curve_rect.width() / (hist.luma.len().max(1) as f32);
        for (i, &count) in hist.luma.iter().enumerate() {
            let frac = (count as f32 / peak).clamp(0.02, 1.0);
            let x0 = curve_rect.left() + i as f32 * bar_w;
            let bar = egui::Rect::from_min_max(
                egui::pos2(x0, curve_rect.bottom() - curve_rect.height() * frac),
                egui::pos2(x0 + bar_w.max(1.0), curve_rect.bottom()),
            );
            painter.rect_filled(bar, 0.0, tokens.accent.gamma_multiply(0.32));
        }
    }

    // Interactive ToneCurve spline nodes (`0..=1`, `0..=1`)
    {
        let pts = &mut app.bitmap_state.tone_curve_points;
        if pts.len() < 2 {
            *pts = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
        }
        if let Some(pos) = curve_resp.interact_pointer_pos() {
            let nx = ((pos.x - curve_rect.left()) / curve_rect.width().max(1.0)).clamp(0.0, 1.0);
            let ny =
                (1.0 - (pos.y - curve_rect.top()) / curve_rect.height().max(1.0)).clamp(0.0, 1.0);
            if curve_resp.drag_started() || curve_resp.clicked() {
                let nearest = pts
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        let da = (a.0 - nx).hypot(a.1 - ny);
                        let db = (b.0 - nx).hypot(b.1 - ny);
                        da.total_cmp(&db)
                    })
                    .filter(|(_, p)| (p.0 - nx).hypot(p.1 - ny) < 0.12)
                    .map(|(i, _)| i);
                if let Some(idx) = nearest {
                    app.bitmap_state.selected_curve_point = Some(idx);
                } else if curve_resp.clicked() && pts.len() < 12 {
                    pts.push((nx, ny));
                    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
                    app.bitmap_state.selected_curve_point = pts
                        .iter()
                        .position(|p| (p.0 - nx).abs() < 1e-4 && (p.1 - ny).abs() < 1e-4);
                }
            } else if curve_resp.dragged() {
                if let Some(idx) = app.bitmap_state.selected_curve_point {
                    let n_pts = pts.len();
                    if let Some(pt) = pts.get_mut(idx) {
                        pt.0 = if idx == 0 {
                            0.0
                        } else if idx + 1 == n_pts {
                            1.0
                        } else {
                            nx.clamp(0.02, 0.98)
                        };
                        pt.1 = ny;
                    }
                    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
                }
            } else if curve_resp.secondary_clicked() && pts.len() > 2 {
                if let Some((idx, _)) = pts.iter().enumerate().min_by(|(_, a), (_, b)| {
                    (a.0 - nx)
                        .hypot(a.1 - ny)
                        .total_cmp(&(b.0 - nx).hypot(b.1 - ny))
                }) {
                    if idx > 0 && idx + 1 < pts.len() {
                        pts.remove(idx);
                        app.bitmap_state.selected_curve_point = None;
                    }
                }
            }
        }
        let screen_pts: Vec<egui::Pos2> = pts
            .iter()
            .map(|(x, y)| {
                egui::pos2(
                    curve_rect.left() + x * curve_rect.width(),
                    curve_rect.bottom() - y * curve_rect.height(),
                )
            })
            .collect();
        painter.add(egui::Shape::line(
            screen_pts.clone(),
            Stroke::new(1.8, tokens.accent),
        ));
        for (i, sp) in screen_pts.iter().enumerate() {
            let sel = app.bitmap_state.selected_curve_point == Some(i);
            painter.circle_filled(
                *sp,
                if sel { 4.2 } else { 3.2 },
                if sel {
                    tokens.accent_secondary
                } else {
                    Color32::WHITE
                },
            );
        }
    }
    shell_dir.with_horizontal(ui, |ui| {
        if ui
            .small_button(crate::i18n::I18nKey::DockBmpApplyToneCurve.tr_ui(lang))
            .clicked()
        {
            let filter = coree_core::RasterFilter::ToneCurve {
                points: app.bitmap_state.tone_curve_points.clone(),
            };
            app.apply_raster_filter_to_selection(filter);
        }
        if ui
            .small_button(crate::i18n::I18nKey::DockBmpResetCurve.tr_ui(lang))
            .clicked()
        {
            app.bitmap_state.tone_curve_points = vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)];
            app.bitmap_state.selected_curve_point = None;
        }
    });
    if let Some(hist) = hist_opt {
        ui.label(
            RichText::new(format!(
                "Histogram ({} px, {} bins)",
                hist.total_pixels, hist.bins
            ))
            .size(10.5)
            .color(tokens.text_muted),
        );
    }
}

// =============================================================================
// 8. Production & Prepress Docker (`S12`, `S13`, `S14`)
// =============================================================================
