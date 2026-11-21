// SPDX-License-Identifier: MPL-2.0

//! Comprehensive integration and edge-case tests for `coree-core`.

use coree_core::*;

fn rect_shape(w: f64, h: f64) -> NodePayload {
    NodePayload::Shape(ParametricShape::Rectangle {
        width: w,
        height: h,
        corner_radii: [0.0; 4],
        corner_style: CornerStyle::Rounded,
        lock_corners: true,
    })
}

#[test]
fn unit_and_page_coordinate_transforms() {
    // Direct conversion between every pair via Unit::convert
    let inches = Unit::convert(25.4, Unit::Millimeter, Unit::Inch, 300.0);
    assert!((inches - 1.0).abs() < 1e-12);
    let cdr_units = Unit::convert(1.0, Unit::Inch, Unit::CdrTenthMicron, 300.0);
    assert!((cdr_units - 254_000.0).abs() < 1e-6);
    let px = Unit::convert(1.0, Unit::Inch, Unit::Pixel, 300.0);
    assert!((px - 300.0).abs() < 1e-9);
    assert_eq!(Unit::Millimeter.format_mm(12.345, 96.0, 2), "12.35 mm");

    // Y-up centered CDR page conversion
    let center = Point2D::ZERO.from_centered_y_up(210.0, 297.0);
    assert_eq!(center, Point2D::new(105.0, 148.5));
    let top_left = Point2D::new(-105.0, 148.5).from_centered_y_up(210.0, 297.0);
    assert_eq!(top_left, Point2D::ZERO);
    assert_eq!(
        Point2D::new(10.0, 20.0).flip_y_in_page(297.0),
        Point2D::new(10.0, 277.0)
    );
}

#[test]
fn color_edge_cases_utf8_spot_and_color_styles() {
    // Multibyte UTF-8 strings of length 3/4/6/8 bytes must error cleanly without panicking
    assert!(Color::from_hex("مرح").is_err());
    assert!(Color::from_hex("€€").is_err());
    assert!(Color::from_hex("#€ab").is_err());
    assert!(Color::from_hex("#1234").is_ok());

    let gray = Color::gray(0.5);
    assert_eq!(gray.to_cmyk(), [0.0, 0.0, 0.0, 0.5]);

    let mut style = ColorStyle::new(
        "Brand Accent",
        Color::hsl(200.0, 0.8, 0.5),
        Some(ColorHarmony::Triadic),
    );
    assert_eq!(style.derived_colors.len(), 3);
    style.set_base_color(Color::hsl(30.0, 0.9, 0.5));
    assert!((style.derived_colors[1].to_hsl()[0] - 150.0).abs() < 1e-3);

    // Delta-E of identical colors is 0; black vs white is ~100
    assert!(Color::CYAN.delta_e76(&Color::CYAN) < 1e-4);
    assert!((Color::BLACK.delta_e76(&Color::WHITE) - 100.0).abs() < 1.0);
}

#[test]
fn curved_circle_boolean_ops_and_multi_shape_simplify() {
    let circle_a = ParametricShape::Ellipse {
        rx: 10.0,
        ry: 10.0,
        mode: EllipseMode::Full,
        start_deg: 0.0,
        end_deg: 360.0,
    }
    .to_bezier_path();
    let circle_b = circle_a.transformed(&Affine2D::translate(10.0, 0.0));
    let circle_c = circle_a.transformed(&Affine2D::translate(5.0, 8.0));

    let single_area = std::f64::consts::PI * 100.0;
    let welded = apply_boolean(BooleanOp::Weld, &circle_a, &circle_b).unwrap();
    assert_eq!(welded.len(), 1);
    assert!(welded[0].area_mm2() > single_area && welded[0].area_mm2() < 2.0 * single_area);

    let inter = apply_boolean(BooleanOp::Intersect, &circle_a, &circle_b).unwrap();
    assert_eq!(inter.len(), 1);
    assert!(inter[0].area_mm2() > 50.0 && inter[0].area_mm2() < single_area);

    // 3-shape Simplify: sum of visible trimmed areas equals total union area
    let simplified = apply_boolean_many(
        BooleanOp::Simplify,
        &[circle_a.clone(), circle_b.clone(), circle_c.clone()],
    )
    .unwrap();
    let welded_all = apply_boolean_many(BooleanOp::Weld, &[circle_a, circle_b, circle_c]).unwrap();
    let sum_simp: f64 = simplified.iter().map(BezierPath::area_mm2).sum();
    assert!((sum_simp - welded_all[0].area_mm2()).abs() < 2.0);
}

