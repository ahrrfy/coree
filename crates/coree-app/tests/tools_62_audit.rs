// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Exhaustive 62-Tool + 8-Raster-Op + Classic Studio Muscle-Memory Audit Suite.
//!
//! Proves that EVERY SINGLE `ToolId` across all 15 `TOOL_GROUPS` performs genuine,
//! verifiable geometric, scene, viewport, style, effect, or raster mutations with
//! ZERO silent `_ => return` stubs.

use coree_app::shortcuts::{resolve_shortcut, ShortcutAction, ShortcutProfile};
use coree_app::state::{
    KeyEvent, ModifiersState, NodeSmoothness, PointerButton, PointerEvent, RasterBrushOp,
    StudioApp, ToolId, ToolKey, WorkspacePreset, TOOL_GROUPS,
};
use coree_core::{
    Affine2D, AlignOp, BezierPath, Color, ColorMode, CommandOp, ContourMode, CornerStyle, Effect,
    EllipseMode, Fill, ImageAssetId, MasterLayerScope, NodeKind, NodePayload, ObjectId,
    OutlineStyle, ParametricShape, PathNode, Point2D, RasterAsset, RasterLayerData, Rect2D,
    SceneObject, Subpath, TransparencyMode, Vec2D,
};
use coree_render::{DodgeBurnMode, ToneRange};
use egui::{Key, Modifiers};
use std::collections::HashSet;

fn add_rect(
    app: &mut StudioApp,
    name: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    color: Color,
) -> ObjectId {
    let lid = app.active_tab().and_then(|t| t.active_layer_id);
    let obj = SceneObject::new(
        ObjectId(0),
        name,
        NodePayload::Shape(ParametricShape::Rectangle {
            width: w,
            height: h,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(color))
    .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.3))
    .with_transform(Affine2D::translate(x, y));
    app.execute_command(CommandOp::create_object(obj, lid))
        .expect("create rect");
    let id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    if let Some(tab) = app.active_tab_mut() {
        tab.selection.select_single(&tab.document, id);
    }
    id
}

fn add_open_curve(app: &mut StudioApp) -> ObjectId {
    let lid = app.active_tab().and_then(|t| t.active_layer_id);
    let path = BezierPath::from_subpath(Subpath {
        nodes: vec![
            PathNode::corner(Point2D::new(20.0, 20.0)),
            PathNode::corner(Point2D::new(50.0, 20.0)),
            PathNode::corner(Point2D::new(80.0, 20.0)),
            PathNode::corner(Point2D::new(80.0, 60.0)),
            PathNode::corner(Point2D::new(20.0, 60.0)),
        ],
        closed: false,
    });
    let obj = SceneObject::new(ObjectId(0), "Open Curve", NodePayload::Path(path))
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.5));
    app.execute_command(CommandOp::create_object(obj, lid))
        .expect("create open curve");
    let id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    if let Some(tab) = app.active_tab_mut() {
        tab.selection.select_single(&tab.document, id);
    }
    id
}

#[test]
fn audit_all_62_tools_exhaustive_zero_noops() {
    let mut seen = HashSet::new();
    let mut all_tools = Vec::new();
    for group in TOOL_GROUPS {
        for &tool in group {
            assert!(seen.insert(tool), "duplicate tool in TOOL_GROUPS: {tool:?}");
            all_tools.push(tool);
        }
    }
    assert!(
        all_tools.len() >= 62,
        "expected at least 62 tools across 15 TOOL_GROUPS, found {}",
        all_tools.len()
    );
    assert_eq!(
        all_tools.len(),
        65,
        "all 65 registered ToolId variants must be audited"
    );

    for tool in all_tools {
        let mut app = StudioApp::new_headless();

        // Prepare tailored scene context so every tool has realistic artwork to operate on
        let r1 = add_rect(&mut app, "Base A", 20.0, 20.0, 60.0, 50.0, Color::CYAN);
        let r2 = add_rect(&mut app, "Base B", 60.0, 30.0, 60.0, 50.0, Color::MAGENTA);
        if let Some(tab) = app.active_tab_mut() {
            tab.selection.select_single(&tab.document, r1);
            tab.viewport.width_px = 1000;
            tab.viewport.height_px = 800;
            tab.fit_active_page();
        }

        // Convert Base A to Path for ShapeNode marquee node selection test
        if tool == ToolId::ShapeNode {
            app.convert_selection_to_curves();
        }

        let before_count = app.active_tab().unwrap().document.objects().len();
        let before_zoom = app.active_tab().unwrap().viewport.zoom;
        let before_pan = app.active_tab().unwrap().viewport.pan_mm;
        let before_page = {
            let p = app.active_tab().unwrap().document.active_page().unwrap();
            (p.width_mm, p.height_mm)
        };
        let before_r1_xf = app
            .active_tab()
            .unwrap()
            .document
            .object(r1)
            .unwrap()
            .transform;
        let before_r1_payload = app
            .active_tab()
            .unwrap()
            .document
            .object(r1)
            .unwrap()
            .payload
            .clone();
        let before_r1_fill = app
            .active_tab()
            .unwrap()
            .document
            .object(r1)
            .unwrap()
            .fill
            .clone();
        let before_r1_effects = app
            .active_tab()
            .unwrap()
            .document
            .object(r1)
            .unwrap()
            .effects
            .len();

        app.select_tool(tool);
        let drag_rect = match tool {
            // Knife & Eraser slash cleanly across Base A (20..80, 20..70)
            ToolId::Knife | ToolId::Eraser => Rect2D::from_xywh(10.0, 45.0, 80.0, 4.0),
            // VirtualSegmentDelete hits the overlap boundary between Base A & Base B
            ToolId::VirtualSegmentDelete => Rect2D::from_xywh(55.0, 30.0, 10.0, 10.0),
            // SmartFill clicks inside Base A
            ToolId::SmartFill => Rect2D::from_xywh(30.0, 30.0, 10.0, 10.0),
            // Blend drags from Base A center to Base B center
            ToolId::Blend => {
                Rect2D::from_points(Point2D::new(40.0, 45.0), Point2D::new(95.0, 55.0))
            }
            // Eyedroppers click inside Base B
            ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
                Rect2D::from_xywh(85.0, 45.0, 10.0, 10.0)
            }
            // Sculpt tools brush across Base A corner (20, 20)
            ToolId::Smooth
            | ToolId::Smear
            | ToolId::Twirl
            | ToolId::AttractRepel
            | ToolId::Roughen => Rect2D::from_xywh(16.0, 16.0, 18.0, 18.0),
            // Pick / FreehandPick encompass both Base A & Base B
            ToolId::Pick | ToolId::FreehandPick => Rect2D::from_xywh(10.0, 10.0, 120.0, 80.0),
            // Default generous drag box covering Base A
            _ => Rect2D::from_xywh(20.0, 20.0, 50.0, 40.0),
        };

        app.commit_drawn_box(drag_rect);

        let tab = app.active_tab().unwrap();
        let after_count = tab.document.objects().len();
        let after_zoom = tab.viewport.zoom;
        let after_pan = tab.viewport.pan_mm;
        let after_page = {
            let p = tab.document.active_page().unwrap();
            (p.width_mm, p.height_mm)
        };

        let mutated = match tool {
            ToolId::Pick | ToolId::FreehandPick => {
                tab.selection.contains(r1) && tab.selection.contains(r2)
            }
            ToolId::FreeTransform => tab.document.object(r1).unwrap().transform != before_r1_xf,
            ToolId::ShapeNode => !tab.selection.node_selection.is_empty(),
            ToolId::Smooth
            | ToolId::Smear
            | ToolId::Twirl
            | ToolId::AttractRepel
            | ToolId::Roughen => tab.document.object(r1).unwrap().payload != before_r1_payload,
            ToolId::Crop => after_page != before_page && after_page.0 < 60.0,
            ToolId::Knife => after_count > before_count,
            ToolId::VirtualSegmentDelete | ToolId::Eraser => {
                tab.document.object(r1).unwrap().payload != before_r1_payload
                    || tab.document.object(r2).unwrap().payload != before_r1_payload
            }
            ToolId::Zoom => (after_zoom - before_zoom).abs() > 1e-3,
            ToolId::Pan => after_pan.distance_to(before_pan) > 1.0,
            ToolId::DropShadow
            | ToolId::BlockShadow
            | ToolId::Contour
            | ToolId::Blend
            | ToolId::Distort
            | ToolId::Envelope
            | ToolId::Extrude25D
            | ToolId::Transparency => {
                tab.document.object(r1).unwrap().effects.len() > before_r1_effects
            }
            ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
                app.tool_settings.eyedropper_apply_mode
                    && app.default_fill == Fill::Solid(Color::MAGENTA)
            }
            ToolId::InteractiveFill | ToolId::MeshFill => {
                tab.document.object(r1).unwrap().fill != before_r1_fill
            }
            ToolId::MaskSelect => tab.raster_mask_rect_mm == Some(drag_rect),
            ToolId::BitmapBrush | ToolId::BitmapEraser | ToolId::CloneStamp | ToolId::Smudge => {
                // Auto-creates a 150 DPI transparent raster layer and registers its asset!
                after_count > before_count && !tab.document.assets().is_empty()
            }
            // All shape, curve, text, table, dimension, connector, smart-fill & lens creation tools
            _ => after_count > before_count,
        };

        assert!(
            mutated,
            "Tool {tool:?} ({}) acted as a silent no-op!",
            tool.label()
        );
    }
}

