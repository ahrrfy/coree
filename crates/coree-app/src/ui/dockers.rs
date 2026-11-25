// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rearrangeable, tabbable, splittable, and floatable studio Dockers powered by
//! `egui_dock 0.22` covering all 14 production panels:
//! `Objects`, `Properties`, `Lens` (all 11 Lenses), `Effects`, `Transform/Align/Shape`,
//! `Vector Trace`, `Bitmap Studio` (all 19 filters), `Production & Prepress`,
//! `Find & Replace`, `Symbols & Styles`, `Font Manager`, `History`, `Macros`, and
//! `CDR Inspector`.

use crate::io_bridge::{read_text_file_bounded, MAX_TEXT_READ_BYTES};
use crate::state::{
    BitmapDockerState, DockerTab, LensDockerState, PropertiesSubTab, RasterBrushOp, StudioApp,
};
use crate::theme::{
    core_color_to_egui, dock_style, egui_color_to_core, DOCKER_DEFAULT_WIDTH, DOCKER_MAX_WIDTH,
    DOCKER_MIN_WIDTH,
};
use crate::ui::dialogs::{
    cached_detect_executable, install_sample_plugin_action, reload_plugins_action,
};
use coree_core::{
    Affine2D, AlignOp, ArrowheadStyle, BlendMode, BooleanOp, Color, ColorMapMode, CommandOp,
    ContourMode, DistortKind, DistributeOp, EnvelopeMode, Fill, GradientStop, LineCap, LineJoin,
    MasterLayerScope, NodePayload, ObjectId, ObjectStyle, OutlineStyle, Point2D,
    PostScriptFillPreset, Rect2D, SymbolDef, TextAlign, TextDirection, Vec2D, ZOrderOp,
};
use coree_prod::{
    builtin_macros, default_laser_layer_rules, export_laser_dxf_r12, export_laser_gcode,
    export_laser_plt, export_laser_svg, plan_laser_toolpath, plan_rhai_plugin_ops,
    BarcodeSymbology, Code128Mode, FefcoCode, FindScope, FluteType, GcodeDialect, HalftoneDotShape,
    LaserLayerRule, LaserOperationKind, LaserOrigin, LensKindFilter, MacroSandboxConfig,
    ObjectKindFilter, OpenLaserProtocol, PluginKind, PluginManifest, PreflightSeverity,
    QrErrorCorrection, ReplaceAction, TrapPlacement,
};
use coree_render::{list_glyphs_for_block, GlyphBlockFilter, OutlinePreset, ToneRange};
use egui::{
    vec2, Color32, ComboBox, CornerRadius, DragValue, Frame, Id, Margin, RichText, Stroke, Ui,
    WidgetText,
};
use egui_dock::{DockArea, DockState, TabViewer};
use std::collections::BTreeMap;
use std::path::PathBuf;

mod effects_lens;
mod find_symbols_fonts;
mod inspector_laser;
mod production;
mod properties;
mod raster_trace;
#[cfg(test)]
mod tests;

/// Renders the right resizable `egui_dock::DockArea` panel containing all open studio dockers.
pub fn show_dockers_panel(ui: &mut Ui, app: &mut StudioApp) {
    if !app.show_dockers {
        return;
    }
    let tokens = app.theme.tokens();
    let d_style = dock_style(&tokens, ui.style());
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(app.ui_language);

    let panel = if shell_dir.dockers_on_left {
        egui::Panel::left("coree_dockers_host")
    } else {
        egui::Panel::right("coree_dockers_host")
    };
    panel
        .resizable(true)
        .default_size(DOCKER_DEFAULT_WIDTH)
        .size_range(DOCKER_MIN_WIDTH..=DOCKER_MAX_WIDTH)
        .frame(
            Frame::NONE
                .fill(tokens.bg)
                .inner_margin(Margin::same(0))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            // Temporarily move DockState out so TabViewer can borrow `&mut StudioApp` cleanly
            let mut dock_state = std::mem::replace(&mut app.dock_state, DockState::new(Vec::new()));
            let mut viewer = StudioDockViewer { app };
            DockArea::new(&mut dock_state)
                .id(Id::unique("coree_studio_dock_area"))
                .style(d_style)
                .show_close_buttons(true)
                .show_leaf_collapse_buttons(true)
                .show_inside(ui, &mut viewer);
            app.dock_state = dock_state;
        });
}

