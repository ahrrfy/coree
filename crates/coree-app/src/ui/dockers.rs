// SPDX-License-Identifier: MPL-2.0

//! Rearrangeable, tabbable, splittable, and floatable studio Dockers powered by
//! `egui_dock 0.22` covering all 14 production panels:
//! `Objects`, `Properties`, `Lens` (all 11 Lenses), `Effects`, `Transform/Align/Shape`,
//! `Vector Trace`, `Bitmap Studio` (all 19 filters), `Production & Prepress`,
//! `Find & Replace`, `Symbols & Styles`, `Font Manager`, `History`, `Macros`, and
//! `CDR Inspector`.

use crate::io_bridge::{read_text_file_bounded, MAX_TEXT_READ_BYTES};
use crate::state::{BitmapDockerState, DockerTab, LensDockerState, PropertiesSubTab, StudioApp};
use crate::theme::{
    core_color_to_egui, dock_style, egui_color_to_core, DOCKER_DEFAULT_WIDTH, DOCKER_MAX_WIDTH,
    DOCKER_MIN_WIDTH,
};
use coree_core::{
    Affine2D, AlignOp, ArrowheadStyle, BlendMode, BooleanOp, Color, ColorMapMode, CommandOp,
    ContourMode, DistortKind, DistributeOp, EnvelopeMode, Fill, GradientStop, LineCap, LineJoin,
    MasterLayerScope, NodePayload, ObjectId, ObjectStyle, OutlineStyle, Point2D, Rect2D,
    SceneObject, SymbolDef, TextAlign, TextDirection, Vec2D, ZOrderOp,
};
use coree_prod::{
    builtin_macros, BarcodeSymbology, FindScope, LensKindFilter, ObjectKindFilter,
    PreflightSeverity, QrErrorCorrection, ReplaceAction,
};
use coree_render::OutlinePreset;
use egui::{ComboBox, DragValue, Frame, Id, Margin, RichText, Stroke, Ui, WidgetText};
use egui_dock::{DockArea, DockState, TabViewer};
use std::path::PathBuf;

/// Renders the right resizable `egui_dock::DockArea` panel containing all open studio dockers.
pub fn show_dockers_panel(ui: &mut Ui, app: &mut StudioApp) {
    if !app.show_dockers {
        return;
    }
    let tokens = app.theme.tokens();
    let d_style = dock_style(&tokens, ui.style());

    egui::Panel::right("coree_dockers_host")
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
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
        egui::ScrollArea::vertical()
            .id_salt(("docker_scroll", *tab))
            .show(ui, |ui| match *tab {
                DockerTab::Objects => ui_objects_docker(ui, self.app),
                DockerTab::Properties => ui_properties_docker(ui, self.app),
                DockerTab::Lens => ui_lens_docker(ui, self.app),
                DockerTab::Effects => ui_effects_docker(ui, self.app),
                DockerTab::TransformAlignShape => ui_transform_align_shape_docker(ui, self.app),
                DockerTab::VectorTrace => ui_vector_trace_docker(ui, self.app),
                DockerTab::BitmapStudio => ui_bitmap_studio_docker(ui, self.app),
                DockerTab::Production => ui_production_docker(ui, self.app),
                DockerTab::FindReplace => ui_find_replace_docker(ui, self.app),
                DockerTab::SymbolsStyles => ui_symbols_styles_docker(ui, self.app),
                DockerTab::FontManager => ui_font_manager_docker(ui, self.app),
                DockerTab::History => ui_history_docker(ui, self.app),
                DockerTab::Macros => ui_macros_docker(ui, self.app),
                DockerTab::CdrInspector => ui_cdr_inspector_docker(ui, self.app),
            });
    }
}

// =============================================================================
// 1. Objects & Layers Docker (`S05`)
// =============================================================================

fn ui_objects_docker(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    ui.horizontal(|ui| {
        if ui.small_button("+ Layer").clicked() {
            let n = app
                .active_tab()
                .and_then(|t| t.document.active_page())
                .map_or(2, |p| p.layers.len() + 1);
            app.add_layer_active(format!("Layer {n}"), false, MasterLayerScope::None);
        }
        if ui.small_button("+ Cut Layer").clicked() {
            app.add_layer_active("CutContour".to_owned(), true, MasterLayerScope::None);
        }
        if ui.small_button("+ Master").clicked() {
            app.add_layer_active("Master".to_owned(), false, MasterLayerScope::AllPages);
        }
    });
    ui.horizontal(|ui| {
        if ui.small_button("⬆ Front").clicked() {
            app.apply_z_order(ZOrderOp::BringToFront);
        }
        if ui.small_button("↑ Up").clicked() {
            app.apply_z_order(ZOrderOp::BringForward);
        }
        if ui.small_button("↓ Down").clicked() {
            app.apply_z_order(ZOrderOp::SendBackward);
        }
        if ui.small_button("⬇ Back").clicked() {
            app.apply_z_order(ZOrderOp::SendToBack);
        }
    });
    ui.separator();

    let Some(tab) = app.active_tab_mut() else {
        ui.label("No active document.");
        return;
    };

    let mut flag_ops = Vec::new();
    let page_count = tab.document.pages.len();

    for p_idx in 0..page_count {
        let p_name = tab.document.pages[p_idx].name.clone();
        let is_active_page = p_idx == tab.document.active_page_index;
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
                let layer_ids: Vec<_> = tab.document.pages[p_idx]
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
                            l.master_scope,
                            l.objects.clone(),
                        )
                    })
                    .collect();

                for (lid, lname, vis, lck, prn, is_cut, scope, objs) in layer_ids {
                    ui.horizontal(|ui| {
                        let is_act_layer = tab.active_layer_id == Some(lid);
                        if ui
                            .selectable_label(is_act_layer, RichText::new(&lname).strong())
                            .clicked()
                        {
                            tab.active_layer_id = Some(lid);
                            tab.document.active_page_index = p_idx;
                        }
                        if is_cut {
                            ui.label(RichText::new("CUT").size(9.5).color(tokens.cutcontour));
                        }
                        if scope != MasterLayerScope::None {
                            ui.label(
                                RichText::new("MASTER")
                                    .size(9.5)
                                    .color(tokens.accent_secondary),
                            );
                        }
                        if ui
                            .small_button(if vis { "👁" } else { "—" })
                            .on_hover_text("Toggle Visible")
                            .clicked()
                        {
                            flag_ops.push((lid, Some(!vis), None, None, None));
                        }
                        if ui
                            .small_button(if lck { "🔒" } else { "🔓" })
                            .on_hover_text("Toggle Lock")
                            .clicked()
                        {
                            flag_ops.push((lid, None, Some(!lck), None, None));
                        }
                        if ui
                            .small_button(if prn { "🖨" } else { "⊘" })
                            .on_hover_text("Toggle Printable")
                            .clicked()
                        {
                            flag_ops.push((lid, None, None, Some(!prn), None));
                        }
                    });

                    ui.indent(("layer_objs", lid), |ui| {
                        for &oid in objs.iter().rev() {
                            if let Some(obj) = tab.document.objects.get(&oid) {
                                let selected = tab.selection.contains(oid);
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
                                if ui.selectable_label(selected, label).clicked() {
                                    tab.document.active_page_index = p_idx;
                                    tab.active_layer_id = Some(lid);
                                    tab.sync_active_layer();
                                    tab.selection.select_single(&tab.document, oid);
                                    tab.metrics_dirty = true;
                                    tab.needs_rerender = true;
                                }
                            }
                        }
                    });
                }
            });
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
}

// =============================================================================
// 2. Properties Docker (`S06`: Fill, Outline, Transparency, Text/Arabic RTL)
// =============================================================================

