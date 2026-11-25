// SPDX-License-Identifier: MIT OR Apache-2.0

//! Central Interactive Canvas Viewport, calibrated 20px Rulers (drag-to-guide),
//! dual-click Transform/Rotate/Skew handles with movable pivot crosshair, live
//! `F10` Bézier Node & handle editor, Interactive Fill gradient handles, live
//! drawing/cutting/brush tools, Page Tabs bar, Document Palette strip, and
//! bottom Status Bar with Live Path Length & Area Meter HUD.

use crate::state::{
    world_transform_of_or_local, CursorHint, DockerTab, HandleShape, KeyEvent, ModifiersState,
    OverlayStrokeKind, PointerButton, PointerEvent, ScaleHandle, StudioApp, ToolId, ToolKey,
    TransientOverlay,
};
use crate::theme::{core_color_to_egui, PAGE_TABS_HEIGHT, RULER_THICKNESS, STATUS_BAR_HEIGHT};
use coree_core::{
    Affine2D, BooleanOp, Color, CommandOp, Effect, Fill, GradientStop, GuideOrientation, NodeKind,
    NodePayload, NodeRef, Point2D, RasterMaskShape, Rect2D, TransformMode, TransparencyMode,
    ZOrderOp,
};
use coree_prod::{simulate_overprint_rgba, LaserOperationKind};
use coree_tools::{
    collect_editable_fillable_targets, find_nearest_object_anchor_in_doc,
    interactive_effect_handle_points, parametric_shape_handles, selection_handle_world_positions,
    ActiveDragState,
};
#[cfg(test)]
use coree_tools::{gradient_ray_local, gradient_stops_mut, interpolate_gradient_stop_color};
use egui::{
    pos2, vec2, Align2, Color32, ColorImage, CornerRadius, FontId, Frame, Margin, Pos2, Rect,
    RichText, Sense, Shape, Stroke, StrokeKind, TextureOptions, Ui,
};

/// Renders the bottom Status Bar (`26px`), bottom Page Tabs & Document Palette Strip,
/// and the central Rulers + Interactive Canvas Viewport.
pub fn show_canvas_and_status(ui: &mut Ui, app: &mut StudioApp) {
    show_status_bar(ui, app);
    show_page_tabs_and_doc_palette(ui, app);
    show_central_viewport(ui, app);
}

// =============================================================================
// Bottom Status Bar + Live Path Length & Area Meter HUD
// =============================================================================

fn short_color_status_label(color: &Color) -> String {
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

fn tool_status_hint(tool: ToolId) -> &'static str {
    match tool {
        ToolId::Pick | ToolId::FreehandPick | ToolId::FreeTransform => {
            "Drag+Space/RightClick: Stamp copy · Ctrl+Click: PowerClip contents"
        }
        ToolId::Rectangle
        | ToolId::ThreePointRectangle
        | ToolId::Ellipse
        | ToolId::ThreePointEllipse => "Ctrl: Square/Circle · Shift: From center",
        ToolId::ShapeNode => "DblClick segment: Add/Del node · Drag handles",
        ToolId::Zoom => "Click: Zoom in · Shift+Click: Zoom out · Drag: Marquee zoom",
        ToolId::Pan => "Drag canvas or Middle-mouse drag anywhere",
        ToolId::Freehand | ToolId::Bezier | ToolId::Pen | ToolId::BSpline | ToolId::Polyline => {
            "Enter/DblClick: Finish open path · Click start: Close"
        }
        ToolId::Crop => "Drag box · Enter/DblClick: Crop to selection",
        ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
            "Click object: Sample · Alt/Toggle: Apply sampled style"
        }
        _ => "Drag on canvas to create · Right-click for context menu",
    }
}

fn show_status_bar(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();

    egui::Panel::bottom("coree_status_bar")
        .exact_size(STATUS_BAR_HEIGHT)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(8, 3))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            let active_tool_label = app.active_tool.label();
            let tool_hint = tool_status_hint(app.active_tool);
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
            ) = if let Some(tab) = app.active_tab_mut() {
                tab.ensure_metrics_fresh();
                let u = tab.document.default_unit();
                let dpi = f64::from(tab.document.dpi());
                let cx = u.from_mm(tab.cursor_world_mm.x, dpi);
                let cy = u.from_mm(tab.cursor_world_mm.y, dpi);
                let c_txt = format!("({cx:.1}, {cy:.1}) {}", u.suffix());
                let s_txt = if tab.selection.is_empty() {
                    format!(
                        "{} · {} obj(s)",
                        active_tool_label,
                        tab.document.objects().len()
                    )
                } else if tab.selection.len() == 1 {
                    let oid = tab.selection.objects[0];
                    let name = tab
                        .document
                        .object(oid)
                        .map(|o| o.name.as_str())
                        .unwrap_or("Object");
                    let mode_lbl = match tab.selection.transform_mode {
                        TransformMode::ScaleAndMove => "Move/Scale",
                        TransformMode::RotateAndSkew => "Rotate/Skew",
                    };
                    format!("Sel: {name} [{mode_lbl}]")
                } else {
                    format!("Sel: {} objects", tab.selection.len())
                };
                let dim_txt = if tab.powerclip_edit_id.is_some() {
                    "PowerClip Edit · Ctrl+Click outside or Esc to finish".to_owned()
                } else if let Some(bb) = (!tab.selection.is_empty())
                    .then(|| tab.selection.bounding_box(&tab.document))
                    .flatten()
                {
                    let bw = u.from_mm(bb.width(), dpi);
                    let bh = u.from_mm(bb.height(), dpi);
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
                (
                    c_txt,
                    s_txt,
                    dim_txt,
                    tab.last_metrics.clone(),
                    tab.viewport.zoom * 100.0,
                    format!("{:?}", tab.document.color_mode()),
                    tab.history.recording_macro,
                    fc,
                    sc,
                    pf_errs,
                )
            } else {
                (
                    "(0.0, 0.0) mm".to_owned(),
                    "No document".to_owned(),
                    String::new(),
                    Default::default(),
                    100.0,
                    "Cmyk".to_owned(),
                    false,
                    None,
                    None,
                    0,
                )
            };

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // Zone 1: Cursor coordinates, selection dimensions & contextual modifier hint
                ui.label(
                    RichText::new(cursor_txt)
                        .monospace()
                        .size(11.0)
                        .color(tokens.text),
                );
                ui.separator();
                ui.label(RichText::new(sel_txt).size(11.0).color(tokens.text_muted));
                if !hint_or_dim_txt.is_empty() {
                    ui.separator();
                    ui.label(
                        RichText::new(hint_or_dim_txt)
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
                                "Cut: {cut_m:.2} m | Area: {area_cm2:.2} cm² | {} nodes",
                                metrics.node_count
                            ))
                            .monospace()
                            .size(11.0)
                            .color(meter_color),
                        )
                        .frame(false),
                    )
                    .on_hover_text("Click to open Production & CutContour Metrics")
                    .clicked()
                {
                    app.production_state.sub_section = 2;
                    app.focus_docker(DockerTab::Production);
                }

                // Active Background Job progress indicator
                if let Some(job) = app.active_jobs.first() {
                    ui.separator();
                    let pct = job.handle.progress();
                    ui.label(
                        RichText::new(format!("⏳ {} ({pct}%)", job.handle.name()))
                            .size(11.0)
                            .color(tokens.accent_secondary),
                    );
                }

                // Right-aligned zone: swatches, color mode, preflight badge, zoom, status
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{zoom_pct:.0}%"))
                            .monospace()
                            .size(11.0)
                            .color(tokens.text),
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
                                RichText::new(format!("⚠ {preflight_errs} Preflight"))
                                    .size(10.5)
                                    .color(tokens.warning),
                            )
                            .clicked()
                    {
                        app.production_state.sub_section = 0;
                        app.focus_docker(DockerTab::Production);
                    }

                    // Active Fill (F11) & Outline (F12) mini swatches + right-click quick clear
                    let stroke_label = stroke_c
                        .as_ref()
                        .map_or_else(|| "None".to_owned(), short_color_status_label);
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
                        .on_hover_text("Outline Pen (F12) · Right-click: Remove Outline");
                    if stroke_resp.clicked() {
                        app.open_outline_pen_dialog();
                    } else if stroke_resp.secondary_clicked() {
                        app.remove_outline();
                    }

                    let fill_label = fill_c
                        .as_ref()
                        .map_or_else(|| "None".to_owned(), short_color_status_label);
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
                    let fill_resp =
                        fill_resp.on_hover_text("Edit Fill (F11) · Right-click: Remove Fill");
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
                });
            });
        });
}

// =============================================================================
// Bottom Page Tabs Bar & Document Palette Strip
// =============================================================================

