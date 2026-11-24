// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Comprehensive headless interaction tests for Layer 3 [`ToolCore`] across Pick, Shape/Node,
//! Drawing, Interactive Fill, Effects, and Guide workflows.

use coree_core::{
    Affine2D, BezierPath, Color, CommandHistory, CommandOp, CornerStyle, Document,
    DocumentInvalidation, Effect, FeatherDirection, Fill, GradientStop, GuideOrientation,
    LensConfig, LensKind, NodeKind, NodePayload, ObjectId, OutlineStyle, ParametricShape, PathNode,
    Point2D, SceneObject, SelectionState, SnapEngine, SnapOptions, Subpath, TextStyle,
    TransformMode, Viewport,
};
use coree_tools::{
    KeyEvent, ModifiersState, PointerEvent, ToolActionRequest, ToolCore, ToolId, ToolKey,
    ToolOutput, ToolReadContext, ToolSettingsSnapshot,
};

struct Harness {
    doc: Document,
    history: CommandHistory,
    selection: SelectionState,
    viewport: Viewport,
    snap_options: SnapOptions,
    default_fill: Fill,
    default_outline: OutlineStyle,
    default_text_style: TextStyle,
    default_lens: LensConfig,
    settings: ToolSettingsSnapshot,
}

impl Harness {
    fn new() -> Self {
        let doc = Document::new(
            "ToolCore Test",
            210.0,
            297.0,
            coree_core::ColorMode::Cmyk,
            300.0,
        );
        let mut viewport = Viewport::new(800, 600);
        viewport.pan_mm = Point2D::new(105.0, 148.5);
        viewport.zoom = 2.0;
        let snap_options = SnapOptions {
            grid: false,
            guidelines: false,
            page_edges: false,
            objects: false,
            nodes: false,
            dynamic_guides: false,
            ..Default::default()
        };
        Self {
            doc,
            history: CommandHistory::default(),
            selection: SelectionState::new(),
            viewport,
            snap_options,
            default_fill: Fill::Solid(Color::CYAN),
            default_outline: OutlineStyle::default(),
            default_text_style: TextStyle::default(),
            default_lens: LensConfig::new(LensKind::Magnify { amount: 2.0 }),
            settings: ToolSettingsSnapshot::default(),
        }
    }

    fn layer_id(&self) -> coree_core::LayerId {
        self.doc.active_page().unwrap().layers[0].id
    }

    fn add_rect(&mut self, name: &str, x: f64, y: f64, w: f64, h: f64) -> ObjectId {
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
        .with_fill(self.default_fill.clone())
        .with_outline(self.default_outline.clone())
        .with_transform(Affine2D::translate(x, y));
        let lid = Some(self.layer_id());
        self.history
            .execute(&mut self.doc, CommandOp::create_object(obj, lid))
            .unwrap();
        *self.doc.objects().keys().last().unwrap()
    }

    fn ctx(&self) -> ToolReadContext<'_> {
        let lid = Some(self.layer_id());
        ToolReadContext {
            document: &self.doc,
            selection: &self.selection,
            snap_engine: &SnapEngine,
            viewport: &self.viewport,
            snap_options: &self.snap_options,
            active_layer_id: lid,
            creation_layer_id: lid,
            powerclip_edit_id: None,
            treat_all_objects_as_filled: true,
            default_fill: &self.default_fill,
            default_outline: &self.default_outline,
            default_text_style: &self.default_text_style,
            default_lens: &self.default_lens,
            settings: &self.settings,
        }
    }

    fn commit(&mut self, out: ToolOutput) -> usize {
        match out {
            ToolOutput::Commit { ops, .. } => {
                let count = ops.len();
                if count == 1 {
                    self.history
                        .execute(&mut self.doc, ops.into_iter().next().unwrap())
                        .unwrap();
                } else if count > 1 {
                    self.history
                        .execute(&mut self.doc, CommandOp::Batch(ops))
                        .unwrap();
                }
                count
            }
            other => panic!("expected Commit, got {other:?}"),
        }
    }
}

#[test]
fn pick_move_zero_mid_drag_doc_mutation_overlay_space_stamp_and_esc_cancel() {
    let mut h = Harness::new();
    let id = h.add_rect("Box", 10.0, 20.0, 40.0, 30.0);
    h.selection.select_single(&h.doc, id);
    let doc_before = h.doc.clone();

    let mut tc = ToolCore::new(ToolId::Pick);
    let p0 = Point2D::new(30.0, 35.0);
    let p1 = Point2D::new(55.0, 45.0);
    let out_down = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(p0, [0.0, 0.0]));
    assert!(matches!(
        out_down,
        ToolOutput::Continue { .. } | ToolOutput::Consumed { .. }
    ));

    let out_move = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(p1, [25.0, 10.0]));
    match out_move {
        ToolOutput::Continue { overlay } => {
            assert!(
                !overlay.preview_paths.is_empty(),
                "Pick move must produce preview_paths"
            );
            assert!(
                overlay.dirty_world_bbox_mm.is_some(),
                "Pick move must produce dirty_world_bbox_mm"
            );
        }
        other => panic!("expected Continue overlay, got {other:?}"),
    }
    assert_eq!(
        h.doc.world_object_bounds(id),
        doc_before.world_object_bounds(id),
        "Document MUST NOT mutate mid-drag"
    );

    // Spacebar mid-drag must emit StampDuplicateDuringDrag without mutating Document inside ToolCore
    let space_out = tc.on_key(
        &h.ctx(),
        &KeyEvent {
            key: ToolKey::Space,
            modifiers: ModifiersState::default(),
        },
    );
    match space_out {
        ToolOutput::Consumed { actions, .. } => {
            assert!(actions.iter().any(|a| matches!(
                a,
                ToolActionRequest::StampDuplicateDuringDrag { live_world_affine }
                    if (live_world_affine.transform_point(Point2D::ZERO).x - 25.0).abs() < 1e-6
            )));
        }
        other => panic!("expected Consumed StampDuplicateDuringDrag, got {other:?}"),
    }

    // Escape mid-drag cancels cleanly leaving Document untouched
    let cancel_out = tc.on_key(
        &h.ctx(),
        &KeyEvent {
            key: ToolKey::Escape,
            modifiers: ModifiersState::default(),
        },
    );
    assert_eq!(cancel_out, ToolOutput::Cancel);
    assert!(!tc.is_dragging());
    assert_eq!(
        h.doc.world_object_bounds(id),
        doc_before.world_object_bounds(id)
    );

    // Full drag -> Commit -> Execute -> Undo -> Redo
    tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(p0, [0.0, 0.0]));
    tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(p1, [25.0, 10.0]));
    let commit_out = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(p1, [25.0, 10.0]));
    assert!(h.commit(commit_out) >= 1);
    assert!(matches!(
        h.history.last_invalidation(),
        DocumentInvalidation::Targeted { object_ids, .. } if object_ids.contains(&id)
    ));
    let moved_bb = h.doc.world_object_bounds(id).unwrap();
    assert!((moved_bb.min_x - 35.0).abs() < 1e-5);
    assert!((moved_bb.min_y - 30.0).abs() < 1e-5);

    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(
        h.doc.world_object_bounds(id),
        doc_before.world_object_bounds(id)
    );
    h.history.redo(&mut h.doc).unwrap();
    assert!((h.doc.world_object_bounds(id).unwrap().min_x - 35.0).abs() < 1e-5);
}

#[test]
fn pick_scale_rotate_and_skew_modes_work_via_pointer_drags() {
    let mut h = Harness::new();
    let id = h.add_rect("Target", 10.0, 10.0, 40.0, 20.0);
    h.selection.select_single(&h.doc, id);

    // 1. Scale bottom-right handle (50, 30) -> (90, 50)
    let mut tc = ToolCore::new(ToolId::Pick);
    let br = Point2D::new(50.0, 30.0);
    let br_s = h.viewport.world_to_screen(br);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(br, [br_s.x as f32, br_s.y as f32]),
    );
    let br2 = Point2D::new(90.0, 50.0);
    let br2_s = h.viewport.world_to_screen(br2);
    let out_m = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(br2, [br2_s.x as f32, br2_s.y as f32]),
    );
    assert!(matches!(out_m, ToolOutput::Continue { overlay } if !overlay.preview_paths.is_empty()));
    let scale_commit = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(br2, [br2_s.x as f32, br2_s.y as f32]),
    );
    h.commit(scale_commit);
    let scaled_bb = h.doc.world_object_bounds(id).unwrap();
    assert!((scaled_bb.width() - 80.0).abs() < 1e-3);
    assert!((scaled_bb.height() - 40.0).abs() < 1e-3);

    // 2. RotateAndSkew corner rotation with Ctrl 15-deg snap
    h.selection.transform_mode = TransformMode::RotateAndSkew;
    let bb = h.doc.world_object_bounds(id).unwrap();
    let pivot = h.selection.pivot;
    let corner = bb.max();
    let corner_s = h.viewport.world_to_screen(corner);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(corner, [corner_s.x as f32, corner_s.y as f32]),
    );
    let rot_pt = Point2D::new(pivot.x, pivot.y + 40.0);
    let rot_s = h.viewport.world_to_screen(rot_pt);
    let ctrl_mods = ModifiersState {
        ctrl: true,
        shift: false,
        alt: false,
    };
    tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::with_modifiers(rot_pt, [rot_s.x as f32, rot_s.y as f32], ctrl_mods),
    );
    let rot_commit = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::with_modifiers(rot_pt, [rot_s.x as f32, rot_s.y as f32], ctrl_mods),
    );
    h.commit(rot_commit);
    assert_ne!(
        h.doc.world_object_bounds(id).unwrap(),
        bb,
        "Rotation must alter object bounds"
    );

    // 3. RotateAndSkew top-edge handle -> Skew
    let bb2 = h.doc.world_object_bounds(id).unwrap();
    let top_mid = Point2D::new(bb2.center().x, bb2.min_y);
    let top_s = h.viewport.world_to_screen(top_mid);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(top_mid, [top_s.x as f32, top_s.y as f32]),
    );
    let skew_pt = Point2D::new(top_mid.x + 20.0, top_mid.y);
    let skew_s = h.viewport.world_to_screen(skew_pt);
    tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(skew_pt, [skew_s.x as f32, skew_s.y as f32]),
    );
    let skew_commit = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(skew_pt, [skew_s.x as f32, skew_s.y as f32]),
    );
    h.commit(skew_commit);
    assert_ne!(h.doc.world_object_bounds(id).unwrap(), bb2);
}

#[test]
fn shape_node_anchor_handle_curve_and_parametric_corner_drag() {
    let mut h = Harness::new();
    // Part A: Bezier path node + handle drag
    let path = BezierPath::from_subpath(Subpath {
        nodes: vec![
            PathNode::new(
                Point2D::new(20.0, 20.0),
                None,
                Some(Point2D::new(35.0, 10.0)),
                NodeKind::Cusp,
            ),
            PathNode::new(
                Point2D::new(80.0, 20.0),
                Some(Point2D::new(65.0, 10.0)),
                None,
                NodeKind::Cusp,
            ),
        ],
        closed: false,
    });
    let path_obj = SceneObject::new(ObjectId(0), "Curve", NodePayload::Path(path))
        .with_outline(h.default_outline.clone());
    let lid = Some(h.layer_id());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(path_obj, lid))
        .unwrap();
    let pid = *h.doc.objects().keys().last().unwrap();
    h.selection.select_single(&h.doc, pid);

    let mut tc = ToolCore::new(ToolId::ShapeNode);
    // Drag anchor node 0 from (20,20) -> (20,50)
    let a0 = Point2D::new(20.0, 20.0);
    let a0_s = h.viewport.world_to_screen(a0);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(a0, [a0_s.x as f32, a0_s.y as f32]),
    );
    let a1 = Point2D::new(20.0, 50.0);
    let a1_s = h.viewport.world_to_screen(a1);
    let mv = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(a1, [a1_s.x as f32, a1_s.y as f32]),
    );
    assert!(
        matches!(mv, ToolOutput::Continue { overlay } if !overlay.preview_paths.is_empty() && !overlay.preview_handles.is_empty())
    );
    // Mid-drag doc node untouched
    if let NodePayload::Path(ref p) = h.doc.object(pid).unwrap().payload {
        assert!((p.subpaths[0].nodes[0].pos.y - 20.0).abs() < 1e-6);
    }
    let up = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(a1, [a1_s.x as f32, a1_s.y as f32]),
    );
    h.commit(up);
    if let NodePayload::Path(ref p) = h.doc.object(pid).unwrap().payload {
        assert!((p.subpaths[0].nodes[0].pos.y - 50.0).abs() < 1e-4);
    } else {
        panic!("expected Path");
    }

    // Part B: Parametric Rectangle corner radius handle drag
    let rid = h.add_rect("ParamRect", 100.0, 20.0, 40.0, 40.0);
    h.selection.select_single(&h.doc, rid);
    let tl_handle = Point2D::new(100.0, 20.0);
    let tl_s = h.viewport.world_to_screen(tl_handle);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(tl_handle, [tl_s.x as f32, tl_s.y as f32]),
    );
    let dragged = Point2D::new(112.0, 32.0);
    let d_s = h.viewport.world_to_screen(dragged);
    let mv_r = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(dragged, [d_s.x as f32, d_s.y as f32]),
    );
    assert!(matches!(mv_r, ToolOutput::Continue { overlay } if !overlay.preview_paths.is_empty()));
    let up_r = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(dragged, [d_s.x as f32, d_s.y as f32]),
    );
    h.commit(up_r);
    match &h.doc.object(rid).unwrap().payload {
        NodePayload::Shape(ParametricShape::Rectangle { corner_radii, .. }) => {
            assert!(
                corner_radii[0] > 5.0,
                "Expected rounded corner > 5mm, got {:?}",
                corner_radii
            );
        }
        other => panic!("expected Rectangle, got {other:?}"),
    }
}

#[test]
fn drawing_tools_rect_ctrl_square_ellipse_bezier_pen_and_3pt_curve() {
    let mut h = Harness::new();

    // 1. Rectangle with Ctrl -> exact square
    let mut tc = ToolCore::new(ToolId::Rectangle);
    let ctrl = ModifiersState {
        ctrl: true,
        shift: false,
        alt: false,
    };
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::with_modifiers(Point2D::new(10.0, 10.0), [0.0, 0.0], ctrl),
    );
    let mv = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::with_modifiers(Point2D::new(70.0, 40.0), [60.0, 30.0], ctrl),
    );
    assert!(matches!(mv, ToolOutput::Continue { overlay } if !overlay.preview_paths.is_empty()));
    let rect_commit = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::with_modifiers(Point2D::new(70.0, 40.0), [60.0, 30.0], ctrl),
    );
    h.commit(rect_commit);
    let rect_id = *h.doc.objects().keys().last().unwrap();
    let r_bb = h.doc.world_object_bounds(rect_id).unwrap();
    assert!((r_bb.width() - r_bb.height()).abs() < 1e-4 && (r_bb.width() - 60.0).abs() < 1e-4);

    // 2. Ellipse
    tc.set_active_tool(ToolId::Ellipse);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(100.0, 20.0), [0.0, 0.0]),
    );
    tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(140.0, 50.0), [40.0, 30.0]),
    );
    let ell_commit = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(140.0, 50.0), [40.0, 30.0]),
    );
    h.commit(ell_commit);
    assert!(matches!(
        h.doc.objects().values().last().unwrap().payload,
        NodePayload::Shape(ParametricShape::Ellipse { .. })
    ));

    // 3. Bezier multi-point + Enter confirm
    tc.set_active_tool(ToolId::Bezier);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(10.0, 100.0), [0.0, 0.0]),
    );
    tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(10.0, 100.0), [0.0, 0.0]),
    );
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(50.0, 100.0), [40.0, 0.0]),
    );
    tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(50.0, 100.0), [40.0, 0.0]),
    );
    assert!(tc.has_active_draft_or_drag());
    let bez_confirm = tc.confirm(&h.ctx());
    h.commit(bez_confirm);
    assert!(!tc.has_active_draft_or_drag());

    // 4. Pen closed path (click back on first node)
    tc.set_active_tool(ToolId::Pen);
    let p0 = Point2D::new(10.0, 150.0);
    let p0_s = h.viewport.world_to_screen(p0);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(p0, [p0_s.x as f32, p0_s.y as f32]),
    );
    tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(p0, [p0_s.x as f32, p0_s.y as f32]),
    );
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(60.0, 150.0), [0.0, 0.0]),
    );
    tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(60.0, 150.0), [0.0, 0.0]),
    );
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(35.0, 190.0), [0.0, 0.0]),
    );
    tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(35.0, 190.0), [0.0, 0.0]),
    );
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(p0, [p0_s.x as f32, p0_s.y as f32]),
    );
    let pen_close = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(p0, [p0_s.x as f32, p0_s.y as f32]),
    );
    h.commit(pen_close);

    // 5. ThreePointCurve (drag chord p1->p2, release, move/click p3)
    tc.set_active_tool(ToolId::ThreePointCurve);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(10.0, 220.0), [0.0, 0.0]),
    );
    tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(80.0, 220.0), [70.0, 0.0]),
    );
    tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(80.0, 220.0), [70.0, 0.0]),
    );
    assert!(tc.three_point_curve_chord.is_some());
    let arc_commit = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(45.0, 200.0), [35.0, -20.0]),
    );
    h.commit(arc_commit);
    assert!(tc.three_point_curve_chord.is_none());
}

#[test]
fn interactive_fill_effect_handles_and_canvas_guide_drag() {
    let mut h = Harness::new();
    let mut grad_obj = SceneObject::new(
        ObjectId(0),
        "Grad",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 80.0,
            height: 40.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::LinearGradient {
        start: Point2D::new(0.0, 20.0),
        end: Point2D::new(80.0, 20.0),
        stops: vec![
            GradientStop::new(0.0, Color::CYAN),
            GradientStop::new(1.0, Color::MAGENTA),
        ],
    });
    grad_obj.effects.push(Effect::DropShadow {
        dx_mm: 5.0,
        dy_mm: 5.0,
        blur_mm: 4.0,
        opacity: 0.6,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Middle,
    });
    let lid = Some(h.layer_id());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(grad_obj, lid))
        .unwrap();
    let gid = *h.doc.objects().keys().last().unwrap();
    h.selection.select_single(&h.doc, gid);

    // 1. InteractiveFill drag end handle (80, 20) -> (60, 30)
    let mut tc = ToolCore::new(ToolId::InteractiveFill);
    let end_pt = Point2D::new(80.0, 20.0);
    let end_s = h.viewport.world_to_screen(end_pt);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(end_pt, [end_s.x as f32, end_s.y as f32]),
    );
    let new_end = Point2D::new(60.0, 30.0);
    let new_s = h.viewport.world_to_screen(new_end);
    let mv_g = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(new_end, [new_s.x as f32, new_s.y as f32]),
    );
    assert!(matches!(mv_g, ToolOutput::Continue { overlay } if !overlay.preview_paths.is_empty()));
    let fill_commit = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(new_end, [new_s.x as f32, new_s.y as f32]),
    );
    h.commit(fill_commit);
    if let Fill::LinearGradient { end, .. } = &h.doc.object(gid).unwrap().fill {
        assert!((end.x - 60.0).abs() < 1e-4 && (end.y - 30.0).abs() < 1e-4);
    } else {
        panic!("expected LinearGradient");
    }

    // 2. DropShadow handle drag (center (40,20) + (5,5) = (45,25)) -> (55, 35)
    tc.set_active_tool(ToolId::DropShadow);
    let sh_pt = Point2D::new(45.0, 25.0);
    let sh_s = h.viewport.world_to_screen(sh_pt);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(sh_pt, [sh_s.x as f32, sh_s.y as f32]),
    );
    let sh2 = Point2D::new(55.0, 35.0);
    let sh2_s = h.viewport.world_to_screen(sh2);
    let mv_s = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(sh2, [sh2_s.x as f32, sh2_s.y as f32]),
    );
    assert!(
        matches!(mv_s, ToolOutput::Continue { overlay } if !overlay.preview_handles.is_empty())
    );
    let sh_commit = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(sh2, [sh2_s.x as f32, sh2_s.y as f32]),
    );
    h.commit(sh_commit);
    if let Effect::DropShadow { dx_mm, dy_mm, .. } = &h.doc.object(gid).unwrap().effects[0] {
        assert!((*dx_mm - 15.0).abs() < 1e-4 && (*dy_mm - 15.0).abs() < 1e-4);
    } else {
        panic!("expected DropShadow");
    }

    // 3. Canvas Guide drag in Pick tool (empty space on guide -> MoveGuide)
    h.selection.clear();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::AddGuide {
                orientation: GuideOrientation::Horizontal(100.0),
                locked: false,
            },
        )
        .unwrap();
    tc.set_active_tool(ToolId::Pick);
    let g_pt = Point2D::new(50.0, 100.0);
    let g_s = h.viewport.world_to_screen(g_pt);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(g_pt, [g_s.x as f32, g_s.y as f32]),
    );
    let g2 = Point2D::new(50.0, 140.0);
    let g2_s = h.viewport.world_to_screen(g2);
    let mv_guide = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(g2, [g2_s.x as f32, g2_s.y as f32]),
    );
    assert!(
        matches!(mv_guide, ToolOutput::Continue { overlay } if !overlay.preview_paths.is_empty())
    );
    assert_eq!(
        h.doc.guides()[0].orientation,
        GuideOrientation::Horizontal(100.0)
    );
    let guide_commit = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(g2, [g2_s.x as f32, g2_s.y as f32]),
    );
    h.commit(guide_commit);
    assert_eq!(
        h.doc.guides()[0].orientation,
        GuideOrientation::Horizontal(140.0)
    );
}

