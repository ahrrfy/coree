// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

use super::*;
use coree_app::i18n::{I18nKey, ShellLayoutDirections, StudioLanguage};
use coree_app::state::DockerTab;
use coree_app::ui::palette_bar::build_tint_grid_colors;
use coree_core::{default_cmyk_palette, ColorMatch, Effect, FeatherDirection};
use coree_prod::{MacroEventHook, ObjectKindFilter};
use egui::PointerButton;

fn find_exact_text_rect_in_shapes(
    shapes: &[egui::epaint::ClippedShape],
    needle: &str,
) -> Option<Rect> {
    fn walk(shape: &egui::Shape, needle: &str) -> Option<Rect> {
        match shape {
            egui::Shape::Text(t) if t.galley.text() == needle => Some(t.visual_bounding_rect()),
            egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
            _ => None,
        }
    }
    shapes.iter().find_map(|cs| walk(&cs.shape, needle))
}

fn find_swatch_rect_by_color(
    shapes: &[egui::epaint::ClippedShape],
    target_fill: egui::Color32,
    min_x: f32,
) -> Option<Rect> {
    fn walk(shape: &egui::Shape, target_fill: egui::Color32, min_x: f32) -> Option<Rect> {
        match shape {
            egui::Shape::Rect(r)
                if r.rect.left() >= min_x
                    && (r.rect.width() - 22.0).abs() <= 1.5
                    && (r.rect.height() - 22.0).abs() <= 1.5
                    && r.fill == target_fill =>
            {
                Some(r.rect)
            }
            egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, target_fill, min_x)),
            _ => None,
        }
    }
    shapes
        .iter()
        .find_map(|cs| walk(&cs.shape, target_fill, min_x))
}

#[test]
fn w5b_find_replace_ui_wires_build_find_query_and_undoable_command_op() {
    let mut d = UiDriver::new();
    d.app.show_dockers = true;
    d.app.focus_docker(DockerTab::FindReplace);

    let cyan_id = d.add_rect("CyanRect", 20.0, 20.0, 30.0, 20.0, Color::CYAN);
    let mag_id = d.add_rect("MagentaRect", 70.0, 20.0, 30.0, 20.0, Color::MAGENTA);
    {
        let lid = d.app.active_tab().unwrap().active_layer_id;
        let mut txt = SceneObject::new(
            ObjectId(0),
            "HelloText",
            NodePayload::ArtisticText(coree_core::ArtisticText::from_plain(
                "Hello",
                Default::default(),
            )),
        )
        .with_fill(Fill::Solid(Color::BLACK))
        .with_transform(Affine2D::translate(30.0, 60.0));
        txt.name = "HelloText".to_owned();
        d.app
            .execute_command(CommandOp::create_object(txt, lid))
            .unwrap();
        // Make CyanRect 0.5mm outline (non-hairline) and MagentaRect 0.05mm hairline
        d.app
            .execute_command(CommandOp::SetOutline {
                ids: vec![cyan_id],
                outline: OutlineStyle::solid(Color::BLACK, 0.5),
            })
            .unwrap();
        d.app
            .execute_command(CommandOp::SetOutline {
                ids: vec![mag_id],
                outline: OutlineStyle::solid(Color::BLACK, 0.05),
            })
            .unwrap();
    }

    let layer_name = d
        .app
        .active_tab()
        .unwrap()
        .document
        .active_page()
        .unwrap()
        .layers[0]
        .name
        .clone();

    {
        let fr = &mut d.app.find_replace_state;
        fr.object_kind = Some(ObjectKindFilter::Shape);
        fr.use_fill_color = true;
        fr.find_fill_color = Color::CYAN;
        fr.use_outline_color = true;
        fr.find_outline_color = Color::BLACK;
        fr.use_outline_width = true;
        fr.outline_width_min_mm = 0.2;
        fr.outline_width_max_mm = 1.0;
        fr.filter_hairline = Some(false);
        fr.layer_filter = layer_name.clone();
        fr.replace_fill_color = Color::YELLOW;
    }

    let q = d.app.build_find_query();
    assert_eq!(q.fill_color, Some(ColorMatch::exact(Color::CYAN)));
    assert_eq!(q.outline_color, Some(ColorMatch::exact(Color::BLACK)));
    assert_eq!(q.outline_width_min_mm, Some(0.2));
    assert_eq!(q.outline_width_max_mm, Some(1.0));
    assert_eq!(q.is_hairline, Some(false));
    assert_eq!(q.layer_name.as_deref(), Some(layer_name.as_str()));

    d.step(Vec::new(), Modifiers::NONE);
    let out = d.step_with_output(Vec::new(), Modifiers::NONE);
    let btn_label = I18nKey::ReplaceFillColor.tr_ui(StudioLanguage::English);
    let btn_rect = find_text_rect_in_shapes(&out.shapes, &btn_label)
        .expect("Replace Fill button must render in FindAndReplace docker");
    let c = btn_rect.center();
    d.step(
        vec![
            Event::PointerMoved(c),
            Event::PointerButton {
                pos: c,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: c,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );

    let tab = d.app.active_tab().unwrap();
    assert_eq!(
        tab.document.object(cyan_id).unwrap().fill,
        Fill::Solid(Color::YELLOW)
    );
    assert_eq!(
        tab.document.object(mag_id).unwrap().fill,
        Fill::Solid(Color::MAGENTA)
    );

    d.app.undo().unwrap();
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .object(cyan_id)
            .unwrap()
            .fill,
        Fill::Solid(Color::CYAN)
    );

    d.app.redo().unwrap();
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .object(cyan_id)
            .unwrap()
            .fill,
        Fill::Solid(Color::YELLOW)
    );
}

