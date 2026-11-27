// SPDX-License-Identifier: MIT OR Apache-2.0

//! Central Interactive Canvas Viewport, calibrated 20px Rulers (drag-to-guide),
//! dual-click Transform/Rotate/Skew handles with movable pivot crosshair, live
//! `F10` Bézier Node & handle editor, Interactive Fill gradient handles, live
//! drawing/cutting/brush tools, Page Tabs bar, Document Palette strip, and
//! bottom Status Bar with Live Path Length & Area Meter HUD.

use crate::state::{
    CursorHint, DockerTab, KeyEvent, ModifiersState, OverlayStrokeKind, PointerButton,
    PointerEvent, ScaleHandle, StudioApp, ToolId, ToolKey, TransientOverlay,
};
use crate::theme::{core_color_to_egui, PAGE_TABS_HEIGHT, RULER_THICKNESS, STATUS_BAR_HEIGHT};
use coree_core::{
    BooleanOp, Color, CommandOp, GuideOrientation, NodePayload, Point2D, Rect2D, TransformMode,
    ZOrderOp,
};
use coree_prod::simulate_overprint_rgba;
use coree_tools::{selection_handle_world_positions, ActiveDragState};
use egui::{
    pos2, vec2, Align2, Color32, ColorImage, CornerRadius, FontId, Frame, Margin, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, TextureOptions, Ui,
};

/// Renders the bottom Status Bar (`26px`), bottom Page Tabs & Document Palette Strip,
/// and the central Rulers + Interactive Canvas Viewport.
mod context_text;
mod navigator;
pub use navigator::show_navigator_popup_area;
mod overlays;
mod page_bar;
#[cfg(test)]
mod tests;

pub fn show_canvas_and_status(ui: &mut Ui, app: &mut StudioApp) {
    show_status_bar(ui, app);
    page_bar::show_page_tabs_and_doc_palette(ui, app);
    show_central_viewport(ui, app);
    if app.show_navigator_popup {
        show_navigator_popup_area(ui, app);
    }
}

// =============================================================================
// Bottom Status Bar + Live Path Length & Area Meter HUD
// =============================================================================

pub(super) fn short_color_status_label(color: &Color) -> String {
    match color {
        Color::Spot { name, tint, .. } => format!("{name} {:.0}%", tint * 100.0),
        Color::Cmyk { c, m, y, k, .. } => format!(
            "C{:.0} M{:.0} Y{:.0} K{:.0}",
            c * 100.0,
            m * 100.0,
            y * 100.0,
            k * 100.0
        ),
        _ => color.to_hex(),
    }
}

pub(super) fn tool_status_hint(tool: ToolId, lang: crate::i18n::StudioLanguage) -> String {
    use crate::i18n::I18nKey;
    let key = match tool {
        ToolId::Pick | ToolId::FreehandPick | ToolId::FreeTransform => I18nKey::HintPickDragStamp,
        ToolId::Rectangle
        | ToolId::ThreePointRectangle
        | ToolId::Ellipse
        | ToolId::ThreePointEllipse => I18nKey::HintRectEllipse,
        ToolId::ShapeNode => I18nKey::HintShapeNode,
        ToolId::Zoom => I18nKey::HintZoom,
        ToolId::Pan => I18nKey::HintPan,
        ToolId::Freehand | ToolId::Bezier | ToolId::Pen | ToolId::BSpline | ToolId::Polyline => {
            I18nKey::HintPathDraw
        }
        ToolId::Crop => I18nKey::HintCrop,
        ToolId::ColorEyedropper | ToolId::AttributesEyedropper => I18nKey::HintEyedropper,
        _ => I18nKey::HintDefaultCanvas,
    };
    key.tr_ui(lang)
}

