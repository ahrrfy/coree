// SPDX-License-Identifier: MPL-2.0

//! Central Interactive Canvas Viewport, calibrated 20px Rulers (drag-to-guide),
//! dual-click Transform/Rotate/Skew handles with movable pivot crosshair, live
//! `F10` Bézier Node & handle editor, Interactive Fill gradient handles, live
//! drawing/cutting/brush tools, Page Tabs bar, Document Palette strip, and
//! bottom Status Bar with Live Path Length & Area Meter HUD.

use crate::state::{
    CanvasDragState, DockerTab, NodeDragPart, PropertiesSubTab, ScaleHandle, StudioApp, ToolId,
};
use crate::theme::{core_color_to_egui, PAGE_TABS_HEIGHT, RULER_THICKNESS, STATUS_BAR_HEIGHT};
use coree_core::{
    erase_circle, knife_cut, virtual_segment_delete, Affine2D, BezierPath, Color, CommandOp, Fill,
    GuideOrientation, LiveSketch, NodeKind, NodePayload, NodeRef, ObjectId, OutlineStyle, Point2D,
    Rect2D, SceneObject, SmartDrawing, TransformMode,
};
use coree_prod::simulate_overprint_rgba;
use coree_render::{
    clone_stamp_stroke, create_smart_fill_object, erase_stroke, hit_test_node, hit_test_point,
    hit_test_rect, paint_brush_stroke, smudge_stroke, SnapEngine,
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

                    // Active Stroke & Fill mini swatches
                    let (sw_rect, sw_resp) =
                        ui.allocate_exact_size(vec2(34.0, 14.0), Sense::click());
                    let r_fill = Rect::from_min_size(sw_rect.left_top(), vec2(14.0, 14.0));
                    let r_stroke =
                        Rect::from_min_size(sw_rect.left_top() + vec2(18.0, 0.0), vec2(14.0, 14.0));
                    ui.painter().rect(
                        r_fill,
                        CornerRadius::same(2),
                        fill_c
                            .as_ref()
                            .map_or(Color32::TRANSPARENT, core_color_to_egui),
                        Stroke::new(1.0, tokens.border_input),
                        StrokeKind::Inside,
                    );
                    ui.painter().rect(
                        r_stroke,
                        CornerRadius::same(2),
                        Color32::TRANSPARENT,
                        Stroke::new(
                            2.5,
                            stroke_c.as_ref().map_or(tokens.border, core_color_to_egui),
                        ),
                        StrokeKind::Inside,
                    );
                    if sw_resp
                        .on_hover_text("Active Fill & Outline (click to open Properties)")
                        .clicked()
                    {
                        app.properties_state.active_tab = PropertiesSubTab::Fill;
                        app.focus_docker(DockerTab::Properties);
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
            let total_rect = ui.available_rect_before_wrap();
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

            // Sync viewport size & render texture via `coree_render::DirtyTileCache`
            let canvas_w = canvas_rect.width().round().clamp(32.0, 4096.0) as u32;
            let canvas_h = canvas_rect.height().round().clamp(32.0, 4096.0) as u32;

            {
                let Some(tab) = app.active_tab_mut() else {
                    return;
                };
                if tab.viewport.width_px != canvas_w || tab.viewport.height_px != canvas_h {
                    tab.viewport.width_px = canvas_w;
                    tab.viewport.height_px = canvas_h;
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

            // Handle Ruler drag -> Create live Horizontal / Vertical Guidelines!
            handle_ruler_guide_drag(app, canvas_rect, &top_ruler_resp, &left_ruler_resp);

            // Handle Canvas pointer interactions (pan, cursor-centered wheel zoom, tools, handles, nodes!)
            handle_canvas_interaction(ui, app, canvas_rect, &canvas_resp);

            // Paint interactive overlays on top of canvas (selection handles, pivot, Bézier nodes, snap guides, drawing preview)
            paint_canvas_overlays(ui, app, canvas_rect);

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

fn screen_to_world(canvas_rect: Rect, tab: &crate::state::DocumentTab, screen: Pos2) -> Point2D {
    let local = Point2D::new(
        f64::from(screen.x - canvas_rect.left()),
        f64::from(screen.y - canvas_rect.top()),
    );
    tab.viewport.screen_to_world(local)
}

fn world_to_screen(canvas_rect: Rect, tab: &crate::state::DocumentTab, world: Point2D) -> Pos2 {
    let local = tab.viewport.world_to_screen(world);
    pos2(
        canvas_rect.left() + local.x as f32,
        canvas_rect.top() + local.y as f32,
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
                let local = Point2D::new(
                    f64::from(hover.x - canvas_rect.left()),
                    f64::from(hover.y - canvas_rect.top()),
                );
                let factor = if scroll_y > 0.0 { 1.12 } else { 1.0 / 1.12 };
                tab.viewport.zoom_at_screen_point(local, factor);
                tab.needs_rerender = true;
            }
        }
    }

    // 2. Middle-mouse pan anywhere
    let middle_down = ui.input(|i| i.pointer.middle_down());
    if middle_down && resp.hovered() {
        let delta = ui.input(|i| i.pointer.delta());
        if delta.length_sq() > 0.0 {
            if let Some(tab) = app.active_tab_mut() {
                let s = tab.viewport.px_per_mm().max(1e-4);
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

    let shift = ui.input(|i| i.modifiers.shift);
    let active_tool = app.active_tool;

    // 3. Click handling (Pick, Zoom, SmartFill, Eyedropper, Text, VirtualSegmentDelete, Polyline/Bezier)
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            match active_tool {
                ToolId::Pick | ToolId::FreehandPick | ToolId::FreeTransform => {
                    if let Some(tab) = app.active_tab_mut() {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        let tol = (6.0 / tab.viewport.px_per_mm()).max(0.8);
                        if let Some(hit_id) =
                            hit_test_point(&tab.document, tab.document.active_page_index, pt, tol)
                        {
                            tab.selection.click_object(&tab.document, hit_id, shift);
                        } else if !shift {
                            tab.selection.clear();
                        }
                        tab.metrics_dirty = true;
                    }
                }
                ToolId::Zoom => {
                    if let Some(tab) = app.active_tab_mut() {
                        let local = Point2D::new(
                            f64::from(pos.x - canvas_rect.left()),
                            f64::from(pos.y - canvas_rect.top()),
                        );
                        let factor = if shift { 0.7 } else { 1.4 };
                        tab.viewport.zoom_at_screen_point(local, factor);
                        tab.needs_rerender = true;
                    }
                }
                ToolId::ArtisticText => {
                    if let Some(pt) = app
                        .active_tab()
                        .map(|t| screen_to_world(canvas_rect, t, pos))
                    {
                        app.insert_artistic_text_at(pt);
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
                    let sampled = app.active_tab().and_then(|tab| {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        let tol = (6.0 / tab.viewport.px_per_mm()).max(1.0);
                        let oid =
                            hit_test_point(&tab.document, tab.document.active_page_index, pt, tol)?;
                        let obj = tab.document.objects.get(&oid)?;
                        Some((obj.fill.clone(), obj.outline.clone()))
                    });
                    if let Some((fill, outline)) = sampled {
                        app.default_fill = fill.clone();
                        if active_tool == ToolId::AttributesEyedropper {
                            app.default_outline = outline;
                            app.push_toast("Sampled Fill & Outline attributes", false);
                        } else {
                            app.push_toast("Sampled Fill color", false);
                        }
                    }
                }
                ToolId::VirtualSegmentDelete => {
                    let edit_op = app.active_tab().and_then(|tab| {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        let tol = (8.0 / tab.viewport.px_per_mm()).max(1.5);
                        let oid =
                            hit_test_point(&tab.document, tab.document.active_page_index, pt, tol)?;
                        let target_path = tab.document.world_path_of(oid)?;
                        let cutters: Vec<BezierPath> = tab
                            .document
                            .objects
                            .keys()
                            .filter(|&&id| id != oid)
                            .filter_map(|&id| tab.document.world_path_of(id))
                            .collect();
                        let updated_world = virtual_segment_delete(&target_path, &cutters, pt, tol);
                        let inv = tab.document.objects[&oid]
                            .transform
                            .inverse()
                            .unwrap_or(Affine2D::IDENTITY);
                        Some(CommandOp::EditPath {
                            id: oid,
                            path: updated_world.transformed(&inv),
                        })
                    });
                    if let Some(op) = edit_op {
                        let _ = app.execute_command(op);
                        app.push_toast("Deleted virtual curve segment", false);
                    }
                }
                ToolId::Bezier | ToolId::Pen | ToolId::BSpline | ToolId::Polyline => {
                    if let Some(tab) = app.active_tab_mut() {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        let (snapped, _) = SnapEngine::snap_point(
                            &tab.document,
                            tab.document.active_page_index,
                            pt,
                            &[],
                            &tab.snap_options,
                        );
                        tab.poly_points_mm.push(snapped);
                    }
                }
                ToolId::ShapeNode => {
                    if let Some(tab) = app.active_tab_mut() {
                        let pt = screen_to_world(canvas_rect, tab, pos);
                        let tol = (8.0 / tab.viewport.px_per_mm()).max(1.5);
                        if let Some(nh) =
                            hit_test_node(&tab.document, &tab.selection.objects, pt, tol)
                        {
                            if shift {
                                tab.selection.node_selection.toggle(nh.node);
                            } else {
                                tab.selection.node_selection.select_single(nh.node);
                            }
                        } else if let Some(oid) =
                            hit_test_point(&tab.document, tab.document.active_page_index, pt, tol)
                        {
                            tab.selection.select_single(&tab.document, oid);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Double-click finishes an open Bezier/Pen/Polyline multi-click curve
    if resp.double_clicked()
        && matches!(
            active_tool,
            ToolId::Bezier | ToolId::Pen | ToolId::BSpline | ToolId::Polyline
        )
    {
        let pts = app
            .active_tab_mut()
            .map(|t| std::mem::take(&mut t.poly_points_mm))
            .unwrap_or_default();
        if pts.len() >= 2 {
            let path = BezierPath::from_polygon(pts, false);
            let lid = app.active_tab().and_then(|t| t.active_layer_id);
            let obj = SceneObject::new(ObjectId(0), active_tool.label(), NodePayload::Path(path))
                .with_fill(Fill::None)
                .with_outline(app.default_outline.clone());
            let _ = app.execute_command(CommandOp::create_object(obj, lid));
        }
    }

    // Right-click zooms out in Zoom tool
    if resp.secondary_clicked() && active_tool == ToolId::Zoom {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(tab) = app.active_tab_mut() {
                let local = Point2D::new(
                    f64::from(pos.x - canvas_rect.left()),
                    f64::from(pos.y - canvas_rect.top()),
                );
                tab.viewport.zoom_at_screen_point(local, 0.7);
                tab.needs_rerender = true;
            }
        }
    }

    // 4. Drag start / Dragging / Drag stop
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            start_canvas_drag(app, canvas_rect, pos, active_tool);
        }
    } else if resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            update_canvas_drag(app, canvas_rect, pos);
        }
    } else if resp.drag_stopped() {
        finish_canvas_drag(app, active_tool);
    }
}

fn start_canvas_drag(app: &mut StudioApp, canvas_rect: Rect, screen_pos: Pos2, tool: ToolId) {
    let Some(tab) = app.active_tab_mut() else {
        return;
    };
    let pt = screen_to_world(canvas_rect, tab, screen_pos);
    let tol_mm = (7.0 / tab.viewport.px_per_mm()).max(1.2);

    match tool {
        ToolId::Pan => {
            tab.drag_state = CanvasDragState::Panning {
                start_screen: [screen_pos.x, screen_pos.y],
                orig_pan_mm: tab.viewport.pan_mm,
            };
            tab.viewport.lod_dragging = true;
        }
        ToolId::Pick | ToolId::FreehandPick | ToolId::FreeTransform => {
            // 1. Check active selection handles (Scale or Rotate/Skew + Movable Pivot)
            if let Some(bb) = tab.selection.bounding_box(&tab.document) {
                match tab.selection.transform_mode {
                    TransformMode::ScaleAndMove => {
                        if let Some(handle) = hit_scale_handle(canvas_rect, tab, bb, screen_pos) {
                            tab.drag_state = CanvasDragState::ScalingSelection {
                                handle,
                                orig_bounds: bb,
                                current_mm: pt,
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
                                };
                                tab.viewport.lod_dragging = true;
                                return;
                            }
                        }
                    }
                }
            }

            // 2. Hit object -> Move selection
            if let Some(hit_id) =
                hit_test_point(&tab.document, tab.document.active_page_index, pt, tol_mm)
            {
                if !tab.selection.contains(hit_id) {
                    tab.selection.select_single(&tab.document, hit_id);
                }
                tab.drag_state = CanvasDragState::MovingSelection {
                    start_mm: pt,
                    last_mm: pt,
                    accum_dx: 0.0,
                    accum_dy: 0.0,
                };
                tab.viewport.lod_dragging = true;
            } else {
                // 3. Empty pasteboard -> Marquee rubber-band selection
                tab.drag_state = CanvasDragState::MarqueeSelect {
                    start_mm: pt,
                    current_mm: pt,
                };
            }
        }
        ToolId::ShapeNode => {
            // Check control handles first, then anchor nodes on selected Path objects
            if let Some((nref, part, orig_path)) =
                hit_editable_bezier_node_or_handle(canvas_rect, tab, screen_pos)
            {
                tab.selection.node_selection.select_single(nref);
                tab.drag_state = CanvasDragState::DraggingNode {
                    node: nref,
                    handle_part: part,
                    orig_path,
                };
                tab.viewport.lod_dragging = true;
            }
        }
        ToolId::InteractiveFill => {
            if let Some(&oid) = tab.selection.objects.first() {
                if let Some(obj) = tab.document.objects.get(&oid) {
                    if let Fill::LinearGradient { start, end, .. } = &obj.fill {
                        let s_pos = world_to_screen(
                            canvas_rect,
                            tab,
                            obj.transform.transform_point(*start),
                        );
                        let e_pos =
                            world_to_screen(canvas_rect, tab, obj.transform.transform_point(*end));
                        if s_pos.distance(screen_pos) <= 10.0 {
                            tab.drag_state = CanvasDragState::DraggingGradientHandle {
                                object_id: oid,
                                is_end: false,
                                orig_fill: obj.fill.clone(),
                            };
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

fn update_canvas_drag(app: &mut StudioApp, canvas_rect: Rect, screen_pos: Pos2) {
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
            let s = tab.viewport.px_per_mm().max(1e-4);
            let dx_px = f64::from(screen_pos.x - start_screen[0]);
            let dy_px = f64::from(screen_pos.y - start_screen[1]);
            tab.viewport.pan_mm =
                Point2D::new(orig_pan_mm.x - dx_px / s, orig_pan_mm.y - dy_px / s);
            tab.needs_rerender = true;
        }
        CanvasDragState::MarqueeSelect { current_mm, .. } => {
            *current_mm = pt;
        }
        CanvasDragState::MovingSelection {
            last_mm,
            accum_dx,
            accum_dy,
            ..
        } => {
            let (snapped, guides) = SnapEngine::snap_point(
                &tab.document,
                tab.document.active_page_index,
                pt,
                &tab.selection.objects,
                &tab.snap_options,
            );
            tab.active_guides = guides;
            let dx = snapped.x - last_mm.x;
            let dy = snapped.y - last_mm.y;
            if dx.abs() > 1e-6 || dy.abs() > 1e-6 {
                let delta = Affine2D::translate(dx, dy);
                for &id in &tab.selection.objects {
                    if let Some(obj) = tab.document.objects.get_mut(&id) {
                        if !obj.locked {
                            obj.transform = delta.compose(&obj.transform);
                        }
                    }
                }
                *accum_dx += dx;
                *accum_dy += dy;
                *last_mm = snapped;
                tab.tile_cache.mark_all_dirty();
                tab.needs_rerender = true;
            }
        }
        CanvasDragState::ScalingSelection { current_mm, .. } => {
            *current_mm = pt;
        }
        CanvasDragState::RotatingSelection {
            pivot_mm,
            current_angle_rad,
            ..
        } => {
            *current_angle_rad = (pt.y - pivot_mm.y).atan2(pt.x - pivot_mm.x);
        }
        CanvasDragState::SkewingSelection { current_mm, .. } => {
            *current_mm = pt;
        }
        CanvasDragState::DraggingPivot => {
            tab.selection.set_pivot(pt);
        }
        CanvasDragState::DraggingNode {
            node, handle_part, ..
        } => {
            if let Some(obj) = tab.document.objects.get_mut(&node.object_id) {
                let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                let local_pt = inv.transform_point(pt);
                if let NodePayload::Path(ref mut path) = obj.payload {
                    if let Some(sp) = path.subpaths.get_mut(node.subpath_index) {
                        if let Some(pnode) = sp.nodes.get_mut(node.node_index) {
                            match handle_part {
                                NodeDragPart::Anchor => {
                                    let delta = local_pt - pnode.pos;
                                    pnode.translate(delta);
                                }
                                NodeDragPart::HandleIn => {
                                    pnode.handle_in = Some(local_pt);
                                    pnode.enforce_continuity();
                                }
                                NodeDragPart::HandleOut => {
                                    pnode.handle_out = Some(local_pt);
                                    pnode.enforce_continuity();
                                }
                            }
                        }
                    }
                }
                tab.tile_cache.mark_all_dirty();
                tab.needs_rerender = true;
            }
        }
        CanvasDragState::DraggingGradientHandle {
            object_id, is_end, ..
        } => {
            if let Some(obj) = tab.document.objects.get_mut(object_id) {
                let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                let local_pt = inv.transform_point(pt);
                if let Fill::LinearGradient { start, end, .. } = &mut obj.fill {
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
            if points_mm
                .last()
                .is_none_or(|last| last.distance_to(pt) > 0.35)
            {
                points_mm.push(pt);
            }
        }
        CanvasDragState::Idle | CanvasDragState::DraggingRulerGuide { .. } => {}
    }
    tab.drag_state = state;
}

fn finish_canvas_drag(app: &mut StudioApp, active_tool: ToolId) {
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
                tab.selection.select_many(&tab.document, &hits);
                tab.metrics_dirty = true;
            }
        }
        CanvasDragState::MovingSelection {
            accum_dx, accum_dy, ..
        } => {
            if accum_dx.abs() > 1e-5 || accum_dy.abs() > 1e-5 {
                // Roll back interactive live delta, then commit cleanly through CommandHistory!
                let ids = app
                    .active_tab()
                    .map(|t| t.selection.objects.clone())
                    .unwrap_or_default();
                if let Some(tab) = app.active_tab_mut() {
                    let inv = Affine2D::translate(-accum_dx, -accum_dy);
                    for &id in &ids {
                        if let Some(obj) = tab.document.objects.get_mut(&id) {
                            if !obj.locked {
                                obj.transform = inv.compose(&obj.transform);
                            }
                        }
                    }
                }
                let _ = app.execute_command(CommandOp::TransformObjects {
                    ids,
                    affine: Affine2D::translate(accum_dx, accum_dy),
                });
            }
        }
        CanvasDragState::ScalingSelection {
            handle,
            orig_bounds,
            current_mm,
        } => {
            let ids = app
                .active_tab()
                .map(|t| t.selection.objects.clone())
                .unwrap_or_default();
            if let Some(affine) = compute_scale_affine(handle, orig_bounds, current_mm) {
                let _ = app.execute_command(CommandOp::TransformObjects { ids, affine });
            }
        }
        CanvasDragState::RotatingSelection {
            pivot_mm,
            start_angle_rad,
            current_angle_rad,
        } => {
            let delta_rad = current_angle_rad - start_angle_rad;
            if delta_rad.abs() > 1e-4 {
                let ids = app
                    .active_tab()
                    .map(|t| t.selection.objects.clone())
                    .unwrap_or_default();
                let affine = Affine2D::rotate_about(delta_rad, pivot_mm);
                let _ = app.execute_command(CommandOp::TransformObjects { ids, affine });
            }
        }
        CanvasDragState::SkewingSelection {
            horizontal,
            pivot_mm,
            start_mm,
            current_mm,
        } => {
            let ids = app
                .active_tab()
                .map(|t| t.selection.objects.clone())
                .unwrap_or_default();
            let (sx, sy) = if horizontal {
                (((current_mm.x - start_mm.x) / 60.0).clamp(-1.0, 1.0), 0.0)
            } else {
                (0.0, ((current_mm.y - start_mm.y) / 60.0).clamp(-1.0, 1.0))
            };
            let affine = Affine2D::translate(pivot_mm.x, pivot_mm.y)
                .compose(&Affine2D::skew(sx, sy))
                .compose(&Affine2D::translate(-pivot_mm.x, -pivot_mm.y));
            let _ = app.execute_command(CommandOp::TransformObjects { ids, affine });
        }
        CanvasDragState::DraggingNode {
            node, orig_path, ..
        } => {
            // Capture final edited path, restore orig_path, then commit cleanly through CommandHistory!
            let final_path = app.active_tab_mut().and_then(|tab| {
                let obj = tab.document.objects.get_mut(&node.object_id)?;
                let NodePayload::Path(ref mut p) = obj.payload else {
                    return None;
                };
                let edited = p.clone();
                *p = orig_path;
                Some(edited)
            });
            if let Some(path) = final_path {
                let _ = app.execute_command(CommandOp::EditPath {
                    id: node.object_id,
                    path,
                });
            }
        }
        CanvasDragState::DraggingGradientHandle {
            object_id,
            orig_fill,
            ..
        } => {
            // Capture edited fill, restore orig_fill, and commit cleanly through CommandHistory + WAL!
            let final_fill = app.active_tab_mut().and_then(|tab| {
                let obj = tab.document.objects.get_mut(&object_id)?;
                let edited = obj.fill.clone();
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
        CanvasDragState::DrawingBox {
            start_mm,
            current_mm,
        } => {
            if active_tool == ToolId::InteractiveFill {
                let per_obj_ops: Vec<CommandOp> = app
                    .active_tab()
                    .map(|t| {
                        t.selection
                            .objects
                            .iter()
                            .copied()
                            .filter_map(|oid| {
                                let obj = t.document.objects.get(&oid)?;
                                let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                                let local_start = inv.transform_point(start_mm);
                                let local_end = inv.transform_point(current_mm);
                                let fill = Fill::LinearGradient {
                                    start: local_start,
                                    end: local_end,
                                    stops: vec![
                                        coree_core::GradientStop::new(
                                            0.0,
                                            app.properties_state.grad_start_color.clone(),
                                        ),
                                        coree_core::GradientStop::new(
                                            1.0,
                                            app.properties_state.grad_end_color.clone(),
                                        ),
                                    ],
                                };
                                Some(CommandOp::SetFill {
                                    ids: vec![oid],
                                    fill,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if per_obj_ops.len() == 1 {
                    let _ = app.execute_command(per_obj_ops.into_iter().next().unwrap());
                } else if !per_obj_ops.is_empty() {
                    let _ = app.execute_command(CommandOp::Batch(per_obj_ops));
                }
            } else {
                let rect = Rect2D::from_points(start_mm, current_mm);
                app.commit_drawn_box(rect);
            }
        }
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
            let path = BezierPath::from_polygon(points, false);
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
            let a = points[0];
            let b = *points.last().unwrap();
            let knife_plan = app.active_tab().and_then(|tab| {
                let oid = *tab.selection.objects.first()?;
                let wp = tab.document.world_path_of(oid)?;
                let obj = tab.document.objects.get(&oid)?;
                let pieces = knife_cut(&wp, a, b, true);
                if pieces.len() <= 1 {
                    return None;
                }
                let mut next_id = tab.document.next_id.max(1);
                let mut new_ids = Vec::with_capacity(pieces.len());
                let mut ops = vec![CommandOp::DeleteObjects { ids: vec![oid] }];
                for (i, piece) in pieces.into_iter().enumerate() {
                    let nid = ObjectId(next_id);
                    next_id = next_id.saturating_add(1);
                    new_ids.push(nid);
                    let mut new_obj = SceneObject::new(
                        nid,
                        format!("{} Slice {}", obj.name, i + 1),
                        NodePayload::Path(piece),
                    );
                    new_obj.fill = obj.fill.clone();
                    new_obj.outline = obj.outline.clone();
                    ops.push(CommandOp::create_object(new_obj, layer_id));
                }
                Some((CommandOp::Batch(ops), new_ids))
            });
            if let Some((op, new_ids)) = knife_plan {
                if app.execute_command(op).is_ok() {
                    if let Some(tab) = app.active_tab_mut() {
                        tab.selection.select_many(&tab.document, &new_ids);
                        tab.metrics_dirty = true;
                    }
                    app.push_toast(
                        format!(
                            "Knife sliced selected shape into {} piece(s)",
                            new_ids.len()
                        ),
                        false,
                    );
                }
            }
        }
        ToolId::Eraser => {
            let radius = app.tool_settings.eraser_radius_mm.max(0.25);
            let step = (radius * 0.4).max(0.25);
            let edit = app.active_tab().and_then(|tab| {
                let oid = *tab.selection.objects.first()?;
                let wp = tab.document.world_path_of(oid)?;

                // Build densely sampled centers along every segment of the eraser stroke
                let mut centers = Vec::with_capacity(points.len());
                centers.push(points[0]);
                for seg in points.windows(2) {
                    let dist = seg[0].distance_to(seg[1]);
                    let steps = ((dist / step).ceil() as usize).clamp(1, 256);
                    for k in 1..=steps {
                        let t = k as f64 / steps as f64;
                        centers.push(seg[0].lerp(seg[1], t));
                    }
                }

                // Apply erase_circle sequentially along the entire stroke across surviving pieces
                let mut pieces = vec![wp];
                for center in centers {
                    let mut next_pieces = Vec::new();
                    for piece in &pieces {
                        if !piece.is_empty() {
                            next_pieces.extend(erase_circle(piece, center, radius));
                        }
                    }
                    pieces = next_pieces;
                    if pieces.is_empty() {
                        break;
                    }
                }

                let combined = BezierPath {
                    subpaths: pieces.into_iter().flat_map(|p| p.subpaths).collect(),
                };
                let inv = tab.document.objects[&oid]
                    .transform
                    .inverse()
                    .unwrap_or(Affine2D::IDENTITY);
                Some(CommandOp::EditPath {
                    id: oid,
                    path: combined.transformed(&inv),
                })
            });
            if let Some(op) = edit {
                let _ = app.execute_command(op);
            }
        }
        ToolId::BitmapBrush | ToolId::BitmapEraser | ToolId::CloneStamp | ToolId::Smudge => {
            let r_px = app.tool_settings.brush_radius_px;
            let hard = app.tool_settings.brush_hardness;
            let col = app.tool_settings.brush_color.clone();
            let smudge_str = app.tool_settings.smudge_strength;
            let mut modified_raster = false;
            if let Some(tab) = app.active_tab_mut() {
                let target = tab.selection.objects.iter().copied().find(|id| {
                    tab.document
                        .objects
                        .get(id)
                        .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
                });
                if let Some(oid) = target {
                    let (aid, w_mm, h_mm, inv_xf) = {
                        let obj = &tab.document.objects[&oid];
                        let NodePayload::Raster(ref r) = obj.payload else {
                            return;
                        };
                        (
                            r.asset_id,
                            r.width_mm.max(1e-3),
                            r.height_mm.max(1e-3),
                            obj.transform.inverse().unwrap_or(Affine2D::IDENTITY),
                        )
                    };
                    if let Some(asset) = tab.document.assets.get_mut(&aid) {
                        let px_pts: Vec<Point2D> = points
                            .into_iter()
                            .map(|p| {
                                let lp = inv_xf.transform_point(p);
                                Point2D::new(
                                    (lp.x / w_mm) * f64::from(asset.width),
                                    (lp.y / h_mm) * f64::from(asset.height),
                                )
                            })
                            .collect();
                        match tool {
                            ToolId::BitmapBrush => {
                                paint_brush_stroke(asset, &px_pts, r_px, hard, col, 1.0);
                            }
                            ToolId::BitmapEraser => {
                                erase_stroke(asset, &px_pts, r_px, hard);
                            }
                            ToolId::CloneStamp => {
                                let src_origin = px_pts
                                    .first()
                                    .map(|p| {
                                        Point2D::new((p.x - 16.0).max(0.0), (p.y - 16.0).max(0.0))
                                    })
                                    .unwrap_or(Point2D::ZERO);
                                clone_stamp_stroke(asset, src_origin, &px_pts, r_px, hard);
                            }
                            ToolId::Smudge => {
                                smudge_stroke(asset, &px_pts, r_px, smudge_str);
                            }
                            _ => {}
                        }
                        tab.mark_modified();
                        modified_raster = true;
                    }
                }
            }
            if modified_raster {
                app.checkpoint_wal_snapshot_active();
            }
        }
        _ => {}
    }
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

fn compute_scale_affine(handle: ScaleHandle, orig: Rect2D, cur: Point2D) -> Option<Affine2D> {
    let ow = orig.width().max(1e-3);
    let oh = orig.height().max(1e-3);
    let (anchor, sx, sy) = match handle {
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
    };
    if (sx - 1.0).abs() < 1e-4 && (sy - 1.0).abs() < 1e-4 {
        return None;
    }
    Some(
        Affine2D::translate(anchor.x, anchor.y)
            .compose(&Affine2D::scale(sx, sy))
            .compose(&Affine2D::translate(-anchor.x, -anchor.y)),
    )
}

fn hit_editable_bezier_node_or_handle(
    canvas_rect: Rect,
    tab: &crate::state::DocumentTab,
    screen_pos: Pos2,
) -> Option<(NodeRef, NodeDragPart, BezierPath)> {
    for &oid in &tab.selection.objects {
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
// Interactive Overlays (Selection Box, Dual-Click Handles, Pivot, Bézier Nodes, Snap Guides)
// =============================================================================

fn paint_canvas_overlays(ui: &egui::Ui, app: &StudioApp, canvas_rect: Rect) {
    let Some(tab) = app.active_tab() else {
        return;
    };
    let tokens = app.theme.tokens();
    let painter = ui.painter().with_clip_rect(canvas_rect);

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
                    // Movable Pivot Crosshair
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

    // 3. `F10` Shape / Node Editing Overlay (Nodes + Cubic Bézier Control Handles)
    if app.active_tool == ToolId::ShapeNode {
        for &oid in &tab.selection.objects {
            if let Some(obj) = tab.document.objects.get(&oid) {
                let path = match &obj.payload {
                    NodePayload::Path(p) => Some(p.clone()),
                    NodePayload::Shape(s) => Some(s.to_bezier_path()),
                    _ => None,
                };
                if let Some(p) = path {
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
            }
        }
    }

    // 4. Interactive Fill Gradient Vector Overlay
    if app.active_tool == ToolId::InteractiveFill {
        if let Some(&oid) = tab.selection.objects.first() {
            if let Some(obj) = tab.document.objects.get(&oid) {
                if let Fill::LinearGradient { start, end, .. } = &obj.fill {
                    let a =
                        world_to_screen(canvas_rect, tab, obj.transform.transform_point(*start));
                    let b = world_to_screen(canvas_rect, tab, obj.transform.transform_point(*end));
                    painter.line_segment([a, b], Stroke::new(2.0, tokens.accent));
                    painter.rect(
                        Rect::from_center_size(a, vec2(9.0, 9.0)),
                        CornerRadius::same(2),
                        Color32::WHITE,
                        Stroke::new(2.0, tokens.accent),
                        StrokeKind::Inside,
                    );
                    painter.circle(b, 5.0, tokens.accent, Stroke::new(1.5, Color32::WHITE));
                }
            }
        }
    }

    // 5. Active Dragging Previews (Marquee, Drawing Box, Freehand/Knife Stroke, Ruler Guide)
    match &tab.drag_state {
        CanvasDragState::MarqueeSelect {
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
        CanvasDragState::DrawingBox {
            start_mm,
            current_mm,
        } => {
            let a = world_to_screen(canvas_rect, tab, *start_mm);
            let b = world_to_screen(canvas_rect, tab, *current_mm);
            let r = Rect::from_two_pos(a, b);
            painter.rect_stroke(
                r,
                CornerRadius::ZERO,
                Stroke::new(1.5, tokens.accent),
                StrokeKind::Inside,
            );
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

    // Multi-click Polyline / Pen preview
    if tab.poly_points_mm.len() >= 2 {
        let pts: Vec<Pos2> = tab
            .poly_points_mm
            .iter()
            .map(|p| world_to_screen(canvas_rect, tab, *p))
            .collect();
        painter.add(Shape::line(pts, Stroke::new(1.5, tokens.accent)));
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

    let px_per_mm = tab.viewport.px_per_mm().max(0.05);
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