#[test]
fn knife_open_path_virtual_segment_closed_loop_and_extreme_coords() {
    // Open path knife cut splits a horizontal line into two open segments
    let line = BezierPath::from_line(Point2D::new(0.0, 0.0), Point2D::new(30.0, 0.0));
    let pieces = knife_cut(
        &line,
        Point2D::new(12.0, -5.0),
        Point2D::new(12.0, 5.0),
        false,
    );
    assert_eq!(pieces.len(), 2);
    assert!((pieces[0].path_length_mm() - 12.0).abs() < 1e-3);
    assert!((pieces[1].path_length_mm() - 18.0).abs() < 1e-3);

    // Closed square with vertical cutter slicing top and bottom -> virtual delete right half
    let sq = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0));
    let cutter = BezierPath::from_line(Point2D::new(10.0, -5.0), Point2D::new(10.0, 25.0));
    let remaining = virtual_segment_delete(&sq, &[cutter], Point2D::new(20.0, 10.0), 2.0);
    assert_eq!(remaining.subpaths.len(), 1);
    assert!(!remaining.subpaths[0].closed);
    // Remaining left U-shape length: 10 (top left) + 20 (left) + 10 (bottom left) = 40mm
    assert!((remaining.path_length_mm() - 40.0).abs() < 0.5);

    // Open path erase circle leaves two outer tails
    let erased_line = erase_circle(&line, Point2D::new(15.0, 0.0), 5.0);
    assert_eq!(erased_line.len(), 2);
    assert!((erased_line[0].path_length_mm() - 10.0).abs() < 0.2);
    assert!((erased_line[1].path_length_mm() - 10.0).abs() < 0.2);

    // Extreme huge coordinates and NaN must never panic
    let huge = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 1e200, 1e200));
    let _ = knife_cut(
        &huge,
        Point2D::new(1e199, -1.0),
        Point2D::new(1e199, 1e201),
        true,
    );
    let _ = erase_circle(&huge, Point2D::new(1e199, 1e199), 1e198);
    assert!(offset_polygon(&huge, 1e9).is_err());
}

#[test]
fn smart_drawing_and_all_artistic_media_styles() {
    // Triangle recognition
    let tri_pts = vec![
        Point2D::new(0.0, 0.0),
        Point2D::new(20.0, 0.0),
        Point2D::new(10.0, 20.0),
        Point2D::new(0.2, 0.1),
    ];
    assert!(matches!(
        SmartDrawing::recognize_rough_stroke(&tri_pts),
        RecognizedShape::Triangle { .. }
    ));

    // Rectangle recognition
    let rect_pts = vec![
        Point2D::new(0.0, 0.0),
        Point2D::new(40.0, 0.0),
        Point2D::new(40.0, 20.0),
        Point2D::new(0.0, 20.0),
        Point2D::new(0.1, 0.1),
    ];
    assert!(matches!(
        SmartDrawing::recognize_rough_stroke(&rect_pts),
        RecognizedShape::Rectangle { .. }
    ));

    // All 5 ArtisticMediaStyle variants expand cleanly
    let center = BezierPath::from_line(Point2D::new(0.0, 0.0), Point2D::new(40.0, 0.0));
    let styles = [
        ArtisticMediaStyle::Preset {
            max_width_mm: 5.0,
            profile: ArtisticWidthProfile::TaperBoth,
        },
        ArtisticMediaStyle::Brush {
            width_mm: 4.0,
            smoothness: 0.6,
        },
        ArtisticMediaStyle::Sprayer {
            spacing_mm: 8.0,
            size_mm: 3.0,
            scatter_mm: 1.5,
            seed: 7,
        },
        ArtisticMediaStyle::Calligraphic {
            width_mm: 4.0,
            angle_deg: 45.0,
        },
        ArtisticMediaStyle::Expression {
            min_width_mm: 1.0,
            max_width_mm: 6.0,
            pressures: vec![0.2, 1.0, 0.4],
        },
    ];
    for st in &styles {
        let out = ArtisticMedia::expand_stroke(&center, st);
        assert!(!out.is_empty() && out.is_finite());
    }
}