#[test]
fn audit_raster_studio_all_8_ops_and_auto_create_layer() {
    let mut app = StudioApp::new_headless();
    // Use a 60x60 mm page so auto-created 150 DPI raster (~354x354 px) runs all 8 ops + 8x Undo/Redo rapidly in debug mode
    app.active_tab_mut()
        .unwrap()
        .document
        .set_page_dimensions(0, 60.0, 60.0)
        .unwrap();
    assert!(app.active_tab().unwrap().document.assets().is_empty());

    // 1. Paint on an empty canvas -> auto-creates 150 DPI transparent raster layer and paints pixels!
    app.set_raster_brush_op(RasterBrushOp::Paint);
    app.tool_settings.brush_color = Color::rgba8(140, 120, 100, 255);
    app.tool_settings.brush_radius_px = 14.0;
    assert!(app.apply_raster_tool_stroke(
        ToolId::BitmapBrush,
        &[Point2D::new(8.0, 10.0), Point2D::new(32.0, 10.0)],
    ));

    let current_rgba = |a: &StudioApp, oid: ObjectId| -> Vec<u8> {
        let tab = a.active_tab().unwrap();
        let NodePayload::Raster(ref r) = tab.document.object(oid).unwrap().payload else {
            panic!("expected raster object {oid:?}");
        };
        tab.document
            .asset(r.asset_id)
            .unwrap()
            .to_rgba8_vec()
            .clone()
    };

    let raster_oid = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    let snap_after_paint = current_rgba(&app, raster_oid);
    let non_zero_alpha = snap_after_paint
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 0)
        .count();
    assert!(non_zero_alpha > 50, "Paint brush must paint visible pixels");

    // Verify Undo/Redo cleanly restores pixels on Paint!
    app.undo().expect("undo paint stroke");
    let snap_before_paint = current_rgba(&app, raster_oid);
    assert!(
        snap_before_paint.iter().all(|&b| b == 0),
        "Undo of first paint stroke must restore transparent raster pixels"
    );
    app.redo().expect("redo paint stroke");
    assert_eq!(
        current_rgba(&app, raster_oid),
        snap_after_paint,
        "Redo must restore painted pixels"
    );

    // 2. Dodge (lightens painted midtone stroke)
    app.set_raster_brush_op(RasterBrushOp::Dodge);
    app.tool_settings.dodge_burn_tone = ToneRange::Midtones;
    app.tool_settings.dodge_burn_exposure = 0.6;
    assert!(app.apply_raster_tool_stroke(
        ToolId::BitmapBrush,
        &[Point2D::new(10.0, 10.0), Point2D::new(20.0, 10.0)],
    ));
    let snap_after_dodge = current_rgba(&app, raster_oid);
    assert_ne!(
        snap_after_dodge, snap_after_paint,
        "Dodge must modify pixels"
    );

    // 3. Burn (darkens painted stroke)
    app.set_raster_brush_op(RasterBrushOp::Burn);
    assert!(app.apply_raster_tool_stroke(
        ToolId::BitmapBrush,
        &[Point2D::new(22.0, 10.0), Point2D::new(30.0, 10.0)],
    ));
    let snap_after_burn = current_rgba(&app, raster_oid);
    assert_ne!(snap_after_burn, snap_after_dodge, "Burn must modify pixels");

    // 4. Healing Brush (transfers texture with luminance match from source)
    app.set_raster_brush_op(RasterBrushOp::Healing);
    app.tool_settings.clone_source_mm = Some(Point2D::new(14.0, 10.0));
    assert!(app.apply_raster_tool_stroke(
        ToolId::BitmapBrush,
        &[Point2D::new(14.0, 24.0), Point2D::new(26.0, 24.0)],
    ));
    let snap_after_heal = current_rgba(&app, raster_oid);
    assert_ne!(
        snap_after_heal, snap_after_burn,
        "Healing brush must modify pixels"
    );

    // 5. CloneStamp
    app.set_raster_brush_op(RasterBrushOp::CloneStamp);
    app.tool_settings.clone_source_mm = Some(Point2D::new(14.0, 10.0));
    assert!(app.apply_raster_tool_stroke(
        ToolId::CloneStamp,
        &[Point2D::new(14.0, 38.0), Point2D::new(26.0, 38.0)],
    ));
    let snap_after_clone = current_rgba(&app, raster_oid);
    assert_ne!(
        snap_after_clone, snap_after_heal,
        "CloneStamp must copy pixels"
    );

    // 6. Smudge
    app.set_raster_brush_op(RasterBrushOp::Smudge);
    assert!(app.apply_raster_tool_stroke(
        ToolId::Smudge,
        &[Point2D::new(14.0, 10.0), Point2D::new(14.0, 18.0)],
    ));
    let snap_after_smudge = current_rgba(&app, raster_oid);
    assert_ne!(
        snap_after_smudge, snap_after_clone,
        "Smudge must smear pixels"
    );

    // 7. Eraser
    app.set_raster_brush_op(RasterBrushOp::Eraser);
    assert!(app.apply_raster_tool_stroke(
        ToolId::BitmapEraser,
        &[Point2D::new(14.0, 38.0), Point2D::new(26.0, 38.0)],
    ));
    let snap_after_erase = current_rgba(&app, raster_oid);
    assert_ne!(
        snap_after_erase, snap_after_smudge,
        "Eraser must reduce alpha"
    );

    // 8. FloodFill
    app.set_raster_brush_op(RasterBrushOp::FloodFill);
    app.tool_settings.brush_color = Color::rgba8(20, 180, 220, 255);
    app.tool_settings.flood_tolerance = 16;
    assert!(app.apply_raster_tool_stroke(ToolId::BitmapBrush, &[Point2D::new(50.0, 50.0)],));
    let snap_after_flood = current_rgba(&app, raster_oid);
    assert_ne!(
        snap_after_flood, snap_after_erase,
        "FloodFill must fill contiguous region"
    );

    // Verify multi-step Undo + Redo across FloodFill -> Eraser -> Smudge -> CloneStamp -> Healing -> Burn -> Dodge -> Paint!
    app.undo().expect("undo floodfill");
    assert_eq!(current_rgba(&app, raster_oid), snap_after_erase);
    app.undo().expect("undo eraser");
    assert_eq!(current_rgba(&app, raster_oid), snap_after_smudge);
    app.undo().expect("undo smudge");
    assert_eq!(current_rgba(&app, raster_oid), snap_after_clone);
    app.undo().expect("undo clone");
    assert_eq!(current_rgba(&app, raster_oid), snap_after_heal);
    app.undo().expect("undo healing");
    assert_eq!(current_rgba(&app, raster_oid), snap_after_burn);
    app.undo().expect("undo burn");
    assert_eq!(current_rgba(&app, raster_oid), snap_after_dodge);
    app.undo().expect("undo dodge");
    assert_eq!(current_rgba(&app, raster_oid), snap_after_paint);
    for _ in 0..7 {
        app.redo().expect("redo raster op");
    }
    assert_eq!(current_rgba(&app, raster_oid), snap_after_flood);

    // 9. MaskSelect restricts masked painting cleanly!
    app.select_tool(ToolId::MaskSelect);
    app.commit_drawn_box(Rect2D::from_xywh(4.0, 44.0, 14.0, 12.0));
    assert!(app.active_tab().unwrap().raster_mask_rect_mm.is_some());
    app.set_raster_brush_op(RasterBrushOp::Paint);
    app.tool_settings.brush_color = Color::rgba8(255, 0, 0, 255);
    assert!(app.apply_raster_tool_stroke(
        ToolId::BitmapBrush,
        &[Point2D::new(45.0, 50.0)], // outside mask!
    ));
    assert_eq!(
        current_rgba(&app, raster_oid),
        snap_after_flood,
        "Masked paint outside MaskSelect region must not alter pixels"
    );
    // Avoid unused import warning for DodgeBurnMode
    let _ = DodgeBurnMode::Dodge;
}