fn show_status_bar(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);

    egui::Panel::bottom("coree_status_bar")
        .exact_size(STATUS_BAR_HEIGHT)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(8, 3))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            let active_tool_label = crate::i18n::tool_i18n_key(app.active_tool).tr_ui(lang);
            let tool_hint = tool_status_hint(app.active_tool, lang);
            app.poll_active_tab_metrics_nonblocking();
            let (
                cursor_txt,
                sel_txt,
                hint_or_dim_txt,
                metrics,
                zoom_pct,
                color_mode,
                rec_macro,
                fill_c,
                stroke_c,
                preflight_errs,
                snap_summary,
                scale_label,
            ) = if let Some(tab) = app.active_tab_mut() {
                let u = tab.document.default_unit();
                let dpi = f64::from(tab.document.dpi());
                let scale = tab.document.drawing_scale();
                let sf = scale.scale_factor();
                let cx = u.from_mm(tab.cursor_world_mm.x * sf, dpi);
                let cy = u.from_mm(tab.cursor_world_mm.y * sf, dpi);
                let c_txt = format!("({cx:.1}, {cy:.1}) {}", u.suffix());
                let s_txt = if tab.selection.is_empty() {
                    format!(
                        "{} · {} {}",
                        active_tool_label,
                        tab.document.objects().len(),
                        crate::i18n::I18nKey::StatusObjectsCount.tr_ui(lang)
                    )
                } else if tab.selection.len() == 1 {
                    let oid = tab.selection.objects[0];
                    let name = tab
                        .document
                        .object(oid)
                        .map(|o| o.name.as_str())
                        .unwrap_or("Object");
                    let mode_lbl = match tab.selection.transform_mode {
                        TransformMode::ScaleAndMove => {
                            crate::i18n::I18nKey::StatusModeMoveScale.tr_ui(lang)
                        }
                        TransformMode::RotateAndSkew => {
                            crate::i18n::I18nKey::StatusModeRotateSkew.tr_ui(lang)
                        }
                    };
                    format!(
                        "{}: {name} [{mode_lbl}]",
                        crate::i18n::I18nKey::StatusSelPrefix.tr_ui(lang)
                    )
                } else {
                    format!(
                        "{}: {} {}",
                        crate::i18n::I18nKey::StatusSelPrefix.tr_ui(lang),
                        tab.selection.len(),
                        crate::i18n::I18nKey::StatusObjectsCount.tr_ui(lang)
                    )
                };
                let dim_txt = if matches!(tab.tool_core.drag_state, ActiveDragState::DraggingPivot)
                {
                    crate::i18n::I18nKey::StatusDraggingPivot.tr_ui(lang)
                } else if tab.powerclip_edit_id.is_some() {
                    crate::i18n::I18nKey::StatusPowerClipEdit.tr_ui(lang)
                } else if let Some(bb) = (!tab.selection.is_empty())
                    .then(|| tab.selection.bounding_box(&tab.document))
                    .flatten()
                {
                    let bw = u.from_mm(bb.width() * sf, dpi);
                    let bh = u.from_mm(bb.height() * sf, dpi);
                    format!("{bw:.1} × {bh:.1} {} · {tool_hint}", u.suffix())
                } else {
                    tool_hint.to_owned()
                };
                let (fc, sc) = tab
                    .selection
                    .objects
                    .first()
                    .and_then(|id| tab.document.object(*id))
                    .map(|o| {
                        let f = o.fill.referenced_colors().into_iter().next();
                        let s = o.outline.is_visible().then(|| o.outline.color.clone());
                        (f, s)
                    })
                    .unwrap_or((None, None));
                let pf_errs = tab
                    .preflight_report
                    .as_ref()
                    .map_or(0, |r| r.error_count() + r.warning_count());
                let so = &tab.snap_options;
                let mut active_snaps = Vec::new();
                if so.objects {
                    active_snaps.push("Obj");
                }
                if so.nodes {
                    active_snaps.push("Node");
                }
                if so.guidelines {
                    active_snaps.push("Guide");
                }
                if so.grid {
                    active_snaps.push("Grid");
                }
                if so.page_edges {
                    active_snaps.push("Page");
                }
                if so.tangent {
                    active_snaps.push("Tan");
                }
                if so.perpendicular {
                    active_snaps.push("Perp");
                }
                if so.quadrant {
                    active_snaps.push("Quad");
                }
                if so.intersection {
                    active_snaps.push("Int");
                }
                if so.baseline {
                    active_snaps.push("Base");
                }
                let snap_txt = if active_snaps.is_empty() {
                    crate::i18n::I18nKey::StatusSnapOff.tr_ui(lang)
                } else if active_snaps.len() <= 3 {
                    format!(
                        "{}: {}",
                        crate::i18n::I18nKey::StatusSnapActive.tr_ui(lang),
                        active_snaps.join("+")
                    )
                } else {
                    format!(
                        "{}: {}+{}",
                        crate::i18n::I18nKey::StatusSnapActive.tr_ui(lang),
                        active_snaps[..2].join("+"),
                        active_snaps.len() - 2
                    )
                };
                let scale_txt = if (sf - 1.0).abs() > 1e-6 {
                    format!(
                        " · {} {}",
                        crate::i18n::I18nKey::StatusScaleLabel.tr_ui(lang),
                        scale.label()
                    )
                } else {
                    String::new()
                };
                (
                    c_txt,
                    s_txt,
                    dim_txt,
                    tab.last_metrics.clone(),
                    tab.viewport.zoom * 100.0,
                    format!(
                        "{}: {:?}",
                        crate::i18n::I18nKey::StatusColorMode.tr_ui(lang),
                        tab.document.color_mode()
                    ),
                    tab.history.recording_macro,
                    fc,
                    sc,
                    pf_errs,
                    snap_txt,
                    scale_txt,
                )
            } else {
                (
                    "(0.0, 0.0) mm".to_owned(),
                    crate::i18n::I18nKey::StatusNoDocument.tr_ui(lang),
                    String::new(),
                    Default::default(),
                    100.0,
                    "Cmyk".to_owned(),
                    false,
                    None,
                    None,
                    0,
                    crate::i18n::I18nKey::StatusSnapOff.tr_ui(lang),
                    String::new(),
                )
            };

            let total_bar_w = ui.available_width();
            shell_dir.with_horizontal(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                // Zone 1: Cursor coordinates, selection dimensions & contextual modifier hint
                ui.label(
                    RichText::new(cursor_txt)
                        .monospace()
                        .size(11.0)
                        .color(tokens.text),
                );
                ui.separator();
                ui.label(RichText::new(sel_txt).size(11.0).color(tokens.text_muted));
                // Show verbose modifier hint only when window is wide enough to avoid collision with right cluster
                if !hint_or_dim_txt.is_empty() && total_bar_w >= 1340.0 {
                    ui.separator();
                    let max_chars = if total_bar_w >= 1650.0 { 56 } else { 32 };
                    let clipped_hint = if hint_or_dim_txt.chars().count() > max_chars {
                        let prefix: String = hint_or_dim_txt.chars().take(max_chars).collect();
                        format!("{prefix}…")
                    } else {
                        hint_or_dim_txt
                    };
                    ui.label(
                        RichText::new(clipped_hint)
                            .size(10.5)
                            .color(tokens.text_muted),
                    );
                }
                ui.separator();

                // Zone 2: Live Path Length & Area Meter HUD
                let cut_m = if metrics.cut_length_m > 0.0 {
                    metrics.cut_length_m
                } else {
                    metrics.total_vector_length_m
                };
                let area_cm2 = metrics.filled_area_mm2 / 100.0;
                let meter_color = if metrics.cut_length_mm > 0.0 {
                    tokens.cutcontour
                } else {
                    tokens.accent
                };
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(format!(
                                "{}: {cut_m:.2} m | {}: {area_cm2:.2} cm² | {} {}",
                                crate::i18n::I18nKey::StatusCutMeter.tr_ui(lang),
                                crate::i18n::I18nKey::StatusAreaLabel.tr_ui(lang),
                                metrics.node_count,
                                crate::i18n::I18nKey::StatusNodesUnit.tr_ui(lang)
                            ))
                            .monospace()
                            .size(11.0)
                            .color(meter_color),
                        )
                        .frame(false),
                    )
                    .on_hover_text(crate::i18n::I18nKey::StatusCutMeterTip.tr_ui(lang))
                    .clicked()
                {
                    app.production_state.sub_section = 2;
                    app.focus_docker(DockerTab::Production);
                }

                // Active Background Job progress indicator
                if let Some((job_name, pct, count)) = app.active_background_progress_summary() {
                    ui.separator();
                    let suffix = if count > 1 {
                        format!(" +{count}")
                    } else {
                        String::new()
                    };
                    ui.label(
                        RichText::new(format!("⏳ {job_name} ({pct}%){suffix}"))
                            .size(11.0)
                            .color(tokens.accent_secondary),
                    );
                }

                // Opposite-aligned zone: navigator, hardware resource badge, swatches, color mode, snap pill, zoom
                ui.with_layout(
                    shell_dir.opposite_horizontal_layout(egui::Align::Center),
                    |ui| {
                        let perf_label = format!(
                            "⚡ {}T",
                            app.resource_budget.worker_threads
                        );
                        let perf_color = if app.resource_budget.under_memory_pressure {
                            tokens.warning
                        } else {
                            tokens.success
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(perf_label)
                                        .monospace()
                                        .size(10.5)
                                        .color(perf_color),
                                )
                                .frame(false),
                            )
                            .on_hover_text(crate::i18n::I18nKey::StatusPerfBadgeTip.tr_ui(lang))
                            .clicked()
                        {
                            app.options_tab = 4;
                            app.show_options_dialog = true;
                        }
                        if ui
                            .selectable_label(
                                app.show_navigator_popup,
                                format!("🧭 {}", crate::i18n::I18nKey::ViewNavigator.tr_ui(lang)),
                            )
                            .on_hover_text(crate::i18n::I18nKey::NavigatorTitle.tr_ui(lang))
                            .clicked()
                        {
                            app.show_navigator_popup = !app.show_navigator_popup;
                        }
                        ui.label(
                            RichText::new(format!("{zoom_pct:.0}%{scale_label}"))
                                .monospace()
                                .size(11.0)
                                .color(tokens.text),
                        );
                        ui.separator();
                        ui.label(
                            RichText::new(snap_summary)
                                .monospace()
                                .size(10.0)
                                .color(tokens.accent),
                        );
                        ui.separator();
                        ui.label(
                            RichText::new(color_mode)
                                .size(11.0)
                                .color(tokens.text_muted),
                        );

                        if rec_macro {
                            ui.label(
                                RichText::new("● REC")
                                    .size(11.0)
                                    .strong()
                                    .color(tokens.accent_secondary),
                            );
                        }

                        if preflight_errs > 0
                            && ui
                                .small_button(
                                    RichText::new(format!(
                                        "⚠ {preflight_errs} {}",
                                        crate::i18n::I18nKey::StatusPreflightLabel.tr_ui(lang)
                                    ))
                                    .size(10.5)
                                    .color(tokens.warning),
                                )
                                .clicked()
                        {
                            app.production_state.sub_section = 0;
                            app.focus_docker(DockerTab::Production);
                        }

                        // Active Fill (F11) & Outline (F12) mini swatches + right-click quick clear
                        let stroke_label = stroke_c.as_ref().map_or_else(
                            || crate::i18n::I18nKey::StatusNoneLabel.tr_ui(lang).to_owned(),
                            short_color_status_label,
                        );
                        ui.label(
                            RichText::new(stroke_label)
                                .monospace()
                                .size(10.0)
                                .color(tokens.text_muted),
                        );
                        let (stroke_rect, stroke_resp) =
                            ui.allocate_exact_size(vec2(15.0, 14.0), Sense::click());
                        ui.painter().rect(
                            stroke_rect,
                            CornerRadius::same(2),
                            Color32::TRANSPARENT,
                            Stroke::new(
                                2.5,
                                stroke_c.as_ref().map_or(tokens.border, core_color_to_egui),
                            ),
                            StrokeKind::Inside,
                        );
                        let stroke_resp = stroke_resp
                            .on_hover_text(crate::i18n::I18nKey::StatusOutlinePenTip.tr_ui(lang));
                        if stroke_resp.clicked() {
                            app.open_outline_pen_dialog();
                        } else if stroke_resp.secondary_clicked() {
                            app.remove_outline();
                        }

                        let fill_label = fill_c.as_ref().map_or_else(
                            || crate::i18n::I18nKey::StatusNoneLabel.tr_ui(lang).to_owned(),
                            short_color_status_label,
                        );
                        ui.label(
                            RichText::new(fill_label)
                                .monospace()
                                .size(10.0)
                                .color(tokens.text_muted),
                        );
                        let (fill_rect, fill_resp) =
                            ui.allocate_exact_size(vec2(15.0, 14.0), Sense::click());
                        ui.painter().rect(
                            fill_rect,
                            CornerRadius::same(2),
                            fill_c
                                .as_ref()
                                .map_or(Color32::TRANSPARENT, core_color_to_egui),
                            Stroke::new(1.0, tokens.border_input),
                            StrokeKind::Inside,
                        );
                        let fill_resp = fill_resp
                            .on_hover_text(crate::i18n::I18nKey::StatusEditFillTip.tr_ui(lang));
                        if fill_resp.clicked() {
                            app.open_edit_fill_dialog();
                        } else if fill_resp.secondary_clicked() {
                            app.remove_fill();
                        }

                        ui.label(
                            RichText::new(&app.status_message)
                                .size(11.0)
                                .color(tokens.text_muted),
                        );
                    },
                );
            });
        });
}

