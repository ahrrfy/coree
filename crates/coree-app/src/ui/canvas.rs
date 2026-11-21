// SPDX-License-Identifier: MIT OR Apache-2.0

//! Central Interactive Canvas Viewport, calibrated 20px Rulers (drag-to-guide),
//! dual-click Transform/Rotate/Skew handles with movable pivot crosshair, live
//! `F10` Bézier Node & handle editor, Interactive Fill gradient handles, live
//! drawing/cutting/brush tools, Page Tabs bar, Document Palette strip, and
//! bottom Status Bar with Live Path Length & Area Meter HUD.

use crate::state::{
    bspline_to_bezier_path, compute_free_transform_affine, is_object_editable_in_doc,
    three_point_arc_path, CanvasDragState, DockerTab, NodeDragPart, ParametricHandleKind,
    RasterBrushOp, ScaleHandle, StudioApp, ToolId,
};
use crate::theme::{core_color_to_egui, PAGE_TABS_HEIGHT, RULER_THICKNESS, STATUS_BAR_HEIGHT};
use coree_core::{
    Affine2D, BezierPath, BooleanOp, Color, CommandOp, Effect, EllipseMode, Fill, GradientStop,
    GuideOrientation, LiveSketch, NodeKind, NodePayload, NodeRef, ObjectId, OutlineStyle,
    ParametricShape, Point2D, Rect2D, SceneObject, SmartDrawing, TransformMode, TransparencyMode,
    Vec2D, ZOrderOp,
};
use coree_prod::{simulate_overprint_rgba, LaserOperationKind};
use coree_render::{
    create_smart_fill_object, hit_test_node, hit_test_point, hit_test_rect, SnapEngine,
};
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
            let (
                cursor_txt,
                sel_txt,
                metrics,
                zoom_pct,
                color_mode,
                rec_macro,
                fill_c,
                stroke_c,
                preflight_errs,
            ) = if let Some(tab) = app.active_tab_mut() {
                tab.ensure_metrics_fresh();
                let u = tab.document.default_unit;
                let dpi = f64::from(tab.document.dpi);
                let cx = u.from_mm(tab.cursor_world_mm.x, dpi);
                let cy = u.from_mm(tab.cursor_world_mm.y, dpi);
                let c_txt = format!("({cx:.1}, {cy:.1}) {}", u.suffix());
                let s_txt = if tab.selection.is_empty() {
                    format!(
                        "Tool: {} · {} obj(s)",
                        active_tool_label,
                        tab.document.objects.len()
                    )
                } else if tab.selection.len() == 1 {
                    let oid = tab.selection.objects[0];
                    let name = tab
                        .document
                        .objects
                        .get(&oid)
                        .map(|o| o.name.as_str())
                        .unwrap_or("Object");
                    let mode_lbl = match tab.selection.transform_mode {
                        TransformMode::ScaleAndMove => "Move/Scale",
                        TransformMode::RotateAndSkew => "Rotate/Skew",
                    };
                    format!("Selected: {name} [{mode_lbl}]")
                } else {
                    format!("Selected: {} objects", tab.selection.len())
                };
                let (fc, sc) = tab
                    .selection
                    .objects
                    .first()
                    .and_then(|id| tab.document.objects.get(id))
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
                    tab.last_metrics.clone(),
                    tab.viewport.zoom * 100.0,
                    format!("{:?}", tab.document.color_mode),
                    tab.history.recording_macro,
                    fc,
                    sc,
                    pf_errs,
                )
            } else {
                (
                    "(0.0, 0.0) mm".to_owned(),
                    "No document".to_owned(),
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
                ui.spacing_mut().item_spacing.x = 10.0;
                // Zone 1: Cursor coordinates & selection summary
                ui.label(
                    RichText::new(cursor_txt)
                        .monospace()
                        .size(11.0)
                        .color(tokens.text),
                );
                ui.separator();
                ui.label(RichText::new(sel_txt).size(11.0).color(tokens.text_muted));
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

                    // Active Fill (F11) & Outline (F12) mini swatches
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
                    if fill_resp.on_hover_text("Edit Fill Dialog (F11)").clicked() {
                        app.open_edit_fill_dialog();
                    }

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
                    if stroke_resp
                        .on_hover_text("Outline Pen Dialog (F12)")
                        .clicked()
                    {
                        app.open_outline_pen_dialog();
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
            let mut apply_doc_fill: Option<Color> = None;
            let mut apply_doc_stroke: Option<Color> = None;

            ui.horizontal(|ui| {
                if let Some(tab) = app.active_tab_mut() {
                    let active_idx = tab.document.active_page_index;
                    let page_count = tab.document.pages.len();

                    for idx in 0..page_count {
                        let page_name = tab.document.pages[idx].name.clone();
                        let page_id = tab.document.pages[idx].id;
                        let is_active = idx == active_idx;

                        if tab.inline_renaming_page == Some(idx) {
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut tab.inline_page_name_buf)
                                    .desired_width(88.0),
                            );
                            if resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                let new_name = tab.inline_page_name_buf.trim().to_owned();
                                if !new_name.is_empty() {
                                    let _ = tab.document.rename_page(page_id, new_name);
                                    tab.dirty = true;
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
                            tab.document.active_page_index = idx;
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
                        .selectable_label(tab.document.multipage_view, "⊞ Multipage")
                        .clicked()
                    {
                        tab.document.multipage_view = !tab.document.multipage_view;
                        tab.viewport.multipage_view = tab.document.multipage_view;
                        tab.needs_rerender = true;
                    }

                    // Document Palette Strip
                    ui.separator();
                    ui.label(
                        RichText::new("Doc Colors:")
                            .size(10.5)
                            .color(tokens.text_muted),
                    );
                    for sw in tab.document.document_palette.swatches.iter().take(16) {
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
                                t.document.pages.len() + 1,
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

            // Sync soft-proofing & gamut-warning options into active tab's RenderOptions
            let soft_proof = app.soft_proof_cmyk;
            let gamut_warn = app.gamut_warning;
            let proof_prof = app.proof_profile;
            let proof_int = app.proof_intent;

            {
                let Some(tab) = app.active_tab_mut() else {
                    return;
                };
                if tab.render_options.soft_proof_cmyk != soft_proof
                    || tab.render_options.gamut_warning != gamut_warn
                    || tab.render_options.proof_profile != proof_prof
                    || tab.render_options.proof_intent != proof_int
                {
                    tab.render_options.soft_proof_cmyk = soft_proof;
                    tab.render_options.gamut_warning = gamut_warn;
                    tab.render_options.proof_profile = proof_prof;
                    tab.render_options.proof_intent = proof_int;
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

            // Handle Ruler drag -> Create live Horizontal / Vertical Guidelines!
            handle_ruler_guide_drag(app, canvas_rect, &top_ruler_resp, &left_ruler_resp);

            // Handle Canvas pointer interactions (pan, cursor-centered wheel zoom, all 62 tools!)
            handle_canvas_interaction(ui, app, canvas_rect, &canvas_resp);

            // Right-Click Canvas Context Menu (disabled during Zoom right-click & mid-drag stamp duplicate)
            let allow_context_menu = app.active_tool != ToolId::Zoom
                && app
                    .active_tab()
                    .is_some_and(|t| matches!(t.drag_state, CanvasDragState::Idle));
            if allow_context_menu {
                show_canvas_context_menu(&canvas_resp, app);
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
                tab.drag_state = CanvasDragState::DraggingRulerGuide {
                    horizontal: true,
                    position_mm: w.y,
                };
            }
        }
    } else if top_resp.drag_stopped() {
        let maybe_pos = app.active_tab_mut().and_then(|t| {
            let st = std::mem::take(&mut t.drag_state);
            match st {
                CanvasDragState::DraggingRulerGuide {
                    horizontal: true,
                    position_mm,
                } => Some(position_mm),
                _ => None,
            }
        });
        if let Some(y) = maybe_pos {
            app.add_guide(GuideOrientation::Horizontal(y));
            app.push_toast(format!("Added Horizontal Guide at Y={y:.1} mm"), false);
        }
    }

    if left_resp.drag_started() || left_resp.dragged() {
        if let Some(pos) = left_resp.interact_pointer_pos() {
            if let Some(tab) = app.active_tab_mut() {
                let w = screen_to_world(canvas_rect, tab, pos);
                tab.drag_state = CanvasDragState::DraggingRulerGuide {
                    horizontal: false,
                    position_mm: w.x,
                };
            }
        }
    } else if left_resp.drag_stopped() {
        let maybe_pos = app.active_tab_mut().and_then(|t| {
            let st = std::mem::take(&mut t.drag_state);
            match st {
                CanvasDragState::DraggingRulerGuide {
                    horizontal: false,
                    position_mm,
                } => Some(position_mm),
                _ => None,
            }
        });
        if let Some(x) = maybe_pos {
            app.add_guide(GuideOrientation::Vertical(x));
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
    // 1. Live cursor tracking & cursor-centered mouse wheel zoom
    if let Some(hover) = resp.hover_pos() {
        if let Some(tab) = app.active_tab_mut() {
            tab.cursor_world_mm = screen_to_world(canvas_rect, tab, hover);
        }
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll_y.abs() > 0.5 {
            if let Some(tab) = app.active_tab_mut() {
                let local = screen_to_viewport_local(canvas_rect, tab, hover);
                let factor = if scroll_y > 0.0 { 1.12 } else { 1.0 / 1.12 };
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
        if tab.viewport.lod_dragging && matches!(tab.drag_state, CanvasDragState::Idle) {
            tab.viewport.lod_dragging = false;
            tab.needs_rerender = true;
        }
    }

    let (shift, ctrl, alt, secondary_pressed) = ui.input(|i| {
        (
            i.modifiers.shift,
            i.modifiers.ctrl || i.modifiers.command,
            i.modifiers.alt,
            i.pointer.secondary_pressed(),
        )
    });
    let active_tool = app.active_tool;

    // 3. Double-click handling (Pick -> inline text edit, ShapeNode -> insert/delete node, Crop -> commit, Curves -> finish)
    if resp.double_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            match active_tool {
                ToolId::Pick
                | ToolId::FreehandPick
                | ToolId::ArtisticText
                | ToolId::ParagraphText => {
                    let hit_text = app.active_tab().and_then(|tab| {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        let tol = (6.0 / tab.viewport.logical_px_per_mm()).max(1.2);
                        let oid = app.hit_test_canvas_point(pt, tol)?;
                        let obj = tab.document.objects.get(&oid)?;
                        if matches!(
                            obj.payload,
                            NodePayload::ArtisticText(_) | NodePayload::ParagraphText(_)
                        ) {
                            Some((oid, pt))
                        } else {
                            None
                        }
                    });
                    if let Some((oid, pt)) = hit_text {
                        app.begin_inline_text_edit(Some(oid), pt, false, Vec2D::new(80.0, 20.0));
                        return;
                    }
                }
                ToolId::ShapeNode => {
                    // Double-click existing node -> DELETE node; double-click curve segment -> INSERT node!
                    let deleted_node = app.active_tab().and_then(|tab| {
                        let (nref, part, _) =
                            hit_editable_bezier_node_or_handle(canvas_rect, tab, pos)?;
                        if part == NodeDragPart::Anchor {
                            Some(nref)
                        } else {
                            None
                        }
                    });
                    if let Some(nref) = deleted_node {
                        if let Some(tab) = app.active_tab_mut() {
                            tab.selection.node_selection.select_single(nref);
                        }
                        app.shape_delete_selected_nodes();
                        return;
                    }
                    let seg_hit = app
                        .active_tab()
                        .and_then(|tab| hit_curve_segment_on_selection(canvas_rect, tab, pos));
                    if let Some((oid, sp_idx, seg_idx, t)) = seg_hit {
                        let edited = app.active_tab().and_then(|tab| {
                            let obj = tab.document.objects.get(&oid)?;
                            let NodePayload::Path(ref p) = obj.payload else {
                                return None;
                            };
                            let mut path = p.clone();
                            let ins = path.add_node_at(sp_idx, seg_idx, t).ok()?;
                            Some((path, ins))
                        });
                        if let Some((path, ins)) = edited {
                            if app
                                .execute_command(CommandOp::EditPath { id: oid, path })
                                .is_ok()
                            {
                                if let Some(tab) = app.active_tab_mut() {
                                    tab.selection.node_selection.select_single(NodeRef {
                                        object_id: oid,
                                        subpath_index: sp_idx,
                                        node_index: ins,
                                    });
                                }
                                app.push_toast("Inserted node on curve segment", false);
                                return;
                            }
                        }
                    }
                }
                ToolId::Crop => {
                    let inside_crop = app.active_tab().and_then(|tab| {
                        let crop = tab.pending_crop_rect?;
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        crop.contains_point(pt).then_some(crop)
                    });
                    if let Some(crop) = inside_crop {
                        app.apply_crop_rect(crop);
                        return;
                    }
                }
                ToolId::Bezier | ToolId::Pen => {
                    app.bezier_finish(false);
                    return;
                }
                ToolId::BSpline => {
                    let pts = app
                        .active_tab_mut()
                        .map(|t| std::mem::take(&mut t.poly_points_mm))
                        .unwrap_or_default();
                    app.commit_bspline_points(&pts, false);
                    return;
                }
                ToolId::Polyline => {
                    let pts = app
                        .active_tab_mut()
                        .map(|t| std::mem::take(&mut t.poly_points_mm))
                        .unwrap_or_default();
                    if pts.len() >= 2 {
                        let path = BezierPath::from_polygon(pts, false);
                        let lid = app.active_tab().and_then(|t| t.active_layer_id);
                        let obj =
                            SceneObject::new(ObjectId(0), "Polyline", NodePayload::Path(path))
                                .with_fill(Fill::None)
                                .with_outline(app.default_outline.clone());
                        let _ = app.execute_command(CommandOp::create_object(obj, lid));
                    }
                    return;
                }
                ToolId::InteractiveFill => {
                    let inserted = app.active_tab().and_then(|tab| {
                        let oid = *tab.selection.objects.first()?;
                        if !is_object_editable_in_doc(&tab.document, oid) {
                            return None;
                        }
                        let obj = tab.document.objects.get(&oid)?;
                        let new_fill = try_insert_gradient_stop_on_ray(canvas_rect, tab, obj, pos)?;
                        Some((oid, new_fill))
                    });
                    if let Some((oid, new_fill)) = inserted {
                        if app
                            .execute_command(CommandOp::SetFill {
                                ids: vec![oid],
                                fill: new_fill,
                            })
                            .is_ok()
                        {
                            app.push_toast("Inserted gradient color stop on fill ray", false);
                        }
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    // 4. Single-click handling
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            // Clicking canvas outside an active inline text editor commits it first!
            if app
                .active_tab()
                .is_some_and(|t| t.inline_text_edit.is_some())
            {
                app.commit_inline_text_edit();
            }

            // Finish 3-Point Curve or 3-Point Rect/Ellipse on second-stage click!
            if let Some((p1, p2)) = app.active_tab().and_then(|t| t.three_point_curve_chord) {
                if let Some(p3) = app
                    .active_tab()
                    .map(|t| screen_to_world(canvas_rect, t, pos))
                {
                    app.commit_three_point_curve(p1, p2, p3);
                    return;
                }
            }
            if let Some((shape_tool, p1, p2)) =
                app.active_tab().and_then(|t| t.three_point_shape_chord)
            {
                if let Some(p3) = app
                    .active_tab()
                    .map(|t| screen_to_world(canvas_rect, t, pos))
                {
                    app.commit_three_point_shape(shape_tool, p1, p2, p3);
                    return;
                }
            }

            match active_tool {
                ToolId::Pick | ToolId::FreehandPick | ToolId::FreeTransform => {
                    let pt_tol = app.active_tab().map(|tab| {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        let tol = (6.0 / tab.viewport.logical_px_per_mm()).max(0.8);
                        (pt, tol)
                    });
                    if let Some((pt, tol)) = pt_tol {
                        let hit = app.hit_test_canvas_point(pt, tol);
                        if let Some(tab) = app.active_tab_mut() {
                            if let Some(hit_id) = hit {
                                tab.selection.click_object(&tab.document, hit_id, shift);
                            } else if !shift {
                                tab.selection.clear();
                            }
                            tab.metrics_dirty = true;
                        }
                    }
                }
                ToolId::Zoom => {
                    if let Some(tab) = app.active_tab_mut() {
                        let local = screen_to_viewport_local(canvas_rect, tab, pos);
                        let factor = if shift { 1.0 / 1.5 } else { 1.5 };
                        tab.viewport.zoom_at_screen_point(local, factor);
                        tab.needs_rerender = true;
                    }
                    app.complete_one_shot_zoom_if_active();
                }
                ToolId::ArtisticText => {
                    let info = app.active_tab().map(|t| {
                        let pt = screen_to_world(canvas_rect, t, pos);
                        let tol = (6.0 / t.viewport.logical_px_per_mm()).max(1.2);
                        let existing_text = app.hit_test_canvas_point(pt, tol).filter(|oid| {
                            t.document.objects.get(oid).is_some_and(|o| {
                                matches!(
                                    o.payload,
                                    NodePayload::ArtisticText(_) | NodePayload::ParagraphText(_)
                                )
                            })
                        });
                        (pt, existing_text)
                    });
                    if let Some((pt, existing)) = info {
                        app.begin_inline_text_edit(existing, pt, false, Vec2D::new(90.0, 18.0));
                    }
                }
                ToolId::ParagraphText => {
                    let info = app.active_tab().map(|t| {
                        let pt = screen_to_world(canvas_rect, t, pos);
                        let tol = (6.0 / t.viewport.logical_px_per_mm()).max(1.2);
                        let existing_text = app.hit_test_canvas_point(pt, tol).filter(|oid| {
                            t.document.objects.get(oid).is_some_and(|o| {
                                matches!(
                                    o.payload,
                                    NodePayload::ParagraphText(_) | NodePayload::ArtisticText(_)
                                )
                            })
                        });
                        (pt, existing_text)
                    });
                    if let Some((pt, existing)) = info {
                        app.begin_inline_text_edit(existing, pt, true, Vec2D::new(80.0, 40.0));
                    }
                }
                ToolId::SmartFill => {
                    let created = app.active_tab().and_then(|tab| {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        create_smart_fill_object(
                            &tab.document,
                            tab.document.active_page_index,
                            pt,
                            app.default_fill.clone(),
                            app.default_outline.clone(),
                        )
                    });
                    if let Some(obj) = created {
                        let lid = app.active_tab().and_then(|t| t.active_layer_id);
                        let _ = app.execute_command(CommandOp::create_object(obj, lid));
                        app.push_toast("Created Smart Fill region", false);
                    } else {
                        app.push_toast("Click inside an enclosed boundary for Smart Fill", false);
                    }
                }
                ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
                    if let Some(pt) = app
                        .active_tab()
                        .map(|t| screen_to_world(canvas_rect, t, pos))
                    {
                        app.eyedropper_click(
                            pt,
                            active_tool == ToolId::AttributesEyedropper,
                            shift,
                        );
                    }
                }
                ToolId::VirtualSegmentDelete => {
                    if let Some(pt) = app
                        .active_tab()
                        .map(|t| screen_to_world(canvas_rect, t, pos))
                    {
                        app.apply_virtual_segment_delete_at(pt);
                    }
                }
                ToolId::Bezier | ToolId::Pen => {
                    let click_plan = app.active_tab().map(|tab| {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        let (snapped, _) = SnapEngine::snap_point(
                            &tab.document,
                            tab.document.active_page_index,
                            pt,
                            &[],
                            &tab.snap_options,
                        );
                        let close_first = tab.bezier_draft_nodes.len() >= 2
                            && world_to_screen(canvas_rect, tab, tab.bezier_draft_nodes[0].pos)
                                .distance(pos)
                                <= 8.0;
                        (snapped, close_first)
                    });
                    if let Some((snapped, close_first)) = click_plan {
                        if close_first {
                            app.bezier_finish(true);
                        } else {
                            app.bezier_add_point(snapped, None);
                        }
                    }
                }
                ToolId::BSpline | ToolId::Polyline => {
                    let click_plan = app.active_tab().map(|tab| {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        let (snapped, _) = SnapEngine::snap_point(
                            &tab.document,
                            tab.document.active_page_index,
                            pt,
                            &[],
                            &tab.snap_options,
                        );
                        let close_first = tab.poly_points_mm.len() >= 2
                            && world_to_screen(canvas_rect, tab, tab.poly_points_mm[0])
                                .distance(pos)
                                <= 8.0;
                        (snapped, close_first)
                    });
                    if let Some((snapped, close_first)) = click_plan {
                        if close_first {
                            let pts = app
                                .active_tab_mut()
                                .map(|t| std::mem::take(&mut t.poly_points_mm))
                                .unwrap_or_default();
                            if active_tool == ToolId::BSpline {
                                app.commit_bspline_points(&pts, true);
                            } else {
                                let lid = app.active_tab().and_then(|t| t.active_layer_id);
                                let path = BezierPath::from_polygon(pts, true);
                                let obj = SceneObject::new(
                                    ObjectId(0),
                                    "Polyline",
                                    NodePayload::Path(path),
                                )
                                .with_fill(app.default_fill.clone())
                                .with_outline(app.default_outline.clone());
                                let _ = app.execute_command(CommandOp::create_object(obj, lid));
                            }
                        } else if let Some(tab) = app.active_tab_mut() {
                            tab.poly_points_mm.push(snapped);
                        }
                    }
                }
                ToolId::ShapeNode => {
                    let pt_tol = app.active_tab().map(|tab| {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        let tol = (8.0 / tab.viewport.logical_px_per_mm()).max(1.5);
                        (pt, tol)
                    });
                    if let Some((pt, tol)) = pt_tol {
                        let hit_obj = app.hit_test_canvas_point(pt, tol);
                        if let Some(tab) = app.active_tab_mut() {
                            if let Some(nh) =
                                hit_test_node(&tab.document, &tab.selection.objects, pt, tol)
                            {
                                if shift {
                                    tab.selection.node_selection.toggle(nh.node);
                                } else {
                                    tab.selection.node_selection.select_single(nh.node);
                                }
                            } else if let Some(oid) = hit_obj {
                                tab.selection.select_single(&tab.document, oid);
                                tab.selection.node_selection.clear();
                            }
                        }
                    }
                }
                ToolId::Envelope => {
                    let hit = app.active_tab().and_then(|tab| {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        tab.selection
                            .objects
                            .first()
                            .copied()
                            .or_else(|| app.hit_test_canvas_point(pt, 6.0))
                    });
                    if let Some(oid) = hit {
                        if let Some(tab) = app.active_tab_mut() {
                            if !tab.selection.contains(oid) {
                                tab.selection.select_single(&tab.document, oid);
                            }
                        }
                        app.ensure_envelope_on_object(oid);
                    }
                }
                ToolId::MeshFill => {
                    if let Some(pt) = app
                        .active_tab()
                        .map(|t| screen_to_world(canvas_rect, t, pos))
                    {
                        let recolor = if shift {
                            app.default_fill.referenced_colors().into_iter().next()
                        } else {
                            None
                        };
                        app.mesh_fill_interact(pt, pt, recolor);
                    }
                }
                ToolId::LensTool => {
                    let hit = app.active_tab().and_then(|tab| {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        app.hit_test_canvas_point(pt, 6.0)
                    });
                    if let Some(oid) = hit {
                        if let Some(tab) = app.active_tab_mut() {
                            tab.selection.select_single(&tab.document, oid);
                        }
                        let cfg = app.lens_state.build_config();
                        app.apply_lens_to_selection(Some(cfg));
                    }
                }
                ToolId::BitmapBrush
                | ToolId::BitmapEraser
                | ToolId::CloneStamp
                | ToolId::Smudge => {
                    if let Some(pt) = app
                        .active_tab()
                        .map(|t| screen_to_world(canvas_rect, t, pos))
                    {
                        if alt
                            && (active_tool == ToolId::CloneStamp
                                || (active_tool == ToolId::BitmapBrush
                                    && matches!(
                                        app.tool_settings.raster_op,
                                        RasterBrushOp::CloneStamp | RasterBrushOp::Healing
                                    )))
                        {
                            app.tool_settings.clone_source_mm = Some(pt);
                            app.push_toast(
                                format!("Set sample source at ({:.1}, {:.1}) mm", pt.x, pt.y),
                                false,
                            );
                        } else {
                            app.apply_raster_tool_stroke(active_tool, &[pt]);
                        }
                    }
                }
                ToolId::Smooth
                | ToolId::Smear
                | ToolId::Twirl
                | ToolId::AttractRepel
                | ToolId::Roughen => {
                    if let Some(pt) = app
                        .active_tab()
                        .map(|t| screen_to_world(canvas_rect, t, pos))
                    {
                        app.apply_sculpt_stroke(active_tool, &[pt], shift);
                    }
                }
                _ => {}
            }
        }
    }

    // Right-click zooms out in Zoom tool (+ returns from one-shot F2!)
    if resp.secondary_clicked() && active_tool == ToolId::Zoom {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(tab) = app.active_tab_mut() {
                let local = screen_to_viewport_local(canvas_rect, tab, pos);
                tab.viewport.zoom_at_screen_point(local, 1.0 / 1.5);
                tab.needs_rerender = true;
            }
            app.complete_one_shot_zoom_if_active();
        }
    }

    // 5. Drag start / Dragging / Drag stop
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            start_canvas_drag(app, canvas_rect, pos, active_tool, shift);
        }
    } else if resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            // Left-Drag + Right-Click stamp duplicate while moving/transforming selection!
            if secondary_pressed {
                let can_stamp = app.active_tab().is_some_and(|t| {
                    matches!(
                        &t.drag_state,
                        CanvasDragState::MovingSelection { .. }
                            | CanvasDragState::ScalingSelection { .. }
                            | CanvasDragState::RotatingSelection { .. }
                            | CanvasDragState::SkewingSelection { .. }
                            | CanvasDragState::FreeTransforming { .. }
                    )
                });
                if can_stamp {
                    if let Ok(created) = app.stamp_duplicate_during_active_drag() {
                        if !created.is_empty() {
                            app.push_toast("Stamped duplicate at cursor", false);
                        }
                    }
                }
            }
            update_canvas_drag(app, canvas_rect, pos, active_tool, ctrl, shift);
        }
    } else if resp.drag_stopped() {
        finish_canvas_drag(app, active_tool, ctrl, shift);
    }
}

fn capture_selected_transforms(tab: &crate::state::DocumentTab) -> Vec<(ObjectId, Affine2D)> {
    tab.selection
        .objects
        .iter()
        .copied()
        .filter_map(|id| {
            if !is_object_editable_in_doc(&tab.document, id) {
                return None;
            }
            let obj = tab.document.objects.get(&id)?;
            Some((id, obj.transform))
        })
        .collect()
}

fn start_canvas_drag(
    app: &mut StudioApp,
    canvas_rect: Rect,
    screen_pos: Pos2,
    tool: ToolId,
    shift: bool,
) {
    // If stage 2 of a 3-Point tool is waiting for P3, commit it cleanly even if the pointer moves slightly
    if let Some((p1, p2)) = app.active_tab().and_then(|t| t.three_point_curve_chord) {
        if let Some(p3) = app
            .active_tab()
            .map(|t| screen_to_world(canvas_rect, t, screen_pos))
        {
            app.commit_three_point_curve(p1, p2, p3);
            return;
        }
    }
    if let Some((shape_tool, p1, p2)) = app.active_tab().and_then(|t| t.three_point_shape_chord) {
        if let Some(p3) = app
            .active_tab()
            .map(|t| screen_to_world(canvas_rect, t, screen_pos))
        {
            app.commit_three_point_shape(shape_tool, p1, p2, p3);
            return;
        }
    }

    let treat_filled = app.treat_all_objects_as_filled;
    let sculpt_r = app.tool_settings.sculpt_radius_mm.max(2.0);
    let Some(tab) = app.active_tab_mut() else {
        return;
    };
    let pt = screen_to_world(canvas_rect, tab, screen_pos);
    let tol_mm = (7.0 / tab.viewport.logical_px_per_mm()).max(1.2);

    match tool {
        ToolId::Pan => {
            tab.drag_state = CanvasDragState::Panning {
                start_screen: [screen_pos.x, screen_pos.y],
                orig_pan_mm: tab.viewport.pan_mm,
            };
            tab.viewport.lod_dragging = true;
        }
        ToolId::Zoom => {
            tab.drag_state = CanvasDragState::ZoomMarquee {
                start_mm: pt,
                current_mm: pt,
            };
        }
        ToolId::FreehandPick => {
            tab.drag_state = CanvasDragState::LassoSelect {
                points_mm: vec![pt],
            };
        }
        ToolId::FreeTransform => {
            if tab.selection.is_empty() {
                if let Some(hit_id) =
                    hit_test_point(&tab.document, tab.document.active_page_index, pt, tol_mm)
                {
                    tab.selection.select_single(&tab.document, hit_id);
                }
            }
            let orig_transforms = capture_selected_transforms(tab);
            if !orig_transforms.is_empty() {
                let anchor = tab
                    .selection
                    .bounding_box(&tab.document)
                    .map_or(pt, |b| b.center());
                tab.drag_state = CanvasDragState::FreeTransforming {
                    anchor_mm: anchor,
                    start_mm: pt,
                    current_mm: pt,
                    orig_transforms,
                    last_affine: Affine2D::IDENTITY,
                };
                tab.viewport.lod_dragging = true;
            }
        }
        ToolId::Pick => {
            // 1. Check active selection handles (Scale or Rotate/Skew + Movable Pivot)
            if let Some(bb) = tab.selection.bounding_box(&tab.document) {
                match tab.selection.transform_mode {
                    TransformMode::ScaleAndMove => {
                        if let Some(handle) = hit_scale_handle(canvas_rect, tab, bb, screen_pos) {
                            let orig_transforms = capture_selected_transforms(tab);
                            tab.drag_state = CanvasDragState::ScalingSelection {
                                handle,
                                orig_bounds: bb,
                                current_mm: pt,
                                orig_transforms,
                                last_affine: Affine2D::IDENTITY,
                            };
                            tab.viewport.lod_dragging = true;
                            return;
                        }
                    }
                    TransformMode::RotateAndSkew => {
                        let pivot_screen = world_to_screen(canvas_rect, tab, tab.selection.pivot);
                        if pivot_screen.distance(screen_pos) <= 9.0 {
                            tab.drag_state = CanvasDragState::DraggingPivot;
                            return;
                        }
                        if let Some(handle) = hit_scale_handle(canvas_rect, tab, bb, screen_pos) {
                            let orig_transforms = capture_selected_transforms(tab);
                            let is_corner = matches!(
                                handle,
                                ScaleHandle::TopLeft
                                    | ScaleHandle::TopRight
                                    | ScaleHandle::BottomLeft
                                    | ScaleHandle::BottomRight
                            );
                            if is_corner {
                                let pv = tab.selection.pivot;
                                let ang = (pt.y - pv.y).atan2(pt.x - pv.x);
                                tab.drag_state = CanvasDragState::RotatingSelection {
                                    pivot_mm: pv,
                                    start_angle_rad: ang,
                                    current_angle_rad: ang,
                                    orig_transforms,
                                    last_affine: Affine2D::IDENTITY,
                                };
                                tab.viewport.lod_dragging = true;
                                return;
                            } else {
                                let horiz =
                                    matches!(handle, ScaleHandle::Top | ScaleHandle::Bottom);
                                tab.drag_state = CanvasDragState::SkewingSelection {
                                    horizontal: horiz,
                                    pivot_mm: tab.selection.pivot,
                                    start_mm: pt,
                                    current_mm: pt,
                                    orig_transforms,
                                    last_affine: Affine2D::IDENTITY,
                                };
                                tab.viewport.lod_dragging = true;
                                return;
                            }
                        }
                    }
                }
            }

            // 2. Hit object -> Move selection
            let hit = hit_test_point(&tab.document, tab.document.active_page_index, pt, tol_mm)
                .or_else(|| {
                    if treat_filled {
                        tab.document.active_page().and_then(|page| {
                            page.layers.iter().rev().find_map(|layer| {
                                if !layer.visible || layer.locked {
                                    return None;
                                }
                                layer.objects.iter().rev().copied().find(|&oid| {
                                    tab.document
                                        .objects
                                        .get(&oid)
                                        .is_some_and(|o| o.visible && !o.locked)
                                        && tab
                                            .document
                                            .world_path_of(oid)
                                            .is_some_and(|wp| wp.contains_point(pt))
                                })
                            })
                        })
                    } else {
                        None
                    }
                });

            if let Some(hit_id) = hit {
                tab.selected_guide_id = None;
                if !tab.selection.contains(hit_id) {
                    if shift {
                        tab.selection.toggle_object(&tab.document, hit_id);
                    } else {
                        tab.selection.select_single(&tab.document, hit_id);
                    }
                }
                tab.drag_state = CanvasDragState::MovingSelection {
                    start_mm: pt,
                    last_mm: pt,
                    accum_dx: 0.0,
                    accum_dy: 0.0,
                };
                tab.viewport.lod_dragging = true;
            } else if let Some((guide_id, orig_orientation, locked)) =
                hit_test_canvas_guide(&tab.document, pt, tol_mm.max(1.8))
            {
                tab.selection.clear();
                tab.selected_guide_id = Some(guide_id);
                if !locked {
                    tab.drag_state = CanvasDragState::DraggingCanvasGuide {
                        guide_id,
                        orig_orientation,
                    };
                }
            } else {
                tab.selected_guide_id = None;
                // 3. Empty pasteboard -> Marquee rubber-band selection
                tab.drag_state = CanvasDragState::MarqueeSelect {
                    start_mm: pt,
                    current_mm: pt,
                };
            }
        }
        ToolId::ShapeNode => {
            // 1. Check live ParametricShape handles (Rectangle corners, Ellipse Arc/Pie, Polygon, Star)
            if let Some((oid, handle, orig_shape)) =
                hit_parametric_shape_handle(canvas_rect, tab, screen_pos)
            {
                tab.drag_state = CanvasDragState::DraggingParametricHandle {
                    object_id: oid,
                    handle,
                    orig_shape,
                };
                tab.viewport.lod_dragging = true;
                return;
            }
            // 2. Check Bezier control handles & anchor nodes (supporting multi-node drag!)
            if let Some((nref, part, _)) =
                hit_editable_bezier_node_or_handle(canvas_rect, tab, screen_pos)
            {
                if part != NodeDragPart::Anchor || !tab.selection.node_selection.contains(nref) {
                    if shift {
                        tab.selection.node_selection.toggle(nref);
                    } else {
                        tab.selection.node_selection.select_single(nref);
                    }
                }
                let mut seen = std::collections::HashSet::new();
                let mut orig_paths = Vec::new();
                for nr in &tab.selection.node_selection.nodes {
                    if seen.insert(nr.object_id) {
                        if let Some(obj) = tab.document.objects.get(&nr.object_id) {
                            if let NodePayload::Path(ref p) = obj.payload {
                                orig_paths.push((nr.object_id, p.clone()));
                            }
                        }
                    }
                }
                if seen.insert(nref.object_id) {
                    if let Some(obj) = tab.document.objects.get(&nref.object_id) {
                        if let NodePayload::Path(ref p) = obj.payload {
                            orig_paths.push((nref.object_id, p.clone()));
                        }
                    }
                }
                tab.drag_state = CanvasDragState::DraggingNode {
                    node: nref,
                    handle_part: part,
                    orig_paths,
                    last_world_mm: pt,
                };
                tab.viewport.lod_dragging = true;
                return;
            }
            // 3. Check direct curve segment dragging to bend curves interactively!
            if let Some((oid, sp_idx, seg_idx, t)) =
                hit_curve_segment_on_selection(canvas_rect, tab, screen_pos)
            {
                if let Some(obj) = tab.document.objects.get(&oid) {
                    if let NodePayload::Path(ref orig_path) = obj.payload {
                        let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                        let local_pt = inv.transform_point(pt);
                        tab.drag_state = CanvasDragState::DraggingCurveSegment {
                            object_id: oid,
                            subpath_index: sp_idx,
                            seg_index: seg_idx,
                            t,
                            last_local_mm: local_pt,
                            orig_path: orig_path.clone(),
                        };
                        tab.viewport.lod_dragging = true;
                        return;
                    }
                }
            }
            // 4. Empty space -> Marquee-select multiple Bezier nodes!
            tab.drag_state = CanvasDragState::NodeMarqueeSelect {
                start_mm: pt,
                current_mm: pt,
            };
        }
        ToolId::Smooth | ToolId::Smear | ToolId::Twirl | ToolId::AttractRepel | ToolId::Roughen => {
            let target = tab
                .selection
                .objects
                .iter()
                .copied()
                .find(|&id| {
                    is_object_editable_in_doc(&tab.document, id)
                        && tab.document.objects.get(&id).is_some_and(|o| {
                            matches!(o.payload, NodePayload::Path(_) | NodePayload::Shape(_))
                        })
                })
                .or_else(|| {
                    hit_test_point(&tab.document, tab.document.active_page_index, pt, sculpt_r)
                        .filter(|&id| {
                            is_object_editable_in_doc(&tab.document, id)
                                && tab.document.objects.get(&id).is_some_and(|o| {
                                    matches!(
                                        o.payload,
                                        NodePayload::Path(_) | NodePayload::Shape(_)
                                    )
                                })
                        })
                });
            if let Some(oid) = target {
                if let Some(obj) = tab.document.objects.get_mut(&oid) {
                    let orig_payload = obj.payload.clone();
                    if let NodePayload::Shape(ref s) = obj.payload {
                        obj.payload = NodePayload::Path(s.to_bezier_path());
                    }
                    tab.selection.select_single(&tab.document, oid);
                    tab.drag_state = CanvasDragState::Sculpting {
                        object_id: oid,
                        orig_payload,
                        last_mm: pt,
                        points_mm: vec![pt],
                        step_counter: 0,
                    };
                    tab.viewport.lod_dragging = true;
                }
            }
        }
        ToolId::Crop => {
            if let Some(existing_crop) = tab.pending_crop_rect {
                if let Some(handle) = hit_scale_handle(canvas_rect, tab, existing_crop, screen_pos)
                {
                    tab.drag_state = CanvasDragState::CropBoxHandle {
                        handle,
                        orig_rect: existing_crop,
                    };
                    return;
                }
            }
            tab.drag_state = CanvasDragState::DrawingBox {
                start_mm: pt,
                current_mm: pt,
            };
        }
        ToolId::Bezier | ToolId::Pen => {
            let (snapped, _) = SnapEngine::snap_point(
                &tab.document,
                tab.document.active_page_index,
                pt,
                &[],
                &tab.snap_options,
            );
            let closing = tab.bezier_draft_nodes.len() >= 2
                && world_to_screen(canvas_rect, tab, tab.bezier_draft_nodes[0].pos)
                    .distance(screen_pos)
                    <= 8.0;
            let anchor = if closing {
                tab.bezier_draft_nodes[0].pos
            } else {
                snapped
            };
            tab.drag_state = CanvasDragState::PullingBezierHandle {
                anchor_mm: anchor,
                handle_out_mm: anchor,
                closing,
            };
        }
        ToolId::InteractiveFill => {
            if let Some(&oid) = tab.selection.objects.first() {
                if is_object_editable_in_doc(&tab.document, oid) {
                    if let Some(obj) = tab.document.objects.get(&oid) {
                        if let Fill::RadialGradient { center, focal, .. } = &obj.fill {
                            let f_pos = world_to_screen(
                                canvas_rect,
                                tab,
                                obj.transform.transform_point(*focal),
                            );
                            if (*focal - *center).length() > 0.2
                                && f_pos.distance(screen_pos) <= 10.0
                            {
                                tab.drag_state = CanvasDragState::DraggingGradientFocal {
                                    object_id: oid,
                                    orig_fill: obj.fill.clone(),
                                };
                                return;
                            }
                        }
                        if let Some((start_l, end_l, stops)) = gradient_ray_local(&obj.fill) {
                            let s_pos = world_to_screen(
                                canvas_rect,
                                tab,
                                obj.transform.transform_point(start_l),
                            );
                            let e_pos = world_to_screen(
                                canvas_rect,
                                tab,
                                obj.transform.transform_point(end_l),
                            );
                            if s_pos.distance(screen_pos) <= 10.0 {
                                if shift && matches!(obj.fill, Fill::RadialGradient { .. }) {
                                    tab.drag_state = CanvasDragState::DraggingGradientFocal {
                                        object_id: oid,
                                        orig_fill: obj.fill.clone(),
                                    };
                                } else {
                                    tab.drag_state = CanvasDragState::DraggingGradientHandle {
                                        object_id: oid,
                                        is_end: false,
                                        orig_fill: obj.fill.clone(),
                                    };
                                }
                                return;
                            }
                            if e_pos.distance(screen_pos) <= 10.0 {
                                tab.drag_state = CanvasDragState::DraggingGradientHandle {
                                    object_id: oid,
                                    is_end: true,
                                    orig_fill: obj.fill.clone(),
                                };
                                return;
                            }
                            for (s_idx, st) in stops.iter().enumerate() {
                                if s_idx == 0 || s_idx + 1 == stops.len() {
                                    continue;
                                }
                                let t = f64::from(st.offset);
                                let w_pt = obj.transform.transform_point(Point2D::new(
                                    start_l.x + (end_l.x - start_l.x) * t,
                                    start_l.y + (end_l.y - start_l.y) * t,
                                ));
                                if world_to_screen(canvas_rect, tab, w_pt).distance(screen_pos)
                                    <= 9.0
                                {
                                    tab.drag_state = CanvasDragState::DraggingGradientStop {
                                        object_id: oid,
                                        stop_index: s_idx,
                                        orig_fill: obj.fill.clone(),
                                    };
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            tab.drag_state = CanvasDragState::DrawingBox {
                start_mm: pt,
                current_mm: pt,
            };
        }
        ToolId::Transparency => {
            if let Some(&oid) = tab.selection.objects.first() {
                if is_object_editable_in_doc(&tab.document, oid) {
                    if let Some(obj) = tab.document.objects.get(&oid) {
                        for (eff_idx, eff) in obj.effects.iter().enumerate() {
                            if let Effect::Transparency {
                                mode: TransparencyMode::FountainLinear { start, end, .. },
                                ..
                            } = eff
                            {
                                let s_pos = world_to_screen(
                                    canvas_rect,
                                    tab,
                                    obj.transform.transform_point(*start),
                                );
                                let e_pos = world_to_screen(
                                    canvas_rect,
                                    tab,
                                    obj.transform.transform_point(*end),
                                );
                                if s_pos.distance(screen_pos) <= 10.0 {
                                    tab.drag_state = CanvasDragState::DraggingTransparencyHandle {
                                        object_id: oid,
                                        effect_index: eff_idx,
                                        is_end: false,
                                        orig_effect: eff.clone(),
                                    };
                                    return;
                                }
                                if e_pos.distance(screen_pos) <= 10.0 {
                                    tab.drag_state = CanvasDragState::DraggingTransparencyHandle {
                                        object_id: oid,
                                        effect_index: eff_idx,
                                        is_end: true,
                                        orig_effect: eff.clone(),
                                    };
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            let target = if !tab.selection.objects.is_empty() {
                tab.selection
                    .objects
                    .iter()
                    .copied()
                    .find(|&id| is_object_editable_in_doc(&tab.document, id))
            } else {
                hit_test_point(&tab.document, tab.document.active_page_index, pt, tol_mm)
                    .filter(|&id| is_object_editable_in_doc(&tab.document, id))
            };
            if let Some(oid) = target {
                tab.drag_state = CanvasDragState::DraggingInteractiveEffect {
                    object_id: oid,
                    start_mm: pt,
                    current_mm: pt,
                };
            }
        }
        ToolId::Envelope => {
            // Check 8-point Coons cage handles on selected object first!
            if let Some(&oid) = tab.selection.objects.first() {
                if is_object_editable_in_doc(&tab.document, oid) {
                    if let Some(obj) = tab.document.objects.get(&oid) {
                        for (eff_idx, eff) in obj.effects.iter().enumerate() {
                            if let Effect::Envelope { cage, .. } = eff {
                                for (h_idx, &c_pt) in cage.iter().enumerate() {
                                    let sp = world_to_screen(
                                        canvas_rect,
                                        tab,
                                        obj.transform.transform_point(c_pt),
                                    );
                                    if sp.distance(screen_pos) <= 9.0 {
                                        tab.drag_state = CanvasDragState::DraggingEnvelopeHandle {
                                            object_id: oid,
                                            effect_index: eff_idx,
                                            handle_index: h_idx,
                                            orig_effect: eff.clone(),
                                        };
                                        tab.viewport.lod_dragging = true;
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let target = if !tab.selection.objects.is_empty() {
                tab.selection
                    .objects
                    .iter()
                    .copied()
                    .find(|&id| is_object_editable_in_doc(&tab.document, id))
            } else {
                hit_test_point(&tab.document, tab.document.active_page_index, pt, tol_mm)
                    .filter(|&id| is_object_editable_in_doc(&tab.document, id))
            };
            if let Some(oid) = target {
                tab.drag_state = CanvasDragState::DraggingInteractiveEffect {
                    object_id: oid,
                    start_mm: pt,
                    current_mm: pt,
                };
            }
        }
        ToolId::DropShadow
        | ToolId::BlockShadow
        | ToolId::Contour
        | ToolId::Blend
        | ToolId::Distort
        | ToolId::Extrude25D => {
            // Check persistent interactive effect handles on active selected object first!
            if let Some(&oid) = tab.selection.objects.first() {
                if is_object_editable_in_doc(&tab.document, oid) {
                    if let Some(obj) = tab.document.objects.get(&oid) {
                        for (eff_idx, eff) in obj.effects.iter().enumerate() {
                            if let Some((_anchor, knob_world)) =
                                interactive_effect_handle_points(&tab.document, obj, eff, tool)
                            {
                                let kp = world_to_screen(canvas_rect, tab, knob_world);
                                if kp.distance(screen_pos) <= 10.0 {
                                    tab.drag_state = CanvasDragState::DraggingEffectHandle {
                                        object_id: oid,
                                        effect_index: eff_idx,
                                        orig_effect: eff.clone(),
                                    };
                                    tab.viewport.lod_dragging = true;
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            let target = if !tab.selection.objects.is_empty() {
                tab.selection
                    .objects
                    .iter()
                    .copied()
                    .find(|&id| is_object_editable_in_doc(&tab.document, id))
            } else {
                hit_test_point(&tab.document, tab.document.active_page_index, pt, tol_mm)
                    .filter(|&id| is_object_editable_in_doc(&tab.document, id))
            };
            if let Some(oid) = target {
                if !tab.selection.contains(oid) {
                    tab.selection.select_single(&tab.document, oid);
                }
                tab.drag_state = CanvasDragState::DraggingInteractiveEffect {
                    object_id: oid,
                    start_mm: pt,
                    current_mm: pt,
                };
            }
        }
        ToolId::MeshFill => {
            // Check existing MeshFill node handles first!
            if let Some(&oid) = tab.selection.objects.first() {
                if is_object_editable_in_doc(&tab.document, oid) {
                    if let Some(obj) = tab.document.objects.get(&oid) {
                        if let Fill::MeshFill { nodes, .. } = &obj.fill {
                            for (n_idx, mnode) in nodes.iter().enumerate() {
                                let sp = world_to_screen(
                                    canvas_rect,
                                    tab,
                                    obj.transform.transform_point(mnode.pos),
                                );
                                if sp.distance(screen_pos) <= 9.0 {
                                    tab.selected_mesh_node = Some((oid, n_idx));
                                    tab.drag_state = CanvasDragState::DraggingMeshNode {
                                        object_id: oid,
                                        node_index: n_idx,
                                        orig_fill: obj.fill.clone(),
                                    };
                                    tab.viewport.lod_dragging = true;
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            tab.drag_state = CanvasDragState::DrawingBox {
                start_mm: pt,
                current_mm: pt,
            };
        }
        ToolId::Freehand
        | ToolId::TwoPointLine
        | ToolId::SmartDrawing
        | ToolId::LiveSketch
        | ToolId::ArtisticMedia
        | ToolId::Knife
        | ToolId::VirtualSegmentDelete
        | ToolId::Eraser
        | ToolId::BitmapBrush
        | ToolId::BitmapEraser
        | ToolId::CloneStamp
        | ToolId::Smudge => {
            tab.drag_state = CanvasDragState::DrawingStroke {
                points_mm: vec![pt],
            };
        }
        _ => {
            let (snapped, guides) = SnapEngine::snap_point(
                &tab.document,
                tab.document.active_page_index,
                pt,
                &[],
                &tab.snap_options,
            );
            tab.active_guides = guides;
            tab.drag_state = CanvasDragState::DrawingBox {
                start_mm: snapped,
                current_mm: snapped,
            };
        }
    }
}

fn update_canvas_drag(
    app: &mut StudioApp,
    canvas_rect: Rect,
    screen_pos: Pos2,
    active_tool: ToolId,
    ctrl: bool,
    shift: bool,
) {
    let sculpt_radius = app.tool_settings.sculpt_radius_mm.clamp(1.0, 200.0);
    let sculpt_strength = app.tool_settings.sculpt_strength.clamp(0.05, 1.0);
    let Some(tab) = app.active_tab_mut() else {
        return;
    };
    let pt = screen_to_world(canvas_rect, tab, screen_pos);

    let mut state = std::mem::take(&mut tab.drag_state);
    match &mut state {
        CanvasDragState::Panning {
            start_screen,
            orig_pan_mm,
        } => {
            let s = tab.viewport.logical_px_per_mm().max(1e-4);
            let dx_px = f64::from(screen_pos.x - start_screen[0]);
            let dy_px = f64::from(screen_pos.y - start_screen[1]);
            tab.viewport.pan_mm =
                Point2D::new(orig_pan_mm.x - dx_px / s, orig_pan_mm.y - dy_px / s);
            tab.needs_rerender = true;
        }
        CanvasDragState::MarqueeSelect { current_mm, .. }
        | CanvasDragState::NodeMarqueeSelect { current_mm, .. }
        | CanvasDragState::ZoomMarquee { current_mm, .. }
        | CanvasDragState::DraggingInteractiveEffect { current_mm, .. } => {
            *current_mm = pt;
        }
        CanvasDragState::LassoSelect { points_mm } => {
            if points_mm
                .last()
                .is_none_or(|last| last.distance_to(pt) > 0.6)
            {
                points_mm.push(pt);
            }
        }
        CanvasDragState::MovingSelection {
            start_mm,
            last_mm,
            accum_dx,
            accum_dy,
        } => {
            let raw_pt = if ctrl {
                let d = pt - *start_mm;
                if d.x.abs() >= d.y.abs() {
                    Point2D::new(pt.x, start_mm.y)
                } else {
                    Point2D::new(start_mm.x, pt.y)
                }
            } else {
                pt
            };
            let (snapped, guides) = SnapEngine::snap_point(
                &tab.document,
                tab.document.active_page_index,
                raw_pt,
                &tab.selection.objects,
                &tab.snap_options,
            );
            tab.active_guides = guides;
            let dx = snapped.x - last_mm.x;
            let dy = snapped.y - last_mm.y;
            if dx.abs() > 1e-6 || dy.abs() > 1e-6 {
                let delta = Affine2D::translate(dx, dy);
                let editable: Vec<ObjectId> = tab
                    .selection
                    .objects
                    .iter()
                    .copied()
                    .filter(|&id| is_object_editable_in_doc(&tab.document, id))
                    .collect();
                for id in editable {
                    if let Some(obj) = tab.document.objects.get_mut(&id) {
                        obj.transform = delta.compose(&obj.transform);
                    }
                }
                *accum_dx += dx;
                *accum_dy += dy;
                *last_mm = snapped;
                tab.tile_cache.mark_all_dirty();
                tab.needs_rerender = true;
            }
        }
        CanvasDragState::ScalingSelection {
            handle,
            orig_bounds,
            current_mm,
            orig_transforms,
            last_affine,
        } => {
            *current_mm = pt;
            let affine = compute_scale_affine(*handle, *orig_bounds, pt, ctrl, shift)
                .unwrap_or(Affine2D::IDENTITY);
            *last_affine = affine;
            for &(id, orig_xf) in orig_transforms.iter() {
                if let Some(obj) = tab.document.objects.get_mut(&id) {
                    obj.transform = affine.compose(&orig_xf);
                }
            }
            tab.tile_cache.mark_all_dirty();
            tab.needs_rerender = true;
        }
        CanvasDragState::RotatingSelection {
            pivot_mm,
            start_angle_rad,
            current_angle_rad,
            orig_transforms,
            last_affine,
        } => {
            let mut delta = (pt.y - pivot_mm.y).atan2(pt.x - pivot_mm.x) - *start_angle_rad;
            if ctrl {
                let step = std::f64::consts::PI / 12.0; // 15° snap
                delta = (delta / step).round() * step;
            }
            *current_angle_rad = *start_angle_rad + delta;
            let affine = Affine2D::rotate_about(delta, *pivot_mm);
            *last_affine = affine;
            for &(id, orig_xf) in orig_transforms.iter() {
                if let Some(obj) = tab.document.objects.get_mut(&id) {
                    obj.transform = affine.compose(&orig_xf);
                }
            }
            tab.tile_cache.mark_all_dirty();
            tab.needs_rerender = true;
        }
        CanvasDragState::SkewingSelection {
            horizontal,
            pivot_mm,
            start_mm,
            current_mm,
            orig_transforms,
            last_affine,
        } => {
            *current_mm = pt;
            let (sx, sy) = if *horizontal {
                (((pt.x - start_mm.x) / 60.0).clamp(-1.2, 1.2), 0.0)
            } else {
                (0.0, ((pt.y - start_mm.y) / 60.0).clamp(-1.2, 1.2))
            };
            let affine = Affine2D::translate(pivot_mm.x, pivot_mm.y)
                .compose(&Affine2D::skew(sx, sy))
                .compose(&Affine2D::translate(-pivot_mm.x, -pivot_mm.y));
            *last_affine = affine;
            for &(id, orig_xf) in orig_transforms.iter() {
                if let Some(obj) = tab.document.objects.get_mut(&id) {
                    obj.transform = affine.compose(&orig_xf);
                }
            }
            tab.tile_cache.mark_all_dirty();
            tab.needs_rerender = true;
        }
        CanvasDragState::FreeTransforming {
            anchor_mm,
            start_mm,
            current_mm,
            orig_transforms,
            last_affine,
        } => {
            *current_mm = pt;
            let affine = compute_free_transform_affine(*anchor_mm, *start_mm, pt, ctrl, shift)
                .unwrap_or(Affine2D::IDENTITY);
            *last_affine = affine;
            for &(id, orig_xf) in orig_transforms.iter() {
                if let Some(obj) = tab.document.objects.get_mut(&id) {
                    obj.transform = affine.compose(&orig_xf);
                }
            }
            tab.tile_cache.mark_all_dirty();
            tab.needs_rerender = true;
        }
        CanvasDragState::DraggingPivot => {
            tab.selection.set_pivot(pt);
        }
        CanvasDragState::DraggingNode {
            node,
            handle_part,
            last_world_mm,
            ..
        } => {
            let world_delta = pt - *last_world_mm;
            *last_world_mm = pt;
            match handle_part {
                NodeDragPart::Anchor => {
                    // Move ALL selected nodes together!
                    let targets = if tab.selection.node_selection.nodes.is_empty() {
                        vec![*node]
                    } else {
                        tab.selection.node_selection.nodes.clone()
                    };
                    for nref in targets {
                        if let Some(obj) = tab.document.objects.get_mut(&nref.object_id) {
                            let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                            let local_delta = Vec2D::new(
                                inv.m[0] * world_delta.x + inv.m[2] * world_delta.y,
                                inv.m[1] * world_delta.x + inv.m[3] * world_delta.y,
                            );
                            if let NodePayload::Path(ref mut path) = obj.payload {
                                if let Some(sp) = path.subpaths.get_mut(nref.subpath_index) {
                                    if let Some(pnode) = sp.nodes.get_mut(nref.node_index) {
                                        pnode.translate(local_delta);
                                    }
                                }
                            }
                        }
                    }
                    tab.tile_cache.mark_all_dirty();
                    tab.needs_rerender = true;
                }
                NodeDragPart::HandleIn | NodeDragPart::HandleOut => {
                    if let Some(obj) = tab.document.objects.get_mut(&node.object_id) {
                        let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                        let local_pt = inv.transform_point(pt);
                        if let NodePayload::Path(ref mut path) = obj.payload {
                            if let Some(sp) = path.subpaths.get_mut(node.subpath_index) {
                                if let Some(pnode) = sp.nodes.get_mut(node.node_index) {
                                    if *handle_part == NodeDragPart::HandleIn {
                                        pnode.handle_in = Some(local_pt);
                                    } else {
                                        pnode.handle_out = Some(local_pt);
                                    }
                                    pnode.enforce_continuity();
                                }
                            }
                        }
                        tab.tile_cache.mark_all_dirty();
                        tab.needs_rerender = true;
                    }
                }
            }
        }
        CanvasDragState::DraggingCurveSegment {
            object_id,
            subpath_index,
            seg_index,
            t,
            last_local_mm,
            ..
        } => {
            if let Some(obj) = tab.document.objects.get_mut(object_id) {
                let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                let local_pt = inv.transform_point(pt);
                let delta = local_pt - *last_local_mm;
                *last_local_mm = local_pt;
                if let NodePayload::Path(ref mut path) = obj.payload {
                    let _ = path.convert_segment_to_curve(*subpath_index, *seg_index);
                    if let Some(sp) = path.subpaths.get_mut(*subpath_index) {
                        if let Some((i0, i1)) = sp.segment_node_indices(*seg_index) {
                            let w0 = (1.0 - *t).clamp(0.15, 0.85) * 1.45;
                            let w1 = (*t).clamp(0.15, 0.85) * 1.45;
                            if let Some(h0) = sp.nodes[i0].handle_out.as_mut() {
                                *h0 = *h0 + delta * w0;
                            }
                            if let Some(h1) = sp.nodes[i1].handle_in.as_mut() {
                                *h1 = *h1 + delta * w1;
                            }
                        }
                    }
                }
                tab.tile_cache.mark_all_dirty();
                tab.needs_rerender = true;
            }
        }
        CanvasDragState::DraggingParametricHandle {
            object_id, handle, ..
        } => {
            if let Some(obj) = tab.document.objects.get_mut(object_id) {
                let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                let lp = inv.transform_point(pt);
                if let NodePayload::Shape(ref mut shape) = obj.payload {
                    update_parametric_shape_live(shape, *handle, lp, ctrl, shift);
                    tab.tile_cache.mark_all_dirty();
                    tab.needs_rerender = true;
                }
            }
        }
        CanvasDragState::Sculpting {
            object_id,
            last_mm,
            points_mm,
            step_counter,
            ..
        } => {
            if last_mm.distance_to(pt) >= 0.35 {
                if let Some(obj) = tab.document.objects.get_mut(object_id) {
                    let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                    let scale_est =
                        ((inv.m[0].hypot(inv.m[1]) + inv.m[2].hypot(inv.m[3])) * 0.5).max(1e-3);
                    let local_r = sculpt_radius * scale_est;
                    let lp = inv.transform_point(pt);
                    let prev_lp = inv.transform_point(*last_mm);
                    if let NodePayload::Path(ref mut path) = obj.payload {
                        match active_tool {
                            ToolId::Smooth => path.smooth_near(lp, local_r, sculpt_strength),
                            ToolId::Smear => path.smear_near(lp, lp - prev_lp, local_r),
                            ToolId::Twirl => {
                                let sign = if shift { -1.0 } else { 1.0 };
                                path.twirl_near(
                                    lp,
                                    local_r,
                                    sign * (0.25 * sculpt_strength + 0.08),
                                );
                            }
                            ToolId::AttractRepel => {
                                path.attract_repel_near(lp, local_r, sculpt_strength, !shift);
                            }
                            ToolId::Roughen => {
                                let amp = (local_r * 0.16 * sculpt_strength).max(0.35);
                                path.roughen_near(
                                    lp,
                                    local_r,
                                    amp,
                                    0x51A7_u64.wrapping_add(*step_counter),
                                );
                            }
                            _ => {}
                        }
                        *step_counter = step_counter.saturating_add(1);
                    }
                    tab.tile_cache.mark_all_dirty();
                    tab.needs_rerender = true;
                }
                *last_mm = pt;
                points_mm.push(pt);
            }
        }
        CanvasDragState::CropBoxHandle { handle, orig_rect } => {
            let mut r = *orig_rect;
            match handle {
                ScaleHandle::TopLeft => {
                    r.min_x = pt.x.min(r.max_x - 2.0);
                    r.min_y = pt.y.min(r.max_y - 2.0);
                }
                ScaleHandle::Top => r.min_y = pt.y.min(r.max_y - 2.0),
                ScaleHandle::TopRight => {
                    r.max_x = pt.x.max(r.min_x + 2.0);
                    r.min_y = pt.y.min(r.max_y - 2.0);
                }
                ScaleHandle::Right => r.max_x = pt.x.max(r.min_x + 2.0),
                ScaleHandle::BottomRight => {
                    r.max_x = pt.x.max(r.min_x + 2.0);
                    r.max_y = pt.y.max(r.min_y + 2.0);
                }
                ScaleHandle::Bottom => r.max_y = pt.y.max(r.min_y + 2.0),
                ScaleHandle::BottomLeft => {
                    r.min_x = pt.x.min(r.max_x - 2.0);
                    r.max_y = pt.y.max(r.min_y + 2.0);
                }
                ScaleHandle::Left => r.min_x = pt.x.min(r.max_x - 2.0),
            }
            tab.pending_crop_rect = Some(r);
        }
        CanvasDragState::PullingBezierHandle { handle_out_mm, .. } => {
            *handle_out_mm = pt;
        }
        CanvasDragState::DraggingGradientHandle {
            object_id, is_end, ..
        } => {
            if is_object_editable_in_doc(&tab.document, *object_id) {
                if let Some(obj) = tab.document.objects.get_mut(object_id) {
                    let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_pt = inv.transform_point(pt);
                    match &mut obj.fill {
                        Fill::LinearGradient { start, end, .. } => {
                            if *is_end {
                                *end = local_pt;
                            } else {
                                *start = local_pt;
                            }
                        }
                        Fill::RadialGradient {
                            center,
                            radius,
                            focal,
                            ..
                        } => {
                            if *is_end {
                                *radius = center.distance_to(local_pt).max(0.5);
                            } else {
                                let delta = local_pt - *center;
                                *center = local_pt;
                                *focal = *focal + delta;
                            }
                        }
                        Fill::ConicalGradient {
                            center, angle_deg, ..
                        } => {
                            if *is_end {
                                let d = local_pt - *center;
                                if d.length() > 1e-3 {
                                    *angle_deg = d.y.atan2(d.x).to_degrees();
                                }
                            } else {
                                *center = local_pt;
                            }
                        }
                        Fill::SquareGradient {
                            center,
                            size,
                            angle_deg,
                            ..
                        } => {
                            if *is_end {
                                let d = local_pt - *center;
                                *size = (d.length() * 2.0).max(1.0);
                                if d.length() > 1e-3 {
                                    *angle_deg = d.y.atan2(d.x).to_degrees();
                                }
                            } else {
                                *center = local_pt;
                            }
                        }
                        _ => {}
                    }
                    tab.tile_cache.mark_all_dirty();
                    tab.needs_rerender = true;
                }
            }
        }
        CanvasDragState::DraggingGradientFocal { object_id, .. } => {
            if is_object_editable_in_doc(&tab.document, *object_id) {
                if let Some(obj) = tab.document.objects.get_mut(object_id) {
                    let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_pt = inv.transform_point(pt);
                    if let Fill::RadialGradient { focal, .. } = &mut obj.fill {
                        *focal = local_pt;
                        tab.tile_cache.mark_all_dirty();
                        tab.needs_rerender = true;
                    }
                }
            }
        }
        CanvasDragState::DraggingGradientStop {
            object_id,
            stop_index,
            ..
        } => {
            if is_object_editable_in_doc(&tab.document, *object_id) {
                if let Some(obj) = tab.document.objects.get_mut(object_id) {
                    let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_pt = inv.transform_point(pt);
                    if let Some((start, end, stops)) = gradient_stops_mut(&mut obj.fill) {
                        let v = end - start;
                        let len_sq = v.length_sq().max(1e-6);
                        let t = ((local_pt - start).dot(v) / len_sq).clamp(0.01, 0.99) as f32;
                        if let Some(st) = stops.get_mut(*stop_index) {
                            st.offset = t;
                        }
                        tab.tile_cache.mark_all_dirty();
                        tab.needs_rerender = true;
                    }
                }
            }
        }
        CanvasDragState::DraggingTransparencyHandle {
            object_id,
            effect_index,
            is_end,
            ..
        } => {
            if is_object_editable_in_doc(&tab.document, *object_id) {
                if let Some(obj) = tab.document.objects.get_mut(object_id) {
                    let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_pt = inv.transform_point(pt);
                    if let Some(Effect::Transparency {
                        mode: TransparencyMode::FountainLinear { start, end, .. },
                        ..
                    }) = obj.effects.get_mut(*effect_index)
                    {
                        if *is_end {
                            *end = local_pt;
                        } else {
                            *start = local_pt;
                        }
                        tab.tile_cache.mark_all_dirty();
                        tab.needs_rerender = true;
                    }
                }
            }
        }
        CanvasDragState::DraggingEnvelopeHandle {
            object_id,
            effect_index,
            handle_index,
            ..
        } => {
            if is_object_editable_in_doc(&tab.document, *object_id) {
                if let Some(obj) = tab.document.objects.get_mut(object_id) {
                    let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_pt = inv.transform_point(pt);
                    if let Some(Effect::Envelope { cage, .. }) = obj.effects.get_mut(*effect_index)
                    {
                        if let Some(slot) = cage.get_mut(*handle_index) {
                            *slot = local_pt;
                            tab.tile_cache.mark_all_dirty();
                            tab.needs_rerender = true;
                        }
                    }
                }
            }
        }
        CanvasDragState::DraggingMeshNode {
            object_id,
            node_index,
            ..
        } => {
            if is_object_editable_in_doc(&tab.document, *object_id) {
                if let Some(obj) = tab.document.objects.get_mut(object_id) {
                    let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_pt = inv.transform_point(pt);
                    if let Fill::MeshFill { nodes, .. } = &mut obj.fill {
                        if let Some(mn) = nodes.get_mut(*node_index) {
                            mn.pos = local_pt;
                            tab.tile_cache.mark_all_dirty();
                            tab.needs_rerender = true;
                        }
                    }
                }
            }
        }
        CanvasDragState::DraggingEffectHandle {
            object_id,
            effect_index,
            ..
        } => {
            if is_object_editable_in_doc(&tab.document, *object_id) {
                let obj_center = tab
                    .document
                    .object_bounds(*object_id)
                    .map_or(Point2D::ZERO, |b| b.center());
                if let Some(obj) = tab.document.objects.get_mut(object_id) {
                    let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                    let local_pt = inv.transform_point(pt);
                    if let Some(eff) = obj.effects.get_mut(*effect_index) {
                        match eff {
                            Effect::DropShadow { dx_mm, dy_mm, .. }
                            | Effect::BlockShadow { dx_mm, dy_mm, .. } => {
                                *dx_mm = (pt.x - obj_center.x).clamp(-150.0, 150.0);
                                *dy_mm = (pt.y - obj_center.y).clamp(-150.0, 150.0);
                            }
                            Effect::Contour { offset_mm, .. } => {
                                *offset_mm = obj_center.distance_to(pt).clamp(0.2, 60.0);
                            }
                            Effect::Extrude25D {
                                vanishing_point, ..
                            } => {
                                *vanishing_point = pt;
                            }
                            Effect::Distort {
                                amplitude, center, ..
                            } => {
                                *amplitude = center.distance_to(local_pt).clamp(0.5, 40.0);
                            }
                            Effect::Blend { rotation_deg, .. } => {
                                let d = pt - obj_center;
                                if d.length() > 1e-3 {
                                    *rotation_deg = d.y.atan2(d.x).to_degrees();
                                }
                            }
                            _ => {}
                        }
                        tab.tile_cache.mark_all_dirty();
                        tab.needs_rerender = true;
                    }
                }
            }
        }
        CanvasDragState::DraggingCanvasGuide { guide_id, .. } => {
            if let Some(g) = tab.document.guides.iter_mut().find(|g| g.id == *guide_id) {
                if !g.locked {
                    match &mut g.orientation {
                        GuideOrientation::Horizontal(y) => *y = pt.y,
                        GuideOrientation::Vertical(x) => *x = pt.x,
                        GuideOrientation::Angled { origin, .. } => *origin = pt,
                    }
                    tab.needs_rerender = true;
                }
            }
        }
        CanvasDragState::DrawingBox { current_mm, .. } => {
            let (snapped, guides) = SnapEngine::snap_point(
                &tab.document,
                tab.document.active_page_index,
                pt,
                &[],
                &tab.snap_options,
            );
            tab.active_guides = guides;
            *current_mm = snapped;
        }
        CanvasDragState::DrawingStroke { points_mm } => {
            let next_pt = if active_tool == ToolId::TwoPointLine && ctrl && !points_mm.is_empty() {
                constrain_line_15deg(points_mm[0], pt)
            } else {
                pt
            };
            if active_tool == ToolId::TwoPointLine && points_mm.len() >= 2 {
                *points_mm.last_mut().unwrap() = next_pt;
            } else if points_mm
                .last()
                .is_none_or(|last| last.distance_to(next_pt) > 0.35)
            {
                points_mm.push(next_pt);
            }
        }
        CanvasDragState::Idle | CanvasDragState::DraggingRulerGuide { .. } => {}
    }
    tab.drag_state = state;
}

fn finish_canvas_drag(app: &mut StudioApp, active_tool: ToolId, ctrl: bool, shift: bool) {
    let state = match app.active_tab_mut() {
        Some(tab) => {
            tab.viewport.lod_dragging = false;
            tab.active_guides.clear();
            tab.needs_rerender = true;
            std::mem::take(&mut tab.drag_state)
        }
        None => return,
    };

    match state {
        CanvasDragState::MarqueeSelect {
            start_mm,
            current_mm,
        } => {
            let rect = Rect2D::from_points(start_mm, current_mm);
            if let Some(tab) = app.active_tab_mut() {
                let hits =
                    hit_test_rect(&tab.document, tab.document.active_page_index, rect, false);
                if shift {
                    let mut combined = tab.selection.objects.clone();
                    for id in hits {
                        if !combined.contains(&id) {
                            combined.push(id);
                        }
                    }
                    tab.selection.select_many(&tab.document, &combined);
                } else {
                    tab.selection.select_many(&tab.document, &hits);
                }
                tab.metrics_dirty = true;
            }
        }
        CanvasDragState::LassoSelect { points_mm } => {
            app.select_by_lasso(&points_mm, shift);
        }
        CanvasDragState::NodeMarqueeSelect {
            start_mm,
            current_mm,
        } => {
            let rect = Rect2D::from_points(start_mm, current_mm);
            app.select_nodes_in_rect(rect, shift);
        }
        CanvasDragState::ZoomMarquee {
            start_mm,
            current_mm,
        } => {
            let rect = Rect2D::from_points(start_mm, current_mm);
            if rect.width() >= 1.0 && rect.height() >= 1.0 {
                if let Some(tab) = app.active_tab_mut() {
                    tab.viewport.fit_rect(rect, 12.0);
                    tab.needs_rerender = true;
                }
                app.complete_one_shot_zoom_if_active();
            }
        }
        CanvasDragState::MovingSelection {
            accum_dx, accum_dy, ..
        } => {
            if accum_dx.abs() > 1e-5 || accum_dy.abs() > 1e-5 {
                let ids: Vec<ObjectId> = app
                    .active_tab()
                    .map(|t| {
                        t.selection
                            .objects
                            .iter()
                            .copied()
                            .filter(|&id| is_object_editable_in_doc(&t.document, id))
                            .collect()
                    })
                    .unwrap_or_default();
                if !ids.is_empty() {
                    if let Some(tab) = app.active_tab_mut() {
                        let inv = Affine2D::translate(-accum_dx, -accum_dy);
                        for &id in &ids {
                            if let Some(obj) = tab.document.objects.get_mut(&id) {
                                obj.transform = inv.compose(&obj.transform);
                            }
                        }
                    }
                    let _ = app.execute_command(CommandOp::TransformObjects {
                        ids,
                        affine: Affine2D::translate(accum_dx, accum_dy),
                    });
                }
            }
        }
        CanvasDragState::ScalingSelection {
            orig_transforms,
            last_affine,
            ..
        }
        | CanvasDragState::RotatingSelection {
            orig_transforms,
            last_affine,
            ..
        }
        | CanvasDragState::SkewingSelection {
            orig_transforms,
            last_affine,
            ..
        }
        | CanvasDragState::FreeTransforming {
            orig_transforms,
            last_affine,
            ..
        } => {
            if !orig_transforms.is_empty() && last_affine != Affine2D::IDENTITY {
                let ids: Vec<ObjectId> = orig_transforms.iter().map(|(id, _)| *id).collect();
                // Roll back live preview transforms so CommandHistory captures exact before/after!
                if let Some(tab) = app.active_tab_mut() {
                    for (id, orig_xf) in orig_transforms {
                        if let Some(obj) = tab.document.objects.get_mut(&id) {
                            obj.transform = orig_xf;
                        }
                    }
                }
                let _ = app.execute_command(CommandOp::TransformObjects {
                    ids,
                    affine: last_affine,
                });
            }
        }
        CanvasDragState::DraggingNode { orig_paths, .. } => {
            let mut ops = Vec::new();
            if let Some(tab) = app.active_tab_mut() {
                for (oid, orig_path) in orig_paths {
                    if let Some(obj) = tab.document.objects.get_mut(&oid) {
                        if let NodePayload::Path(ref mut p) = obj.payload {
                            let edited = p.clone();
                            *p = orig_path;
                            ops.push(CommandOp::EditPath {
                                id: oid,
                                path: edited,
                            });
                        }
                    }
                }
            }
            if ops.len() == 1 {
                let _ = app.execute_command(ops.into_iter().next().unwrap());
            } else if !ops.is_empty() {
                let _ = app.execute_command(CommandOp::Batch(ops));
            }
        }
        CanvasDragState::DraggingCurveSegment {
            object_id,
            orig_path,
            ..
        } => {
            let final_path = app.active_tab_mut().and_then(|tab| {
                let obj = tab.document.objects.get_mut(&object_id)?;
                let NodePayload::Path(ref mut p) = obj.payload else {
                    return None;
                };
                let edited = p.clone();
                *p = orig_path;
                Some(edited)
            });
            if let Some(path) = final_path {
                let _ = app.execute_command(CommandOp::EditPath {
                    id: object_id,
                    path,
                });
            }
        }
        CanvasDragState::DraggingParametricHandle {
            object_id,
            orig_shape,
            ..
        } => {
            let new_shape = app.active_tab().and_then(|tab| {
                let obj = tab.document.objects.get(&object_id)?;
                match &obj.payload {
                    NodePayload::Shape(s) => Some(s.clone()),
                    _ => None,
                }
            });
            if let Some(ns) = new_shape {
                app.commit_parametric_shape_edit(object_id, orig_shape, ns);
            }
        }
        CanvasDragState::Sculpting {
            object_id,
            orig_payload,
            ..
        } => {
            let final_path = app.active_tab_mut().and_then(|tab| {
                let obj = tab.document.objects.get_mut(&object_id)?;
                let edited = match &obj.payload {
                    NodePayload::Path(p) => p.clone(),
                    _ => return None,
                };
                obj.payload = orig_payload;
                Some(edited)
            });
            if let Some(path) = final_path {
                let _ = app.execute_command(CommandOp::EditPath {
                    id: object_id,
                    path,
                });
            }
        }
        CanvasDragState::PullingBezierHandle {
            anchor_mm,
            handle_out_mm,
            closing,
        } => {
            let handle = (handle_out_mm.distance_to(anchor_mm) > 0.35).then_some(handle_out_mm);
            if closing {
                if let Some(tab) = app.active_tab_mut() {
                    if let (Some(first), Some(ho)) = (tab.bezier_draft_nodes.first_mut(), handle) {
                        let v = ho - first.pos;
                        first.handle_out = Some(ho);
                        first.handle_in = Some(first.pos - v);
                        first.kind = NodeKind::Symmetrical;
                    }
                }
                app.bezier_finish(true);
            } else {
                app.bezier_add_point(anchor_mm, handle);
            }
        }
        CanvasDragState::DraggingGradientHandle {
            object_id,
            orig_fill,
            ..
        }
        | CanvasDragState::DraggingGradientFocal {
            object_id,
            orig_fill,
        }
        | CanvasDragState::DraggingGradientStop {
            object_id,
            orig_fill,
            ..
        }
        | CanvasDragState::DraggingMeshNode {
            object_id,
            orig_fill,
            ..
        } => {
            let final_fill = app.active_tab_mut().and_then(|tab| {
                let obj = tab.document.objects.get_mut(&object_id)?;
                let mut edited = obj.fill.clone();
                if let Some((_, _, stops)) = gradient_stops_mut(&mut edited) {
                    stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
                }
                obj.fill = orig_fill;
                Some(edited)
            });
            if let Some(fill) = final_fill {
                let _ = app.execute_command(CommandOp::SetFill {
                    ids: vec![object_id],
                    fill,
                });
            }
        }
        CanvasDragState::DraggingTransparencyHandle {
            object_id,
            effect_index,
            orig_effect,
            ..
        }
        | CanvasDragState::DraggingEnvelopeHandle {
            object_id,
            effect_index,
            orig_effect,
            ..
        }
        | CanvasDragState::DraggingEffectHandle {
            object_id,
            effect_index,
            orig_effect,
        } => {
            let final_eff = app.active_tab_mut().and_then(|tab| {
                let obj = tab.document.objects.get_mut(&object_id)?;
                let slot = obj.effects.get_mut(effect_index)?;
                let edited = slot.clone();
                *slot = orig_effect;
                Some(edited)
            });
            if let Some(eff) = final_eff {
                app.replace_effect_on_object(object_id, Some(effect_index), eff);
            }
        }
        CanvasDragState::DraggingCanvasGuide {
            guide_id,
            orig_orientation,
        } => {
            let final_ori = app.active_tab_mut().and_then(|tab| {
                let g = tab.document.guides.iter_mut().find(|g| g.id == guide_id)?;
                let edited = g.orientation;
                g.orientation = orig_orientation;
                Some(edited)
            });
            if let Some(orientation) = final_ori {
                if orientation != orig_orientation {
                    let _ = app.execute_command(CommandOp::MoveGuide {
                        id: guide_id,
                        orientation,
                    });
                }
            }
        }
        CanvasDragState::DraggingInteractiveEffect {
            start_mm,
            current_mm,
            ..
        } => {
            app.apply_interactive_effect_drag(active_tool, start_mm, current_mm);
        }
        CanvasDragState::DrawingBox {
            start_mm,
            current_mm,
        } => match active_tool {
            ToolId::InteractiveFill => {
                app.apply_interactive_fill_drag(start_mm, current_mm);
            }
            ToolId::MeshFill => {
                app.mesh_fill_interact(start_mm, current_mm, None);
            }
            ToolId::StraightConnector | ToolId::RightAngleConnector => {
                app.commit_connector_drag(
                    start_mm,
                    current_mm,
                    active_tool == ToolId::RightAngleConnector,
                );
            }
            ToolId::ThreePointCurve => {
                if start_mm.distance_to(current_mm) >= 1.0 {
                    if let Some(tab) = app.active_tab_mut() {
                        tab.three_point_curve_chord = Some((start_mm, current_mm));
                    }
                    app.push_toast(
                        "Move cursor to bend arc, then click to commit 3-Point Curve",
                        false,
                    );
                }
            }
            ToolId::ThreePointRectangle | ToolId::ThreePointEllipse => {
                if start_mm.distance_to(current_mm) >= 1.0 {
                    if let Some(tab) = app.active_tab_mut() {
                        tab.three_point_shape_chord = Some((active_tool, start_mm, current_mm));
                    }
                    app.push_toast(
                        "Move cursor perpendicular to baseline, then click to finish",
                        false,
                    );
                }
            }
            ToolId::Crop => {
                let rect = compute_constrained_box(start_mm, current_mm, ctrl, shift);
                if rect.width() >= 2.0 && rect.height() >= 2.0 {
                    if let Some(tab) = app.active_tab_mut() {
                        tab.pending_crop_rect = Some(rect);
                    }
                    app.push_toast(
                        "Adjust crop handles, then press Enter or double-click inside to Crop",
                        false,
                    );
                }
            }
            ToolId::ParagraphText => {
                let rect = compute_constrained_box(start_mm, current_mm, ctrl, shift);
                if rect.width() >= 4.0 && rect.height() >= 4.0 {
                    app.begin_inline_text_edit(
                        None,
                        rect.min(),
                        true,
                        Vec2D::new(rect.width(), rect.height()),
                    );
                }
            }
            _ => {
                let rect = compute_constrained_box(start_mm, current_mm, ctrl, shift);
                app.commit_drawn_box(rect);
            }
        },
        CanvasDragState::DrawingStroke { points_mm } => {
            commit_drawn_stroke(app, active_tool, points_mm);
        }
        _ => {}
    }
}

fn commit_drawn_stroke(app: &mut StudioApp, tool: ToolId, points: Vec<Point2D>) {
    if points.len() < 2 {
        return;
    }
    let layer_id = app.active_tab().and_then(|t| t.active_layer_id);

    match tool {
        ToolId::TwoPointLine => {
            let path = BezierPath::from_line(points[0], *points.last().unwrap());
            let obj = SceneObject::new(ObjectId(0), "Line", NodePayload::Path(path))
                .with_fill(Fill::None)
                .with_outline(app.default_outline.clone());
            let _ = app.execute_command(CommandOp::create_object(obj, layer_id));
        }
        ToolId::Freehand => {
            let mut path = BezierPath::from_polygon(points, false);
            path.reduce_nodes(0.15);
            let obj = SceneObject::new(ObjectId(0), "Freehand Curve", NodePayload::Path(path))
                .with_fill(Fill::None)
                .with_outline(app.default_outline.clone());
            let _ = app.execute_command(CommandOp::create_object(obj, layer_id));
        }
        ToolId::SmartDrawing => {
            let recognized = SmartDrawing::recognize_rough_stroke(&points);
            let path = recognized.to_bezier_path();
            let obj = SceneObject::new(ObjectId(0), "Smart Shape", NodePayload::Path(path))
                .with_fill(app.default_fill.clone())
                .with_outline(app.default_outline.clone());
            let _ = app.execute_command(CommandOp::create_object(obj, layer_id));
        }
        ToolId::LiveSketch => {
            let strokes = if let Some(tab) = app.active_tab_mut() {
                tab.live_sketch_strokes.push(points);
                tab.live_sketch_strokes.clone()
            } else {
                vec![points]
            };
            let path = LiveSketch::average_strokes(&strokes, 32);
            let obj = SceneObject::new(ObjectId(0), "LiveSketch Curve", NodePayload::Path(path))
                .with_fill(Fill::None)
                .with_outline(app.default_outline.clone());
            let _ = app.execute_command(CommandOp::create_object(obj, layer_id));
        }
        ToolId::ArtisticMedia => {
            let centerline = BezierPath::from_polygon(points, false);
            let expanded = app.expand_artistic_stroke(&centerline);
            let obj = SceneObject::new(ObjectId(0), "Artistic Media", NodePayload::Path(expanded))
                .with_fill(app.default_fill.clone())
                .with_outline(OutlineStyle::none());
            let _ = app.execute_command(CommandOp::create_object(obj, layer_id));
        }
        ToolId::Knife => {
            app.apply_knife_cut(points[0], *points.last().unwrap());
        }
        ToolId::VirtualSegmentDelete => {
            for &pt in points.iter().step_by(2) {
                app.apply_virtual_segment_delete_at(pt);
            }
        }
        ToolId::Eraser => {
            app.apply_vector_eraser_stroke(&points);
        }
        ToolId::BitmapBrush | ToolId::BitmapEraser | ToolId::CloneStamp | ToolId::Smudge => {
            app.apply_raster_tool_stroke(tool, &points);
        }
        _ => {}
    }
}

// =============================================================================
// Geometry, Constraint & Handle Hit-Test Helpers
// =============================================================================

fn compute_constrained_box(
    start: Point2D,
    cur: Point2D,
    ctrl_square: bool,
    shift_center: bool,
) -> Rect2D {
    let mut dx = cur.x - start.x;
    let mut dy = cur.y - start.y;
    if ctrl_square {
        let side = dx.abs().max(dy.abs());
        dx = if dx < 0.0 { -side } else { side };
        dy = if dy < 0.0 { -side } else { side };
    }
    if shift_center {
        let p0 = Point2D::new(start.x - dx, start.y - dy);
        let p1 = Point2D::new(start.x + dx, start.y + dy);
        Rect2D::from_points(p0, p1)
    } else {
        Rect2D::from_points(start, Point2D::new(start.x + dx, start.y + dy))
    }
}

fn constrain_line_15deg(start: Point2D, cur: Point2D) -> Point2D {
    let v = cur - start;
    let len = v.length();
    if len < 1e-6 {
        return cur;
    }
    let step = std::f64::consts::PI / 12.0;
    let ang = (v.y.atan2(v.x) / step).round() * step;
    Point2D::new(start.x + len * ang.cos(), start.y + len * ang.sin())
}

fn hit_scale_handle(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    bb: Rect2D,
    screen_pos: Pos2,
) -> Option<ScaleHandle> {
    let handles = selection_handle_positions(canvas_rect, tab, bb);
    for (h, pos) in handles {
        if pos.distance(screen_pos) <= 8.0 {
            return Some(h);
        }
    }
    None
}

fn selection_handle_positions(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    bb: Rect2D,
) -> [(ScaleHandle, Pos2); 8] {
    let tl = world_to_screen(canvas_rect, tab, Point2D::new(bb.min_x, bb.min_y));
    let tr = world_to_screen(canvas_rect, tab, Point2D::new(bb.max_x, bb.min_y));
    let br = world_to_screen(canvas_rect, tab, Point2D::new(bb.max_x, bb.max_y));
    let bl = world_to_screen(canvas_rect, tab, Point2D::new(bb.min_x, bb.max_y));
    let tc = pos2((tl.x + tr.x) * 0.5, tl.y);
    let bc = pos2((bl.x + br.x) * 0.5, bl.y);
    let lc = pos2(tl.x, (tl.y + bl.y) * 0.5);
    let rc = pos2(tr.x, (tr.y + br.y) * 0.5);
    [
        (ScaleHandle::TopLeft, tl),
        (ScaleHandle::Top, tc),
        (ScaleHandle::TopRight, tr),
        (ScaleHandle::Right, rc),
        (ScaleHandle::BottomRight, br),
        (ScaleHandle::Bottom, bc),
        (ScaleHandle::BottomLeft, bl),
        (ScaleHandle::Left, lc),
    ]
}

fn compute_scale_affine(
    handle: ScaleHandle,
    orig: Rect2D,
    cur: Point2D,
    ctrl_proportional: bool,
    shift_from_center: bool,
) -> Option<Affine2D> {
    let ow = orig.width().max(1e-3);
    let oh = orig.height().max(1e-3);
    let (anchor, mut sx, mut sy) = if shift_from_center {
        let c = orig.center();
        let hw = (ow * 0.5).max(1e-3);
        let hh = (oh * 0.5).max(1e-3);
        match handle {
            ScaleHandle::Left | ScaleHandle::Right => {
                (c, ((cur.x - c.x).abs() / hw).clamp(0.05, 50.0), 1.0)
            }
            ScaleHandle::Top | ScaleHandle::Bottom => {
                (c, 1.0, ((cur.y - c.y).abs() / hh).clamp(0.05, 50.0))
            }
            _ => (
                c,
                ((cur.x - c.x).abs() / hw).clamp(0.05, 50.0),
                ((cur.y - c.y).abs() / hh).clamp(0.05, 50.0),
            ),
        }
    } else {
        match handle {
            ScaleHandle::BottomRight => (
                Point2D::new(orig.min_x, orig.min_y),
                ((cur.x - orig.min_x) / ow).clamp(0.05, 50.0),
                ((cur.y - orig.min_y) / oh).clamp(0.05, 50.0),
            ),
            ScaleHandle::TopLeft => (
                Point2D::new(orig.max_x, orig.max_y),
                ((orig.max_x - cur.x) / ow).clamp(0.05, 50.0),
                ((orig.max_y - cur.y) / oh).clamp(0.05, 50.0),
            ),
            ScaleHandle::TopRight => (
                Point2D::new(orig.min_x, orig.max_y),
                ((cur.x - orig.min_x) / ow).clamp(0.05, 50.0),
                ((orig.max_y - cur.y) / oh).clamp(0.05, 50.0),
            ),
            ScaleHandle::BottomLeft => (
                Point2D::new(orig.max_x, orig.min_y),
                ((orig.max_x - cur.x) / ow).clamp(0.05, 50.0),
                ((cur.y - orig.min_y) / oh).clamp(0.05, 50.0),
            ),
            ScaleHandle::Right => (
                Point2D::new(orig.min_x, orig.center().y),
                ((cur.x - orig.min_x) / ow).clamp(0.05, 50.0),
                1.0,
            ),
            ScaleHandle::Left => (
                Point2D::new(orig.max_x, orig.center().y),
                ((orig.max_x - cur.x) / ow).clamp(0.05, 50.0),
                1.0,
            ),
            ScaleHandle::Bottom => (
                Point2D::new(orig.center().x, orig.min_y),
                1.0,
                ((cur.y - orig.min_y) / oh).clamp(0.05, 50.0),
            ),
            ScaleHandle::Top => (
                Point2D::new(orig.center().x, orig.max_y),
                1.0,
                ((orig.max_y - cur.y) / oh).clamp(0.05, 50.0),
            ),
        }
    };
    if ctrl_proportional
        && matches!(
            handle,
            ScaleHandle::TopLeft
                | ScaleHandle::TopRight
                | ScaleHandle::BottomLeft
                | ScaleHandle::BottomRight
        )
    {
        let u = sx.max(sy);
        sx = u;
        sy = u;
    }
    if (sx - 1.0).abs() < 1e-4 && (sy - 1.0).abs() < 1e-4 {
        return None;
    }
    Some(
        Affine2D::translate(anchor.x, anchor.y)
            .compose(&Affine2D::scale(sx, sy))
            .compose(&Affine2D::translate(-anchor.x, -anchor.y)),
    )
}

fn parametric_shape_handles(
    shape: &ParametricShape,
    transform: &Affine2D,
) -> Vec<(ParametricHandleKind, Point2D)> {
    match *shape {
        ParametricShape::Rectangle {
            width,
            height,
            corner_radii,
            ..
        } => {
            let max_r = (width.min(height) * 0.5).max(0.0);
            let r0 = corner_radii[0].clamp(0.0, max_r);
            let r1 = corner_radii[1].clamp(0.0, max_r);
            let r2 = corner_radii[2].clamp(0.0, max_r);
            let r3 = corner_radii[3].clamp(0.0, max_r);
            vec![
                (
                    ParametricHandleKind::RectCorner(0),
                    transform.transform_point(Point2D::new(r0, 0.0)),
                ),
                (
                    ParametricHandleKind::RectCorner(1),
                    transform.transform_point(Point2D::new(width - r1, 0.0)),
                ),
                (
                    ParametricHandleKind::RectCorner(2),
                    transform.transform_point(Point2D::new(width - r2, height)),
                ),
                (
                    ParametricHandleKind::RectCorner(3),
                    transform.transform_point(Point2D::new(r3, height)),
                ),
            ]
        }
        ParametricShape::Ellipse {
            rx,
            ry,
            mode,
            start_deg,
            end_deg,
        } => {
            let a0 = start_deg.to_radians();
            let mut out = vec![(
                ParametricHandleKind::EllipseAngle { is_end: false },
                transform.transform_point(Point2D::new(rx * a0.cos(), ry * a0.sin())),
            )];
            if mode != EllipseMode::Full {
                let a1 = end_deg.to_radians();
                out.push((
                    ParametricHandleKind::EllipseAngle { is_end: true },
                    transform.transform_point(Point2D::new(rx * a1.cos(), ry * a1.sin())),
                ));
            }
            out
        }
        ParametricShape::Polygon { radius_y, .. } => vec![(
            ParametricHandleKind::PolygonVertex,
            transform.transform_point(Point2D::new(0.0, -radius_y)),
        )],
        ParametricShape::Star {
            points,
            outer_radius,
            inner_ratio,
            ..
        } => {
            let n = points.max(3) as f64;
            let valley_ang = -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI / n;
            let ir = outer_radius * inner_ratio.clamp(0.1, 0.92);
            vec![
                (
                    ParametricHandleKind::StarOuter,
                    transform.transform_point(Point2D::new(0.0, -outer_radius)),
                ),
                (
                    ParametricHandleKind::StarInner,
                    transform.transform_point(Point2D::new(
                        ir * valley_ang.cos(),
                        ir * valley_ang.sin(),
                    )),
                ),
            ]
        }
        _ => Vec::new(),
    }
}

fn hit_parametric_shape_handle(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    screen_pos: Pos2,
) -> Option<(ObjectId, ParametricHandleKind, ParametricShape)> {
    for &oid in &tab.selection.objects {
        if !is_object_editable_in_doc(&tab.document, oid) {
            continue;
        }
        let Some(obj) = tab.document.objects.get(&oid) else {
            continue;
        };
        let NodePayload::Shape(ref shape) = obj.payload else {
            continue;
        };
        for (kind, wpt) in parametric_shape_handles(shape, &obj.transform) {
            let sp = world_to_screen(canvas_rect, tab, wpt);
            if sp.distance(screen_pos) <= 9.0 {
                return Some((oid, kind, shape.clone()));
            }
        }
    }
    None
}

fn update_parametric_shape_live(
    shape: &mut ParametricShape,
    handle: ParametricHandleKind,
    local_pt: Point2D,
    ctrl: bool,
    shift: bool,
) {
    match (shape, handle) {
        (
            ParametricShape::Rectangle {
                width,
                height,
                corner_radii,
                lock_corners,
                ..
            },
            ParametricHandleKind::RectCorner(idx),
        ) => {
            let max_r = (width.min(*height) * 0.5).max(0.0);
            let raw_r = match idx {
                0 | 3 => local_pt.x.clamp(0.0, max_r),
                _ => (*width - local_pt.x).clamp(0.0, max_r),
            };
            if *lock_corners && !shift {
                *corner_radii = [raw_r; 4];
            } else if idx < 4 {
                corner_radii[idx] = raw_r;
            }
        }
        (
            ParametricShape::Ellipse {
                rx,
                ry,
                mode,
                start_deg,
                end_deg,
            },
            ParametricHandleKind::EllipseAngle { is_end },
        ) => {
            let nx = local_pt.x / rx.max(1e-3);
            let ny = local_pt.y / ry.max(1e-3);
            let r_norm = nx.hypot(ny);
            // Authentic Classic Studio Ellipse rule: dragging OUTSIDE -> Arc, INSIDE -> Pie!
            *mode = if r_norm > 1.0 {
                EllipseMode::Arc
            } else {
                EllipseMode::Pie
            };
            let mut deg = ny.atan2(nx).to_degrees();
            if deg < 0.0 {
                deg += 360.0;
            }
            if ctrl {
                deg = (deg / 15.0).round() * 15.0;
            }
            if is_end {
                *end_deg = deg;
            } else {
                *start_deg = deg;
                if (*end_deg - *start_deg).abs() < 1.0 || (*end_deg >= 359.5 && *start_deg > 1.0) {
                    *end_deg = 360.0;
                }
            }
        }
        (
            ParametricShape::Polygon {
                radius_x, radius_y, ..
            },
            ParametricHandleKind::PolygonVertex,
        ) => {
            let dist = local_pt.x.hypot(local_pt.y).clamp(2.0, 2000.0);
            if ctrl {
                *radius_x = dist;
                *radius_y = dist;
            } else {
                *radius_y = dist;
                *radius_x = (*radius_x).max(2.0);
            }
        }
        (ParametricShape::Star { outer_radius, .. }, ParametricHandleKind::StarOuter) => {
            *outer_radius = local_pt.x.hypot(local_pt.y).clamp(2.0, 2000.0);
        }
        (
            ParametricShape::Star {
                outer_radius,
                inner_ratio,
                ..
            },
            ParametricHandleKind::StarInner,
        ) => {
            let d = local_pt.x.hypot(local_pt.y);
            *inner_ratio = (d / outer_radius.max(1e-3)).clamp(0.1, 0.92);
        }
        _ => {}
    }
}

fn eval_cubic(p0: Point2D, p1: Point2D, p2: Point2D, p3: Point2D, t: f64) -> Point2D {
    let mt = 1.0 - t;
    let mt2 = mt * mt;
    let t2 = t * t;
    Point2D::new(
        mt2 * mt * p0.x + 3.0 * mt2 * t * p1.x + 3.0 * mt * t2 * p2.x + t2 * t * p3.x,
        mt2 * mt * p0.y + 3.0 * mt2 * t * p1.y + 3.0 * mt * t2 * p2.y + t2 * t * p3.y,
    )
}

fn hit_curve_segment_on_selection(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    screen_pos: Pos2,
) -> Option<(ObjectId, usize, usize, f64)> {
    let mut best: Option<(f32, ObjectId, usize, usize, f64)> = None;
    for &oid in &tab.selection.objects {
        if !is_object_editable_in_doc(&tab.document, oid) {
            continue;
        }
        let Some(obj) = tab.document.objects.get(&oid) else {
            continue;
        };
        let NodePayload::Path(ref path) = obj.payload else {
            continue;
        };
        for (sp_idx, sp) in path.subpaths.iter().enumerate() {
            let sc = sp.segment_count();
            for seg_idx in 0..sc {
                let Some((i0, i1)) = sp.segment_node_indices(seg_idx) else {
                    continue;
                };
                let n0 = sp.nodes[i0];
                let n1 = sp.nodes[i1];
                let p0 = n0.pos;
                let p1 = n0.handle_out.unwrap_or(p0);
                let p2 = n1.handle_in.unwrap_or(n1.pos);
                let p3 = n1.pos;
                for k in 1..16 {
                    let t = f64::from(k) / 16.0;
                    let pt = eval_cubic(p0, p1, p2, p3, t);
                    let s = world_to_screen(canvas_rect, tab, obj.transform.transform_point(pt));
                    let d = s.distance(screen_pos);
                    if d <= 8.0 && best.is_none_or(|b| d < b.0) {
                        best = Some((d, oid, sp_idx, seg_idx, t));
                    }
                }
            }
        }
    }
    best.map(|(_, oid, sp, seg, t)| (oid, sp, seg, t))
}

fn hit_editable_bezier_node_or_handle(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    screen_pos: Pos2,
) -> Option<(NodeRef, NodeDragPart, BezierPath)> {
    for &oid in &tab.selection.objects {
        if !is_object_editable_in_doc(&tab.document, oid) {
            continue;
        }
        let Some(obj) = tab.document.objects.get(&oid) else {
            continue;
        };
        let NodePayload::Path(ref path) = obj.payload else {
            continue;
        };
        for (sp_idx, sp) in path.subpaths.iter().enumerate() {
            for (n_idx, node) in sp.nodes.iter().enumerate() {
                if let Some(h_in) = node.handle_in {
                    let s = world_to_screen(canvas_rect, tab, obj.transform.transform_point(h_in));
                    if s.distance(screen_pos) <= 7.0 {
                        return Some((
                            NodeRef {
                                object_id: oid,
                                subpath_index: sp_idx,
                                node_index: n_idx,
                            },
                            NodeDragPart::HandleIn,
                            path.clone(),
                        ));
                    }
                }
                if let Some(h_out) = node.handle_out {
                    let s = world_to_screen(canvas_rect, tab, obj.transform.transform_point(h_out));
                    if s.distance(screen_pos) <= 7.0 {
                        return Some((
                            NodeRef {
                                object_id: oid,
                                subpath_index: sp_idx,
                                node_index: n_idx,
                            },
                            NodeDragPart::HandleOut,
                            path.clone(),
                        ));
                    }
                }
                let a = world_to_screen(canvas_rect, tab, obj.transform.transform_point(node.pos));
                if a.distance(screen_pos) <= 7.5 {
                    return Some((
                        NodeRef {
                            object_id: oid,
                            subpath_index: sp_idx,
                            node_index: n_idx,
                        },
                        NodeDragPart::Anchor,
                        path.clone(),
                    ));
                }
            }
        }
    }
    None
}

// =============================================================================
// Right-Click Canvas Context Menu & Direct On-Canvas Inline Text Editor
// =============================================================================

fn show_canvas_context_menu(canvas_resp: &egui::Response, app: &mut StudioApp) {
    canvas_resp.context_menu(|ui| {
        let has_sel = app.active_tab().is_some_and(|t| !t.selection.is_empty());
        let has_multi = app.active_tab().is_some_and(|t| t.selection.len() >= 2);
        let has_clip = !app.clipboard_objects.is_empty();

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

    // 0. Live Laser Toolpath Preview Overlay
    paint_laser_toolpath_overlay(&painter, app, tab, canvas_rect);

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
            let Some(obj) = tab.document.objects.get(&oid) else {
                continue;
            };
            match &obj.payload {
                NodePayload::Shape(shape) => {
                    for (_, wpt) in parametric_shape_handles(shape, &obj.transform) {
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
                                obj.transform.transform_point(node.pos),
                            );
                            for h in [node.handle_in, node.handle_out].into_iter().flatten() {
                                let h_pos = world_to_screen(
                                    canvas_rect,
                                    tab,
                                    obj.transform.transform_point(h),
                                );
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
        if let Some(g) = tab.document.guides.iter().find(|g| g.id == sel_gid) {
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
    if let Some(&oid) = tab.selection.objects.first() {
        if let Some(obj) = tab.document.objects.get(&oid) {
            if app.active_tool == ToolId::InteractiveFill {
                match &obj.fill {
                    Fill::LinearGradient { start, end, stops } => {
                        let a = world_to_screen(
                            canvas_rect,
                            tab,
                            obj.transform.transform_point(*start),
                        );
                        let b =
                            world_to_screen(canvas_rect, tab, obj.transform.transform_point(*end));
                        painter.line_segment([a, b], Stroke::new(2.0, tokens.accent));
                        draw_gradient_mid_stops(
                            &painter,
                            canvas_rect,
                            tab,
                            &obj.transform,
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
                        let a = world_to_screen(
                            canvas_rect,
                            tab,
                            obj.transform.transform_point(*center),
                        );
                        let r_pt = Point2D::new(center.x + radius.max(1.0), center.y);
                        let b =
                            world_to_screen(canvas_rect, tab, obj.transform.transform_point(r_pt));
                        let r_px = a.distance(b).max(2.0);
                        painter.circle_stroke(a, r_px, Stroke::new(1.2, tokens.accent));
                        painter.line_segment([a, b], Stroke::new(1.8, tokens.accent));
                        draw_gradient_mid_stops(
                            &painter,
                            canvas_rect,
                            tab,
                            &obj.transform,
                            *center,
                            r_pt,
                            stops,
                        );
                        if (*focal - *center).length() > 0.2 {
                            let f = world_to_screen(
                                canvas_rect,
                                tab,
                                obj.transform.transform_point(*focal),
                            );
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
                        let a = world_to_screen(
                            canvas_rect,
                            tab,
                            obj.transform.transform_point(*center),
                        );
                        let rad = angle_deg.to_radians();
                        let e_pt =
                            Point2D::new(center.x + 25.0 * rad.cos(), center.y + 25.0 * rad.sin());
                        let b =
                            world_to_screen(canvas_rect, tab, obj.transform.transform_point(e_pt));
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
                            &obj.transform,
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
                        let a = world_to_screen(
                            canvas_rect,
                            tab,
                            obj.transform.transform_point(*center),
                        );
                        let rad = angle_deg.to_radians();
                        let half = size.max(2.0) * 0.5;
                        let e_pt =
                            Point2D::new(center.x + half * rad.cos(), center.y + half * rad.sin());
                        let b =
                            world_to_screen(canvas_rect, tab, obj.transform.transform_point(e_pt));
                        painter.line_segment([a, b], Stroke::new(2.0, tokens.accent));
                        draw_gradient_mid_stops(
                            &painter,
                            canvas_rect,
                            tab,
                            &obj.transform,
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
                        let a = world_to_screen(
                            canvas_rect,
                            tab,
                            obj.transform.transform_point(*start),
                        );
                        let b =
                            world_to_screen(canvas_rect, tab, obj.transform.transform_point(*end));
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
                                    world_to_screen(
                                        canvas_rect,
                                        tab,
                                        obj.transform.transform_point(p),
                                    )
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
            if app.active_tool == ToolId::MeshFill {
                if let Fill::MeshFill { rows, cols, nodes } = &obj.fill {
                    let spts: Vec<Pos2> = nodes
                        .iter()
                        .map(|n| {
                            world_to_screen(canvas_rect, tab, obj.transform.transform_point(n.pos))
                        })
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

    // 6. Active Bitmap Mask Selection Rectangle (`MaskSelect`)
    if let Some(mrect) = tab.raster_mask_rect_mm {
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

    // 7. Active Dragging Previews
    let (ctrl, shift) = ui.input(|i| (i.modifiers.ctrl || i.modifiers.command, i.modifiers.shift));
    match &tab.drag_state {
        CanvasDragState::MarqueeSelect {
            start_mm,
            current_mm,
        }
        | CanvasDragState::NodeMarqueeSelect {
            start_mm,
            current_mm,
        } => {
            let a = world_to_screen(canvas_rect, tab, *start_mm);
            let b = world_to_screen(canvas_rect, tab, *current_mm);
            let r = Rect::from_two_pos(a, b);
            painter.rect(
                r,
                CornerRadius::ZERO,
                tokens.accent.gamma_multiply(0.14),
                Stroke::new(1.0, tokens.accent),
                StrokeKind::Inside,
            );
        }
        CanvasDragState::ZoomMarquee {
            start_mm,
            current_mm,
        } => {
            let a = world_to_screen(canvas_rect, tab, *start_mm);
            let b = world_to_screen(canvas_rect, tab, *current_mm);
            let r = Rect::from_two_pos(a, b);
            painter.rect(
                r,
                CornerRadius::ZERO,
                tokens.accent_secondary.gamma_multiply(0.12),
                Stroke::new(1.4, tokens.accent_secondary),
                StrokeKind::Inside,
            );
        }
        CanvasDragState::LassoSelect { points_mm } => {
            if points_mm.len() >= 2 {
                let mut spts: Vec<Pos2> = points_mm
                    .iter()
                    .map(|p| world_to_screen(canvas_rect, tab, *p))
                    .collect();
                spts.push(spts[0]);
                painter.add(Shape::line(spts, Stroke::new(1.5, tokens.accent_secondary)));
            }
        }
        CanvasDragState::DraggingInteractiveEffect {
            start_mm,
            current_mm,
            ..
        } => {
            let a = world_to_screen(canvas_rect, tab, *start_mm);
            let b = world_to_screen(canvas_rect, tab, *current_mm);
            painter.line_segment([a, b], Stroke::new(2.0, tokens.accent_secondary));
            painter.circle(
                a,
                4.5,
                Color32::WHITE,
                Stroke::new(1.5, tokens.accent_secondary),
            );
            painter.circle(
                b,
                5.5,
                tokens.accent_secondary,
                Stroke::new(1.5, Color32::WHITE),
            );
        }
        CanvasDragState::PullingBezierHandle {
            anchor_mm,
            handle_out_mm,
            ..
        } => {
            let a = world_to_screen(canvas_rect, tab, *anchor_mm);
            let ho = world_to_screen(canvas_rect, tab, *handle_out_mm);
            let hi = a - (ho - a);
            painter.line_segment([hi, ho], Stroke::new(1.2, tokens.accent_secondary));
            painter.circle(
                ho,
                4.0,
                tokens.accent_secondary,
                Stroke::new(1.0, Color32::WHITE),
            );
            painter.circle(
                hi,
                4.0,
                tokens.accent_secondary,
                Stroke::new(1.0, Color32::WHITE),
            );
            painter.rect(
                Rect::from_center_size(a, vec2(7.0, 7.0)),
                CornerRadius::ZERO,
                Color32::WHITE,
                Stroke::new(1.5, tokens.accent),
                StrokeKind::Inside,
            );
        }
        CanvasDragState::DrawingBox {
            start_mm,
            current_mm,
        } => {
            if matches!(
                app.active_tool,
                ToolId::ThreePointCurve
                    | ToolId::ThreePointRectangle
                    | ToolId::ThreePointEllipse
                    | ToolId::StraightConnector
                    | ToolId::RightAngleConnector
            ) {
                let a = world_to_screen(canvas_rect, tab, *start_mm);
                let b = world_to_screen(canvas_rect, tab, *current_mm);
                painter.line_segment([a, b], Stroke::new(1.6, tokens.accent));
                painter.circle_filled(a, 3.5, tokens.accent);
                painter.circle_filled(b, 3.5, tokens.accent_secondary);
            } else {
                let box_mm = compute_constrained_box(*start_mm, *current_mm, ctrl, shift);
                let a = world_to_screen(canvas_rect, tab, box_mm.min());
                let b = world_to_screen(canvas_rect, tab, box_mm.max());
                let r = Rect::from_two_pos(a, b);
                if matches!(app.active_tool, ToolId::Ellipse | ToolId::LensTool) {
                    let rx = r.width() * 0.5;
                    let ry = r.height() * 0.5;
                    let c = r.center();
                    let pts: Vec<Pos2> = (0..=48)
                        .map(|i| {
                            let th = (i as f32 / 48.0) * std::f32::consts::TAU;
                            pos2(c.x + rx * th.cos(), c.y + ry * th.sin())
                        })
                        .collect();
                    painter.add(Shape::line(pts, Stroke::new(1.5, tokens.accent)));
                } else {
                    painter.rect_stroke(
                        r,
                        CornerRadius::ZERO,
                        Stroke::new(1.5, tokens.accent),
                        StrokeKind::Inside,
                    );
                }
            }
        }
        CanvasDragState::DrawingStroke { points_mm } => {
            if points_mm.len() >= 2 {
                let screen_pts: Vec<Pos2> = points_mm
                    .iter()
                    .map(|p| world_to_screen(canvas_rect, tab, *p))
                    .collect();
                let col = if app.active_tool == ToolId::Knife {
                    tokens.danger
                } else {
                    tokens.accent
                };
                painter.add(Shape::line(screen_pts, Stroke::new(1.8, col)));
            }
        }
        CanvasDragState::DraggingRulerGuide {
            horizontal,
            position_mm,
        } => {
            if *horizontal {
                let y = world_to_screen(canvas_rect, tab, Point2D::new(0.0, *position_mm)).y;
                painter.line_segment(
                    [pos2(canvas_rect.left(), y), pos2(canvas_rect.right(), y)],
                    Stroke::new(1.2, tokens.accent),
                );
            } else {
                let x = world_to_screen(canvas_rect, tab, Point2D::new(*position_mm, 0.0)).x;
                painter.line_segment(
                    [pos2(x, canvas_rect.top()), pos2(x, canvas_rect.bottom())],
                    Stroke::new(1.2, tokens.accent),
                );
            }
        }
        _ => {}
    }

    // 8. Live Rubber-Band Bezier/Pen Draft Curve Preview (+ 8px close indicator ring!)
    if !tab.bezier_draft_nodes.is_empty() {
        let mut preview_path = BezierPath::from_subpath(coree_core::Subpath {
            nodes: tab.bezier_draft_nodes.clone(),
            closed: false,
        });
        if let Some(sp) = preview_path.subpaths.first_mut() {
            sp.nodes
                .push(coree_core::PathNode::corner(tab.cursor_world_mm));
        }
        for poly in preview_path.flatten(0.4) {
            let spts: Vec<Pos2> = poly
                .into_iter()
                .map(|p| world_to_screen(canvas_rect, tab, p))
                .collect();
            if spts.len() >= 2 {
                painter.add(Shape::line(spts, Stroke::new(1.6, tokens.accent)));
            }
        }
        for (i, n) in tab.bezier_draft_nodes.iter().enumerate() {
            let sp = world_to_screen(canvas_rect, tab, n.pos);
            painter.rect(
                Rect::from_center_size(sp, vec2(6.5, 6.5)),
                CornerRadius::ZERO,
                if i == 0 {
                    tokens.accent_secondary
                } else {
                    Color32::WHITE
                },
                Stroke::new(1.4, tokens.accent),
                StrokeKind::Inside,
            );
        }
    }

    // 9. Live B-Spline / Polyline Rubber-Band Preview
    if matches!(app.active_tool, ToolId::BSpline | ToolId::Polyline)
        && !tab.poly_points_mm.is_empty()
    {
        let mut pts = tab.poly_points_mm.clone();
        pts.push(tab.cursor_world_mm);
        if app.active_tool == ToolId::BSpline {
            let spline = bspline_to_bezier_path(&pts, false);
            for poly in spline.flatten(0.4) {
                let spts: Vec<Pos2> = poly
                    .into_iter()
                    .map(|p| world_to_screen(canvas_rect, tab, p))
                    .collect();
                if spts.len() >= 2 {
                    painter.add(Shape::line(spts, Stroke::new(1.6, tokens.accent)));
                }
            }
        } else {
            let spts: Vec<Pos2> = pts
                .iter()
                .map(|p| world_to_screen(canvas_rect, tab, *p))
                .collect();
            painter.add(Shape::line(spts, Stroke::new(1.5, tokens.accent)));
        }
    }

    // 10. Live 3-Point Curve Arc Preview & 3-Point Rect/Ellipse Preview
    if let Some((p1, p2)) = tab.three_point_curve_chord {
        let arc = three_point_arc_path(p1, p2, tab.cursor_world_mm);
        for poly in arc.flatten(0.4) {
            let spts: Vec<Pos2> = poly
                .into_iter()
                .map(|p| world_to_screen(canvas_rect, tab, p))
                .collect();
            if spts.len() >= 2 {
                painter.add(Shape::line(spts, Stroke::new(1.8, tokens.accent_secondary)));
            }
        }
    }
    if let Some((_, p1, p2)) = tab.three_point_shape_chord {
        let base = p2 - p1;
        let len = base.length().max(1e-3);
        let u = base / len;
        let perp = Vec2D::new(-u.y, u.x);
        let h = (tab.cursor_world_mm - p1).dot(perp);
        let corners = [p1, p2, p2 + perp * h, p1 + perp * h, p1];
        let spts: Vec<Pos2> = corners
            .iter()
            .map(|p| world_to_screen(canvas_rect, tab, *p))
            .collect();
        painter.add(Shape::line(spts, Stroke::new(1.6, tokens.accent)));
    }

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
                if let Some((_, _, snap_pt)) = StudioApp::find_nearest_object_anchor(
                    &tab.document,
                    tab.document.active_page_index,
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
    let u = tab.document.default_unit;
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
            let val = u.from_mm(x, f64::from(tab.document.dpi));
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
            let val = u.from_mm(y, f64::from(tab.document.dpi));
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

fn hit_test_canvas_guide(
    doc: &coree_core::Document,
    pt_mm: Point2D,
    tol_mm: f64,
) -> Option<(u64, GuideOrientation, bool)> {
    for g in doc.guides.iter().rev() {
        let hit = match g.orientation {
            GuideOrientation::Horizontal(y_mm) => (pt_mm.y - y_mm).abs() <= tol_mm,
            GuideOrientation::Vertical(x_mm) => (pt_mm.x - x_mm).abs() <= tol_mm,
            GuideOrientation::Angled { origin, angle_deg } => {
                let rad = angle_deg.to_radians();
                let nx = -rad.sin();
                let ny = rad.cos();
                let dist = ((pt_mm.x - origin.x) * nx + (pt_mm.y - origin.y) * ny).abs();
                dist <= tol_mm
            }
        };
        if hit {
            return Some((g.id, g.orientation, g.locked));
        }
    }
    None
}

fn interactive_effect_handle_points(
    doc: &coree_core::Document,
    obj: &SceneObject,
    eff: &Effect,
    tool: ToolId,
) -> Option<(Point2D, Point2D)> {
    let center = doc
        .object_bounds(obj.id)
        .map_or(Point2D::ZERO, |b| b.center());
    match (tool, eff) {
        (ToolId::DropShadow, Effect::DropShadow { dx_mm, dy_mm, .. })
        | (ToolId::BlockShadow, Effect::BlockShadow { dx_mm, dy_mm, .. }) => {
            Some((center, Point2D::new(center.x + *dx_mm, center.y + *dy_mm)))
        }
        (ToolId::Contour, Effect::Contour { offset_mm, .. }) => {
            Some((center, Point2D::new(center.x + *offset_mm, center.y)))
        }
        (
            ToolId::Extrude25D,
            Effect::Extrude25D {
                vanishing_point, ..
            },
        ) => Some((center, *vanishing_point)),
        (
            ToolId::Distort,
            Effect::Distort {
                amplitude,
                center: lc,
                ..
            },
        ) => {
            let wc = obj.transform.transform_point(*lc);
            Some((wc, Point2D::new(wc.x + *amplitude, wc.y)))
        }
        (ToolId::Blend, Effect::Blend { target_id, .. }) => {
            let target_c = doc
                .object_bounds(*target_id)
                .map_or(Point2D::new(center.x + 20.0, center.y), |b| b.center());
            Some((center, target_c))
        }
        _ => None,
    }
}

fn gradient_ray_local(fill: &Fill) -> Option<(Point2D, Point2D, &[GradientStop])> {
    match fill {
        Fill::LinearGradient { start, end, stops } => Some((*start, *end, stops.as_slice())),
        Fill::RadialGradient {
            center,
            radius,
            stops,
            ..
        } => Some((
            *center,
            Point2D::new(center.x + radius.max(1.0), center.y),
            stops.as_slice(),
        )),
        Fill::ConicalGradient {
            center,
            angle_deg,
            stops,
        } => {
            let rad = angle_deg.to_radians();
            Some((
                *center,
                Point2D::new(center.x + 25.0 * rad.cos(), center.y + 25.0 * rad.sin()),
                stops.as_slice(),
            ))
        }
        Fill::SquareGradient {
            center,
            size,
            angle_deg,
            stops,
        } => {
            let rad = angle_deg.to_radians();
            let half = size.max(2.0) * 0.5;
            Some((
                *center,
                Point2D::new(center.x + half * rad.cos(), center.y + half * rad.sin()),
                stops.as_slice(),
            ))
        }
        _ => None,
    }
}

fn gradient_stops_mut(fill: &mut Fill) -> Option<(Point2D, Point2D, &mut Vec<GradientStop>)> {
    match fill {
        Fill::LinearGradient { start, end, stops } => Some((*start, *end, stops)),
        Fill::RadialGradient {
            center,
            radius,
            stops,
            ..
        } => Some((
            *center,
            Point2D::new(center.x + radius.max(1.0), center.y),
            stops,
        )),
        Fill::ConicalGradient {
            center,
            angle_deg,
            stops,
        } => {
            let rad = angle_deg.to_radians();
            Some((
                *center,
                Point2D::new(center.x + 25.0 * rad.cos(), center.y + 25.0 * rad.sin()),
                stops,
            ))
        }
        Fill::SquareGradient {
            center,
            size,
            angle_deg,
            stops,
        } => {
            let rad = angle_deg.to_radians();
            let half = size.max(2.0) * 0.5;
            Some((
                *center,
                Point2D::new(center.x + half * rad.cos(), center.y + half * rad.sin()),
                stops,
            ))
        }
        _ => None,
    }
}

fn interpolate_gradient_stop_color(stops: &[GradientStop], t: f32) -> Color {
    if stops.is_empty() {
        return Color::KEY_BLACK;
    }
    let mut sorted: Vec<&GradientStop> = stops.iter().collect();
    sorted.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    if t <= sorted[0].offset {
        return sorted[0].color.clone();
    }
    if t >= sorted[sorted.len() - 1].offset {
        return sorted[sorted.len() - 1].color.clone();
    }
    for win in sorted.windows(2) {
        let (a, b) = (win[0], win[1]);
        if t >= a.offset && t <= b.offset {
            let span = (b.offset - a.offset).max(1e-6);
            let frac = ((t - a.offset) / span).clamp(0.0, 1.0);
            let [r1, g1, b1, a1] = a.color.to_rgba_f32();
            let [r2, g2, b2, a2] = b.color.to_rgba_f32();
            return Color::Rgba {
                r: r1 + (r2 - r1) * frac,
                g: g1 + (g2 - g1) * frac,
                b: b1 + (b2 - b1) * frac,
                a: a1 + (a2 - a1) * frac,
            };
        }
    }
    sorted[0].color.clone()
}

fn try_insert_gradient_stop_on_ray(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    obj: &SceneObject,
    screen_pos: Pos2,
) -> Option<Fill> {
    let (start_l, end_l, stops) = gradient_ray_local(&obj.fill)?;
    let a = world_to_screen(canvas_rect, tab, obj.transform.transform_point(start_l));
    let b = world_to_screen(canvas_rect, tab, obj.transform.transform_point(end_l));
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq < 4.0 {
        return None;
    }
    let t = (screen_pos - a).dot(ab) / len_sq;
    if !(0.02..=0.98).contains(&t) {
        return None;
    }
    let closest = a + ab * t;
    if closest.distance(screen_pos) > 10.0 {
        return None;
    }
    for st in stops {
        let st_pt = a + ab * st.offset;
        if st_pt.distance(screen_pos) <= 7.0 {
            return None;
        }
    }
    let color = interpolate_gradient_stop_color(stops, t);
    let mut updated = obj.fill.clone();
    if let Some((_, _, stops_mut)) = gradient_stops_mut(&mut updated) {
        stops_mut.push(GradientStop::new(t, color));
        stops_mut.sort_by(|x, y| x.offset.total_cmp(&y.offset));
        Some(updated)
    } else {
        None
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