#[test]
fn w1c_r2_pick_dual_click_rotate_skew_shift_deselect_and_double_click() {
    let mut h = Harness::new();
    let id = h.add_rect("Target", 20.0, 20.0, 40.0, 30.0);
    let mut tc = ToolCore::new(ToolId::Pick);
    let center = Point2D::new(40.0, 35.0);
    let center_s = h.viewport.world_to_screen(center);
    let cs = [center_s.x as f32, center_s.y as f32];

    // 1. First click selects in ScaleAndMove
    let out1_d = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(center, cs));
    if let ToolOutput::Consumed { actions, .. } = out1_d {
        for a in actions {
            if let ToolActionRequest::SelectObjects { ids, toggle, .. } = a {
                h.selection.click_object(&h.doc, ids[0], toggle);
            }
        }
    }
    let out1_u = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(center, cs));
    assert_eq!(out1_u, ToolOutput::noop());
    assert_eq!(h.selection.objects, vec![id]);
    assert_eq!(h.selection.transform_mode, TransformMode::ScaleAndMove);

    // 2. Second click on already-selected sole object (no drag) toggles RotateAndSkew
    tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(center, cs));
    let out2_u = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(center, cs));
    match out2_u {
        ToolOutput::Consumed { actions, .. } => {
            assert!(actions.iter().any(|a| matches!(
                a,
                ToolActionRequest::SelectObjects { ids, toggle: false, .. } if ids == &vec![id]
            )));
            h.selection.click_object(&h.doc, id, false);
        }
        other => panic!("expected SelectObjects on zero-drag click, got {other:?}"),
    }
    assert_eq!(h.selection.transform_mode, TransformMode::RotateAndSkew);

    // 3. Double-click on selection handle toggles back via ToggleTransformMode
    let corner = Point2D::new(60.0, 50.0);
    let corner_s = h.viewport.world_to_screen(corner);
    let mut dbl_ev = PointerEvent::primary(corner, [corner_s.x as f32, corner_s.y as f32]);
    dbl_ev.click_count = 2;
    let dbl_out = tc.on_pointer_down(&h.ctx(), &dbl_ev);
    assert_eq!(
        dbl_out,
        ToolOutput::actions(vec![ToolActionRequest::ToggleTransformMode])
    );
    h.selection.toggle_transform_mode();
    assert_eq!(h.selection.transform_mode, TransformMode::ScaleAndMove);

    // 4. Double-click on sole selected non-text body toggles to RotateAndSkew
    let mut dbl_body = PointerEvent::primary(center, cs);
    dbl_body.click_count = 2;
    assert_eq!(
        tc.on_pointer_down(&h.ctx(), &dbl_body),
        ToolOutput::actions(vec![ToolActionRequest::ToggleTransformMode])
    );
    h.selection.toggle_transform_mode();
    assert_eq!(h.selection.transform_mode, TransformMode::RotateAndSkew);

    // 5. Shift+Click on selected object deselects it
    let shift_mods = ModifiersState {
        shift: true,
        ctrl: false,
        alt: false,
    };
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::with_modifiers(center, cs, shift_mods),
    );
    let shift_up = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::with_modifiers(center, cs, shift_mods),
    );
    match shift_up {
        ToolOutput::Consumed { actions, .. } => {
            assert!(actions.iter().any(|a| matches!(
                a,
                ToolActionRequest::SelectObjects { ids, toggle: true, .. } if ids == &vec![id]
            )));
            h.selection.click_object(&h.doc, id, true);
        }
        other => panic!("expected Shift+Click toggle action, got {other:?}"),
    }
    assert!(h.selection.is_empty());
}

#[test]
fn w1c_r2_sculpt_envelope_and_freehand_pick_zero_drag_clicks() {
    let mut h = Harness::new();
    let id = h.add_rect("SculptRect", 20.0, 20.0, 60.0, 60.0);
    h.selection.select_single(&h.doc, id);

    // 1. Sculpt single click far from corners/nodes -> noop (ParametricShape preserved!)
    let mut tc = ToolCore::new(ToolId::Smear);
    let far_pt = Point2D::new(50.0, 50.0);
    tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(far_pt, [0.0, 0.0]));
    let far_up = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(far_pt, [0.0, 0.0]));
    assert_eq!(far_up, ToolOutput::noop());
    assert!(matches!(
        h.doc.object(id).unwrap().payload,
        NodePayload::Shape(ParametricShape::Rectangle { .. })
    ));

    // 2. Sculpt single click near a corner -> deforms & commits ConvertToCurves + EditPath
    let corner_pt = Point2D::new(20.0, 20.0);
    tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(corner_pt, [0.0, 0.0]));
    let sculpt_up = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(corner_pt, [0.0, 0.0]));
    h.commit(sculpt_up);
    assert!(matches!(
        h.doc.object(id).unwrap().payload,
        NodePayload::Path(_)
    ));

    // 3. Envelope zero-drag click -> emits EnsureEnvelopeOnObject (not pinched cage)
    let env_id = h.add_rect("EnvRect", 100.0, 20.0, 40.0, 30.0);
    h.selection.clear();
    tc.set_active_tool(ToolId::Envelope);
    let e_pt = Point2D::new(120.0, 35.0);
    tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(e_pt, [0.0, 0.0]));
    let env_up = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(e_pt, [0.0, 0.0]));
    match env_up {
        ToolOutput::Consumed { actions, .. } => {
            assert!(
                actions.contains(&ToolActionRequest::EnsureEnvelopeOnObject { object_id: env_id })
            );
        }
        other => panic!("expected EnsureEnvelopeOnObject, got {other:?}"),
    }

    // 4. FreehandPick single click -> selects hit object instead of empty lasso clear
    tc.set_active_tool(ToolId::FreehandPick);
    tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(e_pt, [0.0, 0.0]));
    let fp_up = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(e_pt, [0.0, 0.0]));
    match fp_up {
        ToolOutput::Consumed { actions, .. } => {
            assert!(actions.iter().any(|a| matches!(
                a,
                ToolActionRequest::SelectObjects { ids, .. } if ids == &vec![env_id]
            )));
        }
        other => panic!("expected SelectObjects on FreehandPick click, got {other:?}"),
    }
}

#[test]
fn w1c_r2_grouped_world_transform_handles_across_interactive_tools() {
    use coree_core::{EnvelopeMode, MeshNode, TransparencyMode};
    use coree_tools::{try_insert_gradient_stop_on_ray_world, ActiveDragState};

    let mut h = Harness::new();
    let mut child = SceneObject::new(
        ObjectId(0),
        "GroupedChild",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 40.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::LinearGradient {
        start: Point2D::new(0.0, 10.0),
        end: Point2D::new(40.0, 10.0),
        stops: vec![
            GradientStop::new(0.0, Color::CYAN),
            GradientStop::new(1.0, Color::MAGENTA),
        ],
    });
    child.effects.push(Effect::DropShadow {
        dx_mm: 5.0,
        dy_mm: 5.0,
        blur_mm: 3.0,
        opacity: 0.6,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Middle,
    });
    child.effects.push(Effect::Transparency {
        mode: TransparencyMode::FountainLinear {
            start: Point2D::new(0.0, 10.0),
            end: Point2D::new(40.0, 10.0),
            start_alpha: 1.0,
            end_alpha: 0.2,
        },
        blend_mode: coree_core::BlendMode::Normal,
    });
    child.effects.push(Effect::Envelope {
        mode: EnvelopeMode::Unconstrained,
        cage: vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(20.0, 0.0),
            Point2D::new(40.0, 0.0),
            Point2D::new(40.0, 10.0),
            Point2D::new(40.0, 20.0),
            Point2D::new(20.0, 20.0),
            Point2D::new(0.0, 20.0),
            Point2D::new(0.0, 10.0),
        ],
    });
    let lid = Some(h.layer_id());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(child, lid))
        .unwrap();
    let cid = *h.doc.objects().keys().max().unwrap();
    let sib = h.add_rect("Sib", 0.0, 30.0, 10.0, 10.0);
    h.history
        .execute(
            &mut h.doc,
            CommandOp::Group {
                ids: vec![cid, sib],
                name: "Grp".to_owned(),
            },
        )
        .unwrap();
    let gid = *h.doc.objects().keys().max().unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::TransformObjects {
                ids: vec![gid],
                affine: Affine2D::translate(100.0, 100.0),
            },
        )
        .unwrap();
    h.selection.select_single(&h.doc, cid);

    // 1. InteractiveFill end handle in world space: (140, 110)
    let mut tc = ToolCore::new(ToolId::InteractiveFill);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(140.0, 110.0), [0.0, 0.0]),
    );
    assert!(matches!(
        tc.drag_state,
        ActiveDragState::DraggingGradientHandle { is_end: true, .. }
    ));
    let _ = tc.cancel();

    // Double-click gradient stop insertion in world space: (120, 110)
    assert!(try_insert_gradient_stop_on_ray_world(
        &h.doc,
        &h.viewport,
        h.doc.object(cid).unwrap(),
        Point2D::new(120.0, 110.0),
    )
    .is_some());

    // 2. Transparency handle in world space: (140, 110)
    tc.set_active_tool(ToolId::Transparency);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(140.0, 110.0), [0.0, 0.0]),
    );
    assert!(matches!(
        tc.drag_state,
        ActiveDragState::DraggingTransparencyHandle { is_end: true, .. }
    ));
    let _ = tc.cancel();

    // 3. Envelope handle in world space: (140, 120)
    tc.set_active_tool(ToolId::Envelope);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(140.0, 120.0), [0.0, 0.0]),
    );
    assert!(matches!(
        tc.drag_state,
        ActiveDragState::DraggingEnvelopeHandle { .. }
    ));
    let _ = tc.cancel();

    // 4. DropShadow knob in world space: center (120, 110) + (5, 5) = (125, 115) -> drag to (135, 125)
    tc.set_active_tool(ToolId::DropShadow);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(125.0, 115.0), [0.0, 0.0]),
    );
    assert!(matches!(
        tc.drag_state,
        ActiveDragState::DraggingEffectHandle { .. }
    ));
    tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(135.0, 125.0), [0.0, 0.0]),
    );
    let ds_up = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(135.0, 125.0), [0.0, 0.0]),
    );
    h.commit(ds_up);
    let has_15_15 = h.doc.object(cid).unwrap().effects.iter().any(|e| {
        matches!(
            e,
            Effect::DropShadow { dx_mm, dy_mm, .. }
                if (*dx_mm - 15.0).abs() < 1e-4 && (*dy_mm - 15.0).abs() < 1e-4
        )
    });
    assert!(
        has_15_15,
        "DropShadow on grouped child must update to (15, 15)"
    );

    // 5. MeshFill node in world space: (140, 120)
    let mesh_fill = Fill::MeshFill {
        rows: 2,
        cols: 2,
        nodes: vec![
            MeshNode {
                row: 0,
                col: 0,
                pos: Point2D::new(0.0, 0.0),
                color: Color::CYAN,
            },
            MeshNode {
                row: 0,
                col: 1,
                pos: Point2D::new(40.0, 0.0),
                color: Color::MAGENTA,
            },
            MeshNode {
                row: 1,
                col: 0,
                pos: Point2D::new(0.0, 20.0),
                color: Color::YELLOW,
            },
            MeshNode {
                row: 1,
                col: 1,
                pos: Point2D::new(40.0, 20.0),
                color: Color::BLACK,
            },
        ],
    };
    h.history
        .execute(
            &mut h.doc,
            CommandOp::SetFill {
                ids: vec![cid],
                fill: mesh_fill,
            },
        )
        .unwrap();
    tc.set_active_tool(ToolId::MeshFill);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(140.0, 120.0), [0.0, 0.0]),
    );
    assert!(matches!(
        tc.drag_state,
        ActiveDragState::DraggingMeshNode { node_index: 3, .. }
    ));
}

#[test]
fn w1c_r2_escape_clears_three_point_bezier_and_crop_drafts() {
    let h = Harness::new();
    let esc = KeyEvent {
        key: ToolKey::Escape,
        modifiers: ModifiersState::default(),
    };

    // 1. ThreePointCurve stage 2
    let mut tc = ToolCore::new(ToolId::ThreePointCurve);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(10.0, 10.0), [0.0, 0.0]),
    );
    tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(60.0, 10.0), [50.0, 0.0]),
    );
    tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(60.0, 10.0), [50.0, 0.0]),
    );
    assert!(tc.has_active_draft_or_drag());
    assert_eq!(tc.on_key(&h.ctx(), &esc), ToolOutput::Cancel);
    assert!(!tc.has_active_draft_or_drag());
    assert!(tc.three_point_curve_chord.is_none());

    // 2. Bezier multi-point draft
    tc.set_active_tool(ToolId::Bezier);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(10.0, 10.0), [0.0, 0.0]),
    );
    tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(10.0, 10.0), [0.0, 0.0]),
    );
    assert!(tc.has_active_draft_or_drag());
    assert_eq!(tc.on_key(&h.ctx(), &esc), ToolOutput::Cancel);
    assert!(!tc.has_active_draft_or_drag());
    assert!(tc.bezier_draft_nodes.is_empty());

    // 3. Crop pending rect
    tc.set_active_tool(ToolId::Crop);
    tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(10.0, 10.0), [0.0, 0.0]),
    );
    tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(80.0, 80.0), [70.0, 70.0]),
    );
    tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(80.0, 80.0), [70.0, 70.0]),
    );
    assert!(tc.has_active_draft_or_drag());
    assert_eq!(tc.on_key(&h.ctx(), &esc), ToolOutput::Cancel);
    assert!(!tc.has_active_draft_or_drag());
    assert!(tc.pending_crop_rect.is_none());
}

#[test]
fn w1c_r3_powerclip_edit_mode_unselected_hit_tests_target_inner_child() {
    use coree_tools::{
        build_interactive_fill_op, build_mesh_fill_output, find_nearest_object_anchor_in_doc,
        ActiveDragState,
    };

    let mut h = Harness::new();
    let container_id = h.add_rect("PCContainer", 10.0, 10.0, 100.0, 100.0);
    let inner_id = h.add_rect("InnerRect", 30.0, 30.0, 40.0, 40.0);
    h.history
        .execute(
            &mut h.doc,
            CommandOp::CreatePowerClip {
                container_id,
                content_ids: vec![inner_id],
            },
        )
        .unwrap();
    h.selection.clear();

    let mut ctx = h.ctx();
    ctx.powerclip_edit_id = Some(container_id);
    let inner_center = Point2D::new(50.0, 50.0);
    let inner_corner = Point2D::new(30.0, 30.0);

    // 1. Envelope zero-drag click targets inner_id, NOT container_id
    let mut tc = ToolCore::new(ToolId::Envelope);
    tc.on_pointer_down(&ctx, &PointerEvent::primary(inner_center, [0.0, 0.0]));
    match tc.on_pointer_up(&ctx, &PointerEvent::primary(inner_center, [0.0, 0.0])) {
        ToolOutput::Consumed { actions, .. } => {
            assert!(
                actions.contains(&ToolActionRequest::EnsureEnvelopeOnObject {
                    object_id: inner_id
                }),
                "Expected EnsureEnvelopeOnObject for inner child {inner_id:?}, got {actions:?}"
            );
            assert!(
                actions.iter().any(|a| matches!(
                    a,
                    ToolActionRequest::SelectObjects { ids, .. } if ids == &vec![inner_id]
                )),
                "Expected SelectObjects([{inner_id:?}]), got {actions:?}"
            );
        }
        other => panic!("Expected Consumed for PowerClip Envelope click, got {other:?}"),
    }

    // 2. Sculpt (Smear) unselected click near inner corner targets inner_id
    tc.set_active_tool(ToolId::Smear);
    tc.on_pointer_down(&ctx, &PointerEvent::primary(inner_corner, [0.0, 0.0]));
    assert!(
        matches!(
            tc.drag_state,
            ActiveDragState::Sculpting { object_id, .. } if object_id == inner_id
        ),
        "Expected Sculpting on inner child {inner_id:?}, got {:?}",
        tc.drag_state
    );
    let _ = tc.cancel();

    // 3. Transparency / DropShadow / FreeTransform unselected clicks target inner_id
    for tool in [ToolId::Transparency, ToolId::DropShadow] {
        tc.set_active_tool(tool);
        tc.on_pointer_down(&ctx, &PointerEvent::primary(inner_center, [0.0, 0.0]));
        assert!(
            matches!(
                tc.drag_state,
                ActiveDragState::DraggingInteractiveEffect { object_id, .. } if object_id == inner_id
            ),
            "Expected {tool:?} to target inner child {inner_id:?}, got {:?}",
            tc.drag_state
        );
        let _ = tc.cancel();
    }

    tc.set_active_tool(ToolId::FreeTransform);
    tc.on_pointer_down(&ctx, &PointerEvent::primary(inner_center, [0.0, 0.0]));
    assert!(
        matches!(
            tc.drag_state,
            ActiveDragState::FreeTransforming { ref orig_transforms, .. }
                if orig_transforms.iter().any(|(id, _)| *id == inner_id)
        ),
        "Expected FreeTransform on inner child {inner_id:?}, got {:?}",
        tc.drag_state
    );
    let _ = tc.cancel();

    // 4. InteractiveFill / MeshFill builders target inner_id
    match build_interactive_fill_op(&ctx, inner_center, Point2D::new(65.0, 50.0)) {
        Some(CommandOp::SetFill { ids, .. }) => assert_eq!(ids, vec![inner_id]),
        other => panic!("Expected SetFill on inner child, got {other:?}"),
    }
    match build_mesh_fill_output(&ctx, inner_center, Point2D::new(60.0, 60.0), false) {
        ToolOutput::Commit { ops, .. } => match ops.first() {
            Some(CommandOp::SetFill { ids, .. }) => assert_eq!(ids, &vec![inner_id]),
            other => panic!("Expected SetFill op on inner child, got {other:?}"),
        },
        other => panic!("Expected Commit for MeshFill on inner child, got {other:?}"),
    }

    // 5. Connector anchor snap inside PowerClip edit mode resolves inner_id
    let snap = find_nearest_object_anchor_in_doc(
        ctx.document,
        ctx.document.active_page_index(),
        ctx.powerclip_edit_id,
        ctx.treat_all_objects_as_filled,
        inner_center,
        8.0,
    );
    assert_eq!(snap.map(|(id, _, _)| id), Some(inner_id));
}

