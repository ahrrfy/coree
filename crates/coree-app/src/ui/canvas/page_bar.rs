// SPDX-License-Identifier: MIT OR Apache-2.0

//! Direct GPU `256 × 256` World-Tile Texture Compositor, Scrollable Multi-Page
//! Navigator Strip (`1 .. 1,000+` pages), and Floating Async Background Task HUD.

use super::*;
use coree_render::TILE_SIZE_PX;
use egui::epaint::Vertex;

/// Uploads only newly rasterized `256 × 256` world tiles to OpenGL/GPU textures and paints
/// all visible viewport tiles directly as hardware-composited quads (`0` full-screen CPU buffer
/// allocation and `0` texture uploads during cached pan/zoom).
pub(super) fn paint_viewport_tiles_gpu(
    ui: &mut Ui,
    canvas_rect: Rect,
    tab: &mut crate::state::DocumentTab,
    max_gpu_textures: usize,
) {
    let _rendered = tab.tile_cache.render_dirty_tiles(
        &tab.document,
        &tab.viewport,
        tab.view_mode,
        &tab.render_options,
        &mut tab.renderer,
    );

    tab.gpu_tile_frame = tab.gpu_tile_frame.wrapping_add(1);
    let cur_frame = tab.gpu_tile_frame;
    let tex_opts = TextureOptions {
        magnification: egui::TextureFilter::Linear,
        minification: egui::TextureFilter::Linear,
        wrap_mode: egui::TextureWrapMode::ClampToEdge,
        mipmap_mode: None,
    };

    // Synchronize GPU TextureHandles ONLY for visible tiles that are missing or newly re-rendered
    let ctx = ui.ctx().clone();
    let tab_id = tab.id;
    let mut draw_list: Vec<(egui::TextureId, Rect2D)> =
        Vec::with_capacity(tab.tile_cache.tile_count());

    {
        let tile_views: Vec<(coree_render::WorldTileKey, u64, Rect2D)> = tab
            .tile_cache
            .active_gpu_tiles()
            .map(|v| (v.key, v.revision, v.world_rect_mm))
            .collect();

        for (key, rev, world_rect_mm) in tile_views {
            let up_to_date = tab
                .gpu_tile_textures
                .get(&key)
                .is_some_and(|&(cached_rev, _, _)| cached_rev == rev && rev > 0);
            if !up_to_date {
                if let Some(view) = tab.tile_cache.active_gpu_tiles().find(|v| v.key == key) {
                    if !view.buffer.is_empty() {
                        let img = ColorImage::from_rgba_unmultiplied(
                            [view.buffer.width as usize, view.buffer.height as usize],
                            &view.buffer.rgba,
                        );
                        if let Some(entry) = tab.gpu_tile_textures.get_mut(&key) {
                            entry.2.set(img, tex_opts);
                            entry.0 = rev;
                            entry.1 = cur_frame;
                        } else {
                            let tex = ctx.load_texture(
                                format!(
                                    "coree_tile_{}_{}_{}_{}_{}",
                                    tab_id, key.page_scope, key.lod_level, key.tile_x, key.tile_y
                                ),
                                img,
                                tex_opts,
                            );
                            tab.gpu_tile_textures.insert(key, (rev, cur_frame, tex));
                        }
                    }
                }
            } else if let Some(entry) = tab.gpu_tile_textures.get_mut(&key) {
                entry.1 = cur_frame;
            }

            if let Some((_, _, ref tex)) = tab.gpu_tile_textures.get(&key) {
                draw_list.push((tex.id(), world_rect_mm));
            }
        }
    }

    tab.trim_gpu_tile_textures(max_gpu_textures.max(64));
    tab.needs_rerender = false;

    let painter = ui.painter().with_clip_rect(canvas_rect);
    // Fill pasteboard well behind tiles so edges during extreme rotation/zoom are clean
    painter.rect_filled(
        canvas_rect,
        CornerRadius::ZERO,
        core_color_to_egui(&tab.render_options.canvas_bg),
    );

    let uv_full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let rotated = tab.viewport.rotation_deg.abs() > 1e-6;
    let _ = TILE_SIZE_PX;

    for (tex_id, wr) in draw_list {
        if !rotated {
            let p00 = world_to_screen(canvas_rect, tab, Point2D::new(wr.min_x, wr.min_y));
            let p11 = world_to_screen(canvas_rect, tab, Point2D::new(wr.max_x, wr.max_y));
            let tile_rect = Rect::from_two_pos(p00, p11);
            if tile_rect.intersects(canvas_rect) {
                painter.image(tex_id, tile_rect, uv_full, Color32::WHITE);
            }
        } else {
            let p00 = world_to_screen(canvas_rect, tab, Point2D::new(wr.min_x, wr.min_y));
            let p10 = world_to_screen(canvas_rect, tab, Point2D::new(wr.max_x, wr.min_y));
            let p11 = world_to_screen(canvas_rect, tab, Point2D::new(wr.max_x, wr.max_y));
            let p01 = world_to_screen(canvas_rect, tab, Point2D::new(wr.min_x, wr.max_y));
            let mut mesh = egui::Mesh::with_texture(tex_id);
            mesh.vertices = vec![
                Vertex {
                    pos: p00,
                    uv: pos2(0.0, 0.0),
                    color: Color32::WHITE,
                },
                Vertex {
                    pos: p10,
                    uv: pos2(1.0, 0.0),
                    color: Color32::WHITE,
                },
                Vertex {
                    pos: p11,
                    uv: pos2(1.0, 1.0),
                    color: Color32::WHITE,
                },
                Vertex {
                    pos: p01,
                    uv: pos2(0.0, 1.0),
                    color: Color32::WHITE,
                },
            ];
            mesh.indices = vec![0, 1, 2, 0, 2, 3];
            painter.add(egui::Shape::mesh(mesh));
        }
    }
}

