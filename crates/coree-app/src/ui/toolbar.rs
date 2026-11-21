// SPDX-License-Identifier: MIT OR Apache-2.0

//! Crisp Standard Studio Toolbar (with procedural clean-room vector icons and
//! Laser Quick-Send strip) + Authentic Context-Sensitive Property Bar (`36px`)
//! adapting live to active tool and selected object(s).

use crate::shortcuts::ShortcutAction;
use crate::state::{
    DockerTab, LensDockerState, NodeSmoothness, PropertiesSubTab, RasterBrushOp, StudioApp, ToolId,
};
use crate::theme::{core_color_to_egui, egui_color_to_core, ThemeTokens, PROPERTY_BAR_HEIGHT};
use coree_core::{
    Affine2D, AlignOp, BlendMode, BooleanOp, CommandOp, ContourMode, CornerStyle, DistortKind,
    EllipseMode, EnvelopeMode, Fill, GradientStop, NodePayload, ParametricShape, Point2D, Rect2D,
    TextAlign, TextDirection, Unit, ZOrderOp,
};
use coree_render::{ToneRange, ViewMode};
use egui::{
    pos2, vec2, Color32, ComboBox, CornerRadius, DragValue, Frame, Margin, Rect, RichText, Sense,
    Shape, Stroke, StrokeKind, Ui,
};

const PAGE_PRESETS: [(&str, f64, f64); 6] = [
    ("A4", 210.0, 297.0),
    ("A3", 297.0, 420.0),
    ("Letter", 215.9, 279.4),
    ("A5", 148.0, 210.0),
    ("100×100", 100.0, 100.0),
    ("Laser 300×200", 300.0, 200.0),
];

const COMMON_FONTS: [&str; 8] = [
    "Noto Sans",
    "Noto Sans Arabic",
    "Noto Naskh Arabic",
    "DejaVu Sans",
    "DejaVu Serif",
    "Liberation Sans",
    "Arial",
    "Tahoma",
];

#[derive(Clone, Copy)]
enum StdIcon {
    NewDoc,
    OpenDoc,
    SaveDoc,
    ImportDoc,
    ExportDoc,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    PreviewF9,
}

/// Renders both the Standard Toolbar (Row 1) and the Context-Sensitive Property Bar (Row 2).
pub fn show_toolbars(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();

    // Row 1: Standard Studio Toolbar
    egui::Panel::top("coree_standard_toolbar")
        .exact_size(31.0)
        .frame(
            Frame::NONE
                .fill(tokens.surface)
                .inner_margin(Margin::symmetric(6, 3))
                .stroke(Stroke::new(1.0, tokens.border)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("std_toolbar_scroll")
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;

                        // File operations with crisp procedural vector icons
                        if std_icon_btn(
                            ui,
                            &tokens,
                            StdIcon::NewDoc,
                            "New Document (Ctrl+N)",
                            true,
                            false,
                        ) {
                            app.new_doc_dialog.open = true;
                        }
                        if std_icon_btn(
                            ui,
                            &tokens,
                            StdIcon::OpenDoc,
                            "Open File (Ctrl+O)",
                            true,
                            false,
                        ) {
                            app.open_file_dialog();
                        }
                        if std_icon_btn(
                            ui,
                            &tokens,
                            StdIcon::SaveDoc,
                            "Save Document (Ctrl+S)",
                            true,
                            false,
                        ) {
                            app.handle_shortcut(ShortcutAction::SaveDocument);
                        }
                        if std_icon_btn(
                            ui,
                            &tokens,
                            StdIcon::ImportDoc,
                            "Import Artwork (Ctrl+I)",
                            true,
                            false,
                        ) {
                            app.import_file_dialog();
                        }
                        if std_icon_btn(
                            ui,
                            &tokens,
                            StdIcon::ExportDoc,
                            "Export Document (Ctrl+E)",
                            true,
                            false,
                        ) {
                            app.export_dialog.open = true;
                        }
                        ui.separator();

                        // History & Clipboard
                        let (can_undo, can_redo, has_sel) =
                            app.active_tab().map_or((false, false, false), |t| {
                                (
                                    t.history.can_undo(),
                                    t.history.can_redo(),
                                    !t.selection.is_empty(),
                                )
                            });
                        if std_icon_btn(
                            ui,
                            &tokens,
                            StdIcon::Undo,
                            "Undo (Ctrl+Z)",
                            can_undo,
                            false,
                        ) {
                            let _ = app.undo();
                        }
                        if std_icon_btn(
                            ui,
                            &tokens,
                            StdIcon::Redo,
                            "Redo (Ctrl+Shift+Z)",
                            can_redo,
                            false,
                        ) {
                            let _ = app.redo();
                        }
                        ui.separator();
                        if std_icon_btn(ui, &tokens, StdIcon::Cut, "Cut (Ctrl+X)", has_sel, false) {
                            app.cut_selection();
                        }
                        if std_icon_btn(ui, &tokens, StdIcon::Copy, "Copy (Ctrl+C)", has_sel, false)
                        {
                            app.copy_selection();
                        }
                        if std_icon_btn(
                            ui,
                            &tokens,
                            StdIcon::Paste,
                            "Paste (Ctrl+V)",
                            !app.clipboard_objects.is_empty(),
                            false,
                        ) {
                            app.paste_clipboard();
                        }
                        ui.separator();

                        // Zoom preset combo
                        let zoom_pct = app
                            .active_tab()
                            .map_or(100.0, |t| (t.viewport.zoom * 100.0).round());
                        ComboBox::from_id_salt("std_zoom_preset")
                            .width(74.0)
                            .selected_text(format!("{zoom_pct:.0}%"))
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_label(false, "To Selected (Shift+F2)")
                                    .clicked()
                                {
                                    app.handle_shortcut(ShortcutAction::ZoomToSelected);
                                }
                                if ui.selectable_label(false, "To Page (Shift+F4)").clicked() {
                                    app.handle_shortcut(ShortcutAction::ZoomToPage);
                                }
                                if ui.selectable_label(false, "To All (F4)").clicked() {
                                    app.handle_shortcut(ShortcutAction::ZoomToAll);
                                }
                                ui.separator();
                                for pct in [25.0, 50.0, 75.0, 100.0, 150.0, 200.0, 400.0] {
                                    if ui.selectable_label(false, format!("{pct:.0}%")).clicked() {
                                        if let Some(t) = app.active_tab_mut() {
                                            t.viewport.zoom = pct / 100.0;
                                            t.needs_rerender = true;
                                        }
                                    }
                                }
                            });

                        // F9 Full-Screen Preview toggle
                        if std_icon_btn(
                            ui,
                            &tokens,
                            StdIcon::PreviewF9,
                            "Full-Screen Preview (F9)",
                            true,
                            app.full_screen_preview,
                        ) {
                            app.handle_shortcut(ShortcutAction::ToggleFullScreenPreview);
                        }

                        // View Mode combo & Snap toggles
                        if let Some(tab) = app.active_tab_mut() {
                            ComboBox::from_id_salt("std_view_mode")
                                .width(84.0)
                                .selected_text(format!("{:?}", tab.view_mode))
                                .show_ui(ui, |ui| {
                                    for mode in [
                                        ViewMode::Wireframe,
                                        ViewMode::Normal,
                                        ViewMode::Enhanced,
                                        ViewMode::Pixels,
                                    ] {
                                        if ui
                                            .selectable_value(
                                                &mut tab.view_mode,
                                                mode,
                                                format!("{mode:?}"),
                                            )
                                            .changed()
                                        {
                                            tab.needs_rerender = true;
                                        }
                                    }
                                });

                            if ui
                                .selectable_label(tab.document.multipage_view, "⊞ Pages")
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

                        // CMYK Soft-Proof toggle
                        if ui
                            .selectable_label(app.soft_proof_cmyk, "Proof CMYK")
                            .on_hover_text("Toggle CMYK Soft-Proofing (ICC Color Management)")
                            .clicked()
                        {
                            app.soft_proof_cmyk = !app.soft_proof_cmyk;
                            app.sync_proof_options_to_tabs();
                        }

                        ui.separator();

                        // Compact Laser Quick-Send Strip
                        if ui
                            .selectable_label(
                                app.show_laser_toolpath_overlay,
                                RichText::new("⚡ Laser Preview").color(
                                    if app.show_laser_toolpath_overlay {
                                        tokens.accent_secondary
                                    } else {
                                        tokens.text
                                    },
                                ),
                            )
                            .on_hover_text("Compute & Toggle Live Laser Toolpath Overlay")
                            .clicked()
                        {
                            if app.show_laser_toolpath_overlay {
                                app.show_laser_toolpath_overlay = false;
                            } else {
                                app.refresh_laser_plan_preview();
                            }
                        }
                        for (plugin_id, short_lbl, tip) in [
                            ("lightburn-bridge", "LightBurn", "Send artwork to LightBurn"),
                            (
                                "rdworks-ruida",
                                "RDWorks",
                                "Send DXF R12 to RDWorks (Ruida)",
                            ),
                            (
                                "ezcad-fiber",
                                "EZCad",
                                "Send Hatched DXF R12 to EZCad (Fiber)",
                            ),
                            (
                                "vinyl-plotter-hpgl",
                                "Plotter",
                                "Send HPGL PLT to Vinyl Plotter",
                            ),
                        ] {
                            if ui.small_button(short_lbl).on_hover_text(tip).clicked() {
                                app.send_to_laser_plugin(plugin_id, true);
                            }
                        }

                        ui.separator();

                        // One-Click CutContour & Macro Record
                        if ui
                            .button(
                                RichText::new("✂ CutContour")
                                    .size(11.0)
                                    .color(tokens.cutcontour),
                            )
                            .on_hover_text("One-click welded CutContour spot hairline")
                            .clicked()
                        {
                            app.generate_cut_contour_active();
                        }
                        let recording = app.active_tab().is_some_and(|t| t.history.recording_macro);
                        if recording {
                            if ui
                                .button(
                                    RichText::new("■ Stop Macro")
                                        .size(11.0)
                                        .color(tokens.accent_secondary),
                                )
                                .clicked()
                            {
                                app.stop_macro_recording();
                            }
                        } else if ui
                            .small_button("● Rec")
                            .on_hover_text("Record actions to Rhai script")
                            .clicked()
                        {
                            app.start_macro_recording();
                        }
                    });
                });
        });

    // Row 2: Authentic Context-Sensitive Property Bar (36px)
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
                        ui.spacing_mut().item_spacing.x = 5.0;
                        show_context_property_contents(ui, app);
                    });
                });
        });
}