// =============================================================================
// Central Rulers + Interactive Canvas Viewport
// =============================================================================

fn show_central_viewport(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);

    egui::CentralPanel::default()
        .frame(Frame::NONE.fill(tokens.canvas_well))
        .show(ui, |ui| {
            let screen_bounds = ui
                .ctx()
                .input(|i| i.raw.screen_rect)
                .unwrap_or_else(|| ui.ctx().content_rect());
            let total_rect = ui.available_rect_before_wrap().intersect(screen_bounds);
            if total_rect.width() < 60.0 || total_rect.height() < 60.0 {
                return;
            }

            let ruler_t = if app.show_rulers {
                RULER_THICKNESS
            } else {
                0.0
            };
            let corner_rect = Rect::from_min_size(total_rect.min, vec2(ruler_t, ruler_t));
            let top_ruler_rect = Rect::from_min_max(
                pos2(total_rect.left() + ruler_t, total_rect.top()),
                pos2(total_rect.right(), total_rect.top() + ruler_t),
            );
            let left_ruler_rect = Rect::from_min_max(
                pos2(total_rect.left(), total_rect.top() + ruler_t),
                pos2(total_rect.left() + ruler_t, total_rect.bottom()),
            );
            let canvas_rect = Rect::from_min_max(
                pos2(total_rect.left() + ruler_t, total_rect.top() + ruler_t),
                total_rect.max,
            );

            // Allocate interactions for rulers and canvas
            let top_ruler_resp = ui.interact(
                top_ruler_rect,
                ui.scope_id().with("top_ruler"),
                Sense::click_and_drag(),
            );
            let left_ruler_resp = ui.interact(
                left_ruler_rect,
                ui.scope_id().with("left_ruler"),
                Sense::click_and_drag(),
            );
            let canvas_resp = ui.interact(
                canvas_rect,
                ui.scope_id().with("main_canvas"),
                Sense::click_and_drag(),
            );

            // Sync native HiDPI viewport size & render texture via `coree_render::DirtyTileCache`
            // (multiplying by `pixels_per_point()` while clamping to `max_texture_side` so 2x/4K
            // monitors render razor-sharp vector curves without ever panicking texture limits)
            let raw_ppp = ui.ctx().pixels_per_point().clamp(1.0, 4.0);
            let reported_max = ui.ctx().input(|i| i.max_texture_side);
            let max_tex_side = (if reported_max <= 256 {
                4096
            } else {
                reported_max
            })
            .clamp(512, 8192) as f32;
            let max_dim = canvas_rect.width().max(canvas_rect.height()).max(1.0);
            let eff_ppp = raw_ppp.min(max_tex_side / max_dim).max(0.5);
            let canvas_w = (canvas_rect.width() * eff_ppp)
                .round()
                .clamp(32.0, max_tex_side) as u32;
            let canvas_h = (canvas_rect.height() * eff_ppp)
                .round()
                .clamp(32.0, max_tex_side) as u32;
            let target_dpi = coree_core::math::DEFAULT_DPI * f64::from(eff_ppp);

            // Sync soft-proofing, gamut-warning & OS Display ICC options into active tab's RenderOptions
            let soft_proof = app.soft_proof_cmyk;
            let gamut_warn = app.gamut_warning;
            let proof_prof = app.proof_profile;
            let proof_int = app.proof_intent;
            let disp_icc_bytes = if app.use_display_icc {
                app.display_icc_info.as_ref().map(|i| i.bytes.clone())
            } else {
                None
            };
            let disp_icc_int = app.display_icc_intent;

            let max_gpu_tiles = app.resource_budget.max_gpu_tile_textures;
            {
                let Some(tab) = app.active_tab_mut() else {
                    return;
                };
                tab.last_canvas_rect = Some([
                    canvas_rect.left(),
                    canvas_rect.top(),
                    canvas_rect.right(),
                    canvas_rect.bottom(),
                ]);
                if tab.render_options.soft_proof_cmyk != soft_proof
                    || tab.render_options.gamut_warning != gamut_warn
                    || tab.render_options.proof_profile != proof_prof
                    || tab.render_options.proof_intent != proof_int
                    || tab.render_options.display_icc_profile != disp_icc_bytes
                    || tab.render_options.display_icc_intent != disp_icc_int
                {
                    tab.render_options.soft_proof_cmyk = soft_proof;
                    tab.render_options.gamut_warning = gamut_warn;
                    tab.render_options.proof_profile = proof_prof;
                    tab.render_options.proof_intent = proof_int;
                    tab.render_options.display_icc_profile = disp_icc_bytes;
                    tab.render_options.display_icc_intent = disp_icc_int;
                    tab.tile_cache.mark_all_dirty();
                    tab.needs_rerender = true;
                }
                if tab.viewport.width_px != canvas_w
                    || tab.viewport.height_px != canvas_h
                    || (tab.viewport.dpi - target_dpi).abs() > 0.01
                {
                    tab.viewport.width_px = canvas_w;
                    tab.viewport.height_px = canvas_h;
                    tab.viewport.dpi = target_dpi;
                    if !tab.viewport_initialized {
                        tab.fit_active_page();
                        tab.viewport_initialized = true;
                    }
                    tab.needs_rerender = true;
                }
                if !tab.viewport_initialized {
                    tab.fit_active_page();
                    tab.viewport_initialized = true;
                }

                if !tab.overprint_preview {
                    // Direct GPU `256 × 256` World-Tile Texture Compositor (`0` full-screen CPU copy!)
                    page_bar::paint_viewport_tiles_gpu(ui, canvas_rect, tab, max_gpu_tiles);
                } else {
                    // Live Overprint / Separation plate preview path
                    let rendered_tiles = tab.tile_cache.render_dirty_tiles(
                        &tab.document,
                        &tab.viewport,
                        tab.view_mode,
                        &tab.render_options,
                        &mut tab.renderer,
                    );
                    if rendered_tiles > 0
                        || tab.needs_rerender
                        || tab.last_rendered_texture.is_none()
                    {
                        let mut rgba_buf = tab.tile_cache.composite_to_rgba();
                        if let Some(ref sep) = tab.separations_preview {
                            let sim = simulate_overprint_rgba(sep);
                            if sim.len() == (sep.width * sep.height * 4) as usize {
                                rgba_buf.width = sep.width;
                                rgba_buf.height = sep.height;
                                rgba_buf.rgba = sim;
                            }
                        }
                        let img = ColorImage::from_rgba_unmultiplied(
                            [rgba_buf.width as usize, rgba_buf.height as usize],
                            &rgba_buf.rgba,
                        );
                        if let Some(ref mut tex) = tab.last_rendered_texture {
                            tex.set(img, TextureOptions::LINEAR);
                        } else {
                            tab.last_rendered_texture = Some(ui.ctx().load_texture(
                                format!("coree_canvas_{}", tab.id),
                                img,
                                TextureOptions::LINEAR,
                            ));
                        }
                        tab.needs_rerender = false;
                    }
                    if let Some(ref tex) = tab.last_rendered_texture {
                        ui.painter().image(
                            tex.id(),
                            canvas_rect,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                }
            }

            // Full-Screen Preview (`F9`): clean unobstructed artwork view with subtle exit pill
            if app.full_screen_preview {
                let pill_rect = Rect::from_center_size(
                    pos2(canvas_rect.center().x, canvas_rect.top() + 24.0),
                    vec2(320.0, 26.0),
                );
                ui.painter().rect(
                    pill_rect,
                    CornerRadius::same(12),
                    tokens.surface_elevated.gamma_multiply(0.92),
                    Stroke::new(1.0, tokens.accent),
                    StrokeKind::Inside,
                );
                ui.painter().text(
                    pill_rect.center(),
                    Align2::CENTER_CENTER,
                    crate::i18n::I18nKey::CanvasFullScreenBanner.tr_ui(lang),
                    FontId::proportional(11.5),
                    tokens.text,
                );
                if canvas_resp.clicked() {
                    app.full_screen_preview = false;
                }
                return;
            }

            // Floating PowerClip Content-Edit Mode pill at top-center of canvas
            let mut pc_bar_clicked_finish = false;
            let mut pc_bar_occludes_pointer = false;
            if app.active_tab().is_some_and(|t| t.editing_symbol.is_some()) {
                let pill_anchor = pos2(canvas_rect.center().x - 165.0, canvas_rect.top() + 10.0);
                let sym_name = app
                    .active_tab()
                    .and_then(|t| t.editing_symbol.as_ref().map(|s| s.symbol_name.clone()))
                    .unwrap_or_default();
                let area_resp = egui::Area::new(ui.scope_id().with("coree_symbol_edit_bar"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pill_anchor)
                    .show(ui.ctx(), |ui| {
                        Frame::NONE
                            .fill(tokens.surface_elevated.gamma_multiply(0.95))
                            .stroke(Stroke::new(1.2, tokens.accent))
                            .corner_radius(CornerRadius::same(12))
                            .inner_margin(Margin::symmetric(10, 4))
                            .show(ui, |ui| {
                                shell_dir.with_horizontal(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{}: {sym_name}",
                                            crate::i18n::I18nKey::CanvasEditingSymbolPrefix
                                                .tr_ui(lang)
                                        ))
                                        .size(11.5)
                                        .strong()
                                        .color(tokens.accent),
                                    );
                                    if ui
                                        .small_button(
                                            crate::i18n::I18nKey::CanvasFinishEditing.tr_ui(lang),
                                        )
                                        .clicked()
                                    {
                                        app.finish_edit_symbol(true);
                                        pc_bar_clicked_finish = true;
                                    }
                                    if ui
                                        .small_button(
                                            crate::i18n::I18nKey::DialogCancel.tr_ui(lang),
                                        )
                                        .clicked()
                                    {
                                        app.finish_edit_symbol(false);
                                        pc_bar_clicked_finish = true;
                                    }
                                });
                            });
                    });
                let ptr_pos =
                    ui.input(|i| i.pointer.interact_pos().or_else(|| i.pointer.hover_pos()));
                pc_bar_occludes_pointer = pc_bar_occludes_pointer
                    || area_resp.response.hovered()
                    || area_resp.response.contains_pointer()
                    || area_resp.response.container_contains_pointer()
                    || ptr_pos.is_some_and(|p| area_resp.response.rect.contains(p));
            }
            if app
                .active_tab()
                .is_some_and(|t| t.powerclip_edit_id.is_some())
            {
                let pill_anchor = pos2(canvas_rect.center().x - 145.0, canvas_rect.top() + 10.0);
                let area_resp = egui::Area::new(ui.scope_id().with("coree_powerclip_edit_bar"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pill_anchor)
                    .show(ui.ctx(), |ui| {
                        Frame::NONE
                            .fill(tokens.surface_elevated.gamma_multiply(0.95))
                            .stroke(Stroke::new(1.2, tokens.accent))
                            .corner_radius(CornerRadius::same(12))
                            .inner_margin(Margin::symmetric(10, 4))
                            .show(ui, |ui| {
                                shell_dir.with_horizontal(ui, |ui| {
                                    ui.label(
                                        RichText::new(
                                            crate::i18n::I18nKey::CanvasEditingPowerClip
                                                .tr_ui(lang),
                                        )
                                        .size(11.5)
                                        .strong()
                                        .color(tokens.accent),
                                    );
                                    if ui
                                        .small_button(
                                            crate::i18n::I18nKey::CanvasFinishEsc.tr_ui(lang),
                                        )
                                        .clicked()
                                    {
                                        app.exit_powerclip_edit();
                                        pc_bar_clicked_finish = true;
                                    }
                                });
                            });
                    });
                let ptr_pos =
                    ui.input(|i| i.pointer.interact_pos().or_else(|| i.pointer.hover_pos()));
                pc_bar_occludes_pointer = area_resp.response.hovered()
                    || area_resp.response.contains_pointer()
                    || area_resp.response.container_contains_pointer()
                    || ptr_pos.is_some_and(|p| area_resp.response.rect.contains(p));
            }

            // Handle Ruler drag -> Create live Horizontal / Vertical Guidelines!
            handle_ruler_guide_drag(app, canvas_rect, &top_ruler_resp, &left_ruler_resp);

            // Handle Canvas pointer interactions (pan, cursor-centered wheel zoom, all 62 tools!)
            // Keep running when a canvas drag is already active so drags that start on open
            // canvas and release over the PowerClip bar still complete cleanly.
            let tool_dragging = app.active_tab().is_some_and(|t| t.tool_core.is_dragging());
            let ruler_dragging = top_ruler_resp.drag_started()
                || top_ruler_resp.dragged()
                || left_ruler_resp.drag_started()
                || left_ruler_resp.dragged();
            if !ruler_dragging
                && (tool_dragging || (!pc_bar_clicked_finish && !pc_bar_occludes_pointer))
            {
                handle_canvas_interaction(ui, app, canvas_rect, &canvas_resp);
            }

            // Right-Click Canvas Context Menu (disabled during Zoom right-click & mid-drag stamp duplicate)
            let allow_context_menu = app.active_tool != ToolId::Zoom
                && app.active_tab().is_some_and(|t| !t.tool_core.is_dragging());
            if allow_context_menu {
                context_text::show_canvas_context_menu(&canvas_resp, app);
            }

            if canvas_resp.hovered() {
                if let Some(hint) = app.active_tab().map(|t| t.transient_overlay.cursor_hint) {
                    let icon = match hint {
                        CursorHint::Default => egui::CursorIcon::Default,
                        CursorHint::Crosshair
                        | CursorHint::Eyedropper
                        | CursorHint::Knife
                        | CursorHint::Eraser => egui::CursorIcon::Crosshair,
                        CursorHint::Move | CursorHint::NodeEdit => egui::CursorIcon::Move,
                        CursorHint::Grab => egui::CursorIcon::Grab,
                        CursorHint::Grabbing => egui::CursorIcon::Grabbing,
                        CursorHint::Text => egui::CursorIcon::Text,
                        CursorHint::ZoomIn => egui::CursorIcon::ZoomIn,
                        CursorHint::ZoomOut => egui::CursorIcon::ZoomOut,
                    };
                    ui.ctx().set_cursor_icon(icon);
                }
            }

            // Paint interactive overlays on top of canvas (laser preview, handles, nodes, effects, crop, cursors)
            overlays::paint_canvas_overlays(ui, app, canvas_rect);

            // Live floating on-canvas TextEdit for ArtisticText / ParagraphText direct typing!
            context_text::show_inline_text_editor_overlay(ui, app, canvas_rect);

            // Paint Top & Left calibrated 20px Rulers + Corner box
            if app.show_rulers {
                overlays::paint_rulers(
                    ui,
                    app,
                    corner_rect,
                    top_ruler_rect,
                    left_ruler_rect,
                    canvas_rect,
                );
            }

            // Non-blocking floating background tasks progress & toasts at top-right of canvas
            page_bar::show_background_jobs_hud(ui, app, canvas_rect);
            overlays::paint_toasts(ui, app, canvas_rect);
        });
}