#[test]
fn w1c_r3_scaled_group_distort_knob_tracks_pointer_and_overlay() {
    use coree_core::DistortKind;
    use coree_tools::ActiveDragState;

    let mut h = Harness::new();
    let mut child = SceneObject::new(
        ObjectId(0),
        "DistortChild",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 20.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::CYAN))
    .with_transform(Affine2D::translate(10.0, 0.0));
    child.effects.push(Effect::Distort {
        kind: DistortKind::PushPull,
        amplitude: 5.0,
        frequency: 2.0,
        center: Point2D::new(10.0, 10.0),
    });
    let lid = Some(h.layer_id());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(child, lid))
        .unwrap();
    let cid = *h.doc.objects().keys().max().unwrap();

    let sib = h.add_rect("Sib", 0.0, 30.0, 5.0, 5.0);
    h.history
        .execute(
            &mut h.doc,
            CommandOp::Group {
                ids: vec![cid, sib],
                name: "Scaled2xGrp".to_owned(),
            },
        )
        .unwrap();
    let gid = *h.doc.objects().keys().max().unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::TransformObjects {
                ids: vec![gid],
                affine: Affine2D::scale(2.0, 2.0),
            },
        )
        .unwrap();
    h.selection.select_single(&h.doc, cid);

    // Local center (10, 10) + child translate (10, 0) = (20, 10); 2x group scale -> world center (40, 20)
    // Local amplitude 5.0 -> world knob at (20 + 5) * 2 = 50.0, y = 20.0
    let knob_start = Point2D::new(50.0, 20.0);
    let target_w = Point2D::new(56.0, 20.0);

    let mut tc = ToolCore::new(ToolId::Distort);
    tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(knob_start, [0.0, 0.0]));
    assert!(
        matches!(tc.drag_state, ActiveDragState::DraggingEffectHandle { .. }),
        "Clicking scaled Distort knob at {knob_start:?} must enter DraggingEffectHandle, got {:?}",
        tc.drag_state
    );
    let move_out = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(target_w, [6.0, 0.0]));
    let overlay = match move_out {
        ToolOutput::Continue { overlay } => overlay,
        other => panic!("Expected Continue with overlay, got {other:?}"),
    };
    assert!(
        overlay.preview_handles.len() >= 2,
        "Expected anchor + knob handles, got {:?}",
        overlay.preview_handles
    );
    let knob_pos = overlay.preview_handles[1].pos_mm;
    assert!(
        knob_pos.distance_to(target_w) < 1e-3,
        "Scaled Distort knob must track target_w={target_w:?} within 1e-3 mm, got {knob_pos:?}"
    );
    let up_out = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(target_w, [0.0, 0.0]));
    h.commit(up_out);

    // Also verify fresh Distort creation on a 2x-scaled group child computes local amplitude
    let fresh_child = h.add_rect("FreshDistort", 10.0, 0.0, 20.0, 20.0);
    h.history
        .execute(
            &mut h.doc,
            CommandOp::TransformObjects {
                ids: vec![fresh_child],
                affine: Affine2D::scale(2.0, 2.0),
            },
        )
        .unwrap();
    h.selection.select_single(&h.doc, fresh_child);
    let out_fresh = coree_tools::build_interactive_effect_output(
        &h.ctx(),
        ToolId::Distort,
        Point2D::new(40.0, 20.0),
        Point2D::new(56.0, 20.0),
    );
    h.commit(out_fresh);
    let amp = h
        .doc
        .object(fresh_child)
        .unwrap()
        .effects
        .iter()
        .find_map(|e| match e {
            Effect::Distort { amplitude, .. } => Some(*amplitude),
            _ => None,
        })
        .unwrap();
    assert!(
        (amp - 8.0).abs() < 1e-3,
        "Expected local amplitude 8.0 on 2x-scaled object for 16mm world drag, got {amp}"
    );
}

#[test]
fn w1c_r4_open_paths_ignored_when_treat_all_objects_as_filled_and_powerclip_anchor() {
    use coree_core::Rect2D;
    use coree_tools::{find_nearest_object_anchor_in_doc, hit_test_canvas_point_in_doc};

    let mut h = Harness::new();
    let lid = Some(h.layer_id());

    // 1. Unfilled closed rect behind an open vertical line (40, 10) -> (40, 70)
    let closed_behind = SceneObject::new(
        ObjectId(0),
        "UnfilledClosedRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            80.0, 20.0, 40.0, 40.0,
        ))),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(closed_behind, lid))
        .unwrap();
    let closed_behind_id = *h.doc.objects().keys().max().unwrap();

    let open_line = SceneObject::new(
        ObjectId(0),
        "OpenVerticalLine",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(40.0, 10.0),
            Point2D::new(40.0, 70.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(open_line, lid))
        .unwrap();
    let open_line_id = *h.doc.objects().keys().max().unwrap();

    // Far off-stroke open space must not hit the open line even with treat_all_objects_as_filled = true
    assert_eq!(
        hit_test_canvas_point_in_doc(&h.doc, None, true, Point2D::new(500.0, 40.0), 3.0),
        None
    );
    // On-stroke click still hits the open line
    assert_eq!(
        hit_test_canvas_point_in_doc(&h.doc, None, true, Point2D::new(40.0, 40.0), 3.0),
        Some(open_line_id)
    );
    // Unfilled closed rect behind the open line is NOT masked at (100, 40)
    assert_eq!(
        hit_test_canvas_point_in_doc(&h.doc, None, true, Point2D::new(100.0, 40.0), 3.0),
        Some(closed_behind_id)
    );

    // Pick click on empty canvas at (180, 40) must emit ClearSelection
    h.selection.select_single(&h.doc, open_line_id);
    let mut tc = ToolCore::new(ToolId::Pick);
    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(180.0, 40.0), [0.0, 0.0]),
    );
    let pick_up = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(180.0, 40.0), [0.0, 0.0]),
    );
    match pick_up {
        ToolOutput::Consumed { actions, .. } => assert!(
            actions
                .iter()
                .any(|a| matches!(a, ToolActionRequest::ClearSelection)),
            "Expected ClearSelection on empty canvas click at (180, 40), got {actions:?}"
        ),
        other => panic!("Expected Consumed with ClearSelection, got {other:?}"),
    }

    // 2. PowerClip edit mode: open cutter line inside PowerClip must not hijack off-stroke hit-test or connector anchor snap
    let inner_rect = SceneObject::new(
        ObjectId(0),
        "InnerRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 40.0, 40.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::CYAN))
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(inner_rect, lid))
        .unwrap();
    let inner_id = *h.doc.objects().keys().max().unwrap();

    // Open vertical line inside PowerClip (world (40, 10) -> (40, 70) when container at (20, 20))
    let inner_cutter = SceneObject::new(
        ObjectId(0),
        "InnerCutter",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(20.0, -10.0),
            Point2D::new(20.0, 50.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(inner_cutter, lid))
        .unwrap();
    let cutter_id = *h.doc.objects().keys().max().unwrap();

    let pc_obj = SceneObject::new(
        ObjectId(0),
        "PowerClipBox",
        NodePayload::PowerClip {
            container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, -10.0, 100.0, 70.0,
            ))),
            container_parametric: None,
            contents: vec![inner_id, cutter_id],
            lock_contents: false,
        },
    )
    .with_fill(Fill::Solid(Color::WHITE))
    .with_outline(OutlineStyle::default())
    .with_transform(Affine2D::translate(20.0, 20.0));
    h.history
        .execute(&mut h.doc, CommandOp::create_object(pc_obj, lid))
        .unwrap();
    let pc_id = *h.doc.objects().keys().max().unwrap();

    // Point (107.5, 57.5) is inside pc_id container (world 20..120 x 10..80), well to the right of cutter_id at x=40.
    let probe_pt = Point2D::new(107.5, 57.5);
    assert_eq!(
        hit_test_canvas_point_in_doc(&h.doc, Some(pc_id), true, probe_pt, 3.0),
        Some(pc_id)
    );
    let anchor = find_nearest_object_anchor_in_doc(
        &h.doc,
        h.doc.active_page_index(),
        Some(pc_id),
        true,
        probe_pt,
        8.0,
    );
    assert_ne!(
        anchor.map(|(id, _, _)| id),
        Some(cutter_id),
        "Connector anchor snap in PowerClip edit mode must ignore off-stroke open path {cutter_id:?}"
    );
    assert_eq!(anchor.map(|(id, _, _)| id), Some(pc_id));
}

#[test]
fn w1c_r4_scaled_and_rotated_group_distort_knob_tracks_high_amplitude_40_to_120() {
    use coree_core::DistortKind;
    use coree_tools::ActiveDragState;

    for angle_deg in [0.0_f64, 45.0_f64] {
        let mut h = Harness::new();
        let mut child = SceneObject::new(
            ObjectId(0),
            "HighAmpDistortChild",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 20.0,
                height: 20.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::CYAN))
        .with_transform(Affine2D::translate(10.0, 0.0));
        child.effects.push(Effect::Distort {
            kind: DistortKind::PushPull,
            amplitude: 60.0,
            frequency: 2.0,
            center: Point2D::new(10.0, 10.0),
        });
        let lid = Some(h.layer_id());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(child, lid))
            .unwrap();
        let cid = *h.doc.objects().keys().max().unwrap();

        let sib = h.add_rect("Sib", 0.0, 30.0, 5.0, 5.0);
        h.history
            .execute(
                &mut h.doc,
                CommandOp::Group {
                    ids: vec![cid, sib],
                    name: "ScaledHighAmpGrp".to_owned(),
                },
            )
            .unwrap();
        let gid = *h.doc.objects().keys().max().unwrap();
        let grp_xf = if angle_deg.abs() > 1e-6 {
            Affine2D::rotate_deg(angle_deg).compose(&Affine2D::scale(2.0, 2.0))
        } else {
            Affine2D::scale(2.0, 2.0)
        };
        h.history
            .execute(
                &mut h.doc,
                CommandOp::TransformObjects {
                    ids: vec![gid],
                    affine: grp_xf,
                },
            )
            .unwrap();
        h.selection.select_single(&h.doc, cid);

        let world_xf = h.doc.world_transform_of(cid).unwrap();
        let local_center = Point2D::new(10.0, 10.0);
        let knob_start =
            world_xf.transform_point(Point2D::new(local_center.x + 60.0, local_center.y));

        let mut tc = ToolCore::new(ToolId::Distort);
        tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(knob_start, [0.0, 0.0]));
        assert!(
            matches!(tc.drag_state, ActiveDragState::DraggingEffectHandle { .. }),
            "[angle={angle_deg}] Clicking high-amp Distort knob at {knob_start:?} must enter DraggingEffectHandle, got {:?}",
            tc.drag_state
        );

        for target_local_amp in [55.0_f64, 110.0_f64] {
            let target_w = world_xf.transform_point(Point2D::new(
                local_center.x + target_local_amp,
                local_center.y,
            ));
            let move_out =
                tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(target_w, [0.0, 0.0]));
            let overlay = match move_out {
                ToolOutput::Continue { overlay } => overlay,
                other => {
                    panic!("[angle={angle_deg}] Expected Continue with overlay, got {other:?}")
                }
            };
            let knob_pos = overlay.preview_handles[1].pos_mm;
            assert!(
                knob_pos.distance_to(target_w) < 1e-3,
                "[angle={angle_deg}, amp={target_local_amp}] Distort knob {knob_pos:?} must track {target_w:?} within 1e-3 mm (err={})",
                knob_pos.distance_to(target_w)
            );
        }

        let final_target_w =
            world_xf.transform_point(Point2D::new(local_center.x + 110.0, local_center.y));
        let up_out = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(final_target_w, [0.0, 0.0]));
        h.commit(up_out);

        let committed_amp = h
            .doc
            .object(cid)
            .unwrap()
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::Distort { amplitude, .. } => Some(*amplitude),
                _ => None,
            })
            .unwrap();
        assert!(
            (committed_amp - 110.0).abs() < 1e-3,
            "[angle={angle_deg}] Expected committed local amplitude 110.0, got {committed_amp}"
        );
    }
}

#[test]
fn w1c_r6_sculpt_drag_over_threshold_outside_node_radius_preserves_parametric_shape() {
    let mut h = Harness::new();
    h.settings.sculpt_radius_mm = 10.0;
    // 60x60 ParametricShape::Rectangle at (20, 20)..(80, 80); center (50, 50) is > 42mm from all 4 corners
    let rect_id = h.add_rect("SculptParamRect", 20.0, 20.0, 60.0, 60.0);
    h.selection.select_single(&h.doc, rect_id);
    let orig_payload = h.doc.object(rect_id).unwrap().payload.clone();
    let undo_len_before = h.history.undo_labels().len();

    let start = Point2D::new(50.0, 50.0);
    let end = Point2D::new(52.0, 50.0); // 2.0mm >= 0.35mm -> increments step_counter > 0

    for tool in [
        ToolId::Smear,
        ToolId::Smooth,
        ToolId::Twirl,
        ToolId::AttractRepel,
        ToolId::Roughen,
    ] {
        let mut tc = ToolCore::new(tool);
        let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(start, [0.0, 0.0]));
        let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(end, [2.0, 0.0]));
        let up = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(end, [2.0, 0.0]));
        assert_eq!(
            up,
            ToolOutput::noop(),
            "[{tool:?}] >=0.35mm drag outside sculpt_radius_mm of all corners must return ToolOutput::noop()"
        );
        assert_eq!(
            h.doc.object(rect_id).unwrap().payload,
            orig_payload,
            "[{tool:?}] ParametricShape must remain intact"
        );
        assert_eq!(h.history.undo_labels().len(), undo_len_before);
    }

    // Sibling VirtualSegmentDelete click in middle of filled ParametricShape & disjoint Eraser drag must also be noop
    let mut tc_vsd = ToolCore::new(ToolId::VirtualSegmentDelete);
    let _ = tc_vsd.on_pointer_down(&h.ctx(), &PointerEvent::primary(start, [0.0, 0.0]));
    assert_eq!(
        tc_vsd.on_pointer_up(&h.ctx(), &PointerEvent::primary(start, [0.0, 0.0])),
        ToolOutput::noop()
    );
    let mut tc_era = ToolCore::new(ToolId::Eraser);
    let far_a = Point2D::new(200.0, 200.0);
    let far_b = Point2D::new(210.0, 200.0);
    let _ = tc_era.on_pointer_down(&h.ctx(), &PointerEvent::primary(far_a, [0.0, 0.0]));
    let _ = tc_era.on_pointer_move(&h.ctx(), &PointerEvent::primary(far_b, [10.0, 0.0]));
    assert_eq!(
        tc_era.on_pointer_up(&h.ctx(), &PointerEvent::primary(far_b, [10.0, 0.0])),
        ToolOutput::noop()
    );
    assert_eq!(h.doc.object(rect_id).unwrap().payload, orig_payload);

    // Positive control: >=0.35mm Smear drag near top-left corner (20, 20) DOES deform & commit [ConvertToCurves, EditPath]
    let mut tc_smear = ToolCore::new(ToolId::Smear);
    let c0 = Point2D::new(20.0, 20.0);
    let c1 = Point2D::new(23.0, 22.0);
    let _ = tc_smear.on_pointer_down(&h.ctx(), &PointerEvent::primary(c0, [0.0, 0.0]));
    let _ = tc_smear.on_pointer_move(&h.ctx(), &PointerEvent::primary(c1, [3.0, 2.0]));
    let smear_up = tc_smear.on_pointer_up(&h.ctx(), &PointerEvent::primary(c1, [3.0, 2.0]));
    h.commit(smear_up);
    assert!(
        matches!(h.doc.object(rect_id).unwrap().payload, NodePayload::Path(_)),
        "Real sculpt drag near corner must convert and edit path"
    );
}

#[test]
fn w1c_r7_eraser_empty_aabb_corner_of_rotated_parametric_rect_is_noop() {
    let mut h = Harness::new();
    h.settings.eraser_radius_mm = 2.0;

    // 60x10 rectangle rotated 45 deg at (150, 150) -> large empty AABB corners
    let mut obj = SceneObject::new(
        ObjectId(0),
        "RotatedParamRect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 60.0,
            height: 10.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_transform(Affine2D::translate(150.0, 150.0).compose(&Affine2D::rotate_deg(45.0)));
    obj.fill = Fill::Solid(Color::CYAN);
    let lid = h.layer_id();
    h.history
        .execute(&mut h.doc, CommandOp::create_object(obj, Some(lid)))
        .unwrap();
    let rect_id = *h.doc.objects().keys().max().unwrap();
    let orig_payload = h.doc.object(rect_id).unwrap().payload.clone();
    let bb = h.doc.world_object_bounds(rect_id).unwrap();

    // Empty AABB corner near (bb.max_x - 3, bb.min_y + 3): inside AABB, > 20mm from rotated rect
    let empty_a = Point2D::new(bb.max_x - 4.0, bb.min_y + 2.0);
    let empty_b = Point2D::new(bb.max_x - 1.0, bb.min_y + 5.0);
    assert!(bb.contains_point(empty_a) && bb.contains_point(empty_b));

    // 1. Selected rotated ParametricShape::Rectangle: empty AABB corner drag must be noop
    h.selection.select_single(&h.doc, rect_id);
    let undo_before = h.history.undo_labels().len();
    let mut tc = ToolCore::new(ToolId::Eraser);
    let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(empty_a, [0.0, 0.0]));
    let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(empty_b, [3.0, 3.0]));
    let up_sel = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(empty_b, [3.0, 3.0]));
    assert_eq!(
        up_sel,
        ToolOutput::noop(),
        "Eraser in empty AABB corner of selected rotated rect must be noop"
    );
    assert_eq!(h.doc.object(rect_id).unwrap().payload, orig_payload);
    assert_eq!(h.history.undo_labels().len(), undo_before);

    // 2. Unselected rotated ParametricShape::Rectangle: empty AABB corner drag must also be noop
    h.selection.clear();
    let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(empty_a, [0.0, 0.0]));
    let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(empty_b, [3.0, 3.0]));
    let up_unsel = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(empty_b, [3.0, 3.0]));
    assert_eq!(
        up_unsel,
        ToolOutput::noop(),
        "Eraser in empty AABB corner of unselected rotated rect must be noop"
    );
    assert_eq!(h.doc.object(rect_id).unwrap().payload, orig_payload);

    // 3. Positive control: dragging across the actual rotated rectangle edge/interior DOES erase
    h.selection.select_single(&h.doc, rect_id);
    let xf = h.doc.world_transform_of(rect_id).unwrap();
    let hit_a = xf.transform_point(Point2D::new(30.0, -2.0));
    let hit_b = xf.transform_point(Point2D::new(30.0, 12.0));
    let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(hit_a, [0.0, 0.0]));
    let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(hit_b, [0.0, 14.0]));
    let up_hit = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(hit_b, [0.0, 14.0]));
    assert!(
        matches!(up_hit, ToolOutput::Commit { .. }),
        "Real eraser drag across rotated rect must commit, got {up_hit:?}"
    );
    h.commit(up_hit);
    assert!(
        matches!(h.doc.object(rect_id).unwrap().payload, NodePayload::Path(_)),
        "Real eraser drag must convert and slice rotated rect"
    );
    let area_after = match &h.doc.object(rect_id).unwrap().payload {
        NodePayload::Path(p) => p.area_mm2(),
        _ => unreachable!(),
    };
    assert!(
        area_after < 600.0 - 20.0,
        "Erased rect area {area_after} must be significantly smaller than 600 mm^2"
    );
}