fn show_context_property_contents(ui: &mut Ui, app: &mut StudioApp) {
    let tokens = app.theme.tokens();

    // 1. Dedicated Tool Property Bars (takes priority regardless of selection count)
    match app.active_tool {
        ToolId::ShapeNode => {
            show_shape_node_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::Smooth | ToolId::Smear | ToolId::Twirl | ToolId::AttractRepel | ToolId::Roughen => {
            show_sculpt_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::BitmapBrush
        | ToolId::BitmapEraser
        | ToolId::CloneStamp
        | ToolId::Smudge
        | ToolId::MaskSelect => {
            show_raster_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::LensTool => {
            show_lens_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::DropShadow
        | ToolId::BlockShadow
        | ToolId::Contour
        | ToolId::Blend
        | ToolId::Distort
        | ToolId::Envelope
        | ToolId::Extrude25D
        | ToolId::Transparency => {
            show_interactive_effect_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::InteractiveFill | ToolId::MeshFill | ToolId::SmartFill => {
            show_fill_tool_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
            show_eyedropper_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::Crop | ToolId::Knife | ToolId::VirtualSegmentDelete | ToolId::Eraser => {
            show_crop_cut_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::Zoom | ToolId::Pan => {
            show_zoom_pan_property_bar(ui, app, &tokens);
            return;
        }
        ToolId::ArtisticText | ToolId::ParagraphText => {
            show_text_property_bar(ui, app, &tokens);
            return;
        }
        _ => {}
    }

    let sel_ids = app
        .active_tab()
        .map(|t| t.selection.objects.clone())
        .unwrap_or_default();

    // 2. Pick / Drawing Tools with 0 Objects Selected -> Document / Page Defaults Bar
    if sel_ids.is_empty() {
        show_page_defaults_property_bar(ui, app, &tokens);
        return;
    }

    // 3. Pick / Drawing Tools with 1+ Objects Selected -> Full Object Transform + Contextual Bar
    show_selected_objects_property_bar(ui, app, &tokens, &sel_ids);
}

// -----------------------------------------------------------------------------
// F10 Shape / Node Editing Property Bar (all 11 ShapeNode operations)
// -----------------------------------------------------------------------------

fn show_shape_node_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    ui.label(RichText::new("Shape (F10):").color(tokens.accent).strong());
    let selected_nodes = app
        .active_tab()
        .map_or(0, |t| t.selection.node_selection.nodes.len());
    ui.label(
        RichText::new(format!("{selected_nodes} node(s)"))
            .size(11.0)
            .color(tokens.text_muted),
    );
    ui.separator();

    if ui
        .small_button("+ Add")
        .on_hover_text("Add Node(s) at Midpoint")
        .clicked()
    {
        app.shape_add_node();
    }
    if ui
        .small_button("− Del")
        .on_hover_text("Delete Selected Node(s)")
        .clicked()
    {
        app.shape_delete_selected_nodes();
    }
    if ui
        .small_button("Join")
        .on_hover_text("Join Two End Nodes")
        .clicked()
    {
        app.shape_join_nodes();
    }
    if ui
        .small_button("Break")
        .on_hover_text("Break Curve at Selected Node")
        .clicked()
    {
        app.shape_break_curve();
    }
    ui.separator();

    if ui
        .small_button("To Line")
        .on_hover_text("Convert Segment to Straight Line")
        .clicked()
    {
        app.shape_convert_to_line();
    }
    if ui
        .small_button("To Curve")
        .on_hover_text("Convert Segment to Bézier Curve")
        .clicked()
    {
        app.shape_convert_to_curve();
    }
    ui.separator();

    if ui
        .small_button("Cusp")
        .on_hover_text("Make Node Cusp (Sharp Corner)")
        .clicked()
    {
        app.shape_set_node_smoothness(NodeSmoothness::Cusp);
    }
    if ui
        .small_button("Smooth")
        .on_hover_text("Make Node Smooth (Collinear Tangents)")
        .clicked()
    {
        app.shape_set_node_smoothness(NodeSmoothness::Smooth);
    }
    if ui
        .small_button("Symm")
        .on_hover_text("Make Node Symmetrical (Equal Tangents)")
        .clicked()
    {
        app.shape_set_node_smoothness(NodeSmoothness::Symmetrical);
    }
    ui.separator();

    if ui
        .small_button("Reverse")
        .on_hover_text("Reverse Subpath Direction")
        .clicked()
    {
        app.shape_reverse_subpaths();
    }
    if ui
        .small_button("Close")
        .on_hover_text("Close Open Curve")
        .clicked()
    {
        app.shape_close_curve();
    }
    if ui
        .small_button("Select All")
        .on_hover_text("Select All Nodes on Curve")
        .clicked()
    {
        app.shape_select_all_nodes();
    }
    ui.separator();

    ui.add(
        DragValue::new(&mut app.tool_settings.shape_reduce_tolerance_mm)
            .range(0.05..=5.0)
            .speed(0.05)
            .prefix("Tol: ")
            .suffix(" mm"),
    );
    if ui
        .small_button("Reduce Nodes")
        .on_hover_text("Simplify curve & reduce redundant nodes")
        .clicked()
    {
        let tol = app.tool_settings.shape_reduce_tolerance_mm;
        app.shape_reduce_nodes(tol);
    }
}

// -----------------------------------------------------------------------------
// Sculpting Tools Property Bar
// -----------------------------------------------------------------------------

fn show_sculpt_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    ui.label(
        RichText::new(format!("{}:", app.active_tool.label()))
            .color(tokens.accent)
            .strong(),
    );
    ui.add(
        DragValue::new(&mut app.tool_settings.sculpt_radius_mm)
            .range(1.0..=100.0)
            .speed(0.5)
            .prefix("Nib: ")
            .suffix(" mm"),
    );
    ui.add(
        DragValue::new(&mut app.tool_settings.sculpt_strength)
            .range(0.05..=1.0)
            .speed(0.02)
            .prefix("Rate: "),
    );
    if app.active_tool == ToolId::Roughen {
        ui.separator();
        ui.add(
            DragValue::new(&mut app.effects_state.frequency)
                .range(0.5..=12.0)
                .speed(0.2)
                .prefix("Spikes/Freq: "),
        );
    }
    ui.separator();
    let hint = match app.active_tool {
        ToolId::Twirl => "Drag across path edges · Shift reverses spin",
        ToolId::AttractRepel => "Drag to Attract · Hold Shift to Repel",
        ToolId::Roughen => "Drag along outlines to add serrated detail",
        _ => "Drag across vector outlines to sculpt live",
    };
    ui.label(RichText::new(hint).size(11.0).color(tokens.text_muted));
}

// -----------------------------------------------------------------------------
// Integrated Bitmap Studio Property Bar (all 8 RasterBrushOps)
// -----------------------------------------------------------------------------

fn show_raster_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    ui.label(RichText::new("Bitmap:").color(tokens.accent).strong());

    ComboBox::from_id_salt("prop_raster_op")
        .width(126.0)
        .selected_text(app.tool_settings.raster_op.label())
        .show_ui(ui, |ui| {
            for op in RasterBrushOp::ALL {
                ui.selectable_value(&mut app.tool_settings.raster_op, op, op.label());
            }
        });

    ui.add(
        DragValue::new(&mut app.tool_settings.brush_radius_px)
            .range(1.0..=200.0)
            .speed(1.0)
            .prefix("Size: ")
            .suffix(" px"),
    );
    ui.add(
        DragValue::new(&mut app.tool_settings.brush_hardness)
            .range(0.0..=1.0)
            .speed(0.02)
            .prefix("Hard: "),
    );
    let mut bc = core_color_to_egui(&app.tool_settings.brush_color);
    if ui.color_edit_button_srgba(&mut bc).changed() {
        app.tool_settings.brush_color = egui_color_to_core(bc);
    }

    match app.tool_settings.raster_op {
        RasterBrushOp::Smudge => {
            ui.add(
                DragValue::new(&mut app.tool_settings.smudge_strength)
                    .range(0.05..=1.0)
                    .speed(0.02)
                    .prefix("Strength: "),
            );
        }
        RasterBrushOp::Dodge | RasterBrushOp::Burn => {
            ui.separator();
            ui.label("Tone:");
            for (tone, lbl) in [
                (ToneRange::Shadows, "Shadows"),
                (ToneRange::Midtones, "Midtones"),
                (ToneRange::Highlights, "Highlights"),
            ] {
                ui.selectable_value(&mut app.tool_settings.dodge_burn_tone, tone, lbl);
            }
            ui.add(
                DragValue::new(&mut app.tool_settings.dodge_burn_exposure)
                    .range(0.05..=1.0)
                    .speed(0.02)
                    .prefix("Exp: "),
            );
        }
        RasterBrushOp::FloodFill => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.flood_tolerance)
                    .range(0..=255)
                    .prefix("Tolerance: "),
            );
        }
        _ => {}
    }

    if app.active_tool == ToolId::MaskSelect
        || app
            .active_tab()
            .is_some_and(|t| t.raster_mask_rect_mm.is_some())
    {
        ui.separator();
        if let Some(mask) = app.active_tab().and_then(|t| t.raster_mask_rect_mm) {
            ui.label(
                RichText::new(format!("Mask: {:.1}×{:.1} mm", mask.width(), mask.height()))
                    .size(11.0)
                    .color(tokens.text_muted),
            );
            if ui.small_button("Clear Mask").clicked() {
                if let Some(tab) = app.active_tab_mut() {
                    tab.raster_mask_rect_mm = None;
                }
            }
        } else {
            ui.label(
                RichText::new("Drag on canvas to define pixel selection mask")
                    .size(11.0)
                    .color(tokens.text_muted),
            );
        }
    }
}

// -----------------------------------------------------------------------------
// Lens, Interactive Effects, Fill, Eyedropper, Crop/Cut, Zoom/Pan Property Bars
// -----------------------------------------------------------------------------

fn show_lens_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    ui.label(RichText::new("Lens:").color(tokens.accent).strong());
    ComboBox::from_id_salt("prop_lens_kind")
        .width(136.0)
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
    if ui.small_button("Remove").clicked() {
        app.apply_lens_to_selection(None);
    }
}