pub(super) fn screen_to_viewport_local(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    screen: Pos2,
) -> Point2D {
    let sx = f64::from(tab.viewport.width_px) / f64::from(canvas_rect.width().max(1.0));
    let sy = f64::from(tab.viewport.height_px) / f64::from(canvas_rect.height().max(1.0));
    Point2D::new(
        f64::from(screen.x - canvas_rect.left()) * sx,
        f64::from(screen.y - canvas_rect.top()) * sy,
    )
}

pub(super) fn screen_to_world(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    screen: Pos2,
) -> Point2D {
    let local = screen_to_viewport_local(canvas_rect, tab, screen);
    tab.viewport.screen_to_world(local)
}

pub(super) fn world_to_screen(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    world: Point2D,
) -> Pos2 {
    let local = tab.viewport.world_to_screen(world);
    let sx = f64::from(tab.viewport.width_px) / f64::from(canvas_rect.width().max(1.0));
    let sy = f64::from(tab.viewport.height_px) / f64::from(canvas_rect.height().max(1.0));
    pos2(
        canvas_rect.left() + (local.x / sx) as f32,
        canvas_rect.top() + (local.y / sy) as f32,
    )
}

pub(super) fn selection_handle_positions(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    bb: Rect2D,
) -> [(ScaleHandle, Pos2); 8] {
    selection_handle_world_positions(bb).map(|(h, w)| (h, world_to_screen(canvas_rect, tab, w)))
}