#[test]
fn lens_clamping_and_fisheye_mapping() {
    let clamped = LensKind::Brighten { rate_pct: 500.0 }.clamped();
    assert_eq!(clamped, LensKind::Brighten { rate_pct: 100.0 });
    assert_eq!(
        LensKind::Brighten { rate_pct: 100.0 }
            .apply_color(&Color::rgb(0.2, 0.4, 0.6))
            .to_rgba8(),
        [255, 255, 255, 255]
    );
    assert_eq!(
        LensKind::Brighten { rate_pct: -100.0 }
            .apply_color(&Color::rgb(0.2, 0.4, 0.6))
            .to_rgba8(),
        [0, 0, 0, 255]
    );

    let nan_mag = LensKind::Magnify { amount: f32::NAN }.clamped();
    assert_eq!(nan_mag, LensKind::Magnify { amount: 2.0 });

    // FishEye pushes interior point outward for positive rate
    let center = Point2D::new(50.0, 50.0);
    let pt = Point2D::new(60.0, 50.0);
    let warped = LensKind::FishEye { rate_pct: 200.0 }.map_point(pt, center, 30.0);
    assert!(warped.distance_to(center) > pt.distance_to(center));
}

#[test]
fn all_command_variants_undo_redo_and_rhai_macro_roundtrip() {
    let mut doc = Document::new_a4("All Commands");
    let mut hist = CommandHistory::default();

    let page1 = doc.pages[0].id;
    let layer1 = doc.pages[0].layers[0].id;

    // 1. AddLayer + SetLayerFlags
    hist.execute(
        &mut doc,
        CommandOp::AddLayer {
            page_id: page1,
            name: "CutContour".into(),
            is_cut_layer: true,
            master_scope: MasterLayerScope::AllPages,
        },
    )
    .unwrap();
    let cut_layer = doc.pages[0].layers[1].id;
    hist.execute(
        &mut doc,
        CommandOp::SetLayerFlags {
            layer_id: cut_layer,
            visible: Some(true),
            locked: Some(false),
            printable: Some(true),
            is_cut_layer: Some(true),
        },
    )
    .unwrap();

    // 2. CreateObject (3 objects)
    let o1 = SceneObject::new(ObjectId(0), "Box1", rect_shape(20.0, 20.0));
    let o2 = SceneObject::new(ObjectId(0), "Box2", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(30.0, 0.0));
    let o3 = SceneObject::new(ObjectId(0), "Box3", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(100.0, 0.0));
    hist.execute(&mut doc, CommandOp::create_object(o1, Some(layer1)))
        .unwrap();
    hist.execute(&mut doc, CommandOp::create_object(o2, Some(layer1)))
        .unwrap();
    hist.execute(&mut doc, CommandOp::create_object(o3, Some(layer1)))
        .unwrap();
    let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
    assert_eq!(ids.len(), 3);

    // 3. SetFill, SetOutline, SetLens, AddEffect, RemoveEffect
    hist.execute(
        &mut doc,
        CommandOp::SetFill {
            ids: vec![ids[0]],
            fill: Fill::Solid(Color::CYAN),
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::SetOutline {
            ids: vec![ids[0]],
            outline: OutlineStyle::cut_contour(),
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::SetLens {
            ids: vec![ids[0]],
            lens: Some(LensConfig::new(LensKind::HeatMap {
                palette_rotation_deg: 15.0,
            })),
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::AddEffect {
            id: ids[0],
            effect: Effect::DropShadow {
                dx_mm: 2.0,
                dy_mm: 2.0,
                blur_mm: 3.0,
                opacity: 0.5,
                color: Color::BLACK,
                feather_direction: FeatherDirection::Outside,
            },
        },
    )
    .unwrap();
    assert_eq!(doc.objects[&ids[0]].effects.len(), 1);
    hist.execute(
        &mut doc,
        CommandOp::RemoveEffect {
            id: ids[0],
            effect_index: 0,
        },
    )
    .unwrap();
    assert!(doc.objects[&ids[0]].effects.is_empty());

    // 4. Distribute, Align, ZOrder, ConvertToCurves, EditPath
    hist.execute(
        &mut doc,
        CommandOp::DistributeObjects {
            ids: ids.clone(),
            op: DistributeOp::SpacingH,
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::AlignObjects {
            ids: ids.clone(),
            op: AlignOp::Top,
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::SetZOrder {
            id: ids[0],
            op: ZOrderOp::BringToFront,
        },
    )
    .unwrap();
    hist.execute(&mut doc, CommandOp::ConvertToCurves { id: ids[0] })
        .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::EditPath {
            id: ids[0],
            path: BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 25.0, 25.0)),
        },
    )
    .unwrap();

    // 5. Group & Ungroup & PowerClip & ExtractPowerClip
    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![ids[1], ids[2]],
            name: "G1".into(),
        },
    )
    .unwrap();
    let group_id = *doc
        .objects
        .iter()
        .find(|(_, o)| matches!(o.payload, NodePayload::Group(_)))
        .unwrap()
        .0;
    hist.execute(&mut doc, CommandOp::Ungroup { group_id })
        .unwrap();

    hist.execute(
        &mut doc,
        CommandOp::CreatePowerClip {
            container_id: ids[0],
            content_ids: vec![ids[1]],
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::ExtractPowerClip {
            powerclip_id: ids[0],
        },
    )
    .unwrap();

    // 6. EditText, EditTable, UpdateRaster
    hist.execute(
        &mut doc,
        CommandOp::EditText {
            id: ids[2],
            payload: NodePayload::ArtisticText(ArtisticText::from_plain(
                "نص عربي",
                TextStyle::default(),
            )),
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::EditTable {
            id: ids[2],
            table: VectorTable::new(2, 3, 15.0, 8.0),
        },
    )
    .unwrap();
    let asset = RasterAsset::new(1, 1, vec![255, 0, 0, 255], 300.0, ColorMode::Rgb).unwrap();
    let asset_id = doc.register_asset(asset);
    hist.execute(
        &mut doc,
        CommandOp::UpdateRaster {
            id: ids[2],
            data: RasterLayerData {
                asset_id,
                width_mm: 10.0,
                height_mm: 10.0,
                filters: vec![RasterFilter::Invert],
                crop_rect: None,
            },
        },
    )
    .unwrap();

    // 7. Page operations + Rhai macro recording & playback
    hist.execute(
        &mut doc,
        CommandOp::AddPage {
            name: "Page 2".into(),
            width_mm: 210.0,
            height_mm: 297.0,
            bleed_mm: 3.0,
        },
    )
    .unwrap();
    let page2 = doc.pages[1].id;
    hist.execute(
        &mut doc,
        CommandOp::RenamePage {
            page_id: page2,
            new_name: "Back Cover".into(),
        },
    )
    .unwrap();
    assert_eq!(doc.pages[1].name, "Back Cover");

    // Record a self-contained macro on a fresh document and replay it on an identical fresh document
    let mut macro_src = Document::new_a4("Macro Source");
    let mut macro_src_hist = CommandHistory::default();
    macro_src_hist.start_macro_recording();
    macro_src_hist
        .execute(
            &mut macro_src,
            CommandOp::AddPage {
                name: "Page 2".into(),
                width_mm: 210.0,
                height_mm: 297.0,
                bleed_mm: 3.0,
            },
        )
        .unwrap();
    let new_page_id = macro_src.pages[1].id;
    macro_src_hist
        .execute(
            &mut macro_src,
            CommandOp::RenamePage {
                page_id: new_page_id,
                new_name: "Back Cover".into(),
            },
        )
        .unwrap();
    let script = macro_src_hist.stop_macro_to_rhai();
    assert!(script.contains("add_page(\"Page 2\", 210.0, 297.0, 3.0);"));

    let mut doc_macro = Document::new_a4("Fresh");
    let mut hist_macro = CommandHistory::default();
    run_rhai_macro(&mut doc_macro, &mut hist_macro, &script).unwrap();
    assert_eq!(doc_macro.pages.len(), 2);
    assert_eq!(doc_macro.pages[1].name, "Back Cover");

    // A macro that fails mid-way rolls back atomically
    let failing_script = format!(
        "add_page(\"Temp\", 100.0, 100.0, 0.0);\nexec_command_json(\"{}\");\n",
        serde_json::to_string(&CommandOp::RenamePage {
            page_id: PageId(9999),
            new_name: "Missing".into(),
        })
        .unwrap()
        .replace('"', "\\\"")
    );
    assert!(run_rhai_macro(&mut doc_macro, &mut hist_macro, &failing_script).is_err());
    assert_eq!(doc_macro.pages.len(), 2);

    // 8. Cancel transaction rolls back completely
    let pages_before = doc.pages.len();
    hist.begin_transaction(&doc, "Cancelled").unwrap();
    hist.execute(&mut doc, CommandOp::DeletePage { page_id: page2 })
        .unwrap();
    assert_eq!(doc.pages.len(), pages_before - 1);
    hist.cancel_transaction(&mut doc).unwrap();
    assert_eq!(doc.pages.len(), pages_before);

    // 9. Deleting a Group recursively cleans up its children with zero orphans
    let g_id = doc.group_objects(&[ids[0], ids[1]], "ToDelete").unwrap();
    doc.remove_object(g_id).unwrap();
    assert!(!doc.objects.contains_key(&ids[0]));
    assert!(!doc.objects.contains_key(&ids[1]));
}

#[test]
fn regression_cyclic_connector_and_no_double_transform() {
    let mut doc = Document::new_a4("Connectors");
    let r1 = doc
        .add_object(
            SceneObject::new(ObjectId(0), "R1", rect_shape(10.0, 10.0)),
            None,
        )
        .unwrap();
    let r2 = doc
        .add_object(
            SceneObject::new(ObjectId(0), "R2", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(30.0, 0.0)),
            None,
        )
        .unwrap();

    // Connector with non-identity transform must NOT double-transform world endpoints
    let conn_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Link",
                NodePayload::Connector(ConnectorObject::between_objects(
                    r1,
                    AnchorPoint::Right,
                    r2,
                    AnchorPoint::Left,
                    ConnectorStyle::Straight,
                )),
            )
            .with_transform(Affine2D::translate(50.0, 50.0)),
            None,
        )
        .unwrap();
    let bb = doc.object_bounds(conn_id).expect("connector bounds");
    assert!((bb.min_x - 10.0).abs() < 1e-6);
    assert!((bb.max_x - 30.0).abs() < 1e-6);
    assert!((bb.min_y - 5.0).abs() < 1e-6);

    // Converting transformed connector to curves preserves exact world bounds
    doc.convert_to_curves(conn_id).unwrap();
    let bb_after = doc.object_bounds(conn_id).expect("converted bounds");
    assert!((bb_after.min_x - 10.0).abs() < 1e-6);
    assert!((bb_after.max_x - 30.0).abs() < 1e-6);

    // Self-referencing connector must NOT overflow the stack
    let self_id = doc.alloc_object_id();
    let self_conn = SceneObject::new(
        self_id,
        "SelfCycle",
        NodePayload::Connector(ConnectorObject::between_objects(
            self_id,
            AnchorPoint::Center,
            self_id,
            AnchorPoint::Center,
            ConnectorStyle::Straight,
        )),
    );
    doc.add_object(self_conn, None).unwrap();
    let _ = doc.object_bounds(self_id);

    // Mutual cycle c1 <-> c2 must NOT overflow the stack
    let c1 = doc.alloc_object_id();
    let c2 = doc.alloc_object_id();
    doc.add_object(
        SceneObject::new(
            c1,
            "C1",
            NodePayload::Connector(ConnectorObject::between_objects(
                c2,
                AnchorPoint::Center,
                c2,
                AnchorPoint::Right,
                ConnectorStyle::Straight,
            )),
        ),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            c2,
            "C2",
            NodePayload::Connector(ConnectorObject::between_objects(
                c1,
                AnchorPoint::Left,
                c1,
                AnchorPoint::Center,
                ConnectorStyle::Straight,
            )),
        ),
        None,
    )
    .unwrap();
    let _ = doc.object_bounds(c1);
    let _ = doc.object_bounds(c2);
}