fn show_interactive_effect_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    let idx = match app.active_tool {
        ToolId::DropShadow => 0,
        ToolId::BlockShadow => 1,
        ToolId::Contour => 2,
        ToolId::Blend => 3,
        ToolId::Distort => 4,
        ToolId::Envelope => 5,
        ToolId::Extrude25D => 6,
        _ => 7,
    };
    app.effects_state.selected_effect_kind = idx;
    ui.label(
        RichText::new(format!("{}:", app.active_tool.label()))
            .color(tokens.accent)
            .strong(),
    );

    let es = &mut app.effects_state;
    match app.active_tool {
        ToolId::DropShadow => {
            ui.add(
                DragValue::new(&mut es.dx_mm)
                    .range(-40.0..=40.0)
                    .prefix("X: ")
                    .suffix(" mm"),
            );
            ui.add(
                DragValue::new(&mut es.dy_mm)
                    .range(-40.0..=40.0)
                    .prefix("Y: ")
                    .suffix(" mm"),
            );
            ui.add(
                DragValue::new(&mut es.blur_mm)
                    .range(0.0..=25.0)
                    .prefix("Feather: ")
                    .suffix(" mm"),
            );
            ui.add(
                DragValue::new(&mut es.opacity)
                    .range(0.0..=1.0)
                    .speed(0.02)
                    .prefix("Alpha: "),
            );
        }
        ToolId::BlockShadow => {
            ui.add(
                DragValue::new(&mut es.dx_mm)
                    .range(-50.0..=50.0)
                    .prefix("Depth X: ")
                    .suffix(" mm"),
            );
            ui.add(
                DragValue::new(&mut es.dy_mm)
                    .range(-50.0..=50.0)
                    .prefix("Depth Y: ")
                    .suffix(" mm"),
            );
        }
        ToolId::Contour => {
            ui.selectable_value(&mut es.contour_mode, ContourMode::ToCenter, "Center");
            ui.selectable_value(&mut es.contour_mode, ContourMode::Inside, "Inside");
            ui.selectable_value(&mut es.contour_mode, ContourMode::Outside, "Outside");
            ui.add(
                DragValue::new(&mut es.steps)
                    .range(1..=30)
                    .prefix("Steps: "),
            );
            ui.add(
                DragValue::new(&mut es.offset_mm)
                    .range(0.2..=25.0)
                    .prefix("Offset: ")
                    .suffix(" mm"),
            );
        }
        ToolId::Blend => {
            ui.add(
                DragValue::new(&mut es.steps)
                    .range(1..=60)
                    .prefix("Blend Steps: "),
            );
        }
        ToolId::Distort => {
            ui.selectable_value(&mut es.distort_kind, DistortKind::PushPull, "Push/Pull");
            ui.selectable_value(&mut es.distort_kind, DistortKind::Zipper, "Zipper");
            ui.selectable_value(&mut es.distort_kind, DistortKind::Twister, "Twister");
            ui.add(
                DragValue::new(&mut es.amplitude)
                    .range(1.0..=100.0)
                    .prefix("Amp: "),
            );
            ui.add(
                DragValue::new(&mut es.frequency)
                    .range(0.5..=12.0)
                    .prefix("Freq: "),
            );
        }
        ToolId::Envelope => {
            ui.selectable_value(&mut es.envelope_mode, EnvelopeMode::Straight, "Straight");
            ui.selectable_value(&mut es.envelope_mode, EnvelopeMode::SingleArc, "Single Arc");
            ui.selectable_value(&mut es.envelope_mode, EnvelopeMode::DoubleArc, "Double Arc");
        }
        ToolId::Extrude25D => {
            ui.add(
                DragValue::new(&mut es.extrude_depth)
                    .range(2.0..=80.0)
                    .prefix("Depth: ")
                    .suffix(" mm"),
            );
        }
        ToolId::Transparency => {
            ui.add(
                DragValue::new(&mut es.opacity)
                    .range(0.0..=1.0)
                    .speed(0.02)
                    .prefix("Opacity: "),
            );
            ComboBox::from_id_salt("prop_trans_blend")
                .width(100.0)
                .selected_text(format!("{:?}", es.blend_mode))
                .show_ui(ui, |ui| {
                    for bm in [
                        BlendMode::Normal,
                        BlendMode::Multiply,
                        BlendMode::Screen,
                        BlendMode::Overlay,
                        BlendMode::Darken,
                        BlendMode::Lighten,
                    ] {
                        ui.selectable_value(&mut es.blend_mode, bm, format!("{bm:?}"));
                    }
                });
        }
        _ => {}
    }

    if ui.small_button("Apply Effect").clicked() {
        let eff = app.build_selected_effect();
        app.add_effect_to_selection(eff);
    }
    if ui.small_button("Effects Docker").clicked() {
        app.focus_docker(DockerTab::Effects);
    }
}

