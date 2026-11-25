// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

use super::*;
use coree_app::state::ToolId;
use coree_prod::{ObjectKindFilter, ReplaceAction};
use egui::{Key, PointerButton};

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
