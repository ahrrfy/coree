// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! End-to-end headless GUI pointer-interaction integration tests (`egui::Context::run_ui`)
//! proving `StudioApp::render_ui` -> `ToolCore` -> `TransientOverlay` -> `CommandHistory`
//! -> `DocumentInvalidation::Targeted` without mid-drag `Document` mutation or full-cache
//! invalidations.

use coree_app::state::{StudioApp, ToolId};
use coree_core::{
    Affine2D, Color, ColorMode, CommandOp, CornerStyle, Document, Fill, NodePayload, ObjectId,
    OutlineStyle, ParametricShape, Point2D, SceneObject,
};
use coree_prod::{ObjectKindFilter, ReplaceAction};
use egui::{pos2, vec2, Context, Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect};

struct UiDriver {
    ctx: Context,
    app: StudioApp,
    time: f64,
    screen: Rect,
}

impl UiDriver {
    fn new() -> Self {
        let mut app = StudioApp::new_headless();
        app.new_document(Document::new(
            "Pointer Test",
            210.0,
            297.0,
            ColorMode::Cmyk,
            300.0,
        ));
        app.show_welcome = false;
        app.show_dockers = false;
        app.fonts_configured = true;
        {
            let tab = app.active_tab_mut().unwrap();
            tab.snap_options.grid = false;
            tab.snap_options.guidelines = false;
            tab.snap_options.page_edges = false;
            tab.snap_options.objects = false;
            tab.snap_options.nodes = false;
            tab.snap_options.dynamic_guides = false;
        }
        let mut driver = Self {
            ctx: Context::default(),
            app,
            time: 0.0,
            screen: Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0)),
        };
        // Frame 0: establish layout, canvas_rect, and initial full tile render
        driver.step(Vec::new(), Modifiers::NONE);
        driver
    }

    fn step(&mut self, mut events: Vec<Event>, modifiers: Modifiers) {
        self.time += 0.02;
        if modifiers != Modifiers::NONE {
            events.insert(0, Event::ModifiersChanged(modifiers));
        }
        let input = RawInput {
            screen_rect: Some(self.screen),
            time: Some(self.time),
            events,
            ..RawInput::default()
        };
        let mut out = self.ctx.run_ui(input, |ui| self.app.render_ui(ui));
        out.textures_delta.clear();
    }

    fn world_to_pos2(&self, world: Point2D) -> Pos2 {
        let [x, y] = self
            .app
            .active_tab()
            .unwrap()
            .world_to_canvas_screen_pos(world)
            .expect("canvas_rect must be populated after first render_ui frame");
        pos2(x, y)
    }

    fn add_rect(&mut self, name: &str, x: f64, y: f64, w: f64, h: f64, color: Color) -> ObjectId {
        let lid = self.app.active_tab().unwrap().active_layer_id;
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
        .with_outline(OutlineStyle::default())
        .with_transform(Affine2D::translate(x, y));
        self.app
            .execute_command(CommandOp::create_object(obj, lid))
            .unwrap();
        *self
            .app
            .active_tab()
            .unwrap()
            .document
            .objects()
            .keys()
            .last()
            .unwrap()
    }
}