fn show_fill_tool_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    ui.label(
        RichText::new(format!("{}:", app.active_tool.label()))
            .color(tokens.accent)
            .strong(),
    );
    match app.active_tool {
        ToolId::MeshFill => {
            ui.add(
                DragValue::new(&mut app.properties_state.mesh_rows)
                    .range(2..=8)
                    .prefix("Rows: "),
            );
            ui.add(
                DragValue::new(&mut app.properties_state.mesh_cols)
                    .range(2..=8)
                    .prefix("Cols: "),
            );
            if ui.small_button("Apply Mesh Grid").clicked() {
                let bb = app
                    .selected_local_bounds()
                    .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 50.0, 50.0));
                let fill = StudioApp::build_mesh_fill_for_bounds(
                    bb,
                    app.properties_state.mesh_rows,
                    app.properties_state.mesh_cols,
                );
                app.apply_fill(fill);
            }
        }
        ToolId::InteractiveFill => {
            let mut sc = core_color_to_egui(&app.properties_state.grad_start_color);
            let mut ec = core_color_to_egui(&app.properties_state.grad_end_color);
            ui.label("Start:");
            if ui.color_edit_button_srgba(&mut sc).changed() {
                app.properties_state.grad_start_color = egui_color_to_core(sc);
            }
            ui.label("End:");
            if ui.color_edit_button_srgba(&mut ec).changed() {
                app.properties_state.grad_end_color = egui_color_to_core(ec);
            }
            if ui.small_button("Linear").clicked() {
                app.apply_fill(Fill::LinearGradient {
                    start: Point2D::ZERO,
                    end: Point2D::new(60.0, 40.0),
                    stops: vec![
                        GradientStop::new(0.0, app.properties_state.grad_start_color.clone()),
                        GradientStop::new(1.0, app.properties_state.grad_end_color.clone()),
                    ],
                });
            }
            if ui.small_button("Radial").clicked() {
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
            if ui.small_button("Conical").clicked() {
                app.apply_fill(Fill::ConicalGradient {
                    center: Point2D::new(30.0, 30.0),
                    angle_deg: 0.0,
                    stops: vec![
                        GradientStop::new(0.0, app.properties_state.grad_start_color.clone()),
                        GradientStop::new(1.0, app.properties_state.grad_end_color.clone()),
                    ],
                });
            }
        }
        ToolId::SmartFill => {
            let mut c32 = core_color_to_egui(
                &app.default_fill
                    .referenced_colors()
                    .into_iter()
                    .next()
                    .unwrap_or(coree_core::Color::CYAN),
            );
            ui.label("Fill Color:");
            if ui.color_edit_button_srgba(&mut c32).changed() {
                app.default_fill = Fill::Solid(egui_color_to_core(c32));
            }
            ui.label(
                RichText::new("Click any enclosed region to generate a filled vector shape")
                    .size(11.0)
                    .color(tokens.text_muted),
            );
        }
        _ => {}
    }
}

fn show_eyedropper_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    ui.label(
        RichText::new(format!("{}:", app.active_tool.label()))
            .color(tokens.accent)
            .strong(),
    );
    ui.selectable_value(
        &mut app.tool_settings.eyedropper_apply_mode,
        false,
        "1. Sample",
    );
    ui.selectable_value(
        &mut app.tool_settings.eyedropper_apply_mode,
        true,
        "2. Apply",
    );
    ui.separator();
    let sample_c = app
        .default_fill
        .referenced_colors()
        .into_iter()
        .next()
        .unwrap_or(coree_core::Color::CYAN);
    let (sw_rect, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    ui.painter().rect(
        sw_rect,
        CornerRadius::same(2),
        core_color_to_egui(&sample_c),
        Stroke::new(1.0, tokens.border_input),
        StrokeKind::Inside,
    );
    ui.label(
        RichText::new("Hold Shift+Click to re-sample anytime")
            .size(11.0)
            .color(tokens.text_muted),
    );
}

fn show_crop_cut_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    ui.label(
        RichText::new(format!("{}:", app.active_tool.label()))
            .color(tokens.accent)
            .strong(),
    );
    match app.active_tool {
        ToolId::Crop => {
            if let Some(mut rect) = app.active_tab().and_then(|t| t.pending_crop_rect) {
                let mut w = rect.width();
                let mut h = rect.height();
                let mut changed = false;
                changed |= ui
                    .add(DragValue::new(&mut rect.min_x).prefix("X: ").suffix(" mm"))
                    .changed();
                changed |= ui
                    .add(DragValue::new(&mut rect.min_y).prefix("Y: ").suffix(" mm"))
                    .changed();
                changed |= ui
                    .add(
                        DragValue::new(&mut w)
                            .range(1.0..=5000.0)
                            .prefix("W: ")
                            .suffix(" mm"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        DragValue::new(&mut h)
                            .range(1.0..=5000.0)
                            .prefix("H: ")
                            .suffix(" mm"),
                    )
                    .changed();
                if changed {
                    rect.max_x = rect.min_x + w;
                    rect.max_y = rect.min_y + h;
                    if let Some(tab) = app.active_tab_mut() {
                        tab.pending_crop_rect = Some(rect);
                    }
                }
                if ui.small_button("✓ Crop (Enter)").clicked() {
                    app.confirm_active_tool_action();
                }
                if ui.small_button("Cancel").clicked() {
                    if let Some(tab) = app.active_tab_mut() {
                        tab.pending_crop_rect = None;
                    }
                }
            } else {
                ui.label(
                    RichText::new("Drag a crop region on canvas, then press Enter to crop")
                        .size(11.0)
                        .color(tokens.text_muted),
                );
            }
        }
        ToolId::Eraser => {
            ui.add(
                DragValue::new(&mut app.tool_settings.eraser_radius_mm)
                    .range(0.5..=60.0)
                    .speed(0.2)
                    .prefix("Eraser Radius: ")
                    .suffix(" mm"),
            );
        }
        _ => {
            ui.label(
                RichText::new("Drag across curves to slice or delete segments")
                    .size(11.0)
                    .color(tokens.text_muted),
            );
        }
    }
}

fn show_zoom_pan_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    ui.label(
        RichText::new(format!("{}:", app.active_tool.label()))
            .color(tokens.accent)
            .strong(),
    );
    if ui.small_button("Zoom In (+)").clicked() {
        app.zoom_step(1.25);
    }
    if ui.small_button("Zoom Out (−)").clicked() {
        app.zoom_step(0.8);
    }
    if ui.small_button("To Selected (Shift+F2)").clicked() {
        app.handle_shortcut(ShortcutAction::ZoomToSelected);
    }
    if ui.small_button("To All (F4)").clicked() {
        app.handle_shortcut(ShortcutAction::ZoomToAll);
    }
    if ui.small_button("To Page (Shift+F4)").clicked() {
        app.handle_shortcut(ShortcutAction::ZoomToPage);
    }
    if ui.small_button("100%").clicked() {
        app.handle_shortcut(ShortcutAction::ZoomActual);
    }
}

// -----------------------------------------------------------------------------
// Text & Arabic RTL / Kashida Property Bar
// -----------------------------------------------------------------------------

fn show_text_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    ui.label(RichText::new("Text:").color(tokens.accent).strong());

    let selected_text = app.active_tab().and_then(|t| {
        let oid = *t.selection.objects.first()?;
        let obj = t.document.objects.get(&oid)?;
        match &obj.payload {
            NodePayload::ArtisticText(_) | NodePayload::ParagraphText(_) => {
                Some((oid, obj.payload.clone()))
            }
            _ => None,
        }
    });

    if let Some((oid, mut payload)) = selected_text {
        let mut changed = false;
        match &mut payload {
            NodePayload::ArtisticText(art) => {
                if let Some(run) = art.runs.first_mut() {
                    changed |= render_text_style_strip(
                        ui,
                        &mut run.style,
                        &mut art.direction,
                        &mut art.align,
                        "prop_txt_font",
                    );
                }
            }
            NodePayload::ParagraphText(para) => {
                if let Some(run) = para.runs.first_mut() {
                    changed |= render_text_style_strip(
                        ui,
                        &mut run.style,
                        &mut para.direction,
                        &mut para.align,
                        "prop_para_font",
                    );
                }
            }
            _ => {}
        }
        if changed {
            let _ = app.execute_command(CommandOp::EditText { id: oid, payload });
        }
    } else {
        let mut dir = app.default_text_style.direction;
        let mut align = app.default_text_style.align;
        let changed = render_text_style_strip(
            ui,
            &mut app.default_text_style,
            &mut dir,
            &mut align,
            "prop_def_font",
        );
        if changed {
            app.default_text_style.direction = dir;
            app.default_text_style.align = align;
        }
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut app.tool_settings.text_insert_draft)
                .hint_text("Draft text...")
                .desired_width(110.0),
        );
    }

    ui.separator();
    if ui
        .small_button("Fit to Path")
        .on_hover_text("Fit Selected Text to Selected Path")
        .clicked()
    {
        app.fit_selected_text_to_path();
    }
    if ui
        .small_button("To Curves")
        .on_hover_text("Convert Text to Vector Curves (Ctrl+Q)")
        .clicked()
    {
        app.convert_selection_to_curves();
    }
    if ui
        .small_button("¶ Glyphs")
        .on_hover_text("Open Interactive Glyphs Browser")
        .clicked()
    {
        app.properties_state.active_tab = PropertiesSubTab::Text;
        app.focus_docker(DockerTab::Properties);
    }
}