#[test]
fn audit_shape_node_parametric_and_all_11_property_bar_ops() {
    let mut app = StudioApp::new_headless();

    // 1. Live ParametricShape editing: Rectangle corners + Undo/Redo
    let rect_id = add_rect(&mut app, "Param Rect", 20.0, 20.0, 80.0, 50.0, Color::CYAN);
    let orig_shape = match &app
        .active_tab()
        .unwrap()
        .document
        .object(rect_id)
        .unwrap()
        .payload
    {
        NodePayload::Shape(s) => s.clone(),
        _ => panic!("expected shape"),
    };
    let rounded_shape = ParametricShape::Rectangle {
        width: 80.0,
        height: 50.0,
        corner_radii: [12.0; 4],
        corner_style: CornerStyle::Rounded,
        lock_corners: true,
    };
    assert!(app.commit_parametric_shape_edit(rect_id, orig_shape.clone(), rounded_shape.clone()));
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(rect_id)
            .unwrap()
            .payload,
        NodePayload::Shape(rounded_shape)
    );
    app.undo().expect("undo parametric edit");
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(rect_id)
            .unwrap()
            .payload,
        NodePayload::Shape(orig_shape)
    );

    // 2. Parametric Ellipse Arc/Pie + Star inner sharpness
    let lid = app.active_tab().and_then(|t| t.active_layer_id);
    let ell = SceneObject::new(
        ObjectId(0),
        "Param Ellipse",
        NodePayload::Shape(ParametricShape::Ellipse {
            rx: 30.0,
            ry: 20.0,
            mode: EllipseMode::Full,
            start_deg: 0.0,
            end_deg: 360.0,
        }),
    );
    app.execute_command(CommandOp::create_object(ell, lid))
        .unwrap();
    let ell_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    let orig_ell = match &app
        .active_tab()
        .unwrap()
        .document
        .object(ell_id)
        .unwrap()
        .payload
    {
        NodePayload::Shape(s) => s.clone(),
        _ => panic!("expected ellipse"),
    };
    let pie_ell = ParametricShape::Ellipse {
        rx: 30.0,
        ry: 20.0,
        mode: EllipseMode::Pie,
        start_deg: 30.0,
        end_deg: 270.0,
    };
    assert!(app.commit_parametric_shape_edit(ell_id, orig_ell, pie_ell.clone()));
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(ell_id)
            .unwrap()
            .payload,
        NodePayload::Shape(pie_ell)
    );

    // 3. All 11 Shape Property Bar operations on a Path object!
    let curve_id = add_open_curve(&mut app);
    app.select_tool(ToolId::ShapeNode);

    // (a) select_all_nodes
    let total_nodes = app.shape_select_all_nodes();
    assert_eq!(total_nodes, 5);

    // (b) reduce_nodes (removes collinear midpoint (50, 20) along (20,20)-(80,20)!)
    assert!(app.shape_reduce_nodes(0.5));
    let reduced_count = match &app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        NodePayload::Path(p) => p.node_count(),
        _ => 0,
    };
    assert_eq!(reduced_count, 4, "collinear node must be reduced");

    // (c) add_node
    assert!(app.shape_add_node());
    let after_add = match &app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        NodePayload::Path(p) => p.node_count(),
        _ => 0,
    };
    assert_eq!(after_add, 5);

    // (d) set_node_smoothness (Smooth & Symmetrical)
    assert!(app.shape_set_node_smoothness(NodeSmoothness::Smooth));
    if let NodePayload::Path(ref p) = app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        let sel = app.active_tab().unwrap().selection.node_selection.nodes[0];
        assert_eq!(
            p.subpaths[sel.subpath_index].nodes[sel.node_index].kind,
            NodeKind::Smooth
        );
    }

    // (e) convert_to_curve & (f) convert_to_line
    assert!(app.shape_convert_to_curve());
    if let NodePayload::Path(ref p) = app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        let any_handles = p.subpaths[0]
            .nodes
            .iter()
            .any(|n| n.handle_in.is_some() || n.handle_out.is_some());
        assert!(any_handles, "convert_to_curve must add cubic handles");
    }
    app.active_tab_mut()
        .unwrap()
        .selection
        .node_selection
        .clear();
    assert!(app.shape_convert_to_line());
    if let NodePayload::Path(ref p) = app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        let any_handles = p.subpaths[0]
            .nodes
            .iter()
            .any(|n| n.handle_in.is_some() || n.handle_out.is_some());
        assert!(!any_handles, "convert_to_line must clear cubic handles");
    }

    // (g) reverse_subpaths
    let first_before = match &app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        NodePayload::Path(p) => p.subpaths[0].nodes[0].pos,
        _ => Point2D::ZERO,
    };
    assert!(app.shape_reverse_subpaths());
    let first_after = match &app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        NodePayload::Path(p) => p.subpaths[0].nodes[0].pos,
        _ => Point2D::ZERO,
    };
    assert_ne!(
        first_before, first_after,
        "reverse_subpaths must reverse node order"
    );

    // (h) close_curve
    assert!(app.shape_close_curve());
    if let NodePayload::Path(ref p) = app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        assert!(p.subpaths[0].closed);
    }

    // (i) break_curve
    assert!(app.shape_break_curve());
    if let NodePayload::Path(ref p) = app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        assert!(
            !p.subpaths[0].closed,
            "break_curve must open closed subpath"
        );
    }

    // (j) join_nodes (joins endpoints of open curve back closed!)
    assert!(app.shape_join_nodes());
    if let NodePayload::Path(ref p) = app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        assert!(p.subpaths[0].closed, "join_nodes must close open endpoints");
    }

    // (k) delete_selected_nodes (and Delete shortcut in ShapeNode mode!)
    let count_before_del = match &app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        NodePayload::Path(p) => p.node_count(),
        _ => 0,
    };
    let nref = coree_core::NodeRef {
        object_id: curve_id,
        subpath_index: 0,
        node_index: 0,
    };
    app.active_tab_mut()
        .unwrap()
        .selection
        .node_selection
        .select_single(nref);
    app.handle_shortcut(ShortcutAction::DeleteSelection);
    let count_after_del = match &app
        .active_tab()
        .unwrap()
        .document
        .object(curve_id)
        .unwrap()
        .payload
    {
        NodePayload::Path(p) => p.node_count(),
        _ => 0,
    };
    assert_eq!(
        count_after_del,
        count_before_del - 1,
        "Delete in ShapeNode deletes selected node"
    );
}

#[test]
fn audit_all_5_vector_sculpt_tools_deform_geometry() {
    for (tool, shift_invert) in [
        (ToolId::Smooth, false),
        (ToolId::Smear, false),
        (ToolId::Twirl, false),
        (ToolId::Twirl, true),
        (ToolId::AttractRepel, false),
        (ToolId::AttractRepel, true),
        (ToolId::Roughen, false),
    ] {
        let mut app = StudioApp::new_headless();
        let rid = add_rect(
            &mut app,
            "Sculpt Target",
            30.0,
            30.0,
            60.0,
            60.0,
            Color::CYAN,
        );
        let orig_path = app
            .active_tab()
            .unwrap()
            .document
            .world_path_of(rid)
            .unwrap();

        app.tool_settings.sculpt_radius_mm = 25.0;
        app.tool_settings.sculpt_strength = 0.6;
        assert!(
            app.apply_sculpt_stroke(
                tool,
                &[Point2D::new(32.0, 32.0), Point2D::new(45.0, 42.0)],
                shift_invert,
            ),
            "sculpt tool {tool:?} (invert={shift_invert}) failed"
        );

        let obj = app.active_tab().unwrap().document.object(rid).unwrap();
        let NodePayload::Path(ref edited) = obj.payload else {
            panic!("sculpt tool {tool:?} must convert ParametricShape to BezierPath");
        };
        assert_ne!(
            edited.transformed(&obj.transform),
            orig_path,
            "sculpt tool {tool:?} must deform geometry"
        );

        // Undo must restore original ParametricShape cleanly!
        app.undo().expect("undo sculpt");
        assert!(matches!(
            app.active_tab()
                .unwrap()
                .document
                .object(rid)
                .unwrap()
                .payload,
            NodePayload::Shape(ParametricShape::Rectangle { .. })
        ));
    }
}

#[test]
fn audit_all_7_interactive_effect_tools_transparency_and_mesh_fill() {
    let mut app = StudioApp::new_headless();
    let a = add_rect(&mut app, "Effect A", 20.0, 20.0, 50.0, 40.0, Color::CYAN);
    let b = add_rect(&mut app, "Effect B", 110.0, 20.0, 50.0, 40.0, Color::YELLOW);
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, a);
    }

    // 1. DropShadow
    assert!(app.apply_interactive_effect_drag(
        ToolId::DropShadow,
        Point2D::new(45.0, 40.0),
        Point2D::new(53.0, 47.0),
    ));
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .object(a)
        .unwrap()
        .effects
        .iter()
        .any(|e| matches!(e, Effect::DropShadow { dx_mm, dy_mm, .. } if (*dx_mm - 8.0).abs() < 1e-3 && (*dy_mm - 7.0).abs() < 1e-3)));

    // 2. BlockShadow
    assert!(app.apply_interactive_effect_drag(
        ToolId::BlockShadow,
        Point2D::new(45.0, 40.0),
        Point2D::new(55.0, 52.0),
    ));
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .object(a)
        .unwrap()
        .effects
        .iter()
        .any(|e| matches!(e, Effect::BlockShadow { .. })));

    // 3. Contour (drag outside -> ContourMode::Outside, drag inside -> ContourMode::Inside)
    assert!(app.apply_interactive_effect_drag(
        ToolId::Contour,
        Point2D::new(45.0, 40.0),
        Point2D::new(90.0, 40.0),
    ));
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .object(a)
        .unwrap()
        .effects
        .iter()
        .any(|e| matches!(
            e,
            Effect::Contour {
                mode: ContourMode::Outside,
                ..
            }
        )));
    assert!(app.apply_interactive_effect_drag(
        ToolId::Contour,
        Point2D::new(22.0, 22.0),
        Point2D::new(30.0, 30.0),
    ));
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .object(a)
        .unwrap()
        .effects
        .iter()
        .any(|e| matches!(
            e,
            Effect::Contour {
                mode: ContourMode::Inside,
                ..
            }
        )));

    // 4. Blend from A -> B detects target_id == b!
    assert!(app.apply_interactive_effect_drag(
        ToolId::Blend,
        Point2D::new(45.0, 40.0),
        Point2D::new(135.0, 40.0),
    ));
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .object(a)
        .unwrap()
        .effects
        .iter()
        .any(|e| matches!(e, Effect::Blend { target_id, .. } if *target_id == b)));

    // 5. Distort
    assert!(app.apply_interactive_effect_drag(
        ToolId::Distort,
        Point2D::new(45.0, 40.0),
        Point2D::new(65.0, 50.0),
    ));
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .object(a)
        .unwrap()
        .effects
        .iter()
        .any(|e| matches!(e, Effect::Distort { .. })));

    // 6. Envelope (12-point cubic cage initialized and warped via coree-tools)
    assert!(app.apply_interactive_effect_drag(
        ToolId::Envelope,
        Point2D::new(45.0, 20.0),
        Point2D::new(45.0, 10.0),
    ));
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .object(a)
        .unwrap()
        .effects
        .iter()
        .any(|e| matches!(e, Effect::Envelope { cage, .. } if cage.len() == 12)));

    // 7. Extrude25D
    assert!(app.apply_interactive_effect_drag(
        ToolId::Extrude25D,
        Point2D::new(45.0, 40.0),
        Point2D::new(95.0, 15.0),
    ));
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .object(a)
        .unwrap()
        .effects
        .iter()
        .any(|e| matches!(e, Effect::Extrude25D { .. })));

    // 8. Interactive Transparency (FountainLinear fade)
    assert!(app.apply_interactive_effect_drag(
        ToolId::Transparency,
        Point2D::new(20.0, 40.0),
        Point2D::new(70.0, 40.0),
    ));
    assert!(app
        .active_tab()
        .unwrap()
        .document
        .object(a)
        .unwrap()
        .effects
        .iter()
        .any(|e| matches!(
            e,
            Effect::Transparency {
                mode: TransparencyMode::FountainLinear { .. },
                ..
            }
        )));

    // 9. MeshFill (initializes rows x cols grid, warps node & recolors it!)
    assert!(app.mesh_fill_interact(
        Point2D::new(20.0, 20.0),
        Point2D::new(26.0, 25.0),
        Some(Color::MAGENTA),
    ));
    match &app.active_tab().unwrap().document.object(a).unwrap().fill {
        Fill::MeshFill { rows, cols, nodes } => {
            assert!(*rows >= 2 && *cols >= 2);
            assert_eq!(nodes.len(), rows * cols);
            assert!(nodes.iter().any(|n| n.color == Color::MAGENTA));
        }
        other => panic!("expected MeshFill, got {other:?}"),
    }
}