fn ui_properties_docker(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    ui.horizontal(|ui| {
        ui.selectable_value(
            &mut app.properties_state.active_tab,
            PropertiesSubTab::Fill,
            "Fill",
        );
        ui.selectable_value(
            &mut app.properties_state.active_tab,
            PropertiesSubTab::Outline,
            "Outline",
        );
        ui.selectable_value(
            &mut app.properties_state.active_tab,
            PropertiesSubTab::Transparency,
            "Blend",
        );
        ui.selectable_value(
            &mut app.properties_state.active_tab,
            PropertiesSubTab::Text,
            "Text",
        );
    });
    ui.separator();

    let sel_id = app
        .active_tab()
        .and_then(|t| t.selection.objects.first().copied());

    match app.properties_state.active_tab {
        PropertiesSubTab::Fill => {
            let cur_fill = sel_id
                .and_then(|id| {
                    app.active_tab()?
                        .document
                        .objects
                        .get(&id)
                        .map(|o| o.fill.clone())
                })
                .unwrap_or_else(|| app.default_fill.clone());

            ui.horizontal_wrapped(|ui| {
                if ui
                    .selectable_label(matches!(cur_fill, Fill::None), "None")
                    .clicked()
                {
                    app.apply_fill(Fill::None);
                }
                if ui
                    .selectable_label(matches!(cur_fill, Fill::Solid(_)), "Solid")
                    .clicked()
                {
                    let c = cur_fill
                        .referenced_colors()
                        .into_iter()
                        .next()
                        .unwrap_or(Color::CYAN);
                    app.apply_fill(Fill::Solid(c));
                }
                if ui
                    .selectable_label(matches!(cur_fill, Fill::LinearGradient { .. }), "Linear")
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
                if ui
                    .selectable_label(matches!(cur_fill, Fill::RadialGradient { .. }), "Radial")
                    .clicked()
                {
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
                if ui
                    .selectable_label(matches!(cur_fill, Fill::ConicalGradient { .. }), "Conical")
                    .clicked()
                {
                    app.apply_fill(Fill::ConicalGradient {
                        center: Point2D::new(30.0, 30.0),
                        angle_deg: 0.0,
                        stops: vec![
                            GradientStop::new(0.0, app.properties_state.grad_start_color.clone()),
                            GradientStop::new(1.0, app.properties_state.grad_end_color.clone()),
                        ],
                    });
                }
                if ui
                    .selectable_label(matches!(cur_fill, Fill::SquareGradient { .. }), "Square")
                    .clicked()
                {
                    app.apply_fill(Fill::SquareGradient {
                        center: Point2D::new(30.0, 30.0),
                        size: 50.0,
                        angle_deg: 0.0,
                        stops: vec![
                            GradientStop::new(0.0, app.properties_state.grad_start_color.clone()),
                            GradientStop::new(1.0, app.properties_state.grad_end_color.clone()),
                        ],
                    });
                }
                if ui
                    .selectable_label(matches!(cur_fill, Fill::TwoColorPattern { .. }), "Pattern")
                    .clicked()
                {
                    app.apply_fill(Fill::TwoColorPattern {
                        fg: Color::CYAN,
                        bg: Color::WHITE,
                        pattern_name: "Checker".to_owned(),
                        tile_w: 8.0,
                        tile_h: 8.0,
                    });
                }
                if ui
                    .selectable_label(matches!(cur_fill, Fill::TextureFill { .. }), "Texture")
                    .clicked()
                {
                    app.apply_fill(Fill::TextureFill {
                        preset: "Marble".to_owned(),
                        seed: 7,
                        fg: Color::CYAN,
                        bg: Color::WHITE,
                    });
                }
                if ui
                    .selectable_label(matches!(cur_fill, Fill::MeshFill { .. }), "Mesh")
                    .clicked()
                {
                    let local_bb = sel_id
                        .and_then(|id| {
                            app.active_tab()
                                .and_then(|t| StudioApp::object_local_bounds(&t.document, id))
                        })
                        .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 50.0, 50.0));
                    let fill = StudioApp::build_mesh_fill_for_bounds(
                        local_bb,
                        app.properties_state.mesh_rows,
                        app.properties_state.mesh_cols,
                    );
                    app.apply_fill(fill);
                }
            });

            ui.separator();
            // Color Model Editors (CMYK / RGB / HSL / Spot / Hex)
            let mut base_color = cur_fill
                .referenced_colors()
                .into_iter()
                .next()
                .unwrap_or(Color::CYAN);

            ui.horizontal(|ui| {
                ui.selectable_value(&mut app.properties_state.color_space_tab, 0, "CMYK");
                ui.selectable_value(&mut app.properties_state.color_space_tab, 1, "RGB");
                ui.selectable_value(&mut app.properties_state.color_space_tab, 2, "HSL");
                ui.selectable_value(&mut app.properties_state.color_space_tab, 3, "Spot");
            });

            let mut color_changed = false;
            match app.properties_state.color_space_tab {
                0 => {
                    let [mut c, mut m, mut y, mut k] = base_color.to_cmyk();
                    let (mut cp, mut mp, mut yp, mut kp) =
                        (c * 100.0, m * 100.0, y * 100.0, k * 100.0);
                    color_changed |= ui
                        .add(egui::Slider::new(&mut cp, 0.0..=100.0).text("C %"))
                        .changed();
                    color_changed |= ui
                        .add(egui::Slider::new(&mut mp, 0.0..=100.0).text("M %"))
                        .changed();
                    color_changed |= ui
                        .add(egui::Slider::new(&mut yp, 0.0..=100.0).text("Y %"))
                        .changed();
                    color_changed |= ui
                        .add(egui::Slider::new(&mut kp, 0.0..=100.0).text("K %"))
                        .changed();
                    if color_changed {
                        c = cp / 100.0;
                        m = mp / 100.0;
                        y = yp / 100.0;
                        k = kp / 100.0;
                        base_color = Color::cmyk(c, m, y, k);
                    }
                }
                1 => {
                    let mut c32 = core_color_to_egui(&base_color);
                    if ui.color_edit_button_srgba(&mut c32).changed() {
                        base_color = egui_color_to_core(c32);
                        color_changed = true;
                    }
                    ui.horizontal(|ui| {
                        ui.label("Hex:");
                        ui.text_edit_singleline(&mut app.properties_state.hex_input);
                        if ui.small_button("Apply").clicked() {
                            if let Ok(parsed) = Color::from_hex(&app.properties_state.hex_input) {
                                base_color = parsed;
                                color_changed = true;
                            }
                        }
                    });
                }
                2 => {
                    let [mut h, mut s, mut l] = base_color.to_hsl();
                    color_changed |= ui
                        .add(egui::Slider::new(&mut h, 0.0..=360.0).text("Hue°"))
                        .changed();
                    color_changed |= ui
                        .add(egui::Slider::new(&mut s, 0.0..=1.0).text("Sat"))
                        .changed();
                    color_changed |= ui
                        .add(egui::Slider::new(&mut l, 0.0..=1.0).text("Light"))
                        .changed();
                    if color_changed {
                        base_color = Color::hsl(h, s, l);
                    }
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.label("Spot:");
                        ui.text_edit_singleline(&mut app.properties_state.spot_name);
                    });
                    ui.add(
                        egui::Slider::new(&mut app.properties_state.spot_tint, 0.0..=1.0)
                            .text("Tint"),
                    );
                    if ui.button("Apply Spot Fill").clicked() {
                        let cmyk = base_color.to_cmyk();
                        base_color = Color::spot(
                            app.properties_state.spot_name.clone(),
                            app.properties_state.spot_tint,
                            cmyk,
                        );
                        color_changed = true;
                    }
                }
            }
            if color_changed {
                app.apply_fill_color(base_color);
            }

            // Gradient Stops editor
            if matches!(
                cur_fill,
                Fill::LinearGradient { .. }
                    | Fill::RadialGradient { .. }
                    | Fill::ConicalGradient { .. }
                    | Fill::SquareGradient { .. }
            ) {
                ui.separator();
                ui.label(RichText::new("Gradient Colors").strong());
                let mut sc = core_color_to_egui(&app.properties_state.grad_start_color);
                let mut ec = core_color_to_egui(&app.properties_state.grad_end_color);
                ui.horizontal(|ui| {
                    ui.label("Start:");
                    let c1 = ui.color_edit_button_srgba(&mut sc).changed();
                    ui.label("End:");
                    let c2 = ui.color_edit_button_srgba(&mut ec).changed();
                    if c1 || c2 {
                        app.properties_state.grad_start_color = egui_color_to_core(sc);
                        app.properties_state.grad_end_color = egui_color_to_core(ec);
                        let mut updated = cur_fill.clone();
                        let new_stops = vec![
                            GradientStop::new(0.0, app.properties_state.grad_start_color.clone()),
                            GradientStop::new(1.0, app.properties_state.grad_end_color.clone()),
                        ];
                        match &mut updated {
                            Fill::LinearGradient { stops, .. }
                            | Fill::RadialGradient { stops, .. }
                            | Fill::ConicalGradient { stops, .. }
                            | Fill::SquareGradient { stops, .. } => *stops = new_stops,
                            _ => {}
                        }
                        app.apply_fill(updated);
                    }
                });
            }
        }
        PropertiesSubTab::Outline => {
            let mut outline = sel_id
                .and_then(|id| {
                    app.active_tab()?
                        .document
                        .objects
                        .get(&id)
                        .map(|o| o.outline.clone())
                })
                .unwrap_or_else(|| app.default_outline.clone());
            let mut changed = false;

            ui.horizontal(|ui| {
                ui.label("Color:");
                let mut c32 = core_color_to_egui(&outline.color);
                if ui.color_edit_button_srgba(&mut c32).changed() {
                    outline.color = egui_color_to_core(c32);
                    changed = true;
                }
                if ui.small_button("CutContour Spot").clicked() {
                    outline = OutlineStyle::cut_contour();
                    changed = true;
                }
            });

            changed |= ui
                .add(egui::Slider::new(&mut outline.width_mm, 0.0..=25.0).text("Width (mm)"))
                .changed();
            changed |= ui
                .checkbox(&mut outline.is_hairline, "Hairline (0.0762 mm)")
                .changed();
            changed |= ui
                .checkbox(&mut outline.behind_fill, "Behind Fill")
                .changed();
            changed |= ui
                .checkbox(&mut outline.scale_with_object, "Scale with Object")
                .changed();
            changed |= ui
                .checkbox(&mut outline.overprint, "Overprint Outline")
                .changed();

            ui.horizontal(|ui| {
                ui.label("Cap:");
                for cap in [
                    LineCap::Butt,
                    LineCap::Round,
                    LineJoinCapHelper::Square.cap(),
                ] {
                    if ui
                        .selectable_label(outline.cap == cap, format!("{cap:?}"))
                        .clicked()
                    {
                        outline.cap = cap;
                        changed = true;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Join:");
                for join in [LineJoin::Miter, LineJoin::Round, LineJoin::Bevel] {
                    if ui
                        .selectable_label(outline.join == join, format!("{join:?}"))
                        .clicked()
                    {
                        outline.join = join;
                        changed = true;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Dash:");
                if ui.small_button("Solid").clicked() {
                    outline.dash_pattern.clear();
                    changed = true;
                }
                if ui.small_button("Dashed").clicked() {
                    outline.dash_pattern = vec![3.0, 2.0];
                    changed = true;
                }
                if ui.small_button("Dotted").clicked() {
                    outline.dash_pattern = vec![0.8, 1.5];
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("End Arrow:");
                ComboBox::from_id_salt("outline_end_arrow")
                    .selected_text(format!("{:?}", outline.end_arrow))
                    .show_ui(ui, |ui| {
                        for a in [
                            ArrowheadStyle::None,
                            ArrowheadStyle::Triangle,
                            ArrowheadStyle::OpenVee,
                            ArrowheadStyle::Diamond,
                            ArrowheadStyle::Circle,
                            ArrowheadStyle::Stealth,
                        ] {
                            if ui
                                .selectable_value(
                                    &mut outline.end_arrow,
                                    a.clone(),
                                    format!("{a:?}"),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
            });

            if changed {
                app.apply_outline(outline);
            }
        }
        PropertiesSubTab::Transparency => {
            if let Some(id) = sel_id {
                let mut mod_flag = false;
                if let Some(tab) = app.active_tab_mut() {
                    if let Some(obj) = tab.document.objects.get_mut(&id) {
                        mod_flag |= ui
                            .add(egui::Slider::new(&mut obj.opacity, 0.0..=1.0).text("Opacity"))
                            .changed();
                        ComboBox::from_id_salt("obj_blend_mode")
                            .selected_text(format!("{:?}", obj.blend_mode))
                            .show_ui(ui, |ui| {
                                for bm in [
                                    BlendMode::Normal,
                                    BlendMode::Multiply,
                                    BlendMode::Screen,
                                    BlendMode::Overlay,
                                    BlendMode::Darken,
                                    BlendMode::Lighten,
                                    BlendMode::Difference,
                                    BlendMode::Exclusion,
                                    BlendMode::SoftLight,
                                    BlendMode::HardLight,
                                    BlendMode::ColorDodge,
                                    BlendMode::ColorBurn,
                                ] {
                                    if ui
                                        .selectable_value(
                                            &mut obj.blend_mode,
                                            bm,
                                            format!("{bm:?}"),
                                        )
                                        .changed()
                                    {
                                        mod_flag = true;
                                    }
                                }
                            });
                        if mod_flag {
                            tab.mark_modified();
                        }
                    }
                }
                if mod_flag {
                    app.checkpoint_wal_snapshot_active();
                }
            } else {
                ui.label(
                    RichText::new("Select an object to adjust Opacity & Blend Mode.")
                        .color(tokens.text_muted),
                );
            }
        }
        PropertiesSubTab::Text => {
            ui.label(RichText::new("Typography & Arabic RTL / Kashida").strong());
            if let Some(id) = sel_id {
                let text_edit_payload = app
                    .active_tab()
                    .and_then(|t| t.document.objects.get(&id))
                    .map(|o| o.payload.clone());
                match text_edit_payload {
                    Some(NodePayload::ArtisticText(mut art)) => {
                        let mut changed = false;
                        if let Some(run) = art.runs.first_mut() {
                            changed |= ui.text_edit_multiline(&mut run.text).changed();
                            ui.horizontal(|ui| {
                                ui.label("Font:");
                                changed |= ui
                                    .text_edit_singleline(&mut run.style.font_family)
                                    .changed();
                            });
                            changed |= ui
                                .add(
                                    egui::Slider::new(&mut run.style.font_size_pt, 6.0..=200.0)
                                        .text("Size (pt)"),
                                )
                                .changed();
                            ui.horizontal(|ui| {
                                ui.label("Direction:");
                                for dir in
                                    [TextDirection::Auto, TextDirection::Ltr, TextDirection::Rtl]
                                {
                                    if ui
                                        .selectable_label(art.direction == dir, format!("{dir:?}"))
                                        .clicked()
                                    {
                                        art.direction = dir;
                                        run.style.direction = dir;
                                        changed = true;
                                    }
                                }
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Align:");
                                for al in [
                                    TextAlign::Left,
                                    TextAlign::Center,
                                    TextAlign::Right,
                                    TextAlign::Justify,
                                    TextAlign::ForceJustifyKashida,
                                ] {
                                    if ui
                                        .selectable_label(art.align == al, format!("{al:?}"))
                                        .clicked()
                                    {
                                        art.align = al;
                                        run.style.align = al;
                                        changed = true;
                                    }
                                }
                            });
                            changed |= ui
                                .add(
                                    egui::Slider::new(&mut run.style.kashida_stretch, 0.0..=2.0)
                                        .text("Kashida Stretch"),
                                )
                                .changed();
                        }
                        if changed {
                            let _ = app.execute_command(CommandOp::EditText {
                                id,
                                payload: NodePayload::ArtisticText(art),
                            });
                        }
                        return;
                    }
                    Some(NodePayload::ParagraphText(mut para)) => {
                        let mut changed = false;
                        if let Some(run) = para.runs.first_mut() {
                            changed |= ui.text_edit_multiline(&mut run.text).changed();
                            ui.horizontal(|ui| {
                                ui.label("Font:");
                                changed |= ui
                                    .text_edit_singleline(&mut run.style.font_family)
                                    .changed();
                            });
                            changed |= ui
                                .add(
                                    egui::Slider::new(&mut run.style.font_size_pt, 6.0..=200.0)
                                        .text("Size (pt)"),
                                )
                                .changed();
                            ui.horizontal(|ui| {
                                ui.label("Direction:");
                                for dir in
                                    [TextDirection::Auto, TextDirection::Ltr, TextDirection::Rtl]
                                {
                                    if ui
                                        .selectable_label(para.direction == dir, format!("{dir:?}"))
                                        .clicked()
                                    {
                                        para.direction = dir;
                                        run.style.direction = dir;
                                        changed = true;
                                    }
                                }
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Align:");
                                for al in [
                                    TextAlign::Left,
                                    TextAlign::Center,
                                    TextAlign::Right,
                                    TextAlign::Justify,
                                    TextAlign::ForceJustifyKashida,
                                ] {
                                    if ui
                                        .selectable_label(para.align == al, format!("{al:?}"))
                                        .clicked()
                                    {
                                        para.align = al;
                                        run.style.align = al;
                                        changed = true;
                                    }
                                }
                            });
                            changed |= ui
                                .add(
                                    egui::Slider::new(&mut run.style.kashida_stretch, 0.0..=2.0)
                                        .text("Kashida Stretch"),
                                )
                                .changed();
                        }
                        ui.horizontal(|ui| {
                            ui.label("Frame:");
                            changed |= ui
                                .add(
                                    DragValue::new(&mut para.frame_size.x)
                                        .range(10.0..=2000.0)
                                        .prefix("W: ")
                                        .suffix(" mm"),
                                )
                                .changed();
                            changed |= ui
                                .add(
                                    DragValue::new(&mut para.frame_size.y)
                                        .range(10.0..=2000.0)
                                        .prefix("H: ")
                                        .suffix(" mm"),
                                )
                                .changed();
                        });
                        ui.horizontal(|ui| {
                            ui.label("Columns:");
                            changed |= ui
                                .add(DragValue::new(&mut para.columns).range(1..=6))
                                .changed();
                            changed |= ui
                                .add(
                                    DragValue::new(&mut para.column_gutter_mm)
                                        .range(0.0..=50.0)
                                        .prefix("Gutter: ")
                                        .suffix(" mm"),
                                )
                                .changed();
                        });
                        if changed {
                            let _ = app.execute_command(CommandOp::EditText {
                                id,
                                payload: NodePayload::ParagraphText(para),
                            });
                        }
                        return;
                    }
                    Some(NodePayload::TextOnPath(mut top)) => {
                        let mut changed = false;
                        if let Some(run) = top.text.runs.first_mut() {
                            changed |= ui.text_edit_singleline(&mut run.text).changed();
                            ui.horizontal(|ui| {
                                ui.label("Font:");
                                changed |= ui
                                    .text_edit_singleline(&mut run.style.font_family)
                                    .changed();
                            });
                            changed |= ui
                                .add(
                                    egui::Slider::new(&mut run.style.font_size_pt, 6.0..=200.0)
                                        .text("Size (pt)"),
                                )
                                .changed();
                            ui.horizontal(|ui| {
                                ui.label("Direction:");
                                for dir in
                                    [TextDirection::Auto, TextDirection::Ltr, TextDirection::Rtl]
                                {
                                    if ui
                                        .selectable_label(
                                            top.text.direction == dir,
                                            format!("{dir:?}"),
                                        )
                                        .clicked()
                                    {
                                        top.text.direction = dir;
                                        run.style.direction = dir;
                                        changed = true;
                                    }
                                }
                            });
                            changed |= ui
                                .add(
                                    egui::Slider::new(&mut run.style.kashida_stretch, 0.0..=2.0)
                                        .text("Kashida Stretch"),
                                )
                                .changed();
                        }
                        ui.horizontal(|ui| {
                            changed |= ui
                                .add(
                                    DragValue::new(&mut top.offset_mm)
                                        .range(-1000.0..=1000.0)
                                        .prefix("Offset: ")
                                        .suffix(" mm"),
                                )
                                .changed();
                            changed |= ui
                                .add(
                                    DragValue::new(&mut top.distance_mm)
                                        .range(-100.0..=100.0)
                                        .prefix("Dist: ")
                                        .suffix(" mm"),
                                )
                                .changed();
                        });
                        ui.horizontal(|ui| {
                            changed |= ui.checkbox(&mut top.flip_horizontal, "Flip H").changed();
                            changed |= ui.checkbox(&mut top.flip_vertical, "Flip V").changed();
                        });
                        if changed {
                            let _ = app.execute_command(CommandOp::EditText {
                                id,
                                payload: NodePayload::TextOnPath(top),
                            });
                        }
                        return;
                    }
                    _ => {}
                }
            }
            ui.label(
                RichText::new(
                    "Select an Artistic/Paragraph Text object, or configure defaults below:",
                )
                .color(tokens.text_muted),
            );
            ui.text_edit_singleline(&mut app.tool_settings.text_insert_draft);
            ui.horizontal(|ui| {
                ui.label("Font:");
                ui.text_edit_singleline(&mut app.default_text_style.font_family);
            });
            ui.add(
                egui::Slider::new(&mut app.default_text_style.font_size_pt, 8.0..=144.0)
                    .text("Size (pt)"),
            );
            ui.add(
                egui::Slider::new(&mut app.default_text_style.kashida_stretch, 0.0..=2.0)
                    .text("Kashida"),
            );
        }
    }
}

enum LineJoinCapHelper {
    Square,
}
impl LineJoinCapHelper {
    const fn cap(self) -> LineCap {
        match self {
            Self::Square => LineCap::Square,
        }
    }
}

// =============================================================================
// 3. 11-Lens Inspector Docker (`S09`)
// =============================================================================

fn ui_lens_docker(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let has_sel = app.active_tab().is_some_and(|t| !t.selection.is_empty());

    if !has_sel {
        ui.label(
            RichText::new(
                "Select a closed vector shape to apply any of the 11 interactive Lenses.",
            )
            .color(tokens.text_muted),
        );
    }

    let ls = &mut app.lens_state;
    ui.horizontal(|ui| {
        ui.label("Lens:");
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
            ui.add(egui::Slider::new(&mut ls.rate_pct, -100.0..=100.0).text("Brighten %"));
        }
        1 | 2 | 9 => {
            ui.add(egui::Slider::new(&mut ls.rate_pct, 0.0..=100.0).text("Rate %"));
            let mut c32 = core_color_to_egui(&ls.tint_color);
            ui.horizontal(|ui| {
                ui.label("Color:");
                if ui.color_edit_button_srgba(&mut c32).changed() {
                    ls.tint_color = egui_color_to_core(c32);
                }
            });
        }
        3 => {
            let mut fc = core_color_to_egui(&ls.from_color);
            let mut tc = core_color_to_egui(&ls.to_color);
            ui.horizontal(|ui| {
                ui.label("From:");
                if ui.color_edit_button_srgba(&mut fc).changed() {
                    ls.from_color = egui_color_to_core(fc);
                }
                ui.label("To:");
                if ui.color_edit_button_srgba(&mut tc).changed() {
                    ls.to_color = egui_color_to_core(tc);
                }
            });
            ComboBox::from_id_salt("lens_map_mode")
                .selected_text(format!("{:?}", ls.map_mode))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut ls.map_mode, ColorMapMode::Direct, "Direct");
                    ui.selectable_value(
                        &mut ls.map_mode,
                        ColorMapMode::ForwardRainbow,
                        "Forward Rainbow",
                    );
                    ui.selectable_value(
                        &mut ls.map_mode,
                        ColorMapMode::ReverseRainbow,
                        "Reverse Rainbow",
                    );
                });
        }
        4 => {
            ui.add(egui::Slider::new(&mut ls.rate_pct, -1000.0..=1000.0).text("Fish Eye %"));
        }
        5 => {
            ui.add(
                egui::Slider::new(&mut ls.heat_rotation_deg, 0.0..=360.0).text("Palette Rotation°"),
            );
        }
        6 => {
            ui.label("Inverts complementary CMY/RGB channels underneath.");
        }
        7 => {
            ui.add(egui::Slider::new(&mut ls.magnify_amount, 1.0..=20.0).text("Magnification ×"));
        }
        8 => {
            let mut tc = core_color_to_egui(&ls.tint_color);
            ui.horizontal(|ui| {
                ui.label("Tint Color:");
                if ui.color_edit_button_srgba(&mut tc).changed() {
                    ls.tint_color = egui_color_to_core(tc);
                }
            });
        }
        _ => {
            let mut oc = core_color_to_egui(&ls.wireframe_outline);
            ui.horizontal(|ui| {
                ui.label("Override Outline:");
                if ui.color_edit_button_srgba(&mut oc).changed() {
                    ls.wireframe_outline = egui_color_to_core(oc);
                }
            });
            ui.checkbox(&mut ls.use_wireframe_fill, "Override Fill");
            if ls.use_wireframe_fill {
                let mut fc = core_color_to_egui(&ls.wireframe_fill);
                if ui.color_edit_button_srgba(&mut fc).changed() {
                    ls.wireframe_fill = egui_color_to_core(fc);
                }
            }
        }
    }

    ui.separator();
    ui.checkbox(&mut ls.frozen, "Frozen (lock snapshot)");
    ui.checkbox(&mut ls.remove_face, "Remove Face");
    ui.checkbox(&mut ls.use_viewpoint, "Custom Viewpoint (mm)");
    if ls.use_viewpoint {
        ui.horizontal(|ui| {
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
    ui.horizontal(|ui| {
        if ui.button(RichText::new("Apply Lens").strong()).clicked() {
            app.apply_lens_to_selection(Some(cfg));
        }
        if ui.button("Remove Lens").clicked() {
            app.apply_lens_to_selection(None);
        }
    });
}

// =============================================================================
// 4. Interactive Vector Effects & PowerClip Docker (`S08`)
// =============================================================================

fn ui_effects_docker(ui: &mut Ui, app: &mut StudioApp) {
    const EFFECT_NAMES: [&str; 10] = [
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
    ];

    ComboBox::from_id_salt("effects_kind_combo")
        .width(200.0)
        .selected_text(EFFECT_NAMES[app.effects_state.selected_effect_kind])
        .show_ui(ui, |ui| {
            for (i, n) in EFFECT_NAMES.iter().enumerate() {
                ui.selectable_value(&mut app.effects_state.selected_effect_kind, i, *n);
            }
        });

    let es = &mut app.effects_state;
    match es.selected_effect_kind {
        0 => {
            ui.add(egui::Slider::new(&mut es.dx_mm, -30.0..=30.0).text("Offset X (mm)"));
            ui.add(egui::Slider::new(&mut es.dy_mm, -30.0..=30.0).text("Offset Y (mm)"));
            ui.add(egui::Slider::new(&mut es.blur_mm, 0.0..=20.0).text("Feather (mm)"));
            ui.add(egui::Slider::new(&mut es.opacity, 0.0..=1.0).text("Opacity"));
        }
        1 => {
            ui.add(egui::Slider::new(&mut es.dx_mm, -40.0..=40.0).text("Depth X (mm)"));
            ui.add(egui::Slider::new(&mut es.dy_mm, -40.0..=40.0).text("Depth Y (mm)"));
        }
        2 => {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut es.contour_mode, ContourMode::ToCenter, "Center");
                ui.selectable_value(&mut es.contour_mode, ContourMode::Inside, "Inside");
                ui.selectable_value(&mut es.contour_mode, ContourMode::Outside, "Outside");
            });
            ui.add(egui::Slider::new(&mut es.steps, 1..=25).text("Steps"));
            ui.add(egui::Slider::new(&mut es.offset_mm, 0.5..=20.0).text("Offset (mm)"));
        }
        3 => {
            ui.add(egui::Slider::new(&mut es.steps, 1..=50).text("Blend Steps"));
        }
        4 => {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut es.distort_kind, DistortKind::PushPull, "Push/Pull");
                ui.selectable_value(&mut es.distort_kind, DistortKind::Zipper, "Zipper");
                ui.selectable_value(&mut es.distort_kind, DistortKind::Twister, "Twister");
            });
            ui.add(egui::Slider::new(&mut es.amplitude, 1.0..=90.0).text("Amplitude"));
            ui.add(egui::Slider::new(&mut es.frequency, 0.5..=10.0).text("Frequency"));
        }
        5 => {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut es.envelope_mode, EnvelopeMode::Straight, "Straight");
                ui.selectable_value(&mut es.envelope_mode, EnvelopeMode::SingleArc, "Single Arc");
                ui.selectable_value(&mut es.envelope_mode, EnvelopeMode::DoubleArc, "Double Arc");
            });
        }
        6 => {
            ui.add(egui::Slider::new(&mut es.extrude_depth, 2.0..=60.0).text("Depth (mm)"));
        }
        7 => {
            ui.add(egui::Slider::new(&mut es.opacity, 0.0..=1.0).text("Alpha"));
        }
        9 => {
            ui.add(egui::Slider::new(&mut es.symmetry_axes, 2..=16).text("Axes"));
            ui.checkbox(&mut es.symmetry_mirror, "Mirror Reflection");
        }
        _ => {}
    }

    if ui.button("+ Add Effect to Selection").clicked() {
        let eff = app.build_selected_effect();
        app.add_effect_to_selection(eff);
    }

    // Active effects stack on selected object
    let active_stack = app.active_tab().and_then(|t| {
        let oid = *t.selection.objects.first()?;
        let obj = t.document.objects.get(&oid)?;
        Some((oid, obj.effects.clone()))
    });
    if let Some((oid, stack)) = active_stack {
        if !stack.is_empty() {
            ui.separator();
            ui.label(RichText::new(format!("Active Stack ({})", stack.len())).strong());
            let mut remove_idx = None;
            for (idx, eff) in stack.iter().enumerate() {
                ui.horizontal(|ui| {
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
    ui.label(RichText::new("PowerClip").strong());
    ui.horizontal(|ui| {
        if ui.button("Place Inside Frame").clicked() {
            app.create_powerclip_from_selection();
        }
        if ui.button("Extract Contents").clicked() {
            app.extract_selected_powerclip();
        }
    });
}

// =============================================================================
// 5. Transform, Align & Shaping Booleans Docker
// =============================================================================

fn ui_transform_align_shape_docker(ui: &mut Ui, app: &mut StudioApp) {
    ui.label(RichText::new("Numeric Transformations").strong());
    let ts = &mut app.transform_state;
    ui.horizontal(|ui| {
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
    ui.horizontal(|ui| {
        ui.add(
            DragValue::new(&mut ts.rotate_deg)
                .prefix("Rot: ")
                .suffix("°"),
        );
        ui.add(
            DragValue::new(&mut ts.copies)
                .range(0..=50)
                .prefix("Copies: "),
        );
    });
    ui.horizontal(|ui| {
        ui.add(
            DragValue::new(&mut ts.scale_x_pct)
                .range(5.0..=500.0)
                .prefix("SX: ")
                .suffix("%"),
        );
        ui.add(
            DragValue::new(&mut ts.scale_y_pct)
                .range(5.0..=500.0)
                .prefix("SY: ")
                .suffix("%"),
        );
    });
    ui.horizontal(|ui| {
        ui.add(
            DragValue::new(&mut ts.skew_x_deg)
                .range(-60.0..=60.0)
                .prefix("SkewX: ")
                .suffix("°"),
        );
        ui.add(
            DragValue::new(&mut ts.skew_y_deg)
                .range(-60.0..=60.0)
                .prefix("SkewY: ")
                .suffix("°"),
        );
    });

    if ui.button("Apply Transform").clicked() {
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
                let _ = app.execute_command(CommandOp::TransformObjects {
                    ids,
                    affine: step_xf,
                });
            }
        } else {
            // Apply to Duplicate N copies sequentially with zero default offset so numeric step_xf is exact
            for _ in 0..ts_c.copies {
                if let Ok(dups) = app.duplicate_selection_with_offset(Vec2D::ZERO) {
                    if !dups.is_empty() {
                        let _ = app.execute_command(CommandOp::TransformObjects {
                            ids: dups,
                            affine: step_xf,
                        });
                    }
                }
            }
        }
    }

    ui.separator();
    ui.label(RichText::new("Align & Distribute").strong());
    ui.horizontal_wrapped(|ui| {
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
    ui.horizontal_wrapped(|ui| {
        if ui.small_button("Distribute H").clicked() {
            app.apply_distribute(DistributeOp::SpacingH);
        }
        if ui.small_button("Distribute V").clicked() {
            app.apply_distribute(DistributeOp::SpacingV);
        }
    });

    ui.separator();
    ui.label(RichText::new("Shaping (9 Vector Booleans)").strong());
    ui.horizontal_wrapped(|ui| {
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

fn ui_vector_trace_docker(ui: &mut Ui, app: &mut StudioApp) {
    ui.label(RichText::new("Bitmap-to-Vector Trace").strong());
    let tr = &mut app.trace_state;
    ui.horizontal(|ui| {
        ui.selectable_value(&mut tr.mode_idx, 0, "Quick");
        ui.selectable_value(&mut tr.mode_idx, 1, "Outline");
        ui.selectable_value(&mut tr.mode_idx, 2, "Centerline");
    });

    if tr.mode_idx == 1 {
        ComboBox::from_id_salt("trace_preset")
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
    }

    ui.add(egui::Slider::new(&mut tr.max_colors, 2..=64).text("Colors"));
    ui.add(egui::Slider::new(&mut tr.detail, 0.1..=1.0).text("Detail"));
    ui.add(egui::Slider::new(&mut tr.smoothing, 0.0..=1.0).text("Smoothing"));
    ui.add(egui::Slider::new(&mut tr.corner_threshold, 10.0..=160.0).text("Corner°"));
    ui.checkbox(&mut tr.remove_background, "Remove Background");

    ui.add_space(6.0);
    if ui
        .button(RichText::new("Trace Selected Bitmap").strong())
        .clicked()
    {
        if let Err(err) = app.trace_selected_raster() {
            app.push_toast(err, true);
        }
    }
}

// =============================================================================
// 7. Integrated Bitmap Studio Docker (`S10` — all 19 RasterFilters + Brush/Resample)
// =============================================================================

fn ui_bitmap_studio_docker(ui: &mut Ui, app: &mut StudioApp) {
    ui.label(RichText::new("Integrated Bitmap Studio").strong());
    ui.horizontal(|ui| {
        ui.add(
            DragValue::new(&mut app.bitmap_state.convert_dpi)
                .range(72.0..=600.0)
                .suffix(" DPI"),
        );
        if ui.button("Convert to Bitmap").clicked() {
            app.convert_selection_to_bitmap();
        }
    });

    ui.separator();
    ui.label(RichText::new("Brush / Eraser / Smudge").strong());
    ui.add(
        egui::Slider::new(&mut app.tool_settings.brush_radius_px, 1.0..=120.0).text("Radius px"),
    );
    ui.add(egui::Slider::new(&mut app.tool_settings.brush_hardness, 0.0..=1.0).text("Hardness"));
    let mut bc = core_color_to_egui(&app.tool_settings.brush_color);
    ui.horizontal(|ui| {
        ui.label("Brush Color:");
        if ui.color_edit_button_srgba(&mut bc).changed() {
            app.tool_settings.brush_color = egui_color_to_core(bc);
        }
    });

    ui.separator();
    ui.label(RichText::new("19 Non-Destructive Filters").strong());
    let bm = &mut app.bitmap_state;
    ComboBox::from_id_salt("bitmap_filter_select")
        .width(210.0)
        .selected_text(BitmapDockerState::FILTER_NAMES[bm.filter_idx])
        .show_ui(ui, |ui| {
            for (idx, name) in BitmapDockerState::FILTER_NAMES.iter().enumerate() {
                ui.selectable_value(&mut bm.filter_idx, idx, *name);
            }
        });
    ui.add(egui::Slider::new(&mut bm.param_a, -1.0..=1.0).text("Param A"));
    ui.add(egui::Slider::new(&mut bm.param_b, -1.0..=1.0).text("Param B"));
    ui.add(egui::Slider::new(&mut bm.param_c, 0.1..=10.0).text("Strength/Radius"));
    let filter = bm.build_filter();
    if ui.button("+ Apply Filter to Bitmap").clicked() {
        app.apply_raster_filter_to_selection(filter);
    }

    ui.separator();
    ui.label(RichText::new("Resample Pixels").strong());
    ui.horizontal(|ui| {
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
        if ui.button("Resample").clicked() {
            let (w, h) = (app.bitmap_state.resample_w, app.bitmap_state.resample_h);
            app.resample_selected_bitmap(w, h);
        }
    });
}

// =============================================================================
// 8. Production & Prepress Docker (`S12`, `S13`, `S14`)
// =============================================================================

fn ui_production_docker(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut app.production_state.sub_section, 0, "Preflight");
        ui.selectable_value(&mut app.production_state.sub_section, 1, "Prepress");
        ui.selectable_value(&mut app.production_state.sub_section, 2, "Cut & Box");
        ui.selectable_value(&mut app.production_state.sub_section, 3, "Barcode");
        ui.selectable_value(&mut app.production_state.sub_section, 4, "Merge/Pack");
    });
    ui.separator();

    match app.production_state.sub_section {
        0 => {
            // Preflight Inspector with click-to-select offending object!
            ui.horizontal(|ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.preflight_min_dpi)
                        .range(72.0..=600.0)
                        .prefix("Min DPI: "),
                );
                if ui.button("Run Preflight").clicked() {
                    app.run_preflight_active();
                }
            });
            let report = app.active_tab().and_then(|t| t.preflight_report.clone());
            if let Some(rep) = report {
                ui.label(
                    RichText::new(format!(
                        "{} Errors · {} Warnings · {} Info",
                        rep.error_count(),
                        rep.warning_count(),
                        rep.info_count()
                    ))
                    .color(if rep.has_errors() {
                        tokens.danger
                    } else {
                        tokens.success
                    }),
                );
                let mut select_obj: Option<ObjectId> = None;
                for issue in &rep.issues {
                    let col = match issue.severity {
                        PreflightSeverity::Error => tokens.danger,
                        PreflightSeverity::Warning => tokens.warning,
                        PreflightSeverity::Info => tokens.accent,
                    };
                    ui.horizontal_wrapped(|ui| {
                        let resp = ui.add(
                            egui::Button::new(
                                RichText::new(format!("[{:?}] {}", issue.severity, issue.message))
                                    .size(11.0)
                                    .color(col),
                            )
                            .frame(false),
                        );
                        if resp.on_hover_text(&issue.suggestion).clicked() {
                            select_obj = issue.object_id;
                        }
                    });
                }
                if let Some(oid) = select_obj {
                    if let Some(tab) = app.active_tab_mut() {
                        tab.selection.select_single(&tab.document, oid);
                        tab.metrics_dirty = true;
                    }
                }
            }
        }
        1 => {
            // Separations, Overprint & Imposition
            ui.horizontal(|ui| {
                if ui.button("Compute Separations").clicked() {
                    app.compute_separations_active();
                }
                if let Some(tab) = app.active_tab_mut() {
                    if ui
                        .checkbox(&mut tab.overprint_preview, "Overprint")
                        .changed()
                    {
                        tab.needs_rerender = true;
                    }
                }
            });
            if let Some(sep) = app
                .active_tab()
                .and_then(|t| t.separations_preview.as_ref())
            {
                ui.label(format!("Max TAC: {:.1}%", sep.max_tac_pct));
                for p in &sep.plates {
                    ui.label(format!(
                        "• {} ({:.1}% avg, {:.1}% peak)",
                        p.name, p.average_coverage_pct, p.max_coverage_pct
                    ));
                }
            }
            ui.separator();
            ui.label(RichText::new("Imposition").strong());
            ui.horizontal(|ui| {
                ui.selectable_value(&mut app.production_state.imposition_kind, 0, "N-Up");
                ui.selectable_value(&mut app.production_state.imposition_kind, 1, "Booklet");
                ui.selectable_value(&mut app.production_state.imposition_kind, 2, "Tiling");
            });
            ui.horizontal(|ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.nup_cols)
                        .range(1..=12)
                        .prefix("Cols: "),
                );
                ui.add(
                    DragValue::new(&mut app.production_state.nup_rows)
                        .range(1..=12)
                        .prefix("Rows: "),
                );
            });
            ui.horizontal(|ui| {
                if ui.button("Impose to New Tab").clicked() {
                    app.run_imposition_active();
                }
                if ui.button("+ Print Marks").clicked() {
                    app.add_print_marks_active();
                }
            });
        }
        2 => {
            // CutContour + Parametric Packaging Dielines + Live Metrics
            ui.label(
                RichText::new("One-Click CutContour")
                    .strong()
                    .color(tokens.cutcontour),
            );
            ui.horizontal(|ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.cut_offset_mm)
                        .range(-10.0..=50.0)
                        .speed(0.2)
                        .prefix("Offset: ")
                        .suffix(" mm"),
                );
                ui.text_edit_singleline(&mut app.production_state.cut_spot_name);
            });
            if ui.button("✂ Generate CutContour").clicked() {
                app.generate_cut_contour_active();
            }

            ui.separator();
            ui.label(RichText::new("Parametric Packaging Dielines").strong());
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut app.production_state.dieline_type, 0, "Box");
                ui.selectable_value(&mut app.production_state.dieline_type, 1, "Envelope");
                ui.selectable_value(&mut app.production_state.dieline_type, 2, "Bag");
                ui.selectable_value(&mut app.production_state.dieline_type, 3, "Stickers");
            });
            ui.horizontal(|ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.box_l_mm)
                        .range(15.0..=1000.0)
                        .prefix("L: "),
                );
                ui.add(
                    DragValue::new(&mut app.production_state.box_w_mm)
                        .range(10.0..=1000.0)
                        .prefix("W: "),
                );
                ui.add(
                    DragValue::new(&mut app.production_state.box_h_mm)
                        .range(15.0..=1000.0)
                        .prefix("H: "),
                );
            });
            if ui.button("Generate Cut + Crease Dieline").clicked() {
                app.generate_dieline_active();
            }

            if let Some(tab) = app.active_tab_mut() {
                tab.ensure_metrics_fresh();
                let m = &tab.last_metrics;
                ui.separator();
                ui.label(RichText::new("Live Cut & Area Meter").strong());
                ui.label(format!(
                    "Cut Length: {:.1} mm ({:.2} m)",
                    m.cut_length_mm, m.cut_length_m
                ));
                ui.label(format!("Crease Length: {:.1} mm", m.crease_length_mm));
                ui.label(format!("Filled Area: {:.1} cm²", m.filled_area_mm2 / 100.0));
                ui.label(format!(
                    "Est. Laser: {:.1}s · Vinyl: {:.1}s",
                    m.estimated_laser_time_sec, m.estimated_vinyl_time_sec
                ));
            }
        }
        3 => {
            // Vector Barcode & QR Generator
            ui.label(RichText::new("1D Vector Barcode").strong());
            ComboBox::from_id_salt("barcode_sym_combo")
                .selected_text(format!("{:?}", app.production_state.barcode_symbology))
                .show_ui(ui, |ui| {
                    for s in [
                        BarcodeSymbology::Ean13,
                        BarcodeSymbology::Ean8,
                        BarcodeSymbology::UpcA,
                        BarcodeSymbology::Code128,
                        BarcodeSymbology::Code39,
                    ] {
                        ui.selectable_value(
                            &mut app.production_state.barcode_symbology,
                            s,
                            format!("{s:?}"),
                        );
                    }
                });
            ui.text_edit_singleline(&mut app.production_state.barcode_payload);
            if ui.button("Insert Vector Barcode").clicked() {
                app.insert_barcode_active();
            }

            ui.separator();
            ui.label(RichText::new("Vector QR Code").strong());
            ui.text_edit_singleline(&mut app.production_state.qr_payload);
            ui.horizontal(|ui| {
                ui.add(
                    DragValue::new(&mut app.production_state.qr_size_mm)
                        .range(8.0..=300.0)
                        .prefix("Size: ")
                        .suffix(" mm"),
                );
                ComboBox::from_id_salt("qr_ec_combo")
                    .width(56.0)
                    .selected_text(format!("{:?}", app.production_state.qr_ec))
                    .show_ui(ui, |ui| {
                        for ec in [
                            QrErrorCorrection::L,
                            QrErrorCorrection::M,
                            QrErrorCorrection::Q,
                            QrErrorCorrection::H,
                        ] {
                            ui.selectable_value(
                                &mut app.production_state.qr_ec,
                                ec,
                                format!("{ec:?}"),
                            );
                        }
                    });
            });
            if ui.button("Insert Vector QR Code").clicked() {
                app.insert_qr_active();
            }
        }
        _ => {
            // Print Merge (CSV) & Collect for Output
            ui.label(RichText::new("Variable Data Print Merge (CSV)").strong());
            ui.add(
                egui::TextEdit::multiline(&mut app.production_state.merge_csv_text)
                    .desired_rows(4)
                    .code_editor(),
            );
            ui.horizontal(|ui| {
                if ui.button("Load CSV...").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("CSV/TSV", &["csv", "tsv", "txt"])
                        .pick_file()
                    {
                        match read_text_file_bounded(&p, MAX_TEXT_READ_BYTES) {
                            Ok(txt) => app.production_state.merge_csv_text = txt,
                            Err(err) => app.push_toast(err, true),
                        }
                    }
                }
                if ui.button("Run Print Merge").clicked() {
                    app.run_print_merge_active();
                }
            });

            ui.separator();
            ui.label(RichText::new("Collect for Output Bundle").strong());
            ui.text_edit_singleline(&mut app.production_state.collect_dir_text);
            ui.horizontal(|ui| {
                if ui.button("Pick Folder...").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        app.production_state.collect_dir_text = dir.to_string_lossy().into_owned();
                    }
                }
                if ui.button("Collect Bundle").clicked() {
                    let dir = PathBuf::from(app.production_state.collect_dir_text.trim());
                    app.run_collect_for_output(&dir);
                }
            });
        }
    }
}