struct StudioDockViewer<'a> {
    app: &'a mut StudioApp,
}

impl TabViewer for StudioDockViewer<'_> {
    type Tab = DockerTab;

    fn id(&mut self, tab: &mut Self::Tab) -> Id {
        Id::unique(("coree_docker_tab", *tab))
    }

    fn title(&mut self, tab: &mut Self::Tab) -> WidgetText {
        tab.title_for(self.app.ui_language).into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
        let shell_dir = crate::i18n::ShellLayoutDirections::from_language(self.app.ui_language);
        egui::ScrollArea::vertical()
            .id_salt(("docker_scroll", *tab))
            .show(ui, |ui| {
                shell_dir.with_vertical(ui, |ui| match *tab {
                    DockerTab::Objects => ui_objects_docker(ui, self.app),
                    DockerTab::Properties => properties::ui_properties_docker(ui, self.app),
                    DockerTab::Lens => effects_lens::ui_lens_docker(ui, self.app),
                    DockerTab::Effects => effects_lens::ui_effects_docker(ui, self.app),
                    DockerTab::TransformAlignShape => {
                        effects_lens::ui_transform_align_shape_docker(ui, self.app)
                    }
                    DockerTab::VectorTrace => raster_trace::ui_vector_trace_docker(ui, self.app),
                    DockerTab::BitmapStudio => raster_trace::ui_bitmap_studio_docker(ui, self.app),
                    DockerTab::Production => production::ui_production_docker(ui, self.app),
                    DockerTab::FindReplace => {
                        find_symbols_fonts::ui_find_replace_docker(ui, self.app)
                    }
                    DockerTab::SymbolsStyles => {
                        find_symbols_fonts::ui_symbols_styles_docker(ui, self.app)
                    }
                    DockerTab::FontManager => {
                        find_symbols_fonts::ui_font_manager_docker(ui, self.app)
                    }
                    DockerTab::History => find_symbols_fonts::ui_history_docker(ui, self.app),
                    DockerTab::Macros => find_symbols_fonts::ui_macros_docker(ui, self.app),
                    DockerTab::CdrInspector => {
                        inspector_laser::ui_cdr_inspector_docker(ui, self.app)
                    }
                });
            });
    }
}

// =============================================================================
// 1. Objects & Layers Docker (`S05`)
// =============================================================================