#[test]
fn w1c_r9_vsd_two_point_drag_and_three_point_or_straddling_swipes() {
    let mut h = Harness::new();
    let lid = Some(h.layer_id());

    // 1. 2-point drag starting outside tolerance (y=95) and releasing directly on virtual segment (y=100)
    // HLine (10..70, 100) split by VLine (40, 70..130); viewport zoom=2.0 -> tol = 4.0mm.
    let hline_obj = SceneObject::new(
        ObjectId(0),
        "HLine",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(10.0, 100.0),
            Point2D::new(70.0, 100.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(hline_obj, lid))
        .unwrap();
    let h_id = *h.doc.objects().keys().max().unwrap();

    let vline_obj = SceneObject::new(
        ObjectId(0),
        "VLine",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(40.0, 70.0),
            Point2D::new(40.0, 130.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(vline_obj, lid))
        .unwrap();
    let v_id = *h.doc.objects().keys().max().unwrap();

    let orig_h_payload = h.doc.object(h_id).unwrap().payload.clone();

    let mut tc = ToolCore::new(ToolId::VirtualSegmentDelete);
    let p_out = Point2D::new(55.0, 95.0);
    let p_on = Point2D::new(55.0, 100.0);
    let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(p_out, [0.0, 0.0]));
    let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(p_on, [0.0, 5.0]));
    let out_2pt = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(p_on, [0.0, 5.0]));
    match &out_2pt {
        ToolOutput::Commit { ops, .. } => {
            assert_eq!(ops.len(), 1);
            assert!(matches!(ops[0], CommandOp::EditPath { id, .. } if id == h_id));
        }
        other => panic!("Expected Commit on 2-pt VSD drag ending on segment, got {other:?}"),
    }
    h.commit(out_2pt);
    match &h.doc.object(h_id).unwrap().payload {
        NodePayload::Path(p) => {
            assert_eq!(p.subpaths.len(), 1);
            let xs: Vec<f64> = p.subpaths[0].nodes.iter().map(|n| n.pos.x).collect();
            assert!(
                (xs[0] - 10.0).abs() < 1e-4 && (xs[xs.len() - 1] - 40.0).abs() < 1e-4,
                "Right half (40..70) must be deleted leaving (10..40), got xs={xs:?}"
            );
        }
        other => panic!("expected Path, got {other:?}"),
    }
    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(h_id).unwrap().payload, orig_h_payload);

    // 2. 3-point swipe crossing virtual segment at points[1]
    let h2_obj = SceneObject::new(
        ObjectId(0),
        "HLine2",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(10.0, 150.0),
            Point2D::new(70.0, 150.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(h2_obj, lid))
        .unwrap();
    let h2_id = *h.doc.objects().keys().max().unwrap();

    let v2_obj = SceneObject::new(
        ObjectId(0),
        "VLine2",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(40.0, 135.0),
            Point2D::new(40.0, 165.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(v2_obj, lid))
        .unwrap();

    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(55.0, 145.0), [0.0, 0.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(55.0, 150.0), [0.0, 5.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(55.0, 155.0), [0.0, 10.0]),
    );
    let out_3pt = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(55.0, 155.0), [0.0, 10.0]),
    );
    assert!(
        matches!(out_3pt, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
        "3-point VSD swipe crossing at points[1] must commit, got {out_3pt:?}"
    );
    h.commit(out_3pt);
    match &h.doc.object(h2_id).unwrap().payload {
        NodePayload::Path(p) => {
            assert_eq!(p.subpaths.len(), 1);
            let xs: Vec<f64> = p.subpaths[0].nodes.iter().map(|n| n.pos.x).collect();
            assert!((xs[0] - 10.0).abs() < 1e-4 && (xs[xs.len() - 1] - 40.0).abs() < 1e-4);
        }
        other => panic!("expected Path, got {other:?}"),
    }
    h.history.undo(&mut h.doc).unwrap();

    // 3. Fast 2-point swipe straddling h_id (both endpoints far outside tol=4mm)
    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(55.0, 80.0), [0.0, 0.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(55.0, 120.0), [0.0, 40.0]),
    );
    let out_fast = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(55.0, 120.0), [0.0, 40.0]),
    );
    assert!(
        matches!(out_fast, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
        "Fast 2-point straddling swipe must resample & commit, got {out_fast:?}"
    );
    h.commit(out_fast);
    match &h.doc.object(h_id).unwrap().payload {
        NodePayload::Path(p) => {
            assert_eq!(p.subpaths.len(), 1);
            let xs: Vec<f64> = p.subpaths[0].nodes.iter().map(|n| n.pos.x).collect();
            assert!((xs[0] - 10.0).abs() < 1e-4 && (xs[xs.len() - 1] - 40.0).abs() < 1e-4);
        }
        other => panic!("expected Path, got {other:?}"),
    }
    h.history.undo(&mut h.doc).unwrap();

    // 4. Single-click (points.len() == 1) and no-op swipe continue to behave cleanly
    let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(p_on, [0.0, 0.0]));
    let out_click = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(p_on, [0.0, 0.0]));
    h.commit(out_click);
    match &h.doc.object(h_id).unwrap().payload {
        NodePayload::Path(p) => {
            assert_eq!(p.subpaths.len(), 1);
            let xs: Vec<f64> = p.subpaths[0].nodes.iter().map(|n| n.pos.x).collect();
            assert!((xs[0] - 10.0).abs() < 1e-4 && (xs[xs.len() - 1] - 40.0).abs() < 1e-4);
        }
        other => panic!("expected Path, got {other:?}"),
    }
    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(150.0, 150.0), [0.0, 0.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(170.0, 170.0), [20.0, 20.0]),
    );
    assert_eq!(
        tc.on_pointer_up(
            &h.ctx(),
            &PointerEvent::primary(Point2D::new(170.0, 170.0), [20.0, 20.0])
        ),
        ToolOutput::noop()
    );
    let _ = v_id;
}

#[test]
fn w1c_r9_vsd_multi_segment_same_object_cumulative_and_undo_redo() {
    let mut h = Harness::new();
    let lid = Some(h.layer_id());

    // HLine (10..90, 100) split into 3 virtual segments by V1(35, 70..130) and V2(65, 70..130)
    let hline = SceneObject::new(
        ObjectId(0),
        "ThreeSegHLine",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(10.0, 100.0),
            Point2D::new(90.0, 100.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(hline, lid))
        .unwrap();
    let h_id = *h.doc.objects().keys().max().unwrap();

    for (name, x) in [("V1", 35.0), ("V2", 65.0)] {
        let vl = SceneObject::new(
            ObjectId(0),
            name,
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(x, 70.0),
                Point2D::new(x, 130.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::default());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(vl, lid))
            .unwrap();
    }
    let orig_h_payload = h.doc.object(h_id).unwrap().payload.clone();

    // U-shaped swipe hitting segments 1 (x=20) and 2 (x=50) without crossing V1(35) -> leaves (65..90)
    let mut tc = ToolCore::new(ToolId::VirtualSegmentDelete);
    let swipe = [
        Point2D::new(20.0, 100.0),
        Point2D::new(20.0, 60.0),
        Point2D::new(50.0, 60.0),
        Point2D::new(50.0, 100.0),
    ];
    let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(swipe[0], [0.0, 0.0]));
    for &p in &swipe[1..] {
        let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(p, [0.0, 0.0]));
    }
    let out = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(swipe[3], [0.0, 0.0]));
    match &out {
        ToolOutput::Commit { ops, .. } => {
            assert_eq!(
                ops.len(),
                1,
                "Multi-segment hit on same ObjectId must emit one cumulative op, got {ops:?}"
            );
            assert!(matches!(ops[0], CommandOp::EditPath { id, .. } if id == h_id));
        }
        other => panic!("Expected Commit, got {other:?}"),
    }
    h.commit(out);

    let after_path = match &h.doc.object(h_id).unwrap().payload {
        NodePayload::Path(p) => p.clone(),
        other => panic!("expected Path, got {other:?}"),
    };
    assert_eq!(after_path.subpaths.len(), 1);
    let xs: Vec<f64> = after_path.subpaths[0]
        .nodes
        .iter()
        .map(|n| n.pos.x)
        .collect();
    assert!(
        (xs[0] - 65.0).abs() < 1e-4 && (xs[xs.len() - 1] - 90.0).abs() < 1e-4,
        "Expected only third segment (65..90) to remain after deleting first two, got xs={xs:?}"
    );

    // Undo restores full (10..90); Redo reapplies deletion of both segments cleanly
    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(h_id).unwrap().payload, orig_h_payload);
    h.history.redo(&mut h.doc).unwrap();
    assert_eq!(
        h.doc.object(h_id).unwrap().payload,
        NodePayload::Path(after_path.clone())
    );

    // Straight swipe across both remaining/original segments when only 1 cutter exists deletes both
    h.history.undo(&mut h.doc).unwrap();
    let straight_ops = coree_tools::build_virtual_segment_delete_stroke_ops(
        &h.ctx(),
        &[
            Point2D::new(20.0, 100.0),
            Point2D::new(35.0, 100.0),
            Point2D::new(50.0, 100.0),
            Point2D::new(80.0, 100.0),
        ],
    );
    assert!(
        straight_ops.iter().any(|op| matches!(
            op,
            CommandOp::EditPath { id, path } if *id == h_id && path.subpaths.is_empty()
        )),
        "Straight swipe across all segments must delete all segments cumulatively, got {straight_ops:?}"
    );
}

#[test]
fn w1c_r9_vsd_parametric_rect_multi_segment_emits_single_convert_and_edit_with_undo_redo() {
    let mut h = Harness::new();
    let lid = Some(h.layer_id());

    // ParametricShape::Rectangle (20..80, 20..60) crossed by VLine(50, 10..70) and HLine(10..90, 40)
    let rect_id = h.add_rect("ParamRect", 20.0, 20.0, 60.0, 40.0);
    let orig_rect_payload = h.doc.object(rect_id).unwrap().payload.clone();
    assert!(matches!(
        orig_rect_payload,
        NodePayload::Shape(ParametricShape::Rectangle { .. })
    ));

    let v_cross = SceneObject::new(
        ObjectId(0),
        "VCross",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(50.0, 10.0),
            Point2D::new(50.0, 70.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(v_cross, lid))
        .unwrap();

    let h_cross = SceneObject::new(
        ObjectId(0),
        "HCross",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(10.0, 40.0),
            Point2D::new(90.0, 40.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(h_cross, lid))
        .unwrap();

    // Swipe across top-left virtual segment (35, 20) and top-right virtual segment (65, 20)
    // going above VCross(50, 10..70) via y=5 so VCross itself is untouched.
    let mut tc = ToolCore::new(ToolId::VirtualSegmentDelete);
    let swipe = [
        Point2D::new(35.0, 20.0),
        Point2D::new(35.0, 5.0),
        Point2D::new(65.0, 5.0),
        Point2D::new(65.0, 20.0),
    ];
    let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(swipe[0], [0.0, 0.0]));
    for &p in &swipe[1..] {
        let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(p, [0.0, 0.0]));
    }
    let out = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(swipe[3], [0.0, 0.0]));

    match &out {
        ToolOutput::Commit { ops, .. } => {
            assert_eq!(ops.len(), 1);
            match &ops[0] {
                CommandOp::Batch(inner) => {
                    let convert_count = inner
                        .iter()
                        .filter(
                            |o| matches!(o, CommandOp::ConvertToCurves { id } if *id == rect_id),
                        )
                        .count();
                    let edit_count = inner
                        .iter()
                        .filter(|o| matches!(o, CommandOp::EditPath { id, .. } if *id == rect_id))
                        .count();
                    assert_eq!(convert_count, 1, "ConvertToCurves must be emitted once");
                    assert_eq!(edit_count, 1, "EditPath must be emitted once");
                }
                other => panic!("Expected Batch([ConvertToCurves, EditPath]), got {other:?}"),
            }
        }
        other => panic!("Expected Commit, got {other:?}"),
    }
    h.commit(out);

    let edited_path = match &h.doc.object(rect_id).unwrap().payload {
        NodePayload::Path(p) => p.clone(),
        other => panic!("expected Path after ConvertToCurves + EditPath, got {other:?}"),
    };
    assert_eq!(edited_path.subpaths.len(), 1);
    assert!(!edited_path.subpaths[0].closed);
    // Both top-left (y=20, x<50) and top-right (y=20, x>50) segments were deleted,
    // leaving only the bottom portion starting/ending at world y=40 (HCross intersections).
    let world_edited = h.doc.world_path_of(rect_id).unwrap();
    let min_world_y = world_edited.subpaths[0]
        .nodes
        .iter()
        .map(|n| n.pos.y)
        .fold(f64::INFINITY, f64::min);
    assert!(
        min_world_y >= 40.0 - 1e-4,
        "Both top virtual segments must be deleted leaving only world y >= 40, got min_world_y={min_world_y}"
    );

    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(rect_id).unwrap().payload, orig_rect_payload);
    h.history.redo(&mut h.doc).unwrap();
    assert_eq!(
        h.doc.object(rect_id).unwrap().payload,
        NodePayload::Path(edited_path)
    );
}