pub(super) fn make_pointer_event(
    tab: &crate::state::DocumentTab,
    canvas_rect: Rect,
    pos: Pos2,
    button: PointerButton,
    modifiers: ModifiersState,
    click_count: u8,
    stylus: coree_core::StylusSample,
) -> PointerEvent {
    let world_pt_mm = screen_to_world(canvas_rect, tab, pos);
    let local = screen_to_viewport_local(canvas_rect, tab, pos);
    PointerEvent {
        world_pt_mm,
        screen_pt_px: [local.x as f32, local.y as f32],
        button,
        modifiers,
        click_count,
        stylus,
    }
}

fn handle_ruler_guide_drag(
    app: &mut StudioApp,
    canvas_rect: Rect,
    top_resp: &egui::Response,
    left_resp: &egui::Response,
) {
    if top_resp.drag_started() || top_resp.dragged() {
        if let Some(pos) = top_resp.interact_pointer_pos() {
            if let Some(tab) = app.active_tab_mut() {
                let w = screen_to_world(canvas_rect, tab, pos);
                if top_resp.drag_started() {
                    tab.tool_core.begin_ruler_guide_drag(true, w.y);
                } else {
                    tab.tool_core.update_ruler_guide_drag(true, w.y);
                }
                let line = coree_core::BezierPath::from_line(
                    Point2D::new(tab.viewport.pan_mm.x - 5000.0, w.y),
                    Point2D::new(tab.viewport.pan_mm.x + 5000.0, w.y),
                );
                tab.transient_overlay = TransientOverlay {
                    preview_paths: vec![coree_tools::OverlayPath::stroke(
                        line,
                        OverlayStrokeKind::Guide,
                    )],
                    ..TransientOverlay::default()
                };
            }
        }
    } else if top_resp.drag_stopped() {
        let maybe_op = app.active_tab_mut().and_then(|t| {
            t.transient_overlay = TransientOverlay::default();
            t.tool_core.finish_ruler_guide_drag()
        });
        if let Some(CommandOp::AddGuide { orientation, .. }) = maybe_op {
            let y = match orientation {
                GuideOrientation::Horizontal(v) => v,
                _ => 0.0,
            };
            app.add_guide(orientation);
            app.push_toast(format!("Added Horizontal Guide at Y={y:.1} mm"), false);
        }
    }

    if left_resp.drag_started() || left_resp.dragged() {
        if let Some(pos) = left_resp.interact_pointer_pos() {
            if let Some(tab) = app.active_tab_mut() {
                let w = screen_to_world(canvas_rect, tab, pos);
                if left_resp.drag_started() {
                    tab.tool_core.begin_ruler_guide_drag(false, w.x);
                } else {
                    tab.tool_core.update_ruler_guide_drag(false, w.x);
                }
                let line = coree_core::BezierPath::from_line(
                    Point2D::new(w.x, tab.viewport.pan_mm.y - 5000.0),
                    Point2D::new(w.x, tab.viewport.pan_mm.y + 5000.0),
                );
                tab.transient_overlay = TransientOverlay {
                    preview_paths: vec![coree_tools::OverlayPath::stroke(
                        line,
                        OverlayStrokeKind::Guide,
                    )],
                    ..TransientOverlay::default()
                };
            }
        }
    } else if left_resp.drag_stopped() {
        let maybe_op = app.active_tab_mut().and_then(|t| {
            t.transient_overlay = TransientOverlay::default();
            t.tool_core.finish_ruler_guide_drag()
        });
        if let Some(CommandOp::AddGuide { orientation, .. }) = maybe_op {
            let x = match orientation {
                GuideOrientation::Vertical(v) => v,
                _ => 0.0,
            };
            app.add_guide(orientation);
            app.push_toast(format!("Added Vertical Guide at X={x:.1} mm"), false);
        }
    }
}