// =============================================================================
// 9. Find & Replace Docker
// =============================================================================

fn ui_find_replace_docker(ui: &mut Ui, app: &mut StudioApp) {
    let fr = &mut app.find_replace_state;
    ui.horizontal(|ui| {
        ui.label("Scope:");
        ui.selectable_value(&mut fr.scope, FindScope::CurrentPage, "Page");
        ui.selectable_value(&mut fr.scope, FindScope::WholeDocument, "Document");
    });
    ComboBox::from_id_salt("fr_kind")
        .selected_text(
            fr.object_kind
                .map_or("Any Object".to_owned(), |k| format!("{k:?}")),
        )
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut fr.object_kind, None, "Any Object");
            for k in [
                ObjectKindFilter::Path,
                ObjectKindFilter::Shape,
                ObjectKindFilter::AnyText,
                ObjectKindFilter::Raster,
                ObjectKindFilter::Table,
                ObjectKindFilter::Group,
                ObjectKindFilter::PowerClip,
            ] {
                ui.selectable_value(&mut fr.object_kind, Some(k), format!("{k:?}"));
            }
        });
    ComboBox::from_id_salt("fr_lens")
        .selected_text(
            fr.lens_filter
                .map_or("Any / No Lens".to_owned(), |l| format!("{l:?}")),
        )
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut fr.lens_filter, None, "Any / No Lens");
            ui.selectable_value(
                &mut fr.lens_filter,
                Some(LensKindFilter::AnyLens),
                "Has Any Lens",
            );
            ui.selectable_value(
                &mut fr.lens_filter,
                Some(LensKindFilter::Magnify),
                "Magnify",
            );
            ui.selectable_value(
                &mut fr.lens_filter,
                Some(LensKindFilter::FishEye),
                "Fish Eye",
            );
            ui.selectable_value(&mut fr.lens_filter, Some(LensKindFilter::Invert), "Invert");
        });
    ui.horizontal(|ui| {
        ui.label("Find Text:");
        ui.text_edit_singleline(&mut fr.text_query);
    });
    ui.horizontal(|ui| {
        ui.label("Replace:");
        ui.text_edit_singleline(&mut fr.replace_text);
    });
    ui.checkbox(&mut fr.case_sensitive, "Case Sensitive");

    let mut action_to_run: Option<ReplaceAction> = None;
    let mut find_only = false;
    ui.horizontal_wrapped(|ui| {
        if ui.button("Find & Select").clicked() {
            find_only = true;
        }
        if ui.button("Replace Text").clicked() {
            action_to_run = Some(ReplaceAction::ReplaceText {
                find: fr.text_query.clone(),
                replace_with: fr.replace_text.clone(),
                case_sensitive: fr.case_sensitive,
            });
        }
        if ui.button("Replace Fill").clicked() {
            action_to_run = Some(ReplaceAction::ReplaceFillColor {
                color: fr.replace_fill_color.clone(),
            });
        }
        if ui.button("Set Overprint").clicked() {
            action_to_run = Some(ReplaceAction::ConvertOverprint { overprint: true });
        }
    });

    let mut rc = core_color_to_egui(&fr.replace_fill_color);
    ui.horizontal(|ui| {
        ui.label("Fill Color:");
        if ui.color_edit_button_srgba(&mut rc).changed() {
            fr.replace_fill_color = egui_color_to_core(rc);
        }
    });

    if find_only {
        app.run_find_select();
    } else if let Some(act) = action_to_run {
        app.run_find_and_replace_action(act);
    }

    if let Some(ref rep) = app.find_replace_state.last_report {
        ui.separator();
        ui.label(format!(
            "Matched: {} · Modified: {} · Replacements: {}",
            rep.matched_ids.len(),
            rep.modified_ids.len(),
            rep.replacements_applied
        ));
    }
}