#[test]
fn w1c_r10_vsd_cutter_isolation_pages_visibility_layers_and_powerclip() {
    use coree_core::{MasterLayerScope, Rect2D};

    let mut h = Harness::new();
    let p1_lid = h.layer_id();

    // Vertical line on Page 1 at x=50 that must NOT leak onto Page 2
    let v_page1 = SceneObject::new(
        ObjectId(0),
        "V_Page1",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(50.0, 10.0),
            Point2D::new(50.0, 90.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(v_page1, Some(p1_lid)))
        .unwrap();

    // Add Page 2 and switch active page to Page 2 (index 1)
    h.history
        .execute(
            &mut h.doc,
            CommandOp::AddPage {
                name: "Page 2".to_owned(),
                width_mm: 210.0,
                height_mm: 297.0,
                bleed_mm: 3.0,
            },
        )
        .unwrap();
    h.doc.set_active_page_index(1).unwrap();
    let p2_id = h.doc.active_page().unwrap().id;
    let p2_lid = h.layer_id();

    // Horizontal line on Page 2: (10..90, 50)
    let h_page2 = SceneObject::new(
        ObjectId(0),
        "H_Page2",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(10.0, 50.0),
            Point2D::new(90.0, 50.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(h_page2, Some(p2_lid)))
        .unwrap();
    let h_p2_id = *h.doc.objects().keys().max().unwrap();

    // 1. Multi-page isolation: swiping (70, 45)->(70, 55) on Page 2 deletes ALL of H_Page2 (Page 1 V_Page1 ignored)
    let swipe = [Point2D::new(70.0, 45.0), Point2D::new(70.0, 55.0)];
    let ops_p2 = coree_tools::build_virtual_segment_delete_stroke_ops(&h.ctx(), &swipe);
    assert_eq!(ops_p2.len(), 1);
    assert!(
        matches!(&ops_p2[0], CommandOp::EditPath { id, path } if *id == h_p2_id && path.subpaths.is_empty()),
        "Inactive Page 1 cutter must NOT split Page 2 line, got {ops_p2:?}"
    );

    // 2. Hidden object (visible = false) isolation on Page 2
    let v_hidden = SceneObject::new(
        ObjectId(0),
        "V_HiddenObj",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(50.0, 10.0),
            Point2D::new(50.0, 90.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(v_hidden, Some(p2_lid)))
        .unwrap();
    let v_hid_id = *h.doc.objects().keys().max().unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::SetObjectVisibility {
                ids: vec![v_hid_id],
                visible: false,
            },
        )
        .unwrap();
    let ops_hid_obj = coree_tools::build_virtual_segment_delete_stroke_ops(&h.ctx(), &swipe);
    assert!(
        matches!(&ops_hid_obj[0], CommandOp::EditPath { id, path } if *id == h_p2_id && path.subpaths.is_empty()),
        "Hidden object (visible=false) must NOT act as VSD cutter, got {ops_hid_obj:?}"
    );

    // 3. Hidden layer (layer.visible = false) isolation on Page 2
    h.history
        .execute(
            &mut h.doc,
            CommandOp::AddLayer {
                page_id: p2_id,
                name: "HiddenLayer".to_owned(),
                is_cut_layer: false,
                master_scope: MasterLayerScope::None,
            },
        )
        .unwrap();
    let hidden_lid = h.doc.active_page().unwrap().layers.last().unwrap().id;
    let v_hidden_layer = SceneObject::new(
        ObjectId(0),
        "V_HiddenLayer",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(50.0, 10.0),
            Point2D::new(50.0, 90.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(
            &mut h.doc,
            CommandOp::create_object(v_hidden_layer, Some(hidden_lid)),
        )
        .unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::SetLayerFlags {
                layer_id: hidden_lid,
                visible: Some(false),
                locked: None,
                printable: None,
                is_cut_layer: None,
            },
        )
        .unwrap();
    let ops_hid_layer = coree_tools::build_virtual_segment_delete_stroke_ops(&h.ctx(), &swipe);
    assert!(
        matches!(&ops_hid_layer[0], CommandOp::EditPath { id, path } if *id == h_p2_id && path.subpaths.is_empty()),
        "Hidden layer (layer.visible=false) must NOT act as VSD cutter, got {ops_hid_layer:?}"
    );

    // 4. Unentered PowerClip clipped contents must NOT leak outside container as phantom cutters
    let pc_small_container = SceneObject::new(
        ObjectId(0),
        "SmallPC",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 20.0, 80.0,
        ))),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(
            &mut h.doc,
            CommandOp::create_object(pc_small_container, Some(p2_lid)),
        )
        .unwrap();
    let small_pc_id = *h.doc.objects().keys().max().unwrap();
    let clipped_v = SceneObject::new(
        ObjectId(0),
        "ClippedV",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(50.0, 10.0),
            Point2D::new(50.0, 90.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(
            &mut h.doc,
            CommandOp::create_object(clipped_v, Some(p2_lid)),
        )
        .unwrap();
    let clipped_v_id = *h.doc.objects().keys().max().unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::CreatePowerClip {
                container_id: small_pc_id,
                content_ids: vec![clipped_v_id],
            },
        )
        .unwrap();
    // Swipe at (70, 50) (right of small_pc_id container [10..30] and clipped_v [50]): clipped_v must NOT cut H_Page2 at x=50;
    // small_pc_id container boundary cuts at x=30, leaving (10..30).
    let ops_unentered_pc = coree_tools::build_virtual_segment_delete_stroke_ops(&h.ctx(), &swipe);
    match &ops_unentered_pc[0] {
        CommandOp::EditPath { id, path } if *id == h_p2_id => {
            assert_eq!(path.subpaths.len(), 1);
            let xs: Vec<f64> = path.subpaths[0].nodes.iter().map(|n| n.pos.x).collect();
            assert!(
                (xs[0] - 10.0).abs() < 1e-4 && (xs[xs.len() - 1] - 30.0).abs() < 1e-4,
                "Unentered PowerClip container boundary (x=30) cuts, NOT clipped child (x=50); got xs={xs:?}"
            );
        }
        other => panic!("Expected EditPath on H_Page2, got {other:?}"),
    }

    // 5. Positive control: visible locked reference layer/line DOES act as cutter
    h.history
        .execute(
            &mut h.doc,
            CommandOp::AddLayer {
                page_id: p2_id,
                name: "LockedRefLayer".to_owned(),
                is_cut_layer: false,
                master_scope: MasterLayerScope::None,
            },
        )
        .unwrap();
    let locked_lid = h.doc.active_page().unwrap().layers.last().unwrap().id;
    let v_locked_ref = SceneObject::new(
        ObjectId(0),
        "V_LockedRef",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(50.0, 10.0),
            Point2D::new(50.0, 90.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(
            &mut h.doc,
            CommandOp::create_object(v_locked_ref, Some(locked_lid)),
        )
        .unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::SetLayerFlags {
                layer_id: locked_lid,
                visible: Some(true),
                locked: Some(true),
                printable: None,
                is_cut_layer: None,
            },
        )
        .unwrap();
    let ops_locked_ref = coree_tools::build_virtual_segment_delete_stroke_ops(&h.ctx(), &swipe);
    match &ops_locked_ref[0] {
        CommandOp::EditPath { id, path } if *id == h_p2_id => {
            assert_eq!(path.subpaths.len(), 1);
            let xs: Vec<f64> = path.subpaths[0].nodes.iter().map(|n| n.pos.x).collect();
            assert!(
                (xs[0] - 10.0).abs() < 1e-4 && (xs[xs.len() - 1] - 50.0).abs() < 1e-4,
                "Visible locked reference line at x=50 MUST act as cutter leaving (10..50), got xs={xs:?}"
            );
        }
        other => panic!("Expected EditPath on H_Page2, got {other:?}"),
    }

    // 6. Positive control: active powerclip_edit_id contents DO act as cutters against each other
    let pc_big = SceneObject::new(
        ObjectId(0),
        "BigPC",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            110.0, 10.0, 80.0, 80.0,
        ))),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(pc_big, Some(p2_lid)))
        .unwrap();
    let big_pc_id = *h.doc.objects().keys().max().unwrap();
    let pc_h = SceneObject::new(
        ObjectId(0),
        "PC_H",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(120.0, 50.0),
            Point2D::new(180.0, 50.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(pc_h, Some(p2_lid)))
        .unwrap();
    let pc_h_id = *h.doc.objects().keys().max().unwrap();
    let pc_v = SceneObject::new(
        ObjectId(0),
        "PC_V",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(150.0, 20.0),
            Point2D::new(150.0, 80.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(pc_v, Some(p2_lid)))
        .unwrap();
    let pc_v_id = *h.doc.objects().keys().max().unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::CreatePowerClip {
                container_id: big_pc_id,
                content_ids: vec![pc_h_id, pc_v_id],
            },
        )
        .unwrap();

    let mut pc_ctx = h.ctx();
    pc_ctx.powerclip_edit_id = Some(big_pc_id);
    let ops_pc_edit = coree_tools::build_virtual_segment_delete_stroke_ops(
        &pc_ctx,
        &[Point2D::new(165.0, 45.0), Point2D::new(165.0, 55.0)],
    );
    assert_eq!(ops_pc_edit.len(), 1);
    match &ops_pc_edit[0] {
        CommandOp::EditPath { id, path } if *id == pc_h_id => {
            assert_eq!(path.subpaths.len(), 1);
            let xs: Vec<f64> = path.subpaths[0].nodes.iter().map(|n| n.pos.x).collect();
            assert!(
                (xs[0] - 120.0).abs() < 1e-4 && (xs[xs.len() - 1] - 150.0).abs() < 1e-4,
                "Active PowerClip edit contents must cut at x=150 leaving (120..150), got xs={xs:?}"
            );
        }
        other => panic!("Expected EditPath on PC_H, got {other:?}"),
    }
}

#[test]
fn w1c_r10_vsd_and_eraser_group_descent_fallthrough_locked_and_undo_redo() {
    let mut h = Harness::new();
    let lid = Some(h.layer_id());

    // 1. Underlying horizontal line at world (50..180, 100) underneath the rotated Group
    let under_line = SceneObject::new(
        ObjectId(0),
        "UnderLine",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(50.0, 100.0),
            Point2D::new(180.0, 100.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(under_line, lid))
        .unwrap();
    let under_id = *h.doc.objects().keys().max().unwrap();

    // 2. Crossing lines inside a nested, translated + 45-deg rotated Group
    let child_line = SceneObject::new(
        ObjectId(0),
        "GroupedLine",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(-30.0, 0.0),
            Point2D::new(30.0, 0.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(child_line, lid))
        .unwrap();
    let child_line_id = *h.doc.objects().keys().max().unwrap();

    let child_cross = SceneObject::new(
        ObjectId(0),
        "GroupedCross",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(0.0, -30.0),
            Point2D::new(0.0, 30.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(child_cross, lid))
        .unwrap();
    let child_cross_id = *h.doc.objects().keys().max().unwrap();

    // Locked sibling line inside the group to verify locked child protection
    let locked_child = SceneObject::new(
        ObjectId(0),
        "LockedGroupedLine",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(-20.0, 25.0),
            Point2D::new(20.0, 25.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(locked_child, lid))
        .unwrap();
    let locked_child_id = *h.doc.objects().keys().max().unwrap();

    h.history
        .execute(
            &mut h.doc,
            CommandOp::Group {
                ids: vec![child_line_id, child_cross_id],
                name: "InnerGrp".to_owned(),
            },
        )
        .unwrap();
    let inner_gid = *h.doc.objects().keys().max().unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::Group {
                ids: vec![inner_gid, locked_child_id],
                name: "OuterGrp".to_owned(),
            },
        )
        .unwrap();
    let outer_gid = *h.doc.objects().keys().max().unwrap();

    let grp_xf = Affine2D::translate(100.0, 100.0).compose(&Affine2D::rotate_deg(45.0));
    h.history
        .execute(
            &mut h.doc,
            CommandOp::TransformObjects {
                ids: vec![outer_gid],
                affine: grp_xf,
            },
        )
        .unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::SetObjectLocked {
                ids: vec![locked_child_id],
                locked: true,
            },
        )
        .unwrap();

    let orig_child_line = h.doc.object(child_line_id).unwrap().payload.clone();
    let orig_child_cross = h.doc.object(child_cross_id).unwrap().payload.clone();

    // Part A: Fast 2-point VSD swipe straddling the right half of rotated child_line (local (15, 0))
    let w_mid = grp_xf.transform_point(Point2D::new(15.0, 0.0));
    let perp_dir =
        coree_core::Vec2D::new(-45.0_f64.to_radians().sin(), 45.0_f64.to_radians().cos());
    let swipe_a = w_mid - perp_dir * 8.0;
    let swipe_b = w_mid + perp_dir * 8.0;

    let mut tc = ToolCore::new(ToolId::VirtualSegmentDelete);
    let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(swipe_a, [0.0, 0.0]));
    let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(swipe_b, [0.0, 0.0]));
    let vsd_out = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(swipe_b, [0.0, 0.0]));
    assert!(
        matches!(vsd_out, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
        "VSD swipe across rotated nested Group child MUST commit, got {vsd_out:?}"
    );
    h.commit(vsd_out);
    let edited_child_line = h.doc.object(child_line_id).unwrap().payload.clone();
    assert_ne!(edited_child_line, orig_child_line);
    // Remaining segment is local (-30, 0)..(0, 0), so world bounds shrink accordingly
    let bb_after = h.doc.world_object_bounds(child_line_id).unwrap();
    assert!(
        bb_after.max_x < 101.0,
        "Right half of rotated line must be deleted, got {bb_after:?}"
    );

    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(
        h.doc.object(child_line_id).unwrap().payload,
        orig_child_line
    );
    h.history.redo(&mut h.doc).unwrap();
    assert_eq!(
        h.doc.object(child_line_id).unwrap().payload,
        edited_child_line
    );
    h.history.undo(&mut h.doc).unwrap();

    // Part B: Fallthrough after Group child trimmed away in same swipe:
    // Swipe at center (100, 100) where child_line, child_cross, and under_line all pass.
    // Wait: swipe from (100, 100) (actually swipe slightly off-cross at (100+d) or twice at a shared point):
    // Here child_line passes through local (0,0) = world (100, 100), and under_line passes through (100, 100).
    // At world (110.6, 110.6) -> no, let's place an under_line directly passing through w_mid!
    let under_at_mid = SceneObject::new(
        ObjectId(0),
        "UnderAtMid",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(w_mid.x - 40.0, w_mid.y),
            Point2D::new(w_mid.x + 40.0, w_mid.y),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(under_at_mid, lid))
        .unwrap();
    let under_mid_id = *h.doc.objects().keys().max().unwrap();
    // Reorder outer_gid in front of under_mid_id by setting Z-order or deleting/recreating? Better: use ZOrderOp::ToFront on outer_gid!
    h.history
        .execute(
            &mut h.doc,
            CommandOp::SetZOrder {
                id: outer_gid,
                op: coree_core::ZOrderOp::BringToFront,
            },
        )
        .unwrap();
    // Swipe hitting w_mid twice (U-turn back onto w_mid): first hit trims child_line at w_mid,
    // second hit at w_mid falls through outer_gid (skip_id = Some(outer_gid)) to hit under_mid_id!
    let fallthrough_swipe = [
        swipe_a,
        w_mid,
        Point2D::new(w_mid.x + 20.0, w_mid.y + 20.0),
        w_mid,
    ];
    let ops_ft = coree_tools::build_virtual_segment_delete_stroke_ops(&h.ctx(), &fallthrough_swipe);
    assert!(
        ops_ft.iter().any(|op| matches!(op, CommandOp::EditPath { id, .. } if *id == child_line_id))
            && ops_ft.iter().any(|op| matches!(op, CommandOp::EditPath { id, .. } if *id == under_mid_id)),
        "Same-swipe revisit at w_mid must trim child_line AND fall through outer_gid to under_mid_id, got {ops_ft:?}"
    );
    let _ = under_id;

    // Part C: Locked child inside unlocked Group must NOT be deleted by VSD
    let locked_w = grp_xf.transform_point(Point2D::new(-15.0, 25.0));
    let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(locked_w, [0.0, 0.0]));
    let locked_out = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(locked_w, [0.0, 0.0]));
    assert_eq!(
        locked_out,
        ToolOutput::noop(),
        "Locked child inside unlocked Group must not be edited by VSD"
    );

    // Part D: Vector Eraser with EMPTY selection erases child_cross inside rotated nested Group + undo/redo
    h.selection.clear();
    h.settings.eraser_radius_mm = 2.0;
    let mut tc_era = ToolCore::new(ToolId::Eraser);
    let e0 = grp_xf.transform_point(Point2D::new(-5.0, -20.0));
    let e1 = grp_xf.transform_point(Point2D::new(5.0, -20.0));
    let _ = tc_era.on_pointer_down(&h.ctx(), &PointerEvent::primary(e0, [0.0, 0.0]));
    let _ = tc_era.on_pointer_move(&h.ctx(), &PointerEvent::primary(e1, [0.0, 0.0]));
    let era_out = tc_era.on_pointer_up(&h.ctx(), &PointerEvent::primary(e1, [0.0, 0.0]));
    assert!(
        matches!(era_out, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
        "Eraser with empty selection across Group child MUST commit, got {era_out:?}"
    );
    h.commit(era_out);
    let erased_cross = h.doc.object(child_cross_id).unwrap().payload.clone();
    assert_ne!(erased_cross, orig_child_cross);
    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(
        h.doc.object(child_cross_id).unwrap().payload,
        orig_child_cross
    );
    h.history.redo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(child_cross_id).unwrap().payload, erased_cross);
    h.history.undo(&mut h.doc).unwrap();

    // Part E: Vector Eraser with SELECTED Group erases child_line inside selected Group + undo/redo
    h.selection.select_single(&h.doc, outer_gid);
    let es0 = grp_xf.transform_point(Point2D::new(-15.0, -5.0));
    let es1 = grp_xf.transform_point(Point2D::new(-15.0, 5.0));
    let _ = tc_era.on_pointer_down(&h.ctx(), &PointerEvent::primary(es0, [0.0, 0.0]));
    let _ = tc_era.on_pointer_move(&h.ctx(), &PointerEvent::primary(es1, [0.0, 0.0]));
    let era_sel_out = tc_era.on_pointer_up(&h.ctx(), &PointerEvent::primary(es1, [0.0, 0.0]));
    assert!(
        matches!(era_sel_out, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
        "Eraser with selected Group across child MUST commit, got {era_sel_out:?}"
    );
    h.commit(era_sel_out);
    assert_ne!(
        h.doc.object(child_line_id).unwrap().payload,
        orig_child_line
    );
    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(
        h.doc.object(child_line_id).unwrap().payload,
        orig_child_line
    );
}

#[test]
fn w1c_r11_vsd_fallthrough_under_powerclip_artistic_text_and_solid_interior() {
    use coree_core::{ArtisticText, Rect2D};

    // Case 1: Unentered PowerClip (Fill::None) on top of TargetLine
    {
        let mut h = Harness::new();
        let lid = Some(h.layer_id());

        let target_line = SceneObject::new(
            ObjectId(0),
            "TargetUnderPC",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(20.0, 80.0),
                Point2D::new(180.0, 80.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::default());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(target_line, lid))
            .unwrap();
        let target_id = *h.doc.objects().keys().max().unwrap();
        let orig_target = h.doc.object(target_id).unwrap().payload.clone();

        // Unentered PowerClip container (Fill::None, x=110..160, y=50..110) on top in Z-order
        let pc_container = SceneObject::new(
            ObjectId(0),
            "PCOnTop",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                110.0, 50.0, 50.0, 60.0,
            ))),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::default());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(pc_container, lid))
            .unwrap();
        let pc_id = *h.doc.objects().keys().max().unwrap();

        let pc_child = SceneObject::new(
            ObjectId(0),
            "PCChild",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                120.0, 60.0, 30.0, 40.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::none());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(pc_child, lid))
            .unwrap();
        let pc_child_id = *h.doc.objects().keys().max().unwrap();

        h.history
            .execute(
                &mut h.doc,
                CommandOp::CreatePowerClip {
                    container_id: pc_id,
                    content_ids: vec![pc_child_id],
                },
            )
            .unwrap();

        let mut tc = ToolCore::new(ToolId::VirtualSegmentDelete);
        let a = Point2D::new(135.0, 76.0);
        let b = Point2D::new(135.0, 84.0);
        let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(a, [0.0, 0.0]));
        let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(b, [0.0, 8.0]));
        let out = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(b, [0.0, 8.0]));
        assert!(
            matches!(out, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "VSD swipe under unentered PowerClip MUST commit, got {out:?}"
        );
        h.commit(out);
        let cut_target = h.doc.object(target_id).unwrap().payload.clone();
        assert_ne!(
            cut_target, orig_target,
            "TargetLine under unentered PowerClip must be cut"
        );
        // PCOnTop boundaries (x=110, x=160) cut TargetLine so (20..110) and (160..180) remain
        match &cut_target {
            NodePayload::Path(p) => {
                assert_eq!(p.subpaths.len(), 2, "expected 2 remaining outer segments");
            }
            other => panic!("expected Path, got {other:?}"),
        }

        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, orig_target);
        h.history.redo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, cut_target);
    }

    // Case 2: ArtisticText overlay on top of TargetLine
    {
        let mut h = Harness::new();
        let lid = Some(h.layer_id());

        let target_line = SceneObject::new(
            ObjectId(0),
            "TargetUnderText",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(20.0, 80.0),
                Point2D::new(180.0, 80.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::default());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(target_line, lid))
            .unwrap();
        let target_id = *h.doc.objects().keys().max().unwrap();
        let orig_target = h.doc.object(target_id).unwrap().payload.clone();

        let v_cross = SceneObject::new(
            ObjectId(0),
            "VCross",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(100.0, 40.0),
                Point2D::new(100.0, 120.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::default());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(v_cross, lid))
            .unwrap();

        let text_obj = SceneObject::new(
            ObjectId(0),
            "OverlayText",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "OVERLAY",
                h.default_text_style.clone(),
            )),
        )
        .with_fill(Fill::Solid(Color::KEY_BLACK))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(50.0, 72.0));
        h.history
            .execute(&mut h.doc, CommandOp::create_object(text_obj, lid))
            .unwrap();
        let text_id = *h.doc.objects().keys().max().unwrap();
        let text_bb = h.doc.world_object_bounds(text_id).unwrap();
        let swipe_x = (text_bb.min_x + text_bb.max_x) * 0.5;
        assert!(
            swipe_x > 40.0 && swipe_x < 95.0,
            "unexpected text bounds {text_bb:?}"
        );

        let mut tc = ToolCore::new(ToolId::VirtualSegmentDelete);
        let a = Point2D::new(swipe_x, 76.0);
        let b = Point2D::new(swipe_x, 84.0);
        let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(a, [0.0, 0.0]));
        let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(b, [0.0, 8.0]));
        let out = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(b, [0.0, 8.0]));
        assert!(
            matches!(out, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "VSD swipe under ArtisticText overlay MUST commit, got {out:?}"
        );
        h.commit(out);
        let cut_target = h.doc.object(target_id).unwrap().payload.clone();
        assert_ne!(cut_target, orig_target);
        match &cut_target {
            NodePayload::Path(p) => {
                assert_eq!(p.subpaths.len(), 1);
                let xs: Vec<f64> = p.subpaths[0].nodes.iter().map(|n| n.pos.x).collect();
                assert!(
                    (xs[0] - 100.0).abs() < 1e-4 && (xs[xs.len() - 1] - 180.0).abs() < 1e-4,
                    "Left segment (20..100) must be deleted leaving (100..180), got xs={xs:?}"
                );
            }
            other => panic!("expected Path, got {other:?}"),
        }

        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, orig_target);
        h.history.redo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, cut_target);
    }

    // Case 3: Fill::Solid rectangle on top of TargetLine — interior falls through, edge cuts self
    {
        let mut h = Harness::new();
        let lid = Some(h.layer_id());

        let target_line = SceneObject::new(
            ObjectId(0),
            "TargetUnderSolid",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(20.0, 80.0),
                Point2D::new(180.0, 80.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::default());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(target_line, lid))
            .unwrap();
        let target_id = *h.doc.objects().keys().max().unwrap();
        let orig_target = h.doc.object(target_id).unwrap().payload.clone();

        // Crosses FilledOverlay's top edge (y=50) at x=50 and x=80 so top edge can be cut cleanly
        let edge_cutter_1 = SceneObject::new(
            ObjectId(0),
            "EdgeCutter1",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(50.0, 40.0),
                Point2D::new(50.0, 60.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::default());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(edge_cutter_1, lid))
            .unwrap();
        let edge_cutter_2 = SceneObject::new(
            ObjectId(0),
            "EdgeCutter2",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(80.0, 40.0),
                Point2D::new(80.0, 60.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::default());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(edge_cutter_2, lid))
            .unwrap();

        let filled_id = h.add_rect("FilledOverlay", 40.0, 50.0, 50.0, 60.0);
        let orig_filled = h.doc.object(filled_id).unwrap().payload.clone();

        // 3a. Swipe inside FilledOverlay interior at x=65, y=76..84 -> must cut TargetLine, NOT FilledOverlay
        let mut tc = ToolCore::new(ToolId::VirtualSegmentDelete);
        let a = Point2D::new(65.0, 76.0);
        let b = Point2D::new(65.0, 84.0);
        let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(a, [0.0, 0.0]));
        let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(b, [0.0, 8.0]));
        let out_interior = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(b, [0.0, 8.0]));
        assert!(
            matches!(out_interior, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "VSD swipe inside Fill::Solid interior MUST fall through to TargetLine, got {out_interior:?}"
        );
        h.commit(out_interior);
        let cut_target = h.doc.object(target_id).unwrap().payload.clone();
        assert_ne!(cut_target, orig_target);
        assert_eq!(
            h.doc.object(filled_id).unwrap().payload,
            orig_filled,
            "FilledOverlay must remain unedited when swiped only in its interior"
        );
        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, orig_target);
        h.history.redo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, cut_target);

        // 3b. Swipe at FilledOverlay's own top edge (x=65, y=48..52) -> MUST still cut FilledOverlay's own outline
        let ea = Point2D::new(65.0, 48.0);
        let eb = Point2D::new(65.0, 52.0);
        let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(ea, [0.0, 0.0]));
        let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(eb, [0.0, 4.0]));
        let out_edge = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(eb, [0.0, 4.0]));
        assert!(
            matches!(out_edge, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "VSD swipe on Fill::Solid shape's own outline MUST still cut FilledOverlay, got {out_edge:?}"
        );
        h.commit(out_edge);
        let cut_filled = h.doc.object(filled_id).unwrap().payload.clone();
        assert_ne!(cut_filled, orig_filled);
        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(filled_id).unwrap().payload, orig_filled);
        h.history.redo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(filled_id).unwrap().payload, cut_filled);
    }
}