#[test]
fn w5b_s05_objects_docker_powerclip_fit_center_stretch_buttons() {
    let mut d = UiDriver::new();
    d.app.show_dockers = true;
    d.app.focus_docker(DockerTab::Objects);

    let inner_id = d.add_rect("PCInner", 10.0, 10.0, 100.0, 50.0, Color::MAGENTA);
    let container_id = d.add_rect("PCBox", 40.0, 40.0, 60.0, 40.0, Color::CYAN);
    d.app
        .execute_command(CommandOp::CreatePowerClip {
            container_id,
            content_ids: vec![inner_id],
        })
        .unwrap();

    d.step(Vec::new(), Modifiers::NONE);
    let out = d.step_with_output(Vec::new(), Modifiers::NONE);

    // Order: Ctr (keeps 100x50, centers at 70,60), Fit (scales 2:1 to 60x30), Str (stretches to 60x40)
    for (label, mode_idx) in [("Ctr", 1usize), ("Fit", 0usize), ("Str", 2usize)] {
        let r = find_exact_text_rect_in_shapes(&out.shapes, label)
            .unwrap_or_else(|| panic!("Objects docker must expose PowerClip '{label}' button"));
        let p = r.center();
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
        let tab = d.app.active_tab().unwrap();
        let pc = tab.document.object(container_id).unwrap();
        let NodePayload::PowerClip { contents, .. } = &pc.payload else {
            panic!("container must remain PowerClip");
        };
        let child_bb = tab.document.world_object_bounds(contents[0]).unwrap();
        let cont_bb = tab.document.world_object_bounds(container_id).unwrap();
        match mode_idx {
            0 => {
                assert!(
                    child_bb.width() <= cont_bb.width() + 0.05
                        && child_bb.height() <= cont_bb.height() + 0.05,
                    "FitContents must fit inside container: child={child_bb:?} cont={cont_bb:?}"
                );
            }
            1 => {
                let cc = child_bb.center();
                let bc = cont_bb.center();
                assert!(
                    (cc.x - bc.x).abs() < 0.05 && (cc.y - bc.y).abs() < 0.05,
                    "CenterContents must align child center with container center"
                );
            }
            _ => {
                assert!(
                    (child_bb.width() - cont_bb.width()).abs() < 0.05
                        && (child_bb.height() - cont_bb.height()).abs() < 0.05,
                    "StretchToFill must stretch child to container dimensions"
                );
            }
        }
    }

    // Undo Str -> back to Fit, Undo Fit -> Ctr, Undo Ctr -> original 100x50
    d.app.undo().unwrap();
    d.app.undo().unwrap();
    d.app.undo().unwrap();
    let tab = d.app.active_tab().unwrap();
    let NodePayload::PowerClip { contents, .. } =
        &tab.document.object(container_id).unwrap().payload
    else {
        unreachable!();
    };
    let orig_bb = tab.document.world_object_bounds(contents[0]).unwrap();
    assert!(
        (orig_bb.width() - 100.0).abs() < 0.1 && (orig_bb.height() - 50.0).abs() < 0.1,
        "Three undos must restore original 100x50 PowerClip child size, got {orig_bb:?}"
    );
}