fn render_text_style_strip(
    ui: &mut Ui,
    style: &mut coree_core::TextStyle,
    direction: &mut TextDirection,
    align: &mut TextAlign,
    combo_salt: &str,
) -> bool {
    let mut changed = false;
    ComboBox::from_id_salt(combo_salt)
        .width(116.0)
        .selected_text(&style.font_family)
        .show_ui(ui, |ui| {
            for f in COMMON_FONTS {
                if ui.selectable_label(style.font_family == f, f).clicked() {
                    style.font_family = f.to_owned();
                    changed = true;
                }
            }
        });
    changed |= ui
        .add(
            DragValue::new(&mut style.font_size_pt)
                .range(4.0..=400.0)
                .speed(0.5)
                .suffix(" pt"),
        )
        .changed();
    if ui
        .selectable_label(style.bold, RichText::new("B").strong())
        .clicked()
    {
        style.set_weight(if style.bold { 400 } else { 700 });
        changed = true;
    }
    let cur_w = style.effective_weight();
    ComboBox::from_id_salt((combo_salt, "weight"))
        .width(64.0)
        .selected_text(format!("w{cur_w}"))
        .show_ui(ui, |ui| {
            for (w, lbl) in [
                (100, "100 Thin"),
                (300, "300 Light"),
                (400, "400 Regular"),
                (500, "500 Medium"),
                (600, "600 SemiBold"),
                (700, "700 Bold"),
                (800, "800 ExtraBold"),
                (900, "900 Black"),
            ] {
                if ui.selectable_label(cur_w == w, lbl).clicked() {
                    style.set_weight(w);
                    changed = true;
                }
            }
        });
    if ui
        .selectable_label(style.italic, RichText::new("I").italics())
        .clicked()
    {
        style.italic = !style.italic;
        changed = true;
    }
    if ui
        .selectable_label(style.underline, RichText::new("U").underline())
        .clicked()
    {
        style.underline = !style.underline;
        changed = true;
    }
    let bidi_cur = style.bidi_font_family.as_deref().unwrap_or("Auto");
    ComboBox::from_id_salt((combo_salt, "bidi_font"))
        .width(96.0)
        .selected_text(format!("RTL:{bidi_cur}"))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(style.bidi_font_family.is_none(), "Auto (Primary)")
                .clicked()
            {
                style.bidi_font_family = None;
                changed = true;
            }
            for f in [
                "Noto Sans Arabic",
                "Noto Naskh Arabic",
                "Amiri",
                "Cairo",
                "Tahoma",
                "Arial",
            ] {
                if ui
                    .selectable_label(style.bidi_font_family.as_deref() == Some(f), f)
                    .clicked()
                {
                    style.bidi_font_family = Some(f.to_owned());
                    changed = true;
                }
            }
        });
    if ui
        .selectable_label(style.opentype_features.liga, "liga")
        .on_hover_text("OpenType Standard Ligatures")
        .clicked()
    {
        style.opentype_features.liga = !style.opentype_features.liga;
        changed = true;
    }
    if ui
        .selectable_label(style.opentype_features.calt, "calt")
        .on_hover_text("OpenType Contextual Alternates (Arabic contextual joining)")
        .clicked()
    {
        style.opentype_features.calt = !style.opentype_features.calt;
        changed = true;
    }
    if ui
        .selectable_label(style.opentype_features.kern, "kern")
        .on_hover_text("OpenType Kerning")
        .clicked()
    {
        style.opentype_features.kern = !style.opentype_features.kern;
        changed = true;
    }
    ui.separator();
    for (al, lbl) in [
        (TextAlign::Left, "L"),
        (TextAlign::Center, "C"),
        (TextAlign::Right, "R"),
        (TextAlign::Justify, "J"),
    ] {
        if ui.selectable_label(*align == al, lbl).clicked() {
            *align = al;
            style.align = al;
            changed = true;
        }
    }
    ui.separator();
    let is_rtl = *direction == TextDirection::Rtl;
    if ui
        .selectable_label(is_rtl, "RTL")
        .on_hover_text("Toggle Arabic Right-to-Left Direction")
        .clicked()
    {
        let next = if is_rtl {
            TextDirection::Ltr
        } else {
            TextDirection::Rtl
        };
        *direction = next;
        style.direction = next;
        changed = true;
    }
    let is_kashida = *align == TextAlign::ForceJustifyKashida;
    if ui
        .selectable_label(is_kashida, "Kashida")
        .on_hover_text("Arabic Kashida Justification")
        .clicked()
    {
        let next = if is_kashida {
            TextAlign::Right
        } else {
            TextAlign::ForceJustifyKashida
        };
        *align = next;
        style.align = next;
        if next == TextAlign::ForceJustifyKashida && style.kashida_stretch < 0.1 {
            style.kashida_stretch = 0.65;
        }
        changed = true;
    }
    changed |= ui
        .add(
            DragValue::new(&mut style.kashida_stretch)
                .range(0.0..=2.0)
                .speed(0.05)
                .prefix("K: "),
        )
        .on_hover_text("Arabic Kashida Stretch (0.0 .. 2.0)")
        .changed();
    changed
}

// -----------------------------------------------------------------------------
// Document / Page Defaults Property Bar (0 Objects Selected)
// -----------------------------------------------------------------------------

fn show_page_defaults_property_bar(ui: &mut Ui, app: &mut StudioApp, tokens: &ThemeTokens) {
    let mut autofit_clicked = false;
    {
        let Some(tab) = app.active_tab_mut() else {
            return;
        };
        let mut page_modified = false;
        if let Some(page) = tab.document.active_page_mut() {
            // Page Preset combo
            let active_preset_name = PAGE_PRESETS
                .iter()
                .find(|(_, pw, ph)| {
                    ((page.width_mm - *pw).abs() < 0.5 && (page.height_mm - *ph).abs() < 0.5)
                        || ((page.width_mm - *ph).abs() < 0.5 && (page.height_mm - *pw).abs() < 0.5)
                })
                .map_or("Custom", |(n, _, _)| *n);
            ComboBox::from_id_salt("prop_page_preset")
                .width(90.0)
                .selected_text(active_preset_name)
                .show_ui(ui, |ui| {
                    for (name, pw, ph) in PAGE_PRESETS {
                        if ui
                            .selectable_label(active_preset_name == name, name)
                            .clicked()
                        {
                            page.width_mm = pw;
                            page.height_mm = ph;
                            page_modified = true;
                        }
                    }
                });

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
            let is_portrait = page.height_mm >= page.width_mm;
            if ui
                .selectable_label(is_portrait, "▯")
                .on_hover_text("Portrait Orientation")
                .clicked()
                && !is_portrait
            {
                std::mem::swap(&mut page.width_mm, &mut page.height_mm);
                page_modified = true;
            }
            if ui
                .selectable_label(!is_portrait, "▭")
                .on_hover_text("Landscape Orientation")
                .clicked()
                && is_portrait
            {
                std::mem::swap(&mut page.width_mm, &mut page.height_mm);
                page_modified = true;
            }
        }
        if page_modified {
            tab.mark_modified();
        }
        ui.separator();

        // Units
        ComboBox::from_id_salt("prop_unit")
            .width(58.0)
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

        // Nudge & Duplicate Offsets
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
    }

    ui.separator();
    ui.checkbox(&mut app.treat_all_objects_as_filled, "Treat as Filled")
        .on_hover_text("Select hollow/unfilled shapes by clicking anywhere inside them");

    if ui
        .small_button(RichText::new("Autofit Page").color(tokens.accent))
        .on_hover_text("Resize active page tightly around artwork (+5 mm margin)")
        .clicked()
    {
        autofit_clicked = true;
    }

    // Creation-tool parameters when a parametric shape tool is active
    match app.active_tool {
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
        ToolId::Polygon => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.polygon_sides)
                    .range(3..=64)
                    .prefix("Sides: "),
            );
        }
        ToolId::Star | ToolId::ComplexStar => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.star_points)
                    .range(3..=64)
                    .prefix("Points: "),
            );
            ui.add(
                DragValue::new(&mut app.tool_settings.star_inner_ratio)
                    .range(0.1..=0.9)
                    .speed(0.02)
                    .prefix("Sharpness: "),
            );
        }
        ToolId::Spiral => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.spiral_revolutions)
                    .range(1.0..=20.0)
                    .speed(0.5)
                    .prefix("Revs: "),
            );
            ui.checkbox(&mut app.tool_settings.spiral_logarithmic, "Logarithmic");
        }
        ToolId::GraphPaper => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.graph_cols)
                    .range(1..=30)
                    .prefix("Cols: "),
            );
            ui.add(
                DragValue::new(&mut app.tool_settings.graph_rows)
                    .range(1..=30)
                    .prefix("Rows: "),
            );
        }
        ToolId::Table => {
            ui.separator();
            ui.add(
                DragValue::new(&mut app.tool_settings.table_cols)
                    .range(1..=20)
                    .prefix("Cols: "),
            );
            ui.add(
                DragValue::new(&mut app.tool_settings.table_rows)
                    .range(1..=20)
                    .prefix("Rows: "),
            );
        }
        _ => {}
    }

    if autofit_clicked {
        app.autofit_active_page(5.0);
    }
}