#[test]
fn w1c_r11_unselected_eraser_and_sculpt_fallthrough_under_powerclip_and_artistic_text() {
    use coree_core::{ArtisticText, Rect2D};

    // Case 1: Unselected Eraser under unentered PowerClip (Fill::None)
    {
        let mut h = Harness::new();
        h.settings.eraser_radius_mm = 4.0;
        let lid = Some(h.layer_id());

        let target_id = h.add_rect("TargetUnderPC", 40.0, 60.0, 80.0, 40.0);
        let orig_target = h.doc.object(target_id).unwrap().payload.clone();

        let pc_container = SceneObject::new(
            ObjectId(0),
            "PCOnTopEraser",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                30.0, 50.0, 100.0, 60.0,
            ))),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::default());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(pc_container, lid))
            .unwrap();
        let pc_id = *h.doc.objects().keys().max().unwrap();

        let pc_child = SceneObject::new(
            ObjectId(0),
            "PCInnerChild",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                35.0, 55.0, 20.0, 20.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::MAGENTA))
        .with_outline(OutlineStyle::none());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(pc_child, lid))
            .unwrap();
        let pc_child_id = *h.doc.objects().keys().max().unwrap();
        h.history
            .execute(
                &mut h.doc,
                CommandOp::CreatePowerClip {
                    container_id: pc_id,
                    content_ids: vec![pc_child_id],
                },
            )
            .unwrap();
        let orig_pc = h.doc.object(pc_id).unwrap().payload.clone();

        h.selection.clear();
        let mut tc = ToolCore::new(ToolId::Eraser);
        let a = Point2D::new(100.0, 75.0);
        let b = Point2D::new(100.0, 85.0);
        let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(a, [0.0, 0.0]));
        let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(b, [0.0, 10.0]));
        let out = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(b, [0.0, 10.0]));
        assert!(
            matches!(out, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "Unselected Eraser under unentered PowerClip MUST commit, got {out:?}"
        );
        h.commit(out);
        let erased_target = h.doc.object(target_id).unwrap().payload.clone();
        assert_ne!(erased_target, orig_target);
        assert_eq!(h.doc.object(pc_id).unwrap().payload, orig_pc);
        let area_after = match &erased_target {
            NodePayload::Path(p) => p.area_mm2(),
            other => panic!("expected Path, got {other:?}"),
        };
        assert!(
            area_after < 80.0 * 40.0 - 50.0,
            "Target area {area_after} must decrease after eraser swipe"
        );

        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, orig_target);
        h.history.redo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, erased_target);
    }

    // Case 2: Unselected Eraser + Sculpt (Smear) under ArtisticText overlay
    {
        let mut h = Harness::new();
        h.settings.eraser_radius_mm = 4.0;
        h.settings.sculpt_radius_mm = 8.0;
        let lid = Some(h.layer_id());

        let target_id = h.add_rect("TargetUnderArtText", 40.0, 60.0, 80.0, 40.0);
        let orig_target = h.doc.object(target_id).unwrap().payload.clone();

        let text_obj = SceneObject::new(
            ObjectId(0),
            "TextOverlay",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "OVERLAY_TEXT",
                h.default_text_style.clone(),
            )),
        )
        .with_fill(Fill::Solid(Color::KEY_BLACK))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(35.0, 65.0));
        h.history
            .execute(&mut h.doc, CommandOp::create_object(text_obj, lid))
            .unwrap();
        let text_id = *h.doc.objects().keys().max().unwrap();
        let text_bb = h.doc.world_object_bounds(text_id).unwrap();
        let hit_x = (text_bb.min_x + text_bb.max_x) * 0.5;
        let hit_y = (text_bb.min_y + text_bb.max_y) * 0.5;
        assert!(
            hit_x > 42.0 && hit_x < 118.0 && hit_y > 60.0 && hit_y < 100.0,
            "Text overlay bounds {text_bb:?} must overlap TargetUnderArtText"
        );

        // 2a. Unselected Eraser under ArtisticText overlay
        h.selection.clear();
        let mut tc = ToolCore::new(ToolId::Eraser);
        let ea = Point2D::new(hit_x, hit_y - 2.0);
        let eb = Point2D::new(hit_x, hit_y + 2.0);
        let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(ea, [0.0, 0.0]));
        let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(eb, [0.0, 4.0]));
        let out_era = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(eb, [0.0, 4.0]));
        assert!(
            matches!(out_era, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "Unselected Eraser under ArtisticText MUST commit, got {out_era:?}"
        );
        h.commit(out_era);
        let erased_target = h.doc.object(target_id).unwrap().payload.clone();
        assert_ne!(erased_target, orig_target);

        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, orig_target);
        h.history.redo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, erased_target);
        h.history.undo(&mut h.doc).unwrap();

        // 2b. Unselected Sculpt (Smear) near top-left corner (40, 60) covered by ArtisticText
        let corner_text = SceneObject::new(
            ObjectId(0),
            "CornerTextOverlay",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "CORNER",
                h.default_text_style.clone(),
            )),
        )
        .with_fill(Fill::Solid(Color::KEY_BLACK))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(35.0, 62.0));
        h.history
            .execute(&mut h.doc, CommandOp::create_object(corner_text, lid))
            .unwrap();
        h.selection.clear();
        let mut tc_smear = ToolCore::new(ToolId::Smear);
        let sa = Point2D::new(40.0, 60.0);
        let sb = Point2D::new(43.0, 62.0);
        let _ = tc_smear.on_pointer_down(&h.ctx(), &PointerEvent::primary(sa, [0.0, 0.0]));
        let _ = tc_smear.on_pointer_move(&h.ctx(), &PointerEvent::primary(sb, [3.0, 2.0]));
        let out_smear = tc_smear.on_pointer_up(&h.ctx(), &PointerEvent::primary(sb, [3.0, 2.0]));
        assert!(
            matches!(out_smear, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "Unselected Smear under ArtisticText MUST fall through and commit, got {out_smear:?}"
        );
        h.commit(out_smear);
        assert_ne!(h.doc.object(target_id).unwrap().payload, orig_target);
        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, orig_target);
    }
}

#[test]
fn w1c_r12_grouped_solid_top_zorder_for_unselected_eraser_and_sculpt() {
    use coree_core::{ArtisticText, Rect2D};

    // Case 1: Group([HiddenLine, TopSolid (Fill::Solid)]) — unselected Eraser inside TopSolid
    // erases TopSolid (not HiddenLine) with undo/redo verification.
    {
        let mut h = Harness::new();
        h.settings.eraser_radius_mm = 4.0;
        let lid = Some(h.layer_id());

        let hidden_line = SceneObject::new(
            ObjectId(0),
            "HiddenLine",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(20.0, 80.0),
                Point2D::new(180.0, 80.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::default());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(hidden_line, lid))
            .unwrap();
        let hidden_id = *h.doc.objects().keys().max().unwrap();
        let orig_hidden = h.doc.object(hidden_id).unwrap().payload.clone();

        let top_solid_id = h.add_rect("TopSolid", 40.0, 50.0, 80.0, 60.0);
        let orig_top_solid = h.doc.object(top_solid_id).unwrap().payload.clone();

        h.history
            .execute(
                &mut h.doc,
                CommandOp::Group {
                    ids: vec![hidden_id, top_solid_id],
                    name: "SolidOverLineGrp".to_owned(),
                },
            )
            .unwrap();

        h.selection.clear();
        let mut tc = ToolCore::new(ToolId::Eraser);
        let a = Point2D::new(80.0, 78.0);
        let b = Point2D::new(80.0, 82.0);
        let _ = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(a, [0.0, 0.0]));
        let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(b, [0.0, 4.0]));
        let out = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(b, [0.0, 4.0]));
        assert!(
            matches!(out, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "Unselected Eraser inside grouped TopSolid MUST commit, got {out:?}"
        );
        h.commit(out);

        let erased_top = h.doc.object(top_solid_id).unwrap().payload.clone();
        assert_ne!(
            erased_top, orig_top_solid,
            "Frontmost Fill::Solid child in Group must be erased"
        );
        assert_eq!(
            h.doc.object(hidden_id).unwrap().payload,
            orig_hidden,
            "HiddenLine behind TopSolid must remain untouched"
        );

        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(top_solid_id).unwrap().payload, orig_top_solid);
        assert_eq!(h.doc.object(hidden_id).unwrap().payload, orig_hidden);

        h.history.redo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(top_solid_id).unwrap().payload, erased_top);
        assert_eq!(h.doc.object(hidden_id).unwrap().payload, orig_hidden);
    }

    // Case 2: Group([HiddenLine, TopSolid (Fill::Solid)]) — unselected Smear inside TopSolid
    // (outside sculpt_radius_mm of TopSolid's outline) must return ToolOutput::noop() and
    // leave HiddenLine untouched.
    {
        let mut h = Harness::new();
        h.settings.sculpt_radius_mm = 4.0;
        let lid = Some(h.layer_id());

        let hidden_line = SceneObject::new(
            ObjectId(0),
            "HiddenLineSculpt",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(20.0, 80.0),
                Point2D::new(180.0, 80.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::default());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(hidden_line, lid))
            .unwrap();
        let hidden_id = *h.doc.objects().keys().max().unwrap();
        let orig_hidden = h.doc.object(hidden_id).unwrap().payload.clone();

        let top_solid_id = h.add_rect("TopSolidSculpt", 40.0, 50.0, 80.0, 60.0);
        let orig_top_solid = h.doc.object(top_solid_id).unwrap().payload.clone();

        h.history
            .execute(
                &mut h.doc,
                CommandOp::Group {
                    ids: vec![hidden_id, top_solid_id],
                    name: "SolidOverLineSculptGrp".to_owned(),
                },
            )
            .unwrap();

        h.selection.clear();
        let mut ctx = h.ctx();
        ctx.treat_all_objects_as_filled = false;

        let mut tc = ToolCore::new(ToolId::Smear);
        let a = Point2D::new(80.0, 80.0);
        let b = Point2D::new(83.0, 80.0);
        let _ = tc.on_pointer_down(&ctx, &PointerEvent::primary(a, [0.0, 0.0]));
        let _ = tc.on_pointer_move(&ctx, &PointerEvent::primary(b, [3.0, 0.0]));
        let out = tc.on_pointer_up(&ctx, &PointerEvent::primary(b, [3.0, 0.0]));
        assert_eq!(
            out,
            ToolOutput::noop(),
            "Unselected Smear inside TopSolid interior (>sculpt_radius_mm from outline) must be noop, got {out:?}"
        );
        assert_eq!(h.doc.object(hidden_id).unwrap().payload, orig_hidden);
        assert_eq!(h.doc.object(top_solid_id).unwrap().payload, orig_top_solid);

        // Positive control: dragging near TopSolid's corner (40, 50) deforms TopSolid cleanly
        let ca = Point2D::new(40.0, 50.0);
        let cb = Point2D::new(43.0, 52.0);
        let _ = tc.on_pointer_down(&ctx, &PointerEvent::primary(ca, [0.0, 0.0]));
        let _ = tc.on_pointer_move(&ctx, &PointerEvent::primary(cb, [3.0, 2.0]));
        let out_edge = tc.on_pointer_up(&ctx, &PointerEvent::primary(cb, [3.0, 2.0]));
        assert!(
            matches!(out_edge, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "Smear at TopSolid's corner must commit, got {out_edge:?}"
        );
        h.commit(out_edge);
        assert_ne!(h.doc.object(top_solid_id).unwrap().payload, orig_top_solid);
        assert_eq!(h.doc.object(hidden_id).unwrap().payload, orig_hidden);
    }

    // Case 3: Group([UnlockedRect, LockedOnTop]) — skips LockedOnTop and edits UnlockedRect
    {
        let mut h = Harness::new();
        h.settings.eraser_radius_mm = 4.0;
        h.settings.sculpt_radius_mm = 6.0;

        let unlocked_id = h.add_rect("UnlockedRect", 30.0, 40.0, 80.0, 60.0);
        let orig_unlocked = h.doc.object(unlocked_id).unwrap().payload.clone();

        let locked_id = h.add_rect("LockedOnTop", 30.0, 40.0, 80.0, 60.0);
        let orig_locked = h.doc.object(locked_id).unwrap().payload.clone();

        h.history
            .execute(
                &mut h.doc,
                CommandOp::Group {
                    ids: vec![unlocked_id, locked_id],
                    name: "LockedTopGrp".to_owned(),
                },
            )
            .unwrap();
        h.history
            .execute(
                &mut h.doc,
                CommandOp::SetObjectLocked {
                    ids: vec![locked_id],
                    locked: true,
                },
            )
            .unwrap();

        // 3a. Unselected Eraser skips LockedOnTop and erases UnlockedRect
        h.selection.clear();
        let mut tc_era = ToolCore::new(ToolId::Eraser);
        let ea = Point2D::new(70.0, 65.0);
        let eb = Point2D::new(70.0, 75.0);
        let _ = tc_era.on_pointer_down(&h.ctx(), &PointerEvent::primary(ea, [0.0, 0.0]));
        let _ = tc_era.on_pointer_move(&h.ctx(), &PointerEvent::primary(eb, [0.0, 10.0]));
        let out_era = tc_era.on_pointer_up(&h.ctx(), &PointerEvent::primary(eb, [0.0, 10.0]));
        assert!(
            matches!(out_era, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "Eraser must skip LockedOnTop and erase UnlockedRect, got {out_era:?}"
        );
        h.commit(out_era);
        assert_ne!(h.doc.object(unlocked_id).unwrap().payload, orig_unlocked);
        assert_eq!(h.doc.object(locked_id).unwrap().payload, orig_locked);

        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(unlocked_id).unwrap().payload, orig_unlocked);

        // 3b. Unselected Smear near corner skips LockedOnTop and deforms UnlockedRect
        let mut tc_smear = ToolCore::new(ToolId::Smear);
        let sa = Point2D::new(30.0, 40.0);
        let sb = Point2D::new(33.0, 42.0);
        let _ = tc_smear.on_pointer_down(&h.ctx(), &PointerEvent::primary(sa, [0.0, 0.0]));
        let _ = tc_smear.on_pointer_move(&h.ctx(), &PointerEvent::primary(sb, [3.0, 2.0]));
        let out_smear = tc_smear.on_pointer_up(&h.ctx(), &PointerEvent::primary(sb, [3.0, 2.0]));
        assert!(
            matches!(out_smear, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "Smear must skip LockedOnTop and deform UnlockedRect, got {out_smear:?}"
        );
        h.commit(out_smear);
        assert_ne!(h.doc.object(unlocked_id).unwrap().payload, orig_unlocked);
        assert_eq!(h.doc.object(locked_id).unwrap().payload, orig_locked);
    }

    // Case 4: Group([LockedChild, ArtisticText]) on top of TargetRect — skips locked/non-vector
    // children cleanly and falls through to TargetRect for both Eraser and Smear.
    {
        let mut h = Harness::new();
        h.settings.eraser_radius_mm = 4.0;
        h.settings.sculpt_radius_mm = 8.0;
        let lid = Some(h.layer_id());

        let target_id = h.add_rect("TargetUnderLockedTextGrp", 40.0, 60.0, 80.0, 40.0);
        let orig_target = h.doc.object(target_id).unwrap().payload.clone();

        let locked_child = SceneObject::new(
            ObjectId(0),
            "LockedChildInGrp",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                35.0, 55.0, 90.0, 50.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::MAGENTA))
        .with_outline(OutlineStyle::none());
        h.history
            .execute(&mut h.doc, CommandOp::create_object(locked_child, lid))
            .unwrap();
        let locked_child_id = *h.doc.objects().keys().max().unwrap();
        let orig_locked_child = h.doc.object(locked_child_id).unwrap().payload.clone();

        let art_text = SceneObject::new(
            ObjectId(0),
            "ArtTextInGrp",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "BLOCKER",
                h.default_text_style.clone(),
            )),
        )
        .with_fill(Fill::Solid(Color::KEY_BLACK))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(35.0, 62.0));
        h.history
            .execute(&mut h.doc, CommandOp::create_object(art_text, lid))
            .unwrap();
        let art_text_id = *h.doc.objects().keys().max().unwrap();
        let orig_art_text = h.doc.object(art_text_id).unwrap().payload.clone();
        let text_bb = h.doc.world_object_bounds(art_text_id).unwrap();
        let hit_x = (text_bb.min_x + text_bb.max_x) * 0.5;
        let hit_y = (text_bb.min_y + text_bb.max_y) * 0.5;

        h.history
            .execute(
                &mut h.doc,
                CommandOp::Group {
                    ids: vec![locked_child_id, art_text_id],
                    name: "LockedAndTextGrp".to_owned(),
                },
            )
            .unwrap();
        h.history
            .execute(
                &mut h.doc,
                CommandOp::SetObjectLocked {
                    ids: vec![locked_child_id],
                    locked: true,
                },
            )
            .unwrap();

        // 4a. Unselected Eraser falls through Group([LockedChild, ArtisticText]) to TargetRect
        h.selection.clear();
        let mut tc_era = ToolCore::new(ToolId::Eraser);
        let ea = Point2D::new(hit_x, hit_y - 2.0);
        let eb = Point2D::new(hit_x, hit_y + 2.0);
        let _ = tc_era.on_pointer_down(&h.ctx(), &PointerEvent::primary(ea, [0.0, 0.0]));
        let _ = tc_era.on_pointer_move(&h.ctx(), &PointerEvent::primary(eb, [0.0, 4.0]));
        let out_era = tc_era.on_pointer_up(&h.ctx(), &PointerEvent::primary(eb, [0.0, 4.0]));
        assert!(
            matches!(out_era, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "Eraser must fall through Group([LockedChild, ArtisticText]) to TargetRect, got {out_era:?}"
        );
        h.commit(out_era);
        assert_ne!(h.doc.object(target_id).unwrap().payload, orig_target);
        assert_eq!(
            h.doc.object(locked_child_id).unwrap().payload,
            orig_locked_child
        );
        assert_eq!(h.doc.object(art_text_id).unwrap().payload, orig_art_text);

        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(h.doc.object(target_id).unwrap().payload, orig_target);

        // 4b. Unselected Smear near corner (40, 60) falls through Group([LockedChild, ArtisticText])
        let mut tc_smear = ToolCore::new(ToolId::Smear);
        let sa = Point2D::new(40.0, 60.0);
        let sb = Point2D::new(43.0, 62.0);
        let _ = tc_smear.on_pointer_down(&h.ctx(), &PointerEvent::primary(sa, [0.0, 0.0]));
        let _ = tc_smear.on_pointer_move(&h.ctx(), &PointerEvent::primary(sb, [3.0, 2.0]));
        let out_smear = tc_smear.on_pointer_up(&h.ctx(), &PointerEvent::primary(sb, [3.0, 2.0]));
        assert!(
            matches!(out_smear, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
            "Smear must fall through Group([LockedChild, ArtisticText]) to TargetRect, got {out_smear:?}"
        );
        h.commit(out_smear);
        assert_ne!(h.doc.object(target_id).unwrap().payload, orig_target);
        assert_eq!(
            h.doc.object(locked_child_id).unwrap().payload,
            orig_locked_child
        );
        assert_eq!(h.doc.object(art_text_id).unwrap().payload, orig_art_text);
    }
}

