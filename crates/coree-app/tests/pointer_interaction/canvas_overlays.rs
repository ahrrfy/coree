// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

use super::*;
use coree_app::state::ToolId;
use egui::{Key, PointerButton};

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