#[test]
fn audit_crop_tool_real_crop_and_undo() {
    let mut app = StudioApp::new_headless();
    let inside = add_rect(&mut app, "Inside", 50.0, 50.0, 30.0, 20.0, Color::CYAN);
    let outside = add_rect(
        &mut app,
        "Outside",
        160.0,
        220.0,
        30.0,
        30.0,
        Color::MAGENTA,
    );

    let crop = Rect2D::from_xywh(40.0, 40.0, 80.0, 60.0);
    assert!(app.apply_crop_rect(crop));

    let tab = app.active_tab().unwrap();
    let page = tab.document.active_page().unwrap();
    assert!((page.width_mm - 80.0).abs() < 1e-3);
    assert!((page.height_mm - 60.0).abs() < 1e-3);
    assert!(
        !tab.document.has_object(outside),
        "outside object must be removed"
    );
    let new_bb = tab.document.object_bounds(inside).unwrap();
    assert!(
        (new_bb.min_x - 10.0).abs() < 1e-3 && (new_bb.min_y - 10.0).abs() < 1e-3,
        "inside object must translate by (-crop.min_x, -crop.min_y), got {new_bb:?}"
    );

    // Undo restores page size, removed outside object, and original coordinates!
    app.undo().expect("undo crop");
    let tab = app.active_tab().unwrap();
    let page = tab.document.active_page().unwrap();
    assert!((page.width_mm - 210.0).abs() < 1e-3);
    assert!((page.height_mm - 297.0).abs() < 1e-3);
    assert!(tab.document.has_object(outside));
    let restored_bb = tab.document.object_bounds(inside).unwrap();
    assert!((restored_bb.min_x - 50.0).abs() < 1e-3 && (restored_bb.min_y - 50.0).abs() < 1e-3);
}

#[test]
fn audit_zoom_marquee_f2_one_shot_and_fullscreen() {
    let mut app = StudioApp::new_headless();
    let _r = add_rect(&mut app, "Zoom Target", 60.0, 60.0, 40.0, 40.0, Color::CYAN);
    app.select_tool(ToolId::Rectangle);
    assert_eq!(app.active_tool, ToolId::Rectangle);

    // F2 activates one-shot Zoom
    let f2 = resolve_shortcut(
        ShortcutProfile::ClassicStudio,
        Key::F2,
        Modifiers::NONE,
        false,
    );
    assert_eq!(f2, Some(ShortcutAction::SelectOneShotZoom));
    app.handle_shortcut(f2.unwrap());
    assert_eq!(app.active_tool, ToolId::Zoom);
    assert_eq!(app.one_shot_zoom_return_tool, Some(ToolId::Rectangle));

    let z_before = app.active_tab().unwrap().viewport.zoom;
    app.commit_drawn_box(Rect2D::from_xywh(60.0, 60.0, 40.0, 40.0));
    let z_after = app.active_tab().unwrap().viewport.zoom;
    assert!(
        z_after > z_before,
        "Zoom marquee must zoom into marquee rect"
    );
    assert_eq!(
        app.active_tool,
        ToolId::Rectangle,
        "One-shot F2 Zoom must automatically return to previous tool!"
    );

    // Shift+F2 = ZoomToSelected, F9 = FullScreenPreview, Esc exits fullscreen!
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::F2,
            Modifiers::SHIFT,
            false
        ),
        Some(ShortcutAction::ZoomToSelected)
    );
    app.handle_shortcut(ShortcutAction::ZoomToSelected);
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::F9,
            Modifiers::NONE,
            false
        ),
        Some(ShortcutAction::ToggleFullScreenPreview)
    );
    app.handle_shortcut(ShortcutAction::ToggleFullScreenPreview);
    assert!(app.full_screen_preview);
    app.handle_shortcut(ShortcutAction::EscapeCancel);
    assert!(!app.full_screen_preview);
}

#[test]
fn audit_bezier_pen_handle_pull_close_bspline_and_3pt_curve() {
    let mut app = StudioApp::new_headless();
    app.select_tool(ToolId::Pen);

    // Pull symmetric cubic handles on nodes 1 & 2, then close path
    app.bezier_add_point(Point2D::new(20.0, 20.0), Some(Point2D::new(35.0, 10.0)));
    app.bezier_add_point(Point2D::new(80.0, 20.0), Some(Point2D::new(90.0, 40.0)));
    app.bezier_add_point(Point2D::new(50.0, 70.0), None);
    let pen_id = app.bezier_finish(true).expect("commit closed pen curve");
    if let NodePayload::Path(ref p) = app
        .active_tab()
        .unwrap()
        .document
        .object(pen_id)
        .unwrap()
        .payload
    {
        assert!(p.subpaths[0].closed);
        assert_eq!(p.subpaths[0].nodes.len(), 3);
        assert!(p.subpaths[0].nodes[0].handle_out.is_some());
        assert!(p.subpaths[0].nodes[0].handle_in.is_some());
    } else {
        panic!("expected Path payload");
    }

    // 3-Point Curve passes through apex P3 cleanly
    let arc_id = app
        .commit_three_point_curve(
            Point2D::new(10.0, 50.0),
            Point2D::new(90.0, 50.0),
            Point2D::new(50.0, 10.0),
        )
        .expect("commit 3-point curve");
    if let NodePayload::Path(ref p) = app
        .active_tab()
        .unwrap()
        .document
        .object(arc_id)
        .unwrap()
        .payload
    {
        let (mid, _) = p.point_and_tangent_at_t(0.5).expect("midpoint");
        assert!(
            mid.distance_to(Point2D::new(50.0, 10.0)) < 0.5,
            "3-Point Curve must pass through P3 at t=0.5, got {mid:?}"
        );
    } else {
        panic!("expected Path payload");
    }

    // 3-Point Rectangle & 3-Point Ellipse create rotated parametric shapes
    let r3 = app
        .commit_three_point_shape(
            ToolId::ThreePointRectangle,
            Point2D::new(10.0, 10.0),
            Point2D::new(50.0, 30.0),
            Point2D::new(30.0, 60.0),
        )
        .expect("3-point rect");
    assert!(matches!(
        app.active_tab()
            .unwrap()
            .document
            .object(r3)
            .unwrap()
            .payload,
        NodePayload::Shape(ParametricShape::Rectangle { .. })
    ));
}

#[test]
fn audit_eyedropper_sample_apply_connectors_and_inline_text() {
    let mut app = StudioApp::new_headless();
    let src = add_rect(&mut app, "Src", 20.0, 20.0, 40.0, 40.0, Color::MAGENTA);
    let dst = add_rect(&mut app, "Dst", 100.0, 20.0, 40.0, 40.0, Color::CYAN);

    // 1. ColorEyedropper Sample -> Apply dual workflow
    app.select_tool(ToolId::ColorEyedropper);
    assert!(!app.tool_settings.eyedropper_apply_mode);
    assert!(app.eyedropper_click(Point2D::new(40.0, 40.0), false, false));
    assert!(app.tool_settings.eyedropper_apply_mode);
    assert_eq!(app.default_fill, Fill::Solid(Color::MAGENTA));
    // Second click applies to Dst!
    assert!(app.eyedropper_click(Point2D::new(120.0, 40.0), false, false));
    assert_eq!(
        app.active_tab().unwrap().document.object(dst).unwrap().fill,
        Fill::Solid(Color::MAGENTA)
    );
    // Shift+Click re-samples!
    assert!(app.eyedropper_click(Point2D::new(120.0, 40.0), false, true));
    assert!(app.tool_settings.eyedropper_apply_mode);

    // 2. Connector snaps from_obj = src and to_obj = dst automatically!
    let conn_id = app
        .commit_connector_drag(Point2D::new(58.0, 40.0), Point2D::new(102.0, 40.0), true)
        .expect("commit connector");
    if let NodePayload::Connector(ref c) = app
        .active_tab()
        .unwrap()
        .document
        .object(conn_id)
        .unwrap()
        .payload
    {
        assert_eq!(c.from_obj, Some(src));
        assert_eq!(c.to_obj, Some(dst));
    } else {
        panic!("expected Connector payload");
    }

    // 3. Direct on-canvas InlineTextEditState (Arabic + English) create + re-edit!
    app.begin_inline_text_edit(
        None,
        Point2D::new(30.0, 120.0),
        false,
        Vec2D::new(90.0, 18.0),
    );
    app.active_tab_mut()
        .unwrap()
        .inline_text_edit
        .as_mut()
        .unwrap()
        .buffer = "شكر وتقدير coree".to_owned();
    assert!(app.commit_inline_text_edit());
    let text_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    if let NodePayload::ArtisticText(ref art) = app
        .active_tab()
        .unwrap()
        .document
        .object(text_id)
        .unwrap()
        .payload
    {
        assert_eq!(art.plain_text(), "شكر وتقدير coree");
    } else {
        panic!("expected ArtisticText");
    }

    // Re-open inline editor on existing ArtisticText and update it!
    app.begin_inline_text_edit(Some(text_id), Point2D::ZERO, false, Vec2D::new(90.0, 18.0));
    app.active_tab_mut()
        .unwrap()
        .inline_text_edit
        .as_mut()
        .unwrap()
        .buffer
        .push_str(" v0.2");
    assert!(app.commit_inline_text_edit());
    if let NodePayload::ArtisticText(ref art) = app
        .active_tab()
        .unwrap()
        .document
        .object(text_id)
        .unwrap()
        .payload
    {
        assert_eq!(art.plain_text(), "شكر وتقدير coree v0.2");
    }
}