#[test]
fn regression_non_finite_bounds_do_not_panic_spatial_index() {
    let mut doc = Document::new_a4("Spatial NaN");
    let good = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Good", rect_shape(10.0, 10.0)),
            None,
        )
        .unwrap();
    let bad_raster = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "BadRaster",
                NodePayload::Raster(RasterLayerData {
                    asset_id: ImageAssetId::new(),
                    width_mm: f64::NAN,
                    height_mm: 10.0,
                    filters: Vec::new(),
                    crop_rect: None,
                }),
            ),
            None,
        )
        .unwrap();
    let bad_crop = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "BadCrop",
                NodePayload::Raster(RasterLayerData {
                    asset_id: ImageAssetId::new(),
                    width_mm: 10.0,
                    height_mm: 10.0,
                    filters: Vec::new(),
                    crop_rect: Some(Rect2D::new(f64::INFINITY, 0.0, 10.0, 10.0)),
                }),
            ),
            None,
        )
        .unwrap();
    let bad_xf = doc
        .add_object(
            SceneObject::new(ObjectId(0), "BadXf", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(f64::NAN, 0.0)),
            None,
        )
        .unwrap();

    assert!(doc.object_bounds(bad_raster).is_none());
    assert!(doc.object_bounds(bad_crop).is_none());
    assert!(doc.object_bounds(bad_xf).is_none());

    let idx = doc.build_spatial_index(0);
    assert_eq!(idx.len(), 1);
    assert_eq!(doc.hit_test(Point2D::new(5.0, 5.0), 1.0), Some(good));
}