// -----------------------------------------------------------------------------
// Selected Object(s) Property Bar (9-Point Origin Grid + Transform + Contextual)
// -----------------------------------------------------------------------------

fn show_selected_objects_property_bar(
    ui: &mut Ui,
    app: &mut StudioApp,
    tokens: &ThemeTokens,
    sel_ids: &[coree_core::ObjectId],
) {
    let bounds = app
        .active_tab()
        .and_then(|t| t.selection.bounding_box(&t.document));

    if let Some(bb) = bounds {
        // Interactive 3x3 9-Point Object Origin Anchor Grid
        render_nine_point_origin_grid(ui, &mut app.object_origin_anchor, tokens);
        let anchor_pt = anchor_point_in_rect(bb, app.object_origin_anchor);

        let mut x = anchor_pt.x;
        let mut y = anchor_pt.y;
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
            let affine = Affine2D::translate(x - anchor_pt.x, y - anchor_pt.y);
            let _ = app.execute_command(CommandOp::TransformObjects {
                ids: sel_ids.to_vec(),
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
            let affine = Affine2D::translate(anchor_pt.x, anchor_pt.y)
                .compose(&Affine2D::scale(sx_final, sy))
                .compose(&Affine2D::translate(-anchor_pt.x, -anchor_pt.y));
            let _ = app.execute_command(CommandOp::TransformObjects {
                ids: sel_ids.to_vec(),
                affine,
            });
        }

        ui.separator();
        // Rotation° DragValue + Mirror H/V
        let mut rot_step = 0.0_f64;
        if ui
            .add(
                DragValue::new(&mut rot_step)
                    .range(-360.0..=360.0)
                    .speed(1.0)
                    .prefix("↻ ")
                    .suffix("°"),
            )
            .on_hover_text("Drag to rotate selection around active origin anchor")
            .changed()
            && rot_step.abs() > 1e-4
        {
            let affine = Affine2D::rotate_about(rot_step.to_radians(), anchor_pt);
            let _ = app.execute_command(CommandOp::TransformObjects {
                ids: sel_ids.to_vec(),
                affine,
            });
        }
        if ui
            .small_button("90°")
            .on_hover_text("Rotate +90°")
            .clicked()
        {
            let affine = Affine2D::rotate_about(90.0_f64.to_radians(), anchor_pt);
            let _ = app.execute_command(CommandOp::TransformObjects {
                ids: sel_ids.to_vec(),
                affine,
            });
        }
        if ui
            .small_button("⇄")
            .on_hover_text("Mirror Horizontally")
            .clicked()
        {
            let affine = Affine2D::translate(anchor_pt.x, anchor_pt.y)
                .compose(&Affine2D::scale(-1.0, 1.0))
                .compose(&Affine2D::translate(-anchor_pt.x, -anchor_pt.y));
            let _ = app.execute_command(CommandOp::TransformObjects {
                ids: sel_ids.to_vec(),
                affine,
            });
        }
        if ui
            .small_button("⇅")
            .on_hover_text("Mirror Vertically")
            .clicked()
        {
            let affine = Affine2D::translate(anchor_pt.x, anchor_pt.y)
                .compose(&Affine2D::scale(1.0, -1.0))
                .compose(&Affine2D::translate(-anchor_pt.x, -anchor_pt.y));
            let _ = app.execute_command(CommandOp::TransformObjects {
                ids: sel_ids.to_vec(),
                affine,
            });
        }
    }

    // 2+ Objects Selected -> Instant Classic Shaping Strip + Combine + Group
    if sel_ids.len() >= 2 {
        ui.separator();
        for (op, lbl) in [
            (BooleanOp::Weld, "Weld"),
            (BooleanOp::Trim, "Trim"),
            (BooleanOp::Intersect, "Intersect"),
            (BooleanOp::Simplify, "Simplify"),
            (BooleanOp::FrontMinusBack, "Front−Back"),
            (BooleanOp::BackMinusFront, "Back−Front"),
            (BooleanOp::CreateBoundary, "Boundary"),
        ] {
            if ui.small_button(lbl).clicked() {
                app.apply_boolean_op(op);
            }
        }
        ui.separator();
        if ui
            .small_button("Combine")
            .on_hover_text("Combine Curves (Ctrl+L)")
            .clicked()
        {
            app.apply_boolean_op(BooleanOp::Combine);
        }
        if ui
            .small_button("Group")
            .on_hover_text("Group Selection (Ctrl+G)")
            .clicked()
        {
            app.group_selection();
        }
    } else if let Some(&single_id) = sel_ids.first() {
        // Single selection: outline + parametric shape / text controls
        let payload_snapshot = app
            .active_tab()
            .and_then(|t| t.document.objects.get(&single_id))
            .map(|o| (o.payload.clone(), o.outline.clone(), o.lens.clone()));

        if let Some((payload, mut outline, lens_opt)) = payload_snapshot {
            ui.separator();
            let mut width = outline.width_mm;
            if ui
                .add(
                    DragValue::new(&mut width)
                        .range(0.0..=50.0)
                        .speed(0.05)
                        .prefix("Line: ")
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

            match payload {
                NodePayload::Shape(orig_shape) => match orig_shape.clone() {
                    ParametricShape::Rectangle {
                        width: rw,
                        height: rh,
                        mut corner_radii,
                        mut corner_style,
                        mut lock_corners,
                    } => {
                        ui.separator();
                        let max_r = (rw.min(rh) * 0.5).max(0.0);
                        let mut changed = false;
                        if lock_corners {
                            let mut r = corner_radii[0];
                            if ui
                                .add(
                                    DragValue::new(&mut r)
                                        .range(0.0..=max_r)
                                        .speed(0.2)
                                        .prefix("R: ")
                                        .suffix(" mm"),
                                )
                                .changed()
                            {
                                corner_radii = [r; 4];
                                changed = true;
                            }
                        } else {
                            for (i, pfx) in ["TL:", "TR:", "BR:", "BL:"].iter().enumerate() {
                                changed |= ui
                                    .add(
                                        DragValue::new(&mut corner_radii[i])
                                            .range(0.0..=max_r)
                                            .speed(0.2)
                                            .prefix(*pfx),
                                    )
                                    .changed();
                            }
                        }
                        if ui
                            .selectable_label(lock_corners, if lock_corners { "🔒" } else { "🔓" })
                            .on_hover_text("Lock all 4 corner radii together")
                            .clicked()
                        {
                            lock_corners = !lock_corners;
                            if lock_corners {
                                corner_radii = [corner_radii[0]; 4];
                            }
                            changed = true;
                        }
                        for (st, lbl) in [
                            (CornerStyle::Rounded, "Round"),
                            (CornerStyle::Scalloped, "Scallop"),
                            (CornerStyle::Chamfered, "Chamfer"),
                        ] {
                            if ui.selectable_label(corner_style == st, lbl).clicked() {
                                corner_style = st;
                                changed = true;
                            }
                        }
                        if changed {
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::Rectangle {
                                    width: rw,
                                    height: rh,
                                    corner_radii,
                                    corner_style,
                                    lock_corners,
                                },
                            );
                        }
                    }
                    ParametricShape::Ellipse {
                        rx,
                        ry,
                        mut mode,
                        mut start_deg,
                        mut end_deg,
                    } => {
                        ui.separator();
                        let mut changed = false;
                        for (m, lbl) in [
                            (EllipseMode::Full, "Full"),
                            (EllipseMode::Pie, "Pie"),
                            (EllipseMode::Arc, "Arc"),
                        ] {
                            if ui.selectable_label(mode == m, lbl).clicked() {
                                mode = m;
                                if mode != EllipseMode::Full && (end_deg - start_deg).abs() >= 359.5
                                {
                                    start_deg = 0.0;
                                    end_deg = 270.0;
                                }
                                changed = true;
                            }
                        }
                        changed |= ui
                            .add(
                                DragValue::new(&mut start_deg)
                                    .range(0.0..=360.0)
                                    .prefix("Start: ")
                                    .suffix("°"),
                            )
                            .changed();
                        changed |= ui
                            .add(
                                DragValue::new(&mut end_deg)
                                    .range(0.0..=360.0)
                                    .prefix("End: ")
                                    .suffix("°"),
                            )
                            .changed();
                        if changed {
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::Ellipse {
                                    rx,
                                    ry,
                                    mode,
                                    start_deg,
                                    end_deg,
                                },
                            );
                        }
                    }
                    ParametricShape::Polygon {
                        mut sides,
                        radius_x,
                        radius_y,
                    } => {
                        ui.separator();
                        if ui
                            .add(DragValue::new(&mut sides).range(3..=64).prefix("Sides: "))
                            .changed()
                        {
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::Polygon {
                                    sides,
                                    radius_x,
                                    radius_y,
                                },
                            );
                        }
                    }
                    ParametricShape::Star {
                        mut points,
                        outer_radius,
                        mut inner_ratio,
                        complex,
                    } => {
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
                            app.commit_parametric_shape_edit(
                                single_id,
                                orig_shape,
                                ParametricShape::Star {
                                    points,
                                    outer_radius,
                                    inner_ratio,
                                    complex,
                                },
                            );
                        }
                    }
                    _ => {}
                },
                NodePayload::ArtisticText(mut art) => {
                    ui.separator();
                    if let Some(run) = art.runs.first_mut() {
                        if render_text_style_strip(
                            ui,
                            &mut run.style,
                            &mut art.direction,
                            &mut art.align,
                            "prop_sel_art_font",
                        ) {
                            let _ = app.execute_command(CommandOp::EditText {
                                id: single_id,
                                payload: NodePayload::ArtisticText(art),
                            });
                        }
                    }
                }
                NodePayload::ParagraphText(mut para) => {
                    ui.separator();
                    if let Some(run) = para.runs.first_mut() {
                        if render_text_style_strip(
                            ui,
                            &mut run.style,
                            &mut para.direction,
                            &mut para.align,
                            "prop_sel_para_font",
                        ) {
                            let _ = app.execute_command(CommandOp::EditText {
                                id: single_id,
                                payload: NodePayload::ParagraphText(para),
                            });
                        }
                    }
                }
                _ => {}
            }

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

    // Always-present classic quick-action strip for selected object(s)
    ui.separator();
    if ui
        .small_button("To Curves")
        .on_hover_text("Convert to Curves (Ctrl+Q)")
        .clicked()
    {
        app.convert_selection_to_curves();
    }
    if ui
        .small_button("Break Apart")
        .on_hover_text("Break Apart Group / PowerClip / Effects / Subpaths (Ctrl+K)")
        .clicked()
    {
        app.break_apart_selection();
    }
    if ui
        .small_button("Center (P)")
        .on_hover_text("Center to Active Page (P)")
        .clicked()
    {
        app.apply_align(AlignOp::CenterPage);
    }
    if ui
        .small_button("⬆ Front")
        .on_hover_text("Order to Front of Layer (Shift+PgUp)")
        .clicked()
    {
        app.apply_z_order(ZOrderOp::BringToFront);
    }
    if ui
        .small_button("⬇ Back")
        .on_hover_text("Order to Back of Layer (Shift+PgDn)")
        .clicked()
    {
        app.apply_z_order(ZOrderOp::SendToBack);
    }
}