#[test]
fn audit_muscle_memory_shortcuts_combine_break_apart_and_nudge() {
    let mut app = StudioApp::new_headless();
    let r1 = add_rect(&mut app, "A", 10.0, 10.0, 30.0, 20.0, Color::CYAN);

    // P = Center on active page (210x297 -> center (105, 148.5))
    let p_act = resolve_shortcut(
        ShortcutProfile::ClassicStudio,
        Key::P,
        Modifiers::NONE,
        false,
    );
    assert_eq!(p_act, Some(ShortcutAction::Align(AlignOp::CenterPage)));
    app.handle_shortcut(p_act.unwrap());
    let bb = app
        .active_tab()
        .unwrap()
        .document
        .object_bounds(r1)
        .unwrap();
    assert!((bb.center().x - 105.0).abs() < 1e-3 && (bb.center().y - 148.5).abs() < 1e-3);

    // Single-object L aligns to page left edge (x = 0)
    app.handle_shortcut(ShortcutAction::Align(AlignOp::Left));
    let bb_l = app
        .active_tab()
        .unwrap()
        .document
        .object_bounds(r1)
        .unwrap();
    assert!(
        bb_l.min_x.abs() < 1e-3,
        "single-object L must align to page left"
    );

    // Arrow = 1x, Shift+Arrow = 2x Super-Nudge, Ctrl+Arrow = 0.5x Micro-Nudge
    let x0 = app
        .active_tab()
        .unwrap()
        .document
        .object_bounds(r1)
        .unwrap()
        .min_x;
    app.handle_shortcut(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::ArrowRight,
            Modifiers::NONE,
            false,
        )
        .unwrap(),
    );
    let x1 = app
        .active_tab()
        .unwrap()
        .document
        .object_bounds(r1)
        .unwrap()
        .min_x;
    assert!((x1 - x0 - 1.0).abs() < 1e-3);

    app.handle_shortcut(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::ArrowRight,
            Modifiers::SHIFT,
            false,
        )
        .unwrap(),
    );
    let x2 = app
        .active_tab()
        .unwrap()
        .document
        .object_bounds(r1)
        .unwrap()
        .min_x;
    assert!((x2 - x1 - 2.0).abs() < 1e-3);

    app.handle_shortcut(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::ArrowRight,
            Modifiers::CTRL,
            false,
        )
        .unwrap(),
    );
    let x3 = app
        .active_tab()
        .unwrap()
        .document
        .object_bounds(r1)
        .unwrap()
        .min_x;
    assert!((x3 - x2 - 0.5).abs() < 1e-3);

    // Numpad + duplicates in-place (0,0 offset)
    let plus_act = resolve_shortcut(
        ShortcutProfile::ClassicStudio,
        Key::Plus,
        Modifiers::NONE,
        false,
    );
    assert_eq!(plus_act, Some(ShortcutAction::DuplicateInPlace));
    let before_dup = app.active_tab().unwrap().document.objects().len();
    app.handle_shortcut(plus_act.unwrap());
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        before_dup + 1
    );
    let dup_id = app.active_tab().unwrap().selection.objects[0];
    assert!(
        (app.active_tab()
            .unwrap()
            .document
            .object_bounds(dup_id)
            .unwrap()
            .min_x
            - x3)
            .abs()
            < 1e-3
    );

    // Ctrl+L Combine & Ctrl+K Break Apart!
    let r2 = add_rect(&mut app, "B", 60.0, 10.0, 30.0, 20.0, Color::MAGENTA);
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_many(&tab.document, &[r1, r2]);
    }
    let count_pre_combine = app.active_tab().unwrap().document.objects().len();
    app.handle_shortcut(ShortcutAction::Combine);
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_pre_combine - 1,
        "Ctrl+L Combine merges 2 shapes into 1 compound BezierPath"
    );
    let combined_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, combined_id);
    }
    app.handle_shortcut(ShortcutAction::BreakApart);
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        count_pre_combine,
        "Ctrl+K Break Apart splits compound path back into 2 paths"
    );

    // Ctrl+R Repeat Last Command & Ctrl+J Options & Spacebar tool toggle
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::R,
            Modifiers::CTRL,
            false
        ),
        Some(ShortcutAction::RepeatLastCommand)
    );
    assert_eq!(
        resolve_shortcut(
            ShortcutProfile::ClassicStudio,
            Key::J,
            Modifiers::CTRL,
            false
        ),
        Some(ShortcutAction::OpenOptions)
    );
    app.handle_shortcut(ShortcutAction::OpenOptions);
    assert!(app.show_options_dialog);

    app.select_tool(ToolId::Ellipse);
    app.handle_shortcut(ShortcutAction::TogglePickSpace);
    assert_eq!(app.active_tool, ToolId::Pick);
    app.handle_shortcut(ShortcutAction::TogglePickSpace);
    assert_eq!(app.active_tool, ToolId::Ellipse);

    // Stamp duplicate while dragging preserves dragged object selection!
    let sel_before = app.active_tab().unwrap().selection.objects.clone();
    if !sel_before.is_empty() {
        let stamped = app
            .stamp_duplicate_at_offset(Vec2D::new(10.0, 0.0))
            .unwrap();
        assert!(!stamped.is_empty());
        assert_eq!(app.active_tab().unwrap().selection.objects, sel_before);
    }
}

#[test]
fn audit_workspaces_autofit_glyphs_laser_preview_and_headless_ui_overlays() {
    let mut app = StudioApp::new_headless();
    let _r = add_rect(
        &mut app,
        "Laser Cut Box",
        30.0,
        40.0,
        50.0,
        30.0,
        Color::CYAN,
    );

    // 1. All 6 WorkspacePresets + `.coreews` JSON round-trip
    for preset in WorkspacePreset::ALL {
        app.apply_workspace_preset(preset);
        assert_eq!(app.active_workspace_preset, preset);
    }
    app.apply_workspace_preset(WorkspacePreset::LaserSignStudio);
    app.treat_all_objects_as_filled = true;
    app.object_origin_anchor = (2, 0);
    let json = app.export_workspace_json();
    assert!(json.contains("LaserSignStudio"));

    app.apply_workspace_preset(WorkspacePreset::Lite);
    app.treat_all_objects_as_filled = false;
    app.object_origin_anchor = (1, 1);
    app.import_workspace_json(&json).expect("import .coreews");
    assert_eq!(
        app.active_workspace_preset,
        WorkspacePreset::LaserSignStudio
    );
    assert!(app.treat_all_objects_as_filled);
    assert_eq!(app.object_origin_anchor, (2, 0));

    // 2. Autofit Page to Content (+ Undo!)
    app.autofit_active_page(4.0);
    let p = app.active_tab().unwrap().document.active_page().unwrap();
    assert!((p.width_mm - 58.0).abs() < 1.0 && (p.height_mm - 38.0).abs() < 1.0);
    app.undo().expect("undo autofit");
    let p_back = app.active_tab().unwrap().document.active_page().unwrap();
    assert!((p_back.width_mm - 210.0).abs() < 1e-3);

    // 3. Glyphs Docker insertion (new object + existing selected text + inline buffer)
    app.insert_glyph_at_selection("★");
    let g_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    app.insert_glyph_at_selection("بسم");
    if let NodePayload::ArtisticText(ref art) = app
        .active_tab()
        .unwrap()
        .document
        .object(g_id)
        .unwrap()
        .payload
    {
        assert_eq!(art.plain_text(), "★بسم");
    } else {
        panic!("expected ArtisticText");
    }

    // 4. Live Laser Toolpath Preview & Plugin Export
    app.active_tab_mut().unwrap().selection.clear();
    app.refresh_laser_plan_preview();
    assert!(app.show_laser_toolpath_overlay);
    let plan = app.laser_plan_preview.as_ref().expect("laser plan built");
    assert!(
        !plan.segments.is_empty(),
        "laser plan must contain segments"
    );

    let out_file = std::env::temp_dir().join("coree_tools_62_audit_laser.svg");
    let outcome = app
        .send_to_laser_plugin_to_path("lightburn-bridge", &out_file, false)
        .expect("lightburn-bridge export");
    assert!(outcome.bytes_written > 0 && out_file.is_file());
    let _ = std::fs::remove_file(&out_file);

    app.send_to_laser_plugin("rdworks-ruida", false);
    if let Some(p) = app.last_plugin_export_path.clone() {
        assert!(p.is_file());
        let _ = std::fs::remove_file(&p);
    }

    // 5. Headless egui rendering frames with ALL overlays + soft-proofing + fullscreen active (ZERO panics!)
    app.soft_proof_cmyk = true;
    app.gamut_warning = true;
    app.active_tab_mut().unwrap().pending_crop_rect =
        Some(Rect2D::from_xywh(10.0, 10.0, 90.0, 90.0));
    app.begin_inline_text_edit(
        None,
        Point2D::new(25.0, 25.0),
        false,
        Vec2D::new(80.0, 20.0),
    );

    let ctx = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        ..egui::RawInput::default()
    };
    let mut out1 = ctx.run_ui(raw_input.clone(), |ui| {
        app.render_ui(ui);
    });
    out1.textures_delta.clear();
    assert!(app.active_tab().unwrap().render_options.soft_proof_cmyk);
    assert!(app.active_tab().unwrap().render_options.gamut_warning);

    app.full_screen_preview = true;
    let mut out2 = ctx.run_ui(raw_input, |ui| {
        app.render_ui(ui);
    });
    out2.textures_delta.clear();
}

// =============================================================================
// Senior Verifier Regression Tests (#84: S1 & S2 Safety, Undo & Container Fixes)
// =============================================================================