fn handle_canvas_interaction(
    ui: &egui::Ui,
    app: &mut StudioApp,
    canvas_rect: Rect,
    resp: &egui::Response,
) {
    let stylus_sample = app.stylus_provider.sample_for_frame(ui.ctx());
    app.last_stylus_sample = stylus_sample;

    // 1. Live cursor tracking & proportional, smooth, bounded cursor-centered wheel/pinch zoom
    if let Some(hover) = resp.hover_pos() {
        if let Some(tab) = app.active_tab_mut() {
            tab.cursor_world_mm = screen_to_world(canvas_rect, tab, hover);
        }
        let (scroll_y, pinch_zoom) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let scroll_factor = if scroll_y.abs() > 0.05 {
            (f64::from(scroll_y) * 0.0022).exp().clamp(0.88, 1.14)
        } else {
            1.0
        };
        let pinch_factor = if (pinch_zoom - 1.0).abs() > 1e-4 {
            f64::from(pinch_zoom).powf(0.55).clamp(0.88, 1.14)
        } else {
            1.0
        };
        let factor = if (pinch_zoom - 1.0).abs() > 1e-4 {
            pinch_factor
        } else {
            scroll_factor
        }
        .clamp(0.88, 1.14);
        if (factor - 1.0).abs() > 1e-6 {
            if let Some(tab) = app.active_tab_mut() {
                let local = screen_to_viewport_local(canvas_rect, tab, hover);
                tab.viewport.zoom_at_screen_point(local, factor);
                tab.needs_rerender = true;
            }
        }
        // Accept DragSwatch color drop onto hovered canvas object (`ADD-09`)
        if ui.input(|i| i.pointer.any_released()) {
            if let Some(dropped_color) = egui::DragAndDrop::take_payload::<Color>(ui.ctx()) {
                let shift_held = ui.input(|i| i.modifiers.shift);
                let hit_id = app.active_tab().and_then(|tab| {
                    let world_pt = screen_to_world(canvas_rect, tab, hover);
                    let tol = (4.0 / tab.viewport.logical_px_per_mm().max(0.05)).max(1.5);
                    coree_render::hit_test_point(
                        &tab.document,
                        tab.document.active_page_index(),
                        world_pt,
                        tol,
                    )
                });
                if let Some(oid) = hit_id {
                    let before_sel = app
                        .active_tab()
                        .map(|t| t.selection.objects.clone())
                        .unwrap_or_default();
                    if let Some(tab) = app.active_tab_mut() {
                        tab.selection.select_single(&tab.document, oid);
                    }
                    app.maybe_fire_selection_change(&before_sel);
                    if shift_held {
                        app.apply_outline_color((*dropped_color).clone());
                    } else {
                        app.apply_fill_color((*dropped_color).clone());
                    }
                } else {
                    if shift_held {
                        app.apply_outline_color((*dropped_color).clone());
                    } else {
                        app.apply_fill_color((*dropped_color).clone());
                    }
                }
            }
        }
    }

    // 1b. Confirm pending Crop or open Bezier/Pen/BSpline/Polyline path on Enter
    let text_focus = ui.ctx().egui_wants_keyboard_input();
    if !text_focus && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.any()) {
        app.confirm_active_tool_action();
    }

    // 2. Middle-mouse pan anywhere
    let middle_down = ui.input(|i| i.pointer.middle_down());
    if middle_down && resp.hovered() {
        let delta = ui.input(|i| i.pointer.delta());
        if delta.length_sq() > 0.0 {
            if let Some(tab) = app.active_tab_mut() {
                let s = tab.viewport.logical_px_per_mm().max(1e-4);
                tab.viewport.pan_mm.x -= f64::from(delta.x) / s;
                tab.viewport.pan_mm.y -= f64::from(delta.y) / s;
                tab.viewport.lod_dragging = true;
                tab.needs_rerender = true;
            }
        }
        return;
    } else if let Some(tab) = app.active_tab_mut() {
        if tab.viewport.lod_dragging && !tab.tool_core.is_dragging() {
            tab.viewport.lod_dragging = false;
            tab.needs_rerender = true;
        }
    }

    let (
        modifiers,
        primary_pressed,
        primary_down,
        primary_released,
        secondary_pressed,
        pointer_pos,
        pointer_delta,
        space_pressed,
    ) = ui.input(|i| {
        let modifiers = ModifiersState {
            shift: i.modifiers.shift,
            ctrl: i.modifiers.ctrl || i.modifiers.command,
            alt: i.modifiers.alt,
        };
        (
            modifiers,
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.primary_released(),
            i.pointer.secondary_pressed(),
            i.pointer.interact_pos(),
            i.pointer.delta(),
            !text_focus && i.key_pressed(egui::Key::Space),
        )
    });

    let has_inline_edit = app
        .active_tab()
        .is_some_and(|t| t.inline_text_edit.is_some());

    // 3. Double-click handling
    if resp.double_clicked() && resp.hovered() {
        if has_inline_edit {
            app.commit_inline_text_edit();
        }
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(tab) = app.active_tab_mut() {
                tab.cancel_active_drag();
            }
            if let Some(ev) = app.active_tab().map(|tab| {
                make_pointer_event(
                    tab,
                    canvas_rect,
                    pos,
                    PointerButton::Primary,
                    modifiers,
                    2,
                    stylus_sample,
                )
            }) {
                app.run_tool_pointer_down(&ev);
                return;
            }
        }
    }

    // 4. Right-click in Zoom tool zooms out immediately
    if resp.secondary_clicked() && resp.hovered() && app.active_tool == ToolId::Zoom {
        if has_inline_edit {
            app.commit_inline_text_edit();
        }
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(ev) = app.active_tab().map(|tab| {
                make_pointer_event(
                    tab,
                    canvas_rect,
                    pos,
                    PointerButton::Secondary,
                    modifiers,
                    1,
                    stylus_sample,
                )
            }) {
                app.run_tool_pointer_down(&ev);
            }
        }
        return;
    }

    // 5. Primary press / move / release routed directly to ToolCore
    // Gate press unconditionally on `resp.hovered()` so no foreground `egui::Area`, popup,
    // context menu, modal `egui::Window`, inline text editor, or PowerClip bar over
    // `canvas_rect` can ever leak a press onto `ToolCore`.
    if primary_pressed && resp.hovered() {
        if has_inline_edit {
            app.commit_inline_text_edit();
        }
        if let Some(pos) = resp.interact_pointer_pos().or(pointer_pos) {
            if let Some(ev) = app.active_tab().map(|tab| {
                make_pointer_event(
                    tab,
                    canvas_rect,
                    pos,
                    PointerButton::Primary,
                    modifiers,
                    1,
                    stylus_sample,
                )
            }) {
                app.run_tool_pointer_down(&ev);
            }
        }
    }

    let is_dragging = app.active_tab().is_some_and(|t| t.tool_core.is_dragging());
    if is_dragging {
        if secondary_pressed || space_pressed {
            if app
                .active_tab()
                .is_some_and(|t| t.tool_core.active_live_world_transform().is_some())
            {
                let _ = app.stamp_duplicate_during_active_drag();
            } else {
                app.run_tool_key_event(&KeyEvent {
                    key: ToolKey::Space,
                    modifiers,
                });
            }
        }
        if (primary_down && !primary_pressed)
            || (primary_released && pointer_delta.length_sq() > 0.0)
        {
            if let Some(pos) = resp.interact_pointer_pos().or(pointer_pos) {
                if let Some(ev) = app.active_tab().map(|tab| {
                    make_pointer_event(
                        tab,
                        canvas_rect,
                        pos,
                        PointerButton::Primary,
                        modifiers,
                        1,
                        stylus_sample,
                    )
                }) {
                    app.run_tool_pointer_move(&ev);
                }
            }
        }
        if primary_released {
            let pos = resp
                .interact_pointer_pos()
                .or(pointer_pos)
                .unwrap_or_else(|| canvas_rect.center());
            if let Some(ev) = app.active_tab().map(|tab| {
                make_pointer_event(
                    tab,
                    canvas_rect,
                    pos,
                    PointerButton::Primary,
                    modifiers,
                    1,
                    stylus_sample,
                )
            }) {
                app.run_tool_pointer_up(&ev);
            }
        }
    } else if primary_released && resp.clicked() && !has_inline_edit {
        if let Some(pos) = resp.interact_pointer_pos().or(pointer_pos) {
            if let Some(ev) = app.active_tab().map(|tab| {
                make_pointer_event(
                    tab,
                    canvas_rect,
                    pos,
                    PointerButton::Primary,
                    modifiers,
                    1,
                    stylus_sample,
                )
            }) {
                app.run_tool_pointer_up(&ev);
            }
        }
    } else if let Some(hover) = resp.hover_pos() {
        if !has_inline_edit
            && pointer_delta.length_sq() > 0.0
            && app
                .active_tab()
                .is_some_and(|t| t.tool_core.has_active_draft_or_drag())
        {
            if let Some(ev) = app.active_tab().map(|tab| {
                make_pointer_event(
                    tab,
                    canvas_rect,
                    hover,
                    PointerButton::Primary,
                    modifiers,
                    1,
                    stylus_sample,
                )
            }) {
                app.run_tool_pointer_move(&ev);
            }
        }
    }
}

// =============================================================================
// Right-Click Canvas Context Menu & Direct On-Canvas Inline Text Editor
// =============================================================================