fn show_page_tabs_and_doc_palette(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();

    egui::Panel::bottom("coree_page_tabs")
        .exact_size(PAGE_TABS_HEIGHT)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(6, 2))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            let mut add_page_req = false;
            let mut delete_page_req = None;
            let mut duplicate_page_req = None;
            let mut rename_page_req: Option<(coree_core::PageId, String)> = None;
            let mut multipage_toggle_req: Option<bool> = None;
            let mut apply_doc_fill: Option<Color> = None;
            let mut apply_doc_stroke: Option<Color> = None;

            ui.horizontal(|ui| {
                if let Some(tab) = app.active_tab_mut() {
                    let active_idx = tab.document.active_page_index();
                    let page_count = tab.document.pages().len();

                    for idx in 0..page_count {
                        let page_name = tab.document.pages()[idx].name.clone();
                        let page_id = tab.document.pages()[idx].id;
                        let is_active = idx == active_idx;

                        if tab.inline_renaming_page == Some(idx) {
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut tab.inline_page_name_buf)
                                    .desired_width(88.0),
                            );
                            if resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                let new_name = tab.inline_page_name_buf.trim().to_owned();
                                if !new_name.is_empty() {
                                    rename_page_req = Some((page_id, new_name));
                                }
                                tab.inline_renaming_page = None;
                            }
                            continue;
                        }

                        let btn = ui.add(
                            egui::Button::new(RichText::new(&page_name).size(11.5).color(
                                if is_active {
                                    tokens.text
                                } else {
                                    tokens.text_muted
                                },
                            ))
                            .fill(if is_active {
                                tokens.surface_elevated
                            } else {
                                tokens.surface
                            })
                            .stroke(if is_active {
                                Stroke::new(1.0, tokens.accent)
                            } else {
                                Stroke::new(1.0, tokens.border)
                            }),
                        );
                        if btn.clicked() {
                            let _ = tab.document.set_active_page_index(idx);
                            tab.selection.clear();
                            tab.sync_active_layer();
                            tab.needs_rerender = true;
                        }
                        if btn.double_clicked() {
                            tab.inline_renaming_page = Some(idx);
                            tab.inline_page_name_buf = page_name.clone();
                        }
                        btn.context_menu(|ui| {
                            if ui.button("Rename Page").clicked() {
                                tab.inline_renaming_page = Some(idx);
                                tab.inline_page_name_buf = page_name.clone();
                                ui.close();
                            }
                            if ui.button("Duplicate Page").clicked() {
                                duplicate_page_req = Some(idx);
                                ui.close();
                            }
                            if ui
                                .add_enabled(page_count > 1, egui::Button::new("Delete Page"))
                                .clicked()
                            {
                                delete_page_req = Some(page_id);
                                ui.close();
                            }
                        });
                    }

                    if ui
                        .small_button(RichText::new("+").color(tokens.accent))
                        .on_hover_text("Add Page")
                        .clicked()
                    {
                        add_page_req = true;
                    }

                    ui.separator();
                    if ui
                        .selectable_label(tab.document.multipage_view(), "⊞ Multipage")
                        .clicked()
                    {
                        let next = !tab.document.multipage_view();
                        multipage_toggle_req = Some(next);
                        tab.viewport.multipage_view = next;
                        tab.needs_rerender = true;
                    }

                    // Document Palette Strip
                    ui.separator();
                    ui.label(
                        RichText::new("Doc Colors:")
                            .size(10.5)
                            .color(tokens.text_muted),
                    );
                    for sw in tab.document.document_palette().swatches.iter().take(16) {
                        let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
                        ui.painter().rect(
                            r,
                            CornerRadius::same(2),
                            core_color_to_egui(&sw.color),
                            Stroke::new(0.8, tokens.border_input),
                            StrokeKind::Inside,
                        );
                        let resp = resp.on_hover_text(format!("{} (L:Fill / R:Outline)", sw.name));
                        if resp.clicked() {
                            apply_doc_fill = Some(sw.color.clone());
                        } else if resp.secondary_clicked() {
                            apply_doc_stroke = Some(sw.color.clone());
                        }
                    }
                }
            });

            if add_page_req {
                let (w, h, b, n) = app
                    .active_tab()
                    .and_then(|t| {
                        t.document.active_page().map(|p| {
                            (
                                p.width_mm,
                                p.height_mm,
                                p.bleed_mm,
                                t.document.pages().len() + 1,
                            )
                        })
                    })
                    .unwrap_or((210.0, 297.0, 3.0, 2));
                let _ = app.execute_command(CommandOp::AddPage {
                    name: format!("Page {n}"),
                    width_mm: w,
                    height_mm: h,
                    bleed_mm: b,
                });
            }
            if let Some(pid) = delete_page_req {
                let _ = app.execute_command(CommandOp::DeletePage { page_id: pid });
            }
            if let Some((page_id, new_name)) = rename_page_req {
                let _ = app.execute_command(CommandOp::RenamePage { page_id, new_name });
            }
            if let Some(next) = multipage_toggle_req {
                let _ = app.execute_command(CommandOp::SetDocumentProperties {
                    title: None,
                    color_mode: None,
                    default_unit: None,
                    dpi: None,
                    multipage_view: Some(next),
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
                });
            }
            if let Some(idx) = duplicate_page_req {
                app.duplicate_page(idx);
            }
            if let Some(c) = apply_doc_fill {
                app.apply_fill_color(c);
            }
            if let Some(c) = apply_doc_stroke {
                app.apply_outline_color(c);
            }
        });
}

// =============================================================================
// Central Rulers + Interactive Canvas Viewport
// =============================================================================