#[test]
fn regression_even_complex_stars_generate_valid_geometry() {
    for points in [5u32, 6, 7, 8, 9, 10, 12] {
        let star = ParametricShape::Star {
            points,
            outer_radius: 20.0,
            inner_ratio: 0.4,
            complex: true,
        }
        .to_bezier_path();
        assert!(!star.is_empty(), "complex star {points} was empty");
        assert!(star.is_finite());
        assert!(
            star.area_mm2() > 10.0,
            "complex star {points} had zero area"
        );
    }
    // {6/2} hexagram consists of 2 interlocking triangles
    let hexagram = ParametricShape::Star {
        points: 6,
        outer_radius: 20.0,
        inner_ratio: 0.5,
        complex: true,
    }
    .to_bezier_path();
    assert_eq!(hexagram.subpaths.len(), 2);
    assert_eq!(hexagram.subpaths[0].nodes.len(), 3);
}

#[test]
fn regression_virtual_segment_delete_closed_seam_and_subpath_indices() {
    // Closed square split vertically at x=10; clicking LEFT edge (0,10) must delete entire left half
    // (leaving right U-half of length 40mm, NOT 50mm with a ghost seam at (0,0))
    let sq = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0));
    let cutter = BezierPath::from_line(Point2D::new(10.0, -5.0), Point2D::new(10.0, 25.0));
    let left_deleted = virtual_segment_delete(&sq, &[cutter], Point2D::new(0.0, 10.0), 2.0);
    assert_eq!(left_deleted.subpaths.len(), 1);
    assert!(!left_deleted.subpaths[0].closed);
    assert!(
        (left_deleted.path_length_mm() - 40.0).abs() < 0.5,
        "expected 40mm, got {}",
        left_deleted.path_length_mm()
    );

    // Multi-subpath with leading degenerate single-node subpath must preserve untouched subpath indices
    let compound = BezierPath {
        subpaths: vec![
            Subpath::from_points([Point2D::new(99.0, 99.0)], false),
            Subpath::from_points(Rect2D::from_xywh(0.0, 0.0, 10.0, 10.0).corners(), true),
            Subpath::from_points([Point2D::new(50.0, 0.0), Point2D::new(70.0, 0.0)], false),
        ],
    };
    let line_cut = BezierPath::from_line(Point2D::new(60.0, -5.0), Point2D::new(60.0, 5.0));
    let trimmed = virtual_segment_delete(&compound, &[line_cut], Point2D::new(65.0, 0.0), 2.0);
    // Subpath 1 (the 10x10 square) must survive intact with area 100mm²
    assert!(trimmed
        .subpaths
        .iter()
        .any(|sp| sp.closed
            && (BezierPath::from_subpath(sp.clone()).area_mm2() - 100.0).abs() < 1e-2));
}