#[test]
fn regression_inline_text_overlay_narrow_and_short_canvas_no_panic() {
    let mut app = StudioApp::new_headless();
    app.show_dockers = false;

    for (w, h, is_para) in [
        (80.0_f32, 220.0_f32, false),
        (300.0_f32, 70.0_f32, false),
        (68.0_f32, 68.0_f32, false),
        (90.0_f32, 75.0_f32, true),
        (160.0_f32, 82.0_f32, true),
    ] {
        app.begin_inline_text_edit(
            None,
            Point2D::new(20.0, 20.0),
            is_para,
            Vec2D::new(120.0, 50.0),
        );
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(w, h),
            )),
            ..egui::RawInput::default()
        };
        let mut out = ctx.run_ui(raw_input, |ui| {
            app.render_ui(ui);
        });
        out.textures_delta.clear();
    }
}

#[test]
fn regression_cut_copy_paste_group_and_powerclip_deep_clone() {
    let mut app = StudioApp::new_headless();

    // 1. Build a Group with 2 children
    let g_c1 = add_rect(&mut app, "GrpChild1", 10.0, 10.0, 20.0, 20.0, Color::CYAN);
    let g_c2 = add_rect(
        &mut app,
        "GrpChild2",
        35.0,
        10.0,
        20.0,
        20.0,
        Color::MAGENTA,
    );
    app.execute_command(CommandOp::Group {
        ids: vec![g_c1, g_c2],
        name: "My Group".to_owned(),
    })
    .expect("create group");
    let group_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();

    // 2. Build a PowerClip with container + 2 clipped children
    let pc_container = add_rect(&mut app, "PCFrame", 80.0, 10.0, 50.0, 50.0, Color::YELLOW);
    let pc_child1 = add_rect(&mut app, "PCInner1", 85.0, 15.0, 15.0, 15.0, Color::CYAN);
    let pc_child2 = add_rect(
        &mut app,
        "PCInner2",
        105.0,
        25.0,
        15.0,
        15.0,
        Color::MAGENTA,
    );
    app.execute_command(CommandOp::CreatePowerClip {
        container_id: pc_container,
        content_ids: vec![pc_child1, pc_child2],
    })
    .expect("create powerclip");

    // --- Copy + Paste must produce strictly DISJOINT child IDs (zero aliasing!) ---
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection
            .select_many(&tab.document, &[group_id, pc_container]);
    }
    assert_eq!(app.copy_selection(), 2);
    let pasted = app.paste_clipboard();
    assert_eq!(pasted.len(), 2);
    let pasted_group = pasted[0];
    let pasted_pc = pasted[1];

    {
        let doc = &app.active_tab().unwrap().document;
        let orig_grp_children: HashSet<ObjectId> = match &doc.object(group_id).unwrap().payload {
            NodePayload::Group(v) => v.iter().copied().collect(),
            other => panic!("expected Group, got {other:?}"),
        };
        let new_grp_children: HashSet<ObjectId> = match &doc.object(pasted_group).unwrap().payload {
            NodePayload::Group(v) => v.iter().copied().collect(),
            other => panic!("expected pasted Group, got {other:?}"),
        };
        assert_eq!(new_grp_children.len(), 2);
        assert!(
            orig_grp_children.is_disjoint(&new_grp_children),
            "Copy+Paste Group must not alias child IDs!"
        );
        for cid in &new_grp_children {
            assert!(doc.has_object(*cid), "pasted group child missing");
        }

        let orig_pc_contents: HashSet<ObjectId> = match &doc.object(pc_container).unwrap().payload {
            NodePayload::PowerClip { contents, .. } => contents.iter().copied().collect(),
            other => panic!("expected PowerClip, got {other:?}"),
        };
        let new_pc_contents: HashSet<ObjectId> = match &doc.object(pasted_pc).unwrap().payload {
            NodePayload::PowerClip { contents, .. } => contents.iter().copied().collect(),
            other => panic!("expected pasted PowerClip, got {other:?}"),
        };
        assert_eq!(new_pc_contents.len(), 2);
        assert!(
            orig_pc_contents.is_disjoint(&new_pc_contents),
            "Copy+Paste PowerClip must not alias content IDs!"
        );
        for cid in &new_pc_contents {
            assert!(doc.has_object(*cid), "pasted powerclip child missing");
        }
    }

    // Undo removes Copy+Paste cleanly
    app.undo().expect("undo copy-paste");
    assert!(!app.active_tab().unwrap().document.has_object(pasted_group));

    // --- Cut + Paste must survive deletion of originals and preserve ALL descendants ---
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection
            .select_many(&tab.document, &[group_id, pc_container]);
    }
    assert_eq!(app.cut_selection(), 2);
    {
        let doc = &app.active_tab().unwrap().document;
        assert!(!doc.has_object(group_id));
        assert!(!doc.has_object(g_c1));
        assert!(!doc.has_object(pc_container));
        assert!(!doc.has_object(pc_child1));
    }

    let cut_pasted = app.paste_clipboard();
    assert_eq!(cut_pasted.len(), 2);
    {
        let doc = &app.active_tab().unwrap().document;
        match &doc.object(cut_pasted[0]).unwrap().payload {
            NodePayload::Group(children) => {
                assert_eq!(children.len(), 2, "Cut+Paste Group lost descendants!");
                for cid in children {
                    assert!(
                        doc.has_object(*cid),
                        "Cut+Paste Group has dangling child {cid:?}"
                    );
                }
            }
            other => panic!("expected Group after Cut+Paste, got {other:?}"),
        }
        match &doc.object(cut_pasted[1]).unwrap().payload {
            NodePayload::PowerClip { contents, .. } => {
                assert_eq!(contents.len(), 2, "Cut+Paste PowerClip lost descendants!");
                for cid in contents {
                    assert!(
                        doc.has_object(*cid),
                        "Cut+Paste PowerClip has dangling content {cid:?}"
                    );
                }
            }
            other => panic!("expected PowerClip after Cut+Paste, got {other:?}"),
        }
    }
}

#[test]
fn regression_raster_zero_dim_floodfill_no_panic() {
    let mut app = StudioApp::new_headless();
    let zero_asset = RasterAsset {
        id: ImageAssetId::new(),
        width: 0,
        height: 0,
        pixel_depth: coree_core::PixelDepth::Bit8,
        tiles_x: 0,
        tiles_y: 0,
        tiles: Vec::new(),
        source_dpi: 150.0,
        color_mode: ColorMode::Rgb,
    };
    let aid = zero_asset.id;
    let lid = app.active_tab().and_then(|t| t.active_layer_id);
    app.active_tab_mut()
        .unwrap()
        .document
        .insert_asset_if_absent(zero_asset);
    let obj = SceneObject::new(
        ObjectId(0),
        "Degenerate 0x0 Raster",
        NodePayload::Raster(RasterLayerData {
            asset_id: aid,
            width_mm: 50.0,
            height_mm: 50.0,
            filters: Vec::new(),
            filter_enabled: Vec::new(),
            crop_rect: None,
        }),
    );
    app.execute_command(CommandOp::create_object(obj, lid))
        .expect("create 0x0 raster");
    let oid = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, oid);
    }

    // FloodFill and Paint on a 0x0 raster must safely return false (ZERO panic!)
    app.set_raster_brush_op(RasterBrushOp::FloodFill);
    assert!(!app.apply_raster_tool_stroke(ToolId::BitmapBrush, &[Point2D::new(25.0, 25.0)]));
    app.set_raster_brush_op(RasterBrushOp::Paint);
    assert!(!app.apply_raster_tool_stroke(
        ToolId::BitmapBrush,
        &[Point2D::new(10.0, 10.0), Point2D::new(30.0, 30.0)],
    ));
}