fn ui_objects_docker(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let lang = app.ui_language;
    let shell_dir = crate::i18n::ShellLayoutDirections::from_language(lang);
    shell_dir.with_horizontal(ui, |ui| {
        if ui
            .small_button(crate::i18n::I18nKey::ObjAddLayer.tr_ui(lang))
            .clicked()
        {
            let n = app
                .active_tab()
                .and_then(|t| t.document.active_page())
                .map_or(2, |p| p.layers.len() + 1);
            app.add_layer_active(format!("Layer {n}"), false, MasterLayerScope::None);
        }
        if ui
            .small_button(crate::i18n::I18nKey::ObjAddCutLayer.tr_ui(lang))
            .clicked()
        {
            app.add_layer_active("CutContour".to_owned(), true, MasterLayerScope::None);
        }
        if ui
            .small_button(crate::i18n::I18nKey::ObjAddMasterLayer.tr_ui(lang))
            .clicked()
        {
            app.add_layer_active("Master".to_owned(), false, MasterLayerScope::AllPages);
        }
        if ui
            .small_button(crate::i18n::I18nKey::ObjDesktopLayer.tr_ui(lang))
            .on_hover_text(crate::i18n::I18nKey::ObjectMoveToDesktop.tr_ui(lang))
            .clicked()
        {
            app.move_selection_to_desktop_layer();
        }
    });
    shell_dir.with_horizontal(ui, |ui| {
        if ui
            .small_button(format!(
                "⬆ {}",
                crate::i18n::I18nKey::ObjectOrderToFront.tr_ui(lang)
            ))
            .clicked()
        {
            app.apply_z_order(ZOrderOp::BringToFront);
        }
        if ui
            .small_button(format!(
                "↑ {}",
                crate::i18n::I18nKey::ObjectOrderForwardOne.tr_ui(lang)
            ))
            .clicked()
        {
            app.apply_z_order(ZOrderOp::BringForward);
        }
        if ui
            .small_button(format!(
                "↓ {}",
                crate::i18n::I18nKey::ObjectOrderBackOne.tr_ui(lang)
            ))
            .clicked()
        {
            app.apply_z_order(ZOrderOp::SendBackward);
        }
        if ui
            .small_button(format!(
                "⬇ {}",
                crate::i18n::I18nKey::ObjectOrderToBack.tr_ui(lang)
            ))
            .clicked()
        {
            app.apply_z_order(ZOrderOp::SendToBack);
        }
    });
    shell_dir.with_horizontal(ui, |ui| {
        ui.label(crate::i18n::I18nKey::ObjSearchFilter.tr_ui(lang));
        ui.add(
            egui::TextEdit::singleline(&mut app.objects_filter)
                .desired_width(160.0)
                .hint_text(crate::i18n::I18nKey::DockObjSearchHint.tr_ui(lang)),
        );
        if !app.objects_filter.is_empty() && ui.small_button("✕").clicked() {
            app.objects_filter.clear();
        }
    });
    ui.separator();

    let filter_trim = app.objects_filter.trim().to_owned();
    let parsed_cql = if filter_trim.starts_with('@') {
        coree_prod::parse_cql_query(&filter_trim).ok()
    } else {
        None
    };

    let mut flag_ops = Vec::new();
    let mut reorder_layer_cmd: Option<CommandOp> = None;
    let mut reorder_objs_cmd: Option<CommandOp> = None;
    let mut fit_powerclip_req: Option<(ObjectId, coree_core::PowerClipFitMode)> = None;
    let mut obj_click_before_sel: Option<Vec<ObjectId>> = None;

    {
        let Some(tab) = app.active_tab_mut() else {
            ui.label(crate::i18n::I18nKey::ErrorNoActiveDocument.tr_ui(lang));
            return;
        };

        let page_count = tab.document.pages().len();

        for p_idx in 0..page_count {
            let page_id = tab.document.pages()[p_idx].id;
            let p_name = tab.document.pages()[p_idx].name.clone();
            let is_active_page = p_idx == tab.document.active_page_index();
            let hdr = if is_active_page {
                RichText::new(format!("📄 {p_name}"))
                    .strong()
                    .color(tokens.accent)
            } else {
                RichText::new(format!("📄 {p_name}")).color(tokens.text_muted)
            };
            egui::CollapsingHeader::new(hdr)
                .id_salt(("obj_page", p_idx))
                .default_open(is_active_page)
                .show(ui, |ui| {
                    let bottom_to_top_layer_ids: Vec<_> = tab.document.pages()[p_idx]
                        .layers
                        .iter()
                        .map(|l| l.id)
                        .collect();
                    let layer_ids: Vec<_> = tab.document.pages()[p_idx]
                        .layers
                        .iter()
                        .rev()
                        .map(|l| {
                            (
                                l.id,
                                l.name.clone(),
                                l.visible,
                                l.locked,
                                l.printable,
                                l.is_cut_layer,
                                l.is_desktop_layer,
                                l.master_scope,
                                l.objects.clone(),
                            )
                        })
                        .collect();

                    for (lid, lname, vis, lck, prn, is_cut, is_desktop, scope, objs) in layer_ids {
                        shell_dir.with_horizontal(ui, |ui| {
                            let is_act_layer = tab.active_layer_id == Some(lid);
                            let layer_btn = ui.add(
                                egui::Button::new(RichText::new(&lname).strong().color(
                                    if is_act_layer {
                                        tokens.accent
                                    } else {
                                        tokens.text
                                    },
                                ))
                                .frame(is_act_layer)
                                .sense(egui::Sense::click_and_drag()),
                            );
                            if layer_btn.drag_started() {
                                egui::DragAndDrop::set_payload(ui.ctx(), (page_id, lid));
                            }
                            if let Some(payload) = layer_btn
                                .dnd_release_payload::<(coree_core::PageId, coree_core::LayerId)>()
                            {
                                let (src_pid, src_lid) = *payload;
                                if src_pid == page_id && src_lid != lid {
                                    let mut next = bottom_to_top_layer_ids.clone();
                                    if let (Some(from_i), Some(to_i)) = (
                                        next.iter().position(|&x| x == src_lid),
                                        next.iter().position(|&x| x == lid),
                                    ) {
                                        let item = next.remove(from_i);
                                        next.insert(to_i, item);
                                        reorder_layer_cmd = Some(CommandOp::ReorderLayers {
                                            page_id,
                                            layer_ids: next,
                                        });
                                    }
                                }
                            }
                            if layer_btn.clicked() {
                                tab.active_layer_id = Some(lid);
                                let _ = tab.document.set_active_page_index(p_idx);
                            }
                            if ui
                                .small_button(
                                    RichText::new(
                                        crate::i18n::I18nKey::DockObjCutBadge.tr_ui(lang),
                                    )
                                    .size(9.5)
                                    .color(if is_cut {
                                        tokens.cutcontour
                                    } else {
                                        tokens.text_muted
                                    }),
                                )
                                .on_hover_text(crate::i18n::I18nKey::ObjToggleCutLayer.tr_ui(lang))
                                .clicked()
                            {
                                flag_ops.push((lid, None, None, None, Some(!is_cut)));
                            }
                            if is_desktop {
                                ui.label(
                                    RichText::new(
                                        crate::i18n::I18nKey::DockObjDesktopBadge.tr_ui(lang),
                                    )
                                    .size(9.5)
                                    .color(tokens.accent),
                                );
                            } else if scope != MasterLayerScope::None {
                                ui.label(
                                    RichText::new(
                                        crate::i18n::I18nKey::DockObjMasterBadge.tr_ui(lang),
                                    )
                                    .size(9.5)
                                    .color(tokens.accent_secondary),
                                );
                            }
                            if ui
                                .small_button(if vis { "👁" } else { "—" })
                                .on_hover_text(crate::i18n::I18nKey::ObjToggleVisible.tr_ui(lang))
                                .clicked()
                            {
                                flag_ops.push((lid, Some(!vis), None, None, None));
                            }
                            if ui
                                .small_button(if lck { "🔒" } else { "🔓" })
                                .on_hover_text(crate::i18n::I18nKey::ObjToggleLocked.tr_ui(lang))
                                .clicked()
                            {
                                flag_ops.push((lid, None, Some(!lck), None, None));
                            }
                            if ui
                                .small_button(if prn { "🖨" } else { "⊘" })
                                .on_hover_text(crate::i18n::I18nKey::ObjTogglePrintable.tr_ui(lang))
                                .clicked()
                            {
                                flag_ops.push((lid, None, None, Some(!prn), None));
                            }
                        });

                        ui.indent(("layer_objs", lid), |ui| {
                            for &oid in objs.iter().rev() {
                                if let Some(obj) = tab.document.object(oid) {
                                    if !filter_trim.is_empty() {
                                        let matches = if let Some(ref expr) = parsed_cql {
                                            coree_prod::cql_matches_object(obj, expr)
                                        } else {
                                            obj.name
                                                .to_ascii_lowercase()
                                                .contains(&filter_trim.to_ascii_lowercase())
                                        };
                                        if !matches {
                                            continue;
                                        }
                                    }
                                    let selected = tab.selection.contains(oid);
                                    let is_powerclip =
                                        matches!(obj.payload, NodePayload::PowerClip { .. });
                                    let badge = match &obj.payload {
                                        NodePayload::Group(c) => format!(" [Grp:{}]", c.len()),
                                        NodePayload::PowerClip { contents, .. } => {
                                            format!(" [Clip:{}]", contents.len())
                                        }
                                        NodePayload::Raster(_) => " [Img]".to_owned(),
                                        NodePayload::ArtisticText(_)
                                        | NodePayload::ParagraphText(_) => " [Txt]".to_owned(),
                                        _ => String::new(),
                                    };
                                    let lens_tag = obj
                                        .lens
                                        .as_ref()
                                        .map(|l| format!(" ◉{}", l.kind.name()))
                                        .unwrap_or_default();
                                    let label = format!("{}{badge}{lens_tag}", obj.name);
                                    shell_dir.with_horizontal(ui, |ui| {
                                        let obj_btn = ui
                                            .add(
                                                egui::Button::new(RichText::new(label).color(
                                                    if selected {
                                                        tokens.accent
                                                    } else {
                                                        tokens.text
                                                    },
                                                ))
                                                .frame(selected)
                                                .sense(egui::Sense::click_and_drag()),
                                            )
                                            .on_hover_text(
                                                crate::i18n::I18nKey::ObjDragReorderHint
                                                    .tr_ui(lang),
                                            );
                                        if obj_btn.drag_started() {
                                            egui::DragAndDrop::set_payload(ui.ctx(), (lid, oid));
                                        }
                                        if let Some(payload) = obj_btn.dnd_release_payload::<(
                                            coree_core::LayerId,
                                            ObjectId,
                                        )>(
                                        ) {
                                            let (src_lid, src_oid) = *payload;
                                            if src_lid == lid && src_oid != oid {
                                                let mut next = objs.clone();
                                                if let (Some(from_i), Some(to_i)) = (
                                                    next.iter().position(|&x| x == src_oid),
                                                    next.iter().position(|&x| x == oid),
                                                ) {
                                                    let item = next.remove(from_i);
                                                    next.insert(to_i, item);
                                                    reorder_objs_cmd =
                                                        Some(CommandOp::ReorderLayerObjects {
                                                            layer_id: lid,
                                                            object_ids: next,
                                                        });
                                                }
                                            }
                                        }
                                        if obj_btn.clicked() {
                                            obj_click_before_sel =
                                                Some(tab.selection.objects.clone());
                                            let _ = tab.document.set_active_page_index(p_idx);
                                            tab.active_layer_id = Some(lid);
                                            tab.sync_active_layer();
                                            tab.selection.select_single(&tab.document, oid);
                                            tab.metrics_dirty = true;
                                            tab.needs_rerender = true;
                                        }
                                        if is_powerclip {
                                            if ui.small_button("Fit").clicked() {
                                                fit_powerclip_req = Some((
                                                    oid,
                                                    coree_core::PowerClipFitMode::FitContents,
                                                ));
                                            }
                                            if ui.small_button("Ctr").clicked() {
                                                fit_powerclip_req = Some((
                                                    oid,
                                                    coree_core::PowerClipFitMode::CenterContents,
                                                ));
                                            }
                                            if ui.small_button("Str").clicked() {
                                                fit_powerclip_req = Some((
                                                    oid,
                                                    coree_core::PowerClipFitMode::StretchToFill,
                                                ));
                                            }
                                        }
                                    });
                                }
                            }
                        });
                    }
                });
        }
    }

    if let Some(prev_sel) = obj_click_before_sel {
        app.maybe_fire_selection_change(&prev_sel);
    }

    for (layer_id, visible, locked, printable, is_cut_layer) in flag_ops {
        let _ = app.execute_command(CommandOp::SetLayerFlags {
            layer_id,
            visible,
            locked,
            printable,
            is_cut_layer,
        });
    }
    if let Some(cmd) = reorder_layer_cmd {
        let _ = app.execute_command(cmd);
    }
    if let Some(cmd) = reorder_objs_cmd {
        let _ = app.execute_command(cmd);
    }
    if let Some((pc_id, mode)) = fit_powerclip_req {
        let _ = app.execute_command(CommandOp::FitPowerClip { id: pc_id, mode });
    }
}