// =============================================================================
// 10. Symbols & Object Styles Docker
// =============================================================================

fn ui_symbols_styles_docker(ui: &mut Ui, app: &mut StudioApp) {
    ui.label(RichText::new("Reusable Symbols").strong());
    if ui.button("+ Create Symbol from Selection").clicked() {
        if let Some(tab) = app.active_tab_mut() {
            let objs: Vec<SceneObject> = tab
                .selection
                .objects
                .iter()
                .filter_map(|id| tab.document.objects.get(id).cloned())
                .collect();
            if !objs.is_empty() {
                let n = tab.document.symbols.len() + 1;
                let def = SymbolDef::new(format!("Symbol {n}"), objs);
                tab.document.symbols.insert(def.id, def);
                tab.mark_modified();
            }
        }
    }

    let syms: Vec<_> = app
        .active_tab()
        .map(|t| {
            t.document
                .symbols
                .values()
                .map(|s| (s.id, s.name.clone(), s.objects.len()))
                .collect()
        })
        .unwrap_or_default();
    for (sid, name, cnt) in syms {
        ui.horizontal(|ui| {
            ui.label(format!("{name} ({cnt} obj)"));
            if ui.small_button("Insert").clicked() {
                let lid = app.active_tab().and_then(|t| t.active_layer_id);
                let inst = SceneObject::new(
                    ObjectId(0),
                    format!("{name} Instance"),
                    NodePayload::SymbolInstance(sid),
                )
                .with_transform(Affine2D::translate(30.0, 30.0));
                let _ = app.execute_command(CommandOp::create_object(inst, lid));
            }
        });
    }

    ui.separator();
    ui.label(RichText::new("Object Styles").strong());
    if ui.button("+ Save Style from Selection").clicked() {
        if let Some(tab) = app.active_tab_mut() {
            if let Some(obj) = tab
                .selection
                .objects
                .first()
                .and_then(|id| tab.document.objects.get(id))
            {
                let mut st =
                    ObjectStyle::new(format!("Style {}", tab.document.object_styles.len() + 1));
                st.fill = Some(obj.fill.clone());
                st.outline = Some(obj.outline.clone());
                st.blend_mode = Some(obj.blend_mode);
                st.opacity = Some(obj.opacity);
                tab.document.object_styles.push(st);
                tab.mark_modified();
            }
        }
    }
    let styles: Vec<_> = app
        .active_tab()
        .map(|t| t.document.object_styles.clone())
        .unwrap_or_default();
    for st in styles {
        ui.horizontal(|ui| {
            ui.label(&st.name);
            if ui.small_button("Apply").clicked() {
                if let Some(fill) = st.fill.clone() {
                    app.apply_fill(fill);
                }
                if let Some(out) = st.outline.clone() {
                    app.apply_outline(out);
                }
            }
        });
    }
}