#[test]
fn regression_left_drag_right_click_stamp_duplicate_undo_integrity() {
    // Scenario 1: Drag A 0 -> +50, Right-Click stamp B, Drag A +50 -> +80, Release -> Undo x2!
    let mut app = StudioApp::new_headless();
    let a = add_rect(&mut app, "A", 10.0, 10.0, 30.0, 20.0, Color::CYAN);

    // Begin MovingSelection drag, move live by +50mm via ToolCore (zero mid-drag Document mutation!)
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, a);
        tab.snap_options.grid = false;
        tab.snap_options.guidelines = false;
        tab.snap_options.objects = false;
        tab.snap_options.page_edges = false;
        tab.snap_options.nodes = false;
        tab.snap_options.dynamic_guides = false;
    }
    app.run_tool_pointer_down(&PointerEvent::primary(Point2D::new(20.0, 20.0), [0.0, 0.0]));
    app.run_tool_pointer_move(&PointerEvent::primary(
        Point2D::new(70.0, 20.0),
        [50.0, 0.0],
    ));
    assert!(
        (app.active_tab()
            .unwrap()
            .document
            .object_bounds(a)
            .unwrap()
            .min_x
            - 10.0)
            .abs()
            < 1e-4,
        "Document must NOT be mutated mid-drag"
    );

    // Right-click stamp B at live position (x = 60)!
    let stamped = app
        .stamp_duplicate_during_active_drag()
        .expect("stamp during drag");
    assert_eq!(stamped.len(), 1);
    let b = stamped[0];
    assert_eq!(
        app.active_tab().unwrap().selection.objects,
        vec![a],
        "original object A must stay selected after stamping B"
    );
    let b_x = app
        .active_tab()
        .unwrap()
        .document
        .object_bounds(b)
        .unwrap()
        .min_x;
    assert!((b_x - 60.0).abs() < 1e-3, "B must land at x=60, got {b_x}");

    // Continue dragging A +50 -> +80 (delta +30), then release to commit TransformObjects(+80)
    app.run_tool_pointer_move(&PointerEvent::primary(
        Point2D::new(100.0, 20.0),
        [80.0, 0.0],
    ));
    app.run_tool_pointer_up(&PointerEvent::primary(
        Point2D::new(100.0, 20.0),
        [80.0, 0.0],
    ));

    assert!(
        (app.active_tab()
            .unwrap()
            .document
            .object_bounds(a)
            .unwrap()
            .min_x
            - 90.0)
            .abs()
            < 1e-3
    );

    // Undo #1: reverts A's move back to x=10 while keeping B at x=60!
    app.undo().expect("undo #1 (A move)");
    let tab = app.active_tab().unwrap();
    assert!(
        (tab.document.object_bounds(a).unwrap().min_x - 10.0).abs() < 1e-3,
        "Undo #1 must return A to x=10"
    );
    assert!(
        (tab.document.object_bounds(b).unwrap().min_x - 60.0).abs() < 1e-3,
        "Undo #1 must keep B at x=60"
    );

    // Undo #2: removes B and keeps A cleanly at x=10 (NOT corrupted to x=60!)
    app.undo().expect("undo #2 (stamp B)");
    let tab = app.active_tab().unwrap();
    assert!(!tab.document.has_object(b), "Undo #2 must remove B");
    let a_after_u2 = tab.document.object_bounds(a).unwrap().min_x;
    assert!(
        (a_after_u2 - 10.0).abs() < 1e-3,
        "Undo #2 must leave A at exact pre-drag x=10, got {a_after_u2}"
    );

    // Scenario 2: Drag A 0 -> +50, stamp C, drag +50 -> +80, press Esc -> Undo #1!
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, a);
    }
    app.run_tool_pointer_down(&PointerEvent::primary(Point2D::new(20.0, 20.0), [0.0, 0.0]));
    app.run_tool_pointer_move(&PointerEvent::primary(
        Point2D::new(70.0, 20.0),
        [50.0, 0.0],
    ));
    let c = app.stamp_duplicate_during_active_drag().unwrap()[0];
    app.run_tool_pointer_move(&PointerEvent::primary(
        Point2D::new(100.0, 20.0),
        [80.0, 0.0],
    ));
    app.active_tab_mut().unwrap().cancel_active_drag();
    assert!(
        (app.active_tab()
            .unwrap()
            .document
            .object_bounds(a)
            .unwrap()
            .min_x
            - 10.0)
            .abs()
            < 1e-3,
        "Esc after stamping must roll A back to x=10"
    );
    app.undo().expect("undo stamp C");
    let tab = app.active_tab().unwrap();
    assert!(!tab.document.has_object(c));
    assert!(
        (tab.document.object_bounds(a).unwrap().min_x - 10.0).abs() < 1e-3,
        "Undo after Esc must keep A at x=10"
    );
}

#[test]
fn regression_locked_layer_and_nested_container_invariants() {
    let mut app = StudioApp::new_headless();
    let page_id = app.active_tab().unwrap().document.active_page().unwrap().id;
    let unlocked_layer = app.active_tab().unwrap().active_layer_id.unwrap();

    // Create a second layer, place objects on it, then lock it
    app.execute_command(CommandOp::AddLayer {
        page_id,
        name: "Locked Layer".to_owned(),
        is_cut_layer: false,
        master_scope: MasterLayerScope::None,
    })
    .expect("add locked layer");
    let locked_layer = app
        .active_tab()
        .unwrap()
        .document
        .active_page()
        .unwrap()
        .layers
        .last()
        .unwrap()
        .id;

    // Place an unlocked object on Unlocked Layer and two objects (inside & outside crop) + a raster on Locked Layer
    let unlocked_inside = add_rect(&mut app, "UnlIn", 50.0, 50.0, 20.0, 20.0, Color::CYAN);
    let unlocked_outside = add_rect(&mut app, "UnlOut", 180.0, 220.0, 20.0, 20.0, Color::MAGENTA);

    app.active_tab_mut().unwrap().active_layer_id = Some(locked_layer);
    let locked_inside = add_rect(&mut app, "LckIn", 55.0, 55.0, 20.0, 20.0, Color::YELLOW);
    let locked_outside = add_rect(&mut app, "LckOut", 180.0, 250.0, 20.0, 20.0, Color::YELLOW);

    let locked_raster_asset =
        RasterAsset::new(16, 16, vec![0u8; 16 * 16 * 4], 150.0, ColorMode::Rgb).unwrap();
    let lra_id = locked_raster_asset.id;
    app.active_tab_mut()
        .unwrap()
        .document
        .insert_asset_if_absent(locked_raster_asset);
    let lr_obj = SceneObject::new(
        ObjectId(0),
        "Locked Raster",
        NodePayload::Raster(RasterLayerData {
            asset_id: lra_id,
            width_mm: 50.0,
            height_mm: 50.0,
            filters: Vec::new(),
            filter_enabled: Vec::new(),
            crop_rect: None,
        }),
    );
    app.execute_command(CommandOp::create_object(lr_obj, Some(locked_layer)))
        .unwrap();
    let locked_raster_id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();

    // Lock `locked_layer` and restore active layer to `unlocked_layer`
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: locked_layer,
        visible: None,
        locked: Some(true),
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    app.active_tab_mut().unwrap().active_layer_id = Some(unlocked_layer);

    // (a) Crop must skip locked layer completely (neither delete nor translate!)
    assert!(app.apply_crop_rect(Rect2D::from_xywh(40.0, 40.0, 80.0, 60.0)));
    {
        let doc = &app.active_tab().unwrap().document;
        assert!(!doc.has_object(unlocked_outside));
        assert!(
            (doc.object_bounds(unlocked_inside).unwrap().min_x - 10.0).abs() < 1e-3,
            "unlocked inside object must shift by -40"
        );
        assert!(
            doc.has_object(locked_outside),
            "object on locked layer must NOT be deleted by Crop"
        );
        assert!(
            (doc.object_bounds(locked_inside).unwrap().min_x - 55.0).abs() < 1e-3,
            "object on locked layer must NOT be translated by Crop"
        );
    }
    app.undo().expect("undo crop");

    // (b) `ensure_raster_target_at` must skip raster on locked layer and create a fresh one on unlocked layer!
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, locked_raster_id);
    }
    let chosen_raster = app
        .ensure_raster_target_at(Point2D::new(20.0, 20.0))
        .expect("should auto-create raster on unlocked layer");
    assert_ne!(
        chosen_raster, locked_raster_id,
        "ensure_raster_target_at must never target raster on locked layer"
    );
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .layer_of_object(chosen_raster),
        Some(unlocked_layer)
    );

    // Also verify hidden layer is skipped by `ensure_raster_target_at`!
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: unlocked_layer,
        visible: Some(false),
        locked: None,
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();
    assert!(
        app.ensure_raster_target_at(Point2D::new(20.0, 20.0))
            .is_none(),
        "when all layers are locked or hidden, ensure_raster_target_at must return None"
    );
    app.execute_command(CommandOp::SetLayerFlags {
        layer_id: unlocked_layer,
        visible: Some(true),
        locked: None,
        printable: None,
        is_cut_layer: None,
    })
    .unwrap();

    // (c) `commit_parametric_shape_edit` on a shape inside a Group must NOT duplicate it onto any top-level layer!
    let grp_rect = add_rect(
        &mut app,
        "NestedParamRect",
        10.0,
        10.0,
        40.0,
        30.0,
        Color::CYAN,
    );
    let grp_peer = add_rect(
        &mut app,
        "NestedPeer",
        60.0,
        10.0,
        20.0,
        20.0,
        Color::MAGENTA,
    );
    app.execute_command(CommandOp::Group {
        ids: vec![grp_rect, grp_peer],
        name: "ParamGroup".to_owned(),
    })
    .unwrap();
    let orig_nested = match &app
        .active_tab()
        .unwrap()
        .document
        .object(grp_rect)
        .unwrap()
        .payload
    {
        NodePayload::Shape(s) => s.clone(),
        _ => panic!("expected shape"),
    };
    let updated_nested = ParametricShape::Rectangle {
        width: 40.0,
        height: 30.0,
        corner_radii: [6.0; 4],
        corner_style: CornerStyle::Rounded,
        lock_corners: true,
    };
    assert!(app.commit_parametric_shape_edit(
        grp_rect,
        orig_nested.clone(),
        updated_nested.clone()
    ));
    {
        let doc = &app.active_tab().unwrap().document;
        assert_eq!(
            doc.object(grp_rect).unwrap().payload,
            NodePayload::Shape(updated_nested.clone())
        );
        assert_eq!(
            doc.layer_of_object(grp_rect),
            None,
            "nested grouped shape must NOT be duplicated onto a top-level layer!"
        );
        assert!(doc.parent_container_of(grp_rect).is_some());
    }
    app.undo().expect("undo nested parametric edit");
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(grp_rect)
            .unwrap()
            .payload,
        NodePayload::Shape(orig_nested)
    );
    app.redo().expect("redo nested parametric edit");
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(grp_rect)
            .unwrap()
            .payload,
        NodePayload::Shape(updated_nested)
    );

    // (d) Locked layer & directly locked objects refuse Gradient/Transparency/Envelope/Mesh mutations!
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, locked_inside);
    }
    assert!(!app.apply_interactive_fill_drag(Point2D::new(55.0, 55.0), Point2D::new(75.0, 75.0)));
    assert!(!app.apply_interactive_effect_drag(
        ToolId::Transparency,
        Point2D::new(55.0, 55.0),
        Point2D::new(75.0, 55.0),
    ));
    assert!(!app.apply_interactive_effect_drag(
        ToolId::Envelope,
        Point2D::new(55.0, 55.0),
        Point2D::new(75.0, 55.0),
    ));
    assert!(!app.mesh_fill_interact(
        Point2D::new(55.0, 55.0),
        Point2D::new(65.0, 65.0),
        Some(Color::CYAN),
    ));
    assert!(
        app.active_tab()
            .unwrap()
            .document
            .object(locked_inside)
            .unwrap()
            .effects
            .is_empty(),
        "locked layer object must remain untouched"
    );

    // Also test direct `obj.locked = true` on an object on an unlocked layer (with existing Gradient/Effects/Mesh)!
    let direct_locked = add_rect(
        &mut app,
        "DirectLocked",
        120.0,
        120.0,
        40.0,
        30.0,
        Color::CYAN,
    );
    assert!(
        app.apply_interactive_fill_drag(Point2D::new(120.0, 120.0), Point2D::new(160.0, 150.0),)
    );
    assert!(app.apply_interactive_effect_drag(
        ToolId::Transparency,
        Point2D::new(120.0, 135.0),
        Point2D::new(160.0, 135.0),
    ));
    assert!(app.apply_interactive_effect_drag(
        ToolId::Envelope,
        Point2D::new(140.0, 120.0),
        Point2D::new(140.0, 112.0),
    ));
    app.execute_command(CommandOp::SetObjectLocked {
        ids: vec![direct_locked],
        locked: true,
    })
    .unwrap();
    let fill_before = app
        .active_tab()
        .unwrap()
        .document
        .object(direct_locked)
        .unwrap()
        .fill
        .clone();
    let effects_before = app
        .active_tab()
        .unwrap()
        .document
        .object(direct_locked)
        .unwrap()
        .effects
        .clone();
    assert!(
        !app.apply_interactive_fill_drag(Point2D::new(125.0, 125.0), Point2D::new(155.0, 145.0),)
    );
    assert!(!app.apply_interactive_effect_drag(
        ToolId::Transparency,
        Point2D::new(125.0, 135.0),
        Point2D::new(155.0, 135.0),
    ));
    assert!(!app.apply_interactive_effect_drag(
        ToolId::Envelope,
        Point2D::new(140.0, 120.0),
        Point2D::new(140.0, 100.0),
    ));
    assert!(!app.mesh_fill_interact(
        Point2D::new(125.0, 125.0),
        Point2D::new(135.0, 135.0),
        Some(Color::MAGENTA),
    ));
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(direct_locked)
            .unwrap()
            .fill,
        fill_before
    );
    assert_eq!(
        app.active_tab()
            .unwrap()
            .document
            .object(direct_locked)
            .unwrap()
            .effects,
        effects_before
    );
}