pub(super) fn render_trace_split_preview(ui: &mut Ui, app: &mut StudioApp) {
    let lang = app.ui_language;
    let tokens = app.theme.tokens();
    let tr = &mut app.trace_state;
    ui.checkbox(
        &mut tr.split_preview,
        crate::i18n::I18nKey::TraceSplitPreview.tr_ui(lang),
    );

    if tr.split_preview {
        let preview_src = app.active_tab().and_then(|tab| {
            let target_id = tab
                .selection
                .objects
                .iter()
                .copied()
                .find(|id| {
                    tab.document
                        .object(*id)
                        .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
                })
                .or_else(|| {
                    tab.document
                        .objects()
                        .iter()
                        .find(|(_, o)| matches!(o.payload, NodePayload::Raster(_)))
                        .map(|(&id, _)| id)
                })?;
            let NodePayload::Raster(ref r) = tab.document.object(target_id)?.payload else {
                return None;
            };
            let asset = tab.document.asset(r.asset_id)?;
            Some((target_id, r.clone(), asset.clone()))
        });
        if let Some((oid, r, asset)) = preview_src {
            let cfg = app.trace_state.to_config();
            let mut key: u64 = oid.0 ^ ((asset.width as u64) << 16) ^ (asset.height as u64);
            for b in format!("{:?}", cfg).as_bytes() {
                key = key.wrapping_mul(0x0100_0000_01b3) ^ u64::from(*b);
            }
            let cached_paths = {
                let mut cache = app.trace_state.split_preview_cache.borrow_mut();
                if cache.as_ref().is_some_and(|(k, _)| *k == key) {
                    cache.as_ref().map(|(_, v)| v.clone()).unwrap_or_default()
                } else {
                    let traced = coree_render::trace_raster_to_mm(
                        &asset,
                        r.width_mm.max(1.0),
                        r.height_mm.max(1.0),
                        &cfg,
                    );
                    *cache = Some((key, traced.clone()));
                    traced
                }
            };
            let (rect, resp) =
                ui.allocate_exact_size(vec2(220.0, 110.0), egui::Sense::click_and_drag());
            let resp = resp.on_hover_text(crate::i18n::I18nKey::TraceSplitDivider.tr_ui(lang));
            if (resp.clicked() || resp.dragged()) && rect.width() > 1.0 {
                if let Some(pos) = resp.interact_pointer_pos() {
                    app.trace_state.split_divider =
                        ((pos.x - rect.left()) / rect.width()).clamp(0.05, 0.95);
                }
            }
            let split_x =
                rect.left() + rect.width() * app.trace_state.split_divider.clamp(0.05, 0.95);
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, CornerRadius::same(4), tokens.canvas_well);

            // Left half: source bitmap grid sample (Before)
            let left_clip = egui::Rect::from_min_max(rect.min, egui::pos2(split_x, rect.max.y));
            let left_painter = painter.with_clip_rect(left_clip);
            let cols = 16u32.min(asset.width.max(1));
            let rows = 10u32.min(asset.height.max(1));
            let rgba = asset.to_rgba8_vec();
            let cell_w = rect.width() / cols as f32;
            let cell_h = rect.height() / rows as f32;
            for ry in 0..rows {
                let sy = ((ry as f32 + 0.5) / rows as f32 * asset.height as f32) as u32;
                let sy = sy.min(asset.height.saturating_sub(1));
                for cx in 0..cols {
                    let sx = ((cx as f32 + 0.5) / cols as f32 * asset.width as f32) as u32;
                    let sx = sx.min(asset.width.saturating_sub(1));
                    let idx = ((sy * asset.width + sx) * 4) as usize;
                    if idx + 3 < rgba.len() {
                        let c = Color32::from_rgba_unmultiplied(
                            rgba[idx],
                            rgba[idx + 1],
                            rgba[idx + 2],
                            rgba[idx + 3],
                        );
                        left_painter.rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(
                                    rect.left() + cx as f32 * cell_w,
                                    rect.top() + ry as f32 * cell_h,
                                ),
                                vec2(cell_w + 0.5, cell_h + 0.5),
                            ),
                            CornerRadius::ZERO,
                            c,
                        );
                    }
                }
            }

            // Right half: live traced vector contours (After)
            let right_clip = egui::Rect::from_min_max(egui::pos2(split_x, rect.min.y), rect.max);
            let right_painter = painter.with_clip_rect(right_clip);
            right_painter.rect_filled(right_clip, CornerRadius::ZERO, Color32::WHITE);
            let sx = rect.width() / (r.width_mm.max(1.0) as f32);
            let sy = rect.height() / (r.height_mm.max(1.0) as f32);
            for (path, col) in cached_paths.iter().take(120) {
                let c32 = core_color_to_egui(col);
                for sub in &path.subpaths {
                    let pts: Vec<egui::Pos2> = sub
                        .nodes
                        .iter()
                        .map(|n| {
                            egui::pos2(
                                rect.left() + n.pos.x as f32 * sx,
                                rect.top() + n.pos.y as f32 * sy,
                            )
                        })
                        .collect();
                    if pts.len() >= 2 {
                        right_painter.add(egui::Shape::line(pts, Stroke::new(1.2, c32)));
                    }
                }
            }
            painter.line_segment(
                [
                    egui::pos2(split_x, rect.top()),
                    egui::pos2(split_x, rect.bottom()),
                ],
                Stroke::new(2.0, tokens.accent),
            );
            painter.text(
                rect.left_top() + vec2(4.0, 2.0),
                egui::Align2::LEFT_TOP,
                crate::i18n::I18nKey::DockTraceBefore.tr_ui(lang),
                egui::FontId::proportional(9.5),
                tokens.text,
            );
            painter.text(
                rect.right_top() + vec2(-4.0, 2.0),
                egui::Align2::RIGHT_TOP,
                format!(
                    "{} ({})",
                    crate::i18n::I18nKey::DockTraceAfter.tr_ui(lang),
                    cached_paths.len()
                ),
                egui::FontId::proportional(9.5),
                Color32::BLACK,
            );
        } else {
            ui.label(
                RichText::new(crate::i18n::I18nKey::DockTraceNoBitmap.tr_ui(lang))
                    .size(10.5)
                    .color(tokens.text_muted),
            );
        }
    }
}

pub(super) fn hex_view(
    ui: &mut Ui,
    ch: &coree_cdr::ChunkSummary,
    lang: crate::theme::StudioLanguage,
) {
    let hex_str = if ch.preview_bytes.is_empty() {
        format!(
            "'{}' ({} B): <container/no inline payload bytes>",
            ch.fourcc, ch.size
        )
    } else {
        format!(
            "'{}' ({} B) Hex / ASCII:\n{}",
            ch.fourcc,
            ch.size,
            coree_cdr::format_hex_ascii_dump(&ch.preview_bytes, 0)
        )
    };
    ui.separator();
    ui.label(
        RichText::new(crate::i18n::I18nKey::CdrHexAsciiViewer.tr_ui(lang))
            .strong()
            .size(11.0),
    );
    egui::ScrollArea::vertical()
        .id_salt("cdr_hex_dump_scroll")
        .max_height(160.0)
        .show(ui, |ui| {
            ui.label(RichText::new(hex_str).monospace().size(10.0));
        });
}

// =============================================================================
// 2. Properties Docker (`S06`: Fill, Outline, Transparency, Text/Arabic RTL)
// =============================================================================
