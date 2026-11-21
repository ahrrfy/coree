// SPDX-License-Identifier: MPL-2.0

//! Standard Studio Toolbar + Context-Sensitive Property Bar (`34px`) adapting live
//! to active tool and selected object(s).

use crate::shortcuts::ShortcutAction;
use crate::state::{DockerTab, LensDockerState, StudioApp, ToolId};
use crate::theme::PROPERTY_BAR_HEIGHT;
use coree_core::{
    Affine2D, BooleanOp, CommandOp, CornerStyle, NodeKind, NodePayload, ParametricShape, TextAlign,
    TextDirection, Unit,
};
use coree_render::ViewMode;
use egui::{ComboBox, DragValue, Frame, Margin, RichText, Stroke, Ui};

/// Renders both the Standard Toolbar and the Context-Sensitive Property Bar.
pub fn show_toolbars(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();

    // Row 1: Standard Toolbar
    egui::Panel::top("coree_standard_toolbar")
        .exact_size(30.0)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(8, 3))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                if ui
                    .small_button("New")
                    .on_hover_text("New Document (Ctrl+N)")
                    .clicked()
                {
                    app.new_doc_dialog.open = true;
                }
                if ui
                    .small_button("Open")
                    .on_hover_text("Open File (Ctrl+O)")
                    .clicked()
                {
                    app.open_file_dialog();
                }
                if ui
                    .small_button("Save")
                    .on_hover_text("Save Document (Ctrl+S)")
                    .clicked()
                {
                    app.handle_shortcut(ShortcutAction::SaveDocument);
                }
                ui.separator();

                let (can_undo, can_redo) = app.active_tab().map_or((false, false), |t| {
                    (t.history.can_undo(), t.history.can_redo())
                });
                if ui
                    .add_enabled(can_undo, egui::Button::new("↶ Undo").small())
                    .clicked()
                {
                    let _ = app.undo();
                }
                if ui
                    .add_enabled(can_redo, egui::Button::new("↷ Redo").small())
                    .clicked()
                {
                    let _ = app.redo();
                }
                ui.separator();

                if ui.small_button("Import").clicked() {
                    app.import_file_dialog();
                }
                if ui.small_button("Export").clicked() {
                    app.export_dialog.open = true;
                }
                ui.separator();

                // Zoom preset dropdown
                let zoom_pct = app
                    .active_tab()
                    .map_or(100.0, |t| (t.viewport.zoom * 100.0).round());
                ComboBox::from_id_salt("std_zoom_preset")
                    .width(74.0)
                    .selected_text(format!("{zoom_pct:.0}%"))
                    .show_ui(ui, |ui| {
                        for pct in [25.0, 50.0, 75.0, 100.0, 150.0, 200.0, 400.0] {
                            if ui.selectable_label(false, format!("{pct:.0}%")).clicked() {
                                if let Some(t) = app.active_tab_mut() {
                                    t.viewport.zoom = pct / 100.0;
                                    t.needs_rerender = true;
                                }
                            }
                        }
                        if ui.selectable_label(false, "Fit Page").clicked() {
                            app.handle_shortcut(ShortcutAction::ZoomToPage);
                        }
                        if ui.selectable_label(false, "Fit All").clicked() {
                            app.handle_shortcut(ShortcutAction::ZoomToAll);
                        }
                    });

                // View Mode selector
                if let Some(tab) = app.active_tab_mut() {
                    ComboBox::from_id_salt("std_view_mode")
                        .width(86.0)
                        .selected_text(format!("{:?}", tab.view_mode))
                        .show_ui(ui, |ui| {
                            for mode in [
                                ViewMode::Wireframe,
                                ViewMode::Normal,
                                ViewMode::Enhanced,
                                ViewMode::Pixels,
                            ] {
                                if ui
                                    .selectable_value(&mut tab.view_mode, mode, format!("{mode:?}"))
                                    .changed()
                                {
                                    tab.needs_rerender = true;
                                }
                            }
                        });

                    if ui
                        .selectable_label(tab.document.multipage_view, "⊞ Multipage")
                        .on_hover_text("Toggle Multipage View")
                        .clicked()
                    {
                        tab.document.multipage_view = !tab.document.multipage_view;
                        tab.viewport.multipage_view = tab.document.multipage_view;
                        tab.needs_rerender = true;
                    }

                    ui.separator();
                    ui.checkbox(&mut tab.snap_options.objects, "Snap Obj");
                    ui.checkbox(&mut tab.snap_options.guidelines, "Guides");
                    ui.checkbox(&mut tab.snap_options.grid, "Grid");
                    if tab.snap_options.grid != tab.document.grid.snap {
                        tab.document.grid.snap = tab.snap_options.grid;
                    }
                }

                ui.separator();

                // Record Macro indicator button
                let recording = app.active_tab().is_some_and(|t| t.history.recording_macro);
                if recording {
                    if ui
                        .button(
                            RichText::new("● Stop Macro")
                                .size(11.5)
                                .color(tokens.accent_secondary),
                        )
                        .clicked()
                    {
                        app.stop_macro_recording();
                    }
                } else if ui
                    .small_button("● Rec Macro")
                    .on_hover_text("Record actions to Rhai script")
                    .clicked()
                {
                    app.start_macro_recording();
                }

                // One-Click CutContour shortcut button
                if ui
                    .button(
                        RichText::new("✂ CutContour")
                            .size(11.5)
                            .color(tokens.cutcontour),
                    )
                    .on_hover_text("One-click welded CutContour spot hairline")
                    .clicked()
                {
                    app.generate_cut_contour_active();
                }
            });
        });

    // Row 2: Context-Sensitive Property Bar (34px)
    egui::Panel::top("coree_property_bar")
        .exact_size(PROPERTY_BAR_HEIGHT)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(8, 4))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("prop_bar_scroll")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        show_context_property_contents(ui, app);
                    });
                });
        });
}