#[test]
fn w1c_r15_sculpt_selected_group_isolation_and_preservation() {
    let mut h = Harness::new();
    h.settings.sculpt_radius_mm = 8.0;
    h.settings.sculpt_strength = 0.6;

    // Group([GroupChild]) at (20, 20, 60, 40)
    let child_id = h.add_rect("GroupChild", 20.0, 20.0, 60.0, 40.0);
    let orig_child = h.doc.object(child_id).unwrap().payload.clone();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::Group {
                ids: vec![child_id],
                name: "SelectedSculptGroup".to_owned(),
            },
        )
        .unwrap();
    let grp_id = *h.doc.objects().keys().max().unwrap();

    // Overlapping unselected object on top of GroupChild's (20, 20) corner
    let overlap_id = h.add_rect("OverlappingUnselected", 18.0, 18.0, 10.0, 10.0);
    let orig_overlap = h.doc.object(overlap_id).unwrap().payload.clone();

    // Disjoint unselected object far away at (150, 150)
    let disjoint_id = h.add_rect("DisjointUnselected", 150.0, 150.0, 30.0, 30.0);
    let orig_disjoint = h.doc.object(disjoint_id).unwrap().payload.clone();

    h.selection.select_single(&h.doc, grp_id);
    assert_eq!(h.selection.objects, vec![grp_id]);

    // 1. Disjoint drag over DisjointUnselected while Group is selected -> noop & zero mutation
    for tool in [
        ToolId::Smear,
        ToolId::Smooth,
        ToolId::Twirl,
        ToolId::AttractRepel,
        ToolId::Roughen,
    ] {
        let mut tc = ToolCore::new(tool);
        let d_down = tc.on_pointer_down(
            &h.ctx(),
            &PointerEvent::primary(Point2D::new(150.0, 150.0), [0.0, 0.0]),
        );
        assert!(
            !matches!(
                d_down,
                ToolOutput::Consumed { ref actions, .. }
                    if actions.iter().any(|a| matches!(a, ToolActionRequest::SelectObjects { .. }))
            ),
            "[{tool:?}] disjoint press must not emit SelectObjects"
        );
        let _ = tc.on_pointer_move(
            &h.ctx(),
            &PointerEvent::primary(Point2D::new(155.0, 155.0), [5.0, 5.0]),
        );
        let up = tc.on_pointer_up(
            &h.ctx(),
            &PointerEvent::primary(Point2D::new(155.0, 155.0), [5.0, 5.0]),
        );
        assert_eq!(
            up,
            ToolOutput::noop(),
            "[{tool:?}] disjoint drag while Group is selected must be noop"
        );
        assert_eq!(h.doc.object(disjoint_id).unwrap().payload, orig_disjoint);
        assert_eq!(h.doc.object(overlap_id).unwrap().payload, orig_overlap);
        assert_eq!(h.doc.object(child_id).unwrap().payload, orig_child);
        assert_eq!(h.selection.objects, vec![grp_id]);

        // Also test build_sculpt_stroke_output directly on disjoint points
        let direct_disjoint = coree_tools::build_sculpt_stroke_output(
            &h.ctx(),
            tool,
            &[Point2D::new(150.0, 150.0), Point2D::new(155.0, 155.0)],
            false,
        );
        assert_eq!(
            direct_disjoint,
            ToolOutput::noop(),
            "[{tool:?}] build_sculpt_stroke_output on disjoint unselected must be noop"
        );
    }

    // 2. Drag Smear over overlapping corner (20, 20) -> sculpts GroupChild ONLY, leaves OverlappingUnselected untouched, preserves grp_id selection
    let mut tc_smear = ToolCore::new(ToolId::Smear);
    let down = tc_smear.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(20.0, 20.0), [0.0, 0.0]),
    );
    assert!(
        !matches!(
            down,
            ToolOutput::Consumed { ref actions, .. }
                if actions.iter().any(|a| matches!(a, ToolActionRequest::SelectObjects { .. }))
        ),
        "Sculpting inside selected Group must not clobber group selection"
    );
    let _ = tc_smear.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(25.0, 25.0), [5.0, 5.0]),
    );
    let up = tc_smear.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(25.0, 25.0), [5.0, 5.0]),
    );
    match &up {
        ToolOutput::Commit { post_actions, .. } => {
            assert!(
                !post_actions
                    .iter()
                    .any(|a| matches!(a, ToolActionRequest::SelectObjects { .. })),
                "Commit must not overwrite selected Group, got {post_actions:?}"
            );
        }
        other => panic!("Expected Commit on selected Group Smear, got {other:?}"),
    }
    h.commit(up);
    let sculpted_child = h.doc.object(child_id).unwrap().payload.clone();
    assert_ne!(
        sculpted_child, orig_child,
        "GroupChild must be sculpted while OverlappingUnselected is untouched"
    );
    assert_eq!(h.doc.object(overlap_id).unwrap().payload, orig_overlap);
    assert_eq!(h.doc.object(disjoint_id).unwrap().payload, orig_disjoint);
    assert_eq!(h.selection.objects, vec![grp_id]);

    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(child_id).unwrap().payload, orig_child);
    h.history.redo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(child_id).unwrap().payload, sculpted_child);
    h.history.undo(&mut h.doc).unwrap();

    // 3. Direct build_sculpt_stroke_output with Smooth on overlapping corner (20, 20)
    let direct_smooth = coree_tools::build_sculpt_stroke_output(
        &h.ctx(),
        ToolId::Smooth,
        &[Point2D::new(20.0, 20.0), Point2D::new(22.0, 22.0)],
        false,
    );
    match &direct_smooth {
        ToolOutput::Commit { post_actions, .. } => {
            assert!(
                !post_actions
                    .iter()
                    .any(|a| matches!(a, ToolActionRequest::SelectObjects { .. })),
                "build_sculpt_stroke_output must preserve selected Group"
            );
        }
        other => panic!("Expected Commit from build_sculpt_stroke_output(Smooth), got {other:?}"),
    }
    h.commit(direct_smooth);
    assert_ne!(h.doc.object(child_id).unwrap().payload, orig_child);
    assert_eq!(h.doc.object(overlap_id).unwrap().payload, orig_overlap);
    assert_eq!(h.selection.objects, vec![grp_id]);
}

#[test]
fn w1c_r15_sculpt_selected_group_skips_locked_and_non_vector_children_in_zorder() {
    use coree_core::{ArtisticText, Rect2D};

    let mut h = Harness::new();
    h.settings.sculpt_radius_mm = 8.0;
    let lid = Some(h.layer_id());

    // Bottom editable vector child at (30, 40, 60, 50)
    let bottom_id = h.add_rect("BottomVectorChild", 30.0, 40.0, 60.0, 50.0);
    let orig_bottom = h.doc.object(bottom_id).unwrap().payload.clone();

    // Middle non-vector child (ArtisticText) covering corner (30, 40)
    let text_obj = SceneObject::new(
        ObjectId(0),
        "TextChild",
        NodePayload::ArtisticText(ArtisticText::from_plain(
            "TEXT_CHILD",
            h.default_text_style.clone(),
        )),
    )
    .with_fill(Fill::Solid(Color::KEY_BLACK))
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(25.0, 35.0));
    h.history
        .execute(&mut h.doc, CommandOp::create_object(text_obj, lid))
        .unwrap();
    let text_id = *h.doc.objects().keys().max().unwrap();
    let orig_text = h.doc.object(text_id).unwrap().payload.clone();

    // Top locked vector child covering the same region
    let locked_obj = SceneObject::new(
        ObjectId(0),
        "LockedChild",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            20.0, 30.0, 80.0, 70.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::MAGENTA))
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(locked_obj, lid))
        .unwrap();
    let locked_id = *h.doc.objects().keys().max().unwrap();
    let orig_locked = h.doc.object(locked_id).unwrap().payload.clone();

    h.history
        .execute(
            &mut h.doc,
            CommandOp::Group {
                ids: vec![bottom_id, text_id, locked_id],
                name: "MixedGroup".to_owned(),
            },
        )
        .unwrap();
    let grp_id = *h.doc.objects().keys().max().unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::SetObjectLocked {
                ids: vec![locked_id],
                locked: true,
            },
        )
        .unwrap();

    h.selection.select_single(&h.doc, grp_id);
    let mut tc = ToolCore::new(ToolId::Smear);
    let c0 = Point2D::new(30.0, 40.0);
    let c1 = Point2D::new(34.0, 43.0);
    let down = tc.on_pointer_down(&h.ctx(), &PointerEvent::primary(c0, [0.0, 0.0]));
    assert!(
        !matches!(
            down,
            ToolOutput::Consumed { ref actions, .. }
                if actions.iter().any(|a| matches!(a, ToolActionRequest::SelectObjects { .. }))
        ),
        "Must not clobber MixedGroup selection"
    );
    let _ = tc.on_pointer_move(&h.ctx(), &PointerEvent::primary(c1, [4.0, 3.0]));
    let up = tc.on_pointer_up(&h.ctx(), &PointerEvent::primary(c1, [4.0, 3.0]));
    assert!(
        matches!(up, ToolOutput::Commit { ref ops, .. } if ops.len() == 1),
        "Smear on selected MixedGroup must skip LockedChild and TextChild to sculpt BottomVectorChild, got {up:?}"
    );
    h.commit(up);

    let sculpted_bottom = h.doc.object(bottom_id).unwrap().payload.clone();
    assert_ne!(sculpted_bottom, orig_bottom);
    assert_eq!(h.doc.object(locked_id).unwrap().payload, orig_locked);
    assert_eq!(h.doc.object(text_id).unwrap().payload, orig_text);
    assert_eq!(h.selection.objects, vec![grp_id]);

    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(bottom_id).unwrap().payload, orig_bottom);
    h.history.redo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(bottom_id).unwrap().payload, sculpted_bottom);
}

#[test]
fn w1c_r15_sculpt_and_eraser_multi_selection_targets_hit_object_and_preserves_selection() {
    let mut h = Harness::new();
    h.settings.sculpt_radius_mm = 8.0;
    h.settings.eraser_radius_mm = 3.0;

    let rect_a = h.add_rect("RectA", 20.0, 20.0, 40.0, 40.0);
    let rect_b = h.add_rect("RectB", 100.0, 20.0, 40.0, 40.0);
    let rect_c = h.add_rect("UnselectedC", 180.0, 20.0, 30.0, 30.0);

    let orig_a = h.doc.object(rect_a).unwrap().payload.clone();
    let orig_b = h.doc.object(rect_b).unwrap().payload.clone();
    let orig_c = h.doc.object(rect_c).unwrap().payload.clone();

    h.selection.select_many(&h.doc, &[rect_a, rect_b]);
    assert_eq!(h.selection.objects, vec![rect_a, rect_b]);

    // 1. Drag Smear over rect_b's corner (100, 20) -> sculpts rect_b (NOT locked onto rect_a!), preserves [rect_a, rect_b]
    let mut tc = ToolCore::new(ToolId::Smear);
    let down_b = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(100.0, 20.0), [0.0, 0.0]),
    );
    assert!(
        !matches!(
            down_b,
            ToolOutput::Consumed { ref actions, .. }
                if actions.iter().any(|a| matches!(a, ToolActionRequest::SelectObjects { .. }))
        ),
        "Sculpting rect_b in multi-selection must not emit SelectObjects"
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(104.0, 23.0), [4.0, 3.0]),
    );
    let up_b = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(104.0, 23.0), [4.0, 3.0]),
    );
    match &up_b {
        ToolOutput::Commit { post_actions, .. } => {
            assert!(
                !post_actions
                    .iter()
                    .any(|a| matches!(a, ToolActionRequest::SelectObjects { .. })),
                "Commit on multi-selection must preserve selection"
            );
        }
        other => panic!("Expected Commit on rect_b Smear, got {other:?}"),
    }
    h.commit(up_b);
    assert_ne!(h.doc.object(rect_b).unwrap().payload, orig_b);
    assert_eq!(h.doc.object(rect_a).unwrap().payload, orig_a);
    assert_eq!(h.doc.object(rect_c).unwrap().payload, orig_c);
    assert_eq!(h.selection.objects, vec![rect_a, rect_b]);

    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(rect_b).unwrap().payload, orig_b);

    // 2. Direct build_sculpt_stroke_output over rect_b -> also targets rect_b cleanly
    let direct_b = coree_tools::build_sculpt_stroke_output(
        &h.ctx(),
        ToolId::Smooth,
        &[Point2D::new(100.0, 20.0), Point2D::new(102.0, 22.0)],
        false,
    );
    h.commit(direct_b);
    assert_ne!(h.doc.object(rect_b).unwrap().payload, orig_b);
    assert_eq!(h.doc.object(rect_a).unwrap().payload, orig_a);
    h.history.undo(&mut h.doc).unwrap();

    // 3. Drag Smear over unselected rect_c while [rect_a, rect_b] is selected -> noop & zero mutation
    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(180.0, 20.0), [0.0, 0.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(184.0, 23.0), [4.0, 3.0]),
    );
    let up_c = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(184.0, 23.0), [4.0, 3.0]),
    );
    assert_eq!(up_c, ToolOutput::noop());
    assert_eq!(h.doc.object(rect_a).unwrap().payload, orig_a);
    assert_eq!(h.doc.object(rect_b).unwrap().payload, orig_b);
    assert_eq!(h.doc.object(rect_c).unwrap().payload, orig_c);
    assert_eq!(h.selection.objects, vec![rect_a, rect_b]);

    // 4. Overlapping multi-selection [back_rect, front_rect] -> both Smear and Eraser hit frontmost front_rect in Z-order
    let back_rect = h.add_rect("BackMulti", 40.0, 100.0, 50.0, 50.0);
    let front_rect = h.add_rect("FrontMulti", 40.0, 100.0, 50.0, 50.0);
    let orig_back = h.doc.object(back_rect).unwrap().payload.clone();
    let orig_front = h.doc.object(front_rect).unwrap().payload.clone();
    h.selection.select_many(&h.doc, &[back_rect, front_rect]);

    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(40.0, 100.0), [0.0, 0.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(44.0, 103.0), [4.0, 3.0]),
    );
    let up_front = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(44.0, 103.0), [4.0, 3.0]),
    );
    h.commit(up_front);
    assert_ne!(
        h.doc.object(front_rect).unwrap().payload,
        orig_front,
        "Frontmost overlapping selected rect must be sculpted"
    );
    assert_eq!(h.doc.object(back_rect).unwrap().payload, orig_back);
    h.history.undo(&mut h.doc).unwrap();

    let mut tc_era = ToolCore::new(ToolId::Eraser);
    let _ = tc_era.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(60.0, 120.0), [0.0, 0.0]),
    );
    let _ = tc_era.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(65.0, 125.0), [5.0, 5.0]),
    );
    let era_front = tc_era.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(65.0, 125.0), [5.0, 5.0]),
    );
    h.commit(era_front);
    assert_ne!(
        h.doc.object(front_rect).unwrap().payload,
        orig_front,
        "Frontmost overlapping selected rect must be erased"
    );
    assert_eq!(h.doc.object(back_rect).unwrap().payload, orig_back);
}

#[test]
fn w1c_r17_interactive_fill_and_mesh_fill_descend_into_transformed_group_and_preserve_selection() {
    use coree_core::{ConnectorObject, ConnectorStyle};
    use coree_tools::ActiveDragState;

    let mut h = Harness::new();
    let lid = Some(h.layer_id());
    let c1 = h.add_rect("FillC1", 0.0, 0.0, 40.0, 40.0);
    let c2 = h.add_rect("FillC2", 50.0, 0.0, 40.0, 40.0);
    let c_locked = h.add_rect("FillLocked", 100.0, 0.0, 20.0, 20.0);
    let conn = SceneObject::new(
        ObjectId(0),
        "FillConn",
        NodePayload::Connector(ConnectorObject {
            from_obj: None,
            from_anchor: coree_core::AnchorPoint::Center,
            from_fallback: Point2D::new(0.0, 50.0),
            to_obj: None,
            to_anchor: coree_core::AnchorPoint::Center,
            to_fallback: Point2D::new(20.0, 50.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 0.0,
        }),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(conn, lid))
        .unwrap();
    let c_conn = *h.doc.objects().keys().max().unwrap();

    h.history
        .execute(
            &mut h.doc,
            CommandOp::Group {
                ids: vec![c1, c2, c_locked, c_conn],
                name: "FillGrp".to_owned(),
            },
        )
        .unwrap();
    let grp_id = *h.doc.objects().keys().max().unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::TransformObjects {
                ids: vec![grp_id],
                affine: Affine2D::translate(20.0, 30.0),
            },
        )
        .unwrap();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::SetObjectLocked {
                ids: vec![c_locked],
                locked: true,
            },
        )
        .unwrap();

    // 1. Selected transformed Group: InteractiveFill drag (20, 50) -> (110, 50)
    h.selection.select_single(&h.doc, grp_id);
    let mut tc = ToolCore::new(ToolId::InteractiveFill);
    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(20.0, 50.0), [0.0, 0.0]),
    );
    let mv = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(110.0, 50.0), [90.0, 0.0]),
    );
    assert!(
        matches!(mv, ToolOutput::Continue { ref overlay } if !overlay.preview_paths.is_empty()),
        "InteractiveFill drag must emit ray preview overlay, got {mv:?}"
    );
    let up = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(110.0, 50.0), [90.0, 0.0]),
    );
    h.commit(up);

    assert_eq!(h.doc.object(grp_id).unwrap().fill, Fill::None);
    assert_eq!(
        h.doc.object(c_locked).unwrap().fill,
        Fill::Solid(Color::CYAN)
    );
    assert_eq!(h.doc.object(c_conn).unwrap().fill, Fill::None);
    assert_eq!(h.selection.objects, vec![grp_id]);

    match &h.doc.object(c1).unwrap().fill {
        Fill::LinearGradient { start, end, .. } => {
            assert!((start.x - 0.0).abs() < 1e-4 && (start.y - 20.0).abs() < 1e-4);
            assert!((end.x - 90.0).abs() < 1e-4 && (end.y - 20.0).abs() < 1e-4);
        }
        other => panic!("Expected LinearGradient on c1, got {other:?}"),
    }
    match &h.doc.object(c2).unwrap().fill {
        Fill::LinearGradient { start, end, .. } => {
            assert!((start.x - (-50.0)).abs() < 1e-4 && (start.y - 20.0).abs() < 1e-4);
            assert!((end.x - 40.0).abs() < 1e-4 && (end.y - 20.0).abs() < 1e-4);
        }
        other => panic!("Expected LinearGradient on c2, got {other:?}"),
    }

    // Undo / Redo restores both children cleanly
    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(c1).unwrap().fill, Fill::Solid(Color::CYAN));
    assert_eq!(h.doc.object(c2).unwrap().fill, Fill::Solid(Color::CYAN));
    h.history.redo(&mut h.doc).unwrap();
    assert!(matches!(
        h.doc.object(c1).unwrap().fill,
        Fill::LinearGradient { .. }
    ));
    assert!(matches!(
        h.doc.object(c2).unwrap().fill,
        Fill::LinearGradient { .. }
    ));

    // 2. Edit existing child gradient end handle (110, 50) -> (105, 60) while Group is selected
    let down_h = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(110.0, 50.0), [0.0, 0.0]),
    );
    assert!(
        matches!(
            tc.drag_state,
            ActiveDragState::DraggingGradientHandle {
                object_id,
                is_end: true,
                ..
            } if object_id == c2
        ),
        "Expected DraggingGradientHandle on c2, got {:?}",
        tc.drag_state
    );
    assert!(
        !matches!(
            down_h,
            ToolOutput::Consumed { ref actions, .. }
                if actions.iter().any(|a| matches!(a, ToolActionRequest::SelectObjects { .. }))
        ),
        "Dragging child gradient handle must not clobber Group selection"
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(105.0, 60.0), [0.0, 0.0]),
    );
    let up_h = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(105.0, 60.0), [0.0, 0.0]),
    );
    h.commit(up_h);
    assert_eq!(h.selection.objects, vec![grp_id]);
    match &h.doc.object(c2).unwrap().fill {
        Fill::LinearGradient { end, .. } => {
            assert!((end.x - 35.0).abs() < 1e-4 && (end.y - 30.0).abs() < 1e-4);
        }
        other => panic!("Expected updated LinearGradient on c2, got {other:?}"),
    }

    // 3. Double-click on c1's gradient ray at (40, 50) inserts mid-stop while Group is selected
    let mut dbl = PointerEvent::primary(Point2D::new(40.0, 50.0), [0.0, 0.0]);
    dbl.click_count = 2;
    let dbl_out = tc.on_pointer_down(&h.ctx(), &dbl);
    h.commit(dbl_out);
    match &h.doc.object(c1).unwrap().fill {
        Fill::LinearGradient { stops, .. } => {
            assert_eq!(stops.len(), 3, "Double-click on child ray must insert stop");
        }
        other => panic!("Expected LinearGradient with 3 stops on c1, got {other:?}"),
    }

    // 4. Unselected Group: InteractiveFill drag starting on c1 at (30, 50) -> (100, 50)
    h.selection.clear();
    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(30.0, 50.0), [0.0, 0.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(100.0, 50.0), [70.0, 0.0]),
    );
    let up_unsel = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(100.0, 50.0), [70.0, 0.0]),
    );
    h.commit(up_unsel);
    assert_eq!(h.doc.object(grp_id).unwrap().fill, Fill::None);
    assert_eq!(
        h.doc.object(c_locked).unwrap().fill,
        Fill::Solid(Color::CYAN)
    );
    match &h.doc.object(c1).unwrap().fill {
        Fill::LinearGradient { start, end, .. } => {
            assert!((start.x - 10.0).abs() < 1e-4 && (end.x - 80.0).abs() < 1e-4);
        }
        other => panic!("Expected unselected LinearGradient on c1, got {other:?}"),
    }

    // 5. MeshFill on selected Group targets child leaf, preserves selection, and edits node handle
    h.selection.select_single(&h.doc, grp_id);
    tc.set_active_tool(ToolId::MeshFill);
    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(20.0, 30.0), [0.0, 0.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(25.0, 35.0), [5.0, 5.0]),
    );
    let up_mesh = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(25.0, 35.0), [5.0, 5.0]),
    );
    h.commit(up_mesh);
    assert_eq!(h.doc.object(grp_id).unwrap().fill, Fill::None);
    assert_eq!(h.selection.objects, vec![grp_id]);
    assert!(matches!(
        h.doc.object(c1).unwrap().fill,
        Fill::MeshFill { .. }
    ));

    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(25.0, 35.0), [0.0, 0.0]),
    );
    assert!(
        matches!(
            tc.drag_state,
            ActiveDragState::DraggingMeshNode { object_id, .. } if object_id == c1
        ),
        "Expected DraggingMeshNode on c1 inside selected Group, got {:?}",
        tc.drag_state
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(28.0, 38.0), [3.0, 3.0]),
    );
    let up_mnode = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(28.0, 38.0), [3.0, 3.0]),
    );
    h.commit(up_mnode);
    assert_eq!(h.selection.objects, vec![grp_id]);
}