#[test]
fn regression_knife_cut_through_interior_vertex() {
    // Open 2-segment polyline cut right through its middle vertex (10, 0)
    let poly = BezierPath::from_polygon(
        [
            Point2D::new(0.0, 0.0),
            Point2D::new(10.0, 0.0),
            Point2D::new(20.0, 0.0),
        ],
        false,
    );
    let halves = knife_cut(
        &poly,
        Point2D::new(10.0, -5.0),
        Point2D::new(10.0, 5.0),
        false,
    );
    assert_eq!(halves.len(), 2);
    assert!((halves[0].path_length_mm() - 10.0).abs() < 1e-3);
    assert!((halves[1].path_length_mm() - 10.0).abs() < 1e-3);

    // Closed square cut diagonally through two opposite corners (0,0) and (20,20)
    let sq = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0));
    let diag = knife_cut(
        &sq,
        Point2D::new(-5.0, -5.0),
        Point2D::new(25.0, 25.0),
        false,
    );
    assert_eq!(diag.len(), 2);
    assert!((diag[0].path_length_mm() - 40.0).abs() < 1e-2);
    assert!((diag[1].path_length_mm() - 40.0).abs() < 1e-2);
}

#[test]
fn regression_table_merge_insert_repair_and_bounds() {
    // Vertical merge split by insert_row must NOT leave cells permanently hidden
    let mut t = VectorTable::new(3, 2, 20.0, 10.0);
    t.merge_cells(0, 0, 2, 0).unwrap();
    t.insert_row(1, 10.0).unwrap();
    for r in 0..t.rows {
        assert!(
            t.cell_rect(r, 0).is_some(),
            "cell ({r}, 0) became permanently hidden after insert_row"
        );
    }

    // Horizontal merge split by insert_col must NOT leave cells permanently hidden
    let mut t2 = VectorTable::new(2, 3, 20.0, 10.0);
    t2.merge_cells(0, 0, 0, 2).unwrap();
    t2.insert_col(1, 20.0).unwrap();
    for c in 0..t2.cols {
        assert!(
            t2.cell_rect(0, c).is_some(),
            "cell (0, {c}) became permanently hidden after insert_col"
        );
    }

    // Deserialized zero-span cell must not underflow
    let mut malformed = VectorTable::new(2, 2, 10.0, 10.0);
    malformed.cell_mut(0, 0).unwrap().row_span = 0;
    malformed.cell_mut(0, 0).unwrap().col_span = 0;
    malformed.merge_cells(0, 0, 1, 1).unwrap();
    assert_eq!(malformed.cell(0, 0).unwrap().row_span, 2);

    // Extreme dimensions clamp to MAX_DIM without OOM
    let huge = VectorTable::new(100_000, 100_000, 10.0, 10.0);
    assert_eq!(huge.rows, VectorTable::MAX_DIM);
    assert_eq!(huge.cols, VectorTable::MAX_DIM);
}