fn show_central_viewport(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();

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

            let corner_rect =
                Rect::from_min_size(total_rect.min, vec2(RULER_THICKNESS, RULER_THICKNESS));
            let top_ruler_rect = Rect::from_min_max(
                pos2(total_rect.left() + RULER_THICKNESS, total_rect.top()),
                pos2(total_rect.right(), total_rect.top() + RULER_THICKNESS),
            );
            let left_ruler_rect = Rect::from_min_max(
                pos2(total_rect.left(), total_rect.top() + RULER_THICKNESS),
                pos2(total_rect.left() + RULER_THICKNESS, total_rect.bottom()),
            );
            let canvas_rect = Rect::from_min_max(
                pos2(
                    total_rect.left() + RULER_THICKNESS,
                    total_rect.top() + RULER_THICKNESS,
                ),
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

                // Re-render dirty 256x256 tiles & update egui TextureHandle
                let rendered_tiles = tab.tile_cache.render_dirty_tiles(
                    &tab.document,
                    &tab.viewport,
                    tab.view_mode,
                    &tab.render_options,
                    &mut tab.renderer,
                );
                if rendered_tiles > 0 || tab.needs_rerender || tab.last_rendered_texture.is_none() {
                    let mut rgba_buf = tab.tile_cache.composite_to_rgba();

                    // Optional live Overprint / Separation plate preview overlay
                    if tab.overprint_preview {
                        if let Some(ref sep) = tab.separations_preview {
                            let sim = simulate_overprint_rgba(sep);
                            if sim.len() == (sep.width * sep.height * 4) as usize {
                                rgba_buf.width = sep.width;
                                rgba_buf.height = sep.height;
                                rgba_buf.rgba = sim;
                            }
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

                // Paint rendered vector canvas texture
                if let Some(ref tex) = tab.last_rendered_texture {
                    ui.painter().image(
                        tex.id(),
                        canvas_rect,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
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
                    "Full-Screen Preview — Press Esc or F9 to exit",
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
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!("Editing Symbol: {sym_name}"))
                                            .size(11.5)
                                            .strong()
                                            .color(tokens.accent),
                                    );
                                    if ui.small_button("Finish Editing").clicked() {
                                        app.finish_edit_symbol(true);
                                        pc_bar_clicked_finish = true;
                                    }
                                    if ui.small_button("Cancel").clicked() {
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
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("Editing PowerClip")
                                            .size(11.5)
                                            .strong()
                                            .color(tokens.accent),
                                    );
                                    if ui.small_button("Finish (Esc)").clicked() {
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
                show_canvas_context_menu(&canvas_resp, app);
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
            paint_canvas_overlays(ui, app, canvas_rect);

            // Live floating on-canvas TextEdit for ArtisticText / ParagraphText direct typing!
            show_inline_text_editor_overlay(ui, app, canvas_rect);

            // Paint Top & Left calibrated 20px Rulers + Corner box
            paint_rulers(
                ui,
                app,
                corner_rect,
                top_ruler_rect,
                left_ruler_rect,
                canvas_rect,
            );

            // Non-blocking floating toasts at top-right of canvas
            paint_toasts(ui, app, canvas_rect);
        });
}

fn screen_to_viewport_local(
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

fn screen_to_world(canvas_rect: Rect, tab: &crate::state::DocumentTab, screen: Pos2) -> Point2D {
    let local = screen_to_viewport_local(canvas_rect, tab, screen);
    tab.viewport.screen_to_world(local)
}

fn world_to_screen(canvas_rect: Rect, tab: &crate::state::DocumentTab, world: Point2D) -> Pos2 {
    let local = tab.viewport.world_to_screen(world);
    let sx = f64::from(tab.viewport.width_px) / f64::from(canvas_rect.width().max(1.0));
    let sy = f64::from(tab.viewport.height_px) / f64::from(canvas_rect.height().max(1.0));
    pos2(
        canvas_rect.left() + (local.x / sx) as f32,
        canvas_rect.top() + (local.y / sy) as f32,
    )
}

fn selection_handle_positions(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    bb: Rect2D,
) -> [(ScaleHandle, Pos2); 8] {
    selection_handle_world_positions(bb).map(|(h, w)| (h, world_to_screen(canvas_rect, tab, w)))
}

fn make_pointer_event(
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
        let factor = if scroll_y.abs() > 0.05 {
            (f64::from(scroll_y) * 0.0022).exp().clamp(0.88, 1.14)
        } else if (pinch_zoom - 1.0).abs() > 1e-4 {
            f64::from(pinch_zoom).powf(0.55).clamp(0.88, 1.14)
        } else {
            1.0
        };
        if (factor - 1.0).abs() > 1e-6 {
            if let Some(tab) = app.active_tab_mut() {
                let local = screen_to_viewport_local(canvas_rect, tab, hover);
                tab.viewport.zoom_at_screen_point(local, factor);
                tab.needs_rerender = true;
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
            app.run_tool_key_event(&KeyEvent {
                key: ToolKey::Space,
                modifiers,
            });
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

fn show_canvas_context_menu(canvas_resp: &egui::Response, app: &mut StudioApp) {
    canvas_resp.context_menu(|ui| {
        let has_sel = app.active_tab().is_some_and(|t| !t.selection.is_empty());
        let has_multi = app.active_tab().is_some_and(|t| t.selection.len() >= 2);
        let has_clone = app.active_tab().is_some_and(|t| {
            t.selection.objects.iter().any(|id| {
                t.document
                    .object(*id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Clone { .. }))
            })
        });
        let has_clip = app.has_clipboard_content();

        if ui
            .add_enabled(has_sel, egui::Button::new("Cut (Ctrl+X)"))
            .clicked()
        {
            app.cut_selection();
            ui.close();
        }
        if ui
            .add_enabled(has_sel, egui::Button::new("Copy (Ctrl+C)"))
            .clicked()
        {
            app.copy_selection();
            ui.close();
        }
        if ui
            .add_enabled(has_clip, egui::Button::new("Paste (Ctrl+V)"))
            .clicked()
        {
            app.paste_clipboard();
            ui.close();
        }
        if ui
            .add_enabled(has_sel, egui::Button::new("Duplicate (Ctrl+D)"))
            .clicked()
        {
            let _ = app.duplicate_selection();
            ui.close();
        }
        if ui
            .add_enabled(has_sel, egui::Button::new("Clone"))
            .clicked()
        {
            let _ = app.clone_selection();
            ui.close();
        }
        if has_clone && ui.button("Unlink Clone").clicked() {
            app.unlink_clone_selection();
            ui.close();
        }
        if ui
            .add_enabled(has_sel, egui::Button::new("Delete (Del)"))
            .clicked()
        {
            app.delete_selection();
            ui.close();
        }

        ui.separator();

        if ui
            .add_enabled(has_sel, egui::Button::new("Convert to Curves (Ctrl+Q)"))
            .clicked()
        {
            app.convert_selection_to_curves();
            ui.close();
        }
        if ui
            .add_enabled(
                has_sel,
                egui::Button::new("Convert Outline to Object (Ctrl+Shift+Q)"),
            )
            .clicked()
        {
            app.convert_outline_to_object_selection();
            ui.close();
        }
        if ui
            .add_enabled(has_sel, egui::Button::new("Break Apart (Ctrl+K)"))
            .clicked()
        {
            app.break_apart_selection();
            ui.close();
        }
        if ui
            .add_enabled(has_multi, egui::Button::new("Combine (Ctrl+L)"))
            .clicked()
        {
            app.apply_boolean_op(BooleanOp::Combine);
            ui.close();
        }
        ui.menu_button("Order", |ui| {
            if ui.button("To Front of Layer (Shift+PgUp)").clicked() {
                app.apply_z_order(ZOrderOp::BringToFront);
                ui.close();
            }
            if ui.button("Forward One (Ctrl+PgUp)").clicked() {
                app.apply_z_order(ZOrderOp::BringForward);
                ui.close();
            }
            if ui.button("Back One (Ctrl+PgDn)").clicked() {
                app.apply_z_order(ZOrderOp::SendBackward);
                ui.close();
            }
            if ui.button("To Back of Layer (Shift+PgDn)").clicked() {
                app.apply_z_order(ZOrderOp::SendToBack);
                ui.close();
            }
        });
        if ui
            .add_enabled(has_multi, egui::Button::new("Group (Ctrl+G)"))
            .clicked()
        {
            app.group_selection();
            ui.close();
        }
        if ui
            .add_enabled(has_sel, egui::Button::new("Ungroup (Ctrl+U)"))
            .clicked()
        {
            app.ungroup_selection();
            ui.close();
        }
        if ui
            .add_enabled(has_multi, egui::Button::new("PowerClip Inside"))
            .clicked()
        {
            app.create_powerclip_from_selection();
            ui.close();
        }
        let selected_pc = app.active_tab().and_then(|t| {
            let oid = *t.selection.objects.first()?;
            matches!(
                t.document.object(oid).map(|o| &o.payload),
                Some(NodePayload::PowerClip { .. })
            )
            .then_some(oid)
        });
        if let Some(pc_id) = selected_pc {
            if ui.button("Edit PowerClip Contents (Ctrl+Click)").clicked() {
                app.enter_powerclip_edit(pc_id);
                ui.close();
            }
            if ui.button("Fit PowerClip Contents").clicked() {
                app.fit_powerclip_selection(coree_core::PowerClipFitMode::FitContents);
                ui.close();
            }
            if ui.button("Center PowerClip Contents").clicked() {
                app.fit_powerclip_selection(coree_core::PowerClipFitMode::CenterContents);
                ui.close();
            }
            if ui.button("Stretch PowerClip to Fill").clicked() {
                app.fit_powerclip_selection(coree_core::PowerClipFitMode::StretchToFill);
                ui.close();
            }
        }
        if app
            .active_tab()
            .is_some_and(|t| t.powerclip_edit_id.is_some())
            && ui.button("Finish Editing PowerClip (Esc)").clicked()
        {
            app.exit_powerclip_edit();
            ui.close();
        }
        if ui
            .add_enabled(has_sel, egui::Button::new("Lock / Unlock"))
            .clicked()
        {
            app.toggle_lock_selection();
            ui.close();
        }

        ui.separator();

        if ui.button("Assign CutContour").clicked() {
            app.generate_cut_contour_active();
            ui.close();
        }
        if ui.button("Autofit Page to Content").clicked() {
            app.autofit_active_page(2.0);
            ui.close();
        }
        if ui.button("Properties").clicked() {
            app.focus_docker(DockerTab::Properties);
            ui.close();
        }
    });
}

fn show_inline_text_editor_overlay(ui: &egui::Ui, app: &mut StudioApp, canvas_rect: Rect) {
    let tokens = app.theme.tokens();
    let editor_geom = app.active_tab().and_then(|tab| {
        let edit = tab.inline_text_edit.as_ref()?;
        let pos = world_to_screen(canvas_rect, tab, edit.world_pos_mm);
        let raw_w = (edit.box_size_mm.x * tab.viewport.logical_px_per_mm()) as f32;
        let raw_h = (edit.box_size_mm.y * tab.viewport.logical_px_per_mm()) as f32;
        let w_px = if raw_w.is_finite() { raw_w } else { 180.0 };
        let h_px = if raw_h.is_finite() { raw_h } else { 48.0 };
        let avail_w = (canvas_rect.width() - 24.0).max(24.0);
        let avail_h = (canvas_rect.height() - 44.0).max(24.0);
        let max_w = avail_w.min(520.0);
        let min_w = max_w.min(180.0);
        let max_h = avail_h.min(260.0);
        let min_h = max_h.min(48.0);
        let width_px = w_px.clamp(min_w, max_w);
        let height_px = h_px.clamp(min_h, max_h);
        let min_x = canvas_rect.left() + 6.0;
        let max_x = (canvas_rect.right() - (width_px + 14.0)).max(min_x);
        let min_y = canvas_rect.top() + 6.0;
        let max_y = (canvas_rect.bottom() - (height_px + 38.0)).max(min_y);
        let px = if pos.x.is_finite() { pos.x } else { min_x };
        let py = if pos.y.is_finite() { pos.y } else { min_y };
        Some((
            pos2(px.clamp(min_x, max_x), py.clamp(min_y, max_y)),
            width_px,
            height_px,
            edit.is_paragraph,
        ))
    });
    let Some((anchor_pos, width_px, height_px, is_para)) = editor_geom else {
        return;
    };

    let mut commit_req = false;
    let mut cancel_req = false;

    egui::Area::new(ui.scope_id().with("coree_inline_text_edit_area"))
        .fixed_pos(anchor_pos)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(tokens.surface_elevated)
                .stroke(Stroke::new(1.5, tokens.accent))
                .corner_radius(CornerRadius::same(4))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    if let Some(tab) = app.active_tab_mut() {
                        if let Some(ref mut edit) = tab.inline_text_edit {
                            let resp = if is_para {
                                ui.add_sized(
                                    vec2(width_px, height_px),
                                    egui::TextEdit::multiline(&mut edit.buffer)
                                        .hint_text("Type paragraph text (Arabic / English)..."),
                                )
                            } else {
                                ui.add_sized(
                                    vec2(width_px, 28.0),
                                    egui::TextEdit::singleline(&mut edit.buffer)
                                        .hint_text("Type text (Arabic / English)..."),
                                )
                            };
                            if edit.just_opened {
                                resp.request_focus();
                                edit.just_opened = false;
                            }
                            let (ctrl_enter, plain_enter, esc) = ui.input(|i| {
                                (
                                    i.key_pressed(egui::Key::Enter)
                                        && (i.modifiers.ctrl || i.modifiers.command),
                                    i.key_pressed(egui::Key::Enter) && !i.modifiers.any(),
                                    i.key_pressed(egui::Key::Escape),
                                )
                            });
                            if esc {
                                cancel_req = true;
                            } else if ctrl_enter || (!is_para && plain_enter) {
                                commit_req = true;
                            }
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.small_button("✓ Apply").clicked() {
                            commit_req = true;
                        }
                        if ui.small_button("✕ Cancel").clicked() {
                            cancel_req = true;
                        }
                        ui.label(
                            RichText::new(if is_para {
                                "Ctrl+Enter to commit · Esc to cancel"
                            } else {
                                "Enter to commit · Esc to cancel"
                            })
                            .size(10.0)
                            .color(tokens.text_muted),
                        );
                    });
                });
        });

    if cancel_req {
        app.cancel_inline_text_edit();
    } else if commit_req {
        app.commit_inline_text_edit();
    }
}

// =============================================================================
// Interactive Overlays (Laser Toolpaths, Handles, Nodes, Effects, Crop, Cursors)
// =============================================================================

fn paint_dashed_line(
    painter: &egui::Painter,
    a: Pos2,
    b: Pos2,
    dash_px: f32,
    gap_px: f32,
    stroke: Stroke,
) {
    let delta = b - a;
    let total = delta.length();
    if total < 1.0 {
        return;
    }
    let dir = delta / total;
    let step = (dash_px + gap_px).max(2.0);
    let mut dist = 0.0_f32;
    let mut guard = 0;
    while dist < total && guard < 400 {
        let end = (dist + dash_px).min(total);
        painter.line_segment([a + dir * dist, a + dir * end], stroke);
        dist += step;
        guard += 1;
    }
}

fn paint_laser_toolpath_overlay(
    painter: &egui::Painter,
    app: &StudioApp,
    tab: &crate::state::DocumentTab,
    canvas_rect: Rect,
) {
    if !app.show_laser_toolpath_overlay {
        return;
    }
    let Some(ref plan) = app.laser_plan_preview else {
        return;
    };
    let rapid_color = Color32::from_rgb(0, 215, 245);
    let mut prev_world = Point2D::ZERO;

    for (idx, seg) in plan.segments.iter().enumerate() {
        if seg.points.is_empty() || seg.operation == LaserOperationKind::Ignore {
            continue;
        }
        let first_pt = seg.points[0];
        let a = world_to_screen(canvas_rect, tab, prev_world);
        let b = world_to_screen(canvas_rect, tab, first_pt);
        paint_dashed_line(painter, a, b, 5.0, 4.0, Stroke::new(1.1, rapid_color));

        let seg_color = match seg.operation {
            LaserOperationKind::Cut => Color32::from_rgb(235, 64, 52),
            LaserOperationKind::VectorEngrave => Color32::from_rgb(52, 152, 235),
            LaserOperationKind::HatchEngrave => Color32::from_rgb(46, 204, 113),
            LaserOperationKind::Perforate => Color32::from_rgb(243, 156, 18),
            LaserOperationKind::Ignore => Color32::GRAY,
        };
        let mut screen_pts: Vec<Pos2> = seg
            .points
            .iter()
            .map(|p| world_to_screen(canvas_rect, tab, *p))
            .collect();
        if seg.closed && screen_pts.len() >= 2 {
            screen_pts.push(screen_pts[0]);
        }
        if screen_pts.len() >= 2 {
            painter.add(Shape::line(screen_pts, Stroke::new(1.8, seg_color)));
        }

        // Numbered cut-order badge (capped to first 60 segments for crisp readability)
        if idx < 60 {
            let badge_pos = b;
            painter.circle(badge_pos, 7.5, seg_color, Stroke::new(1.0, Color32::WHITE));
            painter.text(
                badge_pos,
                Align2::CENTER_CENTER,
                format!("{}", idx + 1),
                FontId::monospace(8.5),
                Color32::WHITE,
            );
        }

        prev_world = if seg.closed {
            first_pt
        } else {
            *seg.points.last().unwrap_or(&first_pt)
        };
    }
}

fn paint_canvas_overlays(ui: &egui::Ui, app: &StudioApp, canvas_rect: Rect) {
    let Some(tab) = app.active_tab() else {
        return;
    };
    let tokens = app.theme.tokens();
    let painter = ui.painter().with_clip_rect(canvas_rect);

    // 0. Live Laser Toolpath Preview Overlay + Active PowerClip Edit Boundary
    paint_laser_toolpath_overlay(&painter, app, tab, canvas_rect);
    if let Some(pc_id) = tab.powerclip_edit_id {
        if let Some(pc_bb) = tab.document.world_object_bounds(pc_id) {
            let a = world_to_screen(canvas_rect, tab, pc_bb.min());
            let b = world_to_screen(canvas_rect, tab, pc_bb.max());
            let pr = Rect::from_two_pos(a, b);
            let dash = Stroke::new(1.6, tokens.accent_secondary);
            paint_dashed_line(&painter, pr.left_top(), pr.right_top(), 6.0, 4.0, dash);
            paint_dashed_line(&painter, pr.right_top(), pr.right_bottom(), 6.0, 4.0, dash);
            paint_dashed_line(
                &painter,
                pr.right_bottom(),
                pr.left_bottom(),
                6.0,
                4.0,
                dash,
            );
            paint_dashed_line(&painter, pr.left_bottom(), pr.left_top(), 6.0, 4.0, dash);
        }
    }

    // 1. Live Dynamic Snap Guidelines
    for g in &tab.active_guides {
        let a = world_to_screen(canvas_rect, tab, g.start);
        let b = world_to_screen(canvas_rect, tab, g.end);
        painter.line_segment([a, b], Stroke::new(1.0, tokens.accent_secondary));
    }

    // 2. Selection Bounding Box + Dual-Click Scale/Move vs Rotate/Skew Handles + Pivot Crosshair
    if let Some(bb) = tab.selection.bounding_box(&tab.document) {
        let min_s = world_to_screen(canvas_rect, tab, bb.min());
        let max_s = world_to_screen(canvas_rect, tab, bb.max());
        let sel_rect = Rect::from_two_pos(min_s, max_s);

        painter.rect_stroke(
            sel_rect,
            CornerRadius::ZERO,
            Stroke::new(1.2, tokens.accent),
            StrokeKind::Outside,
        );

        if app.active_tool != ToolId::ShapeNode {
            let handles = selection_handle_positions(canvas_rect, tab, bb);
            match tab.selection.transform_mode {
                TransformMode::ScaleAndMove => {
                    for (_, pos) in handles {
                        painter.rect(
                            Rect::from_center_size(pos, vec2(7.0, 7.0)),
                            CornerRadius::ZERO,
                            Color32::WHITE,
                            Stroke::new(1.5, tokens.accent),
                            StrokeKind::Inside,
                        );
                    }
                }
                TransformMode::RotateAndSkew => {
                    for (h, pos) in handles {
                        let is_corner = matches!(
                            h,
                            ScaleHandle::TopLeft
                                | ScaleHandle::TopRight
                                | ScaleHandle::BottomLeft
                                | ScaleHandle::BottomRight
                        );
                        if is_corner {
                            painter.circle(
                                pos,
                                5.0,
                                tokens.surface_elevated,
                                Stroke::new(1.5, tokens.accent_secondary),
                            );
                        } else {
                            painter.rect(
                                Rect::from_center_size(pos, vec2(8.0, 5.0)),
                                CornerRadius::same(1),
                                tokens.accent_secondary,
                                Stroke::new(1.0, tokens.on_accent_secondary),
                                StrokeKind::Inside,
                            );
                        }
                    }
                    let pv = world_to_screen(canvas_rect, tab, tab.selection.pivot);
                    painter.circle_stroke(pv, 6.0, Stroke::new(1.5, tokens.accent_secondary));
                    painter.line_segment(
                        [pv - vec2(9.0, 0.0), pv + vec2(9.0, 0.0)],
                        Stroke::new(1.2, tokens.accent_secondary),
                    );
                    painter.line_segment(
                        [pv - vec2(0.0, 9.0), pv + vec2(0.0, 9.0)],
                        Stroke::new(1.2, tokens.accent_secondary),
                    );
                }
            }
        }
    }

    // 3. `F10` Shape / Node Editing Overlay (Parametric Shape Diamonds + Cubic Bézier Nodes & Handles)
    if app.active_tool == ToolId::ShapeNode {
        for &oid in &tab.selection.objects {
            let Some(obj) = tab.document.object(oid) else {
                continue;
            };
            let world_xf = world_transform_of_or_local(&tab.document, oid);
            match &obj.payload {
                NodePayload::Shape(shape) => {
                    for (_, wpt) in parametric_shape_handles(shape, &world_xf) {
                        let p = world_to_screen(canvas_rect, tab, wpt);
                        let diamond = vec![
                            p + vec2(0.0, -6.0),
                            p + vec2(6.0, 0.0),
                            p + vec2(0.0, 6.0),
                            p + vec2(-6.0, 0.0),
                        ];
                        painter.add(Shape::convex_polygon(
                            diamond,
                            Color32::WHITE,
                            Stroke::new(1.6, tokens.accent_secondary),
                        ));
                    }
                }
                NodePayload::Path(p) => {
                    for (sp_idx, sp) in p.subpaths.iter().enumerate() {
                        for (n_idx, node) in sp.nodes.iter().enumerate() {
                            let nref = NodeRef {
                                object_id: oid,
                                subpath_index: sp_idx,
                                node_index: n_idx,
                            };
                            let selected = tab.selection.node_selection.contains(nref);
                            let a_pos = world_to_screen(
                                canvas_rect,
                                tab,
                                world_xf.transform_point(node.pos),
                            );
                            for h in [node.handle_in, node.handle_out].into_iter().flatten() {
                                let h_pos =
                                    world_to_screen(canvas_rect, tab, world_xf.transform_point(h));
                                painter
                                    .line_segment([a_pos, h_pos], Stroke::new(1.0, tokens.accent));
                                painter.circle(
                                    h_pos,
                                    3.5,
                                    tokens.accent,
                                    Stroke::new(1.0, Color32::WHITE),
                                );
                            }
                            let fill = if selected {
                                tokens.accent
                            } else {
                                Color32::WHITE
                            };
                            if node.kind == NodeKind::Cusp {
                                painter.rect(
                                    Rect::from_center_size(a_pos, vec2(7.0, 7.0)),
                                    CornerRadius::ZERO,
                                    fill,
                                    Stroke::new(1.5, tokens.accent),
                                    StrokeKind::Inside,
                                );
                            } else {
                                painter.circle(a_pos, 4.0, fill, Stroke::new(1.5, tokens.accent));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // 3b. Selected Canvas Guide Highlight & Delete cue
    if let Some(sel_gid) = tab.selected_guide_id {
        if let Some(g) = tab.document.guide(sel_gid) {
            let guide_stroke = Stroke::new(1.6, tokens.danger);
            match g.orientation {
                GuideOrientation::Horizontal(y_mm) => {
                    let sy = world_to_screen(canvas_rect, tab, Point2D::new(0.0, y_mm)).y;
                    if sy >= canvas_rect.top() && sy <= canvas_rect.bottom() {
                        painter.line_segment(
                            [pos2(canvas_rect.left(), sy), pos2(canvas_rect.right(), sy)],
                            guide_stroke,
                        );
                        painter.rect_filled(
                            Rect::from_center_size(
                                pos2(canvas_rect.left() + 8.0, sy),
                                vec2(8.0, 8.0),
                            ),
                            CornerRadius::same(1),
                            tokens.danger,
                        );
                    }
                }
                GuideOrientation::Vertical(x_mm) => {
                    let sx = world_to_screen(canvas_rect, tab, Point2D::new(x_mm, 0.0)).x;
                    if sx >= canvas_rect.left() && sx <= canvas_rect.right() {
                        painter.line_segment(
                            [pos2(sx, canvas_rect.top()), pos2(sx, canvas_rect.bottom())],
                            guide_stroke,
                        );
                        painter.rect_filled(
                            Rect::from_center_size(
                                pos2(sx, canvas_rect.top() + 8.0),
                                vec2(8.0, 8.0),
                            ),
                            CornerRadius::same(1),
                            tokens.danger,
                        );
                    }
                }
                GuideOrientation::Angled { origin, angle_deg } => {
                    let rad = angle_deg.to_radians();
                    let p0 = world_to_screen(canvas_rect, tab, origin);
                    let dir = vec2(rad.cos() as f32, rad.sin() as f32) * 2000.0;
                    painter.line_segment([p0 - dir, p0 + dir], guide_stroke);
                }
            }
        }
    }

    // 4. Interactive Fill (all 4 types + mid-stops), Transparency, Persistent Effect Handles, Envelope & Mesh Overlays
    if app.active_tool == ToolId::InteractiveFill && !tab.selection.objects.is_empty() {
        let active_fill_drag_oid = match &tab.tool_core.drag_state {
            ActiveDragState::DraggingGradientHandle { object_id, .. }
            | ActiveDragState::DraggingGradientFocal { object_id, .. }
            | ActiveDragState::DraggingGradientStop { object_id, .. } => Some(*object_id),
            _ => None,
        };
        for oid in collect_editable_fillable_targets(&tab.document, &tab.selection.objects) {
            if Some(oid) == active_fill_drag_oid {
                continue;
            }
            if let Some(obj) = tab.document.object(oid) {
                let world_xf = world_transform_of_or_local(&tab.document, oid);
                match &obj.fill {
                    Fill::LinearGradient { start, end, stops } => {
                        let a = world_to_screen(canvas_rect, tab, world_xf.transform_point(*start));
                        let b = world_to_screen(canvas_rect, tab, world_xf.transform_point(*end));
                        painter.line_segment([a, b], Stroke::new(2.0, tokens.accent));
                        draw_gradient_mid_stops(
                            &painter,
                            canvas_rect,
                            tab,
                            &world_xf,
                            *start,
                            *end,
                            stops,
                        );
                        painter.rect(
                            Rect::from_center_size(a, vec2(9.0, 9.0)),
                            CornerRadius::same(2),
                            Color32::WHITE,
                            Stroke::new(2.0, tokens.accent),
                            StrokeKind::Inside,
                        );
                        painter.circle(b, 5.0, tokens.accent, Stroke::new(1.5, Color32::WHITE));
                    }
                    Fill::RadialGradient {
                        center,
                        radius,
                        focal,
                        stops,
                    } => {
                        let a =
                            world_to_screen(canvas_rect, tab, world_xf.transform_point(*center));
                        let r_pt = Point2D::new(center.x + radius.max(1.0), center.y);
                        let b = world_to_screen(canvas_rect, tab, world_xf.transform_point(r_pt));
                        let r_px = a.distance(b).max(2.0);
                        painter.circle_stroke(a, r_px, Stroke::new(1.2, tokens.accent));
                        painter.line_segment([a, b], Stroke::new(1.8, tokens.accent));
                        draw_gradient_mid_stops(
                            &painter,
                            canvas_rect,
                            tab,
                            &world_xf,
                            *center,
                            r_pt,
                            stops,
                        );
                        if (*focal - *center).length() > 0.2 {
                            let f =
                                world_to_screen(canvas_rect, tab, world_xf.transform_point(*focal));
                            paint_dashed_line(
                                &painter,
                                a,
                                f,
                                4.0,
                                3.0,
                                Stroke::new(1.2, tokens.accent_secondary),
                            );
                            painter.circle(
                                f,
                                4.5,
                                tokens.accent_secondary,
                                Stroke::new(1.5, Color32::WHITE),
                            );
                        }
                        painter.rect(
                            Rect::from_center_size(a, vec2(9.0, 9.0)),
                            CornerRadius::same(2),
                            Color32::WHITE,
                            Stroke::new(2.0, tokens.accent),
                            StrokeKind::Inside,
                        );
                        painter.circle(b, 5.0, tokens.accent, Stroke::new(1.5, Color32::WHITE));
                    }
                    Fill::ConicalGradient {
                        center,
                        angle_deg,
                        stops,
                    } => {
                        let a =
                            world_to_screen(canvas_rect, tab, world_xf.transform_point(*center));
                        let rad = angle_deg.to_radians();
                        let e_pt =
                            Point2D::new(center.x + 25.0 * rad.cos(), center.y + 25.0 * rad.sin());
                        let b = world_to_screen(canvas_rect, tab, world_xf.transform_point(e_pt));
                        painter.circle_stroke(
                            a,
                            a.distance(b).max(16.0),
                            Stroke::new(1.2, tokens.accent),
                        );
                        painter.line_segment([a, b], Stroke::new(2.0, tokens.accent));
                        draw_gradient_mid_stops(
                            &painter,
                            canvas_rect,
                            tab,
                            &world_xf,
                            *center,
                            e_pt,
                            stops,
                        );
                        painter.rect(
                            Rect::from_center_size(a, vec2(9.0, 9.0)),
                            CornerRadius::same(2),
                            Color32::WHITE,
                            Stroke::new(2.0, tokens.accent),
                            StrokeKind::Inside,
                        );
                        painter.circle(b, 5.0, tokens.accent, Stroke::new(1.5, Color32::WHITE));
                    }
                    Fill::SquareGradient {
                        center,
                        size,
                        angle_deg,
                        stops,
                    } => {
                        let a =
                            world_to_screen(canvas_rect, tab, world_xf.transform_point(*center));
                        let rad = angle_deg.to_radians();
                        let half = size.max(2.0) * 0.5;
                        let e_pt =
                            Point2D::new(center.x + half * rad.cos(), center.y + half * rad.sin());
                        let b = world_to_screen(canvas_rect, tab, world_xf.transform_point(e_pt));
                        painter.line_segment([a, b], Stroke::new(2.0, tokens.accent));
                        draw_gradient_mid_stops(
                            &painter,
                            canvas_rect,
                            tab,
                            &world_xf,
                            *center,
                            e_pt,
                            stops,
                        );
                        painter.rect(
                            Rect::from_center_size(a, vec2(9.0, 9.0)),
                            CornerRadius::same(2),
                            Color32::WHITE,
                            Stroke::new(2.0, tokens.accent),
                            StrokeKind::Inside,
                        );
                        painter.circle(b, 5.0, tokens.accent, Stroke::new(1.5, Color32::WHITE));
                    }
                    _ => {}
                }
            }
        }
    } else if app.active_tool == ToolId::MeshFill && !tab.selection.objects.is_empty() {
        let active_mesh_drag_oid = match &tab.tool_core.drag_state {
            ActiveDragState::DraggingMeshNode { object_id, .. }
            | ActiveDragState::DraggingMeshHandle { object_id, .. } => Some(*object_id),
            _ => None,
        };
        for oid in collect_editable_fillable_targets(&tab.document, &tab.selection.objects) {
            if Some(oid) == active_mesh_drag_oid {
                continue;
            }
            if let Some(obj) = tab.document.object(oid) {
                let world_xf = world_transform_of_or_local(&tab.document, oid);
                if let Fill::MeshFill { rows, cols, nodes } = &obj.fill {
                    let spts: Vec<Pos2> = nodes
                        .iter()
                        .map(|n| world_to_screen(canvas_rect, tab, world_xf.transform_point(n.pos)))
                        .collect();
                    for r in 0..*rows {
                        for c in 0..*cols {
                            let idx = r * cols + c;
                            if c + 1 < *cols && idx + 1 < spts.len() {
                                painter.line_segment(
                                    [spts[idx], spts[idx + 1]],
                                    Stroke::new(1.0, tokens.accent.gamma_multiply(0.8)),
                                );
                            }
                            if r + 1 < *rows && idx + cols < spts.len() {
                                painter.line_segment(
                                    [spts[idx], spts[idx + cols]],
                                    Stroke::new(1.0, tokens.accent.gamma_multiply(0.8)),
                                );
                            }
                        }
                    }
                    for (idx, (mn, &sp)) in nodes.iter().zip(spts.iter()).enumerate() {
                        let is_sel = tab.selected_mesh_node == Some((oid, idx));
                        if is_sel {
                            for rel in [mn.handle_n, mn.handle_s, mn.handle_e, mn.handle_w]
                                .into_iter()
                                .flatten()
                            {
                                let h_world = world_xf.transform_point(Point2D::new(
                                    mn.pos.x + rel.x,
                                    mn.pos.y + rel.y,
                                ));
                                let hsp = world_to_screen(canvas_rect, tab, h_world);
                                painter.line_segment(
                                    [sp, hsp],
                                    Stroke::new(1.2, tokens.accent_secondary),
                                );
                                let diamond = vec![
                                    hsp + vec2(0.0, -5.0),
                                    hsp + vec2(5.0, 0.0),
                                    hsp + vec2(0.0, 5.0),
                                    hsp + vec2(-5.0, 0.0),
                                ];
                                painter.add(Shape::convex_polygon(
                                    diamond,
                                    Color32::WHITE,
                                    Stroke::new(1.4, tokens.accent_secondary),
                                ));
                            }
                        }
                        painter.circle(
                            sp,
                            if is_sel { 5.5 } else { 4.2 },
                            core_color_to_egui(&mn.color),
                            Stroke::new(
                                if is_sel { 2.0 } else { 1.3 },
                                if is_sel {
                                    tokens.accent_secondary
                                } else {
                                    Color32::WHITE
                                },
                            ),
                        );
                    }
                }
            }
        }
    } else if let Some(&oid) = tab.selection.objects.first() {
        if let Some(obj) = tab.document.object(oid) {
            let world_xf = world_transform_of_or_local(&tab.document, oid);
            if matches!(
                app.active_tool,
                ToolId::DropShadow
                    | ToolId::BlockShadow
                    | ToolId::Contour
                    | ToolId::Extrude25D
                    | ToolId::Blend
                    | ToolId::Distort
            ) {
                for eff in &obj.effects {
                    if let Some((anchor_w, knob_w)) =
                        interactive_effect_handle_points(&tab.document, obj, eff, app.active_tool)
                    {
                        let sa = world_to_screen(canvas_rect, tab, anchor_w);
                        let sb = world_to_screen(canvas_rect, tab, knob_w);
                        paint_dashed_line(
                            &painter,
                            sa,
                            sb,
                            5.0,
                            3.0,
                            Stroke::new(1.5, tokens.accent_secondary),
                        );
                        painter.rect(
                            Rect::from_center_size(sa, vec2(7.0, 7.0)),
                            CornerRadius::same(1),
                            Color32::WHITE,
                            Stroke::new(1.5, tokens.accent_secondary),
                            StrokeKind::Inside,
                        );
                        painter.circle(
                            sb,
                            5.5,
                            tokens.accent_secondary,
                            Stroke::new(1.5, Color32::WHITE),
                        );
                    }
                }
            }
            if app.active_tool == ToolId::Transparency {
                for eff in &obj.effects {
                    if let Effect::Transparency {
                        mode: TransparencyMode::FountainLinear { start, end, .. },
                        ..
                    } = eff
                    {
                        let a = world_to_screen(canvas_rect, tab, world_xf.transform_point(*start));
                        let b = world_to_screen(canvas_rect, tab, world_xf.transform_point(*end));
                        painter.line_segment([a, b], Stroke::new(2.0, tokens.accent_secondary));
                        painter.rect(
                            Rect::from_center_size(a, vec2(9.0, 9.0)),
                            CornerRadius::same(1),
                            Color32::WHITE,
                            Stroke::new(1.5, Color32::BLACK),
                            StrokeKind::Inside,
                        );
                        painter.rect(
                            Rect::from_center_size(b, vec2(9.0, 9.0)),
                            CornerRadius::same(1),
                            Color32::BLACK,
                            Stroke::new(1.5, Color32::WHITE),
                            StrokeKind::Inside,
                        );
                    }
                }
            }
            if app.active_tool == ToolId::Envelope {
                for eff in &obj.effects {
                    if let Effect::Envelope { cage, .. } = eff {
                        if cage.len() >= 4 {
                            let spts: Vec<Pos2> = cage
                                .iter()
                                .map(|&p| {
                                    world_to_screen(canvas_rect, tab, world_xf.transform_point(p))
                                })
                                .collect();
                            for i in 0..spts.len() {
                                paint_dashed_line(
                                    &painter,
                                    spts[i],
                                    spts[(i + 1) % spts.len()],
                                    4.0,
                                    3.0,
                                    Stroke::new(1.3, tokens.cutcontour),
                                );
                            }
                            for &p in &spts {
                                painter.rect(
                                    Rect::from_center_size(p, vec2(8.0, 8.0)),
                                    CornerRadius::same(1),
                                    Color32::WHITE,
                                    Stroke::new(1.5, tokens.cutcontour),
                                    StrokeKind::Inside,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // 5. Pending Interactive Crop Overlay (Darkened Mask Outside + Draggable Handles)
    if let Some(crop) = tab.pending_crop_rect {
        let cmin = world_to_screen(canvas_rect, tab, crop.min());
        let cmax = world_to_screen(canvas_rect, tab, crop.max());
        let cr = Rect::from_two_pos(cmin, cmax).intersect(canvas_rect);
        let mask_col = Color32::from_rgba_unmultiplied(0, 0, 0, 128);
        painter.rect_filled(
            Rect::from_min_max(canvas_rect.min, pos2(canvas_rect.right(), cr.top())),
            CornerRadius::ZERO,
            mask_col,
        );
        painter.rect_filled(
            Rect::from_min_max(pos2(canvas_rect.left(), cr.bottom()), canvas_rect.max),
            CornerRadius::ZERO,
            mask_col,
        );
        painter.rect_filled(
            Rect::from_min_max(
                pos2(canvas_rect.left(), cr.top()),
                pos2(cr.left(), cr.bottom()),
            ),
            CornerRadius::ZERO,
            mask_col,
        );
        painter.rect_filled(
            Rect::from_min_max(
                pos2(cr.right(), cr.top()),
                pos2(canvas_rect.right(), cr.bottom()),
            ),
            CornerRadius::ZERO,
            mask_col,
        );
        painter.rect_stroke(
            cr,
            CornerRadius::ZERO,
            Stroke::new(1.8, tokens.accent_secondary),
            StrokeKind::Outside,
        );
        for (_, hpos) in selection_handle_positions(canvas_rect, tab, crop) {
            painter.rect(
                Rect::from_center_size(hpos, vec2(8.0, 8.0)),
                CornerRadius::ZERO,
                Color32::WHITE,
                Stroke::new(1.5, tokens.accent_secondary),
                StrokeKind::Inside,
            );
        }
        painter.text(
            pos2(
                cr.center().x,
                (cr.top() - 12.0).max(canvas_rect.top() + 10.0),
            ),
            Align2::CENTER_CENTER,
            format!(
                "Crop {:.1} × {:.1} mm · Enter / Double-Click to apply · Esc to cancel",
                crop.width(),
                crop.height()
            ),
            FontId::proportional(11.0),
            Color32::WHITE,
        );
    }

    // 6. Active Bitmap Mask Selection Overlay (`MaskSelect` — Rect, Ellipse, Lasso, Magic Wand)
    if let Some(ref mask) = tab.raster_mask {
        let stroke = Stroke::new(1.4, Color32::WHITE);
        match &mask.shape {
            RasterMaskShape::Rectangle(mrect) => {
                let a = world_to_screen(canvas_rect, tab, mrect.min());
                let b = world_to_screen(canvas_rect, tab, mrect.max());
                let r = Rect::from_two_pos(a, b);
                paint_dashed_line(&painter, r.left_top(), r.right_top(), 5.0, 4.0, stroke);
                paint_dashed_line(&painter, r.right_top(), r.right_bottom(), 5.0, 4.0, stroke);
                paint_dashed_line(
                    &painter,
                    r.right_bottom(),
                    r.left_bottom(),
                    5.0,
                    4.0,
                    stroke,
                );
                paint_dashed_line(&painter, r.left_bottom(), r.left_top(), 5.0, 4.0, stroke);
            }
            RasterMaskShape::Ellipse(mrect) => {
                let c = mrect.center();
                let rx = mrect.width() * 0.5;
                let ry = mrect.height() * 0.5;
                let steps = 36usize;
                for i in 0..steps {
                    let t0 = (i as f64 / steps as f64) * std::f64::consts::TAU;
                    let t1 = ((i + 1) as f64 / steps as f64) * std::f64::consts::TAU;
                    let p0 = world_to_screen(
                        canvas_rect,
                        tab,
                        Point2D::new(c.x + rx * t0.cos(), c.y + ry * t0.sin()),
                    );
                    let p1 = world_to_screen(
                        canvas_rect,
                        tab,
                        Point2D::new(c.x + rx * t1.cos(), c.y + ry * t1.sin()),
                    );
                    paint_dashed_line(&painter, p0, p1, 4.0, 3.0, stroke);
                }
            }
            RasterMaskShape::Lasso(pts) if pts.len() >= 2 => {
                for i in 0..pts.len() {
                    let p0 = world_to_screen(canvas_rect, tab, pts[i]);
                    let p1 = world_to_screen(canvas_rect, tab, pts[(i + 1) % pts.len()]);
                    paint_dashed_line(&painter, p0, p1, 4.0, 3.0, stroke);
                }
            }
            RasterMaskShape::MagicWand { seed_mm, .. } => {
                let sp = world_to_screen(canvas_rect, tab, *seed_mm);
                painter.circle_stroke(sp, 6.0, stroke);
                painter.line_segment([pos2(sp.x - 8.0, sp.y), pos2(sp.x + 8.0, sp.y)], stroke);
                painter.line_segment([pos2(sp.x, sp.y - 8.0), pos2(sp.x, sp.y + 8.0)], stroke);
            }
            _ => {}
        }
    } else if let Some(mrect) = tab.raster_mask_rect_mm {
        let a = world_to_screen(canvas_rect, tab, mrect.min());
        let b = world_to_screen(canvas_rect, tab, mrect.max());
        let r = Rect::from_two_pos(a, b);
        paint_dashed_line(
            &painter,
            r.left_top(),
            r.right_top(),
            5.0,
            4.0,
            Stroke::new(1.4, Color32::WHITE),
        );
        paint_dashed_line(
            &painter,
            r.right_top(),
            r.right_bottom(),
            5.0,
            4.0,
            Stroke::new(1.4, Color32::WHITE),
        );
        paint_dashed_line(
            &painter,
            r.right_bottom(),
            r.left_bottom(),
            5.0,
            4.0,
            Stroke::new(1.4, Color32::WHITE),
        );
        paint_dashed_line(
            &painter,
            r.left_bottom(),
            r.left_top(),
            5.0,
            4.0,
            Stroke::new(1.4, Color32::WHITE),
        );
    }

    // 7. Layer 3 ToolCore Transient Overlay (previews, transformed outlines, marquees, drafts, guides)
    paint_transient_overlay(&painter, canvas_rect, tab, &tab.transient_overlay, &tokens);

    // 11. Live Circular Brush Cursor (All 5 Sculpt Tools, Vector Eraser & Bitmap Studio Brushes)
    if let Some(hover) = ui.input(|i| i.pointer.hover_pos()) {
        if canvas_rect.contains(hover) {
            let brush_radius_px = match app.active_tool {
                ToolId::Smooth
                | ToolId::Smear
                | ToolId::Twirl
                | ToolId::AttractRepel
                | ToolId::Roughen => Some(
                    (app.tool_settings.sculpt_radius_mm * tab.viewport.logical_px_per_mm()) as f32,
                ),
                ToolId::Eraser => Some(
                    (app.tool_settings.eraser_radius_mm * tab.viewport.logical_px_per_mm()) as f32,
                ),
                ToolId::BitmapBrush
                | ToolId::BitmapEraser
                | ToolId::CloneStamp
                | ToolId::Smudge => Some(app.tool_settings.brush_radius_px),
                _ => None,
            };
            if let Some(r_px) = brush_radius_px {
                let clamped = r_px.clamp(4.0, 220.0);
                painter.circle_stroke(hover, clamped, Stroke::new(1.3, tokens.accent_secondary));
                painter.circle_stroke(
                    hover,
                    (clamped - 1.2).max(2.0),
                    Stroke::new(0.8, Color32::WHITE.gamma_multiply(0.75)),
                );
            }
            // Eyedropper Sample/Apply mode pill near cursor
            if matches!(
                app.active_tool,
                ToolId::ColorEyedropper | ToolId::AttributesEyedropper
            ) {
                let lbl = if app.tool_settings.eyedropper_apply_mode {
                    "Apply"
                } else {
                    "Sample"
                };
                painter.text(
                    hover + vec2(14.0, -12.0),
                    Align2::LEFT_CENTER,
                    lbl,
                    FontId::monospace(10.0),
                    tokens.accent_secondary,
                );
            }
            // Connector snap target ring highlight
            if matches!(
                app.active_tool,
                ToolId::StraightConnector | ToolId::RightAngleConnector
            ) {
                if let Some((_, _, snap_pt)) = find_nearest_object_anchor_in_doc(
                    &tab.document,
                    tab.document.active_page_index(),
                    tab.powerclip_edit_id,
                    app.treat_all_objects_as_filled,
                    tab.cursor_world_mm,
                    8.0,
                ) {
                    let sp = world_to_screen(canvas_rect, tab, snap_pt);
                    painter.circle_stroke(sp, 6.0, Stroke::new(2.0, tokens.accent_secondary));
                }
            }
        }
    }
}

// =============================================================================
// Calibrated Top & Left 20px Rulers
// =============================================================================

fn paint_rulers(
    ui: &egui::Ui,
    app: &StudioApp,
    corner: Rect,
    top: Rect,
    left: Rect,
    canvas_rect: Rect,
) {
    let tokens = app.theme.tokens();
    let painter = ui.painter();

    painter.rect(
        corner,
        CornerRadius::ZERO,
        tokens.surface,
        Stroke::new(1.0, tokens.border),
        StrokeKind::Inside,
    );
    painter.rect(
        top,
        CornerRadius::ZERO,
        tokens.surface,
        Stroke::new(1.0, tokens.border),
        StrokeKind::Inside,
    );
    painter.rect(
        left,
        CornerRadius::ZERO,
        tokens.surface,
        Stroke::new(1.0, tokens.border),
        StrokeKind::Inside,
    );

    let Some(tab) = app.active_tab() else {
        return;
    };
    let u = tab.document.default_unit();
    painter.text(
        corner.center(),
        Align2::CENTER_CENTER,
        u.suffix(),
        FontId::monospace(8.5),
        tokens.text_muted,
    );

    let px_per_mm = tab.viewport.logical_px_per_mm().max(0.05);
    // Choose a clean human tick step in mm based on current zoom
    let step_mm = if px_per_mm >= 8.0 {
        5.0
    } else if px_per_mm >= 3.0 {
        10.0
    } else if px_per_mm >= 1.2 {
        20.0
    } else if px_per_mm >= 0.5 {
        50.0
    } else {
        100.0
    };

    let vis = tab.viewport.visible_world_rect();

    // Horizontal (top) ruler ticks
    let top_painter = painter.with_clip_rect(top);
    let start_x = (vis.min_x / step_mm).floor() * step_mm;
    let mut x = start_x;
    let mut guard = 0;
    while x <= vis.max_x + step_mm && guard < 300 {
        let sx = world_to_screen(canvas_rect, tab, Point2D::new(x, 0.0)).x;
        if sx >= top.left() && sx <= top.right() {
            top_painter.line_segment(
                [pos2(sx, top.bottom() - 7.0), pos2(sx, top.bottom())],
                Stroke::new(1.0, tokens.border_input),
            );
            let val = u.from_mm(x, f64::from(tab.document.dpi()));
            top_painter.text(
                pos2(sx + 2.0, top.top() + 2.0),
                Align2::LEFT_TOP,
                format!("{val:.0}"),
                FontId::monospace(9.0),
                tokens.text_muted,
            );
        }
        x += step_mm;
        guard += 1;
    }

    // Vertical (left) ruler ticks
    let left_painter = painter.with_clip_rect(left);
    let start_y = (vis.min_y / step_mm).floor() * step_mm;
    let mut y = start_y;
    guard = 0;
    while y <= vis.max_y + step_mm && guard < 300 {
        let sy = world_to_screen(canvas_rect, tab, Point2D::new(0.0, y)).y;
        if sy >= left.top() && sy <= left.bottom() {
            left_painter.line_segment(
                [pos2(left.right() - 7.0, sy), pos2(left.right(), sy)],
                Stroke::new(1.0, tokens.border_input),
            );
            let val = u.from_mm(y, f64::from(tab.document.dpi()));
            left_painter.text(
                pos2(left.left() + 1.0, sy + 2.0),
                Align2::LEFT_TOP,
                format!("{val:.0}"),
                FontId::monospace(8.0),
                tokens.text_muted,
            );
        }
        y += step_mm;
        guard += 1;
    }
}

fn paint_toasts(ui: &egui::Ui, app: &StudioApp, canvas_rect: Rect) {
    if app.toasts.is_empty() {
        return;
    }
    let tokens = app.theme.tokens();
    let painter = ui.painter();
    let mut y = canvas_rect.top() + 10.0;
    for toast in app.toasts.iter().rev().take(3) {
        let accent = if toast.is_error {
            tokens.danger
        } else {
            tokens.accent
        };
        let galley = painter.layout_no_wrap(
            toast.message.clone(),
            FontId::proportional(11.5),
            tokens.text,
        );
        let box_w = galley.size().x + 20.0;
        let box_h = galley.size().y + 10.0;
        let rect = Rect::from_min_size(
            pos2(
                (canvas_rect.right() - box_w - 12.0).max(canvas_rect.left() + 8.0),
                y,
            ),
            vec2(box_w, box_h),
        );
        painter.rect(
            rect,
            CornerRadius::same(6),
            tokens.surface_elevated,
            Stroke::new(1.0, accent),
            StrokeKind::Inside,
        );
        painter.galley(rect.left_top() + vec2(10.0, 5.0), galley, tokens.text);
        y += box_h + 6.0;
    }
}

fn paint_transient_overlay(
    painter: &egui::Painter,
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    overlay: &TransientOverlay,
    tokens: &crate::theme::ThemeTokens,
) {
    for g in &overlay.active_guides {
        let a = world_to_screen(canvas_rect, tab, g.start);
        let b = world_to_screen(canvas_rect, tab, g.end);
        painter.line_segment([a, b], Stroke::new(1.0, tokens.accent_secondary));
    }

    for p in &overlay.preview_paths {
        let (width, col) = match p.stroke_kind {
            OverlayStrokeKind::Accent => (1.6, tokens.accent),
            OverlayStrokeKind::Secondary => (1.6, tokens.accent_secondary),
            OverlayStrokeKind::Danger => (1.8, tokens.danger),
            OverlayStrokeKind::Guide => (1.2, tokens.accent),
            OverlayStrokeKind::MarqueeFill => (1.1, tokens.accent),
            OverlayStrokeKind::CropMask => (1.5, tokens.accent_secondary),
            OverlayStrokeKind::DashedGuide => (1.2, tokens.accent_secondary),
        };
        let stroke = Stroke::new(width, col);
        for poly in p.path.flatten(0.4) {
            let spts: Vec<Pos2> = poly
                .into_iter()
                .map(|pt| world_to_screen(canvas_rect, tab, pt))
                .collect();
            if spts.len() < 2 {
                continue;
            }
            if p.closed_fill && spts.len() >= 3 {
                let fill = col.gamma_multiply(0.14);
                if spts.len() <= 5 {
                    painter.add(Shape::convex_polygon(spts, fill, stroke));
                } else {
                    painter.add(Shape::closed_line(spts, stroke));
                }
            } else if p.stroke_kind == OverlayStrokeKind::DashedGuide {
                for seg in spts.windows(2) {
                    paint_dashed_line(painter, seg[0], seg[1], 5.0, 4.0, stroke);
                }
            } else {
                painter.add(Shape::line(spts, stroke));
            }
        }
    }

    for h in &overlay.preview_handles {
        let sp = world_to_screen(canvas_rect, tab, h.pos_mm);
        let col = h
            .color_hint
            .as_ref()
            .map_or(tokens.accent, core_color_to_egui);
        let fill = if h.selected { col } else { Color32::WHITE };
        match h.shape {
            HandleShape::Square => {
                painter.rect(
                    Rect::from_center_size(sp, vec2(7.0, 7.0)),
                    CornerRadius::ZERO,
                    fill,
                    Stroke::new(1.5, col),
                    StrokeKind::Inside,
                );
            }
            HandleShape::Circle => {
                painter.circle(sp, 4.0, fill, Stroke::new(1.5, col));
            }
            HandleShape::Diamond => {
                let diamond = vec![
                    sp + vec2(0.0, -6.0),
                    sp + vec2(6.0, 0.0),
                    sp + vec2(0.0, 6.0),
                    sp + vec2(-6.0, 0.0),
                ];
                painter.add(Shape::convex_polygon(diamond, fill, Stroke::new(1.5, col)));
            }
            HandleShape::Crosshair => {
                painter.circle_stroke(sp, 6.0, Stroke::new(1.5, col));
                painter.line_segment(
                    [sp - vec2(9.0, 0.0), sp + vec2(9.0, 0.0)],
                    Stroke::new(1.2, col),
                );
                painter.line_segment(
                    [sp - vec2(0.0, 9.0), sp + vec2(0.0, 9.0)],
                    Stroke::new(1.2, col),
                );
            }
        }
    }
}

fn draw_gradient_mid_stops(
    painter: &egui::Painter,
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    xf: &Affine2D,
    start_l: Point2D,
    end_l: Point2D,
    stops: &[GradientStop],
) {
    for (s_idx, st) in stops.iter().enumerate() {
        if s_idx == 0 || s_idx + 1 == stops.len() {
            continue;
        }
        let t = f64::from(st.offset);
        let w_pt = xf.transform_point(Point2D::new(
            start_l.x + (end_l.x - start_l.x) * t,
            start_l.y + (end_l.y - start_l.y) * t,
        ));
        let sp = world_to_screen(canvas_rect, tab, w_pt);
        painter.rect(
            Rect::from_center_size(sp, vec2(8.0, 8.0)),
            CornerRadius::same(1),
            core_color_to_egui(&st.color),
            Stroke::new(1.5, Color32::WHITE),
            StrokeKind::Inside,
        );
    }
}

/// Inserts an interpolated color stop at normalized ray fraction `t` across any of the 4
/// interactive fountain fill gradient types.
#[cfg(test)]
fn insert_stop_on_fill_ray(fill: &Fill, t: f32) -> Option<Fill> {
    let clamped = t.clamp(0.02, 0.98);
    let mut updated = fill.clone();
    let (_, _, stops) = gradient_stops_mut(&mut updated)?;
    let color = interpolate_gradient_stop_color(stops, clamped);
    stops.push(GradientStop::new(clamped, color));
    stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    Some(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_fill_inserts_interpolated_stops_across_all_four_gradient_types() {
        let base_stops = vec![
            GradientStop::new(0.0, Color::rgba8(255, 0, 0, 255)),
            GradientStop::new(1.0, Color::rgba8(0, 0, 255, 255)),
        ];
        let variants = [
            Fill::LinearGradient {
                start: Point2D::ZERO,
                end: Point2D::new(40.0, 0.0),
                stops: base_stops.clone(),
            },
            Fill::RadialGradient {
                center: Point2D::ZERO,
                radius: 25.0,
                focal: Point2D::ZERO,
                stops: base_stops.clone(),
            },
            Fill::ConicalGradient {
                center: Point2D::ZERO,
                angle_deg: 0.0,
                stops: base_stops.clone(),
            },
            Fill::SquareGradient {
                center: Point2D::ZERO,
                size: 30.0,
                angle_deg: 45.0,
                stops: base_stops,
            },
        ];
        for fill in variants {
            let next = insert_stop_on_fill_ray(&fill, 0.5).expect("gradient fill accepts mid stop");
            let (_, _, stops) = gradient_ray_local(&next).unwrap();
            assert_eq!(stops.len(), 3);
            assert!((stops[1].offset - 0.5).abs() < 1e-6);
            let [r, _g, b, _] = stops[1].color.to_rgba8();
            assert!(
                r > 100 && b > 100,
                "midpoint color interpolated: r={r}, b={b}"
            );
        }
    }
}