#[test]
fn w1c_r17_eraser_across_group_multi_selection_and_unselected_erases_all_crossed_objects() {
    let mut h = Harness::new();
    h.settings.eraser_radius_mm = 6.0;

    // 1. Selected Group([c1, c2, c3]): stroke across c1 and c2 (not touching c3)
    let c1 = h.add_rect("GrpEraC1", 30.0, 30.0, 40.0, 40.0);
    let c2 = h.add_rect("GrpEraC2", 90.0, 30.0, 40.0, 40.0);
    let c3 = h.add_rect("GrpEraC3", 160.0, 30.0, 40.0, 40.0);
    let orig_c1 = h.doc.object(c1).unwrap().payload.clone();
    let orig_c2 = h.doc.object(c2).unwrap().payload.clone();
    let orig_c3 = h.doc.object(c3).unwrap().payload.clone();
    h.history
        .execute(
            &mut h.doc,
            CommandOp::Group {
                ids: vec![c1, c2, c3],
                name: "EraGrp".to_owned(),
            },
        )
        .unwrap();
    let grp_id = *h.doc.objects().keys().max().unwrap();
    h.selection.select_single(&h.doc, grp_id);

    let mut tc = ToolCore::new(ToolId::Eraser);
    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(25.0, 50.0), [0.0, 0.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(135.0, 50.0), [110.0, 0.0]),
    );
    let up_grp = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(135.0, 50.0), [110.0, 0.0]),
    );
    assert!(
        matches!(up_grp, ToolOutput::Commit { ref ops, .. } if ops.len() == 2),
        "Expected 2 atomic ops for Group([c1, c2]), got {up_grp:?}"
    );
    h.commit(up_grp);
    let erased_c1 = h.doc.object(c1).unwrap().payload.clone();
    let erased_c2 = h.doc.object(c2).unwrap().payload.clone();
    assert_ne!(erased_c1, orig_c1, "c1 in Group must be erased");
    assert_ne!(erased_c2, orig_c2, "c2 in Group must be erased");
    assert_eq!(h.doc.object(c3).unwrap().payload, orig_c3);

    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(c1).unwrap().payload, orig_c1);
    assert_eq!(h.doc.object(c2).unwrap().payload, orig_c2);
    h.history.redo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(c1).unwrap().payload, erased_c1);
    assert_eq!(h.doc.object(c2).unwrap().payload, erased_c2);

    // 2. Multi-selection [m1, m2]: stroke across both erases both atomically
    let m1 = h.add_rect("MultiEra1", 30.0, 100.0, 40.0, 40.0);
    let m2 = h.add_rect("MultiEra2", 90.0, 100.0, 40.0, 40.0);
    let orig_m1 = h.doc.object(m1).unwrap().payload.clone();
    let orig_m2 = h.doc.object(m2).unwrap().payload.clone();
    h.selection.select_many(&h.doc, &[m1, m2]);

    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(25.0, 120.0), [0.0, 0.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(135.0, 120.0), [110.0, 0.0]),
    );
    let up_multi = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(135.0, 120.0), [110.0, 0.0]),
    );
    assert!(
        matches!(up_multi, ToolOutput::Commit { ref ops, .. } if ops.len() == 2),
        "Expected 2 atomic ops for multi-selection [m1, m2], got {up_multi:?}"
    );
    h.commit(up_multi);
    assert_ne!(h.doc.object(m1).unwrap().payload, orig_m1);
    assert_ne!(h.doc.object(m2).unwrap().payload, orig_m2);
    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(m1).unwrap().payload, orig_m1);
    assert_eq!(h.doc.object(m2).unwrap().payload, orig_m2);

    // 3. Unselected [u1, u2]: stroke across both erases both atomically
    let u1 = h.add_rect("UnselEra1", 30.0, 160.0, 40.0, 40.0);
    let u2 = h.add_rect("UnselEra2", 90.0, 160.0, 40.0, 40.0);
    let orig_u1 = h.doc.object(u1).unwrap().payload.clone();
    let orig_u2 = h.doc.object(u2).unwrap().payload.clone();
    h.selection.clear();

    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(25.0, 180.0), [0.0, 0.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(135.0, 180.0), [110.0, 0.0]),
    );
    let up_unsel = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(135.0, 180.0), [110.0, 0.0]),
    );
    assert!(
        matches!(up_unsel, ToolOutput::Commit { ref ops, .. } if ops.len() == 2),
        "Expected 2 atomic ops for unselected [u1, u2], got {up_unsel:?}"
    );
    h.commit(up_unsel);
    assert_ne!(h.doc.object(u1).unwrap().payload, orig_u1);
    assert_ne!(h.doc.object(u2).unwrap().payload, orig_u2);
    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.object(u1).unwrap().payload, orig_u1);
    assert_eq!(h.doc.object(u2).unwrap().payload, orig_u2);
}

#[test]
fn symmetry_drawing_mode_attaches_effect_and_preview_guides() {
    let mut h = Harness::new();
    h.settings.symmetry_enabled = true;
    h.settings.symmetry_folds = 6;
    h.settings.symmetry_mirror = true;
    h.settings.symmetry_center_mm = Some(Point2D::new(100.0, 100.0));

    let mut tc = ToolCore::new(ToolId::Rectangle);
    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(110.0, 110.0), [110.0, 110.0]),
    );
    let out_move = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(130.0, 130.0), [130.0, 130.0]),
    );
    let overlay = match out_move {
        ToolOutput::Continue { overlay } => overlay,
        other => panic!("expected Continue overlay, got {other:?}"),
    };
    assert!(
        overlay.preview_paths.len() >= 7,
        "expected box preview + 6 symmetry axis guides, got {}",
        overlay.preview_paths.len()
    );

    let out_up = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(130.0, 130.0), [130.0, 130.0]),
    );
    h.commit(out_up);
    let last_id = h.doc.pages()[0].layers[0].objects.last().copied().unwrap();
    let obj = h.doc.object(last_id).unwrap();
    assert!(obj.effects.iter().any(|e| matches!(
        e,
        Effect::Symmetry {
            axes: 6,
            mirror: true,
            ..
        }
    )));
}

#[test]
fn table_tool_live_grid_preview_and_commit() {
    let mut h = Harness::new();
    h.settings.table_rows = 3;
    h.settings.table_cols = 4;

    let mut tc = ToolCore::new(ToolId::Table);
    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(20.0, 20.0), [20.0, 20.0]),
    );
    let out_move = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(100.0, 80.0), [100.0, 80.0]),
    );
    let overlay = match out_move {
        ToolOutput::Continue { overlay } => overlay,
        other => panic!("expected Continue overlay, got {other:?}"),
    };
    // Outer rectangle (1) + 2 horizontal dividers + 3 vertical dividers = 6 preview paths
    assert_eq!(overlay.preview_paths.len(), 6);

    let out_up = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(100.0, 80.0), [100.0, 80.0]),
    );
    h.commit(out_up);
    let last_id = h.doc.pages()[0].layers[0].objects.last().copied().unwrap();
    assert!(matches!(
        &h.doc.object(last_id).unwrap().payload,
        NodePayload::Table(t) if t.rows == 3 && t.cols == 4
    ));
}

#[test]
fn rem10_continuity_eraser_hollow_interior_bspline_and_powerclip_marquee() {
    let mut h = Harness::new();

    // 1. Hollow selected Fill::None rect is NOT erased when dragging inside its interior
    let hollow = SceneObject::new(
        ObjectId(0),
        "Hollow",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 80.0,
            height: 80.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::None)
    .with_transform(Affine2D::translate(20.0, 20.0));
    let hollow_id = h.doc.add_object(hollow, None).unwrap();
    h.selection.select_many(&h.doc, &[hollow_id]);
    let mut tc = ToolCore::new(ToolId::Eraser);
    let _ = tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(50.0, 50.0), [50.0, 50.0]),
    );
    let _ = tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(65.0, 65.0), [65.0, 65.0]),
    );
    let up = tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(65.0, 65.0), [65.0, 65.0]),
    );
    assert!(
        !matches!(up, ToolOutput::Commit { .. }),
        "Expected hollow Fill::None interior erase not to commit, got {up:?}"
    );

    // 2. B-Spline tool creates NodePayload::BSpline and ShapeNode drags a control point
    let mut bsp_tc = ToolCore::new(ToolId::BSpline);
    for pt in [
        Point2D::new(10.0, 10.0),
        Point2D::new(40.0, 10.0),
        Point2D::new(40.0, 40.0),
        Point2D::new(10.0, 40.0),
    ] {
        let _ = bsp_tc.on_pointer_down(
            &h.ctx(),
            &PointerEvent::primary(pt, [pt.x as f32, pt.y as f32]),
        );
        let _ = bsp_tc.on_pointer_up(
            &h.ctx(),
            &PointerEvent::primary(pt, [pt.x as f32, pt.y as f32]),
        );
    }
    let commit_bsp = bsp_tc.confirm(&h.ctx());
    h.commit(commit_bsp);
    let bsp_id = h.doc.pages()[0].layers[0].objects.last().copied().unwrap();
    assert!(matches!(
        h.doc.object(bsp_id).unwrap().payload,
        NodePayload::BSpline(_)
    ));

    h.selection.select_many(&h.doc, &[bsp_id]);
    let mut shape_tc = ToolCore::new(ToolId::ShapeNode);
    let _ = shape_tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(40.0, 10.0), [40.0, 10.0]),
    );
    let _ = shape_tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(55.0, 15.0), [55.0, 15.0]),
    );
    let edit_out = shape_tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(55.0, 15.0), [55.0, 15.0]),
    );
    h.commit(edit_out);
    match &h.doc.object(bsp_id).unwrap().payload {
        NodePayload::BSpline(bs) => {
            assert!((bs.control_points[1].x - 55.0).abs() < 1e-6);
        }
        other => panic!("expected BSpline, got {other:?}"),
    }
}

#[test]
fn w2b_r2_shape_node_edits_artistic_media_spine() {
    use coree_core::{ArtisticMediaStyle, ArtisticWidthProfile};
    use coree_tools::ActiveDragState;

    let mut h = Harness::new();
    let spine = BezierPath::from_line(Point2D::new(20.0, 20.0), Point2D::new(80.0, 20.0));
    let style = ArtisticMediaStyle::Preset {
        max_width_mm: 6.0,
        profile: ArtisticWidthProfile::TaperBoth,
    };
    let obj = SceneObject::new(
        ObjectId(0),
        "ArtMedia",
        NodePayload::ArtisticMedia {
            spine,
            style: style.clone(),
        },
    )
    .with_fill(Fill::Solid(Color::rgba8(0, 64, 255, 255)));
    h.history
        .execute(&mut h.doc, CommandOp::create_object(obj, None))
        .unwrap();
    let am_id = h.doc.pages()[0].layers[0].objects.last().copied().unwrap();
    h.selection.select_single(&h.doc, am_id);

    let mut shape_tc = ToolCore::new(ToolId::ShapeNode);
    let idle_overlay = shape_tc.build_overlay(&h.ctx());
    assert!(
        idle_overlay.preview_handles.len() >= 2,
        "ShapeNode idle overlay must expose ArtisticMedia spine node handles"
    );

    let down = shape_tc.on_pointer_down(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(80.0, 20.0), [80.0, 20.0]),
    );
    assert!(
        matches!(
            shape_tc.drag_state,
            ActiveDragState::DraggingArtisticMediaNode { object_id, .. } if object_id == am_id
        ),
        "Expected DraggingArtisticMediaNode, got {:?}",
        shape_tc.drag_state
    );
    assert!(matches!(down, ToolOutput::Continue { .. }));

    let mv = shape_tc.on_pointer_move(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(95.0, 45.0), [95.0, 45.0]),
    );
    assert!(
        matches!(mv, ToolOutput::Continue { ref overlay } if !overlay.preview_paths.is_empty())
    );

    let up = shape_tc.on_pointer_up(
        &h.ctx(),
        &PointerEvent::primary(Point2D::new(95.0, 45.0), [95.0, 45.0]),
    );
    assert!(
        matches!(
            up,
            ToolOutput::Commit { ref ops, .. }
                if matches!(ops.as_slice(), [CommandOp::EditArtisticMedia { id, .. }] if *id == am_id)
        ),
        "Expected CommandOp::EditArtisticMedia commit, got {up:?}"
    );
    h.commit(up);

    match &h.doc.object(am_id).unwrap().payload {
        NodePayload::ArtisticMedia {
            spine: edited_spine,
            style: edited_style,
        } => {
            let end_pt = edited_spine.subpaths[0].nodes[1].pos;
            assert!((end_pt.x - 95.0).abs() < 1e-6 && (end_pt.y - 45.0).abs() < 1e-6);
            assert_eq!(edited_style, &style);
        }
        other => panic!("expected ArtisticMedia, got {other:?}"),
    }
}

#[test]
fn w2b_r3_powerclip_edit_mode_pick_marquee_and_empty_clicks_ignore_container_and_outer() {
    use coree_core::Rect2D;
    use coree_tools::{hit_test_powerclip_edit_point, ActiveDragState};

    let mut h = Harness::new();
    let lid = Some(h.layer_id());

    // Large outer background object overlapping the PowerClip container
    let outer_rect = SceneObject::new(
        ObjectId(0),
        "OuterBg",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 150.0,
            height: 150.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(128, 128, 128, 255)))
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(outer_rect, lid))
        .unwrap();
    let outer_id = *h.doc.objects().keys().max().unwrap();

    // Two inner PowerClip children: [40,40]..[55,55] and [80,80]..[95,95]
    let in1 = SceneObject::new(
        ObjectId(0),
        "Inner1",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            40.0, 40.0, 15.0, 15.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::CYAN))
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(in1, lid))
        .unwrap();
    let in1_id = *h.doc.objects().keys().max().unwrap();

    let in2 = SceneObject::new(
        ObjectId(0),
        "Inner2",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            80.0, 80.0, 15.0, 15.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::MAGENTA))
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(in2, lid))
        .unwrap();
    let in2_id = *h.doc.objects().keys().max().unwrap();

    // Filled PowerClip container [20, 20]..[120, 120]
    let pc_obj = SceneObject::new(
        ObjectId(0),
        "PowerClipBox",
        NodePayload::PowerClip {
            container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                20.0, 20.0, 100.0, 100.0,
            ))),
            container_parametric: None,
            contents: vec![in1_id, in2_id],
            lock_contents: false,
        },
    )
    .with_fill(Fill::Solid(Color::WHITE))
    .with_outline(OutlineStyle::default());
    h.history
        .execute(&mut h.doc, CommandOp::create_object(pc_obj, lid))
        .unwrap();
    let pc_id = *h.doc.objects().keys().max().unwrap();

    assert_eq!(
        hit_test_powerclip_edit_point(&h.doc, pc_id, true, Point2D::new(25.0, 25.0), 3.0),
        None
    );
    assert_eq!(
        hit_test_powerclip_edit_point(&h.doc, pc_id, true, Point2D::new(47.5, 47.5), 3.0),
        Some(in1_id)
    );

    // Pre-select an inner object so empty-space click verification confirms clearing selection.
    h.selection.select_single(&h.doc, in1_id);
    let mut ctx = h.ctx();
    ctx.powerclip_edit_id = Some(pc_id);
    ctx.treat_all_objects_as_filled = true;

    // 1. Pick marquee starting on empty space INSIDE filled PowerClip container (also over outer_id)
    let mut pick = ToolCore::new(ToolId::Pick);
    let start_pt = Point2D::new(25.0, 25.0);
    let end_pt = Point2D::new(100.0, 100.0);
    let _down = pick.on_pointer_down(&ctx, &PointerEvent::primary(start_pt, [25.0, 25.0]));
    assert!(
        matches!(pick.drag_state, ActiveDragState::MarqueeSelect { .. }),
        "Pick pointer-down on empty space inside filled PowerClip must enter MarqueeSelect, got {:?}",
        pick.drag_state
    );
    let _mv = pick.on_pointer_move(&ctx, &PointerEvent::primary(end_pt, [100.0, 100.0]));
    let up = pick.on_pointer_up(&ctx, &PointerEvent::primary(end_pt, [100.0, 100.0]));
    match up {
        ToolOutput::Consumed { actions, .. } => {
            assert!(
                actions.iter().any(|a| matches!(
                    a,
                    ToolActionRequest::SelectObjects { ids, .. }
                        if ids.contains(&in1_id)
                            && ids.contains(&in2_id)
                            && !ids.contains(&pc_id)
                            && !ids.contains(&outer_id)
                )),
                "Marquee inside PowerClip edit mode must select only inner contents, got {actions:?}"
            );
        }
        other => panic!("Expected Consumed SelectObjects from marquee, got {other:?}"),
    }

    // 2. Zero-drag empty-space clicks inside pc_id and over outer_id clear selection (never select pc_id/outer_id)
    for empty_pt in [Point2D::new(25.0, 25.0), Point2D::new(135.0, 135.0)] {
        let mut pick_click = ToolCore::new(ToolId::Pick);
        let _ = pick_click.on_pointer_down(&ctx, &PointerEvent::primary(empty_pt, [0.0, 0.0]));
        let click_up = pick_click.on_pointer_up(&ctx, &PointerEvent::primary(empty_pt, [0.0, 0.0]));
        match click_up {
            ToolOutput::Consumed { actions, .. } => {
                assert!(
                    actions
                        .iter()
                        .any(|a| matches!(a, ToolActionRequest::ClearSelection)),
                    "Empty-space Pick click at {empty_pt:?} in PowerClip edit mode must emit ClearSelection, got {actions:?}"
                );
            }
            other => panic!("Expected Consumed ClearSelection, got {other:?}"),
        }

        let mut lasso_click = ToolCore::new(ToolId::FreehandPick);
        let _ = lasso_click.on_pointer_down(&ctx, &PointerEvent::primary(empty_pt, [0.0, 0.0]));
        let lasso_up =
            lasso_click.on_pointer_up(&ctx, &PointerEvent::primary(empty_pt, [0.0, 0.0]));
        match lasso_up {
            ToolOutput::Consumed { actions, .. } => {
                assert!(
                    actions
                        .iter()
                        .any(|a| matches!(a, ToolActionRequest::ClearSelection)),
                    "Empty-space FreehandPick click at {empty_pt:?} in PowerClip edit mode must emit ClearSelection, got {actions:?}"
                );
            }
            other => panic!("Expected Consumed ClearSelection for Lasso, got {other:?}"),
        }

        let mut node_click = ToolCore::new(ToolId::ShapeNode);
        let _ = node_click.on_pointer_down(&ctx, &PointerEvent::primary(empty_pt, [0.0, 0.0]));
        let node_up = node_click.on_pointer_up(&ctx, &PointerEvent::primary(empty_pt, [0.0, 0.0]));
        assert!(
            matches!(node_up, ToolOutput::Consumed { ref actions, .. } if actions.is_empty())
                || matches!(node_up, ToolOutput::Continue { .. }),
            "Empty-space ShapeNode click at {empty_pt:?} must not select pc_id/outer_id, got {node_up:?}"
        );
    }

    // 3. Ctrl+Click outside pc_id still emits ExitPowerClipEdit
    let mut ctrl_ev = PointerEvent::primary(Point2D::new(135.0, 135.0), [135.0, 135.0]);
    ctrl_ev.modifiers.ctrl = true;
    let mut pick_exit = ToolCore::new(ToolId::Pick);
    let ctrl_down = pick_exit.on_pointer_down(&ctx, &ctrl_ev);
    match ctrl_down {
        ToolOutput::Consumed { actions, .. } => {
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, ToolActionRequest::ExitPowerClipEdit)),
                "Ctrl+Click outside PowerClip container must emit ExitPowerClipEdit, got {actions:?}"
            );
        }
        other => panic!("Expected Consumed ExitPowerClipEdit, got {other:?}"),
    }
}