#[test]
fn w5b_drag_swatch_from_palette_bar_sets_fill_and_shift_sets_outline() {
    let mut d = UiDriver::new();
    let target_id = d.add_rect("Target", 60.0, 60.0, 50.0, 50.0, Color::CYAN);
    let swatch_color = default_cmyk_palette().swatches[1].color.clone();
    let swatch_egui = coree_app::theme::core_color_to_egui(&swatch_color);

    let out = d.step_with_output(Vec::new(), Modifiers::NONE);
    let sw_rect = find_swatch_rect_by_color(&out.shapes, swatch_egui, d.screen.right() - 60.0)
        .expect("Palette swatch rect must be discoverable in right PaletteBar");
    let start = sw_rect.center();
    let drop_pt = d.world_to_pos2(Point2D::new(85.0, 85.0));

    // Drag swatch onto Target -> SetFill
    d.step(
        vec![
            Event::PointerMoved(start),
            Event::PointerButton {
                pos: start,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    for t in [0.25_f32, 0.6, 1.0] {
        let p = start.lerp(drop_pt, t);
        d.step(vec![Event::PointerMoved(p)], Modifiers::NONE);
    }
    d.step(
        vec![Event::PointerButton {
            pos: drop_pt,
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
            .object(target_id)
            .unwrap()
            .fill,
        Fill::Solid(swatch_color.clone())
    );
    d.app.undo().unwrap();
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .object(target_id)
            .unwrap()
            .fill,
        Fill::Solid(Color::CYAN)
    );

    // Repeat with Shift held on release -> SetOutline
    d.step(
        vec![
            Event::PointerMoved(start),
            Event::PointerButton {
                pos: start,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::SHIFT,
            },
        ],
        Modifiers::SHIFT,
    );
    for t in [0.25_f32, 0.6, 1.0] {
        let p = start.lerp(drop_pt, t);
        d.step(vec![Event::PointerMoved(p)], Modifiers::SHIFT);
    }
    d.step(
        vec![Event::PointerButton {
            pos: drop_pt,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::SHIFT,
        }],
        Modifiers::SHIFT,
    );
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .object(target_id)
            .unwrap()
            .outline
            .color,
        swatch_color
    );
    d.app.undo().unwrap();
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .object(target_id)
            .unwrap()
            .outline
            .color,
        Color::BLACK
    );
}

#[test]
fn w5b_palette_long_press_tint_grid_opens_after_half_sec_and_applies_tint() {
    let mut d = UiDriver::new();
    let target_id = d.add_rect("TintTarget", 60.0, 60.0, 50.0, 50.0, Color::CYAN);
    {
        let tab = d.app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, target_id);
    }

    let base_color = default_cmyk_palette().swatches[1].color.clone();
    let base_egui = coree_app::theme::core_color_to_egui(&base_color);
    let out = d.step_with_output(Vec::new(), Modifiers::NONE);
    let sw_rect = find_swatch_rect_by_color(&out.shapes, base_egui, d.screen.right() - 60.0)
        .expect("Palette swatch must be rendered");
    let hold_pt = sw_rect.center();

    d.step(
        vec![
            Event::PointerMoved(hold_pt),
            Event::PointerButton {
                pos: hold_pt,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    assert!(d.app.swatch_hold_state.is_some());
    assert!(d.app.tint_grid_popup.is_none());

    std::thread::sleep(std::time::Duration::from_millis(520));
    d.step(Vec::new(), Modifiers::NONE);
    assert!(
        d.app.tint_grid_popup.is_some(),
        "0.5s swatch hold must open tint_grid_popup"
    );

    // Release primary without dragging; popup remains open
    d.step(
        vec![Event::PointerButton {
            pos: hold_pt,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert!(d.app.tint_grid_popup.is_some());

    let expected_tints = build_tint_grid_colors(&base_color);
    assert_eq!(expected_tints.len(), 25);
    let (_, anchor) = d.app.tint_grid_popup.clone().unwrap();
    let popup_pos = pos2((anchor.x - 148.0).max(8.0), (anchor.y - 68.0).max(32.0));
    let out = d.step_with_output(Vec::new(), Modifiers::NONE);
    let cell_pt = out
        .shapes
        .iter()
        .find_map(|cs| match &cs.shape {
            egui::Shape::Rect(r)
                if r.rect.left() >= popup_pos.x
                    && r.rect.right() < anchor.x - 10.0
                    && r.rect.top() >= popup_pos.y
                    && (r.rect.width() - 20.0).abs() <= 0.5
                    && (r.rect.height() - 20.0).abs() <= 0.5 =>
            {
                Some(r.rect.center())
            }
            _ => None,
        })
        .expect("TintGrid 20x20 cell must render inside foreground popup");

    d.step(
        vec![
            Event::PointerMoved(cell_pt),
            Event::PointerButton {
                pos: cell_pt,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: cell_pt,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );

    assert!(d.app.tint_grid_popup.is_none());
    let tinted_fill = d
        .app
        .active_tab()
        .unwrap()
        .document
        .object(target_id)
        .unwrap()
        .fill
        .clone();
    assert_ne!(tinted_fill, Fill::Solid(Color::CYAN));
    assert_ne!(tinted_fill, Fill::Solid(base_color.clone()));
    // Undo TintGrid fill (then undo initial swatch release-click if applied) restores original Color::CYAN
    d.app.undo().unwrap();
    if d.app
        .active_tab()
        .unwrap()
        .document
        .object(target_id)
        .unwrap()
        .fill
        == Fill::Solid(base_color)
    {
        d.app.undo().unwrap();
    }
    assert_eq!(
        d.app
            .active_tab()
            .unwrap()
            .document
            .object(target_id)
            .unwrap()
            .fill,
        Fill::Solid(Color::CYAN)
    );
}

#[test]
fn w5b_rtl_shell_mirroring_flips_panels_and_egui_direction() {
    let mut d = UiDriver::new();
    d.app.show_dockers = true;
    d.step(Vec::new(), Modifiers::NONE);
    let out_en = d.step_with_output(Vec::new(), Modifiers::NONE);
    assert_eq!(
        ShellLayoutDirections::from_ctx(&d.ctx),
        egui::Direction::LeftToRight
    );
    let c_en = find_exact_text_rect_in_shapes(&out_en.shapes, "C")
        .expect("PaletteBar 'C' cycle button must render in EN");
    assert!(
        c_en.center().x > d.screen.center().x,
        "In LTR English, PaletteBar 'C' must sit on the right half: {c_en:?}"
    );

    d.app.set_ui_language(StudioLanguage::Arabic);
    d.step(Vec::new(), Modifiers::NONE);
    let out_ar = d.step_with_output(Vec::new(), Modifiers::NONE);
    assert_eq!(
        ShellLayoutDirections::from_ctx(&d.ctx),
        egui::Direction::RightToLeft
    );
    let c_ar = find_exact_text_rect_in_shapes(&out_ar.shapes, "C")
        .expect("PaletteBar 'C' cycle button must render in AR");
    assert!(
        c_ar.center().x < d.screen.center().x,
        "In RTL Arabic, PaletteBar 'C' must mirror to the left half: {c_ar:?}"
    );
    let ar_dir = ShellLayoutDirections::from_language(StudioLanguage::Arabic);
    assert!(ar_dir.toolbox_on_right && ar_dir.palette_on_left && ar_dir.dockers_on_left);
    assert!(
        find_text_rect_in_shapes(&out_ar.shapes, I18nKey::MenuFile.tr(StudioLanguage::Arabic))
            .is_some(),
        "Arabic menu bar must render MenuFile translation"
    );
}

#[test]
fn w5b_print_preview_dialog_computes_and_selects_cmyk_spot_separations() {
    let mut d = UiDriver::new();
    let _cmyk_id = d.add_rect(
        "CyanBox",
        20.0,
        20.0,
        40.0,
        40.0,
        Color::cmyk(1.0, 0.0, 0.0, 0.0),
    );
    let _spot_id = d.add_rect(
        "SpotBox",
        70.0,
        20.0,
        40.0,
        40.0,
        Color::spot("SpotInk485", 1.0, [0.0, 0.95, 0.9, 0.0]),
    );
    d.app.show_print_preview_dialog = true;

    d.step(Vec::new(), Modifiers::NONE);
    let out = d.step_with_output(Vec::new(), Modifiers::NONE);
    let sep_lbl = I18nKey::DlgPrintComputeSep.tr_ui(StudioLanguage::English);
    let sep_rect = find_text_rect_in_shapes(&out.shapes, &sep_lbl)
        .expect("Compute Separations button must render in PrintPreviewDialog");
    let sp = sep_rect.center();
    d.step(
        vec![
            Event::PointerMoved(sp),
            Event::PointerButton {
                pos: sp,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: sp,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );

    let plates = d
        .app
        .active_tab()
        .unwrap()
        .separations_preview
        .as_ref()
        .expect("separations_preview must be populated")
        .plates
        .clone();
    assert!(plates.iter().any(|p| p.name == "Cyan"));
    let spot_idx = plates
        .iter()
        .position(|p| p.name.contains("SpotInk485"))
        .expect("SpotInk485 spot plate must be computed");
    let spot_plate_name = plates[spot_idx].name.clone();

    let out = d.step_with_output(Vec::new(), Modifiers::NONE);
    let spot_btn = find_text_rect_in_shapes(&out.shapes, &spot_plate_name)
        .expect("SpotInk485 plate selector must render");
    let pp = spot_btn.center();
    d.step(
        vec![
            Event::PointerMoved(pp),
            Event::PointerButton {
                pos: pp,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: pp,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert_eq!(d.app.print_preview_plate, Some(spot_idx));

    let booklet_lbl = I18nKey::DlgPrintImposBooklet.tr_ui(StudioLanguage::English);
    let booklet_btn = find_text_rect_in_shapes(&out.shapes, &booklet_lbl)
        .expect("Booklet Saddle-Stitch option must render");
    let bp = booklet_btn.center();
    d.step(
        vec![
            Event::PointerMoved(bp),
            Event::PointerButton {
                pos: bp,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: bp,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert_eq!(d.app.production_state.imposition_kind, 1);

    let out = d.step_with_output(Vec::new(), Modifiers::NONE);
    assert!(
        find_text_rect_in_shapes(&out.shapes, "Separations (").is_some(),
        "Print preview dialog must render separations summary"
    );
}

#[test]
fn w5b_carryover_19f_pinch_zoom_precedence_and_clamp_bounds() {
    let mut d = UiDriver::new();
    let canvas_center = d.world_to_pos2(Point2D::new(105.0, 148.5));
    let z0 = d.app.active_tab().unwrap().viewport.zoom;

    // Emit both a positive trackpad pinch Event::Zoom(1.40) and a negative MouseWheel scroll in the same frame:
    // #19f specifies pinch takes precedence over scroll and final factor is clamped to [0.88, 1.14].
    d.step(
        vec![
            Event::PointerMoved(canvas_center),
            Event::Zoom(1.40),
            Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: vec2(0.0, -5.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    let z1 = d.app.active_tab().unwrap().viewport.zoom;
    let ratio = z1 / z0;
    assert!(
        (ratio - 1.14).abs() < 1e-4,
        "#19f pinch must take precedence over wheel scroll and clamp factor to 1.14 (got {ratio})"
    );
}

#[test]
fn w5b_carryover_19k_break_apart_clone_resolves_and_preserves_effects() {
    let mut d = UiDriver::new();
    let master_id = d.add_rect("Master", 20.0, 20.0, 40.0, 30.0, Color::CYAN);
    {
        let tab = d.app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, master_id);
    }
    let clones = d.app.clone_selection();
    assert_eq!(clones.len(), 1);
    let clone_id = clones[0];

    d.app.add_effect_to_selection(Effect::DropShadow {
        dx_mm: 3.0,
        dy_mm: 3.0,
        blur_mm: 2.0,
        opacity: 0.5,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Outside,
    });
    let before_count = d.app.active_tab().unwrap().document.objects().len();
    d.app.break_apart_selection();
    let after_count = d.app.active_tab().unwrap().document.objects().len();
    assert!(
        after_count > before_count,
        "#19k break_apart_selection on a Clone with effects must lower the clone's effects into new objects (before={before_count}, after={after_count}, clone={clone_id:?})"
    );
}

#[test]
fn w5b_carryover_20e_objects_docker_row_click_and_drag_swatch_fire_selection_hook() {
    let mut d = UiDriver::new();
    d.app.show_dockers = true;
    d.app.focus_docker(DockerTab::Objects);
    let rect_a = d.add_rect("HookRectA", 30.0, 30.0, 40.0, 40.0, Color::CYAN);
    let rect_b = d.add_rect("HookRectB", 100.0, 30.0, 40.0, 40.0, Color::MAGENTA);
    {
        let tab = d.app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, rect_a);
    }

    // Register an OnSelectionChange macro hook that updates outline width when selection changes
    d.app.register_macro_hook(
        MacroEventHook::OnSelectionChange,
        "set_outline_mm(all_object_ids(), 3.75);",
    );

    // 1) Click HookRectB row in Objects docker
    d.step(Vec::new(), Modifiers::NONE);
    let out = d.step_with_output(Vec::new(), Modifiers::NONE);
    let row_b = find_text_rect_in_shapes(&out.shapes, "HookRectB")
        .expect("Objects docker must render HookRectB row");
    let rb = row_b.center();
    d.step(
        vec![
            Event::PointerMoved(rb),
            Event::PointerButton {
                pos: rb,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: rb,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    let w_after_row = d
        .app
        .active_tab()
        .unwrap()
        .document
        .object(rect_a)
        .unwrap()
        .outline
        .width_mm;
    assert!(
        (w_after_row - 3.75).abs() < 1e-4,
        "#20e Objects docker row click must fire MacroEventHook::OnSelectionChange (got outline_mm={w_after_row})"
    );

    // 2) Update hook and DragSwatch onto unselected HookRectA -> selects HookRectA and fires OnSelectionChange again
    d.app.register_macro_hook(
        MacroEventHook::OnSelectionChange,
        "set_outline_mm(all_object_ids(), 7.25);",
    );
    let swatch_color = default_cmyk_palette().swatches[1].color.clone();
    let swatch_egui = coree_app::theme::core_color_to_egui(&swatch_color);
    let out = d.step_with_output(Vec::new(), Modifiers::NONE);
    let sw_rect = find_swatch_rect_by_color(&out.shapes, swatch_egui, d.screen.right() - 60.0)
        .expect("Palette swatch must render");
    let start = sw_rect.center();
    let drop_a = d.world_to_pos2(Point2D::new(50.0, 50.0));
    d.step(
        vec![
            Event::PointerMoved(start),
            Event::PointerButton {
                pos: start,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    for t in [0.3_f32, 0.7, 1.0] {
        d.step(
            vec![Event::PointerMoved(start.lerp(drop_a, t))],
            Modifiers::NONE,
        );
    }
    d.step(
        vec![Event::PointerButton {
            pos: drop_a,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    let w_after_drag = d
        .app
        .active_tab()
        .unwrap()
        .document
        .object(rect_b)
        .unwrap()
        .outline
        .width_mm;
    assert!(
        (w_after_drag - 7.25).abs() < 1e-4,
        "#20e DragSwatch onto unselected object ({rect_b:?} -> {rect_a:?}) must fire OnSelectionChange hook (got outline_mm={w_after_drag})"
    );
}

#[test]
fn w5b_arabic_menu_bar_uses_tr_ui_bidi_and_locked_layer_toast() {
    let mut d = UiDriver::new();
    d.app.set_ui_language(StudioLanguage::Arabic);
    d.step(Vec::new(), Modifiers::NONE);
    let out = d.step_with_output(Vec::new(), Modifiers::NONE);

    let laser_menu_label = I18nKey::MenuProduction.tr_ui(StudioLanguage::Arabic);
    let laser_rect = find_exact_text_rect_in_shapes(&out.shapes, &laser_menu_label)
        .expect("Arabic Laser menu button must render in top menu bar");
    let pos = laser_rect.center();
    d.step(
        vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    let menu_out = d.step_with_output(Vec::new(), Modifiers::NONE);
    let expected_bidi = I18nKey::MenuSendLightBurn.tr_ui(StudioLanguage::Arabic);
    let raw_unshaped = I18nKey::MenuSendLightBurn.tr(StudioLanguage::Arabic);
    assert_ne!(expected_bidi, raw_unshaped);
    assert!(
        find_exact_text_rect_in_shapes(&menu_out.shapes, &expected_bidi).is_some(),
        "Arabic menu_bar must render UAX #9 BiDi-ordered label `{expected_bidi}` via tr_ui"
    );
    assert!(
        find_exact_text_rect_in_shapes(&menu_out.shapes, raw_unshaped).is_none(),
        "Arabic menu_bar must NOT bypass tr_ui with raw logical `{raw_unshaped}`"
    );

    // Locked-layer toast must use localized ErrorLayerLocked.tr_ui(Arabic)
    let layer_id = d
        .app
        .active_tab()
        .unwrap()
        .document
        .active_page()
        .unwrap()
        .layers[0]
        .id;
    d.app
        .execute_command(CommandOp::SetLayerFlags {
            layer_id,
            visible: None,
            locked: Some(true),
            printable: None,
            is_cut_layer: None,
        })
        .unwrap();
    assert!(d
        .app
        .ensure_raster_target_at(Point2D::new(20.0, 20.0))
        .is_none());
    let toast = d.app.toasts.last().expect("locked-layer toast expected");
    assert!(toast.is_error);
    assert_eq!(
        toast.message,
        I18nKey::ErrorLayerLocked.tr_ui(StudioLanguage::Arabic)
    );
}

#[test]
fn w5_print_preview_controls_non_aabb_raster_and_spot_plate_tinting() {
    use coree_core::ParametricShape;

    let mut d = UiDriver::new();
    // Create a large non-AABB Ellipse occupying x=[20,190], y=[40,210] (center=(105,125), rx=85, ry=85)
    // filled with SpotInk485 so AABB corners (25, 45) are outside the ellipse!
    {
        let lid = d.app.active_tab().unwrap().active_layer_id;
        let spot_circle = SceneObject::new(
            ObjectId(0),
            "SpotCircle",
            NodePayload::Shape(ParametricShape::Ellipse {
                rx: 85.0,
                ry: 85.0,
                start_deg: 0.0,
                end_deg: 360.0,
                mode: coree_core::EllipseMode::Full,
            }),
        )
        .with_fill(Fill::Solid(Color::spot(
            "SpotInk485",
            1.0,
            [0.0, 0.95, 0.9, 0.0],
        )))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(105.0, 125.0));
        d.app
            .execute_command(CommandOp::create_object(spot_circle, lid))
            .unwrap();
    }
    d.app.show_print_preview_dialog = true;

    // Frame 1: composite SceneRenderer preview texture is generated and uploaded
    let mut frame1_out = d.step_with_raw_output(Vec::new(), Modifiers::NONE);
    let preview_tex_id = d
        .app
        .print_preview_tex
        .borrow()
        .as_ref()
        .map(|(_, h)| h.id())
        .expect("print_preview_tex must be populated by SceneRenderer");

    // Extract Frame 1 texture pixels before clearing textures_delta (prevents Drop panic)
    let uploaded_composite = frame1_out
        .textures_delta
        .set
        .iter()
        .find(|(id, _)| **id == preview_tex_id)
        .map(|(_, deltas)| {
            let egui::ImageData::Color(ref img) = deltas.last().unwrap().image;
            (img.size, img.pixels.clone())
        })
        .expect("First PrintPreviewDialog frame must upload print_preview_tex");
    frame1_out.textures_delta.clear();

    // Frame 2: Window sizing pass completes and all widgets/shapes are visible
    let raw_out = d.step_with_output(Vec::new(), Modifiers::NONE);

    // Verify Scale % DragValue label is exposed interactively
    assert!(
        find_text_rect_in_shapes(
            &raw_out.shapes,
            &I18nKey::PrintScalePercent.tr_ui(StudioLanguage::English)
        )
        .is_some(),
        "PrintScalePercent label/control must render in PrintPreviewDialog"
    );

    // Check Mesh shape referencing preview_tex_id is painted
    fn has_mesh_with_tex(shapes: &[egui::epaint::ClippedShape], tid: egui::TextureId) -> bool {
        fn walk(s: &egui::Shape, tid: egui::TextureId) -> bool {
            match s {
                egui::Shape::Mesh(m) => m.texture_id == tid,
                egui::Shape::Vec(v) => v.iter().any(|c| walk(c, tid)),
                _ => false,
            }
        }
        shapes.iter().any(|cs| walk(&cs.shape, tid))
    }
    assert!(
        has_mesh_with_tex(&raw_out.shapes, preview_tex_id),
        "draw_print_preview_thumbnail must paint rasterized preview texture via painter.image"
    );

    // Inspect uploaded texture delta for composite SceneRenderer non-AABB check
    {
        let ([w, h], pixels) = uploaded_composite;
        // Ellipse AABB top-left corner (~12% x, ~16% y) lies outside the circle -> paper white
        let corner = pixels[(h * 16 / 100) * w + (w * 12 / 100)];
        assert!(
            corner.r() > 240 && corner.g() > 240 && corner.b() > 240,
            "Non-AABB circle AABB corner must stay white (not filled as AABB rect): {corner:?}"
        );
        // Center of circle (~50% x, ~42% y on 210x297 page) -> saturated red SpotInk485
        let center = pixels[(h * 42 / 100) * w + (w * 50 / 100)];
        assert!(
            center.r() > center.g() + 40 && center.r() > center.b() + 40,
            "Center of SpotInk485 circle must render tinted red: {center:?}"
        );
    }

    // Switch to Tiling mode (imposition_kind == 2) -> PrintTileWidth/Height/Overlap DragValues appear
    let tile_lbl = I18nKey::DlgPrintImposTiling.tr_ui(StudioLanguage::English);
    let tile_btn = find_text_rect_in_shapes(&raw_out.shapes, &tile_lbl)
        .expect("Tiling imposition mode button must render");
    let tp = tile_btn.center();
    d.step(
        vec![
            Event::PointerMoved(tp),
            Event::PointerButton {
                pos: tp,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ],
        Modifiers::NONE,
    );
    d.step(
        vec![Event::PointerButton {
            pos: tp,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        Modifiers::NONE,
    );
    assert_eq!(d.app.production_state.imposition_kind, 2);

    let tile_out = d.step_with_output(Vec::new(), Modifiers::NONE);
    for key in [
        I18nKey::PrintTileWidth,
        I18nKey::PrintTileHeight,
        I18nKey::PrintTileOverlap,
    ] {
        assert!(
            find_text_rect_in_shapes(&tile_out.shapes, &key.tr_ui(StudioLanguage::English))
                .is_some(),
            "Tiling mode must expose interactive control for {key:?}"
        );
    }

    // Compute separations and select SpotInk485 plate -> texture must be tinted with Spot color, NOT grayscale!
    d.app.compute_separations_active();
    let spot_idx = d
        .app
        .active_tab()
        .unwrap()
        .separations_preview
        .as_ref()
        .unwrap()
        .plates
        .iter()
        .position(|p| p.name.contains("SpotInk485"))
        .expect("SpotInk485 plate must exist");
    d.app.print_preview_plate = Some(spot_idx);

    let mut sep_raw = d.step_with_raw_output(Vec::new(), Modifiers::NONE);
    let uploaded_spot = sep_raw
        .textures_delta
        .set
        .iter()
        .find(|(id, _)| **id == preview_tex_id)
        .map(|(_, sep_deltas)| {
            let egui::ImageData::Color(ref img) = sep_deltas.last().unwrap().image;
            (img.size, img.pixels.clone())
        })
        .expect("Selecting Spot separation plate must update print_preview_tex");
    sep_raw.textures_delta.clear();
    {
        let ([w, h], pixels) = uploaded_spot;
        let corner = pixels[(h * 16 / 100) * w + (w * 12 / 100)];
        assert!(
            corner.r() > 240 && corner.g() > 240 && corner.b() > 240,
            "Spot plate non-AABB corner must stay white: {corner:?}"
        );
        let center = pixels[(h * 42 / 100) * w + (w * 50 / 100)];
        assert!(
            center.r() > center.g() + 50 && center.r() > center.b() + 50,
            "Spot plate center must be tinted with SpotInk485 ink color (not grayscale): {center:?}"
        );
    }

    // Verify print_scale_pct scales run_imposition_active output
    d.app.production_state.print_scale_pct = 50.0;
    d.app.production_state.imposition_kind = 0; // N-Up
    let before_tabs = d.app.tabs.len();
    d.app.run_imposition_active();
    assert_eq!(d.app.tabs.len(), before_tabs + 1);
    let imposed_doc = &d.app.active_tab().unwrap().document;
    let first_obj = imposed_doc.objects().values().next().unwrap();
    let bb = imposed_doc.world_object_bounds(first_obj.id).unwrap();
    // Original diameter was 170mm; at 50% print scale it must be ~85mm
    assert!(
        (bb.width() - 85.0).abs() < 1.5,
        "print_scale_pct=50 must scale imposed object diameter from 170mm to ~85mm (got {})",
        bb.width()
    );
}