#[test]
fn pointer_rectangle_draw_undo_redo_through_egui_render_ui() {
    let mut d = UiDriver::new();
    d.app.select_tool(ToolId::Rectangle);
    let p0 = d.world_to_pos2(Point2D::new(30.0, 40.0));
    let p1 = d.world_to_pos2(Point2D::new(90.0, 80.0));

    // Move to p0 and press primary
    d.step(
        vec![
            Event::PointerMoved(p0),
            Event::PointerButton {
                pos: p0,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    assert_eq!(
        d.app.active_tab().unwrap().document.objects().len(),
        0,
        "Document must not contain object mid-drag"
    );

    // Drag to p1 -> transient overlay must be populated
    d.step(vec![Event::PointerMoved(p1)], Modifiers::NONE);
    assert_eq!(d.app.active_tab().unwrap().document.objects().len(), 0);
    assert!(
        !d.app
            .active_tab()
            .unwrap()
            .transient_overlay
            .preview_paths
            .is_empty(),
        "Drawing Rectangle must emit TransientOverlay preview_paths"
    );

    // Release -> commits Rectangle cleanly
    d.step(
        vec![Event::PointerButton {
            pos: p1,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert_eq!(d.app.active_tab().unwrap().document.objects().len(), 1);
    let id = *d
        .app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .next()
        .unwrap();
    let bb = d
        .app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(id)
        .unwrap();
    assert!((bb.min_x - 30.0).abs() < 1.5 && (bb.width() - 60.0).abs() < 2.0);
    assert!((bb.min_y - 40.0).abs() < 1.5 && (bb.height() - 40.0).abs() < 2.0);

    d.app.undo().expect("undo created rect");
    assert_eq!(d.app.active_tab().unwrap().document.objects().len(), 0);
    d.app.redo().expect("redo created rect");
    assert_eq!(d.app.active_tab().unwrap().document.objects().len(), 1);
}

#[test]
fn pointer_pick_move_targeted_dirty_tiles_less_than_total() {
    let mut d = UiDriver::new();
    let id = d.add_rect("Box", 40.0, 40.0, 20.0, 20.0, Color::CYAN);
    // Render one clean frame so all tiles start clean
    d.step(Vec::new(), Modifiers::NONE);
    let total_tiles = d.app.active_tab().unwrap().tile_cache.tile_count();
    assert!(total_tiles >= 4, "expected >=4 tiles, got {total_tiles}");
    assert_eq!(d.app.active_tab().unwrap().tile_cache.dirty_tile_count(), 0);

    let orig_bb = d
        .app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(id)
        .unwrap();
    let p0 = d.world_to_pos2(Point2D::new(50.0, 50.0));
    let p1 = d.world_to_pos2(Point2D::new(70.0, 55.0));

    d.step(
        vec![
            Event::PointerMoved(p0),
            Event::PointerButton {
                pos: p0,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(vec![Event::PointerMoved(p1)], Modifiers::NONE);

    // Mid-drag: Document untouched AND tile cache NOT dirtied!
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .world_object_bounds(id)
            .unwrap(),
        orig_bb
    );
    assert_eq!(
        d.app.active_tab().unwrap().tile_cache.dirty_tile_count(),
        0,
        "Drag preview MUST NOT dirty TileCache!"
    );

    // Release -> commits TransformObjects with Targeted dirty-tile invalidation
    d.step(
        vec![Event::PointerButton {
            pos: p1,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    let moved_bb = d
        .app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(id)
        .unwrap();
    assert!((moved_bb.min_x - 60.0).abs() < 1.5);
    assert!((moved_bb.min_y - 45.0).abs() < 1.5);

    let dirty_tiles = d.app.active_tab().unwrap().tile_cache.dirty_tile_count();
    assert!(
        dirty_tiles > 0 && dirty_tiles < total_tiles,
        "Targeted commit must dirty >0 and <total tiles ({dirty_tiles} vs {total_tiles})"
    );
}

#[test]
fn pointer_pick_move_escape_cancel_leaves_document_untouched() {
    let mut d = UiDriver::new();
    let id = d.add_rect("EscBox", 40.0, 40.0, 30.0, 30.0, Color::MAGENTA);
    d.step(Vec::new(), Modifiers::NONE);
    let orig_bb = d
        .app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(id)
        .unwrap();
    let p0 = d.world_to_pos2(Point2D::new(55.0, 55.0));
    let p1 = d.world_to_pos2(Point2D::new(110.0, 90.0));

    d.step(
        vec![
            Event::PointerMoved(p0),
            Event::PointerButton {
                pos: p0,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(vec![Event::PointerMoved(p1)], Modifiers::NONE);
    assert!(d.app.active_tab().unwrap().tool_core.is_dragging());

    // Press Escape mid-drag -> cancels cleanly
    d.step(
        vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(!d.app.active_tab().unwrap().tool_core.is_dragging());
    assert!(d
        .app
        .active_tab()
        .unwrap()
        .transient_overlay
        .preview_paths
        .is_empty());
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .world_object_bounds(id)
            .unwrap(),
        orig_bb
    );

    // Releasing pointer after Escape does not commit
    d.step(
        vec![Event::PointerButton {
            pos: p1,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .world_object_bounds(id)
            .unwrap(),
        orig_bb
    );
}

#[test]
fn pointer_space_stamp_duplicate_on_rotated_group_child() {
    let mut d = UiDriver::new();
    let child_id = d.add_rect("Child", 0.0, 0.0, 20.0, 10.0, Color::CYAN);
    let rot90 =
        Affine2D::translate(100.0, 50.0).compose(&Affine2D::rotate(std::f64::consts::FRAC_PI_2));
    {
        let tab = d.app.active_tab_mut().unwrap();
        let grp_id = tab.document.group_objects(&[child_id], "RotGrp").unwrap();
        tab.document.transform_object(grp_id, rot90).unwrap();
        tab.selection.select_single(&tab.document, child_id);
        tab.viewport.zoom = 1.5;
        tab.viewport.pan_mm = Point2D::ZERO;
    }
    d.step(Vec::new(), Modifiers::NONE);

    let orig_world = d
        .app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    let p0 = d.world_to_pos2(orig_world.center());
    let p1 = d.world_to_pos2(Point2D::new(
        orig_world.center().x + 25.0,
        orig_world.center().y + 10.0,
    ));

    // Press & drag rotated group child
    d.step(
        vec![
            Event::PointerMoved(p0),
            Event::PointerButton {
                pos: p0,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(vec![Event::PointerMoved(p1)], Modifiers::NONE);
    assert!(
        d.app
            .active_tab()
            .unwrap()
            .tool_core
            .is_transform_dragging(),
        "expected transform drag at p0={p0:?} rect={:?}, got {:?}",
        d.app.active_tab().unwrap().last_canvas_rect,
        d.app.active_tab().unwrap().tool_core.drag_state
    );
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .world_object_bounds(child_id)
            .unwrap(),
        orig_world
    );

    // Spacebar mid-drag stamps duplicate at live drag position!
    d.step(
        vec![Event::Key {
            key: Key::Space,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    // Original child + stamped clone exist (3 objects including group)
    assert_eq!(d.app.active_tab().unwrap().document.objects().len(), 3);

    // Release finishes move on original child
    d.step(
        vec![Event::PointerButton {
            pos: p1,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    let moved_world = d
        .app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(child_id)
        .unwrap();
    assert!((moved_world.min_x - (orig_world.min_x + 25.0)).abs() < 1.5);
    assert!((moved_world.min_y - (orig_world.min_y + 10.0)).abs() < 1.5);

    d.app.undo().expect("undo child move");
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .world_object_bounds(child_id)
            .unwrap(),
        orig_world
    );
    d.app.undo().expect("undo stamp duplicate");
    assert_eq!(d.app.active_tab().unwrap().document.objects().len(), 2);
}

#[test]
fn pointer_ctrl_square_and_shape_node_corner_handle_drag() {
    let mut d = UiDriver::new();
    d.app.select_tool(ToolId::Rectangle);
    let p0 = d.world_to_pos2(Point2D::new(20.0, 20.0));
    let p1 = d.world_to_pos2(Point2D::new(80.0, 50.0));

    // Ctrl+drag produces exact 60x60 square
    d.step(
        vec![
            Event::PointerMoved(p0),
            Event::PointerButton {
                pos: p0,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::CTRL,
            },
        ],
        Modifiers::CTRL,
    );
    d.step(vec![Event::PointerMoved(p1)], Modifiers::CTRL);
    d.step(
        vec![Event::PointerButton {
            pos: p1,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::CTRL,
        }],
        Modifiers::CTRL,
    );
    let rect_id = *d
        .app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();
    let sq_bb = d
        .app
        .active_tab()
        .unwrap()
        .document
        .world_object_bounds(rect_id)
        .unwrap();
    assert!((sq_bb.width() - sq_bb.height()).abs() < 1.0);
    assert!((sq_bb.width() - 60.0).abs() < 2.0);

    // Switch to ShapeNode and drag top-left corner handle inward to round corners
    d.app.select_tool(ToolId::ShapeNode);
    let tl = d.world_to_pos2(sq_bb.min());
    let tl_in = d.world_to_pos2(Point2D::new(sq_bb.min_x + 12.0, sq_bb.min_y + 12.0));
    d.step(
        vec![
            Event::PointerMoved(tl),
            Event::PointerButton {
                pos: tl,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(vec![Event::PointerMoved(tl_in)], Modifiers::NONE);
    d.step(
        vec![Event::PointerButton {
            pos: tl_in,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );

    match &d
        .app
        .active_tab()
        .unwrap()
        .document
        .object(rect_id)
        .unwrap()
        .payload
    {
        NodePayload::Shape(ParametricShape::Rectangle { corner_radii, .. }) => {
            assert!(
                corner_radii[0] > 5.0,
                "Expected rounded corner radius > 5mm, got {:?}",
                corner_radii
            );
        }
        other => panic!("expected Rectangle, got {other:?}"),
    }
}

#[test]
fn find_and_replace_non_cloning_executes_command_and_undo_redo() {
    let mut app = StudioApp::new_headless();
    app.new_document(Document::new(
        "FNR Test",
        210.0,
        297.0,
        ColorMode::Cmyk,
        300.0,
    ));
    let lid = app.active_tab().unwrap().active_layer_id;
    let r1 = SceneObject::new(
        ObjectId(0),
        "CyanRect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::CYAN));
    app.execute_command(CommandOp::create_object(r1, lid))
        .unwrap();
    let id1 = *app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .last()
        .unwrap();

    app.find_replace_state.object_kind = Some(ObjectKindFilter::Shape);
    app.find_replace_state.replace_fill_color = Color::YELLOW;
    let rep = app.run_find_and_replace_action(ReplaceAction::ReplaceFillColor {
        color: Color::YELLOW,
    });
    assert_eq!(rep.replacements_applied, 1);
    assert_eq!(
        app.active_tab().unwrap().document.object(id1).unwrap().fill,
        Fill::Solid(Color::YELLOW)
    );

    app.undo().expect("undo FindAndReplace");
    assert_eq!(
        app.active_tab().unwrap().document.object(id1).unwrap().fill,
        Fill::Solid(Color::CYAN)
    );
    app.redo().expect("redo FindAndReplace");
    assert_eq!(
        app.active_tab().unwrap().document.object(id1).unwrap().fill,
        Fill::Solid(Color::YELLOW)
    );
}

#[test]
fn w1c_r2_pick_click_and_handle_double_click_toggle_rotate_skew_and_pivot_drag() {
    use coree_core::TransformMode;

    let mut d = UiDriver::new();
    let id = d.add_rect("Box", 40.0, 40.0, 40.0, 30.0, Color::CYAN);
    d.step(Vec::new(), Modifiers::NONE);

    let center = d.world_to_pos2(Point2D::new(60.0, 55.0));

    // 1. Click to select in ScaleAndMove
    d.step(
        vec![
            Event::PointerMoved(center),
            Event::PointerButton {
                pos: center,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: center,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert_eq!(d.app.active_tab().unwrap().selection.objects, vec![id]);
    assert_eq!(
        d.app.active_tab().unwrap().selection.transform_mode,
        TransformMode::ScaleAndMove
    );

    // 2. Second single click (after advancing time past double-click window) -> RotateAndSkew
    d.time += 1.0;
    d.step(
        vec![Event::PointerButton {
            pos: center,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: center,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert_eq!(
        d.app.active_tab().unwrap().selection.transform_mode,
        TransformMode::RotateAndSkew
    );

    // 3. Dragging pivot crosshair in RotateAndSkew moves selection.pivot
    let p_pivot = d.world_to_pos2(Point2D::new(60.0, 55.0));
    let p_new_pivot = d.world_to_pos2(Point2D::new(85.0, 75.0));
    d.time += 1.0;
    d.step(
        vec![
            Event::PointerMoved(p_pivot),
            Event::PointerButton {
                pos: p_pivot,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(vec![Event::PointerMoved(p_new_pivot)], Modifiers::NONE);
    d.step(
        vec![Event::PointerButton {
            pos: p_new_pivot,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(d.app.active_tab().unwrap().selection.custom_pivot);
    assert!((d.app.active_tab().unwrap().selection.pivot.x - 85.0).abs() < 1.5);

    // 4. Double-click on selection handle toggles RotateAndSkew -> ScaleAndMove
    let br = d.world_to_pos2(Point2D::new(80.0, 70.0));
    d.time += 1.0;
    for _ in 0..2 {
        d.step(
            vec![
                Event::PointerMoved(br),
                Event::PointerButton {
                    pos: br,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
            Modifiers::NONE,
        );
        d.step(
            vec![Event::PointerButton {
                pos: br,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
            Modifiers::NONE,
        );
    }
    assert_eq!(
        d.app.active_tab().unwrap().selection.transform_mode,
        TransformMode::ScaleAndMove
    );

    // 5. Shift+Click on selected object deselects it
    d.time += 1.0;
    d.step(
        vec![
            Event::PointerMoved(center),
            Event::PointerButton {
                pos: center,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::SHIFT,
            },
        ],
        Modifiers::SHIFT,
    );
    d.step(
        vec![Event::PointerButton {
            pos: center,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::SHIFT,
        }],
        Modifiers::SHIFT,
    );
    assert!(d.app.active_tab().unwrap().selection.is_empty());
}

#[test]
fn w1c_r2_grouped_drop_shadow_handle_drag_preserves_offset() {
    use coree_core::{Effect, FeatherDirection};

    let mut d = UiDriver::new();
    let child_id = d.add_rect("DSChild", 0.0, 0.0, 40.0, 30.0, Color::CYAN);
    let sib_id = d.add_rect("DSSib", 0.0, 40.0, 10.0, 10.0, Color::YELLOW);
    {
        let tab = d.app.active_tab_mut().unwrap();
        let grp_id = tab
            .document
            .group_objects(&[child_id, sib_id], "DSGrp")
            .unwrap();
        tab.document
            .transform_object(grp_id, Affine2D::translate(50.0, 50.0))
            .unwrap();
        tab.selection.select_single(&tab.document, child_id);
    }
    d.app
        .execute_command(CommandOp::AddEffect {
            id: child_id,
            effect: Effect::DropShadow {
                dx_mm: 5.0,
                dy_mm: 5.0,
                blur_mm: 4.0,
                opacity: 0.6,
                color: Color::BLACK,
                feather_direction: FeatherDirection::Middle,
            },
        })
        .unwrap();
    d.app.select_tool(ToolId::DropShadow);
    d.step(Vec::new(), Modifiers::NONE);

    // Child center is world (70, 65); DropShadow knob is at (75, 70) -> drag to (85, 80)
    let knob0 = d.world_to_pos2(Point2D::new(75.0, 70.0));
    let knob1 = d.world_to_pos2(Point2D::new(85.0, 80.0));
    d.step(
        vec![
            Event::PointerMoved(knob0),
            Event::PointerButton {
                pos: knob0,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(vec![Event::PointerMoved(knob1)], Modifiers::NONE);
    d.step(
        vec![Event::PointerButton {
            pos: knob1,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );

    let has_15 = d
        .app
        .active_tab()
        .unwrap()
        .document
        .object(child_id)
        .unwrap()
        .effects
        .iter()
        .any(|e| {
            matches!(
                e,
                Effect::DropShadow { dx_mm, dy_mm, .. }
                    if (*dx_mm - 15.0).abs() < 1.5 && (*dy_mm - 15.0).abs() < 1.5
            )
        });
    assert!(
        has_15,
        "Expected DropShadow offset ~(15, 15) on group child, got {:?}",
        d.app
            .active_tab()
            .unwrap()
            .document
            .object(child_id)
            .unwrap()
            .effects
    );
}

#[test]
fn w1c_r2_crop_enter_confirm_clears_tool_core_and_overlay_handles() {
    let mut d = UiDriver::new();
    let _id = d.add_rect("InCrop", 20.0, 20.0, 40.0, 40.0, Color::CYAN);
    d.app.select_tool(ToolId::Crop);
    d.step(Vec::new(), Modifiers::NONE);

    let c0 = d.world_to_pos2(Point2D::new(10.0, 10.0));
    let c1 = d.world_to_pos2(Point2D::new(80.0, 80.0));
    d.step(
        vec![
            Event::PointerMoved(c0),
            Event::PointerButton {
                pos: c0,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(vec![Event::PointerMoved(c1)], Modifiers::NONE);
    d.step(
        vec![Event::PointerButton {
            pos: c1,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    {
        let tab = d.app.active_tab().unwrap();
        assert!(tab.pending_crop_rect.is_some());
        assert!(tab.tool_core.pending_crop_rect.is_some());
        assert!(!tab.transient_overlay.preview_handles.is_empty());
    }

    // Confirm Crop with Enter
    d.step(
        vec![Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    let tab = d.app.active_tab().unwrap();
    assert!(tab.pending_crop_rect.is_none());
    assert!(tab.tool_core.pending_crop_rect.is_none());
    assert!(tab.transient_overlay.preview_handles.is_empty());
    assert!(!tab.tool_core.has_active_draft_or_drag());
}

#[test]
fn w1c_r2_inline_text_edit_auto_commits_on_outside_canvas_press() {
    let mut d = UiDriver::new();
    d.app.select_tool(ToolId::ArtisticText);
    d.step(Vec::new(), Modifiers::NONE);

    let t_pos = d.world_to_pos2(Point2D::new(30.0, 40.0));
    d.step(
        vec![
            Event::PointerMoved(t_pos),
            Event::PointerButton {
                pos: t_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: t_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(d.app.active_tab().unwrap().inline_text_edit.is_some());

    // Type text into the floating inline editor
    d.step(vec![Event::Text("StudioText".to_owned())], Modifiers::NONE);

    // Click far away on canvas (outside the floating editor area) -> auto-commits!
    d.app.select_tool(ToolId::Pick);
    let outside = d.world_to_pos2(Point2D::new(160.0, 220.0));
    d.step(
        vec![
            Event::PointerMoved(outside),
            Event::PointerButton {
                pos: outside,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: outside,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(d.app.active_tab().unwrap().inline_text_edit.is_none());
    assert!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .objects()
            .values()
            .any(|o| matches!(
                &o.payload,
                NodePayload::ArtisticText(t) if t.plain_text().contains("StudioText")
            )),
        "Outside canvas click must auto-commit active inline_text_edit"
    );
}

fn find_text_rect_in_shapes(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<Rect> {
    fn walk(shape: &egui::Shape, needle: &str) -> Option<Rect> {
        match shape {
            egui::Shape::Text(t) if t.galley.text().contains(needle) => {
                Some(t.visual_bounding_rect())
            }
            egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
            _ => None,
        }
    }
    shapes.iter().find_map(|cs| walk(&cs.shape, needle))
}

#[test]
fn w1c_r3_inline_text_edit_inside_click_preserves_buffer_and_apply_commits() {
    let mut d = UiDriver::new();
    d.app.select_tool(ToolId::ArtisticText);
    d.step(Vec::new(), Modifiers::NONE);

    // 1. Click canvas at (30, 40) mm to open inline_text_edit
    let t_pos = d.world_to_pos2(Point2D::new(30.0, 40.0));
    d.step(
        vec![
            Event::PointerMoved(t_pos),
            Event::PointerButton {
                pos: t_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: t_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(d.app.active_tab().unwrap().inline_text_edit.is_some());

    // 2. Type text into the floating inline editor
    d.step(
        vec![Event::Text("ApplyBtnText".to_owned())],
        Modifiers::NONE,
    );
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .inline_text_edit
            .as_ref()
            .unwrap()
            .buffer,
        "ApplyBtnText"
    );

    // 3. Hover and click INSIDE the floating TextEdit box (near anchor + offset) ->
    //    must keep inline_text_edit open AND preserve buffer!
    let inside_editor = pos2(t_pos.x + 24.0, t_pos.y + 14.0);
    d.step(
        vec![
            Event::PointerMoved(inside_editor),
            Event::PointerButton {
                pos: inside_editor,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: inside_editor,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(
        d.app.active_tab().unwrap().inline_text_edit.is_some(),
        "Clicking inside floating TextEdit must keep inline_text_edit open"
    );
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .inline_text_edit
            .as_ref()
            .unwrap()
            .buffer,
        "ApplyBtnText",
        "Clicking inside floating TextEdit must preserve typed buffer"
    );

    // 4. Find the "✓ Apply" button rect from egui's painted shapes and click it ->
    //    must commit the typed ArtisticText object cleanly!
    d.time += 0.02;
    let out = d.ctx.run_ui(
        RawInput {
            screen_rect: Some(d.screen),
            time: Some(d.time),
            ..RawInput::default()
        },
        |ui| d.app.render_ui(ui),
    );
    let apply_rect = find_text_rect_in_shapes(&out.shapes, "✓ Apply")
        .expect("✓ Apply button must be rendered in floating inline_text_edit overlay");
    let apply_center = apply_rect.center();

    d.step(
        vec![
            Event::PointerMoved(apply_center),
            Event::PointerButton {
                pos: apply_center,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: apply_center,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(
        d.app.active_tab().unwrap().inline_text_edit.is_none(),
        "Clicking ✓ Apply must close inline_text_edit"
    );
    assert!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .objects()
            .values()
            .any(|o| matches!(
                &o.payload,
                NodePayload::ArtisticText(t) if t.plain_text() == "ApplyBtnText"
            )),
        "Clicking ✓ Apply must commit ArtisticText('ApplyBtnText')"
    );
}

#[test]
fn w1c_r7_inline_text_edit_drag_from_editor_release_on_canvas_preserves_buffer() {
    let mut d = UiDriver::new();
    d.app.select_tool(ToolId::ArtisticText);
    d.step(Vec::new(), Modifiers::NONE);

    // 1. Click canvas at (30, 40) mm to open inline_text_edit
    let t_pos = d.world_to_pos2(Point2D::new(30.0, 40.0));
    d.step(
        vec![
            Event::PointerMoved(t_pos),
            Event::PointerButton {
                pos: t_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: t_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(d.app.active_tab().unwrap().inline_text_edit.is_some());

    // 2. Type text into the floating inline editor
    d.step(
        vec![Event::Text("KeepThisBuffer".to_owned())],
        Modifiers::NONE,
    );
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .inline_text_edit
            .as_ref()
            .map(|e| e.buffer.as_str()),
        Some("KeepThisBuffer")
    );

    // 3. Press Primary INSIDE the floating TextEdit box, drag out onto the canvas, and release
    //    over the canvas -> must preserve active inline_text_edit and its typed buffer intact!
    let inside_editor = pos2(t_pos.x + 24.0, t_pos.y + 14.0);
    let outside_on_canvas = d.world_to_pos2(Point2D::new(150.0, 140.0));
    d.step(
        vec![
            Event::PointerMoved(inside_editor),
            Event::PointerButton {
                pos: inside_editor,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerMoved(outside_on_canvas)],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: outside_on_canvas,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );

    assert!(
        d.app.active_tab().unwrap().inline_text_edit.is_some(),
        "Drag from floating TextEdit releasing on canvas must keep inline_text_edit open"
    );
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .inline_text_edit
            .as_ref()
            .map(|e| e.buffer.as_str()),
        Some("KeepThisBuffer"),
        "Drag from floating TextEdit releasing on canvas must preserve typed buffer"
    );
}

#[test]
fn w1c_r14_powerclip_edit_bar_blocks_press_fallthrough_and_completes_canvas_drags() {
    let mut d = UiDriver::new();
    let container_id = d.add_rect("PCFrame", 30.0, 30.0, 100.0, 80.0, Color::CYAN);
    let inner_id = d.add_rect("PCChild", 45.0, 45.0, 30.0, 25.0, Color::YELLOW);
    {
        let tab = d.app.active_tab_mut().unwrap();
        tab.document
            .create_powerclip(container_id, &[inner_id])
            .unwrap();
    }
    assert!(d.app.enter_powerclip_edit(container_id));
    d.app.select_tool(ToolId::Rectangle);

    // Run two frames so the floating PowerClip edit bar exits its first-frame sizing pass
    // and paints its foreground widgets cleanly.
    d.step(Vec::new(), Modifiers::NONE);
    d.time += 0.02;
    let out = d.ctx.run_ui(
        RawInput {
            screen_rect: Some(d.screen),
            time: Some(d.time),
            ..RawInput::default()
        },
        |ui| d.app.render_ui(ui),
    );
    let finish_rect = find_text_rect_in_shapes(&out.shapes, "Finish (Esc)")
        .expect("Finish (Esc) button must be rendered in coree_powerclip_edit_bar");
    let pill_body_rect = find_text_rect_in_shapes(&out.shapes, "Editing PowerClip")
        .expect("Editing PowerClip label must be rendered in coree_powerclip_edit_bar");

    let objects_before = d.app.active_tab().unwrap().document.objects().len();

    // 1. Clicking the "Editing PowerClip" pill body with Rectangle active must NOT start a drag
    //    or create phantom objects.
    let body_pos = pill_body_rect.center();
    d.step(
        vec![
            Event::PointerMoved(body_pos),
            Event::PointerButton {
                pos: body_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    assert!(
        !d.app.active_tab().unwrap().tool_core.is_dragging(),
        "Pressing on 'Editing PowerClip' pill body must not start a canvas drag"
    );
    d.step(
        vec![Event::PointerButton {
            pos: body_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(
        !d.app.active_tab().unwrap().tool_core.is_dragging(),
        "Releasing on 'Editing PowerClip' pill body must leave is_dragging == false"
    );
    assert!(
        d.app
            .active_tab()
            .unwrap()
            .transient_overlay
            .preview_paths
            .is_empty(),
        "Pill body click must leave transient_overlay.preview_paths empty"
    );
    assert_eq!(
        d.app.active_tab().unwrap().document.objects().len(),
        objects_before,
        "Pill body click must create 0 phantom objects"
    );
    assert_eq!(
        d.app.active_tab().unwrap().powerclip_edit_id,
        Some(container_id),
        "Clicking pill body label must keep PowerClip edit mode active"
    );

    // 1b. Clicking the pill body with ArtisticText active must also NOT trigger zero-drag click.
    d.app.select_tool(ToolId::ArtisticText);
    d.step(Vec::new(), Modifiers::NONE);
    d.step(
        vec![
            Event::PointerMoved(body_pos),
            Event::PointerButton {
                pos: body_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: body_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(
        d.app.active_tab().unwrap().inline_text_edit.is_none(),
        "Clicking 'Editing PowerClip' pill body with ArtisticText must not open inline_text_edit"
    );
    assert!(!d.app.active_tab().unwrap().tool_core.is_dragging());
    assert_eq!(
        d.app.active_tab().unwrap().document.objects().len(),
        objects_before
    );
    d.app.select_tool(ToolId::Rectangle);
    d.step(Vec::new(), Modifiers::NONE);

    // 2. Pressing and releasing "Finish (Esc)" with ToolId::Rectangle active must exit
    //    powerclip_edit_id with is_dragging == false, empty preview_paths, and 0 phantom objects,
    //    even after subsequent PointerMoved on open canvas.
    let finish_pos = finish_rect.center();
    d.step(
        vec![
            Event::PointerMoved(finish_pos),
            Event::PointerButton {
                pos: finish_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    assert!(
        !d.app.active_tab().unwrap().tool_core.is_dragging(),
        "Pressing 'Finish (Esc)' must not initiate a canvas drag"
    );
    d.step(
        vec![Event::PointerButton {
            pos: finish_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert_eq!(
        d.app.active_tab().unwrap().powerclip_edit_id,
        None,
        "Clicking 'Finish (Esc)' must exit PowerClip edit mode"
    );
    assert!(!d.app.active_tab().unwrap().tool_core.is_dragging());
    assert!(
        d.app
            .active_tab()
            .unwrap()
            .transient_overlay
            .preview_paths
            .is_empty(),
        "Exiting PowerClip edit via 'Finish (Esc)' must leave preview_paths empty"
    );

    let open_canvas = d.world_to_pos2(Point2D::new(140.0, 180.0));
    d.step(vec![Event::PointerMoved(open_canvas)], Modifiers::NONE);
    assert!(!d.app.active_tab().unwrap().tool_core.is_dragging());
    assert!(
        d.app
            .active_tab()
            .unwrap()
            .transient_overlay
            .preview_paths
            .is_empty(),
        "Subsequent PointerMoved after 'Finish (Esc)' must not emit phantom preview_paths"
    );
    assert_eq!(
        d.app.active_tab().unwrap().document.objects().len(),
        objects_before,
        "Clicking 'Finish (Esc)' must create 0 phantom objects"
    );

    // 3. Starting a legitimate Rectangle drag on open canvas and releasing over
    //    coree_powerclip_edit_bar must complete the drag cleanly (`is_dragging() == false`).
    assert!(d.app.enter_powerclip_edit(container_id));
    d.step(Vec::new(), Modifiers::NONE);
    d.step(Vec::new(), Modifiers::NONE);

    let drag_start = d.world_to_pos2(Point2D::new(60.0, 80.0));
    d.step(
        vec![
            Event::PointerMoved(drag_start),
            Event::PointerButton {
                pos: drag_start,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    assert!(
        d.app.active_tab().unwrap().tool_core.is_dragging(),
        "Pressing on open canvas must start legitimate Rectangle drag"
    );

    // Drag onto the floating PowerClip bar ("Finish (Esc)") and release there
    d.step(vec![Event::PointerMoved(finish_pos)], Modifiers::NONE);
    assert!(d.app.active_tab().unwrap().tool_core.is_dragging());
    d.step(
        vec![Event::PointerButton {
            pos: finish_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(
        !d.app.active_tab().unwrap().tool_core.is_dragging(),
        "Releasing legitimate canvas drag over coree_powerclip_edit_bar must complete drag cleanly"
    );
    assert!(
        d.app
            .active_tab()
            .unwrap()
            .transient_overlay
            .preview_paths
            .is_empty(),
        "Completed drag must clear transient_overlay.preview_paths"
    );
    assert_eq!(
        d.app.active_tab().unwrap().powerclip_edit_id,
        Some(container_id),
        "Releasing a canvas drag over 'Finish (Esc)' must not accidentally exit PowerClip edit"
    );
    assert_eq!(
        d.app.active_tab().unwrap().document.objects().len(),
        objects_before + 1,
        "Legitimate Rectangle drag released over PowerClip bar must commit the drawn rectangle"
    );
}

#[test]
fn w1c_r14_foreground_dialog_over_canvas_never_leaks_press_to_tool_core() {
    let mut d = UiDriver::new();
    d.app.select_tool(ToolId::Rectangle);
    d.app.show_edit_fill_dialog = true;

    // Two frames so egui::Window exits sizing pass and lays out inside screen over canvas_rect.
    d.step(Vec::new(), Modifiers::NONE);
    d.time += 0.02;
    let out = d.ctx.run_ui(
        RawInput {
            screen_rect: Some(d.screen),
            time: Some(d.time),
            ..RawInput::default()
        },
        |ui| d.app.render_ui(ui),
    );
    let title_rect = find_text_rect_in_shapes(&out.shapes, "Edit Fill (F11)")
        .expect("Edit Fill dialog must be visible over canvas");
    let click_pos = pos2(title_rect.center().x, title_rect.center().y + 40.0);
    let objects_before = d.app.active_tab().unwrap().document.objects().len();

    d.step(
        vec![
            Event::PointerMoved(click_pos),
            Event::PointerButton {
                pos: click_pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    assert!(
        !d.app.active_tab().unwrap().tool_core.is_dragging(),
        "Pressing inside Edit Fill window over canvas must not leak to ToolCore"
    );
    d.step(
        vec![Event::PointerButton {
            pos: click_pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(!d.app.active_tab().unwrap().tool_core.is_dragging());
    assert_eq!(
        d.app.active_tab().unwrap().document.objects().len(),
        objects_before
    );
}

fn render_fresh_tile_rgba(d: &UiDriver) -> Vec<u8> {
    use coree_render::{DirtyTileCache, SceneRenderer};

    let tab = d.app.active_tab().unwrap();
    let mut fresh = DirtyTileCache::new(tab.viewport.width_px, tab.viewport.height_px);
    let mut renderer = SceneRenderer::new();
    fresh.render_dirty_tiles(
        &tab.document,
        &tab.viewport,
        tab.view_mode,
        &tab.render_options,
        &mut renderer,
    );
    fresh.composite_to_rgba().rgba
}

#[test]
fn w1c_r17_interactive_fill_and_eraser_on_group_pixel_parity() {
    let mut d = UiDriver::new();

    // 1. Selected Group([c1, c2]): InteractiveFill drag renders visible gradient pixels
    //    on both children, preserves Group selection, edits child handle, and Ctrl+Z restores.
    let c1 = d.add_rect("GrpFillC1", 20.0, 20.0, 40.0, 40.0, Color::CYAN);
    let c2 = d.add_rect("GrpFillC2", 70.0, 20.0, 40.0, 40.0, Color::CYAN);
    d.app
        .execute_command(CommandOp::Group {
            ids: vec![c1, c2],
            name: "FillGrp".to_owned(),
        })
        .unwrap();
    let grp_id = *d
        .app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .max()
        .unwrap();
    {
        let tab = d.app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, grp_id);
    }
    d.step(Vec::new(), Modifiers::NONE);

    let initial_rgba = d
        .app
        .active_tab()
        .unwrap()
        .tile_cache
        .composite_to_rgba()
        .rgba;
    assert_eq!(initial_rgba, render_fresh_tile_rgba(&d));

    d.app.select_tool(ToolId::InteractiveFill);
    d.step(Vec::new(), Modifiers::NONE);

    let f0 = d.world_to_pos2(Point2D::new(20.0, 40.0));
    let f1 = d.world_to_pos2(Point2D::new(110.0, 40.0));
    d.step(
        vec![
            Event::PointerMoved(f0),
            Event::PointerButton {
                pos: f0,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(vec![Event::PointerMoved(f1)], Modifiers::NONE);
    d.step(
        vec![Event::PointerButton {
            pos: f1,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    d.step(Vec::new(), Modifiers::NONE);

    {
        let tab = d.app.active_tab().unwrap();
        assert_eq!(tab.document.object(grp_id).unwrap().fill, Fill::None);
        assert!(matches!(
            tab.document.object(c1).unwrap().fill,
            Fill::LinearGradient { .. }
        ));
        assert!(matches!(
            tab.document.object(c2).unwrap().fill,
            Fill::LinearGradient { .. }
        ));
        assert_eq!(tab.selection.objects, vec![grp_id]);
    }
    let after_fill_rgba = d
        .app
        .active_tab()
        .unwrap()
        .tile_cache
        .composite_to_rgba()
        .rgba;
    assert_ne!(
        after_fill_rgba, initial_rgba,
        "InteractiveFill on selected Group must change rendered canvas pixels"
    );
    assert_eq!(
        after_fill_rgba,
        render_fresh_tile_rgba(&d),
        "Incremental DirtyTileCache must match fresh render after InteractiveFill on Group"
    );

    // Edit child c2's gradient end handle (110, 40) -> (90, 55) while Group stays selected
    let f2 = d.world_to_pos2(Point2D::new(90.0, 55.0));
    d.step(
        vec![
            Event::PointerMoved(f1),
            Event::PointerButton {
                pos: f1,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(vec![Event::PointerMoved(f2)], Modifiers::NONE);
    d.step(
        vec![Event::PointerButton {
            pos: f2,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    d.step(Vec::new(), Modifiers::NONE);
    assert_eq!(
        d.app.active_tab().unwrap().selection.objects,
        vec![grp_id],
        "Dragging child gradient handle must preserve Group selection"
    );
    let after_handle_rgba = d
        .app
        .active_tab()
        .unwrap()
        .tile_cache
        .composite_to_rgba()
        .rgba;
    assert_ne!(after_handle_rgba, after_fill_rgba);
    assert_eq!(after_handle_rgba, render_fresh_tile_rgba(&d));

    // Ctrl+Z twice restores exact initial RGBA pixels
    for _ in 0..2 {
        d.step(
            vec![Event::Key {
                key: Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::CTRL,
            }],
            Modifiers::CTRL,
        );
        d.step(Vec::new(), Modifiers::NONE);
    }
    let after_fill_undo_rgba = d
        .app
        .active_tab()
        .unwrap()
        .tile_cache
        .composite_to_rgba()
        .rgba;
    assert_eq!(
        after_fill_undo_rgba, initial_rgba,
        "Ctrl+Z must restore exact initial RGBA bytes after Group InteractiveFill"
    );

    // 2. Selected Group([e1, e2]): Eraser stroke across both modifies both visually & atomically
    let e1 = d.add_rect("GrpEraE1", 20.0, 100.0, 40.0, 40.0, Color::MAGENTA);
    let e2 = d.add_rect("GrpEraE2", 75.0, 100.0, 40.0, 40.0, Color::YELLOW);
    d.app
        .execute_command(CommandOp::Group {
            ids: vec![e1, e2],
            name: "EraGrp".to_owned(),
        })
        .unwrap();
    let egrp_id = *d
        .app
        .active_tab()
        .unwrap()
        .document
        .objects()
        .keys()
        .max()
        .unwrap();
    let orig_e1 = d
        .app
        .active_tab()
        .unwrap()
        .document
        .object(e1)
        .unwrap()
        .payload
        .clone();
    let orig_e2 = d
        .app
        .active_tab()
        .unwrap()
        .document
        .object(e2)
        .unwrap()
        .payload
        .clone();
    {
        let tab = d.app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, egrp_id);
    }
    d.app.tool_settings.eraser_radius_mm = 6.0;
    d.app.select_tool(ToolId::Eraser);
    d.step(Vec::new(), Modifiers::NONE);

    let pre_erase_rgba = d
        .app
        .active_tab()
        .unwrap()
        .tile_cache
        .composite_to_rgba()
        .rgba;
    assert_eq!(pre_erase_rgba, render_fresh_tile_rgba(&d));

    let p0 = d.world_to_pos2(Point2D::new(15.0, 120.0));
    let p1 = d.world_to_pos2(Point2D::new(120.0, 120.0));
    d.step(
        vec![
            Event::PointerMoved(p0),
            Event::PointerButton {
                pos: p0,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(vec![Event::PointerMoved(p1)], Modifiers::NONE);
    d.step(
        vec![Event::PointerButton {
            pos: p1,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    d.step(Vec::new(), Modifiers::NONE);

    {
        let tab = d.app.active_tab().unwrap();
        assert_ne!(
            tab.document.object(e1).unwrap().payload,
            orig_e1,
            "e1 in Group must be erased"
        );
        assert_ne!(
            tab.document.object(e2).unwrap().payload,
            orig_e2,
            "e2 in Group must be erased"
        );
    }
    let after_erase_rgba = d
        .app
        .active_tab()
        .unwrap()
        .tile_cache
        .composite_to_rgba()
        .rgba;
    assert_ne!(after_erase_rgba, pre_erase_rgba);
    assert_eq!(
        after_erase_rgba,
        render_fresh_tile_rgba(&d),
        "Incremental DirtyTileCache must match fresh render after Eraser across Group"
    );

    // Ctrl+Z restores both children and exact pre-erase RGBA pixels
    d.step(
        vec![Event::Key {
            key: Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::CTRL,
        }],
        Modifiers::CTRL,
    );
    d.step(Vec::new(), Modifiers::NONE);
    {
        let tab = d.app.active_tab().unwrap();
        assert_eq!(tab.document.object(e1).unwrap().payload, orig_e1);
        assert_eq!(tab.document.object(e2).unwrap().payload, orig_e2);
    }
    let after_erase_undo_rgba = d
        .app
        .active_tab()
        .unwrap()
        .tile_cache
        .composite_to_rgba()
        .rgba;
    assert_eq!(
        after_erase_undo_rgba, pre_erase_rgba,
        "Ctrl+Z must restore exact pre-erase RGBA bytes"
    );
}

#[test]
fn wheel_zoom_multi_frame_smooth_scroll_is_bounded_and_proportional() {
    let mut d = UiDriver::new();
    let hover = d.world_to_pos2(Point2D::new(105.0, 148.5));
    d.step(vec![Event::PointerMoved(hover)], Modifiers::NONE);

    let z0 = d.app.active_tab().unwrap().viewport.zoom;
    let anchor_before = d
        .app
        .active_tab()
        .unwrap()
        .viewport
        .screen_to_world(Point2D::new(
            f64::from(hover.x - d.app.active_tab().unwrap().last_canvas_rect.unwrap()[0]),
            f64::from(hover.y - d.app.active_tab().unwrap().last_canvas_rect.unwrap()[1]),
        ));

    d.step(
        vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, 1.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );

    let mut prev_z = d.app.active_tab().unwrap().viewport.zoom;
    let first_step = prev_z / z0;
    assert!(
        (1.0..=1.14).contains(&first_step),
        "Single frame zoom step must be <= 1.14 (got {first_step})"
    );

    for _ in 0..15 {
        d.step(Vec::new(), Modifiers::NONE);
        let cur_z = d.app.active_tab().unwrap().viewport.zoom;
        let ratio = cur_z / prev_z;
        assert!(
            (1.0 - 1e-6..=1.14).contains(&ratio),
            "Per-frame smoothed wheel zoom ratio must stay bounded <= 1.14 (got {ratio})"
        );
        prev_z = cur_z;
    }

    let final_ratio = prev_z / z0;
    assert!(
        (1.04..=1.18).contains(&final_ratio),
        "Single wheel notch over 15 smoothed frames must remain bounded ~1.09x, NOT compound to 3.9x (got {final_ratio})"
    );

    let tab = d.app.active_tab().unwrap();
    let rect = tab.last_canvas_rect.unwrap();
    let anchor_after = tab.viewport.screen_to_world(Point2D::new(
        f64::from(hover.x - rect[0]),
        f64::from(hover.y - rect[1]),
    ));
    assert!(
        anchor_before.distance_to(anchor_after) < 0.25,
        "Cursor world anchor shifted too far during wheel zoom: before={anchor_before:?} after={anchor_after:?}"
    );
}

#[test]
fn wheel_zoom_in_then_out_is_symmetric_and_pinch_ctrl_scroll_bounded() {
    let mut d = UiDriver::new();
    let hover = d.world_to_pos2(Point2D::new(105.0, 148.5));
    d.step(vec![Event::PointerMoved(hover)], Modifiers::NONE);
    let z0 = d.app.active_tab().unwrap().viewport.zoom;

    d.step(
        vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, 1.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    for _ in 0..15 {
        d.step(Vec::new(), Modifiers::NONE);
    }

    d.step(
        vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, -1.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    for _ in 0..15 {
        d.step(Vec::new(), Modifiers::NONE);
    }
    let z_returned = d.app.active_tab().unwrap().viewport.zoom;
    assert!(
        ((z_returned / z0) - 1.0).abs() < 0.03,
        "Equal +1/-1 wheel notches must be symmetric within 3% (z0={z0}, z_returned={z_returned})"
    );

    // Ctrl+Scroll / Pinch zoom must also remain smoothly bounded
    d.step(
        vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, 1.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Modifiers::CTRL,
        }],
        Modifiers::CTRL,
    );
    for _ in 0..15 {
        d.step(Vec::new(), Modifiers::CTRL);
    }
    let z_pinch = d.app.active_tab().unwrap().viewport.zoom;
    let pinch_ratio = z_pinch / z_returned;
    assert!(
        (1.03..=1.22).contains(&pinch_ratio),
        "Ctrl+Scroll pinch zoom over 15 frames must be smooth and bounded (got {pinch_ratio})"
    );
}

#[test]
fn zoom_tool_click_and_micro_drag_are_smooth_and_bounded() {
    let mut d = UiDriver::new();
    d.app.select_tool(ToolId::Zoom);
    let p = d.world_to_pos2(Point2D::new(105.0, 148.5));
    let z0 = d.app.active_tab().unwrap().viewport.zoom;

    // Primary click -> 1.25x
    d.step(
        vec![
            Event::PointerMoved(p),
            Event::PointerButton {
                pos: p,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    let z1 = d.app.active_tab().unwrap().viewport.zoom;
    assert!(
        ((z1 / z0) - 1.25).abs() < 1e-4,
        "Zoom tool primary click must apply 1.25x (got {})",
        z1 / z0
    );

    // Micro-drag (< 6 px) even at low zoom must NOT jump to 256x marquee fit
    d.app.select_tool(ToolId::Zoom);
    d.app.active_tab_mut().unwrap().viewport.zoom = 0.05;
    let z_low = d.app.active_tab().unwrap().viewport.zoom;
    let p2 = pos2(p.x + 2.0, p.y + 2.0);
    d.step(
        vec![
            Event::PointerMoved(p),
            Event::PointerButton {
                pos: p,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![
            Event::PointerMoved(p2),
            Event::PointerButton {
                pos: p2,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    let z_after_micro = d.app.active_tab().unwrap().viewport.zoom;
    assert!(
        ((z_after_micro / z_low) - 1.25).abs() < 1e-4,
        "Micro-drag (<6px) at low zoom must click-zoom 1.25x, not marquee fit (got {})",
        z_after_micro / z_low
    );
}

#[test]
fn stylus_touch_force_in_pointer_interaction_creates_expression_stroke() {
    let mut d = UiDriver::new();
    d.app.select_tool(ToolId::ArtisticMedia);
    d.app.tool_settings.pressure_curve_json = "[[0.0,0.2],[1.0,1.0]]".to_owned();
    let p0 = d.world_to_pos2(Point2D::new(30.0, 50.0));
    let p1 = d.world_to_pos2(Point2D::new(90.0, 50.0));

    d.step(
        vec![
            Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase: egui::TouchPhase::Start,
                pos: p0,
                force: Some(0.0),
            },
            Event::PointerMoved(p0),
            Event::PointerButton {
                pos: p0,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![
            Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase: egui::TouchPhase::Move,
                pos: p1,
                force: Some(1.0),
            },
            Event::PointerMoved(p1),
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: p1,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );

    let tab = d.app.active_tab().unwrap();
    let has_expr = tab.document.objects().values().any(|o| {
        matches!(
            o.payload,
            NodePayload::ArtisticMedia {
                style: coree_core::ArtisticMediaStyle::Expression { .. },
                ..
            }
        )
    });
    assert!(
        has_expr,
        "Stylus touch force through egui RawInput must create ArtisticMediaStyle::Expression"
    );
}

#[test]
fn bitmap_brush_stylus_soft_pressure_curve_evaluates_once() {
    let mut d = UiDriver::new();
    d.app.select_tool(ToolId::BitmapBrush);
    d.app.tool_settings.brush_radius_px = 40.0;
    d.app.tool_settings.brush_hardness = 1.0;
    d.app.tool_settings.brush_color = Color::rgba(1.0, 0.0, 0.0, 1.0);
    // "Soft" curve: raw 0.0 -> 0.20 (single eval), whereas double eval would yield 0.42.
    // At radius = 40 px:
    //   p = 0.20 -> dab_r = 40 * (0.25 + 0.75 * 0.20) = 16.0 px, alpha = 0.2 + 0.8 * 0.20 = 0.36
    //   p = 0.42 -> dab_r = 40 * (0.25 + 0.75 * 0.42) = 22.6 px, alpha = 0.2 + 0.8 * 0.42 = 0.536
    d.app.tool_settings.pressure_curve_json = "[[0.0,0.2],[0.5,0.75],[1.0,1.0]]".to_owned();

    // 1) Single-point dab at page center (exercises state.rs single-point ApplyRasterStroke path)
    let center_mm = Point2D::new(105.0, 148.5);
    let p_center = d.world_to_pos2(center_mm);
    d.step(
        vec![
            Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase: egui::TouchPhase::Start,
                pos: p_center,
                force: Some(0.0),
            },
            Event::PointerMoved(p_center),
            Event::PointerButton {
                pos: p_center,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![
            Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase: egui::TouchPhase::End,
                pos: p_center,
                force: Some(0.0),
            },
            Event::PointerButton {
                pos: p_center,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );

    let (cx, cy, center_alpha, inner_alpha, outer_alpha) = {
        let tab = d.app.active_tab().unwrap();
        let raster_data = tab
            .document
            .objects()
            .values()
            .find_map(|o| match &o.payload {
                NodePayload::Raster(r) => Some(r),
                _ => None,
            })
            .expect("BitmapBrush stroke must create a raster layer");
        let asset = tab
            .document
            .asset(raster_data.asset_id)
            .expect("raster asset must exist");
        let cx = ((center_mm.x / raster_data.width_mm) * f64::from(asset.width)).floor() as u32;
        let cy = ((center_mm.y / raster_data.height_mm) * f64::from(asset.height)).floor() as u32;
        let center_px = asset.read_pixel_f32(cx, cy).unwrap();
        let inner_px = asset.read_pixel_f32(cx + 12, cy).unwrap();
        let outer_px = asset.read_pixel_f32(cx + 19, cy).unwrap();
        (cx, cy, center_px[3], inner_px[3], outer_px[3])
    };
    assert!(
        (center_alpha - 0.36).abs() < 0.02,
        "Single-point dab at ({cx}, {cy}) must have single-evaluated alpha ~0.36 (not double-evaluated 0.536), got {center_alpha}"
    );
    assert!(
        (inner_alpha - 0.36).abs() < 0.02,
        "Pixel within single-evaluated 16.0px radius (dx=+12) must have alpha ~0.36, got {inner_alpha}"
    );
    assert!(
        outer_alpha <= 1e-4,
        "Pixel outside 16.0px radius (dx=+19, inside buggy 22.6px radius) must remain transparent, got {outer_alpha}"
    );

    // 2) Multi-point stroke (exercises state.rs multi-point ApplyRasterStroke path)
    let p0_mm = Point2D::new(45.0, 70.0);
    let p1_mm = Point2D::new(95.0, 70.0);
    let p0 = d.world_to_pos2(p0_mm);
    let p1 = d.world_to_pos2(p1_mm);
    d.step(
        vec![
            Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(2),
                phase: egui::TouchPhase::Start,
                pos: p0,
                force: Some(0.0),
            },
            Event::PointerMoved(p0),
            Event::PointerButton {
                pos: p0,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![
            Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(2),
                phase: egui::TouchPhase::Move,
                pos: p1,
                force: Some(0.0),
            },
            Event::PointerMoved(p1),
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![
            Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(2),
                phase: egui::TouchPhase::End,
                pos: p1,
                force: Some(0.0),
            },
            Event::PointerButton {
                pos: p1,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );

    let tab = d.app.active_tab().unwrap();
    let raster_data = tab
        .document
        .objects()
        .values()
        .find_map(|o| match &o.payload {
            NodePayload::Raster(r) => Some(r),
            _ => None,
        })
        .expect("raster layer must still exist");
    let asset = tab.document.asset(raster_data.asset_id).unwrap();
    let mx = (((p0_mm.x + p1_mm.x) * 0.5 / raster_data.width_mm) * f64::from(asset.width)).floor()
        as u32;
    let my = ((p0_mm.y / raster_data.height_mm) * f64::from(asset.height)).floor() as u32;
    let stroke_inner_alpha = asset.read_pixel_f32(mx, my + 12).unwrap()[3];
    let stroke_outer_alpha = asset.read_pixel_f32(mx, my + 19).unwrap()[3];
    assert!(
        stroke_inner_alpha >= 0.34,
        "Multi-point stroke pixel within 16.0px half-width (dy=+12) must be painted, got {stroke_inner_alpha}"
    );
    assert!(
        stroke_outer_alpha <= 1e-4,
        "Multi-point stroke pixel outside 16.0px half-width (dy=+19, inside buggy 22.6px) must remain transparent, got {stroke_outer_alpha}"
    );
}