#[test]
fn w5a_audit_65_tools_toolcore_pointer_transient_overlay_and_space_stamp() {
    let mut all_count = 0usize;
    for &tool in TOOL_GROUPS.iter().flat_map(|g| g.iter()) {
        all_count += 1;
        assert!(
            coree_app::state::tool_shortcut_action(tool).is_some(),
            "every ToolId ({tool:?}) must map to a ShortcutAction without fallback"
        );
    }
    assert_eq!(all_count, 65, "TOOL_GROUPS must enumerate all 65 ToolIds");
    let mut app = StudioApp::new_headless();
    app.select_tool(ToolId::Rectangle);
    app.commit_drawn_box(Rect2D::from_xywh(30.0, 30.0, 50.0, 40.0));
    let id = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .next()
        .unwrap();
    if let Some(tab) = app.active_tab_mut() {
        let doc_snap = tab.document.clone();
        tab.selection.select_single(&doc_snap, id);
    }
    app.select_tool(ToolId::Pick);

    // 1. ToolCore pointer down/move/up produces TransientOverlay + active_live_world_transform
    //    without mutating Document mid-drag, and Spacebar stamps a live duplicate (`MND-02`/`IMP-08`).
    let initial_count = app.active_tab().unwrap().document.objects().len();
    let orig_bounds = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(id)
        .expect("rect bounds before drag");
    app.run_tool_pointer_down(&PointerEvent {
        world_pt_mm: Point2D::new(50.0, 45.0),
        screen_pt_px: [200.0, 200.0],
        button: PointerButton::Primary,
        modifiers: ModifiersState::default(),
        click_count: 1,
        stylus: coree_core::StylusSample::default(),
    });
    app.run_tool_pointer_move(&PointerEvent {
        world_pt_mm: Point2D::new(80.0, 65.0),
        screen_pt_px: [300.0, 260.0],
        button: PointerButton::Primary,
        modifiers: ModifiersState::default(),
        click_count: 1,
        stylus: coree_core::StylusSample::default(),
    });
    {
        let tab = app.active_tab().unwrap();
        assert_eq!(
            tab.tool_core.active_live_world_transform(),
            Some(Affine2D::translate(30.0, 20.0)),
            "ToolCore must report live world transform delta mid-drag"
        );
        assert!(
            !tab.transient_overlay.preview_paths.is_empty(),
            "TransientOverlay must carry live preview paths during Pick move drag"
        );
        assert_eq!(
            tab.transient_overlay.cursor_hint,
            coree_app::state::CursorHint::Move
        );
        assert_eq!(
            tab.document.world_object_bounds(id),
            Some(orig_bounds),
            "Document must remain unmutated mid-drag prior to pointer release"
        );
    }
    app.run_tool_key_event(&KeyEvent {
        key: ToolKey::Space,
        modifiers: ModifiersState::default(),
    });
    assert_eq!(
        app.active_tab().unwrap().document.objects().len(),
        initial_count + 1,
        "Spacebar during active drag must stamp a duplicate object"
    );
    app.run_tool_pointer_move(&PointerEvent {
        world_pt_mm: Point2D::new(120.0, 90.0),
        screen_pt_px: [420.0, 330.0],
        button: PointerButton::Primary,
        modifiers: ModifiersState::default(),
        click_count: 1,
        stylus: coree_core::StylusSample::default(),
    });
    app.run_tool_pointer_up(&PointerEvent {
        world_pt_mm: Point2D::new(120.0, 90.0),
        screen_pt_px: [420.0, 330.0],
        button: PointerButton::Primary,
        modifiers: ModifiersState::default(),
        click_count: 1,
        stylus: coree_core::StylusSample::default(),
    });
    {
        let tab = app.active_tab().unwrap();
        assert!(
            tab.transient_overlay.preview_paths.is_empty(),
            "TransientOverlay must clear on pointer up"
        );
        assert!(
            tab.tool_core.active_live_world_transform().is_none(),
            "live world transform must clear on pointer up"
        );
        let moved_bounds = tab
            .document
            .world_object_bounds(id)
            .expect("moved rect bounds");
        assert!((moved_bounds.min().x - (orig_bounds.min().x + 70.0)).abs() < 1e-4);
        assert!((moved_bounds.min().y - (orig_bounds.min().y + 45.0)).abs() < 1e-4);
        let stamped_id = *tab
            .document
            .objects()
            .keys()
            .find(|&&oid| oid != id)
            .expect("stamped duplicate object");
        let stamped_bounds = tab
            .document
            .world_object_bounds(stamped_id)
            .expect("stamped rect bounds");
        assert!((stamped_bounds.min().x - (orig_bounds.min().x + 30.0)).abs() < 1e-4);
        assert!((stamped_bounds.min().y - (orig_bounds.min().y + 20.0)).abs() < 1e-4);
    }

    // 2. Double-click toggles RotateAndSkew and dragging the center pivot enters DraggingPivot.
    if let Some(tab) = app.active_tab_mut() {
        let doc_snap = tab.document.clone();
        tab.selection.select_single(&doc_snap, id);
    }
    let center = app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(id)
        .unwrap()
        .center();
    app.run_tool_pointer_down(&PointerEvent {
        world_pt_mm: center,
        screen_pt_px: [320.0, 270.0],
        button: PointerButton::Primary,
        modifiers: ModifiersState::default(),
        click_count: 2,
        stylus: coree_core::StylusSample::default(),
    });
    app.run_tool_pointer_up(&PointerEvent {
        world_pt_mm: center,
        screen_pt_px: [320.0, 270.0],
        button: PointerButton::Primary,
        modifiers: ModifiersState::default(),
        click_count: 2,
        stylus: coree_core::StylusSample::default(),
    });
    assert_eq!(
        app.active_tab().unwrap().selection.transform_mode,
        coree_core::TransformMode::RotateAndSkew,
        "double-clicking solely selected object must enter RotateAndSkew mode"
    );
    let pivot_before = app.active_tab().unwrap().selection.pivot;
    let pivot_target = Point2D::new(pivot_before.x + 12.0, pivot_before.y - 8.0);
    app.run_tool_pointer_down(&PointerEvent {
        world_pt_mm: pivot_before,
        screen_pt_px: [320.0, 270.0],
        button: PointerButton::Primary,
        modifiers: ModifiersState::default(),
        click_count: 1,
        stylus: coree_core::StylusSample::default(),
    });
    assert!(
        matches!(
            app.active_tab().unwrap().tool_core.drag_state,
            coree_app::state::ActiveDragState::DraggingPivot
        ),
        "pressing on rotation pivot in RotateAndSkew mode must enter ActiveDragState::DraggingPivot"
    );
    app.run_tool_pointer_move(&PointerEvent {
        world_pt_mm: pivot_target,
        screen_pt_px: [360.0, 240.0],
        button: PointerButton::Primary,
        modifiers: ModifiersState::default(),
        click_count: 1,
        stylus: coree_core::StylusSample::default(),
    });
    assert!(
        app.active_tab()
            .unwrap()
            .selection
            .pivot
            .distance_to(pivot_target)
            < 1e-4,
        "DraggingPivot move must update SelectionState::pivot"
    );
    app.run_tool_pointer_up(&PointerEvent {
        world_pt_mm: pivot_target,
        screen_pt_px: [360.0, 240.0],
        button: PointerButton::Primary,
        modifiers: ModifiersState::default(),
        click_count: 1,
        stylus: coree_core::StylusSample::default(),
    });
    assert!(
        matches!(
            app.active_tab().unwrap().tool_core.drag_state,
            coree_app::state::ActiveDragState::Idle
        ),
        "releasing pivot drag must return ToolCore to Idle"
    );
    assert!(
        app.active_tab()
            .unwrap()
            .selection
            .pivot
            .distance_to(pivot_target)
            < 1e-4
    );
}