// -----------------------------------------------------------------------------
// Interactive 3x3 Object Origin Anchor Grid Widget
// -----------------------------------------------------------------------------

fn render_nine_point_origin_grid(ui: &mut Ui, anchor: &mut (u8, u8), tokens: &ThemeTokens) {
    let (rect, resp) = ui.allocate_exact_size(vec2(21.0, 21.0), Sense::click());
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(2),
        tokens.bg,
        Stroke::new(1.0, tokens.border),
        StrokeKind::Inside,
    );

    let cell = 7.0;
    for row in 0..3u8 {
        for col in 0..3u8 {
            let dot_center = pos2(
                rect.left() + (col as f32 + 0.5) * cell,
                rect.top() + (row as f32 + 0.5) * cell,
            );
            let active = anchor.0 == col && anchor.1 == row;
            let dot_rect = Rect::from_center_size(
                dot_center,
                if active {
                    vec2(5.0, 5.0)
                } else {
                    vec2(3.2, 3.2)
                },
            );
            painter.rect_filled(
                dot_rect,
                CornerRadius::same(1),
                if active {
                    tokens.accent
                } else {
                    tokens.text_muted
                },
            );
        }
    }

    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let col = (((pos.x - rect.left()) / cell).floor() as i32).clamp(0, 2) as u8;
            let row = (((pos.y - rect.top()) / cell).floor() as i32).clamp(0, 2) as u8;
            *anchor = (col, row);
        }
    }
    resp.on_hover_text(
        "Object Origin (9-Point Reference Grid)\nClick any dot to set X/Y, Scale & Rotate reference anchor",
    );
}

fn anchor_point_in_rect(bb: Rect2D, anchor: (u8, u8)) -> Point2D {
    let fx = f64::from(anchor.0.min(2)) * 0.5;
    let fy = f64::from(anchor.1.min(2)) * 0.5;
    Point2D::new(bb.min_x + bb.width() * fx, bb.min_y + bb.height() * fy)
}

// -----------------------------------------------------------------------------
// Crisp Procedural Vector Toolbar Icon Buttons
// -----------------------------------------------------------------------------

fn std_icon_btn(
    ui: &mut Ui,
    tokens: &ThemeTokens,
    icon: StdIcon,
    tooltip: &str,
    enabled: bool,
    active: bool,
) -> bool {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(23.0, 22.0), sense);
    let painter = ui.painter();

    let bg = if active {
        tokens.accent.gamma_multiply(0.24)
    } else if enabled && resp.hovered() {
        tokens.surface_elevated
    } else {
        Color32::TRANSPARENT
    };
    let border = if active {
        Stroke::new(1.0, tokens.accent)
    } else if enabled && resp.hovered() {
        Stroke::new(1.0, tokens.border_input)
    } else {
        Stroke::NONE
    };
    painter.rect(rect, CornerRadius::same(3), bg, border, StrokeKind::Inside);

    let color = if !enabled {
        tokens.text_muted.gamma_multiply(0.45)
    } else if active {
        tokens.accent
    } else {
        tokens.text
    };
    paint_std_icon(painter, rect.shrink(4.0), icon, color);

    let resp = resp.on_hover_text(tooltip);
    enabled && resp.clicked()
}