fn show_context_property_contents(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();
    let sel_ids = app
        .active_tab()
        .map(|t| t.selection.objects.clone())
        .unwrap_or_default();

    // Tool-specific context controls first for Shape/Node (F10) or Lens Tool
    if app.active_tool == ToolId::ShapeNode {
        ui.label(RichText::new("Node Edit:").color(tokens.accent).strong());
        let selected_nodes = app
            .active_tab()
            .map(|t| t.selection.node_selection.nodes.clone())
            .unwrap_or_default();
        let primary_oid = app
            .active_tab()
            .and_then(|t| t.selection.objects.first().copied());
        ui.label(
            RichText::new(format!("{} node(s)", selected_nodes.len())).color(tokens.text_muted),
        );
        let mut pending_node_kind: Option<NodeKind> = None;
        for (kind, lbl) in [
            (NodeKind::Cusp, "Cusp"),
            (NodeKind::Smooth, "Smooth"),
            (NodeKind::Symmetrical, "Symmetrical"),
        ] {
            if ui.small_button(lbl).clicked() && !selected_nodes.is_empty() {
                pending_node_kind = Some(kind);
            }
        }
        let close_clicked = ui.small_button("Close Path").clicked();
        let reverse_clicked = ui.small_button("Reverse").clicked();

        if let Some(kind) = pending_node_kind {
            let mut edited_paths: std::collections::BTreeMap<
                coree_core::ObjectId,
                coree_core::BezierPath,
            > = std::collections::BTreeMap::new();
            if let Some(tab) = app.active_tab() {
                for nref in &selected_nodes {
                    let path_entry = edited_paths.entry(nref.object_id).or_insert_with(|| {
                        tab.document
                            .objects
                            .get(&nref.object_id)
                            .and_then(|o| match &o.payload {
                                NodePayload::Path(p) => Some(p.clone()),
                                _ => None,
                            })
                            .unwrap_or_default()
                    });
                    if let Some(sp) = path_entry.subpaths.get_mut(nref.subpath_index) {
                        if let Some(node) = sp.nodes.get_mut(nref.node_index) {
                            node.kind = kind;
                            node.enforce_continuity();
                        }
                    }
                }
            }
            let ops: Vec<CommandOp> = edited_paths
                .into_iter()
                .filter(|(_, p)| !p.is_empty())
                .map(|(id, path)| CommandOp::EditPath { id, path })
                .collect();
            if ops.len() == 1 {
                let _ = app.execute_command(ops.into_iter().next().unwrap());
            } else if !ops.is_empty() {
                let _ = app.execute_command(CommandOp::Batch(ops));
            }
        }
        if close_clicked {
            if let Some(oid) = primary_oid {
                let edited = app.active_tab().and_then(|tab| {
                    let obj = tab.document.objects.get(&oid)?;
                    let NodePayload::Path(ref p) = obj.payload else {
                        return None;
                    };
                    let mut cloned = p.clone();
                    for sp in &mut cloned.subpaths {
                        sp.closed = true;
                    }
                    Some(cloned)
                });
                if let Some(path) = edited {
                    let _ = app.execute_command(CommandOp::EditPath { id: oid, path });
                }
            }
        }
        if reverse_clicked {
            if let Some(oid) = primary_oid {
                let edited = app.active_tab().and_then(|tab| {
                    let obj = tab.document.objects.get(&oid)?;
                    let NodePayload::Path(ref p) = obj.payload else {
                        return None;
                    };
                    let mut cloned = p.clone();
                    for sp in &mut cloned.subpaths {
                        sp.reverse();
                    }
                    Some(cloned)
                });
                if let Some(path) = edited {
                    let _ = app.execute_command(CommandOp::EditPath { id: oid, path });
                }
            }
        }
        return;
    }

    if app.active_tool == ToolId::LensTool {
        ui.label(RichText::new("Lens:").color(tokens.accent).strong());
        ComboBox::from_id_salt("prop_lens_kind")
            .width(130.0)
            .selected_text(LensDockerState::NAMES[app.lens_state.selected_index])
            .show_ui(ui, |ui| {
                for (idx, name) in LensDockerState::NAMES.iter().enumerate() {
                    ui.selectable_value(&mut app.lens_state.selected_index, idx, *name);
                }
            });
        ui.checkbox(&mut app.lens_state.frozen, "Frozen");
        ui.checkbox(&mut app.lens_state.use_viewpoint, "Viewpoint");
        ui.checkbox(&mut app.lens_state.remove_face, "Remove Face");
        if ui.small_button("Apply Lens").clicked() {
            let cfg = app.lens_state.build_config();
            app.apply_lens_to_selection(Some(cfg));
        }
        return;
    }

    // No selection: Document defaults bar (Page Size, Orientation, Units, Nudge, Duplicate Offset)
    if sel_ids.is_empty() {
        let Some(tab) = app.active_tab_mut() else {
            return;
        };
        ui.label(RichText::new("Page:").color(tokens.text_muted));
        let mut page_modified = false;
        if let Some(page) = tab.document.active_page_mut() {
            let mut w = page.width_mm;
            let mut h = page.height_mm;
            let w_chg = ui
                .add(
                    DragValue::new(&mut w)
                        .range(5.0..=5000.0)
                        .suffix(" mm")
                        .prefix("W: "),
                )
                .changed();
            let h_chg = ui
                .add(
                    DragValue::new(&mut h)
                        .range(5.0..=5000.0)
                        .suffix(" mm")
                        .prefix("H: "),
                )
                .changed();
            if w_chg || h_chg {
                page.width_mm = w;
                page.height_mm = h;
                page_modified = true;
            }
            if ui
                .small_button("⇄ Swap")
                .on_hover_text("Toggle Portrait / Landscape")
                .clicked()
            {
                std::mem::swap(&mut page.width_mm, &mut page.height_mm);
                page_modified = true;
            }
        }
        if page_modified {
            tab.mark_modified();
        }
        ui.separator();
        ui.label(RichText::new("Unit:").color(tokens.text_muted));
        ComboBox::from_id_salt("prop_unit")
            .width(64.0)
            .selected_text(tab.document.default_unit.suffix())
            .show_ui(ui, |ui| {
                for u in [
                    Unit::Millimeter,
                    Unit::Centimeter,
                    Unit::Meter,
                    Unit::Inch,
                    Unit::Point,
                    Unit::Pixel,
                ] {
                    ui.selectable_value(&mut tab.document.default_unit, u, u.suffix());
                }
            });
        ui.separator();
        ui.add(
            DragValue::new(&mut tab.nudge_mm)
                .range(0.05..=100.0)
                .speed(0.1)
                .prefix("Nudge: ")
                .suffix(" mm"),
        );
        ui.add(
            DragValue::new(&mut tab.duplicate_offset_mm.x)
                .range(-200.0..=200.0)
                .prefix("Dup X: ")
                .suffix(" mm"),
        );
        ui.add(
            DragValue::new(&mut tab.duplicate_offset_mm.y)
                .range(-200.0..=200.0)
                .prefix("Dup Y: ")
                .suffix(" mm"),
        );

        // Quick tool parameters when a drawing tool is active
        match app.active_tool {
            ToolId::Polygon => {
                ui.separator();
                ui.add(
                    DragValue::new(&mut app.tool_settings.polygon_sides)
                        .range(3..=32)
                        .prefix("Sides: "),
                );
            }
            ToolId::Star | ToolId::ComplexStar => {
                ui.separator();
                ui.add(
                    DragValue::new(&mut app.tool_settings.star_points)
                        .range(3..=32)
                        .prefix("Points: "),
                );
                ui.add(
                    DragValue::new(&mut app.tool_settings.star_inner_ratio)
                        .range(0.1..=0.9)
                        .speed(0.02)
                        .prefix("Sharpness: "),
                );
            }
            ToolId::Rectangle | ToolId::ThreePointRectangle => {
                ui.separator();
                ui.add(
                    DragValue::new(&mut app.tool_settings.rect_corner_radius_mm)
                        .range(0.0..=100.0)
                        .speed(0.2)
                        .prefix("Corner R: ")
                        .suffix(" mm"),
                );
            }
            ToolId::ArtisticText | ToolId::ParagraphText => {
                ui.separator();
                ui.label("Text:");
                ui.add(
                    egui::TextEdit::singleline(&mut app.tool_settings.text_insert_draft)
                        .desired_width(140.0),
                );
            }
            _ => {}
        }
        return;
    }

    // Selection active: X / Y / W / H + Aspect Lock + Rotate + Mirror + Shape / Text / Boolean contextual bar
    let bounds = app
        .active_tab()
        .and_then(|t| t.selection.bounding_box(&t.document));
    if let Some(bb) = bounds {
        let mut x = bb.min_x;
        let mut y = bb.min_y;
        let mut w = bb.width().max(0.1);
        let mut h = bb.height().max(0.1);
        let lock = app.active_tab().is_some_and(|t| t.lock_aspect);

        let x_chg = ui
            .add(
                DragValue::new(&mut x)
                    .speed(0.5)
                    .prefix("X: ")
                    .suffix(" mm"),
            )
            .changed();
        let y_chg = ui
            .add(
                DragValue::new(&mut y)
                    .speed(0.5)
                    .prefix("Y: ")
                    .suffix(" mm"),
            )
            .changed();
        if x_chg || y_chg {
            let affine = Affine2D::translate(x - bb.min_x, y - bb.min_y);
            let _ = app.execute_command(CommandOp::TransformObjects {
                ids: sel_ids.clone(),
                affine,
            });
        }

        let w_chg = ui
            .add(
                DragValue::new(&mut w)
                    .range(0.1..=5000.0)
                    .speed(0.5)
                    .prefix("W: ")
                    .suffix(" mm"),
            )
            .changed();
        let h_chg = ui
            .add(
                DragValue::new(&mut h)
                    .range(0.1..=5000.0)
                    .speed(0.5)
                    .prefix("H: ")
                    .suffix(" mm"),
            )
            .changed();
        if let Some(t) = app.active_tab_mut() {
            ui.checkbox(&mut t.lock_aspect, "🔒")
                .on_hover_text("Lock aspect ratio");
        }
        if w_chg || h_chg {
            let sx = (w / bb.width().max(1e-4)).clamp(0.01, 100.0);
            let sy = if lock && w_chg {
                sx
            } else {
                (h / bb.height().max(1e-4)).clamp(0.01, 100.0)
            };
            let sx_final = if lock && h_chg && !w_chg { sy } else { sx };
            let c = bb.center();
            let affine = Affine2D::translate(c.x, c.y)
                .compose(&Affine2D::scale(sx_final, sy))
                .compose(&Affine2D::translate(-c.x, -c.y));
            let _ = app.execute_command(CommandOp::TransformObjects {
                ids: sel_ids.clone(),
                affine,
            });
        }

        ui.separator();
        if ui
            .small_button("↻ 15°")
            .on_hover_text("Rotate +15°")
            .clicked()
        {
            let affine = Affine2D::rotate_about(15.0_f64.to_radians(), bb.center());
            let _ = app.execute_command(CommandOp::TransformObjects {
                ids: sel_ids.clone(),
                affine,
            });
        }
        if ui.small_button("⇄ Mirror H").clicked() {
            let c = bb.center();
            let affine = Affine2D::translate(c.x, c.y)
                .compose(&Affine2D::scale(-1.0, 1.0))
                .compose(&Affine2D::translate(-c.x, -c.y));
            let _ = app.execute_command(CommandOp::TransformObjects {
                ids: sel_ids.clone(),
                affine,
            });
        }
        if ui.small_button("⇅ Mirror V").clicked() {
            let c = bb.center();
            let affine = Affine2D::translate(c.x, c.y)
                .compose(&Affine2D::scale(1.0, -1.0))
                .compose(&Affine2D::translate(-c.x, -c.y));
            let _ = app.execute_command(CommandOp::TransformObjects {
                ids: sel_ids.clone(),
                affine,
            });
        }
    }

    // Multi-selection: quick Boolean shaping buttons!
    if sel_ids.len() >= 2 {
        ui.separator();
        for (op, lbl) in [
            (BooleanOp::Weld, "Weld"),
            (BooleanOp::Trim, "Trim"),
            (BooleanOp::Intersect, "Intersect"),
            (BooleanOp::Simplify, "Simplify"),
            (BooleanOp::CreateBoundary, "Boundary"),
        ] {
            if ui.small_button(lbl).clicked() {
                app.apply_boolean_op(op);
            }
        }
        return;
    }

    // Single selection: contextual shape / text / outline / lens controls
    let single_id = sel_ids[0];
    let payload_snapshot = app
        .active_tab()
        .and_then(|t| t.document.objects.get(&single_id))
        .map(|o| (o.payload.clone(), o.outline.clone(), o.lens.clone()));

    if let Some((payload, mut outline, lens_opt)) = payload_snapshot {
        ui.separator();
        // Quick outline width / hairline control
        let mut width = outline.width_mm;
        if ui
            .add(
                DragValue::new(&mut width)
                    .range(0.0..=50.0)
                    .speed(0.05)
                    .prefix("Stroke: ")
                    .suffix(" mm"),
            )
            .changed()
        {
            outline.width_mm = width;
            outline.is_hairline = false;
            app.apply_outline(outline.clone());
        }
        if ui
            .selectable_label(outline.is_hairline, "Hairline")
            .clicked()
        {
            outline.is_hairline = !outline.is_hairline;
            app.apply_outline(outline);
        }

        // Parametric Shape controls
        match payload {
            NodePayload::Shape(ParametricShape::Rectangle {
                width: rw,
                height: rh,
                corner_radii,
                mut corner_style,
                lock_corners,
            }) => {
                ui.separator();
                let mut r = corner_radii[0];
                let r_chg = ui
                    .add(
                        DragValue::new(&mut r)
                            .range(0.0..=(rw.min(rh) * 0.5))
                            .speed(0.2)
                            .prefix("Corner: ")
                            .suffix(" mm"),
                    )
                    .changed();
                let mut style_chg = false;
                for (st, lbl) in [
                    (CornerStyle::Rounded, "Rnd"),
                    (CornerStyle::Scalloped, "Scallop"),
                    (CornerStyle::Chamfered, "Chamf"),
                ] {
                    if ui.selectable_label(corner_style == st, lbl).clicked() {
                        corner_style = st;
                        style_chg = true;
                    }
                }
                if r_chg || style_chg {
                    if let Some(tab) = app.active_tab_mut() {
                        if let Some(obj) = tab.document.objects.get_mut(&single_id) {
                            obj.payload = NodePayload::Shape(ParametricShape::Rectangle {
                                width: rw,
                                height: rh,
                                corner_radii: [r; 4],
                                corner_style,
                                lock_corners,
                            });
                        }
                        tab.mark_modified();
                    }
                }
            }
            NodePayload::Shape(ParametricShape::Polygon {
                mut sides,
                radius_x,
                radius_y,
            }) => {
                ui.separator();
                if ui
                    .add(DragValue::new(&mut sides).range(3..=64).prefix("Sides: "))
                    .changed()
                {
                    if let Some(tab) = app.active_tab_mut() {
                        if let Some(obj) = tab.document.objects.get_mut(&single_id) {
                            obj.payload = NodePayload::Shape(ParametricShape::Polygon {
                                sides,
                                radius_x,
                                radius_y,
                            });
                        }
                        tab.mark_modified();
                    }
                }
            }
            NodePayload::Shape(ParametricShape::Star {
                mut points,
                outer_radius,
                mut inner_ratio,
                complex,
            }) => {
                ui.separator();
                let p_chg = ui
                    .add(DragValue::new(&mut points).range(3..=64).prefix("Pts: "))
                    .changed();
                let i_chg = ui
                    .add(
                        DragValue::new(&mut inner_ratio)
                            .range(0.1..=0.9)
                            .speed(0.02)
                            .prefix("Sharp: "),
                    )
                    .changed();
                if p_chg || i_chg {
                    if let Some(tab) = app.active_tab_mut() {
                        if let Some(obj) = tab.document.objects.get_mut(&single_id) {
                            obj.payload = NodePayload::Shape(ParametricShape::Star {
                                points,
                                outer_radius,
                                inner_ratio,
                                complex,
                            });
                        }
                        tab.mark_modified();
                    }
                }
            }
            NodePayload::ArtisticText(mut art) => {
                ui.separator();
                if let Some(run) = art.runs.first_mut() {
                    let mut changed = false;
                    changed |= ui
                        .add(
                            DragValue::new(&mut run.style.font_size_pt)
                                .range(4.0..=400.0)
                                .suffix(" pt"),
                        )
                        .changed();
                    if ui.selectable_label(run.style.bold, "B").clicked() {
                        run.style.bold = !run.style.bold;
                        changed = true;
                    }
                    if ui.selectable_label(run.style.italic, "I").clicked() {
                        run.style.italic = !run.style.italic;
                        changed = true;
                    }
                    let is_rtl = art.direction == TextDirection::Rtl;
                    if ui.selectable_label(is_rtl, "RTL").clicked() {
                        let next = if is_rtl {
                            TextDirection::Ltr
                        } else {
                            TextDirection::Rtl
                        };
                        art.direction = next;
                        run.style.direction = next;
                        changed = true;
                    }
                    let is_kashida = art.align == TextAlign::ForceJustifyKashida;
                    if ui.selectable_label(is_kashida, "Kashida").clicked() {
                        let next = if is_kashida {
                            TextAlign::Right
                        } else {
                            TextAlign::ForceJustifyKashida
                        };
                        art.align = next;
                        run.style.align = next;
                        if next == TextAlign::ForceJustifyKashida && run.style.kashida_stretch < 0.1
                        {
                            run.style.kashida_stretch = 0.6;
                        }
                        changed = true;
                    }
                    if changed {
                        let _ = app.execute_command(CommandOp::EditText {
                            id: single_id,
                            payload: NodePayload::ArtisticText(art),
                        });
                    }
                }
            }
            _ => {}
        }

        // Active lens pill
        if let Some(lcfg) = lens_opt {
            ui.separator();
            if ui
                .button(
                    RichText::new(format!("◉ Lens: {}", lcfg.kind.name()))
                        .size(11.0)
                        .color(tokens.accent_secondary),
                )
                .clicked()
            {
                app.lens_state.load_from(&lcfg);
                app.focus_docker(DockerTab::Lens);
            }
        }
    }
}