#[test]
fn regression_nested_ungroup_powerclip_and_strict_base64() {
    let mut doc = Document::new_a4("Hierarchy");
    let a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "A", rect_shape(10.0, 10.0)),
            None,
        )
        .unwrap();
    let b = doc
        .add_object(
            SceneObject::new(ObjectId(0), "B", rect_shape(10.0, 10.0)),
            None,
        )
        .unwrap();
    let c = doc
        .add_object(
            SceneObject::new(ObjectId(0), "C", rect_shape(10.0, 10.0)),
            None,
        )
        .unwrap();

    // Nested group: outer([inner([a, b]), c]) -> ungroup inner puts [a, b] into outer
    let inner = doc.group_objects(&[a, b], "Inner").unwrap();
    let outer = doc.group_objects(&[inner, c], "Outer").unwrap();
    doc.ungroup_object(inner).unwrap();
    assert!(!doc.objects.contains_key(&inner));
    assert_eq!(
        doc.objects[&outer].payload,
        NodePayload::Group(vec![a, b, c])
    );

    // Nested PowerClip inside outer group -> extract_powerclip restores contents into outer
    let pc_child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Child", rect_shape(5.0, 5.0))
                .with_transform(Affine2D::translate(20.0, 20.0)),
            None,
        )
        .unwrap();
    doc.create_powerclip(c, &[pc_child]).unwrap();
    // Re-adding pc_child to the same PowerClip must be idempotent (no duplicate ID, no double inverse)
    let tf_once = doc.objects[&pc_child].transform;
    doc.create_powerclip(c, &[pc_child]).unwrap();
    assert_eq!(doc.objects[&pc_child].transform, tf_once);
    if let NodePayload::PowerClip { contents, .. } = &doc.objects[&c].payload {
        assert_eq!(contents, &vec![pc_child]);
    } else {
        panic!("expected PowerClip");
    }

    doc.extract_powerclip(c).unwrap();
    assert_eq!(doc.parent_container_of(pc_child), Some(outer));

    // ConvertToCurves on PowerClip cleans up clipped children with zero orphans
    doc.create_powerclip(c, &[pc_child]).unwrap();
    doc.convert_to_curves(c).unwrap();
    assert!(!doc.objects.contains_key(&pc_child));

    // Strict base64 rejects malformed padding ("AA=A" and "AA==AAAA")
    let bad1 = r#"{"fourcc":[118,114,115,110],"version_hint":1,"raw_bytes":"AA=A"}"#;
    let bad2 = r#"{"fourcc":[118,114,115,110],"version_hint":1,"raw_bytes":"AA==AAAA"}"#;
    assert!(serde_json::from_str::<UnknownCdrChunk>(bad1).is_err());
    assert!(serde_json::from_str::<UnknownCdrChunk>(bad2).is_err());
}