fn paint_std_icon(painter: &egui::Painter, r: Rect, icon: StdIcon, color: Color32) {
    let s = Stroke::new(1.35, color);
    let thin = Stroke::new(1.05, color);
    let c = r.center();

    match icon {
        StdIcon::NewDoc => {
            let pts = vec![
                r.left_top() + vec2(2.0, 0.0),
                pos2(r.right() - 4.0, r.top()),
                pos2(r.right() - 1.0, r.top() + 3.5),
                r.right_bottom() - vec2(1.0, 0.0),
                r.left_bottom() + vec2(2.0, 0.0),
            ];
            painter.add(Shape::closed_line(pts, s));
        }
        StdIcon::OpenDoc => {
            let folder = Rect::from_min_max(pos2(r.left(), r.top() + 3.0), r.right_bottom());
            painter.rect_stroke(folder, CornerRadius::same(1), s, StrokeKind::Inside);
            painter.line_segment(
                [
                    pos2(r.left() + 1.0, r.top() + 1.5),
                    pos2(c.x, r.top() + 1.5),
                ],
                s,
            );
        }
        StdIcon::SaveDoc => {
            painter.rect_stroke(r.shrink(0.5), CornerRadius::same(1), s, StrokeKind::Inside);
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(r.left() + 3.0, r.top() + 1.0),
                    pos2(r.right() - 3.0, c.y),
                ),
                CornerRadius::ZERO,
                color.gamma_multiply(0.35),
            );
            painter.rect_stroke(
                Rect::from_min_max(
                    pos2(r.left() + 3.5, c.y + 1.5),
                    pos2(r.right() - 3.5, r.bottom() - 1.0),
                ),
                CornerRadius::ZERO,
                thin,
                StrokeKind::Inside,
            );
        }
        StdIcon::ImportDoc => {
            painter.line_segment([pos2(c.x, r.top()), pos2(c.x, r.bottom() - 3.0)], s);
            painter.line_segment([pos2(c.x - 3.0, c.y), pos2(c.x, r.bottom() - 2.5)], s);
            painter.line_segment([pos2(c.x + 3.0, c.y), pos2(c.x, r.bottom() - 2.5)], s);
            painter.line_segment([r.left_bottom(), r.right_bottom()], s);
        }
        StdIcon::ExportDoc => {
            painter.line_segment([pos2(c.x, r.bottom() - 2.0), pos2(c.x, r.top() + 1.0)], s);
            painter.line_segment([pos2(c.x - 3.0, c.y - 1.0), pos2(c.x, r.top() + 1.0)], s);
            painter.line_segment([pos2(c.x + 3.0, c.y - 1.0), pos2(c.x, r.top() + 1.0)], s);
            painter.line_segment([r.left_bottom(), r.right_bottom()], s);
        }
        StdIcon::Undo => {
            let pts = vec![
                pos2(r.left() + 2.0, c.y - 1.0),
                pos2(r.right() - 2.0, c.y - 1.0),
                pos2(r.right() - 1.0, r.bottom() - 2.0),
            ];
            painter.add(Shape::line(pts, s));
            painter.line_segment(
                [
                    pos2(r.left() + 5.0, r.top() + 2.0),
                    pos2(r.left() + 1.5, c.y - 1.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.left() + 5.0, c.y + 2.5),
                    pos2(r.left() + 1.5, c.y - 1.0),
                ],
                s,
            );
        }
        StdIcon::Redo => {
            let pts = vec![
                pos2(r.right() - 2.0, c.y - 1.0),
                pos2(r.left() + 2.0, c.y - 1.0),
                pos2(r.left() + 1.0, r.bottom() - 2.0),
            ];
            painter.add(Shape::line(pts, s));
            painter.line_segment(
                [
                    pos2(r.right() - 5.0, r.top() + 2.0),
                    pos2(r.right() - 1.5, c.y - 1.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.right() - 5.0, c.y + 2.5),
                    pos2(r.right() - 1.5, c.y - 1.0),
                ],
                s,
            );
        }
        StdIcon::Cut => {
            painter.line_segment(
                [
                    r.left_top() + vec2(2.0, 1.0),
                    r.right_bottom() - vec2(1.0, 2.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    r.right_top() + vec2(-2.0, 1.0),
                    r.left_bottom() + vec2(1.0, -2.0),
                ],
                s,
            );
            painter.circle_stroke(r.left_bottom() + vec2(3.0, -2.5), 2.0, thin);
            painter.circle_stroke(r.right_bottom() + vec2(-3.0, -2.5), 2.0, thin);
        }
        StdIcon::Copy => {
            let b = Rect::from_min_size(r.left_top(), vec2(r.width() * 0.65, r.height() * 0.72));
            let f = Rect::from_min_size(
                r.left_top() + vec2(r.width() * 0.32, r.height() * 0.26),
                vec2(r.width() * 0.65, r.height() * 0.72),
            );
            painter.rect_stroke(b, CornerRadius::same(1), thin, StrokeKind::Inside);
            painter.rect_stroke(f, CornerRadius::same(1), s, StrokeKind::Inside);
        }
        StdIcon::Paste => {
            let board = Rect::from_min_max(
                pos2(r.left() + 1.5, r.top() + 2.5),
                r.right_bottom() - vec2(1.5, 0.0),
            );
            painter.rect_stroke(board, CornerRadius::same(1), s, StrokeKind::Inside);
            painter.rect_filled(
                Rect::from_center_size(pos2(c.x, r.top() + 2.0), vec2(5.5, 2.5)),
                CornerRadius::same(1),
                color,
            );
        }
        StdIcon::PreviewF9 => {
            painter.rect_stroke(r.shrink(0.5), CornerRadius::same(1), s, StrokeKind::Inside);
            painter.line_segment(
                [r.left_top() + vec2(2.5, 2.5), r.left_top() + vec2(5.5, 2.5)],
                thin,
            );
            painter.line_segment(
                [r.left_top() + vec2(2.5, 2.5), r.left_top() + vec2(2.5, 5.5)],
                thin,
            );
            painter.line_segment(
                [
                    r.right_bottom() - vec2(2.5, 2.5),
                    r.right_bottom() - vec2(5.5, 2.5),
                ],
                thin,
            );
            painter.line_segment(
                [
                    r.right_bottom() - vec2(2.5, 2.5),
                    r.right_bottom() - vec2(2.5, 5.5),
                ],
                thin,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::WorkspacePreset;
    use crate::theme::StudioTheme;
    use coree_core::{Color, PostScriptFillPreset};
    use coree_render::GlyphBlockFilter;

    #[test]
    fn classic_studio_chrome_renders_all_property_bars_options_laser_glyphs_and_ps_fills() {
        let ctx = egui::Context::default();
        let mut app = StudioApp::new_headless();
        app.new_sample_document();
        app.fonts_configured = true;

        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0))),
            ..egui::RawInput::default()
        };

        // 1. Exercise all 3 themes + all 6 Workspace Presets + Welcome hub
        app.show_welcome = true;
        for theme in StudioTheme::ALL {
            app.set_theme(theme);
            let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
            assert!(!out.shapes.is_empty());
            out.textures_delta.clear();
        }
        app.show_welcome = false;
        for preset in WorkspacePreset::ALL {
            app.apply_workspace_preset(preset);
        }

        // 2. Exercise Options (Ctrl+J) across all 4 tabs + Export G-Code dialog
        app.show_options_dialog = true;
        app.export_dialog.format_index = crate::ui::dialogs::EXPORT_FMT_GCODE;
        app.export_dialog.open = true;
        for tab_idx in 0..4 {
            app.options_tab = tab_idx;
            let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
            assert!(!out.shapes.is_empty());
            out.textures_delta.clear();
        }
        app.show_options_dialog = false;
        app.export_dialog.open = false;

        // 3. Exercise Laser & Plugins Studio docker with live computed toolpath plan
        app.refresh_laser_plan_preview();
        app.production_state.sub_section = 5;
        app.focus_docker(DockerTab::Production);
        app.active_palette_index = 3; // Laser & Cut Colors
        app.palette_show_names = true;
        let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
        assert!(!out.shapes.is_empty());
        out.textures_delta.clear();

        // 4. Exercise PostScript Procedural Fill + Interactive Glyphs Browser
        app.apply_fill(PostScriptFillPreset::CrossHatch.to_fill(Color::KEY_BLACK, Color::WHITE, 4));
        app.properties_state.active_tab = PropertiesSubTab::Fill;
        app.focus_docker(DockerTab::Properties);
        let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
        assert!(!out.shapes.is_empty());
        out.textures_delta.clear();

        app.properties_state.active_tab = PropertiesSubTab::Text;
        app.glyph_block_filter = GlyphBlockFilter::Arabic;
        let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
        assert!(!out.shapes.is_empty());
        out.textures_delta.clear();

        // 5. Exercise contextual Property Bar across key tool modes & 9-point anchor
        for (col, row) in [(0, 0), (1, 1), (2, 2)] {
            app.object_origin_anchor = (col, row);
        }
        for tool in [
            ToolId::Pick,
            ToolId::ShapeNode,
            ToolId::Smooth,
            ToolId::Roughen,
            ToolId::BitmapBrush,
            ToolId::DropShadow,
            ToolId::Contour,
            ToolId::InteractiveFill,
            ToolId::MeshFill,
            ToolId::ColorEyedropper,
            ToolId::Crop,
            ToolId::Zoom,
            ToolId::ArtisticText,
        ] {
            app.select_tool(tool);
            if tool == ToolId::BitmapBrush {
                app.tool_settings.raster_op = RasterBrushOp::Dodge;
            }
            let mut out = ctx.run_ui(input.clone(), |ui| app.render_ui(ui));
            assert!(!out.shapes.is_empty());
            out.textures_delta.clear();
        }

        // 6. Exercise Command Palette search for Laser & Workspaces
        app.command_palette.open = true;
        app.command_palette.query = "lightburn".to_owned();
        let mut out = ctx.run_ui(input, |ui| app.render_ui(ui));
        assert!(!out.shapes.is_empty());
        out.textures_delta.clear();

        // Verify Laser & Cut Colors palette contains all standard laser colors + CutContour
        let lpal = crate::ui::palette_bar::laser_and_cut_palette();
        assert!(lpal.swatches.iter().any(|s| s.name.contains("Laser Cut")));
        assert!(lpal.swatches.iter().any(|s| s.name == "CutContour"));

        app.shutdown_cleanly();
    }
}