/// Renders the bottom Page Tabs bar with compact First/Prev/Jump/Next/Last navigation
/// and bounded horizontal scrolling so `27` to `1,000+` page documents never crowd out
/// `+`, `Multipage View`, or `Doc Colors`.
pub(super) fn show_page_tabs_and_doc_palette(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let page_lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(page_lang);

    egui::Panel::bottom("coree_page_tabs")
        .exact_size(PAGE_TABS_HEIGHT)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(6, 2))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            let total_w = ui.available_width();
            let mut add_page_req = false;
            let mut delete_page_req = None;
            let mut duplicate_page_req = None;
            let mut rename_page_req: Option<(coree_core::PageId, String)> = None;
            let mut reorder_pages_req: Option<Vec<coree_core::PageId>> = None;
            let mut multipage_toggle_req: Option<bool> = None;
            let mut apply_doc_fill: Option<Color> = None;
            let mut apply_doc_stroke: Option<Color> = None;
            let mut jump_to_page_idx: Option<usize> = None;

            shell_dir.with_horizontal(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if let Some(tab) = app.active_tab_mut() {
                    let active_idx = tab.document.active_page_index();
                    let page_ids: Vec<coree_core::PageId> =
                        tab.document.pages().iter().map(|p| p.id).collect();
                    let page_count = page_ids.len();

                    // Compact navigation controls + quick Go-To-Page popup for multi-page documents
                    if page_count > 1 {
                        if ui
                            .add_enabled(active_idx > 0, egui::Button::new("⏮").small())
                            .on_hover_text(crate::i18n::I18nKey::PageNavFirstTip.tr_ui(page_lang))
                            .clicked()
                        {
                            jump_to_page_idx = Some(0);
                        }
                        if ui
                            .add_enabled(active_idx > 0, egui::Button::new("◀").small())
                            .on_hover_text(crate::i18n::I18nKey::PageNavPrevTip.tr_ui(page_lang))
                            .clicked()
                        {
                            jump_to_page_idx = Some(active_idx.saturating_sub(1));
                        }

                        egui::ComboBox::from_id_salt("page_bar_quick_jump")
                            .width(74.0)
                            .selected_text(
                                RichText::new(format!("{} / {page_count}", active_idx + 1))
                                    .monospace()
                                    .size(11.0)
                                    .color(tokens.accent),
                            )
                            .show_ui(ui, |ui| {
                                ui.label(
                                    RichText::new(
                                        crate::i18n::I18nKey::PageNavJumpHint.tr_ui(page_lang),
                                    )
                                    .size(10.5)
                                    .color(tokens.text_muted),
                                );
                                egui::ScrollArea::vertical()
                                    .max_height(240.0)
                                    .show(ui, |ui| {
                                        for p_i in 0..page_count {
                                            let p_lbl = format!(
                                                "{}. {}",
                                                p_i + 1,
                                                tab.document.pages()[p_i].name
                                            );
                                            if ui
                                                .selectable_label(p_i == active_idx, p_lbl)
                                                .clicked()
                                            {
                                                jump_to_page_idx = Some(p_i);
                                            }
                                        }
                                    });
                            });

                        if ui
                            .add_enabled(
                                active_idx + 1 < page_count,
                                egui::Button::new("▶").small(),
                            )
                            .on_hover_text(crate::i18n::I18nKey::PageNavNextTip.tr_ui(page_lang))
                            .clicked()
                        {
                            jump_to_page_idx = Some((active_idx + 1).min(page_count - 1));
                        }
                        if ui
                            .add_enabled(
                                active_idx + 1 < page_count,
                                egui::Button::new("⏭").small(),
                            )
                            .on_hover_text(crate::i18n::I18nKey::PageNavLastTip.tr_ui(page_lang))
                            .clicked()
                        {
                            jump_to_page_idx = Some(page_count - 1);
                        }
                        ui.separator();
                    }

                    // Bounded horizontal ScrollArea so 27..500+ page tabs scroll smoothly without
                    // pushing `+`, `Multipage View`, or `Doc Colors` off-screen!
                    let tabs_cap = if total_w.is_finite() && total_w > 0.0 {
                        (total_w * 0.54).max(32.0)
                    } else {
                        32.0
                    };
                    let tabs_floor = 180.0_f32.min(tabs_cap);
                    let max_tabs_w = (total_w - 430.0).clamp(tabs_floor, tabs_cap);
                    let prev_tracked_id = ui.scope_id().with("last_scrolled_page_idx");
                    let last_scrolled: Option<usize> =
                        ui.ctx().data(|d| d.get_temp::<usize>(prev_tracked_id));
                    let should_auto_scroll = last_scrolled != Some(active_idx);
                    if should_auto_scroll {
                        ui.ctx()
                            .data_mut(|d| d.insert_temp(prev_tracked_id, active_idx));
                    }

                    ui.allocate_ui(vec2(max_tabs_w, PAGE_TABS_HEIGHT - 4.0), |ui| {
                        egui::ScrollArea::horizontal()
                            .id_salt("coree_page_tabs_scroll")
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .max_width(max_tabs_w)
                            .show(ui, |ui| {
                                shell_dir.with_horizontal(ui, |ui| {
                                    ui.spacing_mut().item_spacing.x = 3.0;
                                    for idx in 0..page_count {
                                        let page_name = tab.document.pages()[idx].name.clone();
                                        let page_id = page_ids[idx];
                                        let is_active = idx == active_idx;

                                        if tab.inline_renaming_page == Some(idx) {
                                            let resp = ui.add(
                                                egui::TextEdit::singleline(
                                                    &mut tab.inline_page_name_buf,
                                                )
                                                .desired_width(88.0),
                                            );
                                            if resp.lost_focus()
                                                || ui.input(|i| i.key_pressed(egui::Key::Enter))
                                            {
                                                let new_name =
                                                    tab.inline_page_name_buf.trim().to_owned();
                                                if !new_name.is_empty() {
                                                    rename_page_req = Some((page_id, new_name));
                                                }
                                                tab.inline_renaming_page = None;
                                            }
                                            continue;
                                        }

                                        let btn =
                                            ui.add(
                                                egui::Button::new(
                                                    RichText::new(&page_name).size(11.0).color(
                                                        if is_active {
                                                            tokens.text
                                                        } else {
                                                            tokens.text_muted
                                                        },
                                                    ),
                                                )
                                                .sense(Sense::click_and_drag())
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
                                        if is_active && should_auto_scroll {
                                            btn.scroll_to_me(Some(egui::Align::Center));
                                        }
                                        if btn.drag_started() {
                                            egui::DragAndDrop::set_payload(ui.ctx(), idx);
                                        }
                                        if let Some(src_idx) = btn.dnd_release_payload::<usize>() {
                                            let src = *src_idx;
                                            if src < page_count && src != idx {
                                                let mut reordered = page_ids.clone();
                                                let moved = reordered.remove(src);
                                                reordered.insert(idx, moved);
                                                reorder_pages_req = Some(reordered);
                                            }
                                        }
                                        if btn.clicked() {
                                            jump_to_page_idx = Some(idx);
                                        }
                                        if btn.double_clicked() {
                                            tab.inline_renaming_page = Some(idx);
                                            tab.inline_page_name_buf = page_name.clone();
                                        }
                                        btn.context_menu(|ui| {
                                            let lang = page_lang;
                                            if ui
                                                .button(
                                                    crate::i18n::I18nKey::LayoutRenamePage
                                                        .tr_ui(lang),
                                                )
                                                .clicked()
                                            {
                                                tab.inline_renaming_page = Some(idx);
                                                tab.inline_page_name_buf = page_name.clone();
                                                ui.close();
                                            }
                                            if ui
                                                .button(
                                                    crate::i18n::I18nKey::LayoutDuplicatePage
                                                        .tr_ui(lang),
                                                )
                                                .clicked()
                                            {
                                                duplicate_page_req = Some(idx);
                                                ui.close();
                                            }
                                            if idx > 0
                                                && ui
                                                    .button(
                                                        crate::i18n::I18nKey::PageMoveLeft
                                                            .tr_ui(lang),
                                                    )
                                                    .clicked()
                                            {
                                                let mut reordered = page_ids.clone();
                                                reordered.swap(idx - 1, idx);
                                                reorder_pages_req = Some(reordered);
                                                ui.close();
                                            }
                                            if idx + 1 < page_count
                                                && ui
                                                    .button(
                                                        crate::i18n::I18nKey::PageMoveRight
                                                            .tr_ui(lang),
                                                    )
                                                    .clicked()
                                            {
                                                let mut reordered = page_ids.clone();
                                                reordered.swap(idx, idx + 1);
                                                reorder_pages_req = Some(reordered);
                                                ui.close();
                                            }
                                            if ui
                                                .add_enabled(
                                                    page_count > 1,
                                                    egui::Button::new(
                                                        crate::i18n::I18nKey::LayoutDeletePage
                                                            .tr_ui(lang),
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                delete_page_req = Some(page_id);
                                                ui.close();
                                            }
                                        });
                                    }
                                });
                            });
                    });

                    if ui
                        .small_button(RichText::new("+").strong().color(tokens.accent))
                        .on_hover_text(crate::i18n::I18nKey::PageAddButton.tr_ui(page_lang))
                        .clicked()
                    {
                        add_page_req = true;
                    }

                    ui.separator();
                    if ui
                        .selectable_label(
                            tab.document.multipage_view(),
                            format!("⊞ {}", crate::i18n::I18nKey::ViewMultipage.tr_ui(page_lang)),
                        )
                        .clicked()
                    {
                        let next = !tab.document.multipage_view();
                        multipage_toggle_req = Some(next);
                        tab.viewport.multipage_view = next;
                        tab.needs_rerender = true;
                    }

                    // Document Palette Strip (always visible!)
                    ui.separator();
                    ui.label(
                        RichText::new(crate::i18n::I18nKey::StatusDocColorsLabel.tr_ui(page_lang))
                            .size(10.5)
                            .color(tokens.text_muted),
                    );
                    for sw in tab.document.document_palette().swatches.iter().take(12) {
                        let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
                        ui.painter().rect(
                            r,
                            CornerRadius::same(2),
                            core_color_to_egui(&sw.color),
                            Stroke::new(0.8, tokens.border_input),
                            StrokeKind::Inside,
                        );
                        let resp = resp.on_hover_text(format!(
                            "{} ({})",
                            sw.name,
                            crate::i18n::I18nKey::StatusFillOutlineHint.tr_ui(page_lang)
                        ));
                        if resp.clicked() {
                            apply_doc_fill = Some(sw.color.clone());
                        } else if resp.secondary_clicked() {
                            apply_doc_stroke = Some(sw.color.clone());
                        }
                    }

                    if let Some(target_idx) = jump_to_page_idx {
                        if target_idx < tab.document.pages().len() && target_idx != active_idx {
                            let _ = tab.document.set_active_page_index(target_idx);
                            tab.selection.clear();
                            tab.sync_active_layer();
                            tab.metrics_dirty = true;
                            tab.needs_rerender = true;
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
            if let Some(page_ids) = reorder_pages_req {
                let _ = app.execute_command(CommandOp::ReorderPages { page_ids });
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
                    drawing_scale: None,
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

/// Renders a sleek non-blocking floating progress banner with Cancel button at the top-right
/// of the canvas whenever heavy background operations (Open PDF/CDR, Import, Trace, Export) run.
pub(super) fn show_background_jobs_hud(ui: &Ui, app: &mut StudioApp, canvas_rect: Rect) {
    let Some((label, pct, count)) = app.active_background_progress_summary() else {
        return;
    };
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    let anchor = pos2(
        (canvas_rect.right() - 295.0).max(canvas_rect.left() + 8.0),
        canvas_rect.top() + 38.0,
    );
    let mut cancel_req = false;

    egui::Area::new(ui.scope_id().with("coree_bg_jobs_hud"))
        .order(egui::Order::Foreground)
        .fixed_pos(anchor)
        .show(ui.ctx(), |ui| {
            Frame::NONE
                .fill(tokens.surface_elevated.gamma_multiply(0.96))
                .stroke(Stroke::new(1.2, tokens.accent))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_min_width(265.0);
                    shell_dir.with_horizontal(ui, |ui| {
                        let title = if count > 1 {
                            format!("⏳ {label} (+{})", count - 1)
                        } else {
                            format!("⏳ {label}")
                        };
                        ui.label(RichText::new(title).size(11.5).strong().color(tokens.text));
                        shell_dir.with_opposite_horizontal(ui, |ui| {
                            if ui
                                .small_button(
                                    RichText::new(format!(
                                        "✕ {}",
                                        crate::i18n::I18nKey::CanvasBgTaskCancel.tr_ui(lang)
                                    ))
                                    .size(10.5)
                                    .color(tokens.danger),
                                )
                                .clicked()
                            {
                                cancel_req = true;
                            }
                        });
                    });
                    ui.add_space(3.0);
                    ui.add(
                        egui::ProgressBar::new(f32::from(pct) / 100.0)
                            .desired_width(265.0)
                            .desired_height(6.0)
                            .show_percentage(),
                    );
                });
        });

    if cancel_req {
        app.cancel_all_background_jobs();
    }
}