#[test]
fn regression_command_locks_tx_undo_rhai_fidelity_and_simplify_styles() {
    let mut doc = Document::new_a4("Command Hardening");
    let mut hist = CommandHistory::default();

    let mut locked_obj = SceneObject::new(ObjectId(0), "Locked", rect_shape(10.0, 10.0));
    locked_obj.locked = true;
    let locked_id = doc.add_object(locked_obj, None).unwrap();

    // Locked object must reject DeleteObjects, BooleanOp, and ConvertToCurves
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::DeleteObjects { ids: vec![locked_id] }),
        Err(CoreError::ObjectLocked(id)) if id == locked_id
    ));
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::BooleanOp {
                op: BooleanOp::Weld,
                ids: vec![locked_id],
            }
        ),
        Err(CoreError::ObjectLocked(id)) if id == locked_id
    ));
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::ConvertToCurves { id: locked_id }),
        Err(CoreError::ObjectLocked(id)) if id == locked_id
    ));
    assert!(doc.objects.contains_key(&locked_id));

    // undo() and redo() must fail while a transaction is active
    hist.begin_transaction(&doc, "OpenTx").unwrap();
    assert!(matches!(
        hist.undo(&mut doc),
        Err(CoreError::TransactionAlreadyActive)
    ));
    assert!(matches!(
        hist.redo(&mut doc),
        Err(CoreError::TransactionAlreadyActive)
    ));
    hist.cancel_transaction(&mut doc).unwrap();

    // Rhai macro preserves CMYK and Spot colors without lossy hex conversion
    let target = doc
        .add_object(
            SceneObject::new(ObjectId(0), "PrintObj", rect_shape(10.0, 10.0)),
            None,
        )
        .unwrap();
    let spot_cut = Color::cut_contour_spot();
    hist.start_macro_recording();
    hist.execute(
        &mut doc,
        CommandOp::SetFill {
            ids: vec![target],
            fill: Fill::Solid(Color::CYAN),
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::SetFill {
            ids: vec![target],
            fill: Fill::Solid(spot_cut.clone()),
        },
    )
    .unwrap();
    let macro_script = hist.stop_macro_to_rhai();
    assert!(!macro_script.contains("set_fill_hex"));

    let mut doc2 = Document::new_a4("Playback");
    let mut clone_obj = SceneObject::new(target, "PrintObj", rect_shape(10.0, 10.0));
    clone_obj.id = target;
    doc2.add_object(clone_obj, None).unwrap();
    let mut hist2 = CommandHistory::default();
    run_rhai_macro(&mut doc2, &mut hist2, &macro_script).unwrap();
    assert_eq!(doc2.objects[&target].fill, Fill::Solid(spot_cut));

    // Invalid Rhai macro arguments return explicit errors instead of being silently ignored
    assert!(run_rhai_macro(&mut doc2, &mut hist2, r#"set_fill_hex([1], "not-hex");"#).is_err());
    assert!(run_rhai_macro(&mut doc2, &mut hist2, r#"align_objects([1], "diagonal");"#).is_err());
    assert!(run_rhai_macro(&mut doc2, &mut hist2, r#"exec_command_json("{bad");"#).is_err());
    assert!(run_rhai_macro(
        &mut doc2,
        &mut hist2,
        r#"transform_objects([1], [1.0, 2.0]);"#
    )
    .is_err());

    // BooleanOp::Simplify preserves each source object's distinct fill color
    let mut doc3 = Document::new_a4("Simplify Styles");
    let mut hist3 = CommandHistory::default();
    let bottom = doc3
        .add_object(
            SceneObject::new(ObjectId(0), "Bottom", rect_shape(20.0, 10.0))
                .with_fill(Fill::Solid(Color::CYAN)),
            None,
        )
        .unwrap();
    let top = doc3
        .add_object(
            SceneObject::new(ObjectId(0), "Top", rect_shape(20.0, 10.0))
                .with_fill(Fill::Solid(Color::MAGENTA))
                .with_transform(Affine2D::translate(10.0, 0.0)),
            None,
        )
        .unwrap();
    hist3
        .execute(
            &mut doc3,
            CommandOp::BooleanOp {
                op: BooleanOp::Simplify,
                ids: vec![bottom, top],
            },
        )
        .unwrap();
    assert_eq!(doc3.objects.len(), 2);
    let fills: Vec<Fill> = doc3.objects.values().map(|o| o.fill.clone()).collect();
    assert!(fills.contains(&Fill::Solid(Color::CYAN)));
    assert!(fills.contains(&Fill::Solid(Color::MAGENTA)));
}

#[test]
fn regression_angular_nan_and_zero_sx_decompose() {
    let bad_ang = DimensionObject::angular(
        Point2D::new(f64::NAN, 0.0),
        Point2D::new(10.0, 0.0),
        Point2D::new(0.0, 10.0),
        5.0,
    );
    assert_eq!(bad_ang.compute_measurement(Unit::Millimeter), "0.0°");

    // Zero-X scale combined with rotation must reconstruct accurately from decompose()
    let mat = Affine2D::translate(5.0, -3.0)
        .compose(&Affine2D::rotate_deg(30.0))
        .compose(&Affine2D::scale(0.0, 2.0));
    let dec = mat.decompose();
    let rebuilt = Affine2D::translate(dec.translation.x, dec.translation.y)
        .compose(&Affine2D::rotate(dec.rotation_rad))
        .compose(&Affine2D::skew(dec.skew_x_rad, 0.0))
        .compose(&Affine2D::scale(dec.scale.x, dec.scale.y));
    for (got, expected) in rebuilt.m.iter().zip(mat.m.iter()) {
        assert!(
            (got - expected).abs() < 1e-9,
            "got {got} expected {expected}"
        );
    }
}