// =============================================================================
// 11. Local Font Manager Docker (`S07` — backed by `coree-storage` font_index)
// =============================================================================

fn ui_font_manager_docker(ui: &mut Ui, app: &mut StudioApp) {
    let mut need_refresh = !app.font_manager_state.scanned_once;
    let mut rescan = false;

    ui.horizontal(|ui| {
        if ui
            .add(
                egui::TextEdit::singleline(&mut app.font_manager_state.search_query)
                    .hint_text("Search local fonts...")
                    .desired_width(150.0),
            )
            .changed()
        {
            need_refresh = true;
        }
        if ui.small_button("↻ Scan").clicked() {
            rescan = true;
            need_refresh = true;
        }
    });
    ui.horizontal(|ui| {
        if ui
            .checkbox(&mut app.font_manager_state.arabic_only, "Arabic RTL")
            .changed()
        {
            need_refresh = true;
        }
        if ui
            .checkbox(&mut app.font_manager_state.kashida_only, "Kashida")
            .changed()
        {
            need_refresh = true;
        }
    });

    if need_refresh {
        app.refresh_font_manager(rescan);
    }

    let tokens = app.theme.tokens();
    ui.label(
        RichText::new(format!(
            "{} local font face(s)",
            app.font_manager_state.indexed_fonts.len()
        ))
        .size(11.0)
        .color(tokens.text_muted),
    );
    ui.separator();

    let fonts = app.font_manager_state.indexed_fonts.clone();
    for f in fonts.iter().take(80) {
        ui.horizontal(|ui| {
            let tag = if f.supports_arabic_rtl { " [AR]" } else { "" };
            if ui
                .button(format!("{} {}{tag}", f.family_name, f.subfamily_name))
                .clicked()
            {
                app.default_text_style.font_family = f.family_name.clone();
                // Apply to selected text object(s) via undoable, WAL-journaled EditText commands
                let edit_ops: Vec<CommandOp> = app
                    .active_tab()
                    .map(|tab| {
                        tab.selection
                            .objects
                            .iter()
                            .copied()
                            .filter_map(|oid| {
                                let obj = tab.document.objects.get(&oid)?;
                                let mut payload = obj.payload.clone();
                                match &mut payload {
                                    NodePayload::ArtisticText(t) => {
                                        for r in &mut t.runs {
                                            r.style.font_family = f.family_name.clone();
                                        }
                                        Some(CommandOp::EditText { id: oid, payload })
                                    }
                                    NodePayload::ParagraphText(p) => {
                                        for r in &mut p.runs {
                                            r.style.font_family = f.family_name.clone();
                                        }
                                        Some(CommandOp::EditText { id: oid, payload })
                                    }
                                    NodePayload::TextOnPath(top) => {
                                        for r in &mut top.text.runs {
                                            r.style.font_family = f.family_name.clone();
                                        }
                                        Some(CommandOp::EditText { id: oid, payload })
                                    }
                                    _ => None,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if edit_ops.len() == 1 {
                    let _ = app.execute_command(edit_ops.into_iter().next().unwrap());
                } else if !edit_ops.is_empty() {
                    let _ = app.execute_command(CommandOp::Batch(edit_ops));
                }
                app.push_toast(format!("Selected font '{}'", f.family_name), false);
            }
        });
    }
}

// =============================================================================
// 12. Visual Command History Docker (`S17`)
// =============================================================================

fn ui_history_docker(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    ui.horizontal(|ui| {
        if ui.button("↶ Undo").clicked() {
            let _ = app.undo();
        }
        if ui.button("↷ Redo").clicked() {
            let _ = app.redo();
        }
        if ui.button("Save Snapshot").clicked() {
            if let Err(e) = app.perform_autosave_active() {
                app.push_toast(e, true);
            } else {
                app.push_toast("Saved crash-recovery snapshot", false);
            }
        }
    });
    ui.separator();

    if let Some(tab) = app.active_tab() {
        ui.label(RichText::new("Applied Command Log").strong());
        let undo_labels = tab.history.undo_labels();
        if undo_labels.is_empty() {
            ui.label(RichText::new("(No commands yet)").color(tokens.text_muted));
        }
        for (i, lbl) in undo_labels.iter().enumerate() {
            ui.label(format!("{}. {lbl}", i + 1));
        }

        let redo_labels = tab.history.redo_labels();
        if !redo_labels.is_empty() {
            ui.separator();
            ui.label(RichText::new("Redo Stack").color(tokens.text_muted));
            for lbl in redo_labels.iter().rev() {
                ui.label(RichText::new(format!("↳ {lbl}")).color(tokens.text_muted));
            }
        }
    }
}

// =============================================================================
// 13. Sandboxed Rhai Macros Studio Docker (`S17`)
// =============================================================================

fn ui_macros_docker(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let recording = app.active_tab().is_some_and(|t| t.history.recording_macro);

    ui.horizontal(|ui| {
        if recording {
            if ui
                .button(RichText::new("■ Stop -> Rhai").color(tokens.accent_secondary))
                .clicked()
            {
                app.stop_macro_recording();
            }
        } else if ui.button("● Record").clicked() {
            app.start_macro_recording();
        }
        if ui
            .button(RichText::new("▶ Run Script").color(tokens.success))
            .clicked()
        {
            let src = app.macro_state.script_source.clone();
            let _ = app.run_rhai_script(&src);
        }
    });

    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new("Built-ins:")
                .size(11.0)
                .color(tokens.text_muted),
        );
        for m in builtin_macros() {
            if ui.small_button(m.id).on_hover_text(m.description).clicked() {
                app.macro_state.script_source = m.script.trim().to_owned();
            }
        }
    });

    ui.add(
        egui::TextEdit::multiline(&mut app.macro_state.script_source)
            .code_editor()
            .desired_rows(10)
            .desired_width(f32::INFINITY),
    );

    if let Some(ref err) = app.macro_state.last_error {
        ui.label(RichText::new(format!("Error: {err}")).color(tokens.danger));
    }
    if !app.macro_state.output_logs.is_empty() {
        ui.separator();
        ui.label(RichText::new("Console Output").strong());
        for line in &app.macro_state.output_logs {
            ui.label(
                RichText::new(line)
                    .monospace()
                    .size(11.0)
                    .color(tokens.success),
            );
        }
    }
}

// =============================================================================
// 14. CDR Binary & Chunk Inspector Docker (`S16`)
// =============================================================================

fn ui_cdr_inspector_docker(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    if ui.button("Refresh Structural Inspection").clicked() {
        app.refresh_inspector_for_active(true);
    }
    let Some(rep) = app.active_tab().and_then(|t| t.inspection_report.as_ref()) else {
        ui.label(
            RichText::new(
                "Open a .cdr/.cdt/.cmx/.cpt file or click Refresh to inspect document structure.",
            )
            .color(tokens.text_muted),
        );
        return;
    };

    ui.label(RichText::new(&rep.format_era).strong().color(tokens.accent));
    ui.label(format!(
        "{} page(s) · {} layer(s) · {} obj(s) · {} bitmap(s)",
        rep.page_count, rep.layer_count, rep.object_count, rep.bitmap_count
    ));
    if !rep.fonts.is_empty() {
        ui.label(format!("Fonts: {}", rep.fonts.join(", ")));
    }
    if !rep.color_models.is_empty() {
        ui.label(format!("Colors: {}", rep.color_models.join(", ")));
    }

    if !rep.unknown_chunks.is_empty() {
        ui.separator();
        ui.label(
            RichText::new(format!(
                "Preserved Unknown Chunks ({})",
                rep.unknown_chunks.len()
            ))
            .strong()
            .color(tokens.warning),
        );
        for unk in &rep.unknown_chunks {
            ui.label(
                RichText::new(format!(
                    "• '{}' [{}] ({} B, v{})",
                    unk.fourcc, unk.scope, unk.byte_len, unk.version_hint
                ))
                .monospace()
                .size(11.0),
            );
        }
    }

    if !rep.zip_entries.is_empty() {
        ui.separator();
        egui::CollapsingHeader::new(format!("ZIP Entries ({})", rep.zip_entries.len())).show(
            ui,
            |ui| {
                for ze in &rep.zip_entries {
                    ui.label(
                        RichText::new(format!(
                            "{} ({} → {} B)",
                            ze.name, ze.compressed_size, ze.uncompressed_size
                        ))
                        .monospace()
                        .size(10.5),
                    );
                }
            },
        );
    }

    if !rep.chunks.is_empty() {
        ui.separator();
        egui::CollapsingHeader::new(format!("RIFF Chunk Tree ({})", rep.chunks.len())).show(
            ui,
            |ui| {
                for ch in rep.chunks.iter().take(150) {
                    let indent = "  ".repeat(ch.depth.min(8));
                    let tag = if ch.is_list { "LIST " } else { "" };
                    ui.label(
                        RichText::new(format!("{indent}{tag}'{}' ({} B)", ch.fourcc, ch.size))
                            .monospace()
                            .size(10.5),
                    );
                }
            },
        );
    }
}
