// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

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

    let page1 = doc.pages()[0].id;
    let layer1 = doc.pages()[0].layers[0].id;

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
    let cut_layer = doc.pages()[0].layers[1].id;
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
    let ids: Vec<ObjectId> = doc.objects().keys().copied().collect();
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
    assert_eq!(doc.object(ids[0]).unwrap().effects.len(), 1);
    hist.execute(
        &mut doc,
        CommandOp::RemoveEffect {
            id: ids[0],
            effect_index: 0,
        },
    )
    .unwrap();
    assert!(doc.object(ids[0]).unwrap().effects.is_empty());

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
        .objects()
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
                filter_enabled: Vec::new(),
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
    let page2 = doc.pages()[1].id;
    hist.execute(
        &mut doc,
        CommandOp::RenamePage {
            page_id: page2,
            new_name: "Back Cover".into(),
        },
    )
    .unwrap();
    assert_eq!(doc.pages()[1].name, "Back Cover");

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
    let new_page_id = macro_src.pages()[1].id;
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
    assert_eq!(doc_macro.pages().len(), 2);
    assert_eq!(doc_macro.pages()[1].name, "Back Cover");

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
    assert_eq!(doc_macro.pages().len(), 2);

    // 8. Cancel transaction rolls back completely
    let pages_before = doc.pages().len();
    hist.begin_transaction(&doc, "Cancelled").unwrap();
    hist.execute(&mut doc, CommandOp::DeletePage { page_id: page2 })
        .unwrap();
    assert_eq!(doc.pages().len(), pages_before - 1);
    hist.cancel_transaction(&mut doc).unwrap();
    assert_eq!(doc.pages().len(), pages_before);

    // 9. Deleting a Group recursively cleans up its children with zero orphans
    let g_id = doc.group_objects(&[ids[0], ids[1]], "ToDelete").unwrap();
    doc.remove_object(g_id).unwrap();
    assert!(!doc.has_object(ids[0]));
    assert!(!doc.has_object(ids[1]));
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
                    filter_enabled: Vec::new(),
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
                    filter_enabled: Vec::new(),
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
    assert!(!doc.has_object(inner));
    assert_eq!(
        doc.object(outer).unwrap().payload,
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
    let tf_once = doc.object(pc_child).unwrap().transform;
    doc.create_powerclip(c, &[pc_child]).unwrap();
    assert_eq!(doc.object(pc_child).unwrap().transform, tf_once);
    if let NodePayload::PowerClip { contents, .. } = &doc.object(c).unwrap().payload {
        assert_eq!(contents, &vec![pc_child]);
    } else {
        panic!("expected PowerClip");
    }

    doc.extract_powerclip(c).unwrap();
    assert_eq!(doc.parent_container_of(pc_child), Some(outer));

    // ConvertToCurves on PowerClip cleans up clipped children with zero orphans
    doc.create_powerclip(c, &[pc_child]).unwrap();
    doc.convert_to_curves(c).unwrap();
    assert!(!doc.has_object(pc_child));

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
    assert!(doc.has_object(locked_id));

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
    assert_eq!(doc2.object(target).unwrap().fill, Fill::Solid(spot_cut));

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
    assert_eq!(doc3.objects().len(), 2);
    let fills: Vec<Fill> = doc3.objects().values().map(|o| o.fill.clone()).collect();
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

#[test]
fn autofit_page_to_content_resizes_shifts_and_ignores_hidden_objects() {
    let mut doc = Document::new_a4("Autofit Test");
    // Empty page must return an error instead of collapsing page size
    assert!(doc.autofit_page_to_content(0, 5.0).is_err());
    // Out-of-range page index returns PageIndexOutOfRange
    assert!(matches!(
        doc.autofit_page_to_content(9, 5.0),
        Err(CoreError::PageIndexOutOfRange { index: 9, len: 1 })
    ));

    // Place two visible rects at (40, 60, 30x20) and (90, 80, 30x30) -> union [40..120, 60..110] (80x50)
    let r1 = doc
        .add_object(
            SceneObject::new(ObjectId(0), "R1", rect_shape(30.0, 20.0))
                .with_transform(Affine2D::translate(40.0, 60.0)),
            None,
        )
        .unwrap();
    let r2 = doc
        .add_object(
            SceneObject::new(ObjectId(0), "R2", rect_shape(30.0, 30.0))
                .with_transform(Affine2D::translate(90.0, 80.0)),
            None,
        )
        .unwrap();
    // Hidden object far away must NOT inflate page bounds
    let mut hidden_obj = SceneObject::new(ObjectId(0), "HiddenFar", rect_shape(50.0, 50.0))
        .with_transform(Affine2D::translate(400.0, 500.0));
    hidden_obj.visible = false;
    let hidden_id = doc.add_object(hidden_obj, None).unwrap();

    let (new_w, new_h) = doc.autofit_page_to_content(0, 5.0).unwrap();
    assert!((new_w - 90.0).abs() < 1e-6, "expected 90mm, got {new_w}");
    assert!((new_h - 60.0).abs() < 1e-6, "expected 60mm, got {new_h}");
    assert!((doc.pages()[0].width_mm - 90.0).abs() < 1e-6);
    assert!((doc.pages()[0].height_mm - 60.0).abs() < 1e-6);

    let b1 = doc.object_bounds(r1).unwrap();
    let b2 = doc.object_bounds(r2).unwrap();
    assert!((b1.min_x - 5.0).abs() < 1e-6);
    assert!((b1.min_y - 5.0).abs() < 1e-6);
    assert!((b2.max_x - 85.0).abs() < 1e-6);
    assert!((b2.max_y - 55.0).abs() < 1e-6);

    // Hidden object shifted by the exact same delta (-35, -55) so relative alignment stays intact
    let bh = doc.object_bounds(hidden_id).unwrap();
    assert!((bh.min_x - 365.0).abs() < 1e-6);
    assert!((bh.min_y - 445.0).abs() < 1e-6);

    // Works cleanly with negative object coordinates and NaN margin sanitization
    doc.set_object_transform(r1, Affine2D::translate(-25.0, -15.0))
        .unwrap();
    let (w2, h2) = doc.autofit_page_to_content(0, f64::NAN).unwrap();
    assert!(w2 >= 1.0 && h2 >= 1.0);
    let b1_after = doc.object_bounds(r1).unwrap();
    assert!(b1_after.min_x.abs() < 1e-6);
    assert!(b1_after.min_y.abs() < 1e-6);
}

#[test]
fn autofit_page_to_content_multipage_symbol_and_hidden_layer_isolation() {
    use coree_core::{MasterLayerScope, SymbolDef};

    let mut doc = Document::new_a4("Autofit MultiPage Symbol");
    let p0_id = doc.pages()[0].id;
    // Page 0: visible rect [30..50, 40..60] + far-away rect on hidden layer [500..540, 600..640]
    let p0_vis = doc
        .add_object(
            SceneObject::new(ObjectId(0), "P0Vis", rect_shape(20.0, 20.0))
                .with_transform(Affine2D::translate(30.0, 40.0)),
            None,
        )
        .unwrap();
    let hidden_lid = doc
        .add_layer(p0_id, "HiddenLayer", false, MasterLayerScope::None)
        .unwrap();
    doc.set_layer_flags(hidden_lid, Some(false), None, None, None)
        .unwrap();
    let p0_hidden = doc
        .add_object(
            SceneObject::new(ObjectId(0), "P0Hidden", rect_shape(40.0, 40.0))
                .with_transform(Affine2D::translate(500.0, 600.0)),
            Some(hidden_lid),
        )
        .unwrap();

    // Page 1: SymbolInstance (20x10 rect offset inside SymbolDef) + Group
    doc.add_page("Page 2", 210.0, 297.0, 0.0);
    let p1_lid = doc.pages()[1].layers[0].id;
    let sym_sub = SceneObject::new(ObjectId(1), "SymSub", rect_shape(20.0, 10.0))
        .with_transform(Affine2D::translate(10.0, 5.0));
    let sym_def = SymbolDef::new("BoxSym", vec![sym_sub]);
    let sym_id = sym_def.id;
    doc.upsert_symbol(sym_def);
    let inst_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(50.0, 80.0)),
            Some(p1_lid),
        )
        .unwrap();
    let g_a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "GA", rect_shape(15.0, 15.0))
                .with_transform(Affine2D::translate(95.0, 100.0)),
            Some(p1_lid),
        )
        .unwrap();
    let grp_id = doc.group_objects(&[g_a], "P1Grp").unwrap();

    // Autofit Page 1 with 10mm margin: sym world [60..80, 85..95] U grp [95..110, 100..115] -> 50x30 -> page 70x50
    let (w1, h1) = doc.autofit_page_to_content(1, 10.0).unwrap();
    assert!((w1 - 70.0).abs() < 1e-6, "expected w1=70, got {w1}");
    assert!((h1 - 50.0).abs() < 1e-6, "expected h1=50, got {h1}");
    let inst_b = doc.world_object_bounds(inst_id).unwrap();
    assert!((inst_b.min_x - 10.0).abs() < 1e-6);
    assert!((inst_b.min_y - 10.0).abs() < 1e-6);
    let grp_b = doc.world_object_bounds(grp_id).unwrap();
    assert!((grp_b.max_x - 60.0).abs() < 1e-6);
    assert!((grp_b.max_y - 40.0).abs() < 1e-6);

    // Page 0 must remain completely untouched (210x297, p0_vis still at 30,40)
    assert!((doc.pages()[0].width_mm - 210.0).abs() < 1e-6);
    assert!((doc.pages()[0].height_mm - 297.0).abs() < 1e-6);
    let p0_b_before = doc.world_object_bounds(p0_vis).unwrap();
    assert!((p0_b_before.min_x - 30.0).abs() < 1e-6);

    // Autofit Page 0 with 5mm margin: ignores hidden layer object [500..540, 600..640], yields 30x30
    let (w0, h0) = doc.autofit_page_to_content(0, 5.0).unwrap();
    assert!((w0 - 30.0).abs() < 1e-6, "expected w0=30, got {w0}");
    assert!((h0 - 30.0).abs() < 1e-6, "expected h0=30, got {h0}");
    let p0_b_after = doc.world_object_bounds(p0_vis).unwrap();
    assert!((p0_b_after.min_x - 5.0).abs() < 1e-6);
    assert!((p0_b_after.min_y - 5.0).abs() < 1e-6);
    // Hidden layer object shifted by (-25, -35) to preserve alignment
    let hidden_b = doc.world_object_bounds(p0_hidden).unwrap();
    assert!((hidden_b.min_x - 475.0).abs() < 1e-6);
    assert!((hidden_b.min_y - 565.0).abs() < 1e-6);
}

#[test]
fn icc_profiles_gamut_and_postscript_presets_end_to_end() {
    // Display P3 <-> sRGB roundtrip through D50 PCS
    let sample = [0.42_f32, 0.58, 0.71];
    let p3 = convert_rgb_between_builtin_profiles(
        sample,
        BuiltinIccProfile::Srgb,
        BuiltinIccProfile::DisplayP3,
    );
    let back = convert_rgb_between_builtin_profiles(
        p3,
        BuiltinIccProfile::DisplayP3,
        BuiltinIccProfile::Srgb,
    );
    for ch in 0..3 {
        assert!(
            (sample[ch] - back[ch]).abs() < 1e-3,
            "sRGB->P3->sRGB drift on ch {ch}: {} vs {}",
            sample[ch],
            back[ch]
        );
    }

    // All 4 rendering intents execute cleanly without NaNs on both CMYK profiles
    for prof in [
        BuiltinIccProfile::IsoCoatedV2Cmyk,
        BuiltinIccProfile::UsWebCoatedSwopCmyk,
    ] {
        for intent in [
            RenderingIntent::Perceptual,
            RenderingIntent::RelativeColorimetric,
            RenderingIntent::Saturation,
            RenderingIntent::AbsoluteColorimetric,
        ] {
            let cmyk = convert_rgb_to_cmyk_icc([0.8, 0.2, 0.5], prof, intent);
            assert!(cmyk
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
            let proof = soft_proof_srgb_through_cmyk([0.8, 0.2, 0.5], prof, intent);
            assert!(proof
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        }
        // Built-in profile metadata checks
        let built = IccProfile::builtin(prof);
        assert_eq!(built.header.color_space, IccColorSpace::Cmyk);
        assert!(built.white_point.is_some());
    }

    // Parametric curve types 0..=4 forward + inverse roundtrip
    let curves = [
        IccToneCurve::Parametric {
            func_type: 0,
            params: [2.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
        IccToneCurve::Parametric {
            func_type: 1,
            params: [2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        },
        IccToneCurve::Parametric {
            func_type: 2,
            params: [2.0, 0.95, 0.05, 0.0, 0.0, 0.0, 0.0],
        },
        IccToneCurve::srgb(),
        IccToneCurve::Parametric {
            func_type: 4,
            params: [
                2.4,
                1.0 / 1.055,
                0.055 / 1.055,
                1.0 / 12.92,
                0.04045,
                0.0,
                0.0,
            ],
        },
    ];
    for c in &curves {
        let y = c.eval(0.5);
        let x = c.eval_inverse(y);
        assert!((x - 0.5).abs() < 1e-4, "curve inverse mismatch: {x}");
    }

    // All 10 PostScript presets roundtrip cleanly with hyphen & case tolerance
    assert_eq!(PostScriptFillPreset::ALL.len(), 10);
    assert_eq!(
        PostScriptFillPreset::from_preset_str("PS:cross-hatch"),
        Some(PostScriptFillPreset::CrossHatch)
    );
    assert_eq!(
        PostScriptFillPreset::from_preset_str("ps:halftone-dots"),
        Some(PostScriptFillPreset::HalftoneDots)
    );
}

#[test]
fn regression_nested_powerclip_world_transforms_locked_layer_hit_test_and_zero_impact() {
    let mut doc = Document::new_a4("Audit Core Fixes");

    // 1. Nested PowerClip inside a translated Group must preserve exact child world coordinates!
    let container = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Container", rect_shape(40.0, 40.0))
                .with_transform(Affine2D::translate(10.0, 5.0)),
            None,
        )
        .unwrap();
    let grp = doc.group_objects(&[container], "ParentGroup").unwrap();
    doc.set_object_transform(grp, Affine2D::translate(50.0, 20.0))
        .unwrap();

    // Top-level child at world (65, 30)
    let child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Child", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(65.0, 30.0)),
            None,
        )
        .unwrap();

    doc.create_powerclip(container, &[child]).unwrap();
    // Composed world transform of child inside Group(50,20) -> Container(10,5) -> Child must remain (65,30)!
    let child_world = doc.world_transform_of(child).unwrap();
    let origin_world = child_world.transform_point(Point2D::ZERO);
    assert!(
        (origin_world.x - 65.0).abs() < 1e-6 && (origin_world.y - 30.0).abs() < 1e-6,
        "Nested PowerClip drifted child world position to {origin_world:?}"
    );

    // 2. world_path_of must include ancestor Group transform!
    let container_world_bb = doc
        .world_path_of(container)
        .unwrap()
        .bounding_box()
        .unwrap();
    assert!((container_world_bb.min_x - 60.0).abs() < 1e-6);
    assert!((container_world_bb.min_y - 25.0).abs() < 1e-6);

    // 3. Document::hit_test must skip objects on locked layers
    let page_id = doc.pages()[0].id;
    let locked_lid = doc
        .add_layer(page_id, "LockedLayer", false, MasterLayerScope::None)
        .unwrap();
    let on_locked = doc
        .add_object(
            SceneObject::new(ObjectId(0), "OnLocked", rect_shape(20.0, 20.0))
                .with_transform(Affine2D::translate(150.0, 150.0)),
            Some(locked_lid),
        )
        .unwrap();
    assert_eq!(
        doc.hit_test(Point2D::new(160.0, 160.0), 1.0),
        Some(on_locked)
    );
    doc.set_layer_flags(locked_lid, None, Some(true), None, None)
        .unwrap();
    assert_eq!(doc.hit_test(Point2D::new(160.0, 160.0), 1.0), None);

    // 4. Zero-size ParametricShape::Impact returns empty path safely
    let zero_impact = ParametricShape::Impact {
        rays: 12,
        inner_radius: 0.0,
        outer_radius: 0.0,
        style: ImpactStyle::Radial,
    }
    .to_bezier_path();
    assert!(zero_impact.is_empty());
}

#[test]
fn convert_outline_to_object_and_scale_with_object_work_transactional() {
    let mut doc = Document::new_a4("Outline Expand Test");
    let lid = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::default();

    // 1. Filled rectangle with thick 4mm scale_with_object outline scaled 2x -> 8mm closed contour placed above fill
    let red = Color::rgba8(220, 40, 40, 255);
    let rect_obj = SceneObject::new(ObjectId(0), "Badge", rect_shape(20.0, 10.0))
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle {
            scale_with_object: true,
            ..OutlineStyle::solid(red.clone(), 4.0)
        })
        .with_transform(Affine2D::scale(2.0, 2.0));
    let badge_id = doc.add_object(rect_obj, Some(lid)).unwrap();
    let scaled_w = doc
        .object(badge_id)
        .unwrap()
        .outline
        .scaled_width_mm_for_transform(&doc.object(badge_id).unwrap().transform);
    assert!((scaled_w - 8.0).abs() < 1e-6);

    hist.execute(&mut doc, CommandOp::ConvertOutlineToObject { id: badge_id })
        .unwrap();
    assert_eq!(doc.objects().len(), 2);
    assert!(!doc.object(badge_id).unwrap().outline.is_visible());
    let new_id = doc.pages()[0].layers[0].objects[1];
    assert_eq!(doc.object(new_id).unwrap().name, "Badge (Outline)");
    assert_eq!(doc.object(new_id).unwrap().fill, Fill::Solid(red));
    assert!(!doc.object(new_id).unwrap().outline.is_visible());
    // 20x10 scaled 2x -> 40x20 + 8mm stroke -> 48x28 bounds
    let new_bb = doc.object_bounds(new_id).unwrap();
    assert!((new_bb.width() - 48.0).abs() < 0.25);
    assert!((new_bb.height() - 28.0).abs() < 0.25);

    hist.undo(&mut doc).unwrap();
    assert_eq!(doc.objects().len(), 1);
    assert!(doc.object(badge_id).unwrap().outline.is_visible());

    // 2. Open unfilled line converted in-place cleanly without spawning extra objects
    let line_obj = SceneObject::new(
        ObjectId(0),
        "OpenCut",
        NodePayload::Path(BezierPath::from_line(
            Point2D::ZERO,
            Point2D::new(30.0, 0.0),
        )),
    )
    .with_outline(OutlineStyle::solid(Color::BLACK, 2.0));
    let line_id = doc.add_object(line_obj, Some(lid)).unwrap();
    hist.execute(&mut doc, CommandOp::ConvertOutlineToObject { id: line_id })
        .unwrap();
    assert_eq!(doc.object(line_id).unwrap().fill, Fill::Solid(Color::BLACK));
    assert!(!doc.object(line_id).unwrap().outline.is_visible());
    let line_bb = doc.object_bounds(line_id).unwrap();
    assert!((line_bb.height() - 2.0).abs() < 0.15);
}

#[test]
fn w1b_all_25_new_command_ops_lossless_execute_undo_redo() {
    use coree_core::find_replace::{
        ColorMatch, FindQuery, FindScope, ReplaceAction, TextMatchMode,
    };

    let mut doc = Document::new_a4("Initial Title");
    let mut hist = CommandHistory::default();

    let p1_id = doc.pages()[0].id;
    let l1_id = doc.pages()[0].layers[0].id;
    let p2_id = doc.add_page("Page 2", 100.0, 150.0, 2.0);
    let _ = p2_id;

    // Seed two objects on Page 1 Layer 1 plus a PowerClip container
    let r1 = SceneObject::new(ObjectId(0), "RectA", rect_shape(20.0, 10.0))
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::solid(Color::BLACK, 0.5))
        .with_transform(Affine2D::translate(10.0, 10.0));
    let id1 = doc.add_object(r1, Some(l1_id)).unwrap();

    let r2 = SceneObject::new(
        ObjectId(0),
        "HelloText",
        NodePayload::ArtisticText(ArtisticText::from_plain(
            "Hello World",
            TextStyle::default(),
        )),
    )
    .with_fill(Fill::Solid(Color::MAGENTA))
    .with_transform(Affine2D::translate(50.0, 50.0));
    let id2 = doc.add_object(r2, Some(l1_id)).unwrap();

    let pc_frame = SceneObject::new(ObjectId(0), "PCFrame", rect_shape(40.0, 40.0))
        .with_transform(Affine2D::translate(100.0, 100.0));
    let pc_id = doc.add_object(pc_frame, Some(l1_id)).unwrap();
    let pc_child = SceneObject::new(ObjectId(0), "PCChild", rect_shape(10.0, 10.0))
        .with_transform(Affine2D::translate(5.0, 5.0));
    let pc_child_id = doc.add_object(pc_child, Some(l1_id)).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::CreatePowerClip {
            container_id: pc_id,
            content_ids: vec![pc_child_id],
        },
    )
    .unwrap();

    let assert_reversible =
        |doc: &mut Document, hist: &mut CommandHistory, op: CommandOp, label: &str| {
            let before = doc.clone();
            hist.execute(doc, op)
                .unwrap_or_else(|e| panic!("{label} execute failed: {e}"));
            let after = doc.clone();
            assert_ne!(&after, &before, "{label} should mutate document");
            hist.undo(doc)
                .unwrap_or_else(|e| panic!("{label} undo failed: {e}"));
            assert_eq!(doc, &before, "{label} undo must be 100% lossless");
            hist.redo(doc)
                .unwrap_or_else(|e| panic!("{label} redo failed: {e}"));
            assert_eq!(
                doc, &after,
                "{label} redo must reproduce exact post-exec state"
            );
        };

    // 1. DuplicateObjects
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::DuplicateObjects {
            ids: vec![id1],
            offset: Vec2D::new(5.0, 5.0),
        },
        "DuplicateObjects",
    );

    // 2. SetObjectVisibility
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetObjectVisibility {
            ids: vec![id1],
            visible: false,
        },
        "SetObjectVisibility",
    );

    // 3. SetObjectLocked (lock then unlock so subsequent edits on id1 succeed)
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetObjectLocked {
            ids: vec![id1],
            locked: true,
        },
        "SetObjectLocked(true)",
    );
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetObjectLocked {
            ids: vec![id1],
            locked: false,
        },
        "SetObjectLocked(false)",
    );

    // 4. SetObjectPrintable
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetObjectPrintable {
            ids: vec![id1],
            printable: false,
        },
        "SetObjectPrintable",
    );

    // 5. RenameObject
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::RenameObject {
            id: id1,
            new_name: "RenamedRectA".to_owned(),
        },
        "RenameObject",
    );

    // 6. SetOpacity
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetOpacity {
            ids: vec![id1],
            opacity: 0.4,
        },
        "SetOpacity",
    );

    // 7. SetBlendMode
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetBlendMode {
            ids: vec![id1],
            blend_mode: BlendMode::Multiply,
        },
        "SetBlendMode",
    );

    // 8. SetDocumentProperties (all mutable document metadata & custom ICC fields)
    let mut meta = doc.metadata().clone();
    meta.author = "Verifier".to_owned();
    meta.subject = "Parity".to_owned();
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetDocumentProperties {
            title: Some("Updated Title".to_owned()),
            color_mode: Some(ColorMode::Rgb),
            default_unit: Some(Unit::Inch),
            dpi: Some(150.0),
            multipage_view: Some(true),
            active_page_index: Some(1),
            metadata: Some(meta),
            custom_icc_profile: Some(coree_core::icc::generate_minimal_srgb_icc_v2(
                "Test Custom Profile",
            )),
            clear_custom_icc: false,
            rgb_icc_profile: None,
            clear_rgb_icc: false,
            cmyk_icc_profile: None,
            clear_cmyk_icc: false,
            gray_icc_profile: None,
            clear_gray_icc: false,
            rendering_intent: None,
            black_point_compensation: None,
            drawing_scale: None,
        },
        "SetDocumentProperties(set)",
    );
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetDocumentProperties {
            title: None,
            color_mode: None,
            default_unit: None,
            dpi: None,
            multipage_view: None,
            active_page_index: None,
            metadata: None,
            custom_icc_profile: None,
            clear_custom_icc: true,
            rgb_icc_profile: None,
            clear_rgb_icc: false,
            cmyk_icc_profile: None,
            clear_cmyk_icc: false,
            gray_icc_profile: None,
            clear_gray_icc: false,
            rendering_intent: None,
            black_point_compensation: None,
            drawing_scale: None,
        },
        "SetDocumentProperties(clear_icc)",
    );

    // 9. SetPageProperties
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetPageProperties {
            page_id: p1_id,
            name: Some("Cover".to_owned()),
            width_mm: Some(180.0),
            height_mm: Some(260.0),
            bleed_mm: Some(5.0),
            facing_pages: Some(true),
            background_fill: None,
            printable_background: None,
        },
        "SetPageProperties",
    );

    // 10. ReorderPages
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::ReorderPages {
            page_ids: vec![p2_id, p1_id],
        },
        "ReorderPages",
    );

    // 11. RenameLayer
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::RenameLayer {
            layer_id: l1_id,
            new_name: "Artwork".to_owned(),
        },
        "RenameLayer",
    );

    // 12. ReorderLayerObjects
    let mut rev_objs = doc.find_layer(l1_id).unwrap().objects.clone();
    rev_objs.reverse();
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::ReorderLayerObjects {
            layer_id: l1_id,
            object_ids: rev_objs,
        },
        "ReorderLayerObjects",
    );

    // 13. SetGridConfig
    let mut grid = doc.grid();
    grid.visible = true;
    grid.spacing_x_mm = 12.5;
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetGridConfig { config: grid },
        "SetGridConfig",
    );

    // 14. SetPerspectiveGrid
    let mut pgrid = doc.perspective_grid().clone();
    pgrid.enabled = true;
    pgrid.density = 16;
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetPerspectiveGrid { config: pgrid },
        "SetPerspectiveGrid",
    );

    // 15 & 16. UpsertColorStyle + DeleteColorStyle
    let cs = ColorStyle::new("BrandBlue", Color::CYAN, Some(ColorHarmony::Complementary));
    let cs_id = cs.id;
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::UpsertColorStyle { style: cs },
        "UpsertColorStyle",
    );
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::DeleteColorStyle { id: cs_id },
        "DeleteColorStyle",
    );

    // 17, 18, 19. UpsertObjectStyle + SetObjectStyleLink + DeleteObjectStyle (verifying style_id cleanup)
    let mut os = ObjectStyle::new("HeroStyle");
    os.opacity = Some(0.8);
    let os_id = os.id;
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::UpsertObjectStyle { style: os },
        "UpsertObjectStyle",
    );
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::SetObjectStyleLink {
            ids: vec![id1],
            style_id: Some(os_id),
        },
        "SetObjectStyleLink",
    );
    assert_eq!(doc.object(id1).unwrap().style_id, Some(os_id));
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::DeleteObjectStyle { id: os_id },
        "DeleteObjectStyle",
    );
    assert_eq!(
        doc.object(id1).unwrap().style_id,
        None,
        "DeleteObjectStyle must clear referencing object style_id"
    );

    // 20, 21, 22, 23. UpsertSymbol + InstantiateSymbol + UnlinkSymbolInstance + DeleteSymbol
    let sym = SymbolDef::new(
        "BadgeSym",
        vec![SceneObject::new(
            ObjectId(0),
            "SymBox",
            rect_shape(12.0, 12.0),
        )],
    );
    let sym_id = sym.id;
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::UpsertSymbol { symbol: sym },
        "UpsertSymbol",
    );
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::InstantiateSymbol {
            symbol_id: sym_id,
            layer_id: Some(l1_id),
            transform: Affine2D::translate(20.0, 20.0),
            name: Some("BadgeInst1".to_owned()),
        },
        "InstantiateSymbol(1)",
    );
    let inst1_id = doc
        .objects()
        .values()
        .find(|o| o.name == "BadgeInst1")
        .unwrap()
        .id;
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::UnlinkSymbolInstance { id: inst1_id },
        "UnlinkSymbolInstance",
    );
    assert!(matches!(
        doc.object(inst1_id).unwrap().payload,
        NodePayload::Path(_) | NodePayload::Shape(_) | NodePayload::Group(_)
    ));

    // Create a second instance + VectorPattern referencing sym_id, then DeleteSymbol must clean both!
    hist.execute(
        &mut doc,
        CommandOp::InstantiateSymbol {
            symbol_id: sym_id,
            layer_id: Some(l1_id),
            transform: Affine2D::translate(30.0, 30.0),
            name: Some("BadgeInst2".to_owned()),
        },
    )
    .unwrap();
    let inst2_id = doc
        .objects()
        .values()
        .find(|o| o.name == "BadgeInst2")
        .unwrap()
        .id;
    hist.execute(
        &mut doc,
        CommandOp::SetFill {
            ids: vec![id1],
            fill: Fill::VectorPattern {
                symbol_id: sym_id,
                tile_w: 10.0,
                tile_h: 10.0,
            },
        },
    )
    .unwrap();
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::DeleteSymbol { id: sym_id },
        "DeleteSymbol",
    );
    assert!(!matches!(
        doc.object(inst2_id).unwrap().payload,
        NodePayload::SymbolInstance(_)
    ));
    assert_eq!(doc.object(id1).unwrap().fill, Fill::None);

    // 24. FitPowerClip (all 3 modes)
    for mode in [
        PowerClipFitMode::FitContents,
        PowerClipFitMode::CenterContents,
        PowerClipFitMode::StretchToFill,
    ] {
        assert_reversible(
            &mut doc,
            &mut hist,
            CommandOp::FitPowerClip { id: pc_id, mode },
            "FitPowerClip",
        );
    }

    // 25. FindAndReplace
    let q = FindQuery {
        scope: FindScope::WholeDocument,
        text_pattern: Some((
            "Hello".to_owned(),
            TextMatchMode::Contains {
                case_sensitive: true,
            },
        )),
        ..FindQuery::default()
    };
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::FindAndReplace {
            query: q,
            action: ReplaceAction::ReplaceText {
                find: "Hello".to_owned(),
                replace_with: "Welcome".to_owned(),
                case_sensitive: true,
            },
        },
        "FindAndReplace(Text)",
    );
    let q_color = FindQuery {
        scope: FindScope::WholeDocument,
        fill_color: Some(ColorMatch::exact(Color::MAGENTA)),
        ..FindQuery::default()
    };
    assert_reversible(
        &mut doc,
        &mut hist,
        CommandOp::FindAndReplace {
            query: q_color,
            action: ReplaceAction::ReplaceFillColor {
                color: Color::YELLOW,
            },
        },
        "FindAndReplace(FillColor)",
    );
    assert_eq!(doc.object(id2).unwrap().fill, Fill::Solid(Color::YELLOW));
}

#[test]
fn w1b_edit_path_guard_rejects_all_non_path_payloads_and_boolean_preserves_metadata() {
    let mut doc = Document::new_a4("Guard & Boolean Test");
    let lid = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::default();

    let asset = RasterAsset::new(2, 2, vec![0u8; 16], 150.0, ColorMode::Rgb).unwrap();
    let aid = doc.register_asset(asset);
    let sym = SymbolDef::new(
        "Sym",
        vec![SceneObject::new(ObjectId(0), "S", rect_shape(5.0, 5.0))],
    );
    let sid = sym.id;
    hist.execute(&mut doc, CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();

    let shape_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Shape", rect_shape(20.0, 20.0)),
            Some(lid),
        )
        .unwrap();
    let text_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Art",
                NodePayload::ArtisticText(ArtisticText::from_plain("A", TextStyle::default())),
            ),
            Some(lid),
        )
        .unwrap();
    let para_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Para",
                NodePayload::ParagraphText(ParagraphText::new(
                    "B",
                    Vec2D::new(30.0, 20.0),
                    TextStyle::default(),
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let host_path_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "HostPath",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::ZERO,
                    Point2D::new(40.0, 0.0),
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let top_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "TOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("C", TextStyle::default()),
                    path_id: host_path_id,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(lid),
        )
        .unwrap();
    let table_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Tbl",
                NodePayload::Table(VectorTable::new(2, 2, 10.0, 10.0)),
            ),
            Some(lid),
        )
        .unwrap();
    let dim_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Dim",
                NodePayload::Dimension(DimensionObject::linear(
                    DimensionKind::Horizontal,
                    Point2D::ZERO,
                    Point2D::new(10.0, 0.0),
                    5.0,
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let conn_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Conn",
                NodePayload::Connector(ConnectorObject::between_objects(
                    shape_id,
                    AnchorPoint::Right,
                    text_id,
                    AnchorPoint::Left,
                    ConnectorStyle::Straight,
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let raster_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Raster",
                NodePayload::Raster(RasterLayerData {
                    asset_id: aid,
                    width_mm: 10.0,
                    height_mm: 10.0,
                    filters: Vec::new(),
                    filter_enabled: Vec::new(),
                    crop_rect: None,
                }),
            ),
            Some(lid),
        )
        .unwrap();
    let sym_inst_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid)),
            Some(lid),
        )
        .unwrap();

    let g_c1 = doc
        .add_object(
            SceneObject::new(ObjectId(0), "GC1", rect_shape(5.0, 5.0)),
            Some(lid),
        )
        .unwrap();
    let g_c2 = doc
        .add_object(
            SceneObject::new(ObjectId(0), "GC2", rect_shape(5.0, 5.0)),
            Some(lid),
        )
        .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![g_c1, g_c2],
            name: "Grp".to_owned(),
        },
    )
    .unwrap();
    let grp_id = doc.objects().values().find(|o| o.name == "Grp").unwrap().id;

    let pc_child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "PCC", rect_shape(5.0, 5.0)),
            Some(lid),
        )
        .unwrap();
    let pc_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "PC", rect_shape(20.0, 20.0)),
            Some(lid),
        )
        .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::CreatePowerClip {
            container_id: pc_id,
            content_ids: vec![pc_child],
        },
    )
    .unwrap();

    let dummy_path = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 5.0, 5.0));
    let before_doc = doc.clone();
    for &non_path_id in &[
        shape_id,
        text_id,
        para_id,
        top_id,
        table_id,
        dim_id,
        conn_id,
        raster_id,
        sym_inst_id,
        grp_id,
        pc_id,
    ] {
        let err = hist
            .execute(
                &mut doc,
                CommandOp::EditPath {
                    id: non_path_id,
                    path: dummy_path.clone(),
                },
            )
            .expect_err("EditPath must reject non-Path payload");
        assert!(matches!(err, CoreError::InvalidParameter(_)));
        assert_eq!(
            doc, before_doc,
            "failed EditPath must leave document untouched"
        );
    }

    // PowerClip BooleanOp: destructive Weld rejected, CreateBoundary succeeds
    assert!(hist
        .execute(
            &mut doc,
            CommandOp::BooleanOp {
                op: BooleanOp::Weld,
                ids: vec![pc_id, shape_id],
            }
        )
        .is_err());
    assert!(hist
        .execute(
            &mut doc,
            CommandOp::BooleanOp {
                op: BooleanOp::CreateBoundary,
                ids: vec![pc_id],
            }
        )
        .is_ok());

    // BooleanOp metadata & z-order preservation + FrontMinusBack + BreakApart
    let sty = ObjectStyle::new("BoolStyle");
    let sty_id = sty.id;
    hist.execute(&mut doc, CommandOp::UpsertObjectStyle { style: sty })
        .unwrap();

    let mut b1 = SceneObject::new(ObjectId(0), "PrimaryMeta", rect_shape(30.0, 30.0))
        .with_fill(Fill::Solid(Color::CYAN))
        .with_transform(Affine2D::translate(10.0, 10.0));
    b1.visible = false;
    b1.printable = false;
    b1.style_id = Some(sty_id);
    b1.effects.push(Effect::DropShadow {
        dx_mm: 2.0,
        dy_mm: 2.0,
        blur_mm: 3.0,
        opacity: 0.5,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Middle,
    });
    b1.lens = Some(LensConfig {
        kind: LensKind::Brighten { rate_pct: 20.0 },
        frozen: false,
        viewpoint: Some(Point2D::ZERO),
        remove_face: false,
        frozen_snapshot: None,
    });
    b1.unknown_chunks.push(UnknownCdrChunk {
        fourcc: *b"META",
        version_hint: 1,
        raw_bytes: vec![9, 8, 7],
    });
    let b1_id = doc.add_object(b1, Some(lid)).unwrap();
    let b2_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "FrontExtend", rect_shape(30.0, 20.0))
                .with_fill(Fill::Solid(Color::MAGENTA))
                .with_transform(Affine2D::translate(25.0, 15.0)),
            Some(lid),
        )
        .unwrap();
    let top_marker = doc
        .add_object(
            SceneObject::new(ObjectId(0), "TopMarker", rect_shape(5.0, 5.0)),
            Some(lid),
        )
        .unwrap();
    let pos_b1 = doc
        .find_layer(lid)
        .unwrap()
        .objects
        .iter()
        .position(|&x| x == b1_id)
        .unwrap();

    // Weld preserves primary source (b1) metadata & z-order slot below TopMarker
    hist.execute(
        &mut doc,
        CommandOp::BooleanOp {
            op: BooleanOp::Weld,
            ids: vec![b1_id, b2_id],
        },
    )
    .unwrap();
    let layer_objs = &doc.find_layer(lid).unwrap().objects;
    let res_id = layer_objs[pos_b1];
    assert_ne!(
        res_id, top_marker,
        "result must stay below TopMarker at source position"
    );
    let res = doc.object(res_id).unwrap();
    assert_eq!(res.name, "PrimaryMeta");
    assert!(!res.visible && !res.printable);
    assert_eq!(res.style_id, Some(sty_id));
    assert_eq!(res.effects.len(), 1);
    assert!(res.lens.is_some());
    assert_eq!(res.unknown_chunks.len(), 1);
    hist.undo(&mut doc).unwrap();
    assert!(doc.has_object(b1_id) && doc.has_object(b2_id));

    // FrontMinusBack uses sources.last() ("FrontExtend") for result styling/naming
    hist.execute(
        &mut doc,
        CommandOp::BooleanOp {
            op: BooleanOp::FrontMinusBack,
            ids: vec![b1_id, b2_id],
        },
    )
    .unwrap();
    let fmb_id = doc.find_layer(lid).unwrap().objects[pos_b1];
    assert_eq!(doc.object(fmb_id).unwrap().name, "FrontExtend");
    assert_eq!(
        doc.object(fmb_id).unwrap().fill,
        Fill::Solid(Color::MAGENTA)
    );
    hist.undo(&mut doc).unwrap();
    assert!(doc.has_object(b1_id) && doc.has_object(b2_id));
}

#[test]
fn w1b_relational_cleanup_remove_delete_page_duplicate_and_table_assets() {
    let mut doc = Document::new_a4("Relational Cleanup");
    let p1_layer = doc.pages()[0].layers[0].id;
    let p2_id = doc.add_page("Page 2", 210.0, 297.0, 0.0);
    let p2_layer = doc.page_by_id(p2_id).unwrap().layers[0].id;

    // Asset inside a Table cell BitmapPattern
    let cell_asset = RasterAsset::new(2, 2, vec![255u8; 16], 72.0, ColorMode::Rgb).unwrap();
    let cell_aid = doc.register_asset(cell_asset);
    let mut tbl = VectorTable::new(1, 1, 20.0, 10.0);
    tbl.cells[0].fill = Fill::BitmapPattern {
        image_id: cell_aid,
        tile_w: 5.0,
        tile_h: 5.0,
    };
    let _tbl_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "CellAssetTbl", NodePayload::Table(tbl)),
            Some(p1_layer),
        )
        .unwrap();
    assert!(doc.referenced_asset_ids().contains(&cell_aid));

    // Host curve & two boxes on Page 1; Linked ParagraphText chain A -> B -> C; Connector; Blend; TextOnPath
    let host_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "HostCurve",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 0.0),
                    Point2D::new(40.0, 0.0),
                )),
            )
            .with_transform(Affine2D::translate(15.0, 25.0)),
            Some(p1_layer),
        )
        .unwrap();
    let top_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "CurvedText",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("OnPath", TextStyle::default()),
                    path_id: host_id,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(p1_layer),
        )
        .unwrap();
    let box_a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "BoxA", rect_shape(20.0, 20.0))
                .with_transform(Affine2D::translate(0.0, 0.0)),
            Some(p1_layer),
        )
        .unwrap();
    let box_b = doc
        .add_object(
            SceneObject::new(ObjectId(0), "BoxB", rect_shape(20.0, 20.0))
                .with_transform(Affine2D::translate(60.0, 0.0)),
            Some(p2_layer),
        )
        .unwrap();
    let conn_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Conn",
                NodePayload::Connector(ConnectorObject::between_objects(
                    box_a,
                    AnchorPoint::Right,
                    box_b,
                    AnchorPoint::Left,
                    ConnectorStyle::Straight,
                )),
            ),
            Some(p1_layer),
        )
        .unwrap();

    // Linked paragraph frames A -> B -> C where B is on Page 2
    let f_c = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "FrameC",
                NodePayload::ParagraphText(ParagraphText::new(
                    "C",
                    Vec2D::new(30.0, 20.0),
                    TextStyle::default(),
                )),
            ),
            Some(p1_layer),
        )
        .unwrap();
    let mut pb = ParagraphText::new("B", Vec2D::new(30.0, 20.0), TextStyle::default());
    pb.linked_next_frame = Some(f_c);
    let f_b = doc
        .add_object(
            SceneObject::new(ObjectId(0), "FrameB", NodePayload::ParagraphText(pb)),
            Some(p2_layer),
        )
        .unwrap();
    let mut pa = ParagraphText::new("A", Vec2D::new(30.0, 20.0), TextStyle::default());
    pa.linked_next_frame = Some(f_b);
    let f_a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "FrameA", NodePayload::ParagraphText(pa)),
            Some(p1_layer),
        )
        .unwrap();

    // Remove host_id -> TextOnPath converts to ArtisticText preserving host anchor!
    doc.remove_object(host_id).unwrap();
    let top_after = doc.object(top_id).unwrap();
    assert!(matches!(top_after.payload, NodePayload::ArtisticText(_)));
    assert!((top_after.transform.m[4] - 15.0).abs() < 1e-6);
    assert!((top_after.transform.m[5] - 25.0).abs() < 1e-6);

    // Delete Page 2 -> removes box_b & f_b: Connector freezes to_fallback and clears to_obj; FrameA splices to FrameC!
    let mut hist = CommandHistory::default();
    hist.execute(&mut doc, CommandOp::DeletePage { page_id: p2_id })
        .unwrap();
    if let NodePayload::Connector(ref c) = doc.object(conn_id).unwrap().payload {
        assert_eq!(c.from_obj, Some(box_a));
        assert_eq!(c.to_obj, None);
        assert!((c.to_fallback.x - 60.0).abs() < 1e-4);
    } else {
        panic!("expected Connector");
    }
    if let NodePayload::ParagraphText(ref p) = doc.object(f_a).unwrap().payload {
        assert_eq!(
            p.linked_next_frame,
            Some(f_c),
            "FrameA must splice A -> B -> C cleanly to A -> C"
        );
    } else {
        panic!("expected ParagraphText");
    }

    // Duplicate FrameA alone (without FrameC) -> duplicate's linked_next_frame cleared to None!
    let dups = doc
        .duplicate_objects(&[f_a], Vec2D::new(10.0, 10.0))
        .unwrap();
    assert_eq!(dups.len(), 1);
    if let NodePayload::ParagraphText(ref p) = doc.object(dups[0]).unwrap().payload {
        assert_eq!(p.linked_next_frame, None);
    } else {
        panic!("expected ParagraphText duplicate");
    }
}

#[test]
fn w1b_world_bounds_symbol_cycles_align_and_distribute() {
    let mut doc = Document::new_a4("Bounds & Align");
    let lid = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::default();

    // Symbol containing a 20x10 rectangle
    let sym = SymbolDef::new(
        "Sym20x10",
        vec![SceneObject::new(
            ObjectId(0),
            "SymR",
            rect_shape(20.0, 10.0),
        )],
    );
    let sid = sym.id;
    hist.execute(&mut doc, CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();

    // Cyclic symbol def must not stack-overflow in object_bounds
    let mut cyc_sym = SymbolDef::new("Cyclic", Vec::new());
    let cyc_id = cyc_sym.id;
    cyc_sym.objects.push(SceneObject::new(
        ObjectId(0),
        "SelfRef",
        NodePayload::SymbolInstance(cyc_id),
    ));
    hist.execute(&mut doc, CommandOp::UpsertSymbol { symbol: cyc_sym })
        .unwrap();
    let cyc_inst = doc
        .add_object(
            SceneObject::new(ObjectId(0), "CycInst", NodePayload::SymbolInstance(cyc_id)),
            Some(lid),
        )
        .unwrap();
    assert!(doc.world_object_bounds(cyc_inst).is_none());

    // SymbolInstance inside translated Group
    let inst_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sid))
                .with_transform(Affine2D::scale(2.0, 1.0)),
            Some(lid),
        )
        .unwrap();
    let peer_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Peer", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(0.0, 20.0)),
            Some(lid),
        )
        .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![inst_id, peer_id],
            name: "SymGroup".to_owned(),
        },
    )
    .unwrap();
    let grp_id = doc
        .objects()
        .values()
        .find(|o| o.name == "SymGroup")
        .unwrap()
        .id;
    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![grp_id],
            affine: Affine2D::translate(50.0, 30.0),
        },
    )
    .unwrap();

    let inst_bb = doc.world_object_bounds(inst_id).unwrap();
    assert!((inst_bb.min_x - 50.0).abs() < 1e-6);
    assert!((inst_bb.width() - 40.0).abs() < 1e-6);
    assert!((inst_bb.height() - 10.0).abs() < 1e-6);

    // Passing BOTH grp_id and child peer_id to align_objects must NOT double-translate peer_id!
    let other_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Other", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(10.0, 0.0)),
            Some(lid),
        )
        .unwrap();
    let peer_before = doc.world_object_bounds(peer_id).unwrap();
    doc.align_objects(&[other_id, grp_id, peer_id], AlignOp::Left)
        .unwrap();
    let grp_after = doc.world_object_bounds(grp_id).unwrap();
    let peer_after = doc.world_object_bounds(peer_id).unwrap();
    assert!((grp_after.min_x - 10.0).abs() < 1e-6);
    assert!(
        (peer_after.min_x - 10.0).abs() < 1e-6,
        "peer inside group must stay aligned at 10.0 (not double-shifted!)"
    );
    assert!((peer_after.min_y - peer_before.min_y).abs() < 1e-6);

    // Distribute spacing across 3 items including nested group child
    let box3 = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Box3", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(100.0, 0.0)),
            Some(lid),
        )
        .unwrap();
    doc.distribute_objects(&[other_id, grp_id, box3], DistributeOp::SpacingH)
        .unwrap();
    let mid_bb = doc.world_object_bounds(grp_id).unwrap();
    assert!(mid_bb.min_x > 10.0 && mid_bb.max_x < 110.0);
}

#[test]
fn w1b_r2_paragraph_cycle_duplicate_cycles_topmost_and_symbol_group_bounds() {
    let mut doc = Document::new_a4("W1B R2 Kernel Invariants");
    let lid = doc.pages()[0].layers[0].id;

    // 1. ParagraphText cycle A -> B -> A: deleting B must set A.linked_next_frame = None (never Some(A))
    let mut pa = ParagraphText::new("A", Vec2D::new(30.0, 20.0), TextStyle::default());
    let mut pb = ParagraphText::new("B", Vec2D::new(30.0, 20.0), TextStyle::default());
    let fa_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "FrameA",
                NodePayload::ParagraphText(pa.clone()),
            ),
            Some(lid),
        )
        .unwrap();
    pb.linked_next_frame = Some(fa_id);
    let fb_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "FrameB", NodePayload::ParagraphText(pb)),
            Some(lid),
        )
        .unwrap();
    pa.linked_next_frame = Some(fb_id);
    let mut setup_hist = CommandHistory::default();
    setup_hist
        .execute(
            &mut doc,
            CommandOp::EditText {
                id: fa_id,
                payload: NodePayload::ParagraphText(pa),
            },
        )
        .unwrap();

    doc.remove_object(fb_id).unwrap();
    if let NodePayload::ParagraphText(ref p) = doc.object(fa_id).unwrap().payload {
        assert_eq!(
            p.linked_next_frame, None,
            "deleting B from A->B->A must break cycle to None, never A->A"
        );
    } else {
        panic!("expected ParagraphText");
    }

    // 2. duplicate_objects on cyclic Group (G1 -> G2 -> G1) must fail with CycleDetected and roll back atomically
    let ok_rect = doc
        .add_object(
            SceneObject::new(ObjectId(0), "OkRect", rect_shape(10.0, 10.0)),
            Some(lid),
        )
        .unwrap();
    let g1 = doc.alloc_object_id();
    let g2 = doc.alloc_object_id();
    doc.add_object(
        SceneObject::new(g1, "CycG1", NodePayload::Group(vec![g2])),
        Some(lid),
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(g2, "CycG2", NodePayload::Group(vec![g1])),
        Some(lid),
    )
    .unwrap();

    let before_cyclic_dup = doc.clone();
    let err = doc
        .duplicate_objects(&[ok_rect, g1], Vec2D::new(5.0, 5.0))
        .expect_err("cyclic group duplication must return CycleDetected");
    assert!(matches!(err, CoreError::CycleDetected(_)));
    assert_eq!(
        doc, before_cyclic_dup,
        "failed duplicate_objects must roll back any partial clones"
    );

    let err_both = doc
        .duplicate_objects(&[g1, g2], Vec2D::new(5.0, 5.0))
        .expect_err("mutually cyclic pair must return CycleDetected");
    assert!(matches!(err_both, CoreError::CycleDetected(_)));
    assert_eq!(doc, before_cyclic_dup);

    // 3. duplicate_objects(&[child, grp]) must use filter_topmost_selection (single parent, single offset)
    let child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Child", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(5.0, 0.0)),
            Some(lid),
        )
        .unwrap();
    let grp = doc.group_objects(&[child], "ParentGrp").unwrap();
    doc.transform_object(grp, Affine2D::translate(20.0, 0.0))
        .unwrap();
    let layer_len_before = doc.find_layer(lid).unwrap().objects.len();
    let obj_len_before = doc.objects().len();

    let created = doc
        .duplicate_objects(&[child, grp], Vec2D::new(10.0, 0.0))
        .unwrap();
    assert_eq!(
        created.len(),
        1,
        "only topmost group should be created at root"
    );
    let dup_grp = created[0];
    assert_eq!(
        doc.find_layer(lid).unwrap().objects.len(),
        layer_len_before + 1
    );
    assert_eq!(doc.objects().len(), obj_len_before + 2);
    let dup_child = match &doc.object(dup_grp).unwrap().payload {
        NodePayload::Group(c) => {
            assert_eq!(c.len(), 1);
            c[0]
        }
        other => panic!("expected Group, got {other:?}"),
    };
    assert!(
        !doc.find_layer(lid).unwrap().objects.contains(&dup_child),
        "duplicated child must NOT also be added directly to the layer"
    );
    let dup_child_bb = doc.world_object_bounds(dup_child).unwrap();
    assert!(
        (dup_child_bb.min_x - 35.0).abs() < 1e-6 && (dup_child_bb.max_x - 45.0).abs() < 1e-6,
        "expected single offset to x=35..45, got {dup_child_bb:?}"
    );

    // 4. SymbolDef with nested transformed Group computes exact SymbolInstance bounds
    let sym_child_id = ObjectId(102);
    let sym_grp_id = ObjectId(101);
    let sym_nested_grp = SymbolDef::new(
        "SymWithGroup",
        vec![
            SceneObject::new(sym_grp_id, "SymGrp", NodePayload::Group(vec![sym_child_id]))
                .with_transform(Affine2D::translate(100.0, 0.0)),
            SceneObject::new(sym_child_id, "SymChild", rect_shape(10.0, 10.0)),
        ],
    );
    let sg_id = sym_nested_grp.id;
    doc.upsert_symbol(sym_nested_grp);
    let sg_inst = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "SymGrpInst",
                NodePayload::SymbolInstance(sg_id),
            ),
            Some(lid),
        )
        .unwrap();
    let sg_bb = doc.world_object_bounds(sg_inst).unwrap();
    assert!(
        (sg_bb.min_x - 100.0).abs() < 1e-6
            && (sg_bb.max_x - 110.0).abs() < 1e-6
            && (sg_bb.min_y - 0.0).abs() < 1e-6
            && (sg_bb.max_y - 10.0).abs() < 1e-6,
        "SymbolInstance bounds must resolve nested Group inside SymbolDef, got {sg_bb:?}"
    );
}

#[test]
fn w1b_r2_boolean_simplify_dedup_symbol_delete_asset_gc_opacity_and_find_replace() {
    use coree_core::find_replace::{
        find_and_replace, FindQuery, FindScope, ReplaceAction, TextMatchMode,
    };

    // 1. BooleanOp::Simplify with duplicate IDs [a, a] must not leave orphaned objects
    let mut doc = Document::new_a4("W1B R2 Ops");
    let lid = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::default();

    let a_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "BoxA", rect_shape(20.0, 20.0)),
            Some(lid),
        )
        .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::BooleanOp {
            op: BooleanOp::Simplify,
            ids: vec![a_id, a_id],
        },
    )
    .unwrap();
    assert_eq!(doc.objects().len(), 1);
    assert_eq!(doc.find_layer(lid).unwrap().objects.len(), 1);

    // 2. DeleteSymbol unlinks nested SymbolInstance inside surviving SymbolDef (and undo restores)
    let sym_b = SymbolDef::new(
        "SymB",
        vec![SceneObject::new(ObjectId(0), "LeafB", rect_shape(8.0, 8.0))],
    );
    let sym_b_id = sym_b.id;
    let sym_a = SymbolDef::new(
        "SymA",
        vec![SceneObject::new(
            ObjectId(0),
            "NestedB",
            NodePayload::SymbolInstance(sym_b_id),
        )
        .with_transform(Affine2D::translate(15.0, 0.0))],
    );
    let sym_a_id = sym_a.id;
    hist.execute(&mut doc, CommandOp::UpsertSymbol { symbol: sym_b })
        .unwrap();
    hist.execute(&mut doc, CommandOp::UpsertSymbol { symbol: sym_a })
        .unwrap();

    let before_del_sym = doc.clone();
    hist.execute(&mut doc, CommandOp::DeleteSymbol { id: sym_b_id })
        .unwrap();
    let updated_sym_a = doc.symbols().get(&sym_a_id).unwrap();
    assert!(
        updated_sym_a
            .objects
            .iter()
            .all(|o| !matches!(o.payload, NodePayload::SymbolInstance(sid) if sid == sym_b_id)),
        "DeleteSymbol(SymB) must unlink nested SymbolInstance(SymB) inside SymA"
    );
    assert!(
        updated_sym_a
            .objects
            .iter()
            .any(|o| matches!(o.payload, NodePayload::Group(_))),
        "nested SymbolInstance must be materialized into Group inside SymA"
    );
    hist.undo(&mut doc).unwrap();
    assert_eq!(doc, before_del_sym);
    hist.redo(&mut doc).unwrap();

    // 3. ObjectStyle::fill asset GC retention + CommandHistory::execute undo parity with pre-command unreferenced asset
    let style_asset = RasterAsset::new(2, 2, vec![128u8; 16], 72.0, ColorMode::Rgb).unwrap();
    let style_aid = doc.register_asset(style_asset);
    let mut img_style = ObjectStyle::new("BmpPatternStyle");
    img_style.fill = Some(Fill::BitmapPattern {
        image_id: style_aid,
        tile_w: 10.0,
        tile_h: 10.0,
    });
    let img_style_id = img_style.id;
    hist.execute(&mut doc, CommandOp::UpsertObjectStyle { style: img_style })
        .unwrap();
    assert!(doc.referenced_asset_ids().contains(&style_aid));
    assert_eq!(hist.gc_document_assets(&mut doc), 0);
    assert!(doc.asset(style_aid).is_some());

    // DeleteObjectStyle moves style into undo delta -> asset stays kept until history is cleared
    hist.execute(&mut doc, CommandOp::DeleteObjectStyle { id: img_style_id })
        .unwrap();
    assert!(hist.referenced_asset_ids().contains(&style_aid));
    assert!(doc.asset(style_aid).is_some());

    // Pre-command unreferenced asset: execute prunes it AND undo restores doc == before bit-for-bit!
    let orphan_asset = RasterAsset::new(2, 2, vec![99u8; 16], 72.0, ColorMode::Rgb).unwrap();
    let orphan_aid = doc.register_asset(orphan_asset);
    let before_exec = doc.clone();
    hist.execute(
        &mut doc,
        CommandOp::create_object(
            SceneObject::new(ObjectId(0), "ShapeAfterAsset", rect_shape(5.0, 5.0)),
            Some(lid),
        ),
    )
    .unwrap();
    assert!(
        doc.asset(orphan_aid).is_none(),
        "execute must still GC unreferenced asset"
    );
    hist.undo(&mut doc).unwrap();
    assert_eq!(
        doc, before_exec,
        "undo after execute with pre-command unreferenced asset must restore doc == before"
    );

    // 4. Rhai macro post-commit GC reclaims overwritten redo-branch assets cleanly
    let macro_asset = RasterAsset::new(2, 2, vec![77u8; 16], 72.0, ColorMode::Rgb).unwrap();
    let macro_aid = doc.register_asset(macro_asset);
    let before_macro = doc.clone();
    let script = format!(
        r#"exec_command_json("{{\"RenameObject\":{{\"id\":{},\"new_name\":\"MacroRenamed\"}}}}");"#,
        a_id.0
    );
    // Execute a trivial rename on existing object in doc to trigger macro commit + gc_after_commit
    let _ = script;
    let first_oid = *doc.objects().keys().next().unwrap();
    let macro_script = format!(
        r#"exec_command_json("{{\"RenameObject\":{{\"id\":{},\"new_name\":\"MacroRenamed\"}}}}");"#,
        first_oid.0
    );
    let ran = run_rhai_macro(&mut doc, &mut hist, &macro_script).unwrap();
    assert_eq!(ran, 1);
    assert!(
        doc.asset(macro_aid).is_none(),
        "run_rhai_macro must GC unreferenced assets after commit_transaction"
    );
    hist.undo(&mut doc).unwrap();
    assert_eq!(
        doc, before_macro,
        "undo after run_rhai_macro must also restore doc == before"
    );

    // 5. UpsertObjectStyle & SetObjectStyleLink reject NaN/Inf opacity cleanly
    let mut bad_style = ObjectStyle::new("BadNaN");
    bad_style.opacity = Some(f32::NAN);
    let bad_err = hist
        .execute(&mut doc, CommandOp::UpsertObjectStyle { style: bad_style })
        .expect_err("NaN opacity in UpsertObjectStyle must error");
    assert!(matches!(bad_err, CoreError::InvalidParameter(_)));

    let mut bad_direct = ObjectStyle::new("DirectNaN");
    bad_direct.opacity = Some(f32::INFINITY);
    doc.upsert_object_style(bad_direct);
    assert!(
        doc.object_styles().last().unwrap().opacity.is_none(),
        "direct upsert_object_style must sanitize non-finite opacity"
    );

    // 6. FindQuery rejects unknown JSON fields via #[serde(deny_unknown_fields)]
    let bad_json =
        r#"{"FindAndReplace":{"query":{"text_contains":"abc"},"action":"SelectMatches"}}"#;
    assert!(
        serde_json::from_str::<CommandOp>(bad_json).is_err(),
        "unknown field text_contains in FindQuery must fail deserialization"
    );

    // 7. Unicode case-insensitive ReplaceText preserves prefix ('İstanbul WORLD' -> 'İstanbul Earth')
    let txt_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "TrText",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "İstanbul WORLD",
                    TextStyle::default(),
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let rep = find_and_replace(
        &mut doc,
        &FindQuery {
            scope: FindScope::WholeDocument,
            text_pattern: Some((
                "world".to_owned(),
                TextMatchMode::Contains {
                    case_sensitive: false,
                },
            )),
            ..FindQuery::default()
        },
        &ReplaceAction::ReplaceText {
            find: "world".to_owned(),
            replace_with: "Earth".to_owned(),
            case_sensitive: false,
        },
    );
    assert_eq!(rep.replacements_applied, 1);
    if let NodePayload::ArtisticText(ref t) = doc.object(txt_id).unwrap().payload {
        assert_eq!(t.plain_text(), "İstanbul Earth");
    } else {
        panic!("expected ArtisticText");
    }
}

#[test]
fn w1b_r3_gc_after_commit_empty_tx_and_regex_like_backslash_parity() {
    // 1. gc_after_commit after empty commit_transaction / undo must NEVER pollute prior undo entries
    let mut doc = Document::new_a4("EmptyTxGC");
    let lid = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::default();

    hist.execute(
        &mut doc,
        CommandOp::create_object(
            SceneObject::new(ObjectId(0), "FirstBox", rect_shape(10.0, 10.0)),
            Some(lid),
        ),
    )
    .unwrap();

    // Register a later unreferenced asset AFTER FirstBox was already committed
    let orphan_later = RasterAsset::new(2, 2, vec![42u8; 16], 72.0, ColorMode::Rgb).unwrap();
    let orphan_id = doc.register_asset(orphan_later);

    // Commit an empty transaction and call gc_after_commit
    hist.begin_transaction(&doc, "EmptyTx").unwrap();
    hist.commit_transaction(&doc).unwrap();
    let pruned_empty = hist.gc_after_commit(&mut doc);
    assert_eq!(pruned_empty, 1);
    assert!(doc.asset(orphan_id).is_none());

    // Undoing FirstBox must NOT resurrect orphan_later (which did not exist before FirstBox!)
    hist.undo(&mut doc).unwrap();
    assert!(
        doc.asset(orphan_id).is_none(),
        "undoing earlier CreateObject after empty commit_transaction + gc_after_commit must not resurrect later orphan asset"
    );

    // Similarly, calling gc_after_commit right after undo() must not mutate an even earlier entry
    hist.redo(&mut doc).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::create_object(
            SceneObject::new(ObjectId(0), "SecondBox", rect_shape(12.0, 12.0)),
            Some(lid),
        ),
    )
    .unwrap();
    hist.undo(&mut doc).unwrap();
    let orphan_after_undo = RasterAsset::new(2, 2, vec![77u8; 16], 72.0, ColorMode::Rgb).unwrap();
    let orphan_after_undo_id = doc.register_asset(orphan_after_undo);
    assert_eq!(hist.gc_after_commit(&mut doc), 1);
    hist.undo(&mut doc).unwrap();
    assert!(
        doc.asset(orphan_after_undo_id).is_none(),
        "gc_after_commit immediately after undo() must not pollute earlier undo entry"
    );

    // 2. RegexLike backslash parity before '$' anchor
    let mut rdoc = Document::new_a4("RegexParity");
    let rlid = rdoc.pages()[0].layers[0].id;
    let id_backslash = rdoc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "BackslashEnd",
                NodePayload::ArtisticText(ArtisticText::from_plain(r"foo\", TextStyle::default())),
            ),
            Some(rlid),
        )
        .unwrap();
    let _id_backslash_bar = rdoc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "BackslashBar",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    r"foo\bar",
                    TextStyle::default(),
                )),
            ),
            Some(rlid),
        )
        .unwrap();
    let id_dollar = rdoc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "DollarEnd",
                NodePayload::ArtisticText(ArtisticText::from_plain("foo$", TextStyle::default())),
            ),
            Some(rlid),
        )
        .unwrap();

    // Even number of backslashes before '$': '$' IS the end anchor
    let hits_even = find_objects(
        &rdoc,
        &FindQuery {
            scope: FindScope::WholeDocument,
            text_pattern: Some((
                r"^foo\\$".to_owned(),
                TextMatchMode::RegexLike {
                    case_sensitive: true,
                },
            )),
            ..FindQuery::default()
        },
    );
    assert_eq!(
        hits_even,
        vec![id_backslash],
        r"^foo\\$ must match 'foo\' and reject 'foo\bar'"
    );

    // Odd number of backslashes before '$': '\$' is literal '$'
    let hits_odd = find_objects(
        &rdoc,
        &FindQuery {
            scope: FindScope::WholeDocument,
            text_pattern: Some((
                r"^foo\$".to_owned(),
                TextMatchMode::RegexLike {
                    case_sensitive: true,
                },
            )),
            ..FindQuery::default()
        },
    );
    assert_eq!(hits_odd, vec![id_dollar], r"^foo\$ must match 'foo$'");
}

#[test]
fn w1b_r4_edit_shape_delete_layer_register_asset_and_create_detached_object() {
    let mut doc = Document::new_a4("W1B-R4");
    let init_doc = doc.clone();
    let mut hist = CommandHistory::default();
    let page_id = doc.pages()[0].id;
    let base_layer = doc.pages()[0].layers[0].id;

    // 1. EditShape on top-level & nested Group ParametricShape + error rejections
    hist.execute(
        &mut doc,
        CommandOp::create_object(
            SceneObject::new(ObjectId(0), "TopRect", rect_shape(20.0, 10.0)),
            Some(base_layer),
        ),
    )
    .unwrap();
    let top_rect_id = *doc.objects().keys().last().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::create_object(
            SceneObject::new(ObjectId(0), "NestedRect", rect_shape(30.0, 15.0)),
            Some(base_layer),
        ),
    )
    .unwrap();
    let child_rect_id = *doc.objects().keys().last().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::create_object(
            SceneObject::new(
                ObjectId(0),
                "PlainPath",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 5.0, 5.0))),
            ),
            Some(base_layer),
        ),
    )
    .unwrap();
    let path_id = *doc.objects().keys().last().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![child_rect_id, path_id],
            name: "G1".to_owned(),
        },
    )
    .unwrap();

    let updated_shape = ParametricShape::Rectangle {
        width: 45.0,
        height: 25.0,
        corner_radii: [4.0; 4],
        corner_style: CornerStyle::Rounded,
        lock_corners: true,
    };
    hist.execute(
        &mut doc,
        CommandOp::EditShape {
            id: child_rect_id,
            shape: updated_shape.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        doc.object(child_rect_id).unwrap().payload,
        NodePayload::Shape(updated_shape.clone())
    );
    assert_eq!(
        doc.layer_of_object(child_rect_id),
        None,
        "nested EditShape must stay detached from top-level layers"
    );

    // Reject NaN shape parameters
    let nan_shape = ParametricShape::Rectangle {
        width: f64::NAN,
        height: 10.0,
        corner_radii: [0.0; 4],
        corner_style: CornerStyle::Rounded,
        lock_corners: true,
    };
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::EditShape {
                id: top_rect_id,
                shape: nan_shape,
            },
        ),
        Err(CoreError::InvalidGeometry(_))
    ));

    // Reject non-Shape payload (Path)
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::EditShape {
                id: path_id,
                shape: updated_shape.clone(),
            },
        ),
        Err(CoreError::InvalidParameter(_))
    ));

    // Reject locked object
    hist.execute(
        &mut doc,
        CommandOp::SetObjectLocked {
            ids: vec![top_rect_id],
            locked: true,
        },
    )
    .unwrap();
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::EditShape {
                id: top_rect_id,
                shape: updated_shape.clone(),
            },
        ),
        Err(CoreError::ObjectLocked(_))
    ));

    // 2. DeleteLayer: rejects CannotDeleteLastLayer, locked layer, and locked object; succeeds on unlocked layer
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::DeleteLayer {
                layer_id: base_layer,
            },
        ),
        Err(CoreError::CannotDeleteLastLayer)
    ));
    hist.execute(
        &mut doc,
        CommandOp::AddLayer {
            page_id,
            name: "Temp Layer".to_owned(),
            is_cut_layer: false,
            master_scope: MasterLayerScope::None,
        },
    )
    .unwrap();
    let temp_layer = doc.pages()[0].layers.last().unwrap().id;
    hist.execute(
        &mut doc,
        CommandOp::SetLayerFlags {
            layer_id: temp_layer,
            visible: None,
            locked: Some(true),
            printable: None,
            is_cut_layer: None,
        },
    )
    .unwrap();
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::DeleteLayer {
                layer_id: temp_layer,
            },
        ),
        Err(CoreError::InvalidParameter(_))
    ));
    hist.execute(
        &mut doc,
        CommandOp::SetLayerFlags {
            layer_id: temp_layer,
            visible: None,
            locked: Some(false),
            printable: None,
            is_cut_layer: None,
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::DeleteLayer {
            layer_id: temp_layer,
        },
    )
    .unwrap();
    assert!(doc.find_layer(temp_layer).is_none());

    // 3. RegisterAsset validation & Batch with CreateObject
    let mut bad_asset = RasterAsset::new(2, 2, vec![255u8; 16], 72.0, ColorMode::Rgb).unwrap();
    bad_asset.truncate_last_tile_for_test();
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::RegisterAsset { asset: bad_asset }),
        Err(CoreError::InvalidParameter(_))
    ));
    let good_asset = RasterAsset::new(2, 2, vec![42u8; 16], 150.0, ColorMode::Rgb).unwrap();
    let asset_id = good_asset.id;
    let raster_obj = SceneObject::new(
        ObjectId(0),
        "RegisteredRaster",
        NodePayload::Raster(RasterLayerData {
            asset_id,
            width_mm: 10.0,
            height_mm: 10.0,
            filters: Vec::new(),
            filter_enabled: Vec::new(),
            crop_rect: None,
        }),
    );
    hist.execute(
        &mut doc,
        CommandOp::Batch(vec![
            CommandOp::RegisterAsset { asset: good_asset },
            CommandOp::create_object(raster_obj, Some(base_layer)),
        ]),
    )
    .unwrap();
    assert!(doc.asset(asset_id).is_some());

    // 4. CreateDetachedObject + Group wrapper
    let detached_id = ObjectId(doc.next_id().max(1));
    let detached_child = SceneObject::new(detached_id, "DetachedChild", rect_shape(8.0, 8.0));
    let wrapper_id = ObjectId(detached_id.0 + 1);
    let wrapper_group = SceneObject::new(
        wrapper_id,
        "ImportedGroup",
        NodePayload::Group(vec![detached_id]),
    );
    hist.execute(
        &mut doc,
        CommandOp::Batch(vec![
            CommandOp::create_detached_object(detached_child),
            CommandOp::create_object(wrapper_group, Some(base_layer)),
        ]),
    )
    .unwrap();
    assert_eq!(doc.layer_of_object(detached_id), None);
    assert_eq!(doc.parent_container_of(detached_id), Some(wrapper_id));

    // Verify replay_log & undo/redo parity
    let log = hist.command_log();
    let mut replayed = init_doc;
    CommandHistory::replay_log(&mut replayed, &log).unwrap();
    assert_eq!(replayed.objects(), doc.objects());
    assert_eq!(replayed.pages(), doc.pages());
    assert_eq!(replayed.assets(), doc.assets());

    let before_undo = doc.clone();
    hist.undo(&mut doc).unwrap();
    assert!(doc.object(detached_id).is_none());
    hist.redo(&mut doc).unwrap();
    assert_eq!(doc.objects(), before_undo.objects());
    assert_eq!(doc.pages(), before_undo.pages());
}

#[test]
fn w1b_r5_standalone_register_asset_eviction_and_create_detached_guards() {
    // 1. Standalone RegisterAsset + two-step RegisterAsset -> UpdateRaster + replay_log parity
    let mut doc = Document::new_a4("W1B-R5-RegisterAsset");
    let init_doc = doc.clone();
    let base_layer = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::default();

    let standalone_asset = RasterAsset::new(2, 2, vec![11u8; 16], 150.0, ColorMode::Rgb).unwrap();
    let standalone_aid = standalone_asset.id;
    hist.execute(
        &mut doc,
        CommandOp::RegisterAsset {
            asset: standalone_asset.clone(),
        },
    )
    .expect("standalone RegisterAsset must succeed");
    assert_eq!(
        doc.asset(standalone_aid),
        Some(&standalone_asset),
        "standalone RegisterAsset must retain asset in live_doc after gc_after_commit"
    );

    // Undo / Redo standalone RegisterAsset
    hist.undo(&mut doc).unwrap();
    assert!(doc.asset(standalone_aid).is_none());
    hist.redo(&mut doc).unwrap();
    assert_eq!(doc.asset(standalone_aid), Some(&standalone_asset));

    // Replay parity right after standalone RegisterAsset
    let mut replayed_standalone = init_doc.clone();
    CommandHistory::replay_log(&mut replayed_standalone, &hist.command_log()).unwrap();
    assert_eq!(replayed_standalone.assets(), doc.assets());

    // Two-step RegisterAsset -> UpdateRaster
    let initial_asset = RasterAsset::new(2, 2, vec![22u8; 16], 72.0, ColorMode::Rgb).unwrap();
    let initial_aid = initial_asset.id;
    let raster_obj = SceneObject::new(
        ObjectId(0),
        "TargetRaster",
        NodePayload::Raster(RasterLayerData {
            asset_id: initial_aid,
            width_mm: 10.0,
            height_mm: 10.0,
            filters: Vec::new(),
            filter_enabled: Vec::new(),
            crop_rect: None,
        }),
    );
    hist.execute(
        &mut doc,
        CommandOp::Batch(vec![
            CommandOp::RegisterAsset {
                asset: initial_asset,
            },
            CommandOp::create_object(raster_obj, Some(base_layer)),
        ]),
    )
    .unwrap();
    let raster_oid = *doc.objects().keys().last().unwrap();

    let replacement_asset = RasterAsset::new(2, 2, vec![33u8; 16], 300.0, ColorMode::Rgb).unwrap();
    let replacement_aid = replacement_asset.id;
    hist.execute(
        &mut doc,
        CommandOp::RegisterAsset {
            asset: replacement_asset.clone(),
        },
    )
    .unwrap();
    assert_eq!(doc.asset(replacement_aid), Some(&replacement_asset));

    hist.execute(
        &mut doc,
        CommandOp::UpdateRaster {
            id: raster_oid,
            data: RasterLayerData {
                asset_id: replacement_aid,
                width_mm: 20.0,
                height_mm: 20.0,
                filters: Vec::new(),
                filter_enabled: Vec::new(),
                crop_rect: None,
            },
        },
    )
    .unwrap();
    assert_eq!(doc.asset(replacement_aid), Some(&replacement_asset));

    let mut replayed_two_step = init_doc;
    CommandHistory::replay_log(&mut replayed_two_step, &hist.command_log()).unwrap();
    assert_eq!(replayed_two_step.assets(), doc.assets());
    assert_eq!(replayed_two_step.objects(), doc.objects());

    // 2. max_history eviction still prunes unreferenced assets once RegisterAsset is evicted
    let mut ev_doc = Document::new_a4("Evict");
    let ev_layer = ev_doc.pages()[0].layers[0].id;
    let ev_box_id = ev_doc
        .add_object(
            SceneObject::new(ObjectId(0), "Box", rect_shape(10.0, 10.0)),
            Some(ev_layer),
        )
        .unwrap();
    let ev_init = ev_doc.clone();
    let mut ev_hist = CommandHistory::new(2);

    let orphan_asset = RasterAsset::new(2, 2, vec![55u8; 16], 72.0, ColorMode::Rgb).unwrap();
    let orphan_aid = orphan_asset.id;
    ev_hist
        .execute(
            &mut ev_doc,
            CommandOp::RegisterAsset {
                asset: orphan_asset,
            },
        )
        .unwrap();
    assert!(ev_doc.asset(orphan_aid).is_some());

    // 1st unrelated op: RegisterAsset is still on undo_stack[0] -> asset kept
    ev_hist
        .execute(
            &mut ev_doc,
            CommandOp::RenameObject {
                id: ev_box_id,
                new_name: "Box1".to_owned(),
            },
        )
        .unwrap();
    assert!(ev_doc.asset(orphan_aid).is_some());

    // 2nd unrelated op: RegisterAsset is evicted -> gc_after_commit prunes unreferenced asset
    ev_hist
        .execute(
            &mut ev_doc,
            CommandOp::RenameObject {
                id: ev_box_id,
                new_name: "Box2".to_owned(),
            },
        )
        .unwrap();
    assert!(
        ev_doc.asset(orphan_aid).is_none(),
        "unreferenced RegisterAsset asset must be pruned after eviction from max_history"
    );

    // Note: replay_log from initial doc with remaining [Rename1, Rename2] matches ev_doc.assets()
    let mut ev_replayed = ev_init;
    CommandHistory::replay_log(&mut ev_replayed, &ev_hist.command_log()).unwrap();
    assert_eq!(ev_replayed.assets(), ev_doc.assets());
    assert_eq!(ev_replayed.objects(), ev_doc.objects());

    // 3. CreateDetachedObject collision & non-finite transform/geometry rejection
    let mut det_doc = Document::new_a4("DetachedGuards");
    let det_layer = det_doc.pages()[0].layers[0].id;
    let existing_id = det_doc
        .add_object(
            SceneObject::new(ObjectId(0), "Original", rect_shape(15.0, 15.0)),
            Some(det_layer),
        )
        .unwrap();
    let mut det_hist = CommandHistory::default();

    let colliding_obj = SceneObject::new(existing_id, "Overwritten", rect_shape(5.0, 5.0));
    let coll_err = det_hist
        .execute(
            &mut det_doc,
            CommandOp::create_detached_object(colliding_obj),
        )
        .expect_err("CreateDetachedObject with colliding ObjectId must fail");
    assert!(matches!(coll_err, CoreError::InvalidParameter(_)));
    assert_eq!(det_doc.object(existing_id).unwrap().name, "Original");

    let mut nan_xf_obj = SceneObject::new(ObjectId(0), "NanXf", rect_shape(5.0, 5.0));
    nan_xf_obj.transform = Affine2D::new(f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0);
    let nan_xf_err = det_hist
        .execute(&mut det_doc, CommandOp::create_detached_object(nan_xf_obj))
        .expect_err("CreateDetachedObject with NaN transform must fail");
    assert!(matches!(nan_xf_err, CoreError::InvalidGeometry(_)));

    let nan_shape_obj = SceneObject::new(
        ObjectId(0),
        "NanShape",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: f64::INFINITY,
            height: 10.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    );
    let nan_shape_err = det_hist
        .execute(
            &mut det_doc,
            CommandOp::create_detached_object(nan_shape_obj),
        )
        .expect_err("CreateDetachedObject with Inf ParametricShape must fail");
    assert!(matches!(nan_shape_err, CoreError::InvalidGeometry(_)));

    let mut nan_path = BezierPath::new();
    nan_path.subpaths.push(Subpath {
        nodes: vec![PathNode::corner(Point2D::new(f64::NAN, 1.0))],
        closed: false,
    });
    let nan_path_obj = SceneObject::new(ObjectId(0), "NanPath", NodePayload::Path(nan_path));
    let nan_path_err = det_hist
        .execute(
            &mut det_doc,
            CommandOp::create_detached_object(nan_path_obj),
        )
        .expect_err("CreateDetachedObject with NaN BezierPath must fail");
    assert!(matches!(nan_path_err, CoreError::InvalidGeometry(_)));
}

#[test]
fn w1b_r6_container_guards_style_finiteness_locked_descendants_and_table_dim_colors() {
    let mut doc = Document::new_a4("W1B-R6-Guards");
    let layer = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::default();

    // 1. EditTable / UpdateRaster / EditText reject Group and PowerClip without orphaning children
    let child_a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ChildA", rect_shape(10.0, 10.0)),
            Some(layer),
        )
        .unwrap();
    let child_b = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ChildB", rect_shape(12.0, 12.0)),
            Some(layer),
        )
        .unwrap();
    let group_id = doc.group_objects(&[child_a, child_b], "Grp").unwrap();

    let clip_child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ClipChild", rect_shape(8.0, 8.0)),
            Some(layer),
        )
        .unwrap();
    let clip_container = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ClipHost", rect_shape(30.0, 30.0)),
            Some(layer),
        )
        .unwrap();
    doc.create_powerclip(clip_container, &[clip_child]).unwrap();

    let asset = RasterAsset::new(1, 1, vec![255, 0, 0, 255], 300.0, ColorMode::Rgb).unwrap();
    let aid = doc.register_asset(asset);

    for container_id in [group_id, clip_container] {
        let err_tbl = hist
            .execute(
                &mut doc,
                CommandOp::EditTable {
                    id: container_id,
                    table: VectorTable::new(1, 1, 10.0, 5.0),
                },
            )
            .expect_err("EditTable on container must fail");
        assert!(matches!(err_tbl, CoreError::InvalidParameter(_)));

        let err_ras = hist
            .execute(
                &mut doc,
                CommandOp::UpdateRaster {
                    id: container_id,
                    data: RasterLayerData {
                        asset_id: aid,
                        width_mm: 10.0,
                        height_mm: 10.0,
                        filters: Vec::new(),
                        filter_enabled: Vec::new(),
                        crop_rect: None,
                    },
                },
            )
            .expect_err("UpdateRaster on container must fail");
        assert!(matches!(err_ras, CoreError::InvalidParameter(_)));

        let err_txt = hist
            .execute(
                &mut doc,
                CommandOp::EditText {
                    id: container_id,
                    payload: NodePayload::ArtisticText(ArtisticText::from_plain(
                        "X",
                        TextStyle::default(),
                    )),
                },
            )
            .expect_err("EditText on container must fail");
        assert!(matches!(err_txt, CoreError::InvalidParameter(_)));
    }
    assert!(matches!(
        doc.object(group_id).unwrap().payload,
        NodePayload::Group(_)
    ));
    assert!(doc.has_object(child_a) && doc.has_object(child_b));
    assert!(matches!(
        doc.object(clip_container).unwrap().payload,
        NodePayload::PowerClip { .. }
    ));
    assert!(doc.has_object(clip_child));

    // Non-finite payload validation in EditText, EditTable, UpdateRaster, EditDimension
    let leaf_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Leaf", rect_shape(10.0, 10.0)),
            Some(layer),
        )
        .unwrap();
    let bad_style = TextStyle {
        font_size_pt: f64::NAN,
        ..TextStyle::default()
    };
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::EditText {
                id: leaf_id,
                payload: NodePayload::ArtisticText(ArtisticText::from_plain("Bad", bad_style)),
            },
        ),
        Err(CoreError::InvalidGeometry(_))
    ));

    let mut bad_tbl = VectorTable::new(1, 1, 10.0, 5.0);
    bad_tbl.col_widths_mm[0] = f64::INFINITY;
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::EditTable {
                id: leaf_id,
                table: bad_tbl,
            },
        ),
        Err(CoreError::InvalidGeometry(_))
    ));

    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::UpdateRaster {
                id: leaf_id,
                data: RasterLayerData {
                    asset_id: aid,
                    width_mm: f64::NEG_INFINITY,
                    height_mm: 10.0,
                    filters: Vec::new(),
                    filter_enabled: Vec::new(),
                    crop_rect: None,
                },
            },
        ),
        Err(CoreError::InvalidGeometry(_))
    ));

    // 2. CreateDetachedObject rejects non-finite opacity / fill / outline / lens / effects
    let mut nan_opacity = SceneObject::new(ObjectId(0), "NanOp", rect_shape(5.0, 5.0));
    nan_opacity.opacity = f32::NAN;
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::create_detached_object(nan_opacity)),
        Err(CoreError::InvalidGeometry(_))
    ));

    let mut nan_grad = SceneObject::new(ObjectId(0), "NanGrad", rect_shape(5.0, 5.0));
    nan_grad.fill = Fill::LinearGradient {
        start: Point2D::new(f64::NAN, 0.0),
        end: Point2D::new(10.0, 0.0),
        stops: vec![GradientStop::new(0.0, Color::BLACK)],
    };
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::create_detached_object(nan_grad)),
        Err(CoreError::InvalidGeometry(_))
    ));

    let mut nan_outline = SceneObject::new(ObjectId(0), "NanOutl", rect_shape(5.0, 5.0));
    nan_outline.outline.width_mm = f64::INFINITY;
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::create_detached_object(nan_outline)),
        Err(CoreError::InvalidGeometry(_))
    ));

    let mut nan_eff = SceneObject::new(ObjectId(0), "NanEff", rect_shape(5.0, 5.0));
    nan_eff.effects.push(Effect::DropShadow {
        dx_mm: f64::NAN,
        dy_mm: 2.0,
        blur_mm: 3.0,
        opacity: 0.5,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Middle,
    });
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::create_detached_object(nan_eff)),
        Err(CoreError::InvalidGeometry(_))
    ));

    let mut nan_lens = SceneObject::new(ObjectId(0), "NanLens", rect_shape(5.0, 5.0));
    nan_lens.lens = Some(LensConfig {
        kind: LensKind::Brighten { rate_pct: f32::NAN },
        frozen: false,
        viewpoint: None,
        remove_face: false,
        frozen_snapshot: None,
    });
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::create_detached_object(nan_lens)),
        Err(CoreError::InvalidGeometry(_))
    ));

    // 3. DeleteObjects rejects unlocked Group/PowerClip containing a locked descendant atomically
    hist.execute(
        &mut doc,
        CommandOp::SetObjectLocked {
            ids: vec![child_b],
            locked: true,
        },
    )
    .unwrap();
    let del_err = hist
        .execute(
            &mut doc,
            CommandOp::DeleteObjects {
                ids: vec![leaf_id, group_id],
            },
        )
        .expect_err("DeleteObjects on Group with locked child must fail");
    assert!(matches!(del_err, CoreError::ObjectLocked(id) if id == child_b));
    assert!(doc.has_object(leaf_id));
    assert!(doc.has_object(group_id));
    assert!(doc.has_object(child_a));
    assert!(doc.has_object(child_b));

    // Also test locked child inside PowerClip
    hist.execute(
        &mut doc,
        CommandOp::SetObjectLocked {
            ids: vec![clip_child],
            locked: true,
        },
    )
    .unwrap();
    let del_pc_err = hist
        .execute(
            &mut doc,
            CommandOp::DeleteObjects {
                ids: vec![clip_container],
            },
        )
        .expect_err("DeleteObjects on PowerClip with locked child must fail");
    assert!(matches!(del_pc_err, CoreError::ObjectLocked(id) if id == clip_child));
    assert!(doc.has_object(clip_container));
    assert!(doc.has_object(clip_child));

    // 4. Document::map_unlocked_object_colors covers non-solid TableCell::fill, text_style.fill, border, Dimension
    let mut tbl = VectorTable::new(1, 2, 20.0, 10.0);
    tbl.cells[0].fill = Fill::LinearGradient {
        start: Point2D::ZERO,
        end: Point2D::new(20.0, 10.0),
        stops: vec![GradientStop::new(0.0, Color::rgb(1.0, 0.0, 0.0))],
    };
    tbl.cells[0].text_style.fill = Color::rgb(0.9, 0.1, 0.1);
    tbl.cells[1].fill = Fill::MeshFill {
        rows: 1,
        cols: 1,
        nodes: vec![MeshNode::new(
            0,
            0,
            Point2D::ZERO,
            Color::rgb(0.2, 0.4, 0.8),
        )],
    };
    tbl.border_style = OutlineStyle::solid(Color::rgb(1.0, 0.0, 0.0), 0.5);
    let tbl_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "StyledTable", NodePayload::Table(tbl)),
            Some(layer),
        )
        .unwrap();

    let mut dim = DimensionObject::linear(
        DimensionKind::Horizontal,
        Point2D::ZERO,
        Point2D::new(50.0, 0.0),
        8.0,
    );
    dim.text_style.fill = Color::rgb(1.0, 0.0, 0.0);
    let dim_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "StyledDim", NodePayload::Dimension(dim)),
            Some(layer),
        )
        .unwrap();

    let mapped = doc.map_unlocked_object_colors(|c| match c {
        Color::Rgba { r, g, b, a } => {
            *c = Color::cmyk(1.0 - *r, 1.0 - *g, 1.0 - *b, 0.0).with_alpha(*a);
            true
        }
        _ => false,
    });
    assert!(mapped >= 5);
    if let NodePayload::Table(ref t) = doc.object(tbl_id).unwrap().payload {
        assert!(matches!(
            t.cells[0].fill,
            Fill::LinearGradient { ref stops, .. } if matches!(stops[0].color, Color::Cmyk { .. })
        ));
        assert!(matches!(t.cells[0].text_style.fill, Color::Cmyk { .. }));
        assert!(matches!(
            t.cells[1].fill,
            Fill::MeshFill { ref nodes, .. } if matches!(nodes[0].color, Color::Cmyk { .. })
        ));
        assert!(matches!(t.border_style.color, Color::Cmyk { .. }));
    } else {
        panic!("expected Table");
    }
    if let NodePayload::Dimension(ref d) = doc.object(dim_id).unwrap().payload {
        assert!(matches!(d.text_style.fill, Color::Cmyk { .. }));
    } else {
        panic!("expected Dimension");
    }

    // EditDimension undo/redo + guard
    let base_before_edit_dim = doc.clone();
    let mut dim2 = match &doc.object(dim_id).unwrap().payload {
        NodePayload::Dimension(d) => d.clone(),
        _ => unreachable!(),
    };
    dim2.text_style.fill = Color::CYAN;
    let mut dim_hist = CommandHistory::default();
    dim_hist
        .execute(
            &mut doc,
            CommandOp::EditDimension {
                id: dim_id,
                dimension: dim2,
            },
        )
        .unwrap();
    if let NodePayload::Dimension(ref d) = doc.object(dim_id).unwrap().payload {
        assert_eq!(d.text_style.fill, Color::CYAN);
    }
    dim_hist.undo(&mut doc).unwrap();
    assert_eq!(doc.objects(), base_before_edit_dim.objects());
    dim_hist.redo(&mut doc).unwrap();
    if let NodePayload::Dimension(ref d) = doc.object(dim_id).unwrap().payload {
        assert_eq!(d.text_style.fill, Color::CYAN);
    }
}

#[test]
fn w1b_r6_delete_page_cross_page_relational_cleanup_undo_redo_replay() {
    let mut doc = Document::new_a4("DeletePage-CrossPage");
    let p1_layer = doc.pages()[0].layers[0].id;
    let p2_id = doc.add_page("Page 2", 210.0, 297.0, 0.0);
    let p2_layer = doc.page_by_id(p2_id).unwrap().layers[0].id;

    // Objects on Page 2 that Page 1 objects reference
    let p2_box = doc
        .add_object(
            SceneObject::new(ObjectId(0), "P2Box", rect_shape(20.0, 20.0))
                .with_transform(Affine2D::translate(80.0, 40.0)),
            Some(p2_layer),
        )
        .unwrap();
    let p2_path = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "P2Path",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 0.0),
                    Point2D::new(50.0, 0.0),
                )),
            )
            .with_transform(Affine2D::translate(10.0, 30.0)),
            Some(p2_layer),
        )
        .unwrap();
    let p2_blend_target = doc
        .add_object(
            SceneObject::new(ObjectId(0), "P2BlendTarget", rect_shape(15.0, 15.0)),
            Some(p2_layer),
        )
        .unwrap();

    // Cross-page references from Page 1 -> Page 2
    let p1_box = doc
        .add_object(
            SceneObject::new(ObjectId(0), "P1Box", rect_shape(20.0, 20.0)),
            Some(p1_layer),
        )
        .unwrap();
    let conn_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "CrossConn",
                NodePayload::Connector(ConnectorObject::between_objects(
                    p1_box,
                    AnchorPoint::Right,
                    p2_box,
                    AnchorPoint::Left,
                    ConnectorStyle::Straight,
                )),
            ),
            Some(p1_layer),
        )
        .unwrap();

    let mut blend_src = SceneObject::new(ObjectId(0), "BlendSrc", rect_shape(10.0, 10.0));
    blend_src.effects.push(Effect::Blend {
        target_id: p2_blend_target,
        steps: 4,
        rotation_deg: 0.0,
        along_path: Some(p2_path),
        rotate_along_path: true,
    });
    let blend_src_id = doc.add_object(blend_src, Some(p1_layer)).unwrap();

    let mut blend_along_only =
        SceneObject::new(ObjectId(0), "BlendAlongOnly", rect_shape(10.0, 10.0));
    blend_along_only.effects.push(Effect::Blend {
        target_id: p1_box,
        steps: 3,
        rotation_deg: 0.0,
        along_path: Some(p2_path),
        rotate_along_path: true,
    });
    let blend_along_id = doc.add_object(blend_along_only, Some(p1_layer)).unwrap();

    let top_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "CrossTextOnPath",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("CrossPage", TextStyle::default()),
                    path_id: p2_path,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(p1_layer),
        )
        .unwrap();

    let frame_c = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "FrameC",
                NodePayload::ParagraphText(ParagraphText::new(
                    "C",
                    Vec2D::new(30.0, 20.0),
                    TextStyle::default(),
                )),
            ),
            Some(p1_layer),
        )
        .unwrap();
    let mut pb = ParagraphText::new("B", Vec2D::new(30.0, 20.0), TextStyle::default());
    pb.linked_next_frame = Some(frame_c);
    let frame_b = doc
        .add_object(
            SceneObject::new(ObjectId(0), "FrameB", NodePayload::ParagraphText(pb)),
            Some(p2_layer),
        )
        .unwrap();
    let mut pa = ParagraphText::new("A", Vec2D::new(30.0, 20.0), TextStyle::default());
    pa.linked_next_frame = Some(frame_b);
    let frame_a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "FrameA", NodePayload::ParagraphText(pa)),
            Some(p1_layer),
        )
        .unwrap();

    let base_doc = doc.clone();
    let mut hist = CommandHistory::default();
    hist.execute(&mut doc, CommandOp::DeletePage { page_id: p2_id })
        .unwrap();

    assert_eq!(doc.pages().len(), 1);
    assert!(!doc.has_object(p2_box));
    assert!(!doc.has_object(p2_path));
    assert!(!doc.has_object(p2_blend_target));
    assert!(!doc.has_object(frame_b));
    assert!(doc.object(blend_src_id).unwrap().effects.is_empty());
    assert_eq!(
        doc.object(blend_along_id).unwrap().effects,
        vec![Effect::Blend {
            target_id: p1_box,
            steps: 3,
            rotation_deg: 0.0,
            along_path: None,
            rotate_along_path: true,
        }]
    );
    assert!(matches!(
        doc.object(top_id).unwrap().payload,
        NodePayload::ArtisticText(_)
    ));
    if let NodePayload::Connector(ref c) = doc.object(conn_id).unwrap().payload {
        assert_eq!(c.from_obj, Some(p1_box));
        assert_eq!(c.to_obj, None);
    } else {
        panic!("expected Connector");
    }
    if let NodePayload::ParagraphText(ref p) = doc.object(frame_a).unwrap().payload {
        assert_eq!(p.linked_next_frame, Some(frame_c));
    } else {
        panic!("expected ParagraphText");
    }

    let post_delete = doc.clone();
    hist.undo(&mut doc).unwrap();
    assert_eq!(doc.objects(), base_doc.objects());
    assert_eq!(doc.pages(), base_doc.pages());

    hist.redo(&mut doc).unwrap();
    assert_eq!(doc.objects(), post_delete.objects());
    assert_eq!(doc.pages(), post_delete.pages());

    let mut replayed = base_doc;
    CommandHistory::replay_log(&mut replayed, &hist.command_log()).unwrap();
    assert_eq!(replayed.objects(), post_delete.objects());
    assert_eq!(replayed.pages(), post_delete.pages());
}

#[test]
fn w1b_r7_map_unlocked_object_colors_respects_locked_layers_and_containers() {
    let mut doc = Document::new_a4("W1B-R7-Locked-Layers-And-Containers");
    let page_id = doc.pages()[0].id;
    let unlocked_layer = doc.pages()[0].layers[0].id;
    let locked_layer = doc
        .add_layer(page_id, "Locked Layer", false, MasterLayerScope::None)
        .unwrap();

    // --- Unlocked layer: genuinely unlocked objects & containers ---
    let unlocked_leaf = doc
        .add_object(
            SceneObject::new(ObjectId(0), "UnlockedLeaf", rect_shape(10.0, 10.0))
                .with_fill(Fill::Solid(Color::rgb(1.0, 0.0, 0.0)))
                .with_outline(OutlineStyle::solid(Color::rgb(0.0, 1.0, 0.0), 0.5)),
            Some(unlocked_layer),
        )
        .unwrap();

    let unlocked_group_child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "UnlockedGrpChild", rect_shape(8.0, 8.0))
                .with_fill(Fill::Solid(Color::rgb(0.0, 0.0, 1.0)))
                .with_outline(OutlineStyle::none()),
            Some(unlocked_layer),
        )
        .unwrap();
    let unlocked_group = doc
        .group_objects(&[unlocked_group_child], "UnlockedGrp")
        .unwrap();

    let unlocked_pc_child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "UnlockedPcChild", rect_shape(6.0, 6.0))
                .with_fill(Fill::Solid(Color::rgb(0.5, 0.5, 0.0)))
                .with_outline(OutlineStyle::none()),
            Some(unlocked_layer),
        )
        .unwrap();
    let unlocked_pc = doc
        .add_object(
            SceneObject::new(ObjectId(0), "UnlockedPc", rect_shape(20.0, 20.0))
                .with_fill(Fill::Solid(Color::rgb(0.2, 0.3, 0.4)))
                .with_outline(OutlineStyle::none()),
            Some(unlocked_layer),
        )
        .unwrap();
    doc.create_powerclip(unlocked_pc, &[unlocked_pc_child])
        .unwrap();

    // --- Unlocked layer: locked Group & locked PowerClip containers with unlocked descendants ---
    let locked_grp_child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LockedGrpChild", rect_shape(10.0, 10.0))
                .with_fill(Fill::Solid(Color::rgb(0.8, 0.1, 0.2)))
                .with_outline(OutlineStyle::solid(Color::rgb(0.1, 0.8, 0.2), 0.25)),
            Some(unlocked_layer),
        )
        .unwrap();
    let mut locked_grp_pc_dim_payload = DimensionObject::linear(
        DimensionKind::Horizontal,
        Point2D::ZERO,
        Point2D::new(30.0, 0.0),
        5.0,
    );
    locked_grp_pc_dim_payload.text_style.fill = Color::rgb(0.7, 0.2, 0.3);
    let locked_grp_pc_dim = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "LockedGrpPcDim",
                NodePayload::Dimension(locked_grp_pc_dim_payload),
            ),
            Some(unlocked_layer),
        )
        .unwrap();
    let locked_grp_nested_pc = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LockedGrpNestedPc", rect_shape(25.0, 25.0))
                .with_fill(Fill::Solid(Color::rgb(0.3, 0.4, 0.5)))
                .with_outline(OutlineStyle::none()),
            Some(unlocked_layer),
        )
        .unwrap();
    doc.create_powerclip(locked_grp_nested_pc, &[locked_grp_pc_dim])
        .unwrap();
    let locked_group = doc
        .group_objects(&[locked_grp_child, locked_grp_nested_pc], "LockedGrp")
        .unwrap();

    let locked_pc_child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LockedPcChild", rect_shape(9.0, 9.0))
                .with_fill(Fill::Solid(Color::rgb(0.9, 0.2, 0.1)))
                .with_outline(OutlineStyle::solid(Color::rgb(0.2, 0.9, 0.1), 0.5)),
            Some(unlocked_layer),
        )
        .unwrap();
    let locked_pc = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LockedPc", rect_shape(30.0, 30.0))
                .with_fill(Fill::Solid(Color::rgb(0.4, 0.5, 0.6)))
                .with_outline(OutlineStyle::none()),
            Some(unlocked_layer),
        )
        .unwrap();
    doc.create_powerclip(locked_pc, &[locked_pc_child]).unwrap();

    // Mixed unlocked Group containing one locked inner Group and one unlocked sibling
    let inner_locked_leaf = doc
        .add_object(
            SceneObject::new(ObjectId(0), "InnerLockedLeaf", rect_shape(5.0, 5.0))
                .with_fill(Fill::Solid(Color::rgb(0.6, 0.1, 0.7)))
                .with_outline(OutlineStyle::none()),
            Some(unlocked_layer),
        )
        .unwrap();
    let inner_locked_grp = doc
        .group_objects(&[inner_locked_leaf], "InnerLockedGrp")
        .unwrap();
    let inner_unlocked_leaf = doc
        .add_object(
            SceneObject::new(ObjectId(0), "InnerUnlockedLeaf", rect_shape(5.0, 5.0))
                .with_fill(Fill::Solid(Color::rgb(0.1, 0.6, 0.7)))
                .with_outline(OutlineStyle::none()),
            Some(unlocked_layer),
        )
        .unwrap();
    let mixed_outer_grp = doc
        .group_objects(&[inner_locked_grp, inner_unlocked_leaf], "MixedOuterGrp")
        .unwrap();

    let mut hist = CommandHistory::default();
    hist.execute(
        &mut doc,
        CommandOp::SetObjectLocked {
            ids: vec![locked_group, locked_pc, inner_locked_grp],
            locked: true,
        },
    )
    .unwrap();

    // --- Locked layer: unlocked leaf + unlocked Group with Table / Dimension / PowerClip ---
    let on_locked_layer_leaf = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LockedLayerLeaf", rect_shape(14.0, 14.0))
                .with_fill(Fill::Solid(Color::rgb(0.85, 0.15, 0.15)))
                .with_outline(OutlineStyle::solid(Color::rgb(0.15, 0.85, 0.15), 0.5)),
            Some(locked_layer),
        )
        .unwrap();

    let mut tbl = VectorTable::new(1, 1, 20.0, 10.0);
    tbl.cells[0].fill = Fill::Solid(Color::rgb(0.3, 0.6, 0.9));
    tbl.cells[0].text_style.fill = Color::rgb(0.9, 0.6, 0.3);
    tbl.border_style = OutlineStyle::solid(Color::rgb(0.2, 0.3, 0.4), 0.5);
    let on_locked_layer_table = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LockedLayerTable", NodePayload::Table(tbl)),
            Some(locked_layer),
        )
        .unwrap();

    let mut dim = DimensionObject::linear(
        DimensionKind::Horizontal,
        Point2D::ZERO,
        Point2D::new(40.0, 0.0),
        6.0,
    );
    dim.text_style.fill = Color::rgb(0.9, 0.1, 0.4);
    let on_locked_layer_dim = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LockedLayerDim", NodePayload::Dimension(dim)),
            Some(locked_layer),
        )
        .unwrap();

    let txt_style = TextStyle {
        fill: Color::rgb(0.25, 0.75, 0.5),
        ..TextStyle::default()
    };
    let on_locked_layer_pc_text = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "LockedLayerPcText",
                NodePayload::ArtisticText(ArtisticText::from_plain("Locked", txt_style)),
            ),
            Some(locked_layer),
        )
        .unwrap();
    let on_locked_layer_pc = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LockedLayerPc", rect_shape(30.0, 30.0))
                .with_fill(Fill::Solid(Color::rgb(0.6, 0.7, 0.8)))
                .with_outline(OutlineStyle::none()),
            Some(locked_layer),
        )
        .unwrap();
    doc.create_powerclip(on_locked_layer_pc, &[on_locked_layer_pc_text])
        .unwrap();
    let on_locked_layer_group = doc
        .group_objects(
            &[
                on_locked_layer_table,
                on_locked_layer_dim,
                on_locked_layer_pc,
            ],
            "LockedLayerGrp",
        )
        .unwrap();
    doc.set_layer_flags(locked_layer, None, Some(true), None, None)
        .unwrap();

    let locked_ids = doc.effective_locked_object_ids();
    for expected_locked in [
        locked_group,
        locked_grp_child,
        locked_grp_nested_pc,
        locked_grp_pc_dim,
        locked_pc,
        locked_pc_child,
        inner_locked_grp,
        inner_locked_leaf,
        on_locked_layer_leaf,
        on_locked_layer_group,
        on_locked_layer_table,
        on_locked_layer_dim,
        on_locked_layer_pc,
        on_locked_layer_pc_text,
    ] {
        assert!(
            locked_ids.contains(&expected_locked),
            "expected {expected_locked:?} to be effectively locked"
        );
    }
    for expected_unlocked in [
        unlocked_leaf,
        unlocked_group,
        unlocked_group_child,
        unlocked_pc,
        unlocked_pc_child,
        mixed_outer_grp,
        inner_unlocked_leaf,
    ] {
        assert!(
            !locked_ids.contains(&expected_unlocked),
            "expected {expected_unlocked:?} to remain effectively unlocked"
        );
    }

    let snapshot_before = doc.clone();
    let mapped = doc.map_unlocked_object_colors(|c| match c {
        Color::Rgba { r, g, b, a } => {
            *c = Color::cmyk(1.0 - *r, 1.0 - *g, 1.0 - *b, 0.0).with_alpha(*a);
            true
        }
        _ => false,
    });
    // Exact count: unlocked_leaf (fill + outline = 2), unlocked_group (OutlineStyle::none() = 0),
    // unlocked_group_child (fill = 1), unlocked_pc (fill = 1), unlocked_pc_child (fill = 1),
    // mixed_outer_grp (OutlineStyle::none() = 0), inner_unlocked_leaf (fill = 1).
    assert_eq!(mapped, 6);

    // Genuinely unlocked objects and unlocked container children converted to CMYK
    assert!(matches!(
        doc.object(unlocked_leaf).unwrap().fill,
        Fill::Solid(Color::Cmyk { .. })
    ));
    assert!(matches!(
        doc.object(unlocked_leaf).unwrap().outline.color,
        Color::Cmyk { .. }
    ));
    assert!(matches!(
        doc.object(unlocked_group_child).unwrap().fill,
        Fill::Solid(Color::Cmyk { .. })
    ));
    assert!(matches!(
        doc.object(unlocked_pc).unwrap().fill,
        Fill::Solid(Color::Cmyk { .. })
    ));
    assert!(matches!(
        doc.object(unlocked_pc_child).unwrap().fill,
        Fill::Solid(Color::Cmyk { .. })
    ));
    assert!(matches!(
        doc.object(inner_unlocked_leaf).unwrap().fill,
        Fill::Solid(Color::Cmyk { .. })
    ));

    // All objects on locked layers or inside locked Group/PowerClip containers remain untouched
    for untouched_id in [
        locked_group,
        locked_grp_child,
        locked_grp_nested_pc,
        locked_grp_pc_dim,
        locked_pc,
        locked_pc_child,
        inner_locked_grp,
        inner_locked_leaf,
        on_locked_layer_leaf,
        on_locked_layer_group,
        on_locked_layer_table,
        on_locked_layer_dim,
        on_locked_layer_pc,
        on_locked_layer_pc_text,
    ] {
        assert_eq!(
            doc.object(untouched_id).unwrap(),
            snapshot_before.object(untouched_id).unwrap(),
            "effectively locked object {untouched_id:?} must not be mutated"
        );
    }

    // Re-running after all unlocked RGB objects are CMYK must convert 0 colors
    let second_pass = doc.map_unlocked_object_colors(|c| match c {
        Color::Rgba { r, g, b, a } => {
            *c = Color::cmyk(1.0 - *r, 1.0 - *g, 1.0 - *b, 0.0).with_alpha(*a);
            true
        }
        _ => false,
    });
    assert_eq!(second_pass, 0);

    // Pure locked-only document (no unlocked objects outside locked layer / locked containers)
    let mut locked_only_doc = Document::new_a4("LockedOnly");
    let lo_page = locked_only_doc.pages()[0].id;
    let lo_unlocked_layer = locked_only_doc.pages()[0].layers[0].id;
    let lo_locked_layer = locked_only_doc
        .add_layer(lo_page, "LockedOnlyLayer", false, MasterLayerScope::None)
        .unwrap();
    locked_only_doc
        .set_layer_flags(lo_locked_layer, None, Some(true), None, None)
        .unwrap();
    let lo_layer_shape = locked_only_doc
        .add_object(
            SceneObject::new(ObjectId(0), "LayerLockedShape", rect_shape(10.0, 10.0))
                .with_fill(Fill::Solid(Color::rgb(1.0, 0.2, 0.2))),
            Some(lo_locked_layer),
        )
        .unwrap();
    let lo_pc_child = locked_only_doc
        .add_object(
            SceneObject::new(ObjectId(0), "LockedPcLeaf", rect_shape(5.0, 5.0))
                .with_fill(Fill::Solid(Color::rgb(0.2, 0.8, 0.4))),
            Some(lo_unlocked_layer),
        )
        .unwrap();
    let lo_pc = locked_only_doc
        .add_object(
            SceneObject::new(ObjectId(0), "LockedPcHost", rect_shape(20.0, 20.0))
                .with_fill(Fill::Solid(Color::rgb(0.4, 0.4, 0.9))),
            Some(lo_unlocked_layer),
        )
        .unwrap();
    locked_only_doc
        .create_powerclip(lo_pc, &[lo_pc_child])
        .unwrap();
    CommandHistory::default()
        .execute(
            &mut locked_only_doc,
            CommandOp::SetObjectLocked {
                ids: vec![lo_pc],
                locked: true,
            },
        )
        .unwrap();
    let lo_before = locked_only_doc.clone();
    let lo_mapped = locked_only_doc.map_unlocked_object_colors(|c| match c {
        Color::Rgba { r, g, b, a } => {
            *c = Color::cmyk(1.0 - *r, 1.0 - *g, 1.0 - *b, 0.0).with_alpha(*a);
            true
        }
        _ => false,
    });
    assert_eq!(lo_mapped, 0);
    assert_eq!(locked_only_doc.objects(), lo_before.objects());
    assert!(locked_only_doc
        .effective_locked_object_ids()
        .contains(&lo_layer_shape));
}

#[test]
fn w1b_r8_effective_locks_find_replace_nan_validation_and_command_matrix() {
    use coree_core::{
        find_and_replace, find_objects, ColorMatch, FindQuery, FindScope, LensConfig, LensKind,
        ObjectStyle, PowerClipFitMode, RasterLayerData, ReplaceAction, SymbolDef, TextMatchMode,
        ZOrderOp,
    };

    let mut doc = Document::new_a4("W1B-R8-Remediation");
    let page_id = doc.pages()[0].id;
    let unlocked_layer = doc.pages()[0].layers[0].id;
    let locked_layer = doc
        .add_layer(page_id, "LockedLayer", false, MasterLayerScope::None)
        .unwrap();

    let mut base_style = ObjectStyle::new("LinkedStyle");
    base_style.fill = Some(Fill::Solid(Color::CYAN));
    let style_id = base_style.id;
    let mut hist = CommandHistory::default();
    hist.execute(&mut doc, CommandOp::UpsertObjectStyle { style: base_style })
        .unwrap();

    let sym = SymbolDef::new(
        "R8Sym",
        vec![SceneObject::new(
            ObjectId(0),
            "SymRect",
            rect_shape(5.0, 5.0),
        )],
    );
    let sym_id = sym.id;
    hist.execute(&mut doc, CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();

    // Unlocked layer objects: unlocked leaf, bystander, locked Group (with unlocked children),
    // locked PowerClip (with unlocked child)
    let unlocked_shape = doc
        .add_object(
            SceneObject::new(ObjectId(0), "UnlockedShape", rect_shape(10.0, 10.0))
                .with_fill(Fill::Solid(Color::CYAN))
                .with_outline(OutlineStyle::hairline(Color::KEY_BLACK))
                .with_transform(Affine2D::translate(0.0, 0.0)),
            Some(unlocked_layer),
        )
        .unwrap();
    let unlocked_text = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "UnlockedText",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "OldLabel",
                    TextStyle {
                        font_family: "Arial".into(),
                        fill: Color::CYAN,
                        ..TextStyle::default()
                    },
                )),
            ),
            Some(unlocked_layer),
        )
        .unwrap();
    let bystander = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Bystander", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(100.0, 0.0)),
            Some(unlocked_layer),
        )
        .unwrap();

    // Locked Group with unlocked Shape + ArtisticText + Path + Table + Raster + Dimension + SymbolInstance
    let lg_shape = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LGShape", rect_shape(10.0, 10.0))
                .with_fill(Fill::Solid(Color::CYAN))
                .with_outline(OutlineStyle::hairline(Color::KEY_BLACK))
                .with_transform(Affine2D::translate(20.0, 0.0)),
            Some(unlocked_layer),
        )
        .unwrap();
    let lg_text = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "LGText",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "OldLabel",
                    TextStyle {
                        font_family: "Arial".into(),
                        fill: Color::CYAN,
                        ..TextStyle::default()
                    },
                )),
            ),
            Some(unlocked_layer),
        )
        .unwrap();
    let lg_path = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "LGPath",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 8.0, 8.0))),
            )
            .with_outline(OutlineStyle::hairline(Color::KEY_BLACK)),
            Some(unlocked_layer),
        )
        .unwrap();
    let lg_table = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "LGTable",
                NodePayload::Table(VectorTable::new(1, 1, 10.0, 10.0)),
            ),
            Some(unlocked_layer),
        )
        .unwrap();
    let asset = RasterAsset::new(2, 2, vec![128u8; 16], 150.0, ColorMode::Rgb).unwrap();
    let asset_id = doc.register_asset(asset);
    let lg_raster = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "LGRaster",
                NodePayload::Raster(RasterLayerData {
                    asset_id,
                    width_mm: 10.0,
                    height_mm: 10.0,
                    filters: Vec::new(),
                    filter_enabled: Vec::new(),
                    crop_rect: None,
                }),
            ),
            Some(unlocked_layer),
        )
        .unwrap();
    let lg_dim = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "LGDim",
                NodePayload::Dimension(DimensionObject::linear(
                    DimensionKind::Horizontal,
                    Point2D::ZERO,
                    Point2D::new(10.0, 0.0),
                    5.0,
                )),
            ),
            Some(unlocked_layer),
        )
        .unwrap();
    let lg_sym = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LGSym", NodePayload::SymbolInstance(sym_id)),
            Some(unlocked_layer),
        )
        .unwrap();
    let lg_inner_group = doc.group_objects(&[lg_sym], "LGInnerGrp").unwrap();
    let locked_group = doc
        .group_objects(
            &[
                lg_shape,
                lg_text,
                lg_path,
                lg_table,
                lg_raster,
                lg_dim,
                lg_inner_group,
            ],
            "LockedGroup",
        )
        .unwrap();

    // Locked PowerClip with unlocked child
    let lpc_child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LPcChild", rect_shape(6.0, 6.0))
                .with_fill(Fill::Solid(Color::CYAN))
                .with_outline(OutlineStyle::hairline(Color::KEY_BLACK)),
            Some(unlocked_layer),
        )
        .unwrap();
    let locked_pc = doc
        .add_object(
            SceneObject::new(ObjectId(0), "LockedPc", rect_shape(20.0, 20.0))
                .with_fill(Fill::Solid(Color::CYAN)),
            Some(unlocked_layer),
        )
        .unwrap();
    doc.create_powerclip(locked_pc, &[lpc_child]).unwrap();

    // Unlocked objects on locked_layer (populate + link style BEFORE locking layer)
    let on_ll_shape = doc
        .add_object(
            SceneObject::new(ObjectId(0), "OnLockedLayerShape", rect_shape(10.0, 10.0))
                .with_fill(Fill::Solid(Color::CYAN))
                .with_transform(Affine2D::translate(50.0, 0.0)),
            Some(locked_layer),
        )
        .unwrap();
    let on_ll_text = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "OnLockedLayerText",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "OldLabel",
                    TextStyle {
                        font_family: "Arial".into(),
                        fill: Color::CYAN,
                        ..TextStyle::default()
                    },
                )),
            ),
            Some(locked_layer),
        )
        .unwrap();
    let on_ll_bystander = doc
        .add_object(
            SceneObject::new(ObjectId(0), "OnLockedLayer2", rect_shape(10.0, 10.0)),
            Some(locked_layer),
        )
        .unwrap();

    // Link style to unlocked_shape, lg_shape, and on_ll_shape BEFORE locking containers/layer
    hist.execute(
        &mut doc,
        CommandOp::SetObjectStyleLink {
            ids: vec![unlocked_shape, lg_shape, on_ll_shape],
            style_id: Some(style_id),
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::AddEffect {
            id: lg_shape,
            effect: Effect::DropShadow {
                dx_mm: 1.0,
                dy_mm: 1.0,
                blur_mm: 2.0,
                opacity: 0.5,
                color: Color::BLACK,
                feather_direction: FeatherDirection::Middle,
            },
        },
    )
    .unwrap();

    // Lock the Group, PowerClip, and locked_layer
    hist.execute(
        &mut doc,
        CommandOp::SetObjectLocked {
            ids: vec![locked_group, locked_pc],
            locked: true,
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::SetLayerFlags {
            layer_id: locked_layer,
            visible: None,
            locked: Some(true),
            printable: None,
            is_cut_layer: None,
        },
    )
    .unwrap();

    // 1. FindAndReplace skips unlocked descendants inside locked Group & locked PowerClip
    //    while find_objects still reports all matches!
    let cyan_q = FindQuery {
        scope: FindScope::WholeDocument,
        fill_color: Some(ColorMatch::exact(Color::CYAN)),
        ..FindQuery::default()
    };
    let all_cyan_matches = find_objects(&doc, &cyan_q);
    assert!(all_cyan_matches.contains(&unlocked_shape));
    assert!(all_cyan_matches.contains(&lg_shape));
    assert!(all_cyan_matches.contains(&lpc_child));
    assert!(all_cyan_matches.contains(&on_ll_shape));

    let pre_fr = doc.clone();
    hist.execute(
        &mut doc,
        CommandOp::FindAndReplace {
            query: cyan_q.clone(),
            action: ReplaceAction::ReplaceFillColor {
                color: Color::MAGENTA,
            },
        },
    )
    .unwrap();
    assert_eq!(
        doc.object(unlocked_shape).unwrap().fill,
        Fill::Solid(Color::MAGENTA)
    );
    assert_eq!(doc.object(unlocked_text).unwrap().fill, Fill::None);
    for protected_id in [lg_shape, lpc_child, locked_pc, on_ll_shape] {
        assert_eq!(
            doc.object(protected_id).unwrap(),
            pre_fr.object(protected_id).unwrap(),
            "FindAndReplace must NOT mutate protected object {protected_id:?}"
        );
    }

    // Also test ReplaceText, ReplaceFontFamily, ReplaceOutlineWidth, ConvertOverprint
    let text_q = FindQuery {
        scope: FindScope::WholeDocument,
        text_pattern: Some((
            "OldLabel".into(),
            TextMatchMode::Contains {
                case_sensitive: true,
            },
        )),
        ..FindQuery::default()
    };
    hist.execute(
        &mut doc,
        CommandOp::FindAndReplace {
            query: text_q,
            action: ReplaceAction::ReplaceText {
                find: "OldLabel".into(),
                replace_with: "NewLabel".into(),
                case_sensitive: true,
            },
        },
    )
    .unwrap();
    if let NodePayload::ArtisticText(ref t) = doc.object(unlocked_text).unwrap().payload {
        assert_eq!(t.plain_text(), "NewLabel");
    } else {
        panic!("expected ArtisticText");
    }
    assert_eq!(
        doc.object(lg_text).unwrap(),
        pre_fr.object(lg_text).unwrap()
    );
    assert_eq!(
        doc.object(on_ll_text).unwrap(),
        pre_fr.object(on_ll_text).unwrap()
    );

    let font_q = FindQuery {
        scope: FindScope::WholeDocument,
        font_family: Some("Arial".into()),
        ..FindQuery::default()
    };
    hist.execute(
        &mut doc,
        CommandOp::FindAndReplace {
            query: font_q,
            action: ReplaceAction::ReplaceFontFamily {
                font_family: "Cairo".into(),
            },
        },
    )
    .unwrap();
    assert_eq!(
        doc.object(lg_text).unwrap(),
        pre_fr.object(lg_text).unwrap()
    );
    assert_eq!(
        doc.object(on_ll_text).unwrap(),
        pre_fr.object(on_ll_text).unwrap()
    );

    let hl_q = FindQuery {
        scope: FindScope::WholeDocument,
        is_hairline: Some(true),
        ..FindQuery::default()
    };
    hist.execute(
        &mut doc,
        CommandOp::FindAndReplace {
            query: hl_q,
            action: ReplaceAction::ReplaceOutlineWidth {
                width_mm: 2.0,
                is_hairline: false,
            },
        },
    )
    .unwrap();
    assert!((doc.object(unlocked_shape).unwrap().outline.width_mm - 2.0).abs() < 1e-6);
    assert_eq!(
        doc.object(lg_path).unwrap(),
        pre_fr.object(lg_path).unwrap()
    );
    assert_eq!(
        doc.object(lpc_child).unwrap(),
        pre_fr.object(lpc_child).unwrap()
    );

    // Direct find_and_replace also preserves matched_ids while skipping locked containers
    let direct_rep = find_and_replace(
        &mut doc,
        &FindQuery {
            scope: FindScope::WholeDocument,
            object_kind: Some(ObjectKindFilter::Shape),
            ..FindQuery::default()
        },
        &ReplaceAction::ConvertOverprint { overprint: true },
    );
    assert!(direct_rep.matched_ids.contains(&lg_shape));
    assert!(direct_rep.matched_ids.contains(&lpc_child));
    assert!(!direct_rep.modified_ids.contains(&lg_shape));
    assert!(!direct_rep.modified_ids.contains(&lpc_child));
    assert!(!direct_rep.modified_ids.contains(&on_ll_shape));
    assert!(doc.object(unlocked_shape).unwrap().outline.overprint);
    assert!(!doc.object(lg_shape).unwrap().outline.overprint);
    assert!(!doc.object(lpc_child).unwrap().outline.overprint);

    // 2. FindAndReplace NaN/Inf/negative width rejection without mutating doc
    let before_bad_fr = doc.clone();
    let bad_actions = [
        ReplaceAction::ReplaceFillColor {
            color: Color::Rgba {
                r: f32::NAN,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        },
        ReplaceAction::ReplaceOutlineColor {
            color: Color::Cmyk {
                c: 0.0,
                m: f32::INFINITY,
                y: 0.0,
                k: 0.0,
                a: 1.0,
            },
        },
        ReplaceAction::ReplaceOutlineWidth {
            width_mm: f64::NAN,
            is_hairline: false,
        },
        ReplaceAction::ReplaceOutlineWidth {
            width_mm: -0.25,
            is_hairline: false,
        },
    ];
    for bad_action in bad_actions {
        let err = hist
            .execute(
                &mut doc,
                CommandOp::FindAndReplace {
                    query: FindQuery::default(),
                    action: bad_action,
                },
            )
            .expect_err("non-finite/negative FindAndReplace action must be rejected");
        assert!(matches!(err, CoreError::InvalidGeometry(_)));
        assert_eq!(doc.objects(), before_bad_fr.objects());
    }
    // Non-finite FindQuery filters rejected cleanly
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::FindAndReplace {
                query: FindQuery {
                    fill_color: Some(ColorMatch::exact(Color::Rgba {
                        r: f32::NAN,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    })),
                    ..FindQuery::default()
                },
                action: ReplaceAction::SelectMatches,
            },
        ),
        Err(CoreError::InvalidGeometry(_))
    ));
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::FindAndReplace {
                query: FindQuery {
                    outline_width_min_mm: Some(f64::NAN),
                    ..FindQuery::default()
                },
                action: ReplaceAction::SelectMatches,
            },
        ),
        Err(CoreError::InvalidGeometry(_))
    ));
    // Hairline=true with negative width_mm is allowed because width_mm is ignored
    hist.execute(
        &mut doc,
        CommandOp::FindAndReplace {
            query: FindQuery {
                object_kind: Some(ObjectKindFilter::Shape),
                ..FindQuery::default()
            },
            action: ReplaceAction::ReplaceOutlineWidth {
                width_mm: -1.0,
                is_hairline: true,
            },
        },
    )
    .unwrap();

    // 3. DeleteObjects atomic rejection on locked layer & locked Group/PowerClip children
    for target in [on_ll_shape, lg_shape, lpc_child] {
        let err = hist
            .execute(
                &mut doc,
                CommandOp::DeleteObjects {
                    ids: vec![bystander, target],
                },
            )
            .expect_err("DeleteObjects on effectively locked target must fail");
        assert!(
            matches!(err, CoreError::ObjectLocked(id) if id == target),
            "expected ObjectLocked({target:?}), got {err:?}"
        );
        assert!(
            doc.has_object(bystander),
            "unlocked bystander must survive failed atomic DeleteObjects"
        );
        assert!(doc.has_object(target));
    }

    // 4. UpsertObjectStyle skips objects on locked layers and inside locked containers
    let pre_style = doc.clone();
    hist.execute(
        &mut doc,
        CommandOp::UpsertObjectStyle {
            style: ObjectStyle {
                id: style_id,
                name: "LinkedStyle".into(),
                fill: Some(Fill::Solid(Color::YELLOW)),
                outline: Some(OutlineStyle::solid(Color::rgb(1.0, 0.0, 0.0), 3.0)),
                blend_mode: Some(BlendMode::Multiply),
                opacity: Some(0.5),
                color_style_id: None,
                color_style_slot: None,
            },
        },
    )
    .unwrap();
    assert_eq!(
        doc.object(unlocked_shape).unwrap().fill,
        Fill::Solid(Color::YELLOW)
    );
    assert_eq!(
        doc.object(lg_shape).unwrap(),
        pre_style.object(lg_shape).unwrap()
    );
    assert_eq!(
        doc.object(on_ll_shape).unwrap(),
        pre_style.object(on_ll_shape).unwrap()
    );

    // 5. AlignObjects & DistributeObjects skip objects on locked layers & inside locked containers
    let ll_bounds_before = doc.world_object_bounds(on_ll_shape).unwrap();
    let lg_bounds_before = doc.world_object_bounds(lg_shape).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::AlignObjects {
            ids: vec![unlocked_shape, on_ll_shape, lg_shape, bystander],
            op: AlignOp::Right,
        },
    )
    .unwrap();
    assert_eq!(
        doc.world_object_bounds(on_ll_shape).unwrap(),
        ll_bounds_before
    );
    assert_eq!(doc.world_object_bounds(lg_shape).unwrap(), lg_bounds_before);
    assert!(
        (doc.world_object_bounds(unlocked_shape).unwrap().max_x
            - doc.world_object_bounds(bystander).unwrap().max_x)
            .abs()
            < 1e-6
    );
    hist.execute(
        &mut doc,
        CommandOp::DistributeObjects {
            ids: vec![unlocked_shape, on_ll_shape, bystander],
            op: DistributeOp::Left,
        },
    )
    .unwrap();
    assert_eq!(
        doc.world_object_bounds(on_ll_shape).unwrap(),
        ll_bounds_before
    );

    // 6. Holistic CommandOp effective-lock audit across ALL direct object-ID mutators
    let pre_holistic = doc.clone();
    let ops_on_locked_targets = vec![
        CommandOp::TransformObjects {
            ids: vec![bystander, on_ll_shape],
            affine: Affine2D::translate(5.0, 5.0),
        },
        CommandOp::TransformObjects {
            ids: vec![lg_shape],
            affine: Affine2D::translate(5.0, 5.0),
        },
        CommandOp::SetFill {
            ids: vec![bystander, lg_shape],
            fill: Fill::Solid(Color::rgb(1.0, 0.0, 0.0)),
        },
        CommandOp::SetOutline {
            ids: vec![on_ll_shape],
            outline: OutlineStyle::solid(Color::rgb(1.0, 0.0, 0.0), 1.0),
        },
        CommandOp::SetLens {
            ids: vec![lg_shape],
            lens: Some(LensConfig::new(LensKind::Invert)),
        },
        CommandOp::AddEffect {
            id: on_ll_shape,
            effect: Effect::DropShadow {
                dx_mm: 1.0,
                dy_mm: 1.0,
                blur_mm: 1.0,
                opacity: 0.5,
                color: Color::BLACK,
                feather_direction: FeatherDirection::Middle,
            },
        },
        CommandOp::RemoveEffect {
            id: lg_shape,
            effect_index: 0,
        },
        CommandOp::EditPath {
            id: lg_path,
            path: BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 2.0, 2.0)),
        },
        CommandOp::EditShape {
            id: on_ll_shape,
            shape: match rect_shape(2.0, 2.0) {
                NodePayload::Shape(s) => s,
                _ => unreachable!(),
            },
        },
        CommandOp::EditText {
            id: lg_text,
            payload: NodePayload::ArtisticText(ArtisticText::from_plain(
                "Hacked",
                TextStyle::default(),
            )),
        },
        CommandOp::EditTable {
            id: lg_table,
            table: VectorTable::new(2, 2, 5.0, 5.0),
        },
        CommandOp::UpdateRaster {
            id: lg_raster,
            data: RasterLayerData {
                asset_id,
                width_mm: 20.0,
                height_mm: 20.0,
                filters: Vec::new(),
                filter_enabled: Vec::new(),
                crop_rect: None,
            },
        },
        CommandOp::EditDimension {
            id: lg_dim,
            dimension: DimensionObject::linear(
                DimensionKind::Vertical,
                Point2D::ZERO,
                Point2D::new(0.0, 20.0),
                4.0,
            ),
        },
        CommandOp::ConvertToCurves { id: on_ll_shape },
        CommandOp::ConvertOutlineToObject { id: lg_shape },
        CommandOp::SetOpacity {
            ids: vec![lg_shape],
            opacity: 0.2,
        },
        CommandOp::SetBlendMode {
            ids: vec![on_ll_shape],
            blend_mode: BlendMode::Multiply,
        },
        CommandOp::SetObjectStyleLink {
            ids: vec![lpc_child],
            style_id: Some(style_id),
        },
        CommandOp::FitPowerClip {
            id: locked_pc,
            mode: PowerClipFitMode::CenterContents,
        },
        CommandOp::RenameObject {
            id: lg_shape,
            new_name: "Renamed".into(),
        },
        CommandOp::SetObjectVisibility {
            ids: vec![on_ll_shape],
            visible: false,
        },
        CommandOp::SetObjectPrintable {
            ids: vec![lg_shape],
            printable: false,
        },
        CommandOp::SetZOrder {
            id: on_ll_shape,
            op: ZOrderOp::BringToFront,
        },
        CommandOp::Group {
            ids: vec![on_ll_shape, on_ll_bystander],
            name: "IllegalGrp".into(),
        },
        CommandOp::Ungroup {
            group_id: lg_inner_group,
        },
        CommandOp::CreatePowerClip {
            container_id: on_ll_shape,
            content_ids: vec![on_ll_bystander],
        },
        CommandOp::ExtractPowerClip {
            powerclip_id: locked_pc,
        },
        CommandOp::SetPowerClipLock {
            id: locked_pc,
            lock_contents: false,
        },
        CommandOp::MoveToDesktopLayer { id: lg_shape },
        CommandOp::UnlinkSymbolInstance { id: lg_sym },
        CommandOp::BooleanOp {
            op: BooleanOp::Weld,
            ids: vec![on_ll_shape, on_ll_bystander],
        },
    ];
    for op in ops_on_locked_targets {
        let label = op.default_label();
        let err = hist
            .execute(&mut doc, op)
            .expect_err(&format!("{label} on effectively locked target must fail"));
        assert!(
            matches!(err, CoreError::ObjectLocked(_)),
            "{label} expected ObjectLocked, got {err:?}"
        );
    }
    assert_eq!(
        doc.objects(),
        pre_holistic.objects(),
        "none of the locked CommandOps may mutate objects"
    );
    assert_eq!(doc.pages(), pre_holistic.pages());

    // ReorderLayerObjects on a locked layer must fail
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::ReorderLayerObjects {
                layer_id: locked_layer,
                object_ids: vec![on_ll_bystander, on_ll_text, on_ll_shape],
            },
        ),
        Err(CoreError::InvalidParameter(_))
    ));

    // SetObjectLocked CAN still unlock locked_pc, and DuplicateObjects CAN still duplicate it
    hist.execute(
        &mut doc,
        CommandOp::DuplicateObjects {
            ids: vec![locked_pc],
            offset: Vec2D::new(5.0, 5.0),
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::SetObjectLocked {
            ids: vec![locked_pc],
            locked: false,
        },
    )
    .unwrap();
    assert!(!doc.object(locked_pc).unwrap().locked);
}

#[test]
fn convert_outline_to_object_powerclip_locks_orphan_cleanup_and_nested_group_siblings() {
    let mut doc = Document::new_a4("ConvertOutline PowerClip & Nested");
    let mut hist = CommandHistory::default();
    let l1 = doc.pages()[0].layers[0].id;
    let p2 = doc.add_page("Page 2", 210.0, 297.0, 3.0);
    let l2 = doc.pages()[1].layers[0].id;
    let _ = p2;

    // 1. Unfilled PowerClip with visible outline containing a locked child must reject ObjectLocked atomically
    let pc_locked_container =
        SceneObject::new(ObjectId(0), "PCLockedContainer", rect_shape(40.0, 40.0))
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::solid(Color::BLACK, 2.0));
    let pc_locked_id = doc.add_object(pc_locked_container, Some(l1)).unwrap();
    let locked_child = SceneObject::new(ObjectId(0), "LockedChild", rect_shape(10.0, 10.0))
        .with_fill(Fill::Solid(Color::CYAN));
    let locked_child_id = doc.add_object(locked_child, Some(l1)).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::CreatePowerClip {
            container_id: pc_locked_id,
            content_ids: vec![locked_child_id],
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::SetObjectLocked {
            ids: vec![locked_child_id],
            locked: true,
        },
    )
    .unwrap();
    let pre_locked_snap = doc.clone();
    let err = hist
        .execute(
            &mut doc,
            CommandOp::ConvertOutlineToObject { id: pc_locked_id },
        )
        .expect_err("unfilled PowerClip with locked child must reject ConvertOutlineToObject");
    assert!(
        matches!(err, CoreError::ObjectLocked(id) if id == locked_child_id),
        "expected ObjectLocked({locked_child_id}), got {err:?}"
    );
    assert_eq!(doc.objects(), pre_locked_snap.objects());
    assert_eq!(doc.pages(), pre_locked_snap.pages());

    // 2. Unfilled PowerClip with unlocked contents converts to Path and removes contents with zero orphans + undo/redo
    let pc_open_container = SceneObject::new(ObjectId(0), "PCOpen", rect_shape(30.0, 20.0))
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::solid(Color::MAGENTA, 2.0));
    let pc_open_id = doc.add_object(pc_open_container, Some(l1)).unwrap();
    let unlocked_child = SceneObject::new(ObjectId(0), "UnlockedChild", rect_shape(8.0, 8.0))
        .with_fill(Fill::Solid(Color::YELLOW));
    let unlocked_child_id = doc.add_object(unlocked_child, Some(l1)).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::CreatePowerClip {
            container_id: pc_open_id,
            content_ids: vec![unlocked_child_id],
        },
    )
    .unwrap();
    let count_before_unfilled = doc.objects().len();
    hist.execute(
        &mut doc,
        CommandOp::ConvertOutlineToObject { id: pc_open_id },
    )
    .unwrap();
    assert!(matches!(
        doc.object(pc_open_id).unwrap().payload,
        NodePayload::Path(_)
    ));
    assert!(!doc.has_object(unlocked_child_id));
    assert_eq!(doc.objects().len(), count_before_unfilled - 1);
    assert_eq!(
        doc.object(pc_open_id).unwrap().fill,
        Fill::Solid(Color::MAGENTA)
    );
    assert!(!doc.object(pc_open_id).unwrap().outline.is_visible());

    hist.undo(&mut doc).unwrap();
    assert!(doc.has_object(unlocked_child_id));
    assert!(matches!(
        doc.object(pc_open_id).unwrap().payload,
        NodePayload::PowerClip { .. }
    ));
    hist.redo(&mut doc).unwrap();
    assert!(!doc.has_object(unlocked_child_id));
    assert!(matches!(
        doc.object(pc_open_id).unwrap().payload,
        NodePayload::Path(_)
    ));

    // 3. Group with visible outline returns explicit InvalidGeometry without destroying children
    let g_child = SceneObject::new(ObjectId(0), "GChild", rect_shape(10.0, 10.0));
    let g_child_id = doc.add_object(g_child, Some(l1)).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![g_child_id],
            name: "GroupNoOutline".into(),
        },
    )
    .unwrap();
    let group_id = doc.parent_container_of(g_child_id).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::SetOutline {
            ids: vec![group_id],
            outline: OutlineStyle::solid(Color::BLACK, 1.5),
        },
    )
    .unwrap();
    let pre_group_snap = doc.clone();
    let g_err = hist
        .execute(&mut doc, CommandOp::ConvertOutlineToObject { id: group_id })
        .expect_err("Group payload cannot be directly converted to an outline object");
    assert!(matches!(g_err, CoreError::InvalidGeometry(_)));
    assert_eq!(doc.objects(), pre_group_snap.objects());

    // 4. Filled object inside a translated + rotated Group on Page 2 (while Page 1 is active)
    //    keeps "<Name> (Outline)" inside the same Group right after source with matching world_object_bounds
    let grouped_fill = SceneObject::new(ObjectId(0), "Badge", rect_shape(10.0, 10.0))
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::solid(Color::BLACK, 2.0));
    let grouped_fill_id = doc.add_object(grouped_fill, Some(l2)).unwrap();
    let sibling_after = SceneObject::new(ObjectId(0), "After", rect_shape(5.0, 5.0))
        .with_transform(Affine2D::translate(20.0, 0.0));
    let sibling_after_id = doc.add_object(sibling_after, Some(l2)).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![grouped_fill_id, sibling_after_id],
            name: "RotatedGroup".into(),
        },
    )
    .unwrap();
    let rot_group_id = doc.parent_container_of(grouped_fill_id).unwrap();
    let group_xf = Affine2D::translate(100.0, 100.0).compose(&Affine2D::rotate_deg(30.0));
    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![rot_group_id],
            affine: group_xf,
        },
    )
    .unwrap();

    // Standalone reference object with identical world transform to verify exact world_object_bounds
    let ref_obj = SceneObject::new(ObjectId(0), "RefBadge", rect_shape(10.0, 10.0))
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::solid(Color::BLACK, 2.0))
        .with_transform(group_xf);
    let ref_id = doc.add_object(ref_obj, Some(l2)).unwrap();
    hist.execute(&mut doc, CommandOp::ConvertOutlineToObject { id: ref_id })
        .unwrap();
    let ref_outline_id = doc.pages()[1].layers[0]
        .objects
        .iter()
        .copied()
        .find(|&oid| {
            doc.object(oid)
                .is_some_and(|o| o.name == "RefBadge (Outline)")
        })
        .unwrap();
    let expected_world_bb = doc.world_object_bounds(ref_outline_id).unwrap();

    hist.execute(
        &mut doc,
        CommandOp::ConvertOutlineToObject {
            id: grouped_fill_id,
        },
    )
    .unwrap();
    let NodePayload::Group(ref group_children) = doc.object(rot_group_id).unwrap().payload else {
        panic!("expected Group");
    };
    assert_eq!(group_children.len(), 3);
    assert_eq!(group_children[0], grouped_fill_id);
    let outline_id = group_children[1];
    assert_eq!(group_children[2], sibling_after_id);
    assert_eq!(doc.parent_container_of(outline_id), Some(rot_group_id));
    assert_eq!(doc.layer_of_object(outline_id), None);
    assert_eq!(doc.object(outline_id).unwrap().name, "Badge (Outline)");
    let actual_world_bb = doc.world_object_bounds(outline_id).unwrap();
    assert!((actual_world_bb.min_x - expected_world_bb.min_x).abs() < 1e-4);
    assert!((actual_world_bb.min_y - expected_world_bb.min_y).abs() < 1e-4);
    assert!((actual_world_bb.max_x - expected_world_bb.max_x).abs() < 1e-4);
    assert!((actual_world_bb.max_y - expected_world_bb.max_y).abs() < 1e-4);

    // Undo / Redo parity for grouped outline conversion
    hist.undo(&mut doc).unwrap();
    assert!(!doc.has_object(outline_id));
    assert!(doc.object(grouped_fill_id).unwrap().outline.is_visible());
    hist.redo(&mut doc).unwrap();
    assert!(doc.has_object(outline_id));
    assert_eq!(doc.parent_container_of(outline_id), Some(rot_group_id));

    // 5. Filled object inside a PowerClip also keeps "<Name> (Outline)" inside the PowerClip
    let pc_host = SceneObject::new(ObjectId(0), "PCHost", rect_shape(50.0, 50.0))
        .with_transform(Affine2D::translate(50.0, 50.0));
    let pc_host_id = doc.add_object(pc_host, Some(l1)).unwrap();
    let pc_inner_filled = SceneObject::new(ObjectId(0), "InnerBadge", rect_shape(12.0, 12.0))
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::solid(Color::BLACK, 2.0))
        .with_transform(Affine2D::translate(55.0, 55.0));
    let pc_inner_id = doc.add_object(pc_inner_filled, Some(l1)).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::CreatePowerClip {
            container_id: pc_host_id,
            content_ids: vec![pc_inner_id],
        },
    )
    .unwrap();
    let inner_bb_before = doc.world_object_bounds(pc_inner_id).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::ConvertOutlineToObject { id: pc_inner_id },
    )
    .unwrap();
    let NodePayload::PowerClip { ref contents, .. } = doc.object(pc_host_id).unwrap().payload
    else {
        panic!("expected PowerClip");
    };
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0], pc_inner_id);
    let pc_outline_id = contents[1];
    assert_eq!(doc.parent_container_of(pc_outline_id), Some(pc_host_id));
    assert_eq!(doc.layer_of_object(pc_outline_id), None);
    let pc_outline_bb = doc.world_object_bounds(pc_outline_id).unwrap();
    assert!((pc_outline_bb.min_x - (inner_bb_before.min_x - 1.0)).abs() < 0.15);
    assert!((pc_outline_bb.max_x - (inner_bb_before.max_x + 1.0)).abs() < 0.15);
    assert!((pc_outline_bb.min_y - (inner_bb_before.min_y - 1.0)).abs() < 0.15);
    assert!((pc_outline_bb.max_y - (inner_bb_before.max_y + 1.0)).abs() < 0.15);
}

#[test]
fn w1b_r11_command_op_finiteness_domain_guards_and_rollback() {
    let mut doc = Document::new_a4("Finiteness Guards");
    let mut hist = CommandHistory::default();
    let layer = doc.pages()[0].layers[0].id;
    let rect = SceneObject::new(ObjectId(0), "ValidRect", rect_shape(20.0, 10.0))
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::solid(Color::BLACK, 1.0));
    hist.execute(&mut doc, CommandOp::create_object(rect, Some(layer)))
        .unwrap();
    let rect_id = doc.pages()[0].layers[0].objects[0];
    hist.execute(
        &mut doc,
        CommandOp::AddGuide {
            orientation: GuideOrientation::Vertical(25.0),
            locked: false,
        },
    )
    .unwrap();
    let guide_id = doc.guides()[0].id;

    let pre_snap = doc.clone();
    let pre_log_len = hist.command_log().len();

    let mut bad_xform_obj = SceneObject::new(ObjectId(0), "BadXf", rect_shape(10.0, 10.0));
    bad_xform_obj.transform = Affine2D::translate(f64::NAN, 0.0);

    let mut bad_fill_obj = SceneObject::new(ObjectId(0), "BadFill", rect_shape(10.0, 10.0));
    bad_fill_obj.fill = Fill::Solid(Color::Rgba {
        r: f32::NAN,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    });

    let mut bad_outline_obj = SceneObject::new(ObjectId(0), "BadOutl", rect_shape(10.0, 10.0));
    bad_outline_obj.outline = OutlineStyle {
        width_mm: -1.0,
        ..OutlineStyle::solid(Color::BLACK, 1.0)
    };

    let mut bad_miter_obj = SceneObject::new(ObjectId(0), "BadMiter", rect_shape(10.0, 10.0));
    bad_miter_obj.outline.miter_limit = -0.5;

    let mut bad_style_width = ObjectStyle::new("BadWidth");
    bad_style_width.outline = Some(OutlineStyle {
        width_mm: -2.0,
        ..OutlineStyle::solid(Color::BLACK, 1.0)
    });

    let mut bad_style_opacity = ObjectStyle::new("BadOpacity");
    bad_style_opacity.opacity = Some(1.5);

    let mut bad_style_fill = ObjectStyle::new("BadStyleFill");
    bad_style_fill.fill = Some(Fill::Solid(Color::Rgba {
        r: f32::INFINITY,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    }));

    let mut bad_color_style = ColorStyle::new("BadColorStyle", Color::CYAN, None);
    bad_color_style.base_color = Color::Rgba {
        r: f32::NAN,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    let mut bad_derived_color_style = ColorStyle::new("BadDerived", Color::CYAN, None);
    bad_derived_color_style.derived_colors = vec![Color::Rgba {
        r: 0.0,
        g: f32::INFINITY,
        b: 0.0,
        a: 1.0,
    }];

    let bad_sym = SymbolDef::new("BadSym", vec![bad_xform_obj.clone()]);

    let invalid_ops = vec![
        CommandOp::create_object(bad_xform_obj, Some(layer)),
        CommandOp::create_object(bad_fill_obj, Some(layer)),
        CommandOp::create_object(bad_outline_obj, Some(layer)),
        CommandOp::create_object(bad_miter_obj, Some(layer)),
        CommandOp::SetFill {
            ids: vec![rect_id],
            fill: Fill::Solid(Color::Rgba {
                r: 0.0,
                g: f32::INFINITY,
                b: 0.0,
                a: 1.0,
            }),
        },
        CommandOp::SetOutline {
            ids: vec![rect_id],
            outline: OutlineStyle {
                width_mm: f64::NAN,
                ..OutlineStyle::solid(Color::BLACK, 1.0)
            },
        },
        CommandOp::SetOutline {
            ids: vec![rect_id],
            outline: OutlineStyle {
                width_mm: -5.0,
                ..OutlineStyle::solid(Color::BLACK, 1.0)
            },
        },
        CommandOp::SetOutline {
            ids: vec![rect_id],
            outline: {
                let mut o = OutlineStyle::solid(Color::BLACK, 1.0);
                o.miter_limit = -1.0;
                o
            },
        },
        CommandOp::SetLens {
            ids: vec![rect_id],
            lens: Some(LensConfig {
                kind: LensKind::Brighten { rate_pct: f32::NAN },
                frozen: false,
                viewpoint: None,
                remove_face: false,
                frozen_snapshot: None,
            }),
        },
        CommandOp::AddEffect {
            id: rect_id,
            effect: Effect::DropShadow {
                dx_mm: 2.0,
                dy_mm: 2.0,
                blur_mm: f64::NAN,
                opacity: 0.5,
                color: Color::BLACK,
                feather_direction: FeatherDirection::Middle,
            },
        },
        CommandOp::AddPage {
            name: "BadNeg".into(),
            width_mm: -10.0,
            height_mm: 297.0,
            bleed_mm: 3.0,
        },
        CommandOp::AddPage {
            name: "BadBleed".into(),
            width_mm: 210.0,
            height_mm: 297.0,
            bleed_mm: -2.0,
        },
        CommandOp::AddPage {
            name: "BadNaN".into(),
            width_mm: f64::NAN,
            height_mm: 297.0,
            bleed_mm: 3.0,
        },
        CommandOp::AddGuide {
            orientation: GuideOrientation::Horizontal(f64::NAN),
            locked: false,
        },
        CommandOp::AddGuide {
            orientation: GuideOrientation::Angled {
                origin: Point2D::new(f64::INFINITY, 0.0),
                angle_deg: 45.0,
            },
            locked: false,
        },
        CommandOp::MoveGuide {
            id: guide_id,
            orientation: GuideOrientation::Vertical(f64::NEG_INFINITY),
        },
        CommandOp::UpsertColorStyle {
            style: bad_color_style,
        },
        CommandOp::UpsertColorStyle {
            style: bad_derived_color_style,
        },
        CommandOp::UpsertObjectStyle {
            style: bad_style_width,
        },
        CommandOp::UpsertObjectStyle {
            style: bad_style_opacity,
        },
        CommandOp::UpsertObjectStyle {
            style: bad_style_fill,
        },
        CommandOp::UpsertSymbol { symbol: bad_sym },
        CommandOp::FindAndReplace {
            query: FindQuery::default(),
            action: ReplaceAction::ReplaceOutlineWidth {
                width_mm: f64::NAN,
                is_hairline: true,
            },
        },
        CommandOp::FindAndReplace {
            query: FindQuery::default(),
            action: ReplaceAction::ReplaceOutlineWidth {
                width_mm: f64::INFINITY,
                is_hairline: true,
            },
        },
        CommandOp::FindAndReplace {
            query: FindQuery::default(),
            action: ReplaceAction::ReplaceOutlineWidth {
                width_mm: -1.0,
                is_hairline: false,
            },
        },
    ];

    for op in invalid_ops {
        let label = op.default_label();
        let err = hist
            .execute(&mut doc, op)
            .expect_err(&format!("{label} with invalid/non-finite value must fail"));
        assert!(
            matches!(
                err,
                CoreError::InvalidGeometry(_) | CoreError::InvalidParameter(_)
            ),
            "{label} returned unexpected error: {err:?}"
        );
    }

    assert_eq!(doc.objects(), pre_snap.objects());
    assert_eq!(doc.pages(), pre_snap.pages());
    assert_eq!(doc.guides(), pre_snap.guides());
    assert_eq!(doc.color_styles(), pre_snap.color_styles());
    assert_eq!(doc.object_styles(), pre_snap.object_styles());
    assert_eq!(doc.symbols(), pre_snap.symbols());
    assert_eq!(hist.command_log().len(), pre_log_len);

    // JSON log must still export and import without NaN -> null corruption
    let json = hist.export_log_json().unwrap();
    let parsed = CommandHistory::import_log_json(&json).unwrap();
    assert_eq!(parsed.len(), pre_log_len);
}

#[test]
fn w1b_r11_locked_descendant_guards_across_containers_align_distribute() {
    let mut doc = Document::new_a4("Locked Descendants");
    let mut hist = CommandHistory::default();
    let layer = doc.pages()[0].layers[0].id;

    // 1. Unlocked PowerClip (lock_contents: true) containing a locked child
    let pc_host = doc
        .add_object(
            SceneObject::new(ObjectId(0), "PCHost", rect_shape(40.0, 40.0)),
            Some(layer),
        )
        .unwrap();
    let pc_child = SceneObject::new(ObjectId(0), "PCLockedChild", rect_shape(10.0, 10.0))
        .with_transform(Affine2D::translate(10.0, 10.0));
    let pc_child_id = doc.add_object(pc_child, Some(layer)).unwrap();
    doc.create_powerclip(pc_host, &[pc_child_id]).unwrap();
    apply_command(
        &mut doc,
        &CommandOp::SetObjectLocked {
            ids: vec![pc_child_id],
            locked: true,
        },
    )
    .unwrap();

    // 2. Unlocked Group containing a locked child
    let grp_a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "GrpUnlockedChild", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(30.0, 0.0)),
            Some(layer),
        )
        .unwrap();
    let mut grp_b = SceneObject::new(ObjectId(0), "GrpLockedChild", rect_shape(10.0, 10.0))
        .with_transform(Affine2D::translate(50.0, 0.0));
    grp_b.locked = true;
    let grp_b_id = doc.add_object(grp_b, Some(layer)).unwrap();
    let grp_id = doc
        .group_objects(&[grp_a, grp_b_id], "LockedChildGrp")
        .unwrap();

    // 3. Unlocked sibling & bystander for align/distribute
    let free_a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "FreeA", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(5.0, 0.0)),
            Some(layer),
        )
        .unwrap();
    let free_b = doc
        .add_object(
            SceneObject::new(ObjectId(0), "FreeB", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(100.0, 0.0)),
            Some(layer),
        )
        .unwrap();

    let pre = doc.clone();

    // FitPowerClip, ExtractPowerClip, Ungroup, TransformObjects must reject ObjectLocked(locked_child)
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::FitPowerClip {
                id: pc_host,
                mode: PowerClipFitMode::CenterContents,
            },
        ),
        Err(CoreError::ObjectLocked(id)) if id == pc_child_id
    ));
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::ExtractPowerClip {
                powerclip_id: pc_host,
            },
        ),
        Err(CoreError::ObjectLocked(id)) if id == pc_child_id
    ));
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::TransformObjects {
                ids: vec![pc_host],
                affine: Affine2D::translate(10.0, 0.0),
            },
        ),
        Err(CoreError::ObjectLocked(id)) if id == pc_child_id
    ));
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::Ungroup { group_id: grp_id }),
        Err(CoreError::ObjectLocked(id)) if id == grp_b_id
    ));
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::TransformObjects {
                ids: vec![grp_id],
                affine: Affine2D::translate(10.0, 0.0),
            },
        ),
        Err(CoreError::ObjectLocked(id)) if id == grp_b_id
    ));
    assert_eq!(doc.objects(), pre.objects());

    // Direct doc methods also reject
    assert!(matches!(
        doc.fit_powerclip(pc_host, PowerClipFitMode::FitContents),
        Err(CoreError::ObjectLocked(id)) if id == pc_child_id
    ));
    assert!(matches!(
        doc.extract_powerclip(pc_host),
        Err(CoreError::ObjectLocked(id)) if id == pc_child_id
    ));
    assert!(matches!(
        doc.ungroup_object(grp_id),
        Err(CoreError::ObjectLocked(id)) if id == grp_b_id
    ));
    assert!(matches!(
        doc.transform_object(grp_id, Affine2D::translate(5.0, 0.0)),
        Err(CoreError::ObjectLocked(id)) if id == grp_b_id
    ));

    // PowerClip with lock_contents == false keeps locked child world bounds stationary when container moves,
    // and rejects singular transform that cannot keep child stationary
    doc.set_powerclip_lock_contents(pc_host, false).unwrap();
    let child_world_before = doc.world_object_bounds(pc_child_id).unwrap();
    assert!(matches!(
        doc.transform_object(pc_host, Affine2D::scale(0.0, 0.0)),
        Err(CoreError::ObjectLocked(id)) if id == pc_child_id
    ));
    doc.transform_object(pc_host, Affine2D::translate(20.0, 15.0))
        .unwrap();
    let child_world_after = doc.world_object_bounds(pc_child_id).unwrap();
    assert!((child_world_after.min_x - child_world_before.min_x).abs() < 1e-6);
    assert!((child_world_after.min_y - child_world_before.min_y).abs() < 1e-6);
    doc.set_powerclip_lock_contents(pc_host, true).unwrap();

    // AlignObjects and DistributeObjects skip Group/PowerClip containing locked child
    let grp_bb_before = doc.world_object_bounds(grp_id).unwrap();
    let pc_bb_before = doc.world_object_bounds(pc_host).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::AlignObjects {
            ids: vec![free_a, grp_id, pc_host, free_b],
            op: AlignOp::Right,
        },
    )
    .unwrap();
    assert_eq!(doc.world_object_bounds(grp_id).unwrap(), grp_bb_before);
    assert_eq!(doc.world_object_bounds(pc_host).unwrap(), pc_bb_before);
    assert!(
        (doc.world_object_bounds(free_a).unwrap().max_x
            - doc.world_object_bounds(free_b).unwrap().max_x)
            .abs()
            < 1e-6
    );

    hist.execute(
        &mut doc,
        CommandOp::DistributeObjects {
            ids: vec![free_a, grp_id, free_b],
            op: DistributeOp::Left,
        },
    )
    .unwrap();
    assert_eq!(doc.world_object_bounds(grp_id).unwrap(), grp_bb_before);
}

#[test]
fn w1b_r11_convert_outline_to_object_world_scale_invariance() {
    use crate::dimension::{AnchorPoint, ConnectorObject, ConnectorStyle};

    let mut doc = Document::new_a4("ConvertOutline World Scale");
    let layer = doc.pages()[0].layers[0].id;

    // 1. Unfilled 10x10 rect (width_mm = 2.0, scale_with_object = false) inside 4x scaled Group
    //    -> world bbox width must be 42.0 mm (40 + 2*1.0 half-stroke), NOT 48.0 mm!
    let mut o1 = SceneObject::new(ObjectId(0), "UnfilledNoScale", rect_shape(10.0, 10.0))
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::solid(Color::BLACK, 2.0));
    o1.outline.scale_with_object = false;
    let o1_id = doc.add_object(o1, Some(layer)).unwrap();
    let g1 = doc.group_objects(&[o1_id], "Scale4x").unwrap();
    doc.transform_object(g1, Affine2D::scale(4.0, 4.0)).unwrap();

    assert!(doc.convert_outline_to_object(o1_id).unwrap().is_none());
    let bb1 = doc.world_object_bounds(o1_id).unwrap();
    assert!(
        (bb1.width() - 42.0).abs() < 0.1 && (bb1.height() - 42.0).abs() < 0.1,
        "expected 42x42 world bbox for scale_with_object=false in 4x Group, got {bb1:?}"
    );

    // 2. Filled 10x10 rect (width_mm = 2.0, scale_with_object = false) inside anisotropic (5x, 1x) Group
    //    -> uniform 1mm half-stroke in world space => 52.0 x 12.0 mm!
    let mut o2 = SceneObject::new(ObjectId(0), "FilledAniso", rect_shape(10.0, 10.0))
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::solid(Color::BLACK, 2.0));
    o2.outline.scale_with_object = false;
    let o2_id = doc.add_object(o2, Some(layer)).unwrap();
    let g2 = doc.group_objects(&[o2_id], "Aniso5x1").unwrap();
    doc.transform_object(g2, Affine2D::scale(5.0, 1.0)).unwrap();

    let o2_outline_id = doc.convert_outline_to_object(o2_id).unwrap().unwrap();
    let bb2 = doc.world_object_bounds(o2_outline_id).unwrap();
    assert!(
        (bb2.width() - 52.0).abs() < 0.1 && (bb2.height() - 12.0).abs() < 0.1,
        "expected 52x12 world bbox in (5x,1x) Group, got {bb2:?}"
    );

    // 3. Filled 10x10 rect with scale_with_object = true inside 4x scaled Group
    //    -> 4x * 2mm = 8mm stroke => 48.0 x 48.0 mm!
    let mut o3 = SceneObject::new(ObjectId(0), "FilledScaled", rect_shape(10.0, 10.0))
        .with_fill(Fill::Solid(Color::MAGENTA))
        .with_outline(OutlineStyle::solid(Color::BLACK, 2.0));
    o3.outline.scale_with_object = true;
    let o3_id = doc.add_object(o3, Some(layer)).unwrap();
    let g3 = doc.group_objects(&[o3_id], "Scale4xTrue").unwrap();
    doc.transform_object(g3, Affine2D::scale(4.0, 4.0)).unwrap();

    let o3_outline_id = doc.convert_outline_to_object(o3_id).unwrap().unwrap();
    let bb3 = doc.world_object_bounds(o3_outline_id).unwrap();
    assert!(
        (bb3.width() - 48.0).abs() < 0.2 && (bb3.height() - 48.0).abs() < 0.2,
        "expected 48x48 world bbox for scale_with_object=true in 4x Group, got {bb3:?}"
    );

    // 4. PowerClip scaled 4x containing an unfilled rect with scale_with_object = false -> 42.0 mm
    let pc_host = doc
        .add_object(
            SceneObject::new(ObjectId(0), "PCHost", rect_shape(30.0, 30.0)),
            Some(layer),
        )
        .unwrap();
    let mut pc_child = SceneObject::new(ObjectId(0), "PCChild", rect_shape(10.0, 10.0))
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::solid(Color::BLACK, 2.0));
    pc_child.outline.scale_with_object = false;
    let pc_child_id = doc.add_object(pc_child, Some(layer)).unwrap();
    doc.create_powerclip(pc_host, &[pc_child_id]).unwrap();
    doc.transform_object(pc_host, Affine2D::scale(4.0, 4.0))
        .unwrap();

    assert!(doc
        .convert_outline_to_object(pc_child_id)
        .unwrap()
        .is_none());
    let bb4 = doc.world_object_bounds(pc_child_id).unwrap();
    assert!(
        (bb4.width() - 42.0).abs() < 0.1 && (bb4.height() - 42.0).abs() < 0.1,
        "expected 42x42 world bbox inside 4x PowerClip, got {bb4:?}"
    );

    // 5. Connector inside 4x scaled Group with scale_with_object = false
    let mut conn_obj = SceneObject::new(
        ObjectId(0),
        "GroupedConn",
        NodePayload::Connector(ConnectorObject {
            style: ConnectorStyle::Straight,
            from_obj: None,
            to_obj: None,
            from_anchor: AnchorPoint::Center,
            to_anchor: AnchorPoint::Center,
            from_fallback: Point2D::new(0.0, 0.0),
            to_fallback: Point2D::new(10.0, 0.0),
            corner_radius_mm: 0.0,
        }),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::solid(Color::BLACK, 2.0));
    conn_obj.outline.scale_with_object = false;
    let conn_id = doc.add_object(conn_obj, Some(layer)).unwrap();
    let g_conn = doc.group_objects(&[conn_id], "ConnScale4x").unwrap();
    doc.transform_object(g_conn, Affine2D::scale(4.0, 4.0))
        .unwrap();

    assert!(doc.convert_outline_to_object(conn_id).unwrap().is_none());
    let bb_conn = doc.world_object_bounds(conn_id).unwrap();
    assert!(
        (bb_conn.height() - 2.0).abs() < 0.1,
        "expected 2.0mm world stroke thickness on horizontal Connector in 4x Group, got {bb_conn:?}"
    );
}

#[test]
fn w1b_r11_duplicate_objects_nested_page_placement() {
    let mut doc = Document::new_a4("Dup Nested Page");
    let _p2 = doc.add_page("Page 2", 210.0, 297.0, 3.0);
    let p1_l = doc.pages()[0].layers[0].id;
    let p2_l = doc.pages()[1].layers[0].id;

    let child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "P2Child", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(20.0, 30.0)),
            Some(p2_l),
        )
        .unwrap();
    let _grp = doc.group_objects(&[child], "P2Group").unwrap();

    // Active page is Page 1 (index 0); duplicating child inside Page 2 Group must land on Page 2!
    assert_eq!(doc.active_page_index(), 0);
    let dups = doc
        .duplicate_objects(&[child], Vec2D::new(5.0, 5.0))
        .unwrap();
    assert_eq!(dups.len(), 1);
    let dup_id = dups[0];
    assert_eq!(
        doc.layer_of_object(dup_id),
        Some(p2_l),
        "duplicated nested child from Page 2 must be placed on Page 2, not Page 1"
    );
    assert!(!doc.pages()[0].layers[0].objects.contains(&dup_id));
    let _ = p1_l;
}

#[test]
fn w1b_r12_create_powerclip_move_to_desktop_locked_descendants_and_create_object_layer_guards() {
    let mut doc = Document::new_a4("R12 Container & Layer Guards");
    let mut hist = CommandHistory::default();
    let p1 = doc.pages()[0].id;
    let p1_l1 = doc.pages()[0].layers[0].id;

    // 1. Unlocked PowerClip & unlocked Group each containing a locked descendant
    let pc_host = doc
        .add_object(
            SceneObject::new(ObjectId(0), "PCHost", rect_shape(40.0, 40.0)),
            Some(p1_l1),
        )
        .unwrap();
    let pc_locked_child = SceneObject::new(ObjectId(0), "PCLocked", rect_shape(10.0, 10.0));
    let pc_locked_id = doc.add_object(pc_locked_child, Some(p1_l1)).unwrap();
    doc.create_powerclip(pc_host, &[pc_locked_id]).unwrap();
    apply_command(
        &mut doc,
        &CommandOp::SetObjectLocked {
            ids: vec![pc_locked_id],
            locked: true,
        },
    )
    .unwrap();

    let grp_a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "GrpFree", rect_shape(10.0, 10.0)),
            Some(p1_l1),
        )
        .unwrap();
    let mut grp_b = SceneObject::new(ObjectId(0), "GrpLocked", rect_shape(10.0, 10.0));
    grp_b.locked = true;
    let grp_b_id = doc.add_object(grp_b, Some(p1_l1)).unwrap();
    let grp_id = doc.group_objects(&[grp_a, grp_b_id], "LockedGrp").unwrap();

    let outer_container = doc
        .add_object(
            SceneObject::new(ObjectId(0), "OuterClip", rect_shape(80.0, 80.0)),
            Some(p1_l1),
        )
        .unwrap();
    let extra_free = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ExtraFree", rect_shape(5.0, 5.0)),
            Some(p1_l1),
        )
        .unwrap();

    let pre = doc.clone();

    // CreatePowerClip must reject unlocked Group with locked child as content_ids (both command & direct)
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::CreatePowerClip {
                container_id: outer_container,
                content_ids: vec![grp_id],
            },
        ),
        Err(CoreError::ObjectLocked(id)) if id == grp_b_id
    ));
    assert!(matches!(
        doc.create_powerclip(outer_container, &[grp_id]),
        Err(CoreError::ObjectLocked(id)) if id == grp_b_id
    ));

    // CreatePowerClip must also reject existing PowerClip with locked child as container_id
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::CreatePowerClip {
                container_id: pc_host,
                content_ids: vec![extra_free],
            },
        ),
        Err(CoreError::ObjectLocked(id)) if id == pc_locked_id
    ));
    assert!(matches!(
        doc.create_powerclip(pc_host, &[extra_free]),
        Err(CoreError::ObjectLocked(id)) if id == pc_locked_id
    ));

    // MoveToDesktopLayer must reject both Group and PowerClip containing locked descendants
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::MoveToDesktopLayer { id: grp_id }),
        Err(CoreError::ObjectLocked(id)) if id == grp_b_id
    ));
    assert!(matches!(
        doc.move_object_to_desktop_layer(grp_id),
        Err(CoreError::ObjectLocked(id)) if id == grp_b_id
    ));
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::MoveToDesktopLayer { id: pc_host }),
        Err(CoreError::ObjectLocked(id)) if id == pc_locked_id
    ));
    assert!(matches!(
        doc.move_object_to_desktop_layer(pc_host),
        Err(CoreError::ObjectLocked(id)) if id == pc_locked_id
    ));
    assert_eq!(doc.objects(), pre.objects());
    assert_eq!(doc.pages(), pre.pages());

    // 2. Document::add_object(..., None) prefers visible && !locked over topmost hidden && !locked
    let hidden_top = doc
        .add_layer(p1, "HiddenTop", false, MasterLayerScope::None)
        .unwrap();
    doc.set_layer_flags(hidden_top, Some(false), Some(false), None, None)
        .unwrap();
    let visible_obj = doc
        .add_object(
            SceneObject::new(ObjectId(0), "VisibleLand", rect_shape(10.0, 10.0)),
            None,
        )
        .unwrap();
    assert_eq!(doc.layer_of_object(visible_obj), Some(p1_l1));

    // 3. CommandOp::CreateObject(..., None) rejects when all layers on active page are locked
    doc.set_layer_flags(p1_l1, None, Some(true), None, None)
        .unwrap();
    doc.set_layer_flags(hidden_top, None, Some(true), None, None)
        .unwrap();
    let count_before = doc.objects().len();
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::create_object(
                SceneObject::new(ObjectId(0), "BlockedCreate", rect_shape(5.0, 5.0)),
                None,
            ),
        ),
        Err(CoreError::ObjectLocked(_))
    ));
    assert_eq!(doc.objects().len(), count_before);

    // 4. find_objects respects MasterLayerScope::EvenPages on Page 1 vs Page 2
    doc.set_layer_flags(p1_l1, Some(true), Some(false), None, None)
        .unwrap();
    let _p2 = doc.add_page("Page 2", 210.0, 297.0, 3.0);
    let even_layer = doc
        .add_layer(p1, "EvenMaster", false, MasterLayerScope::EvenPages)
        .unwrap();
    let even_rect = doc
        .add_object(
            SceneObject::new(ObjectId(0), "EvenMasterRect", rect_shape(15.0, 15.0)),
            Some(even_layer),
        )
        .unwrap();
    let p1_matches = find_objects(
        &doc,
        &FindQuery {
            scope: FindScope::CurrentPage,
            ..FindQuery::default()
        },
    );
    assert!(!p1_matches.contains(&even_rect));
    doc.set_active_page_index(1).unwrap();
    let p2_matches = find_objects(
        &doc,
        &FindQuery {
            scope: FindScope::CurrentPage,
            ..FindQuery::default()
        },
    );
    assert!(p2_matches.contains(&even_rect));

    // 5. find_and_replace rejects non-finite width even when is_hairline == true
    let pre_fr = doc.objects().clone();
    let rep = find_and_replace(
        &mut doc,
        &FindQuery::default(),
        &ReplaceAction::ReplaceOutlineWidth {
            width_mm: f64::NAN,
            is_hairline: true,
        },
    );
    assert_eq!(rep.replacements_applied, 0);
    assert_eq!(doc.objects(), &pre_fr);
}

#[test]
fn w1b_r13_create_symbol_duplicate_locked_layer_and_group_ancestor_page_preservation() {
    let mut doc = Document::new_a4("R13 Layer Lock & Group Ancestry");
    let mut hist = CommandHistory::default();
    let p1 = doc.pages()[0].id;
    let p1_l1 = doc.pages()[0].layers[0].id;

    let sym_id = SymbolId::new();
    let sym = SymbolDef {
        id: sym_id,
        name: "R13Sym".into(),
        objects: vec![SceneObject::new(
            ObjectId(1),
            "SymRect",
            rect_shape(10.0, 10.0),
        )],
    };
    hist.execute(&mut doc, CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();

    // Populate unlocked layer with a normal object, a nested container, and an individually locked object
    let on_l1 = doc
        .add_object(
            SceneObject::new(ObjectId(0), "OnLockedLayer", rect_shape(15.0, 15.0)),
            Some(p1_l1),
        )
        .unwrap();
    let nested_child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "NestedLockedLayer", rect_shape(8.0, 8.0)),
            Some(p1_l1),
        )
        .unwrap();
    let _locked_layer_grp = doc
        .group_objects(&[nested_child], "LockedLayerGrp")
        .unwrap();

    let unlocked_l2 = doc
        .add_layer(p1, "UnlockedLayer2", false, MasterLayerScope::None)
        .unwrap();
    let mut indiv_obj = SceneObject::new(ObjectId(0), "IndivLocked", rect_shape(12.0, 12.0));
    indiv_obj.locked = true;
    let indiv_locked_on_unlocked = doc.add_object(indiv_obj, Some(unlocked_l2)).unwrap();

    // Lock Layer 1 and Layer 2 temporarily to verify CreateObject(Some) & InstantiateSymbol(None/Some)
    doc.set_layer_flags(p1_l1, None, Some(true), None, None)
        .unwrap();
    doc.set_layer_flags(unlocked_l2, None, Some(true), None, None)
        .unwrap();
    let snapshot_all_locked = doc.clone();

    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::create_object(
                SceneObject::new(ObjectId(0), "ExplicitLockedCreate", rect_shape(5.0, 5.0)),
                Some(p1_l1),
            ),
        ),
        Err(CoreError::ObjectLocked(_))
    ));
    assert_eq!(doc.objects(), snapshot_all_locked.objects());
    assert_eq!(doc.pages(), snapshot_all_locked.pages());

    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::InstantiateSymbol {
                symbol_id: sym_id,
                layer_id: Some(p1_l1),
                transform: Affine2D::IDENTITY,
                name: None,
            },
        ),
        Err(CoreError::ObjectLocked(_))
    ));
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::InstantiateSymbol {
                symbol_id: sym_id,
                layer_id: None,
                transform: Affine2D::IDENTITY,
                name: None,
            },
        ),
        Err(CoreError::ObjectLocked(_))
    ));
    assert_eq!(doc.objects(), snapshot_all_locked.objects());
    assert_eq!(doc.pages(), snapshot_all_locked.pages());

    // DuplicateObjects must reject objects on locked layers (both direct and nested inside containers)
    assert!(matches!(
        doc.duplicate_objects(&[on_l1], Vec2D::new(5.0, 5.0)),
        Err(CoreError::ObjectLocked(_))
    ));
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::DuplicateObjects {
                ids: vec![on_l1],
                offset: Vec2D::new(5.0, 5.0),
            },
        ),
        Err(CoreError::ObjectLocked(_))
    ));
    assert!(matches!(
        hist.execute(
            &mut doc,
            CommandOp::DuplicateObjects {
                ids: vec![nested_child],
                offset: Vec2D::new(5.0, 5.0),
            },
        ),
        Err(CoreError::ObjectLocked(_))
    ));
    assert_eq!(doc.objects(), snapshot_all_locked.objects());
    assert_eq!(doc.pages(), snapshot_all_locked.pages());

    // Unlock Layer 2: individually locked object on unlocked Layer 2 still duplicates, and InstantiateSymbol(None) lands on Layer 2
    doc.set_layer_flags(unlocked_l2, None, Some(false), None, None)
        .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::DuplicateObjects {
            ids: vec![indiv_locked_on_unlocked],
            offset: Vec2D::new(3.0, 3.0),
        },
    )
    .unwrap();
    let dup_id = *doc.objects().keys().last().unwrap();
    assert_eq!(doc.layer_of_object(dup_id), Some(unlocked_l2));

    hist.execute(
        &mut doc,
        CommandOp::InstantiateSymbol {
            symbol_id: sym_id,
            layer_id: None,
            transform: Affine2D::translate(10.0, 10.0),
            name: None,
        },
    )
    .unwrap();
    let inst_id = *doc.objects().keys().last().unwrap();
    assert_eq!(doc.layer_of_object(inst_id), Some(unlocked_l2));
    hist.undo(&mut doc).unwrap();
    assert!(!doc.has_object(inst_id));
    hist.redo(&mut doc).unwrap();
    assert_eq!(doc.layer_of_object(inst_id), Some(unlocked_l2));

    // Group([child, parent_group]) on Page 2 while Page 1 is active must preserve Page 2 placement and world bounds
    let _p2 = doc.add_page("Page 2", 210.0, 297.0, 3.0);
    let p2_l1 = doc.pages()[1].layers[0].id;
    assert_eq!(doc.active_page_index(), 0);
    let p1_l1_count_before = doc.pages()[0].layers[0].objects.len();
    let p1_l2_count_before = doc.pages()[0].layers[1].objects.len();

    let p2_child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "P2Child", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(10.0, 10.0)),
            Some(p2_l1),
        )
        .unwrap();
    let p2_parent_grp = doc.group_objects(&[p2_child], "P2InnerGroup").unwrap();
    doc.transform_object(p2_parent_grp, Affine2D::translate(100.0, 100.0))
        .unwrap();
    let expected_bb = doc.world_object_bounds(p2_child).unwrap();
    assert!((expected_bb.min_x - 110.0).abs() < 1e-6);
    assert!((expected_bb.min_y - 110.0).abs() < 1e-6);

    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![p2_child, p2_parent_grp],
            name: "P2OuterGroup".into(),
        },
    )
    .unwrap();
    assert_eq!(doc.pages()[0].layers[0].objects.len(), p1_l1_count_before);
    assert_eq!(doc.pages()[0].layers[1].objects.len(), p1_l2_count_before);
    assert_eq!(doc.pages()[1].layers[0].objects.len(), 1);
    let outer_grp = doc.pages()[1].layers[0].objects[0];
    assert_eq!(doc.parent_container_of(p2_parent_grp), Some(outer_grp));
    assert_eq!(doc.parent_container_of(p2_child), Some(p2_parent_grp));
    let grouped_bb = doc.world_object_bounds(p2_child).unwrap();
    assert!((grouped_bb.min_x - expected_bb.min_x).abs() < 1e-6);
    assert!((grouped_bb.min_y - expected_bb.min_y).abs() < 1e-6);
    assert!((grouped_bb.max_x - expected_bb.max_x).abs() < 1e-6);
    assert!((grouped_bb.max_y - expected_bb.max_y).abs() < 1e-6);

    hist.undo(&mut doc).unwrap();
    assert_eq!(doc.pages()[1].layers[0].objects, vec![p2_parent_grp]);
    let undone_bb = doc.world_object_bounds(p2_child).unwrap();
    assert!((undone_bb.min_x - 110.0).abs() < 1e-6);
    hist.redo(&mut doc).unwrap();
    let redone_bb = doc.world_object_bounds(p2_child).unwrap();
    assert!((redone_bb.min_x - 110.0).abs() < 1e-6);
}

#[test]
fn w1b_r14_group_mixed_containers_preserves_world_coords() {
    let assert_rect_eq = |got: Rect2D, min_x: f64, min_y: f64, max_x: f64, max_y: f64| {
        assert!((got.min_x - min_x).abs() < 1e-6, "got {got:?}");
        assert!((got.min_y - min_y).abs() < 1e-6, "got {got:?}");
        assert!((got.max_x - max_x).abs() < 1e-6, "got {got:?}");
        assert!((got.max_y - max_y).abs() < 1e-6, "got {got:?}");
    };

    // Order A: [nested_in_translated_group, top_level]
    let mut doc = Document::new_a4("MixedGroupA");
    let mut hist = CommandHistory::new(64);
    let inner = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Inner", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(5.0, 5.0)),
            None,
        )
        .unwrap();
    let grp_a = doc.group_objects(&[inner], "GrpA").unwrap();
    doc.transform_object(grp_a, Affine2D::translate(100.0, 100.0))
        .unwrap();
    let sib = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Sib", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(10.0, 10.0)),
            None,
        )
        .unwrap();

    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![inner, sib],
            name: "MixedA".into(),
        },
    )
    .unwrap();
    assert_rect_eq(
        doc.world_object_bounds(inner).unwrap(),
        105.0,
        105.0,
        115.0,
        115.0,
    );
    assert_rect_eq(
        doc.world_object_bounds(sib).unwrap(),
        10.0,
        10.0,
        20.0,
        20.0,
    );
    hist.undo(&mut doc).unwrap();
    assert_rect_eq(
        doc.world_object_bounds(inner).unwrap(),
        105.0,
        105.0,
        115.0,
        115.0,
    );
    assert_rect_eq(
        doc.world_object_bounds(sib).unwrap(),
        10.0,
        10.0,
        20.0,
        20.0,
    );
    hist.redo(&mut doc).unwrap();
    assert_rect_eq(
        doc.world_object_bounds(inner).unwrap(),
        105.0,
        105.0,
        115.0,
        115.0,
    );
    assert_rect_eq(
        doc.world_object_bounds(sib).unwrap(),
        10.0,
        10.0,
        20.0,
        20.0,
    );

    // Order B: [top_level, nested_in_translated_group]
    let mut doc_b = Document::new_a4("MixedGroupB");
    let inner_b = doc_b
        .add_object(
            SceneObject::new(ObjectId(0), "InnerB", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(5.0, 5.0)),
            None,
        )
        .unwrap();
    let grp_b = doc_b.group_objects(&[inner_b], "GrpB").unwrap();
    doc_b
        .transform_object(grp_b, Affine2D::translate(100.0, 100.0))
        .unwrap();
    let sib_b = doc_b
        .add_object(
            SceneObject::new(ObjectId(0), "SibB", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(10.0, 10.0)),
            None,
        )
        .unwrap();
    doc_b.group_objects(&[sib_b, inner_b], "MixedB").unwrap();
    assert_rect_eq(
        doc_b.world_object_bounds(inner_b).unwrap(),
        105.0,
        105.0,
        115.0,
        115.0,
    );
    assert_rect_eq(
        doc_b.world_object_bounds(sib_b).unwrap(),
        10.0,
        10.0,
        20.0,
        20.0,
    );

    // Order C: [group_a_child, group_b_child] across two differently transformed groups
    let mut doc_c = Document::new_a4("MixedGroupC");
    let c1 = doc_c
        .add_object(
            SceneObject::new(ObjectId(0), "C1", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(2.0, 3.0)),
            None,
        )
        .unwrap();
    let ga = doc_c.group_objects(&[c1], "GA").unwrap();
    doc_c
        .transform_object(ga, Affine2D::translate(50.0, 60.0))
        .unwrap();
    let c2 = doc_c
        .add_object(
            SceneObject::new(ObjectId(0), "C2", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(4.0, 5.0)),
            None,
        )
        .unwrap();
    let gb = doc_c.group_objects(&[c2], "GB").unwrap();
    doc_c
        .transform_object(gb, Affine2D::translate(200.0, 150.0))
        .unwrap();
    doc_c.group_objects(&[c1, c2], "MixedAB").unwrap();
    assert_rect_eq(
        doc_c.world_object_bounds(c1).unwrap(),
        52.0,
        63.0,
        62.0,
        73.0,
    );
    assert_rect_eq(
        doc_c.world_object_bounds(c2).unwrap(),
        204.0,
        155.0,
        214.0,
        165.0,
    );
}

#[test]
fn w1b_r14_ungroup_cleans_dangling_connector_and_blend() {
    let mut doc = Document::new_a4("UngroupCleanup");
    let mut hist = CommandHistory::new(64);
    let c1 = doc
        .add_object(
            SceneObject::new(ObjectId(0), "C1", rect_shape(20.0, 20.0))
                .with_transform(Affine2D::translate(10.0, 10.0)),
            None,
        )
        .unwrap();
    let c2 = doc
        .add_object(
            SceneObject::new(ObjectId(0), "C2", rect_shape(20.0, 20.0))
                .with_transform(Affine2D::translate(40.0, 10.0)),
            None,
        )
        .unwrap();
    let grp = doc.group_objects(&[c1, c2], "Grp").unwrap();
    doc.transform_object(grp, Affine2D::translate(100.0, 50.0))
        .unwrap();
    let grp_bb = doc.world_object_bounds(grp).unwrap();

    let target_box = doc
        .add_object(
            SceneObject::new(ObjectId(0), "TargetBox", rect_shape(20.0, 20.0))
                .with_transform(Affine2D::translate(220.0, 80.0)),
            None,
        )
        .unwrap();
    let conn_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Conn",
                NodePayload::Connector(ConnectorObject::between_objects(
                    grp,
                    AnchorPoint::Center,
                    target_box,
                    AnchorPoint::Center,
                    ConnectorStyle::Straight,
                )),
            ),
            None,
        )
        .unwrap();

    let mut blend_src = SceneObject::new(ObjectId(0), "BlendSrc", rect_shape(10.0, 10.0));
    blend_src.effects.push(Effect::Blend {
        target_id: grp,
        steps: 4,
        rotation_deg: 0.0,
        along_path: Some(grp),
        rotate_along_path: true,
    });
    let blend_id = doc.add_object(blend_src, None).unwrap();

    hist.execute(&mut doc, CommandOp::Ungroup { group_id: grp })
        .unwrap();
    assert!(!doc.has_object(grp));
    let NodePayload::Connector(ref conn) = doc.object(conn_id).unwrap().payload else {
        panic!("expected Connector");
    };
    assert_eq!(conn.from_obj, None);
    assert_eq!(conn.to_obj, Some(target_box));
    assert!((conn.from_fallback.x - grp_bb.center().x).abs() < 1e-6);
    assert!((conn.from_fallback.y - grp_bb.center().y).abs() < 1e-6);
    assert!(doc.object(blend_id).unwrap().effects.is_empty());

    hist.undo(&mut doc).unwrap();
    assert!(doc.has_object(grp));
    let NodePayload::Connector(ref conn_u) = doc.object(conn_id).unwrap().payload else {
        panic!("expected Connector");
    };
    assert_eq!(conn_u.from_obj, Some(grp));
    assert_eq!(doc.object(blend_id).unwrap().effects.len(), 1);
}

#[test]
fn w1b_r14_create_powerclip_topmost_preserves_group_children() {
    let mut doc = Document::new_a4("PowerClipTopmost");
    let mut hist = CommandHistory::new(64);
    let pc = doc
        .add_object(
            SceneObject::new(ObjectId(0), "PC", rect_shape(100.0, 100.0)),
            None,
        )
        .unwrap();
    let child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Child", rect_shape(20.0, 20.0))
                .with_transform(Affine2D::translate(10.0, 10.0)),
            None,
        )
        .unwrap();
    let parent_grp = doc.group_objects(&[child], "ParentGrp").unwrap();

    hist.execute(
        &mut doc,
        CommandOp::CreatePowerClip {
            container_id: pc,
            content_ids: vec![parent_grp, child],
        },
    )
    .unwrap();
    assert_eq!(doc.parent_container_of(child), Some(parent_grp));
    assert_eq!(doc.parent_container_of(parent_grp), Some(pc));
    let NodePayload::PowerClip { ref contents, .. } = doc.object(pc).unwrap().payload else {
        panic!("expected PowerClip");
    };
    assert_eq!(contents, &vec![parent_grp]);

    hist.undo(&mut doc).unwrap();
    assert_eq!(doc.parent_container_of(child), Some(parent_grp));
    assert_eq!(doc.parent_container_of(parent_grp), None);
}

#[test]
fn w1b_r14_fit_powerclip_translated_with_text_on_path_and_connector() {
    let mut doc = Document::new_a4("FitPowerClipWorld");
    let mut hist = CommandHistory::new(64);
    let pc = doc
        .add_object(
            SceneObject::new(ObjectId(0), "PC", rect_shape(100.0, 100.0))
                .with_transform(Affine2D::translate(300.0, 300.0)),
            None,
        )
        .unwrap();
    let path_obj = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "GuidePath",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    0.0, 0.0, 20.0, 20.0,
                ))),
            )
            .with_transform(Affine2D::translate(300.0, 300.0)),
            None,
        )
        .unwrap();
    let top_obj = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Top",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("Hi", TextStyle::default()),
                    path_id: path_obj,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            )
            .with_transform(Affine2D::translate(300.0, 300.0)),
            None,
        )
        .unwrap();
    let conn_obj = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Conn",
                NodePayload::Connector(ConnectorObject::between_objects(
                    path_obj,
                    AnchorPoint::Center,
                    path_obj,
                    AnchorPoint::Center,
                    ConnectorStyle::Straight,
                )),
            )
            .with_transform(Affine2D::translate(300.0, 300.0)),
            None,
        )
        .unwrap();

    hist.execute(
        &mut doc,
        CommandOp::CreatePowerClip {
            container_id: pc,
            content_ids: vec![path_obj, top_obj, conn_obj],
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::FitPowerClip {
            id: pc,
            mode: PowerClipFitMode::CenterContents,
        },
    )
    .unwrap();
    let path_bb = doc.world_object_bounds(path_obj).unwrap();
    assert!((path_bb.min_x - 340.0).abs() < 1e-6, "got {path_bb:?}");
    assert!((path_bb.min_y - 340.0).abs() < 1e-6, "got {path_bb:?}");
    assert!((path_bb.max_x - 360.0).abs() < 1e-6, "got {path_bb:?}");
    assert!((path_bb.max_y - 360.0).abs() < 1e-6, "got {path_bb:?}");
    let top_bb = doc.world_object_bounds(top_obj).unwrap();
    assert!((top_bb.min_x - 340.0).abs() < 1e-6, "got {top_bb:?}");
    assert!((top_bb.min_y - 340.0).abs() < 1e-6, "got {top_bb:?}");
}

#[test]
fn w1b_r14_move_to_desktop_layer_preserves_nested_world_and_rejects_locked_desktop() {
    let mut doc = Document::new_a4("MoveDesktop");
    let mut hist = CommandHistory::new(64);
    let child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Child", rect_shape(20.0, 20.0))
                .with_transform(Affine2D::translate(10.0, 10.0)),
            None,
        )
        .unwrap();
    let grp = doc.group_objects(&[child], "Grp").unwrap();
    doc.transform_object(grp, Affine2D::translate(100.0, 100.0))
        .unwrap();

    hist.execute(&mut doc, CommandOp::MoveToDesktopLayer { id: child })
        .unwrap();
    let desktop_lid = doc.desktop_layer_id().unwrap();
    assert_eq!(doc.layer_of_object(child), Some(desktop_lid));
    assert_eq!(doc.parent_container_of(child), None);
    let bb = doc.world_object_bounds(child).unwrap();
    assert!((bb.min_x - 110.0).abs() < 1e-6, "got {bb:?}");
    assert!((bb.min_y - 110.0).abs() < 1e-6, "got {bb:?}");
    assert!((bb.max_x - 130.0).abs() < 1e-6, "got {bb:?}");
    assert!((bb.max_y - 130.0).abs() < 1e-6, "got {bb:?}");

    // Lock the Desktop layer and verify moving another object to Desktop rejects atomically
    doc.set_layer_flags(desktop_lid, None, Some(true), None, None)
        .unwrap();
    let l1 = doc.pages()[0].layers[0].id;
    let other = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Other", rect_shape(15.0, 15.0)),
            Some(l1),
        )
        .unwrap();
    let err = hist.execute(&mut doc, CommandOp::MoveToDesktopLayer { id: other });
    assert!(matches!(err, Err(CoreError::ObjectLocked(id)) if id == other));
    assert_eq!(doc.layer_of_object(other), Some(l1));
    assert!(!doc
        .find_layer(desktop_lid)
        .unwrap()
        .objects
        .contains(&other));
}

#[test]
fn w1b_r15_fit_powerclip_rotated_and_nested_preserves_exact_local_bounds() {
    // 1. 45-degree rotated 100x100 PowerClip containing a 100%-filling square: FitContents must NOT shrink it
    let mut doc = Document::new_a4("RotatedPowerClipFit");
    let mut hist = CommandHistory::new(64);
    let lid = doc.pages()[0].layers[0].id;
    let angle = std::f64::consts::FRAC_PI_4;

    let pc_rect = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "PC100",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    0.0, 0.0, 100.0, 100.0,
                ))),
            )
            .with_transform(Affine2D::translate(200.0, 200.0).compose(&Affine2D::rotate(angle))),
            Some(lid),
        )
        .unwrap();
    let full_child = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "FullChild",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    0.0, 0.0, 100.0, 100.0,
                ))),
            )
            .with_transform(Affine2D::translate(200.0, 200.0).compose(&Affine2D::rotate(angle))),
            Some(lid),
        )
        .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::CreatePowerClip {
            container_id: pc_rect,
            content_ids: vec![full_child],
        },
    )
    .unwrap();
    let expected_world = doc.world_object_bounds(full_child).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::FitPowerClip {
            id: pc_rect,
            mode: PowerClipFitMode::FitContents,
        },
    )
    .unwrap();
    let after_fit_world = doc.world_object_bounds(full_child).unwrap();
    assert!(
        (after_fit_world.min_x - expected_world.min_x).abs() < 1e-6
            && (after_fit_world.max_x - expected_world.max_x).abs() < 1e-6
            && (after_fit_world.min_y - expected_world.min_y).abs() < 1e-6
            && (after_fit_world.max_y - expected_world.max_y).abs() < 1e-6,
        "rotated PowerClip FitContents must not shrink 100%-filling child: expected {expected_world:?}, got {after_fit_world:?}"
    );

    // 2. Rotated + scaled PowerClip nested inside a rotated Group with asymmetric L-shape + TextOnPath + Connector
    let mut doc2 = Document::new_a4("NestedRotatedPC");
    let mut hist2 = CommandHistory::new(64);
    let lid2 = doc2.pages()[0].layers[0].id;
    let pc_xf = Affine2D::translate(50.0, 40.0)
        .compose(&Affine2D::rotate(angle))
        .compose(&Affine2D::scale(1.5, 1.5));
    let pc2 = doc2
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "PC2",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    0.0, 0.0, 100.0, 100.0,
                ))),
            ),
            Some(lid2),
        )
        .unwrap();
    let outer_grp = doc2.group_objects(&[pc2], "OuterRotGrp").unwrap();
    doc2.transform_object(outer_grp, Affine2D::rotate(0.35))
        .unwrap();
    doc2.transform_object(pc2, pc_xf).unwrap();
    let pc2_world = doc2.world_transform_of(pc2).unwrap();

    // Asymmetric L-shape in [0..40, 0..40] in PowerClip-local coordinates
    let l_local = BezierPath::from_subpath(Subpath::from_points(
        [
            Point2D::new(0.0, 0.0),
            Point2D::new(40.0, 0.0),
            Point2D::new(40.0, 10.0),
            Point2D::new(10.0, 10.0),
            Point2D::new(10.0, 40.0),
            Point2D::new(0.0, 40.0),
        ],
        true,
    ));
    let l_obj = doc2
        .add_object(
            SceneObject::new(ObjectId(0), "LShape", NodePayload::Path(l_local.clone()))
                .with_transform(pc2_world),
            Some(lid2),
        )
        .unwrap();
    let path_for_top = doc2
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "GuidePath",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(5.0, 5.0),
                    Point2D::new(35.0, 20.0),
                )),
            )
            .with_transform(pc2_world),
            Some(lid2),
        )
        .unwrap();
    let top_obj = doc2
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "TOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("Curve", TextStyle::default()),
                    path_id: path_for_top,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            )
            .with_transform(pc2_world),
            Some(lid2),
        )
        .unwrap();
    let mut conn = ConnectorObject::between_objects(
        l_obj,
        AnchorPoint::Center,
        l_obj,
        AnchorPoint::Center,
        ConnectorStyle::Straight,
    );
    conn.from_obj = None;
    conn.to_obj = None;
    conn.from_fallback = Point2D::new(2.0, 2.0);
    conn.to_fallback = Point2D::new(38.0, 38.0);
    let conn_obj = doc2
        .add_object(
            SceneObject::new(ObjectId(0), "Conn", NodePayload::Connector(conn))
                .with_transform(pc2_world),
            Some(lid2),
        )
        .unwrap();

    hist2
        .execute(
            &mut doc2,
            CommandOp::CreatePowerClip {
                container_id: pc2,
                content_ids: vec![l_obj, path_for_top, top_obj, conn_obj],
            },
        )
        .unwrap();

    // CenterContents: [0..40, 0..40] centered in [0..100, 0..100] -> [30..70, 30..70] in PC-local
    hist2
        .execute(
            &mut doc2,
            CommandOp::FitPowerClip {
                id: pc2,
                mode: PowerClipFitMode::CenterContents,
            },
        )
        .unwrap();
    let l_centered_local = match &doc2.object(l_obj).unwrap().payload {
        NodePayload::Path(p) => p
            .transformed(&doc2.object(l_obj).unwrap().transform)
            .bounding_box()
            .unwrap(),
        _ => panic!("expected Path"),
    };
    assert!(
        (l_centered_local.min_x - 30.0).abs() < 1e-6
            && (l_centered_local.min_y - 30.0).abs() < 1e-6
            && (l_centered_local.max_x - 70.0).abs() < 1e-6
            && (l_centered_local.max_y - 70.0).abs() < 1e-6,
        "rotated nested CenterContents must place L-shape at [30..70, 30..70], got {l_centered_local:?}"
    );

    // Undo and run StretchToFill: [0..40, 0..40] -> [0..100, 0..100] in PC-local
    hist2.undo(&mut doc2).unwrap();
    hist2
        .execute(
            &mut doc2,
            CommandOp::FitPowerClip {
                id: pc2,
                mode: PowerClipFitMode::StretchToFill,
            },
        )
        .unwrap();
    let l_stretched_local = match &doc2.object(l_obj).unwrap().payload {
        NodePayload::Path(p) => p
            .transformed(&doc2.object(l_obj).unwrap().transform)
            .bounding_box()
            .unwrap(),
        _ => panic!("expected Path"),
    };
    assert!(
        l_stretched_local.min_x.abs() < 1e-6
            && l_stretched_local.min_y.abs() < 1e-6
            && (l_stretched_local.max_x - 100.0).abs() < 1e-6
            && (l_stretched_local.max_y - 100.0).abs() < 1e-6,
        "rotated nested StretchToFill must fill [0..100, 0..100], got {l_stretched_local:?}"
    );
}

#[test]
fn w1b_r15_delete_symbol_rejects_locked_references_atomically() {
    let mut doc = Document::new_a4("DeleteSymbolLocks");
    let mut hist = CommandHistory::new(64);
    let p1 = doc.pages()[0].id;
    let l1 = doc.pages()[0].layers[0].id;
    let locked_layer = doc
        .add_layer(p1, "LockedLayer", false, MasterLayerScope::None)
        .unwrap();

    let sym = SymbolDef::new(
        "TargetSym",
        vec![SceneObject::new(
            ObjectId(0),
            "Part",
            rect_shape(10.0, 10.0),
        )],
    );
    let sym_id = sym.id;
    hist.execute(&mut doc, CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();

    // Case 1: Directly locked SymbolInstance
    let mut inst = SceneObject::new(
        ObjectId(0),
        "LockedInst",
        NodePayload::SymbolInstance(sym_id),
    );
    inst.locked = true;
    let inst_id = doc.add_object(inst, Some(l1)).unwrap();

    // Unlocked VectorPattern bystander that must NOT be mutated when DeleteSymbol aborts
    let vpat_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "VPat", rect_shape(20.0, 20.0)).with_fill(
                Fill::VectorPattern {
                    symbol_id: sym_id,
                    tile_w: 10.0,
                    tile_h: 10.0,
                },
            ),
            Some(l1),
        )
        .unwrap();

    let snap1 = doc.clone();
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::DeleteSymbol { id: sym_id }),
        Err(CoreError::ObjectLocked(id)) if id == inst_id
    ));
    assert_eq!(doc.objects(), snap1.objects());
    assert!(doc.symbol(sym_id).is_some());

    // Unlock instance, lock vpat via locked layer -> still rejects atomically!
    hist.execute(
        &mut doc,
        CommandOp::SetObjectLocked {
            ids: vec![inst_id],
            locked: false,
        },
    )
    .unwrap();
    let tbl = VectorTable::new(1, 1, 20.0, 10.0);
    let mut tbl_obj = SceneObject::new(ObjectId(0), "LockedTable", NodePayload::Table(tbl));
    if let NodePayload::Table(ref mut t) = tbl_obj.payload {
        t.cells[0].fill = Fill::VectorPattern {
            symbol_id: sym_id,
            tile_w: 5.0,
            tile_h: 5.0,
        };
    }
    let tbl_id = doc.add_object(tbl_obj, Some(locked_layer)).unwrap();
    doc.set_layer_flags(locked_layer, None, Some(true), None, None)
        .unwrap();

    let snap2 = doc.clone();
    assert!(matches!(
        hist.execute(&mut doc, CommandOp::DeleteSymbol { id: sym_id }),
        Err(CoreError::ObjectLocked(id)) if id == tbl_id
    ));
    assert_eq!(doc.objects(), snap2.objects());
    assert!(doc.symbol(sym_id).is_some());

    // Unlock layer -> DeleteSymbol succeeds and unlinks/clears cleanly, with undo/redo
    doc.set_layer_flags(locked_layer, None, Some(false), None, None)
        .unwrap();
    hist.execute(&mut doc, CommandOp::DeleteSymbol { id: sym_id })
        .unwrap();
    assert!(doc.symbol(sym_id).is_none());
    assert!(matches!(
        doc.object(inst_id).unwrap().payload,
        NodePayload::Group(_)
    ));
    assert_eq!(doc.object(vpat_id).unwrap().fill, Fill::None);
    if let NodePayload::Table(ref t) = doc.object(tbl_id).unwrap().payload {
        assert_eq!(t.cells[0].fill, Fill::None);
    } else {
        panic!("expected Table");
    }

    hist.undo(&mut doc).unwrap();
    assert!(doc.symbol(sym_id).is_some());
    assert!(matches!(
        doc.object(inst_id).unwrap().payload,
        NodePayload::SymbolInstance(sid) if sid == sym_id
    ));
}

#[test]
fn w1b_r15_replace_object_in_parent_and_find_replace_table_cells() {
    use coree_core::find_replace::{
        find_and_replace, ColorMatch, FindQuery, FindScope, ReplaceAction,
    };

    let mut doc = Document::new_a4("ReplaceAndTable");
    let mut hist = CommandHistory::new(64);
    let lid = doc.pages()[0].layers[0].id;

    let before = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Before", rect_shape(5.0, 5.0)),
            Some(lid),
        )
        .unwrap();
    let target = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Target", rect_shape(20.0, 20.0)),
            Some(lid),
        )
        .unwrap();
    let after = doc
        .add_object(
            SceneObject::new(ObjectId(0), "After", rect_shape(5.0, 5.0)),
            Some(lid),
        )
        .unwrap();
    let grp = doc.group_objects(&[before, target, after], "Grp").unwrap();

    let s1 = SceneObject::new(
        ObjectId(0),
        "Slice1",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 10.0, 20.0,
        ))),
    );
    let s2 = SceneObject::new(
        ObjectId(0),
        "Slice2",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 0.0, 10.0, 20.0,
        ))),
    );
    hist.execute(
        &mut doc,
        CommandOp::ReplaceObjectInParent {
            id: target,
            replacements: vec![s1, s2],
        },
    )
    .unwrap();
    assert!(!doc.has_object(target));
    if let NodePayload::Group(ref children) = doc.object(grp).unwrap().payload {
        assert_eq!(children.len(), 4);
        assert_eq!(children[0], before);
        assert_eq!(children[3], after);
    } else {
        panic!("expected Group");
    }
    hist.undo(&mut doc).unwrap();
    assert!(doc.has_object(target));
    if let NodePayload::Group(ref children) = doc.object(grp).unwrap().payload {
        assert_eq!(children, &[before, target, after]);
    } else {
        panic!("expected Group");
    }

    // Find & Replace on VectorTable cell fills & fonts
    let red = Color::rgb(1.0, 0.0, 0.0);
    let blue = Color::rgb(0.0, 0.0, 1.0);
    let mut tbl = VectorTable::new(1, 2, 20.0, 10.0);
    tbl.cells[0].fill = Fill::Solid(red.clone());
    tbl.cells[0].text = "Header".into();
    tbl.cells[0].text_style.font_family = "Arial".into();
    tbl.cells[0].text_style.fill = red.clone();
    let tbl_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Tbl", NodePayload::Table(tbl)),
            Some(lid),
        )
        .unwrap();

    let rep_color = find_and_replace(
        &mut doc,
        &FindQuery {
            scope: FindScope::WholeDocument,
            fill_color: Some(ColorMatch::exact(red)),
            ..FindQuery::default()
        },
        &ReplaceAction::ReplaceFillColor {
            color: blue.clone(),
        },
    );
    assert!(rep_color.modified_ids.contains(&tbl_id));
    let rep_font = find_and_replace(
        &mut doc,
        &FindQuery {
            scope: FindScope::WholeDocument,
            font_family: Some("Arial".into()),
            ..FindQuery::default()
        },
        &ReplaceAction::ReplaceFontFamily {
            font_family: "Cairo".into(),
        },
    );
    assert!(rep_font.modified_ids.contains(&tbl_id));
    if let NodePayload::Table(ref t) = doc.object(tbl_id).unwrap().payload {
        assert_eq!(t.cells[0].fill, Fill::Solid(blue.clone()));
        assert_eq!(t.cells[0].text_style.fill, blue);
        assert_eq!(t.cells[0].text_style.font_family, "Cairo");
    } else {
        panic!("expected Table");
    }
}

#[test]
fn w1b_r16_replace_in_parent_and_boolean_op_preserve_container_fallbacks() {
    let angle = std::f64::consts::FRAC_PI_4;
    let grp_xf = Affine2D::translate(100.0, 200.0).compose(&Affine2D::rotate(angle));

    // 1. ReplaceObjectInParent on a child inside a translated+rotated Group referenced by
    //    an external Connector and an external TextOnPath -> preserves world anchor fallbacks!
    let mut doc = Document::new_a4("R16ReplaceInParent");
    let mut hist = CommandHistory::new(64);
    let lid = doc.pages()[0].layers[0].id;

    let host_in_grp = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "HostInGrp",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 0.0),
                    Point2D::new(40.0, 0.0),
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let _grp = doc.group_objects(&[host_in_grp], "RotGrp").unwrap();
    doc.transform_object(_grp, grp_xf).unwrap();

    let expected_host_center = doc.world_object_bounds(host_in_grp).unwrap().center();
    let expected_anchor_pt = doc
        .world_path_of(host_in_grp)
        .unwrap()
        .point_and_tangent_at(0.0)
        .unwrap()
        .0;

    let mut conn = ConnectorObject::between_objects(
        host_in_grp,
        AnchorPoint::Center,
        host_in_grp,
        AnchorPoint::Center,
        ConnectorStyle::Straight,
    );
    conn.to_obj = None;
    conn.to_fallback = Point2D::new(10.0, 10.0);
    let conn_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ExtConn", NodePayload::Connector(conn)),
            Some(lid),
        )
        .unwrap();
    let top_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ExtTOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("OnPath", TextStyle::default()),
                    path_id: host_in_grp,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(lid),
        )
        .unwrap();

    let rep = SceneObject::new(
        ObjectId(0),
        "Replacement",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 10.0, 10.0,
        ))),
    );
    hist.execute(
        &mut doc,
        CommandOp::ReplaceObjectInParent {
            id: host_in_grp,
            replacements: vec![rep],
        },
    )
    .unwrap();

    if let NodePayload::Connector(ref c) = doc.object(conn_id).unwrap().payload {
        assert!(c.from_obj.is_none());
        assert!(
            c.from_fallback.distance_to(expected_host_center) < 1e-4,
            "ReplaceObjectInParent must capture world fallback {:?}, got {:?}",
            expected_host_center,
            c.from_fallback
        );
    } else {
        panic!("expected Connector");
    }
    let top_obj = doc.object(top_id).unwrap();
    assert!(matches!(top_obj.payload, NodePayload::ArtisticText(_)));
    let top_pos = top_obj.transform.transform_point(Point2D::ZERO);
    assert!(
        top_pos.distance_to(expected_anchor_pt) < 1e-4,
        "ReplaceObjectInParent must preserve TextOnPath world anchor {:?}, got {:?}",
        expected_anchor_pt,
        top_pos
    );

    hist.undo(&mut doc).unwrap();
    assert!(doc.has_object(host_in_grp));
    if let NodePayload::Connector(ref c) = doc.object(conn_id).unwrap().payload {
        assert_eq!(c.from_obj, Some(host_in_grp));
    }
    assert!(matches!(
        doc.object(top_id).unwrap().payload,
        NodePayload::TextOnPath(_)
    ));
    hist.redo(&mut doc).unwrap();
    assert!(!doc.has_object(host_in_grp));

    // 2. BooleanOp::{Weld, Simplify, BreakApart} inside translated+rotated Group/PowerClip
    for op in [BooleanOp::Weld, BooleanOp::Simplify, BooleanOp::BreakApart] {
        let mut bdoc = Document::new_a4("R16BoolGroup");
        let mut bhist = CommandHistory::new(64);
        let blid = bdoc.pages()[0].layers[0].id;

        let a_id = bdoc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "A",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 20.0, 20.0,
                    ))),
                ),
                Some(blid),
            )
            .unwrap();
        let b_id = bdoc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "B",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        10.0, 0.0, 20.0, 20.0,
                    ))),
                ),
                Some(blid),
            )
            .unwrap();
        let grp = bdoc.group_objects(&[a_id, b_id], "BoolGrp").unwrap();
        bdoc.transform_object(grp, grp_xf).unwrap();

        let exp_a_center = bdoc.world_object_bounds(a_id).unwrap().center();
        let exp_b_anchor = bdoc
            .world_path_of(b_id)
            .unwrap()
            .point_and_tangent_at(0.0)
            .unwrap()
            .0;

        let mut c2 = ConnectorObject::between_objects(
            a_id,
            AnchorPoint::Center,
            a_id,
            AnchorPoint::Center,
            ConnectorStyle::Straight,
        );
        c2.to_obj = None;
        c2.to_fallback = Point2D::ZERO;
        let c2_id = bdoc
            .add_object(
                SceneObject::new(ObjectId(0), "ConnBool", NodePayload::Connector(c2)),
                Some(blid),
            )
            .unwrap();
        let t2_id = bdoc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "TOPBool",
                    NodePayload::TextOnPath(TextOnPath {
                        text: ArtisticText::from_plain("Bool", TextStyle::default()),
                        path_id: b_id,
                        offset_mm: 0.0,
                        distance_mm: 0.0,
                        flip_vertical: false,
                        flip_horizontal: false,
                    }),
                ),
                Some(blid),
            )
            .unwrap();

        let ids = vec![a_id, b_id];
        bhist
            .execute(&mut bdoc, CommandOp::BooleanOp { op, ids })
            .unwrap();

        if let NodePayload::Connector(ref c) = bdoc.object(c2_id).unwrap().payload {
            assert!(c.from_obj.is_none());
            assert!(
                c.from_fallback.distance_to(exp_a_center) < 1e-4,
                "{op:?} must preserve Connector fallback {:?}, got {:?}",
                exp_a_center,
                c.from_fallback
            );
        } else {
            panic!("expected Connector");
        }
        let t2_obj = bdoc.object(t2_id).unwrap();
        assert!(matches!(t2_obj.payload, NodePayload::ArtisticText(_)));
        let t2_pos = t2_obj.transform.transform_point(Point2D::ZERO);
        assert!(
            t2_pos.distance_to(exp_b_anchor) < 1e-4,
            "{op:?} must preserve TextOnPath anchor {:?}, got {:?}",
            exp_b_anchor,
            t2_pos
        );
    }
}

#[test]
fn w1b_r16_convert_to_curves_and_outline_powerclip_preserve_child_fallbacks() {
    let pc_xf =
        Affine2D::translate(120.0, 80.0).compose(&Affine2D::rotate(std::f64::consts::FRAC_PI_4));

    // 1. ConvertToCurves on rotated/translated PowerClip whose child is referenced externally
    let mut doc = Document::new_a4("R16PowerClipCurves");
    let mut hist = CommandHistory::new(64);
    let lid = doc.pages()[0].layers[0].id;

    let pc_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "PC",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    0.0, 0.0, 80.0, 80.0,
                ))),
            )
            .with_transform(pc_xf),
            Some(lid),
        )
        .unwrap();
    let child_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ChildLine",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(10.0, 10.0),
                    Point2D::new(50.0, 10.0),
                )),
            )
            .with_transform(pc_xf),
            Some(lid),
        )
        .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::CreatePowerClip {
            container_id: pc_id,
            content_ids: vec![child_id],
        },
    )
    .unwrap();

    let exp_center = doc.world_object_bounds(child_id).unwrap().center();
    let exp_anchor = doc
        .world_path_of(child_id)
        .unwrap()
        .point_and_tangent_at(0.0)
        .unwrap()
        .0;

    let mut conn = ConnectorObject::between_objects(
        child_id,
        AnchorPoint::Center,
        child_id,
        AnchorPoint::Center,
        ConnectorStyle::Straight,
    );
    conn.to_obj = None;
    conn.to_fallback = Point2D::ZERO;
    let conn_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Conn", NodePayload::Connector(conn)),
            Some(lid),
        )
        .unwrap();
    let top_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "TOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("PCChild", TextStyle::default()),
                    path_id: child_id,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(lid),
        )
        .unwrap();

    hist.execute(&mut doc, CommandOp::ConvertToCurves { id: pc_id })
        .unwrap();
    assert!(!doc.has_object(child_id));
    if let NodePayload::Connector(ref c) = doc.object(conn_id).unwrap().payload {
        assert!(
            c.from_fallback.distance_to(exp_center) < 1e-4,
            "ConvertToCurves on PowerClip must capture world fallback {:?}, got {:?}",
            exp_center,
            c.from_fallback
        );
    } else {
        panic!("expected Connector");
    }
    let top_pos = doc
        .object(top_id)
        .unwrap()
        .transform
        .transform_point(Point2D::ZERO);
    assert!(
        top_pos.distance_to(exp_anchor) < 1e-4,
        "ConvertToCurves on PowerClip must preserve TextOnPath anchor {:?}, got {:?}",
        exp_anchor,
        top_pos
    );

    hist.undo(&mut doc).unwrap();
    assert!(doc.has_object(child_id));

    // 2. ConvertOutlineToObject on Fill::None rotated/translated PowerClip with visible outline
    let mut doc2 = Document::new_a4("R16PowerClipOutline");
    let mut hist2 = CommandHistory::new(64);
    let lid2 = doc2.pages()[0].layers[0].id;

    let pc2 = doc2
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "PCNoFill",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    0.0, 0.0, 80.0, 80.0,
                ))),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::solid(Color::BLACK, 2.0))
            .with_transform(pc_xf),
            Some(lid2),
        )
        .unwrap();
    let child2 = doc2
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Child2",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(10.0, 10.0),
                    Point2D::new(50.0, 10.0),
                )),
            )
            .with_transform(pc_xf),
            Some(lid2),
        )
        .unwrap();
    hist2
        .execute(
            &mut doc2,
            CommandOp::CreatePowerClip {
                container_id: pc2,
                content_ids: vec![child2],
            },
        )
        .unwrap();

    let exp_center2 = doc2.world_object_bounds(child2).unwrap().center();
    let exp_anchor2 = doc2
        .world_path_of(child2)
        .unwrap()
        .point_and_tangent_at(0.0)
        .unwrap()
        .0;

    let mut conn2 = ConnectorObject::between_objects(
        child2,
        AnchorPoint::Center,
        child2,
        AnchorPoint::Center,
        ConnectorStyle::Straight,
    );
    conn2.to_obj = None;
    conn2.to_fallback = Point2D::ZERO;
    let conn2_id = doc2
        .add_object(
            SceneObject::new(ObjectId(0), "Conn2", NodePayload::Connector(conn2)),
            Some(lid2),
        )
        .unwrap();
    let top2_id = doc2
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "TOP2",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("PCChild2", TextStyle::default()),
                    path_id: child2,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(lid2),
        )
        .unwrap();

    hist2
        .execute(&mut doc2, CommandOp::ConvertOutlineToObject { id: pc2 })
        .unwrap();
    assert!(!doc2.has_object(child2));
    if let NodePayload::Connector(ref c) = doc2.object(conn2_id).unwrap().payload {
        assert!(
            c.from_fallback.distance_to(exp_center2) < 1e-4,
            "ConvertOutlineToObject on PowerClip must capture world fallback {:?}, got {:?}",
            exp_center2,
            c.from_fallback
        );
    } else {
        panic!("expected Connector");
    }
    let top2_pos = doc2
        .object(top2_id)
        .unwrap()
        .transform
        .transform_point(Point2D::ZERO);
    assert!(
        top2_pos.distance_to(exp_anchor2) < 1e-4,
        "ConvertOutlineToObject on PowerClip must preserve TextOnPath anchor {:?}, got {:?}",
        exp_anchor2,
        top2_pos
    );
    hist2.undo(&mut doc2).unwrap();
    assert!(doc2.has_object(child2));
}

#[test]
fn w1b_r16_local_object_bounds_and_symbol_local_bounds_zero_aabb_inflation() {
    let mut doc = Document::new_a4("R16LocalBounds");
    let lid = doc.pages()[0].layers[0].id;
    let rot45 =
        Affine2D::translate(50.0, 40.0).compose(&Affine2D::rotate(std::f64::consts::FRAC_PI_4));

    // 1. 45-deg rotated 100x10 Group nested inside a 30-deg rotated OuterGroup
    let bar = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Bar",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    0.0, 0.0, 100.0, 10.0,
                ))),
            ),
            Some(lid),
        )
        .unwrap();
    let inner_grp = doc.group_objects(&[bar], "InnerBarGrp").unwrap();
    doc.transform_object(inner_grp, rot45).unwrap();
    let outer_grp = doc.group_objects(&[inner_grp], "OuterGrp").unwrap();
    doc.transform_object(
        outer_grp,
        Affine2D::translate(20.0, 30.0).compose(&Affine2D::rotate(0.52)),
    )
    .unwrap();

    let grp_local = doc.local_object_bounds(inner_grp).unwrap();
    assert!(
        (grp_local.min_x - 0.0).abs() < 1e-4
            && (grp_local.min_y - 0.0).abs() < 1e-4
            && (grp_local.width() - 100.0).abs() < 1e-4
            && (grp_local.height() - 10.0).abs() < 1e-4,
        "45-deg rotated 100x10 Group local_object_bounds must not inflate via world AABB: {grp_local:?}"
    );

    // 2. 45-deg rotated SymbolInstance of a 100x10 bar + cycle guard
    let sym = SymbolDef::new(
        "BarSym",
        vec![SceneObject::new(
            ObjectId(1),
            "SymBar",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 100.0, 10.0,
            ))),
        )],
    );
    let sym_id = sym.id;
    doc.upsert_symbol(sym);
    let sym_inst = doc
        .add_object(
            SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
                .with_transform(rot45),
            Some(lid),
        )
        .unwrap();
    let sym_local = doc.symbol_local_bounds(sym_id).unwrap();
    let inst_local = doc.local_object_bounds(sym_inst).unwrap();
    assert!((sym_local.width() - 100.0).abs() < 1e-4 && (sym_local.height() - 10.0).abs() < 1e-4);
    assert!((inst_local.width() - 100.0).abs() < 1e-4 && (inst_local.height() - 10.0).abs() < 1e-4);

    // Self-referential symbol cycle terminates cleanly
    let mut cyc = SymbolDef::new("Cyclic", Vec::new());
    let cyc_id = cyc.id;
    cyc.objects.push(SceneObject::new(
        ObjectId(2),
        "SelfRef",
        NodePayload::SymbolInstance(cyc_id),
    ));
    doc.upsert_symbol(cyc);
    assert!(doc.symbol_local_bounds(cyc_id).is_none());

    // 3. 45-deg rotated horizontal Connector inside rotated ancestor
    let mut conn = ConnectorObject::between_objects(
        bar,
        AnchorPoint::Center,
        bar,
        AnchorPoint::Center,
        ConnectorStyle::Straight,
    );
    conn.from_obj = None;
    conn.to_obj = None;
    conn.from_fallback = Point2D::new(0.0, 5.0);
    conn.to_fallback = Point2D::new(100.0, 5.0);
    let conn_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "HorizConn", NodePayload::Connector(conn))
                .with_transform(rot45),
            Some(lid),
        )
        .unwrap();
    let conn_grp = doc.group_objects(&[conn_id], "ConnGrp").unwrap();
    doc.transform_object(conn_grp, Affine2D::rotate(0.3))
        .unwrap();
    let conn_local = doc.local_object_bounds(conn_id).unwrap();
    assert!(
        (conn_local.width() - 100.0).abs() < 1e-4 && conn_local.height() < 1e-4,
        "45-deg rotated horizontal Connector local_object_bounds must have ~0 height, got {conn_local:?}"
    );
}

#[test]
fn w1b_r16_object_bounds_vs_world_object_bounds_consistency_for_text_on_path_and_connector() {
    let mut doc = Document::new_a4("R16BoundsConsistency");
    let lid = doc.pages()[0].layers[0].id;

    let host_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Host",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(10.0, 10.0),
                    Point2D::new(50.0, 10.0),
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let top_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "TOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("Nested", TextStyle::default()),
                    path_id: host_id,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(lid),
        )
        .unwrap();
    let mut conn = ConnectorObject::between_objects(
        host_id,
        AnchorPoint::Left,
        host_id,
        AnchorPoint::Right,
        ConnectorStyle::Straight,
    );
    conn.to_obj = None;
    conn.to_fallback = Point2D::new(40.0, 20.0);
    let conn_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Conn", NodePayload::Connector(conn)),
            Some(lid),
        )
        .unwrap();

    let inner_grp = doc
        .group_objects(&[host_id, top_id, conn_id], "Inner")
        .unwrap();
    let outer_grp = doc.group_objects(&[inner_grp], "Outer").unwrap();
    doc.transform_object(outer_grp, Affine2D::translate(60.0, 60.0))
        .unwrap();

    // `object_bounds(inner_grp)` must be Outer-parent-local (x in [10..50], WITHOUT +60),
    // while `world_object_bounds(inner_grp)` must be page-world (x in [70..110], WITH +60).
    let inner_parent_local = doc.object_bounds(inner_grp).unwrap();
    let inner_world = doc.world_object_bounds(inner_grp).unwrap();
    assert!(
        (inner_parent_local.min_x - 10.0).abs() < 1e-4
            && (inner_parent_local.max_x - 50.0).abs() < 1e-4
            && (inner_parent_local.min_y - 10.0).abs() < 1e-4
            && (inner_parent_local.max_y - 20.0).abs() < 1e-4,
        "object_bounds(inner_grp) must return parent-local bounds without +60 shift: {inner_parent_local:?}"
    );
    assert!(
        (inner_world.min_x - 70.0).abs() < 1e-4
            && (inner_world.max_x - 110.0).abs() < 1e-4
            && (inner_world.min_y - 70.0).abs() < 1e-4
            && (inner_world.max_y - 80.0).abs() < 1e-4,
        "world_object_bounds(inner_grp) must include +60 shift: {inner_world:?}"
    );

    // Direct child `object_bounds(top_id)` and `object_bounds(conn_id)` are also parent-local
    let top_local = doc.object_bounds(top_id).unwrap();
    let conn_local = doc.object_bounds(conn_id).unwrap();
    assert!((top_local.min_x - 10.0).abs() < 1e-4 && (top_local.max_x - 50.0).abs() < 1e-4);
    assert!((conn_local.min_x - 10.0).abs() < 1e-4 && (conn_local.max_x - 40.0).abs() < 1e-4);
}

#[test]
fn w1b_r17_symbol_cyclic_connectors_terminate_under_50ms() {
    let mut doc = Document::new_a4("R17 Cyclic Symbol Connector");
    let box_id = ObjectId(8001);
    let c1_id = ObjectId(8002);
    let c2_id = ObjectId(8003);
    let c3_id = ObjectId(8004);

    let sym_box = SceneObject::new(
        box_id,
        "AnchorBox",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 20.0, 20.0,
        ))),
    );
    let mut c1 = ConnectorObject::between_objects(
        c2_id,
        AnchorPoint::Center,
        box_id,
        AnchorPoint::Center,
        ConnectorStyle::Straight,
    );
    c1.from_fallback = Point2D::new(5.0, 5.0);
    c1.to_fallback = Point2D::new(20.0, 20.0);
    let mut c2 = ConnectorObject::between_objects(
        c1_id,
        AnchorPoint::Center,
        c1_id,
        AnchorPoint::Right,
        ConnectorStyle::Straight,
    );
    c2.from_fallback = Point2D::new(35.0, 25.0);
    c2.to_fallback = Point2D::new(45.0, 25.0);
    let mut c3 = ConnectorObject::between_objects(
        c3_id,
        AnchorPoint::Left,
        c3_id,
        AnchorPoint::Right,
        ConnectorStyle::Straight,
    );
    c3.from_fallback = Point2D::new(50.0, 10.0);
    c3.to_fallback = Point2D::new(70.0, 30.0);

    let sym = SymbolDef::new(
        "CyclicConnectors",
        vec![
            sym_box,
            SceneObject::new(c1_id, "C1", NodePayload::Connector(c1)),
            SceneObject::new(c2_id, "C2", NodePayload::Connector(c2)),
            SceneObject::new(c3_id, "C3", NodePayload::Connector(c3)),
        ],
    );
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    let inst_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "CyclicInst",
                NodePayload::SymbolInstance(sym_id),
            )
            .with_transform(Affine2D::translate(30.0, 40.0)),
            None,
        )
        .unwrap();

    let t0 = std::time::Instant::now();
    let sym_local = doc.symbol_local_bounds(sym_id);
    let inst_local = doc.local_object_bounds(inst_id);
    let inst_obj = doc.object_bounds(inst_id);
    let inst_world = doc.world_object_bounds(inst_id);
    let elapsed = t0.elapsed();

    assert!(sym_local.is_some());
    assert!(inst_local.is_some());
    assert!(inst_obj.is_some());
    assert!(inst_world.is_some());
    assert!(
        elapsed.as_millis() < 50,
        "cyclic Connectors inside SymbolDef must terminate in < 50ms, took {elapsed:?}"
    );
}

#[test]
fn w1b_r18_acyclic_connector_dag_n32_linear_time_symbol_and_document() {
    // 1. 36-node diamond Connector DAG inside SymbolDef (2^36 naive paths)
    let mut doc = Document::new_a4("R18 DAG Symbol+Document");
    let leaf_id = ObjectId(9000);
    let mut sym_objs = vec![SceneObject::new(
        leaf_id,
        "SymLeaf",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 20.0, 30.0, 10.0,
        ))),
    )];
    let mut prev_id = leaf_id;
    let mut sym_conn_ids = Vec::new();
    for i in 1..=36u64 {
        let cid = ObjectId(9000 + i);
        let conn = ConnectorObject::between_objects(
            prev_id,
            if i % 2 == 0 {
                AnchorPoint::Left
            } else {
                AnchorPoint::Right
            },
            prev_id,
            if i % 2 == 0 {
                AnchorPoint::Right
            } else {
                AnchorPoint::Left
            },
            ConnectorStyle::Straight,
        );
        sym_objs.push(SceneObject::new(
            cid,
            format!("SymC{i}"),
            NodePayload::Connector(conn),
        ));
        sym_conn_ids.push(cid);
        prev_id = cid;
    }
    // Reverse order so top-most DAG node evaluates first before any lower nodes
    sym_objs.reverse();
    let sym = SymbolDef::new("DiamondDagSym", sym_objs);
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    let inst_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "DagSymInst",
                NodePayload::SymbolInstance(sym_id),
            )
            .with_transform(Affine2D::translate(15.0, 25.0)),
            None,
        )
        .unwrap();

    let t0 = std::time::Instant::now();
    let sym_local = doc.symbol_local_bounds(sym_id).expect("sym_local_bounds");
    let inst_world = doc.world_object_bounds(inst_id).expect("inst_world");
    let sym_elapsed = t0.elapsed();
    assert!(
        (sym_local.min_x - 10.0).abs() < 1e-4
            && (sym_local.max_x - 40.0).abs() < 1e-4
            && (sym_local.min_y - 20.0).abs() < 1e-4
            && (sym_local.max_y - 30.0).abs() < 1e-4,
        "unexpected SymbolDef DAG bounds: {sym_local:?}"
    );
    assert!(
        (inst_world.min_x - 25.0).abs() < 1e-4 && (inst_world.max_x - 55.0).abs() < 1e-4,
        "unexpected SymbolInstance world bounds: {inst_world:?}"
    );
    assert!(
        sym_elapsed.as_millis() < 50,
        "N=36 diamond Connector DAG in SymbolDef must run in < 50ms, took {sym_elapsed:?}"
    );

    // 2. 36-node diamond Connector DAG in top-level Document (with Group container)
    let mut doc2 = Document::new_a4("R18 DAG Document");
    let doc_leaf = doc2
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "DocLeaf",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    0.0, 0.0, 20.0, 10.0,
                ))),
            ),
            None,
        )
        .unwrap();
    let grp_leaf = doc2.group_objects(&[doc_leaf], "LeafGroup").unwrap();
    doc2.transform_object(grp_leaf, Affine2D::translate(5.0, 5.0))
        .unwrap();

    let mut prev_doc_id = doc_leaf;
    for i in 1..=36u64 {
        let conn = ConnectorObject::between_objects(
            prev_doc_id,
            AnchorPoint::Left,
            prev_doc_id,
            AnchorPoint::Right,
            ConnectorStyle::Straight,
        );
        prev_doc_id = doc2
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    format!("DocC{i}"),
                    NodePayload::Connector(conn),
                ),
                None,
            )
            .unwrap();
    }

    let t1 = std::time::Instant::now();
    let top_world = doc2
        .world_object_bounds(prev_doc_id)
        .expect("top_world DAG bounds");
    let spatial = doc2.build_spatial_index(0);
    let doc_elapsed = t1.elapsed();
    assert!(
        (top_world.min_x - 5.0).abs() < 1e-4
            && (top_world.max_x - 25.0).abs() < 1e-4
            && (top_world.min_y - 10.0).abs() < 1e-4
            && (top_world.max_y - 10.0).abs() < 1e-4,
        "unexpected Document DAG bounds: {top_world:?}"
    );
    assert!(!spatial.is_empty());
    assert!(
        doc_elapsed.as_millis() < 50,
        "N=36 diamond Connector DAG in Document must run in < 50ms, took {doc_elapsed:?}"
    );
}

#[test]
fn w1b_r19_symbol_visual_bounds_group_extrude_blend_and_envelope_perspective() {
    // 1. SymbolInstance -> Group -> Shape + DropShadow + Symmetry, wrapped in outer Group
    let mut doc = Document::new("R19 SymVWB", 300.0, 300.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;

    let mut sh = SceneObject::new(ObjectId(1), "GroupedShape", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(5.0, 5.0));
    sh.effects.push(Effect::DropShadow {
        dx_mm: 15.0,
        dy_mm: 15.0,
        blur_mm: 2.0,
        opacity: 0.7,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Outside,
    });
    sh.effects.push(Effect::Symmetry {
        axes: 2,
        mirror: false,
        origin: Point2D::new(40.0, 40.0),
    });
    let grp = SceneObject::new(
        ObjectId(2),
        "SymGroup",
        NodePayload::Group(vec![ObjectId(1)]),
    )
    .with_transform(Affine2D::translate(10.0, 10.0));
    let sym_id = SymbolId::new();
    doc.upsert_symbol(SymbolDef {
        id: sym_id,
        name: "GrpSym".into(),
        objects: vec![sh, grp],
    });
    let inst_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(30.0, 40.0)),
            Some(lid),
        )
        .unwrap();
    let outer_grp = doc.group_objects(&[inst_id], "OuterGrp").unwrap();

    let inst_vwb = doc
        .visual_world_bounds(inst_id)
        .expect("SymbolInstance containing Group must return Some(visual_world_bounds)");
    let outer_vwb = doc
        .visual_world_bounds(outer_grp)
        .expect("Outer Group containing SymbolInstance->Group must return Some");
    assert!(
        inst_vwb.min_x <= 24.0 && inst_vwb.max_x >= 86.0 && inst_vwb.max_y >= 96.0,
        "SymbolInstance Group visual_world_bounds must include shape + DropShadow + Symmetry: {inst_vwb:?}"
    );
    assert!(
        outer_vwb.min_x <= inst_vwb.min_x
            && outer_vwb.min_y <= inst_vwb.min_y
            && outer_vwb.max_x >= inst_vwb.max_x
            && outer_vwb.max_y >= inst_vwb.max_y,
        "Outer Group visual_world_bounds {outer_vwb:?} must enclose SymbolInstance {inst_vwb:?}"
    );

    // 2. SymbolInstance -> Extrude25D vanishing point + Effect::Blend target & along_path
    let mut ex_shape = SceneObject::new(ObjectId(1), "ExShape", rect_shape(20.0, 20.0));
    ex_shape.effects.push(Effect::Extrude25D {
        vanishing_point: Point2D::new(200.0, 200.0),
        depth: 10.0,
        bevel_depth: 0.0,
        shade_color: Color::gray(0.4),
        light_dir: [0.5, -0.5, 0.7],
    });
    let ex_sym_id = SymbolId::new();
    doc.upsert_symbol(SymbolDef {
        id: ex_sym_id,
        name: "ExSym".into(),
        objects: vec![ex_shape],
    });
    let ex_inst = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ExInst",
                NodePayload::SymbolInstance(ex_sym_id),
            )
            .with_transform(Affine2D::translate(10.0, 10.0)),
            Some(lid),
        )
        .unwrap();
    let ex_vwb = doc.visual_world_bounds(ex_inst).unwrap();
    assert!(
        ex_vwb.max_x >= 200.0 && ex_vwb.max_y >= 200.0,
        "SymbolInstance Extrude25D visual_world_bounds must include vanishing_point (200,200): {ex_vwb:?}"
    );

    // Blend inside SymbolDef targeting both a symbol-internal object and a doc-level along_path
    let doc_along = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "DocAlong",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(10.0, 220.0),
                    Point2D::new(240.0, 220.0),
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let sym_tgt = SceneObject::new(ObjectId(2), "SymTarget", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(150.0, 150.0));
    let mut sym_src = SceneObject::new(ObjectId(1), "SymSrc", rect_shape(20.0, 20.0));
    sym_src.effects.push(Effect::Blend {
        target_id: ObjectId(2),
        steps: 4,
        rotation_deg: 0.0,
        along_path: Some(doc_along),
        rotate_along_path: true,
    });
    let bl_sym_id = SymbolId::new();
    doc.upsert_symbol(SymbolDef {
        id: bl_sym_id,
        name: "BlendSym".into(),
        objects: vec![sym_src, sym_tgt],
    });
    let bl_inst = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "BlInst",
                NodePayload::SymbolInstance(bl_sym_id),
            )
            .with_transform(Affine2D::translate(10.0, 10.0)),
            Some(lid),
        )
        .unwrap();
    let bl_vwb = doc.visual_world_bounds(bl_inst).unwrap();
    assert!(
        bl_vwb.max_x >= 240.0
            && bl_vwb.max_y >= 220.0
            && bl_vwb.contains_point(Point2D::new(170.0, 170.0)),
        "SymbolInstance Blend visual_world_bounds must cover internal target and along_path: {bl_vwb:?}"
    );

    // 3. Reviewer 2 Minor 1: Effect::Envelope & Effect::Perspective deformed cage/quad world bounds
    let mut env_obj = SceneObject::new(ObjectId(0), "EnvObj", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(50.0, 50.0));
    env_obj.effects.push(Effect::Envelope {
        mode: EnvelopeMode::Unconstrained,
        cage: vec![
            Point2D::new(-15.0, -10.0),
            Point2D::new(10.0, -25.0),
            Point2D::new(45.0, -10.0),
            Point2D::new(55.0, 10.0),
            Point2D::new(45.0, 35.0),
            Point2D::new(10.0, 45.0),
            Point2D::new(-15.0, 35.0),
            Point2D::new(-20.0, 10.0),
        ],
    });
    let env_id = doc.add_object(env_obj, Some(lid)).unwrap();
    let env_vwb = doc.visual_world_bounds(env_id).unwrap();
    assert!(
        env_vwb.min_x <= 30.0
            && env_vwb.min_y <= 25.0
            && env_vwb.max_x >= 105.0
            && env_vwb.max_y >= 95.0,
        "Envelope visual_world_bounds must enclose deformed 8-pt cage in world space: {env_vwb:?}"
    );

    let mut persp_obj = SceneObject::new(ObjectId(0), "PerspObj", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(20.0, 30.0));
    persp_obj.effects.push(Effect::Perspective {
        quad: [
            Point2D::new(-10.0, -5.0),
            Point2D::new(40.0, -10.0),
            Point2D::new(50.0, 60.0),
            Point2D::new(-5.0, 40.0),
        ],
    });
    let persp_id = doc.add_object(persp_obj, Some(lid)).unwrap();
    let persp_vwb = doc.visual_world_bounds(persp_id).unwrap();
    assert!(
        persp_vwb.min_x <= 10.0
            && persp_vwb.min_y <= 20.0
            && persp_vwb.max_x >= 70.0
            && persp_vwb.max_y >= 90.0,
        "Perspective visual_world_bounds must enclose deformed quad in world space: {persp_vwb:?}"
    );
}

#[test]
fn w1b_r19_connector_cycle_order_invariance_and_hybrid_dag_performance() {
    // 1. Document-level asymmetric 2-cycle CycleA <-> CycleB
    let mut doc = Document::new("R19 CycleOrder", 300.0, 300.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;
    let cyc_a = doc.alloc_object_id();
    let cyc_b = doc.alloc_object_id();
    doc.add_object(
        SceneObject::new(
            cyc_a,
            "CycleA",
            NodePayload::Connector(ConnectorObject {
                from_obj: None,
                from_anchor: AnchorPoint::Center,
                from_fallback: Point2D::new(0.0, 0.0),
                to_obj: Some(cyc_b),
                to_anchor: AnchorPoint::Center,
                to_fallback: Point2D::new(120.0, 120.0),
                style: ConnectorStyle::Straight,
                corner_radius_mm: 0.0,
            }),
        ),
        Some(lid),
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            cyc_b,
            "CycleB",
            NodePayload::Connector(ConnectorObject {
                from_obj: Some(cyc_a),
                from_anchor: AnchorPoint::Center,
                from_fallback: Point2D::new(10.0, 10.0),
                to_obj: None,
                to_anchor: AnchorPoint::Center,
                to_fallback: Point2D::new(100.0, 100.0),
                style: ConnectorStyle::Straight,
                corner_radius_mm: 0.0,
            }),
        ),
        Some(lid),
    )
    .unwrap();

    let solo_a = doc.world_object_bounds(cyc_a).unwrap();
    let solo_b = doc.world_object_bounds(cyc_b).unwrap();
    let comb_ab = doc.combined_bounds(&[cyc_a, cyc_b]).unwrap();
    let comb_ba = doc.combined_bounds(&[cyc_b, cyc_a]).unwrap();
    assert_eq!(
        comb_ab, comb_ba,
        "combined_bounds must be commutative on cycles"
    );
    assert_eq!(comb_ab, solo_a.union(solo_b));

    // build_spatial_index must index CycleB at its true solo_b bounds [60..100, 60..100] (not [0..10])
    let spatial = doc.build_spatial_index(0);
    let hits_mid_b = spatial.query_point(Point2D::new(80.0, 80.0), 1.0);
    assert!(
        hits_mid_b.contains(&cyc_b),
        "build_spatial_index must hit CycleB at (80,80) matching solo_b={solo_b:?}, got {hits_mid_b:?}"
    );
    let hits_low_b = spatial.query_point(Point2D::new(5.0, 5.0), 1.0);
    assert!(
        !hits_low_b.contains(&cyc_b),
        "build_spatial_index must NOT hit CycleB at (5,5) (solo_b={solo_b:?}), got {hits_low_b:?}"
    );

    // 2. SymbolDef order invariance for [SymCycA, SymCycB] vs [SymCycB, SymCycA]
    let sym_a = SceneObject::new(
        ObjectId(1),
        "SymCycA",
        NodePayload::Connector(ConnectorObject {
            from_obj: None,
            from_anchor: AnchorPoint::Center,
            from_fallback: Point2D::new(0.0, 0.0),
            to_obj: Some(ObjectId(2)),
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(120.0, 120.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 0.0,
        }),
    );
    let sym_b = SceneObject::new(
        ObjectId(2),
        "SymCycB",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(ObjectId(1)),
            from_anchor: AnchorPoint::Center,
            from_fallback: Point2D::new(10.0, 10.0),
            to_obj: None,
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(100.0, 100.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 0.0,
        }),
    );
    let sym_ab_id = SymbolId::new();
    let sym_ba_id = SymbolId::new();
    doc.upsert_symbol(SymbolDef {
        id: sym_ab_id,
        name: "AB".into(),
        objects: vec![sym_a.clone(), sym_b.clone()],
    });
    doc.upsert_symbol(SymbolDef {
        id: sym_ba_id,
        name: "BA".into(),
        objects: vec![sym_b, sym_a],
    });
    let inst_ab = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "InstAB",
                NodePayload::SymbolInstance(sym_ab_id),
            ),
            Some(lid),
        )
        .unwrap();
    let inst_ba = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "InstBA",
                NodePayload::SymbolInstance(sym_ba_id),
            ),
            Some(lid),
        )
        .unwrap();
    assert_eq!(
        doc.world_object_bounds(inst_ab),
        doc.world_object_bounds(inst_ba),
        "SymbolDef bounds must be invariant to [SymCycA, SymCycB] vs [SymCycB, SymCycA] ordering"
    );

    // 3. N=40 hybrid diamond DAG feeding into CycleA <-> CycleB runs in < 50ms
    let mut prev = cyc_a;
    for i in 1..=40u64 {
        let conn = ConnectorObject::between_objects(
            prev,
            AnchorPoint::Left,
            prev,
            AnchorPoint::Right,
            ConnectorStyle::Straight,
        );
        prev = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    format!("Hybrid{i}"),
                    NodePayload::Connector(conn),
                ),
                Some(lid),
            )
            .unwrap();
    }
    let t0 = std::time::Instant::now();
    let hybrid_bb = doc.combined_bounds(&[prev, cyc_b]).unwrap();
    let elapsed = t0.elapsed();
    assert!(hybrid_bb.is_finite());
    assert!(
        elapsed.as_millis() < 50,
        "N=40 hybrid DAG+cycle must evaluate in < 50ms, took {elapsed:?}"
    );
}

#[test]
fn w1b_r20_single_and_double_arc_envelope_base_h_gt_cage_h_and_symbol() {
    let mut doc = Document::new("R20 Arc Envelope", 500.0, 500.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;
    let cage = vec![
        Point2D::new(0.0, 350.0),
        Point2D::new(100.0, 350.0),
        Point2D::new(100.0, 370.0),
        Point2D::new(0.0, 370.0),
    ];

    // 1. SingleArc with base_h = 200 > cage_h = 20 (shifted cage y in [350..370])
    let mut single_obj = SceneObject::new(
        ObjectId(0),
        "TallSingleArc",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 100.0, 200.0,
        ))),
    );
    single_obj.effects.push(Effect::Envelope {
        mode: EnvelopeMode::SingleArc,
        cage: cage.clone(),
    });
    let s_id = doc.add_object(single_obj, Some(lid)).unwrap();
    let s_vwb = doc.visual_world_bounds(s_id).unwrap();
    // SingleArc apexes at u=0.5: 350 - 0.08*200 = 334, 370 + 0.08*200 = 386
    assert!(
        s_vwb.contains_point(Point2D::new(50.0, 334.0))
            && s_vwb.contains_point(Point2D::new(50.0, 386.0)),
        "SingleArc visual_world_bounds {s_vwb:?} must enclose deformed apexes y=334..386 when base_h=200 > cage_h=20"
    );

    // 2. DoubleArc with base_h = 200 > cage_h = 20
    let mut double_obj = SceneObject::new(
        ObjectId(0),
        "TallDoubleArc",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 100.0, 200.0,
        ))),
    );
    double_obj.effects.push(Effect::Envelope {
        mode: EnvelopeMode::DoubleArc,
        cage: cage.clone(),
    });
    let d_id = doc.add_object(double_obj, Some(lid)).unwrap();
    let d_vwb = doc.visual_world_bounds(d_id).unwrap();
    // DoubleArc apexes at u=0.25/0.75: 350 - 0.06*200 = 338, 370 + 0.06*200 = 382
    assert!(
        d_vwb.contains_point(Point2D::new(25.0, 338.0))
            && d_vwb.contains_point(Point2D::new(75.0, 382.0)),
        "DoubleArc visual_world_bounds {d_vwb:?} must enclose deformed apexes y=338..382 when base_h=200 > cage_h=20"
    );

    // 3. Rotated 30 deg object with SingleArc base_h = 200 > cage_h = 20
    let rot = Affine2D::translate(120.0, 80.0).compose(&Affine2D::rotate_deg(30.0));
    let mut rot_obj = SceneObject::new(
        ObjectId(0),
        "RotSingleArc",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 100.0, 200.0,
        ))),
    )
    .with_transform(rot);
    rot_obj.effects.push(Effect::Envelope {
        mode: EnvelopeMode::SingleArc,
        cage: cage.clone(),
    });
    let r_id = doc.add_object(rot_obj, Some(lid)).unwrap();
    let r_vwb = doc.visual_world_bounds(r_id).unwrap();
    for local_apex in [
        Point2D::new(50.0, 334.0),
        Point2D::new(50.0, 386.0),
        Point2D::new(0.0, 350.0),
        Point2D::new(100.0, 370.0),
    ] {
        let wp = rot.transform_point(local_apex);
        assert!(
            r_vwb.contains_point(wp),
            "Rotated SingleArc visual_world_bounds {r_vwb:?} must enclose world apex {wp:?}"
        );
    }

    // 4. Inside SymbolInstance with SingleArc base_h = 200 > cage_h = 20
    let mut sym_shape = SceneObject::new(
        ObjectId(1),
        "SymArc",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 100.0, 200.0,
        ))),
    );
    sym_shape.effects.push(Effect::Envelope {
        mode: EnvelopeMode::SingleArc,
        cage,
    });
    let sym = SymbolDef::new("ArcEnvSym", vec![sym_shape]);
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    let inst_xf = Affine2D::translate(40.0, 10.0).compose(&Affine2D::rotate_deg(45.0));
    let inst = doc
        .add_object(
            SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
                .with_transform(inst_xf),
            Some(lid),
        )
        .unwrap();
    let inst_vwb = doc.visual_world_bounds(inst).unwrap();
    for local_apex in [Point2D::new(50.0, 334.0), Point2D::new(50.0, 386.0)] {
        let wp = inst_xf.transform_point(local_apex);
        assert!(
            inst_vwb.contains_point(wp),
            "SymbolInstance SingleArc visual_world_bounds {inst_vwb:?} must enclose world apex {wp:?}"
        );
    }
}

#[test]
fn w1b_r21_chained_deform_bounds_and_no_origin_pollution() {
    let mut doc = Document::new("R21 Chained Deform", 600.0, 600.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;

    // 1. Chained Perspective(h=300) -> SingleArc(cage_h=20) & DoubleArc(cage_h=20)
    // Perspective expands 10x10 path to h=300, so SingleArc arc_pad = 0.08 * 300 = 24.0 -> y in [326..394]
    let persp = Effect::Perspective {
        quad: [
            Point2D::new(0.0, 0.0),
            Point2D::new(10.0, 0.0),
            Point2D::new(10.0, 300.0),
            Point2D::new(0.0, 300.0),
        ],
    };
    let arc_cage = vec![
        Point2D::new(0.0, 350.0),
        Point2D::new(100.0, 350.0),
        Point2D::new(100.0, 370.0),
        Point2D::new(0.0, 370.0),
    ];
    let mut chained_single = SceneObject::new(
        ObjectId(0),
        "PerspSingleArc",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 10.0, 10.0,
        ))),
    );
    chained_single.effects.push(persp.clone());
    chained_single.effects.push(Effect::Envelope {
        mode: EnvelopeMode::SingleArc,
        cage: arc_cage.clone(),
    });
    let cs_id = doc.add_object(chained_single, Some(lid)).unwrap();
    let cs_vwb = doc.visual_world_bounds(cs_id).unwrap();
    assert!(
        cs_vwb.contains_point(Point2D::new(50.0, 326.0))
            && cs_vwb.contains_point(Point2D::new(50.0, 394.0)),
        "Chained Perspective(h=300)->SingleArc(cage_h=20) visual_world_bounds {cs_vwb:?} must enclose apexes (50,326) and (50,394)"
    );

    let mut chained_double = SceneObject::new(
        ObjectId(0),
        "PerspDoubleArc",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 10.0, 10.0,
        ))),
    );
    chained_double.effects.push(persp);
    chained_double.effects.push(Effect::Envelope {
        mode: EnvelopeMode::DoubleArc,
        cage: arc_cage.clone(),
    });
    let cd_id = doc.add_object(chained_double, Some(lid)).unwrap();
    let cd_vwb = doc.visual_world_bounds(cd_id).unwrap();
    // DoubleArc apex: 0.06 * 300 = 18.0 -> 350 - 18 = 332, 370 + 18 = 388
    assert!(
        cd_vwb.contains_point(Point2D::new(25.0, 332.0))
            && cd_vwb.contains_point(Point2D::new(75.0, 388.0)),
        "Chained Perspective(h=300)->DoubleArc(cage_h=20) visual_world_bounds {cd_vwb:?} must enclose apexes (25,332) and (75,388)"
    );

    // 8-pt Coons Envelope(h=250) -> SingleArc(cage_h=20)
    let mut coons_single = SceneObject::new(
        ObjectId(0),
        "CoonsSingleArc",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 10.0, 10.0,
        ))),
    );
    coons_single.effects.push(Effect::Envelope {
        mode: EnvelopeMode::Unconstrained,
        cage: vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(50.0, -50.0),
            Point2D::new(100.0, 0.0),
            Point2D::new(100.0, 100.0),
            Point2D::new(100.0, 200.0),
            Point2D::new(50.0, 225.0),
            Point2D::new(0.0, 200.0),
            Point2D::new(0.0, 100.0),
        ],
    });
    coons_single.effects.push(Effect::Envelope {
        mode: EnvelopeMode::SingleArc,
        cage: arc_cage,
    });
    let coons_id = doc.add_object(coons_single, Some(lid)).unwrap();
    let coons_vwb = doc.visual_world_bounds(coons_id).unwrap();
    assert!(
        coons_vwb.min_y <= 332.0 && coons_vwb.max_y >= 388.0,
        "Chained 8-pt Coons->SingleArc visual_world_bounds {coons_vwb:?} must reflect Coons Y-span"
    );

    // 2. challenger2-r20-1: No origin-(0,0) pollution for Envelope/Perspective away from origin
    let far_cage = vec![
        Point2D::new(100.0, 100.0),
        Point2D::new(160.0, 100.0),
        Point2D::new(160.0, 150.0),
        Point2D::new(100.0, 150.0),
    ];
    let mut far_shape = SceneObject::new(
        ObjectId(0),
        "FarShape",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            100.0, 100.0, 40.0, 30.0,
        ))),
    );
    far_shape.effects.push(Effect::Envelope {
        mode: EnvelopeMode::Straight,
        cage: far_cage.clone(),
    });
    let fs_id = doc.add_object(far_shape, Some(lid)).unwrap();
    let fs_vwb = doc.visual_world_bounds(fs_id).unwrap();
    assert!(
        fs_vwb.min_x >= 99.0 && fs_vwb.min_y >= 99.0,
        "Far Shape+Envelope must NOT pollute bounds with (0,0), got {fs_vwb:?}"
    );

    let g_child = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "FarChild",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    100.0, 100.0, 40.0, 30.0,
                ))),
            ),
            Some(lid),
        )
        .unwrap();
    let far_grp = doc.group_objects(&[g_child], "FarGrp").unwrap();
    apply_command(
        &mut doc,
        &CommandOp::AddEffect {
            id: far_grp,
            effect: Effect::Envelope {
                mode: EnvelopeMode::Straight,
                cage: far_cage,
            },
        },
    )
    .unwrap();
    let fg_vwb = doc.visual_world_bounds(far_grp).unwrap();
    assert!(
        fg_vwb.min_x >= 95.0
            && fg_vwb.min_y >= 95.0
            && fg_vwb.max_x >= 160.0
            && fg_vwb.max_y >= 150.0,
        "Far Group+Envelope must NOT pollute bounds with (0,0), got {fg_vwb:?}"
    );
}

#[test]
fn w1b_r21_path_metrics_grouped_powerclip_symbol_and_cycle_guard() {
    let mut doc = Document::new("R21 PathMetrics", 300.0, 300.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;

    // 1. Child 20x10 inside Group with scale(2.0, 3.0) * rotate(30 deg) -> world 40x30 (len=140, area=1200)
    let c1 = doc
        .add_object(
            SceneObject::new(ObjectId(0), "RectChild", rect_shape(20.0, 10.0)),
            Some(lid),
        )
        .unwrap();
    let grp = doc.group_objects(&[c1], "ScaledRotGroup").unwrap();
    let xf = Affine2D::translate(50.0, 60.0)
        .compose(&Affine2D::rotate_deg(30.0))
        .compose(&Affine2D::scale(2.0, 3.0));
    doc.set_object_transform(grp, xf).unwrap();

    let m = doc.path_metrics(&[c1]);
    assert!(
        (m.total_length_mm - 140.0).abs() < 0.5 && (m.total_area_mm2 - 1200.0).abs() < 2.0,
        "Document::path_metrics(&[grouped_child]) must apply parent Group transform (exp len=140, area=1200), got {m:?}"
    );

    // 2. Child inside translated+scaled PowerClip
    let pc_child = doc
        .add_object(
            SceneObject::new(ObjectId(0), "PCChild", rect_shape(10.0, 10.0)),
            Some(lid),
        )
        .unwrap();
    let pc_cont = doc
        .add_object(
            SceneObject::new(ObjectId(0), "PCCont", rect_shape(30.0, 30.0)),
            Some(lid),
        )
        .unwrap();
    let pc = doc.create_powerclip(pc_cont, &[pc_child]).unwrap();
    doc.set_object_transform(
        pc,
        Affine2D::translate(10.0, 10.0).compose(&Affine2D::scale(2.0, 2.0)),
    )
    .unwrap();
    let pcm = doc.path_metrics(&[pc_child]);
    assert!(
        (pcm.total_length_mm - 80.0).abs() < 0.5 && (pcm.total_area_mm2 - 400.0).abs() < 1.0,
        "Document::path_metrics(&[pc_child]) must apply parent PowerClip transform (exp len=80, area=400), got {pcm:?}"
    );

    // 3. SymbolInstance -> Group -> 10x20 rect, scaled by 2x -> world 20x40 (len=120, area=800)
    let sym_leaf = SceneObject::new(ObjectId(1), "SymLeaf", rect_shape(10.0, 20.0));
    let sym_grp = SceneObject::new(ObjectId(2), "SymGrp", NodePayload::Group(vec![ObjectId(1)]));
    let sym = SymbolDef::new("MetricsSym", vec![sym_leaf, sym_grp]);
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    let inst = doc
        .add_object(
            SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::scale(2.0, 2.0)),
            Some(lid),
        )
        .unwrap();
    let sm = doc.path_metrics(&[inst]);
    assert!(
        (sm.total_length_mm - 120.0).abs() < 0.5 && (sm.total_area_mm2 - 800.0).abs() < 1.5,
        "Document::path_metrics(&[sym_inst]) must expand SymbolInstance (exp len=120, area=800), got {sm:?}"
    );

    // 4. Mutually recursive SymbolDefs S1 <-> S2 must terminate cleanly
    let mut s1 = SymbolDef::new("S1", vec![]);
    let mut s2 = SymbolDef::new("S2", vec![]);
    let s1_id = s1.id;
    let s2_id = s2.id;
    s1.objects.push(SceneObject::new(
        ObjectId(1),
        "RefS2",
        NodePayload::SymbolInstance(s2_id),
    ));
    s1.objects
        .push(SceneObject::new(ObjectId(2), "Leaf1", rect_shape(5.0, 5.0)));
    s2.objects.push(SceneObject::new(
        ObjectId(1),
        "RefS1",
        NodePayload::SymbolInstance(s1_id),
    ));
    doc.insert_symbol(s1);
    doc.insert_symbol(s2);
    let cyc_inst = doc
        .add_object(
            SceneObject::new(ObjectId(0), "CycInst", NodePayload::SymbolInstance(s1_id)),
            Some(lid),
        )
        .unwrap();
    let cm = doc.path_metrics(&[cyc_inst]);
    assert!(
        (cm.total_length_mm - 20.0).abs() < 0.5,
        "Mutually recursive SymbolDef path_metrics must terminate and measure leaf (len=20), got {cm:?}"
    );
}

#[test]
fn w1b_r21_symbol_external_connector_and_text_on_path_bounds() {
    let mut doc = Document::new("R21 Sym External Refs", 500.0, 500.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;

    let ext_a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ExtA", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(10.0, 10.0)),
            Some(lid),
        )
        .unwrap();
    let ext_b = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ExtB", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(200.0, 150.0)),
            Some(lid),
        )
        .unwrap();
    let ext_host = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ExtHost",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(20.0, 220.0),
                    Point2D::new(180.0, 220.0),
                )),
            ),
            Some(lid),
        )
        .unwrap();

    let sym = SymbolDef::new(
        "ExtRefSym",
        vec![
            SceneObject::new(
                ObjectId(1),
                "SymConn",
                NodePayload::Connector(ConnectorObject::between_objects(
                    ext_a,
                    AnchorPoint::Center,
                    ext_b,
                    AnchorPoint::Center,
                    ConnectorStyle::Straight,
                )),
            ),
            SceneObject::new(
                ObjectId(2),
                "SymTOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("EXTERNAL", TextStyle::default()),
                    path_id: ext_host,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
        ],
    );
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    let inst = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ExtSymInst",
                NodePayload::SymbolInstance(sym_id),
            ),
            Some(lid),
        )
        .unwrap();

    let bb = doc.world_object_bounds(inst).unwrap();
    assert!(
        bb.contains_point(Point2D::new(205.0, 155.0))
            && bb.contains_point(Point2D::new(180.0, 220.0)),
        "SymbolInstance bounds {bb:?} must include external Connector endpoint (205,155) and TextOnPath host (180,220)"
    );

    let spatial = doc.build_spatial_index(0);
    let hits = spatial.query_rect(Rect2D::from_xywh(195.0, 145.0, 20.0, 20.0));
    assert!(
        hits.contains(&inst),
        "SpatialIndex must index SymbolInstance at external Connector endpoint, hits={hits:?}"
    );
}

#[test]
fn w1b_r22_scaled_distort_and_asymmetric_coons_envelope_bounds() {
    let mut doc = Document::new("R22 Distort & Coons", 1000.0, 1000.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;

    let base_rect = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 10.0, 10.0));
    let distort = Effect::Distort {
        kind: DistortKind::PushPull,
        amplitude: 50.0,
        frequency: 1.0,
        center: Point2D::new(5.0, 5.0),
    };
    let expected_distort_bb = Effect::apply_distort_to_path(
        &base_rect,
        DistortKind::PushPull,
        50.0,
        1.0,
        Point2D::new(5.0, 5.0),
    )
    .transformed(&Affine2D::scale(10.0, 10.0))
    .bounding_box()
    .unwrap();

    // 1a. Standalone 10x scaled Distort object
    let mut standalone = SceneObject::new(
        ObjectId(0),
        "ScaledDistort",
        NodePayload::Path(base_rect.clone()),
    )
    .with_transform(Affine2D::scale(10.0, 10.0));
    standalone.effects.push(distort.clone());
    let st_id = doc.add_object(standalone, Some(lid)).unwrap();
    let st_vwb = doc.visual_world_bounds(st_id).unwrap();
    assert!(
        st_vwb.contains_rect(expected_distort_bb),
        "10x scaled standalone Distort visual_world_bounds {st_vwb:?} must enclose {expected_distort_bb:?}"
    );

    // 1b. Group scaled 10x around Distort child
    let mut grp_child = SceneObject::new(
        ObjectId(0),
        "DistortChild",
        NodePayload::Path(base_rect.clone()),
    );
    grp_child.effects.push(distort.clone());
    let gc_id = doc.add_object(grp_child, Some(lid)).unwrap();
    let grp = doc.group_objects(&[gc_id], "ScaledDistortGrp").unwrap();
    doc.set_object_transform(grp, Affine2D::scale(10.0, 10.0))
        .unwrap();
    let grp_vwb = doc.visual_world_bounds(grp).unwrap();
    assert!(
        grp_vwb.contains_rect(expected_distort_bb),
        "10x scaled Group around Distort child visual_world_bounds {grp_vwb:?} must enclose {expected_distort_bb:?}"
    );

    // 1c. SymbolInstance scaled 10x around Distort child
    let mut sym_child = SceneObject::new(
        ObjectId(1),
        "SymDistortChild",
        NodePayload::Path(base_rect.clone()),
    );
    sym_child.effects.push(distort);
    let sym = SymbolDef::new("DistortSym", vec![sym_child]);
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    let s_inst = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "SymDistortInst",
                NodePayload::SymbolInstance(sym_id),
            )
            .with_transform(Affine2D::scale(10.0, 10.0)),
            Some(lid),
        )
        .unwrap();
    let sym_vwb = doc.visual_world_bounds(s_inst).unwrap();
    assert!(
        sym_vwb.contains_rect(expected_distort_bb),
        "10x scaled SymbolInstance around Distort child visual_world_bounds {sym_vwb:?} must enclose {expected_distort_bb:?}"
    );

    // 2. Asymmetric 8-pt Coons Envelope surface extremum (-102.08)
    let mut coons = SceneObject::new(ObjectId(0), "AsymCoons", NodePayload::Path(base_rect));
    coons.effects.push(Effect::Envelope {
        mode: EnvelopeMode::Unconstrained,
        cage: vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(50.0, -100.0),
            Point2D::new(0.0, 0.0),
            Point2D::new(100.0, -100.0),
            Point2D::new(100.0, 100.0),
            Point2D::new(50.0, -100.0),
            Point2D::new(0.0, 100.0),
            Point2D::new(0.0, -100.0),
        ],
    });
    let coons_id = doc.add_object(coons, Some(lid)).unwrap();
    let coons_vwb = doc.visual_world_bounds(coons_id).unwrap();
    assert!(
        coons_vwb.min_y <= -102.08,
        "Asymmetric 8-pt Coons Envelope visual_world_bounds {coons_vwb:?} must enclose -102.08"
    );
}

#[test]
fn w1b_r22_group_symbol_instance_external_connector_and_text_on_path_local_bounds() {
    let mut doc = Document::new(
        "R22 GrpSymLocalBounds",
        1000.0,
        1000.0,
        ColorMode::Rgb,
        96.0,
    );
    let lid = doc.pages()[0].layers[0].id;

    let ext_a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ExtA", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(15.0, 15.0)),
            Some(lid),
        )
        .unwrap();
    let ext_host = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ExtHost",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(20.0, 220.0),
                    Point2D::new(180.0, 220.0),
                )),
            ),
            Some(lid),
        )
        .unwrap();

    let sym = SymbolDef::new(
        "ExtLocalSym",
        vec![
            SceneObject::new(ObjectId(101), "SymBox", rect_shape(10.0, 10.0)),
            SceneObject::new(
                ObjectId(102),
                "SymExtConn",
                NodePayload::Connector(ConnectorObject::between_objects(
                    ext_a,
                    AnchorPoint::TopLeft,
                    ObjectId(101),
                    AnchorPoint::Center,
                    ConnectorStyle::Straight,
                )),
            ),
            SceneObject::new(
                ObjectId(103),
                "SymExtTOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("EXT", TextStyle::default()),
                    path_id: ext_host,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
        ],
    );
    let sym_id = sym.id;
    doc.insert_symbol(sym);

    let inst = doc
        .add_object(
            SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
                .with_transform(Affine2D::translate(300.0, 300.0)),
            Some(lid),
        )
        .unwrap();
    let grp = doc.group_objects(&[inst], "WrapGrp").unwrap();

    // 1. Translated Group(500, 500)
    doc.set_object_transform(grp, Affine2D::translate(500.0, 500.0))
        .unwrap();
    let tr_lbb = doc.local_object_bounds(grp).unwrap();
    assert!(
        tr_lbb.contains_point(Point2D::new(-485.0, -485.0))
            && tr_lbb.contains_point(Point2D::new(-320.0, -280.0))
            && tr_lbb.contains_point(Point2D::new(310.0, 310.0)),
        "Translated Group(SymbolInstance) local_object_bounds {tr_lbb:?} must include (-485,-485), (-320,-280), and (310,310)"
    );

    // 2. Translated + Rotated Group(500, 500) * rotate(30)
    let rot_xf = Affine2D::translate(500.0, 500.0).compose(&Affine2D::rotate_deg(30.0));
    doc.set_object_transform(grp, rot_xf).unwrap();
    let inv_rot = rot_xf.inverse().unwrap();
    let exp_ext_a = inv_rot.transform_point(Point2D::new(15.0, 15.0));
    let exp_ext_host_end = inv_rot.transform_point(Point2D::new(180.0, 220.0));
    let rot_lbb = doc.local_object_bounds(grp).unwrap();
    assert!(
        rot_lbb.contains_point(exp_ext_a)
            && rot_lbb.contains_point(exp_ext_host_end)
            && rot_lbb.contains_point(Point2D::new(310.0, 310.0)),
        "Rotated Group(SymbolInstance) local_object_bounds {rot_lbb:?} must enclose {exp_ext_a:?}, {exp_ext_host_end:?}, and (310,310)"
    );
}

#[test]
fn w1b_r23_twister_rotational_bounds_and_standalone_symbol_instance_local_bounds() {
    // 1a. 1x origin-corner DistortKind::Twister (challenger1-r22-1)
    let mut doc = Document::new(
        "R23 Twister & SymLocal",
        2000.0,
        2000.0,
        ColorMode::Rgb,
        96.0,
    );
    let lid = doc.pages()[0].layers[0].id;

    let rect_100 = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 100.0, 100.0));
    let tw_1x = Effect::Distort {
        kind: DistortKind::Twister,
        amplitude: 5.0,
        frequency: 20.0,
        center: Point2D::ZERO,
    };
    let mut obj_1x = SceneObject::new(
        ObjectId(0),
        "Twister1x",
        NodePayload::Path(rect_100.clone()),
    );
    obj_1x.effects.push(tw_1x);
    let id_1x = doc.add_object(obj_1x, Some(lid)).unwrap();
    let actual_1x =
        Effect::apply_distort_to_path(&rect_100, DistortKind::Twister, 5.0, 20.0, Point2D::ZERO)
            .bounding_box()
            .unwrap();
    let vwb_1x = doc.visual_world_bounds(id_1x).unwrap();
    assert!(
        vwb_1x.contains_rect(actual_1x),
        "1x origin-corner Twister visual_world_bounds {vwb_1x:?} must enclose {actual_1x:?}"
    );

    // 1b. 10x scaled + 37 deg rotated off-center Twister across standalone, with_parent, Group, SymbolInstance
    let rect_10 = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 10.0, 10.0));
    let xf_10x = Affine2D::scale(10.0, 10.0).compose(&Affine2D::rotate_deg(37.0));
    let tw_off = Effect::Distort {
        kind: DistortKind::Twister,
        amplitude: 10.0,
        frequency: 10.0,
        center: Point2D::new(-100.0, 5.0),
    };
    let actual_off = Effect::apply_distort_to_path(
        &rect_10,
        DistortKind::Twister,
        10.0,
        10.0,
        Point2D::new(-100.0, 5.0),
    )
    .transformed(&xf_10x)
    .bounding_box()
    .unwrap();

    let mut st_off = SceneObject::new(
        ObjectId(0),
        "Twister10x",
        NodePayload::Path(rect_10.clone()),
    )
    .with_transform(xf_10x);
    st_off.effects.push(tw_off.clone());
    let st_off_id = doc.add_object(st_off, Some(lid)).unwrap();
    let st_off_vwb = doc.visual_world_bounds(st_off_id).unwrap();
    assert!(
        st_off_vwb.contains_rect(actual_off),
        "10x rotated off-center Twister standalone vwb {st_off_vwb:?} must enclose {actual_off:?}"
    );

    let mut plain_tw = SceneObject::new(
        ObjectId(0),
        "TwisterChild",
        NodePayload::Path(rect_10.clone()),
    );
    plain_tw.effects.push(tw_off.clone());
    let plain_tw_id = doc.add_object(plain_tw, Some(lid)).unwrap();
    let with_parent_vwb = doc
        .visual_world_bounds_with_parent(plain_tw_id, xf_10x)
        .unwrap();
    assert!(
        with_parent_vwb.contains_rect(actual_off),
        "10x rotated off-center Twister visual_world_bounds_with_parent {with_parent_vwb:?} must enclose {actual_off:?}"
    );
    let grp_tw = doc.group_objects(&[plain_tw_id], "TwisterGrp").unwrap();
    doc.set_object_transform(grp_tw, xf_10x).unwrap();
    let grp_vwb = doc.visual_world_bounds(grp_tw).unwrap();
    assert!(
        grp_vwb.contains_rect(actual_off),
        "10x rotated off-center Twister Group vwb {grp_vwb:?} must enclose {actual_off:?}"
    );

    let mut sym_tw = SceneObject::new(ObjectId(1), "SymTw", NodePayload::Path(rect_10));
    sym_tw.effects.push(tw_off);
    let tw_sym = SymbolDef::new("TwisterSym", vec![sym_tw]);
    let tw_sym_id = tw_sym.id;
    doc.insert_symbol(tw_sym);
    let tw_inst = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "TwisterSymInst",
                NodePayload::SymbolInstance(tw_sym_id),
            )
            .with_transform(xf_10x),
            Some(lid),
        )
        .unwrap();
    let tw_inst_vwb = doc.visual_world_bounds(tw_inst).unwrap();
    assert!(
        tw_inst_vwb.contains_rect(actual_off),
        "10x rotated off-center Twister SymbolInstance vwb {tw_inst_vwb:?} must enclose {actual_off:?}"
    );

    // 2. Standalone rotated+translated SymbolInstance local_object_bounds with external Connector & TextOnPath (challenger1-r22-2)
    let ext_a = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ExtA", rect_shape(10.0, 10.0))
                .with_transform(Affine2D::translate(15.0, 15.0)),
            Some(lid),
        )
        .unwrap();
    let ext_host = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ExtHost",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(20.0, 220.0),
                    Point2D::new(180.0, 220.0),
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let sym = SymbolDef::new(
        "StandaloneExtSym",
        vec![
            SceneObject::new(ObjectId(201), "SymBox", rect_shape(10.0, 10.0)),
            SceneObject::new(
                ObjectId(202),
                "SymExtConn",
                NodePayload::Connector(ConnectorObject::between_objects(
                    ext_a,
                    AnchorPoint::TopLeft,
                    ObjectId(201),
                    AnchorPoint::Center,
                    ConnectorStyle::Straight,
                )),
            ),
            SceneObject::new(
                ObjectId(203),
                "SymExtTOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("EXT", TextStyle::default()),
                    path_id: ext_host,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
        ],
    );
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    let rot_tr = Affine2D::translate(500.0, 500.0).compose(&Affine2D::rotate_deg(30.0));
    let standalone_inst = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "StandaloneSymInst",
                NodePayload::SymbolInstance(sym_id),
            )
            .with_transform(rot_tr),
            Some(lid),
        )
        .unwrap();
    let inv_rot = rot_tr.inverse().unwrap();
    let exp_ext_a = inv_rot.transform_point(Point2D::new(15.0, 15.0));
    let exp_ext_host_end = inv_rot.transform_point(Point2D::new(180.0, 220.0));
    let st_lbb = doc.local_object_bounds(standalone_inst).unwrap();
    assert!(
        st_lbb.contains_point(exp_ext_a)
            && st_lbb.contains_point(exp_ext_host_end)
            && st_lbb.contains_point(Point2D::new(10.0, 10.0)),
        "Standalone rotated SymbolInstance local_object_bounds {st_lbb:?} must enclose {exp_ext_a:?}, {exp_ext_host_end:?}, and (10,10)"
    );
    assert!(
        st_lbb.max_x < 100.0 && st_lbb.max_y < 100.0,
        "Standalone rotated SymbolInstance local_object_bounds {st_lbb:?} must not leak raw world coords (180,220)"
    );

    // 3. R1 minor: sym_local_object_bounds inside rotated SymbolDef Group(SingleArc) with TextOnPath & Connector
    let mut inner_sym = SymbolDef::new(
        "InnerExtSym",
        vec![SceneObject::new(
            ObjectId(301),
            "InnerBox",
            rect_shape(10.0, 10.0),
        )],
    );
    inner_sym.objects.push(SceneObject::new(
        ObjectId(302),
        "InnerExtConn",
        NodePayload::Connector(ConnectorObject::between_objects(
            ext_a,
            AnchorPoint::Center,
            ObjectId(301),
            AnchorPoint::Center,
            ConnectorStyle::Straight,
        )),
    ));
    let inner_sym_id = inner_sym.id;
    doc.insert_symbol(inner_sym);

    let mut sym_grp = SceneObject::new(
        ObjectId(401),
        "SymArcGrp",
        NodePayload::Group(vec![ObjectId(402), ObjectId(403), ObjectId(404)]),
    );
    sym_grp.effects.push(Effect::Envelope {
        mode: EnvelopeMode::SingleArc,
        cage: vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(20.0, 0.0),
            Point2D::new(20.0, 20.0),
            Point2D::new(0.0, 20.0),
        ],
    });
    let outer_sym = SymbolDef::new(
        "OuterArcSym",
        vec![
            SceneObject::new(
                ObjectId(402),
                "GrpExtTOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("ARC", TextStyle::default()),
                    path_id: ext_host,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            SceneObject::new(
                ObjectId(403),
                "GrpExtConn",
                NodePayload::Connector(ConnectorObject::between_objects(
                    ext_a,
                    AnchorPoint::TopLeft,
                    ext_host,
                    AnchorPoint::Center,
                    ConnectorStyle::Straight,
                )),
            ),
            SceneObject::new(
                ObjectId(404),
                "GrpNestedSym",
                NodePayload::SymbolInstance(inner_sym_id),
            ),
            sym_grp,
        ],
    );
    let outer_sym_id = outer_sym.id;
    doc.insert_symbol(outer_sym);
    let outer_inst = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "OuterArcInst",
                NodePayload::SymbolInstance(outer_sym_id),
            )
            .with_transform(Affine2D::rotate_deg(45.0)),
            Some(lid),
        )
        .unwrap();
    let outer_vwb = doc.visual_world_bounds(outer_inst).unwrap();
    assert!(
        outer_vwb.is_finite() && outer_vwb.contains_point(Point2D::new(15.0, 15.0)),
        "Rotated SymbolInstance wrapping Group(SingleArc) with external TextOnPath/Connector/SymbolInstance must yield valid finite bounds: {outer_vwb:?}"
    );
}

#[test]
fn w1b_r26_symbol_zero_id_containers_and_boolean_op_local_coords() {
    // 1. challenger1-r25-1: ObjectId(0) Group & PowerClip containers in SymbolDef propagate transforms
    //    to descendant TextOnPath and Connector in world_object_bounds & visual_world_bounds.
    let mut doc = Document::new_a4("R26 ZeroId Sym Containers");
    let lid = doc.pages()[0].layers[0].id;

    let sym_path = SceneObject::new(
        ObjectId(10),
        "SymPath",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(0.0, 0.0),
            Point2D::new(40.0, 0.0),
        )),
    );
    let sym_top = SceneObject::new(
        ObjectId(11),
        "SymTOP",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain("ZERO", TextStyle::default()),
            path_id: ObjectId(10),
            offset_mm: 0.0,
            distance_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    let sym_conn = SceneObject::new(
        ObjectId(12),
        "SymConn",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(ObjectId(10)),
            from_anchor: AnchorPoint::Right,
            from_fallback: Point2D::ZERO,
            to_obj: None,
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(50.0, 10.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 0.0,
        }),
    );
    let zero_grp = SceneObject::new(
        ObjectId(0),
        "ZeroIdGroup",
        NodePayload::Group(vec![ObjectId(10), ObjectId(11), ObjectId(12)]),
    )
    .with_transform(Affine2D::translate(100.0, 50.0));
    let sym_grp = SymbolDef::new("ZeroIdGrpSym", vec![sym_path, sym_top, sym_conn, zero_grp]);
    let sym_grp_id = sym_grp.id;
    doc.insert_symbol(sym_grp);
    let inst_grp = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "InstZeroGrp",
                NodePayload::SymbolInstance(sym_grp_id),
            ),
            Some(lid),
        )
        .unwrap();

    let wb_grp = doc.world_object_bounds(inst_grp).unwrap();
    let vwb_grp = doc.visual_world_bounds(inst_grp).unwrap();
    assert!(
        wb_grp.min_x >= 99.0 && wb_grp.min_y >= 49.0,
        "ObjectId(0) Group in SymbolDef must propagate translate(100,50) to TextOnPath/Connector: {wb_grp:?}"
    );
    assert!(
        vwb_grp.min_x >= 80.0 && vwb_grp.min_y >= 30.0,
        "visual_world_bounds must propagate translate(100,50) without leaking origin: {vwb_grp:?}"
    );

    let pc_p = SceneObject::new(
        ObjectId(20),
        "PCPath",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(0.0, 0.0),
            Point2D::new(20.0, 0.0),
        )),
    );
    let pc_top = SceneObject::new(
        ObjectId(21),
        "PCTOP",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain("PC", TextStyle::default()),
            path_id: ObjectId(20),
            offset_mm: 0.0,
            distance_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    let zero_pc = SceneObject::new(
        ObjectId(0),
        "ZeroIdPC",
        NodePayload::PowerClip {
            container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 30.0, 20.0,
            ))),
            container_parametric: None,
            contents: vec![ObjectId(20), ObjectId(21)],
            lock_contents: false,
        },
    )
    .with_transform(Affine2D::translate(80.0, 40.0));
    let sym_pc = SymbolDef::new("ZeroIdPCSym", vec![pc_p, pc_top, zero_pc]);
    let sym_pc_id = sym_pc.id;
    doc.insert_symbol(sym_pc);
    let inst_pc = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "InstZeroPC",
                NodePayload::SymbolInstance(sym_pc_id),
            ),
            Some(lid),
        )
        .unwrap();
    let wb_pc = doc.world_object_bounds(inst_pc).unwrap();
    assert!(
        wb_pc.min_x >= 79.0 && wb_pc.min_y >= 39.0,
        "ObjectId(0) PowerClip in SymbolDef must propagate translate(80,40): {wb_pc:?}"
    );

    // 2. challenger1-r25-2: BooleanOp bakes object-local Fill/Effect coordinates when baking transform
    let mut hist = CommandHistory::new(32);
    let mut obj1 = SceneObject::new(
        ObjectId(0),
        "Src1",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 20.0, 20.0,
        ))),
    )
    .with_transform(Affine2D::translate(100.0, 50.0))
    .with_fill(Fill::LinearGradient {
        start: Point2D::new(0.0, 0.0),
        end: Point2D::new(20.0, 0.0),
        stops: vec![
            GradientStop::new(0.0, Color::rgba8(255, 0, 0, 255)),
            GradientStop::new(1.0, Color::rgba8(0, 0, 255, 255)),
        ],
    });
    obj1.effects.push(Effect::Perspective {
        quad: [
            Point2D::new(-2.0, 0.0),
            Point2D::new(22.0, 0.0),
            Point2D::new(22.0, 22.0),
            Point2D::new(-2.0, 22.0),
        ],
    });
    obj1.effects.push(Effect::Distort {
        kind: DistortKind::Twister,
        amplitude: 30.0,
        frequency: 1.0,
        center: Point2D::new(10.0, 10.0),
    });
    obj1.effects.push(Effect::Transparency {
        mode: TransparencyMode::FountainLinear {
            start: Point2D::new(0.0, 0.0),
            end: Point2D::new(20.0, 0.0),
            start_alpha: 1.0,
            end_alpha: 0.2,
        },
        blend_mode: BlendMode::Normal,
    });
    let id1 = doc.add_object(obj1, Some(lid)).unwrap();
    let id2 = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Src2",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    110.0, 50.0, 20.0, 20.0,
                ))),
            ),
            Some(lid),
        )
        .unwrap();

    hist.execute(
        &mut doc,
        CommandOp::BooleanOp {
            op: BooleanOp::Weld,
            ids: vec![id1, id2],
        },
    )
    .unwrap();

    let rid = *doc
        .objects()
        .keys()
        .find(|id| doc.object(**id).is_some_and(|o| o.name == "Src1"))
        .unwrap();
    let res = doc.object(rid).unwrap();
    assert_eq!(res.transform, Affine2D::IDENTITY);
    match &res.fill {
        Fill::LinearGradient { start, end, .. } => {
            assert!((start.x - 100.0).abs() < 1e-6 && (start.y - 50.0).abs() < 1e-6);
            assert!((end.x - 120.0).abs() < 1e-6 && (end.y - 50.0).abs() < 1e-6);
        }
        other => panic!("expected LinearGradient, got {other:?}"),
    }
    match &res.effects[0] {
        Effect::Perspective { quad } => {
            assert!((quad[0].x - 98.0).abs() < 1e-6 && (quad[0].y - 50.0).abs() < 1e-6);
            assert!((quad[2].x - 122.0).abs() < 1e-6 && (quad[2].y - 72.0).abs() < 1e-6);
        }
        other => panic!("expected Perspective, got {other:?}"),
    }
    match &res.effects[1] {
        Effect::Distort { center, .. } => {
            assert!((center.x - 110.0).abs() < 1e-6 && (center.y - 60.0).abs() < 1e-6);
        }
        other => panic!("expected Distort, got {other:?}"),
    }
    match &res.effects[2] {
        Effect::Transparency {
            mode: TransparencyMode::FountainLinear { start, end, .. },
            ..
        } => {
            assert!((start.x - 100.0).abs() < 1e-6 && (start.y - 50.0).abs() < 1e-6);
            assert!((end.x - 120.0).abs() < 1e-6 && (end.y - 50.0).abs() < 1e-6);
        }
        other => panic!("expected Transparency, got {other:?}"),
    }
    let vwb = doc.visual_world_bounds(rid).unwrap();
    let vwb_center = vwb.center();
    assert!(
        (vwb_center.x - 110.0).abs() < 2.0
            && (vwb_center.y - 60.0).abs() < 2.0
            && vwb.min_x > 0.0
            && vwb.max_x < 210.0,
        "Weld result visual_world_bounds must stay centered near (110, 60), got {vwb:?}"
    );
}

#[test]
fn w1b_r28_find_replace_vector_table_border_style() {
    let mut doc = Document::new_a4("R28 Table Border FindReplace");
    let lid = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::new(50);

    let mut cut_tbl = VectorTable::new(1, 2, 20.0, 10.0);
    cut_tbl.border_style = OutlineStyle::cut_contour();
    let cut_tid = doc
        .add_object(
            SceneObject::new(ObjectId(0), "CutTable", NodePayload::Table(cut_tbl))
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none()),
            Some(lid),
        )
        .unwrap();

    let def_tbl = VectorTable::new(1, 1, 20.0, 10.0);
    let def_tid = doc
        .add_object(
            SceneObject::new(ObjectId(0), "DefaultTable", NodePayload::Table(def_tbl)),
            Some(lid),
        )
        .unwrap();

    assert_eq!(
        find_objects(
            &doc,
            &FindQuery {
                outline_color: Some(ColorMatch::exact(Color::cut_contour_spot())),
                ..FindQuery::default()
            }
        ),
        vec![cut_tid]
    );
    assert_eq!(
        find_objects(
            &doc,
            &FindQuery {
                is_hairline: Some(true),
                ..FindQuery::default()
            }
        ),
        vec![cut_tid]
    );
    assert_eq!(
        find_objects(
            &doc,
            &FindQuery {
                outline_width_min_mm: Some(0.15),
                outline_width_max_mm: Some(0.25),
                ..FindQuery::default()
            }
        ),
        vec![def_tid]
    );

    let def_query = FindQuery {
        object_kind: Some(ObjectKindFilter::Table),
        is_hairline: Some(false),
        ..FindQuery::default()
    };
    let magenta = Color::cmyk(0.0, 1.0, 0.0, 0.0);
    hist.execute(
        &mut doc,
        CommandOp::FindAndReplace {
            query: def_query.clone(),
            action: ReplaceAction::ReplaceOutlineColor {
                color: magenta.clone(),
            },
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::FindAndReplace {
            query: def_query.clone(),
            action: ReplaceAction::ReplaceOutlineWidth {
                width_mm: 0.75,
                is_hairline: false,
            },
        },
    )
    .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::FindAndReplace {
            query: def_query,
            action: ReplaceAction::ConvertOverprint { overprint: true },
        },
    )
    .unwrap();

    let NodePayload::Table(ref t2) = doc.object(def_tid).unwrap().payload else {
        panic!("expected Table");
    };
    assert_eq!(t2.border_style.color, magenta);
    assert!((t2.border_style.width_mm - 0.75).abs() < 1e-9);
    assert!(t2.border_style.overprint);
    assert_eq!(doc.object(def_tid).unwrap().outline.color, magenta);

    // Undo all 3 and verify restoration
    hist.undo(&mut doc).unwrap();
    hist.undo(&mut doc).unwrap();
    hist.undo(&mut doc).unwrap();
    let NodePayload::Table(ref t2_restored) = doc.object(def_tid).unwrap().payload else {
        panic!("expected Table");
    };
    assert_eq!(t2_restored.border_style, OutlineStyle::default());
}

#[test]
fn w1c_carryovers_set_fill_text_runs_set_outline_table_group_outline_none_and_invalidation() {
    let mut doc = Document::new("W1C Carryovers", 210.0, 297.0, ColorMode::Cmyk, 300.0);
    let mut hist = CommandHistory::default();

    // 1. Carryover 1: SetFill & SetObjectStyleLink sync run.style.fill across ArtisticText, ParagraphText, TextOnPath
    let host_path = SceneObject::new(
        ObjectId(0),
        "HostPath",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(10.0, 10.0),
            Point2D::new(90.0, 10.0),
        )),
    );
    let host_id = doc.add_object(host_path, None).unwrap();

    let art_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ArtText",
                NodePayload::ArtisticText(ArtisticText::from_plain("Hello", TextStyle::default())),
            ),
            None,
        )
        .unwrap();
    let para_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ParaText",
                NodePayload::ParagraphText(ParagraphText::new(
                    "Paragraph",
                    Vec2D::new(60.0, 25.0),
                    TextStyle::default(),
                )),
            ),
            None,
        )
        .unwrap();
    let top_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "PathText",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("Curved", TextStyle::default()),
                    path_id: host_id,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            None,
        )
        .unwrap();

    let red = Color::rgba8(220, 40, 20, 255);
    hist.execute(
        &mut doc,
        CommandOp::SetFill {
            ids: vec![art_id, para_id, top_id],
            fill: Fill::Solid(red.clone()),
        },
    )
    .unwrap();
    for tid in [art_id, para_id, top_id] {
        let obj = doc.object(tid).unwrap();
        assert_eq!(obj.fill, Fill::Solid(red.clone()));
        for r in text_runs_ref(&obj.payload) {
            assert_eq!(r.style.fill, red);
        }
    }

    hist.execute(
        &mut doc,
        CommandOp::SetFill {
            ids: vec![art_id, para_id, top_id],
            fill: Fill::None,
        },
    )
    .unwrap();
    for tid in [art_id, para_id, top_id] {
        let obj = doc.object(tid).unwrap();
        assert_eq!(obj.fill, Fill::None);
        for r in text_runs_ref(&obj.payload) {
            assert_eq!(r.style.fill, Color::TRANSPARENT);
        }
    }

    hist.undo(&mut doc).unwrap();
    for tid in [art_id, para_id, top_id] {
        for r in text_runs_ref(&doc.object(tid).unwrap().payload) {
            assert_eq!(r.style.fill, red);
        }
    }
    hist.redo(&mut doc).unwrap();
    for tid in [art_id, para_id, top_id] {
        for r in text_runs_ref(&doc.object(tid).unwrap().payload) {
            assert_eq!(r.style.fill, Color::TRANSPARENT);
        }
    }

    // Sibling SetObjectStyleLink also syncs text run fill & table border_style
    let tbl_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Tbl",
                NodePayload::Table(VectorTable::new(2, 2, 20.0, 10.0)),
            ),
            None,
        )
        .unwrap();
    let cyan = Color::cmyk(1.0, 0.0, 0.0, 0.0);
    let style_outline = OutlineStyle::solid(cyan.clone(), 0.9);
    let mut style = ObjectStyle::new("BrandStyle");
    style.fill = Some(Fill::Solid(cyan.clone()));
    style.outline = Some(style_outline.clone());
    let sid = style.id;
    hist.execute(&mut doc, CommandOp::UpsertObjectStyle { style })
        .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::SetObjectStyleLink {
            ids: vec![art_id, tbl_id],
            style_id: Some(sid),
        },
    )
    .unwrap();
    for r in text_runs_ref(&doc.object(art_id).unwrap().payload) {
        assert_eq!(r.style.fill, cyan);
    }

    // 2. Carryover 2: SetOutline, SetObjectStyleLink, and EditTable sync VectorTable::border_style <-> obj.outline
    let NodePayload::Table(ref t_styled) = doc.object(tbl_id).unwrap().payload else {
        panic!("expected Table");
    };
    assert_eq!(t_styled.border_style, style_outline);
    assert_eq!(doc.object(tbl_id).unwrap().outline, style_outline);

    let green = Color::rgba8(0, 180, 90, 255);
    let new_outline = OutlineStyle::solid(green.clone(), 1.25);
    hist.execute(
        &mut doc,
        CommandOp::SetOutline {
            ids: vec![tbl_id],
            outline: new_outline.clone(),
        },
    )
    .unwrap();
    let NodePayload::Table(ref t_after_outline) = doc.object(tbl_id).unwrap().payload else {
        panic!("expected Table");
    };
    assert_eq!(t_after_outline.border_style, new_outline);
    assert_eq!(doc.object(tbl_id).unwrap().outline, new_outline);

    let mut edited_tbl = t_after_outline.clone();
    edited_tbl.border_style = OutlineStyle::solid(red.clone(), 0.4);
    hist.execute(
        &mut doc,
        CommandOp::EditTable {
            id: tbl_id,
            table: edited_tbl.clone(),
        },
    )
    .unwrap();
    assert_eq!(doc.object(tbl_id).unwrap().outline, edited_tbl.border_style);

    hist.undo(&mut doc).unwrap();
    assert_eq!(doc.object(tbl_id).unwrap().outline, new_outline);

    // 3. Carryover 3: Document::group_objects and CommandOp::Group emit OutlineStyle::none()
    let r1 = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "R1",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 20.0,
                    height: 20.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_transform(Affine2D::translate(10.0, 10.0)),
            None,
        )
        .unwrap();
    let r2 = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "R2",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 20.0,
                    height: 20.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_transform(Affine2D::translate(40.0, 10.0)),
            None,
        )
        .unwrap();
    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![r1, r2],
            name: "CleanGroup".to_owned(),
        },
    )
    .unwrap();
    let grp_id = *doc.objects().keys().max().unwrap();
    let grp_obj = doc.object(grp_id).unwrap();
    assert!(matches!(grp_obj.payload, NodePayload::Group(_)));
    assert_eq!(grp_obj.outline, OutlineStyle::none());
    assert!(!grp_obj.outline.is_visible());

    // 4. DocumentInvalidation: Targeted for object transform (including before & after rects)
    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![r1],
            affine: Affine2D::translate(100.0, 0.0),
        },
    )
    .unwrap();
    match hist.last_invalidation() {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            assert!(object_ids.contains(&r1));
            assert!(
                dirty_rects_mm.iter().any(|r| r.min_x < 25.0)
                    && dirty_rects_mm.iter().any(|r| r.max_x > 120.0),
                "Targeted invalidation must cover both before and after rects: {dirty_rects_mm:?}"
            );
        }
        DocumentInvalidation::All => panic!("Expected Targeted invalidation for TransformObjects"),
    }

    // 5. preview_find_and_replace matches find_and_replace without mutating doc
    let fq = FindQuery {
        object_kind: Some(ObjectKindFilter::AnyText),
        text_pattern: Some((
            "Hello".to_owned(),
            TextMatchMode::Contains {
                case_sensitive: true,
            },
        )),
        ..FindQuery::default()
    };
    let act = ReplaceAction::ReplaceText {
        find: "Hello".to_owned(),
        replace_with: "World".to_owned(),
        case_sensitive: true,
    };
    let preview = preview_find_and_replace(&doc, &fq, &act);
    assert_eq!(preview.matched_ids, vec![art_id]);
    assert_eq!(preview.modified_ids, vec![art_id]);
    assert_eq!(preview.replacements_applied, 1);
    assert_eq!(
        extract_searchable_text(&doc.object(art_id).unwrap().payload).as_deref(),
        Some("Hello")
    );
}

#[test]
fn w1c_r2_targeted_invalidation_before_bounds_and_relational_dependents() {
    use coree_core::{
        ArtisticText, ConnectorObject, ConnectorStyle, SymbolDef, TextOnPath, TextStyle,
    };

    let mut doc = Document::new_a4("W1C-R2-Invalidation");
    let lid = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::default();

    // 1. SymbolInstance move & delete -> Targeted invalidation covers both before & after rects
    let sym = SymbolDef::new(
        "InvSym",
        vec![SceneObject::new(
            ObjectId(0),
            "SymRect",
            rect_shape(20.0, 10.0),
        )],
    );
    let sym_id = sym.id;
    hist.execute(&mut doc, CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();
    let sym_inst_obj =
        SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
            .with_transform(Affine2D::translate(10.0, 10.0));
    hist.execute(&mut doc, CommandOp::create_object(sym_inst_obj, Some(lid)))
        .unwrap();
    let sym_inst_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![sym_inst_id],
            affine: Affine2D::translate(140.0, 140.0),
        },
    )
    .unwrap();
    match hist.last_invalidation() {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            assert!(object_ids.contains(&sym_inst_id));
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.min_x <= 10.5 && r.min_y <= 10.5 && r.max_x >= 29.5),
                "SymbolInstance before rect missing: {dirty_rects_mm:?}"
            );
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.min_x <= 150.5 && r.max_x >= 169.5 && r.max_y >= 159.5),
                "SymbolInstance after rect missing: {dirty_rects_mm:?}"
            );
        }
        DocumentInvalidation::All => panic!("Expected Targeted for SymbolInstance move"),
    }
    hist.undo(&mut doc).unwrap();
    assert!(matches!(
        hist.last_invalidation(),
        DocumentInvalidation::Targeted { dirty_rects_mm, .. }
            if dirty_rects_mm.iter().any(|r| r.min_x <= 10.5)
                && dirty_rects_mm.iter().any(|r| r.max_x >= 169.5)
    ));
    hist.redo(&mut doc).unwrap();

    // 2. Standalone Connector move & delete -> Targeted covers before & after
    let conn_obj = SceneObject::new(
        ObjectId(0),
        "Conn",
        NodePayload::Connector(ConnectorObject {
            from_obj: None,
            from_anchor: AnchorPoint::Center,
            from_fallback: Point2D::new(10.0, 10.0),
            to_obj: None,
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(30.0, 20.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 2.0,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(conn_obj, Some(lid)))
        .unwrap();
    let conn_id = *doc.objects().keys().max().unwrap();
    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![conn_id],
            affine: Affine2D::translate(140.0, 140.0),
        },
    )
    .unwrap();
    match hist.last_invalidation() {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            assert!(object_ids.contains(&conn_id));
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.min_x <= 10.5 && r.min_y <= 10.5),
                "Connector before rect missing: {dirty_rects_mm:?}"
            );
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.max_x >= 169.5 && r.max_y >= 159.5),
                "Connector after rect missing: {dirty_rects_mm:?}"
            );
        }
        DocumentInvalidation::All => panic!("Expected Targeted for Connector move"),
    }
    hist.execute(&mut doc, CommandOp::DeleteObjects { ids: vec![conn_id] })
        .unwrap();
    match hist.last_invalidation() {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            assert!(object_ids.contains(&conn_id));
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.max_x >= 169.5 && r.max_y >= 159.5),
                "Deleted Connector before rect missing: {dirty_rects_mm:?}"
            );
        }
        DocumentInvalidation::All => panic!("Expected Targeted for Connector delete"),
    }

    // 3. TextOnPath host path move -> invalidates both host AND dependent TextOnPath (before & after)
    let host_path = BezierPath::from_line(Point2D::new(10.0, 10.0), Point2D::new(50.0, 10.0));
    hist.execute(
        &mut doc,
        CommandOp::create_object(
            SceneObject::new(ObjectId(0), "HostPath", NodePayload::Path(host_path)),
            Some(lid),
        ),
    )
    .unwrap();
    let host_id = *doc.objects().keys().max().unwrap();
    let top_obj = SceneObject::new(
        ObjectId(0),
        "TOP",
        NodePayload::TextOnPath(TextOnPath {
            path_id: host_id,
            text: ArtisticText::from_plain("OnPath", TextStyle::default()),
            distance_mm: 2.0,
            offset_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(top_obj, Some(lid)))
        .unwrap();
    let top_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![host_id],
            affine: Affine2D::translate(140.0, 140.0),
        },
    )
    .unwrap();
    match hist.last_invalidation() {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            assert!(object_ids.contains(&host_id));
            assert!(
                object_ids.contains(&top_id),
                "Dependent TextOnPath {top_id:?} missing from object_ids: {object_ids:?}"
            );
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.min_x <= 10.0 && r.min_y <= 10.0),
                "Dependent TextOnPath before bounds missing: {dirty_rects_mm:?}"
            );
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.max_x >= 190.0 && r.max_y >= 150.0),
                "Dependent TextOnPath after bounds missing: {dirty_rects_mm:?}"
            );
        }
        DocumentInvalidation::All => panic!("Expected Targeted for TextOnPath host move"),
    }
    hist.undo(&mut doc).unwrap();
    assert!(matches!(
        hist.last_invalidation(),
        DocumentInvalidation::Targeted { object_ids, .. } if object_ids.contains(&top_id)
    ));
    hist.redo(&mut doc).unwrap();

    hist.execute(&mut doc, CommandOp::DeleteObjects { ids: vec![top_id] })
        .unwrap();
    match hist.last_invalidation() {
        DocumentInvalidation::Targeted { dirty_rects_mm, .. } => {
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.max_x >= 188.0 && r.max_y >= 148.0),
                "Deleted TextOnPath before bounds missing: {dirty_rects_mm:?}"
            );
        }
        DocumentInvalidation::All => panic!("Expected Targeted for TextOnPath delete"),
    }

    // 4. Nested deleted group child -> preserves before ancestor transform chain
    let leaf = SceneObject::new(ObjectId(0), "Leaf", rect_shape(15.0, 10.0));
    hist.execute(&mut doc, CommandOp::create_object(leaf, Some(lid)))
        .unwrap();
    let leaf_id = *doc.objects().keys().max().unwrap();
    let keep = SceneObject::new(ObjectId(0), "Keep", rect_shape(5.0, 5.0))
        .with_transform(Affine2D::translate(30.0, 0.0));
    hist.execute(&mut doc, CommandOp::create_object(keep, Some(lid)))
        .unwrap();
    let keep_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![leaf_id, keep_id],
            name: "InnerGrp".to_owned(),
        },
    )
    .unwrap();
    let inner_id = *doc.objects().keys().max().unwrap();
    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![inner_id],
            affine: Affine2D::translate(20.0, 10.0),
        },
    )
    .unwrap();
    let outer_sib = SceneObject::new(ObjectId(0), "OuterSib", rect_shape(5.0, 5.0));
    hist.execute(&mut doc, CommandOp::create_object(outer_sib, Some(lid)))
        .unwrap();
    let outer_sib_id = *doc.objects().keys().max().unwrap();
    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![inner_id, outer_sib_id],
            name: "OuterGrp".to_owned(),
        },
    )
    .unwrap();
    let outer_id = *doc.objects().keys().max().unwrap();
    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![outer_id],
            affine: Affine2D::translate(80.0, 90.0),
        },
    )
    .unwrap();

    // Leaf world position is (100..115, 100..110). Delete leaf_id directly!
    hist.execute(&mut doc, CommandOp::DeleteObjects { ids: vec![leaf_id] })
        .unwrap();
    match hist.last_invalidation() {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            assert!(object_ids.contains(&leaf_id));
            assert!(
                dirty_rects_mm.iter().any(|r| {
                    r.min_x <= 100.0 && r.max_x >= 115.0 && r.min_y <= 100.0 && r.max_y >= 110.0
                }),
                "Nested deleted child before rect must include world (100..115, 100..110): {dirty_rects_mm:?}"
            );
        }
        DocumentInvalidation::All => panic!("Expected Targeted for nested child delete"),
    }
    hist.undo(&mut doc).unwrap();
    assert!(doc.has_object(leaf_id));
    hist.redo(&mut doc).unwrap();
    assert!(!doc.has_object(leaf_id));
}

#[test]
fn w1c_r3_grouped_and_powerclip_dependents_targeted_invalidation_exec_undo_redo() {
    use coree_core::{
        ArtisticText, ConnectorObject, ConnectorStyle, Effect, SymbolDef, TextOnPath, TextStyle,
    };

    let mut doc = Document::new_a4("W1C-R3-Grouped-Dependents");
    let lid = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::default();

    // 1. Group containing:
    //    - conn_src (0..20, 0..20)
    //    - host_path (0..40 at y=30)
    //    - blend_target (0..15, 45..60)
    //    - sym_ref_rect (25..40, 0..15)
    let conn_src = SceneObject::new(ObjectId(0), "ConnSrc", rect_shape(20.0, 20.0));
    hist.execute(&mut doc, CommandOp::create_object(conn_src, Some(lid)))
        .unwrap();
    let conn_src_id = *doc.objects().keys().max().unwrap();

    let host_path = SceneObject::new(
        ObjectId(0),
        "HostPath",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(0.0, 30.0),
            Point2D::new(40.0, 30.0),
        )),
    );
    hist.execute(&mut doc, CommandOp::create_object(host_path, Some(lid)))
        .unwrap();
    let host_path_id = *doc.objects().keys().max().unwrap();

    let blend_tgt = SceneObject::new(ObjectId(0), "BlendTgt", rect_shape(15.0, 15.0))
        .with_transform(Affine2D::translate(0.0, 45.0));
    hist.execute(&mut doc, CommandOp::create_object(blend_tgt, Some(lid)))
        .unwrap();
    let blend_tgt_id = *doc.objects().keys().max().unwrap();

    let sym_ref = SceneObject::new(ObjectId(0), "SymRef", rect_shape(15.0, 15.0))
        .with_transform(Affine2D::translate(25.0, 0.0));
    hist.execute(&mut doc, CommandOp::create_object(sym_ref, Some(lid)))
        .unwrap();
    let sym_ref_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![conn_src_id, host_path_id, blend_tgt_id, sym_ref_id],
            name: "HostGroup".to_owned(),
        },
    )
    .unwrap();
    let grp_id = *doc.objects().keys().max().unwrap();

    // External dependents: Connector, TextOnPath, Blend source, and SymbolInstance referencing grouped child
    let ext_conn_dst = SceneObject::new(ObjectId(0), "ConnDst", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(120.0, 10.0));
    hist.execute(&mut doc, CommandOp::create_object(ext_conn_dst, Some(lid)))
        .unwrap();
    let ext_conn_dst_id = *doc.objects().keys().max().unwrap();

    let conn = SceneObject::new(
        ObjectId(0),
        "ExtConn",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(conn_src_id),
            from_anchor: AnchorPoint::Center,
            from_fallback: Point2D::ZERO,
            to_obj: Some(ext_conn_dst_id),
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(120.0, 10.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 2.0,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(conn, Some(lid)))
        .unwrap();
    let conn_id = *doc.objects().keys().max().unwrap();

    let top = SceneObject::new(
        ObjectId(0),
        "ExtTOP",
        NodePayload::TextOnPath(TextOnPath {
            path_id: host_path_id,
            text: ArtisticText::from_plain("GroupedPath", TextStyle::default()),
            distance_mm: 2.0,
            offset_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(top, Some(lid)))
        .unwrap();
    let top_id = *doc.objects().keys().max().unwrap();

    let mut blend_src = SceneObject::new(ObjectId(0), "ExtBlendSrc", rect_shape(10.0, 10.0))
        .with_transform(Affine2D::translate(100.0, 60.0));
    blend_src.effects.push(Effect::Blend {
        target_id: blend_tgt_id,
        steps: 4,
        rotation_deg: 0.0,
        along_path: None,
        rotate_along_path: true,
    });
    hist.execute(&mut doc, CommandOp::create_object(blend_src, Some(lid)))
        .unwrap();
    let blend_src_id = *doc.objects().keys().max().unwrap();

    let dep_sym = SymbolDef::new(
        "DepSym",
        vec![SceneObject::new(
            ObjectId(0),
            "InnerTOP",
            NodePayload::TextOnPath(TextOnPath {
                path_id: sym_ref_id,
                text: ArtisticText::from_plain("S", TextStyle::default()),
                distance_mm: 1.0,
                offset_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        )],
    );
    let dep_sym_id = dep_sym.id;
    hist.execute(&mut doc, CommandOp::UpsertSymbol { symbol: dep_sym })
        .unwrap();
    let sym_inst = SceneObject::new(
        ObjectId(0),
        "ExtSymInst",
        NodePayload::SymbolInstance(dep_sym_id),
    );
    hist.execute(&mut doc, CommandOp::create_object(sym_inst, Some(lid)))
        .unwrap();
    let sym_inst_id = *doc.objects().keys().max().unwrap();

    let conn_before = doc.visual_world_bounds(conn_id).unwrap();
    let top_before = doc.visual_world_bounds(top_id).unwrap();

    // Move the Group by (+90, +110)
    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![grp_id],
            affine: Affine2D::translate(90.0, 110.0),
        },
    )
    .unwrap();
    let conn_after = doc.visual_world_bounds(conn_id).unwrap();
    let top_after = doc.visual_world_bounds(top_id).unwrap();

    let check_group_inv = |inv: &DocumentInvalidation, phase: &str| match inv {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            for dep in [conn_id, top_id, blend_src_id, sym_inst_id] {
                assert!(
                    object_ids.contains(&dep),
                    "[{phase}] Dependent {dep:?} missing from object_ids: {object_ids:?}"
                );
            }
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.contains_rect(conn_before.expand(-0.5))),
                "[{phase}] Connector before bounds missing: {dirty_rects_mm:?}"
            );
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.contains_rect(conn_after.expand(-0.5))),
                "[{phase}] Connector after bounds missing: {dirty_rects_mm:?}"
            );
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.contains_rect(top_before.expand(-0.5))),
                "[{phase}] TextOnPath before bounds missing: {dirty_rects_mm:?}"
            );
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.contains_rect(top_after.expand(-0.5))),
                "[{phase}] TextOnPath after bounds missing: {dirty_rects_mm:?}"
            );
        }
        DocumentInvalidation::All => panic!("[{phase}] Expected Targeted invalidation"),
    };
    check_group_inv(hist.last_invalidation(), "group execute");
    hist.undo(&mut doc).unwrap();
    check_group_inv(hist.last_invalidation(), "group undo");
    hist.redo(&mut doc).unwrap();
    check_group_inv(hist.last_invalidation(), "group redo");

    // 2. PowerClip containing a Connector endpoint and a TextOnPath host path
    let pc_container = SceneObject::new(ObjectId(0), "PCContainer", rect_shape(60.0, 60.0));
    hist.execute(&mut doc, CommandOp::create_object(pc_container, Some(lid)))
        .unwrap();
    let pc_container_id = *doc.objects().keys().max().unwrap();

    let pc_rect = SceneObject::new(ObjectId(0), "PCRect", rect_shape(15.0, 15.0))
        .with_transform(Affine2D::translate(5.0, 5.0));
    hist.execute(&mut doc, CommandOp::create_object(pc_rect, Some(lid)))
        .unwrap();
    let pc_rect_id = *doc.objects().keys().max().unwrap();

    let pc_path = SceneObject::new(
        ObjectId(0),
        "PCPath",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(10.0, 35.0),
            Point2D::new(50.0, 35.0),
        )),
    );
    hist.execute(&mut doc, CommandOp::create_object(pc_path, Some(lid)))
        .unwrap();
    let pc_path_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::CreatePowerClip {
            container_id: pc_container_id,
            content_ids: vec![pc_rect_id, pc_path_id],
        },
    )
    .unwrap();

    let pc_conn = SceneObject::new(
        ObjectId(0),
        "PCConn",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(pc_rect_id),
            from_anchor: AnchorPoint::Center,
            from_fallback: Point2D::ZERO,
            to_obj: Some(ext_conn_dst_id),
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(120.0, 10.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 2.0,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(pc_conn, Some(lid)))
        .unwrap();
    let pc_conn_id = *doc.objects().keys().max().unwrap();

    let pc_top = SceneObject::new(
        ObjectId(0),
        "PCTOP",
        NodePayload::TextOnPath(TextOnPath {
            path_id: pc_path_id,
            text: ArtisticText::from_plain("ClippedHost", TextStyle::default()),
            distance_mm: 2.0,
            offset_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(pc_top, Some(lid)))
        .unwrap();
    let pc_top_id = *doc.objects().keys().max().unwrap();

    let pc_conn_before = doc.visual_world_bounds(pc_conn_id).unwrap();
    let pc_top_before = doc.visual_world_bounds(pc_top_id).unwrap();

    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![pc_container_id],
            affine: Affine2D::translate(40.0, 120.0),
        },
    )
    .unwrap();
    let pc_conn_after = doc.visual_world_bounds(pc_conn_id).unwrap();
    let pc_top_after = doc.visual_world_bounds(pc_top_id).unwrap();

    let check_pc_inv = |inv: &DocumentInvalidation, phase: &str| match inv {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            assert!(
                object_ids.contains(&pc_conn_id),
                "[{phase}] PC Connector missing: {object_ids:?}"
            );
            assert!(
                object_ids.contains(&pc_top_id),
                "[{phase}] PC TextOnPath missing: {object_ids:?}"
            );
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.contains_rect(pc_conn_before.expand(-0.5)))
                    && dirty_rects_mm
                        .iter()
                        .any(|r| r.contains_rect(pc_conn_after.expand(-0.5))),
                "[{phase}] PC Connector before/after missing: {dirty_rects_mm:?}"
            );
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.contains_rect(pc_top_before.expand(-0.5)))
                    && dirty_rects_mm
                        .iter()
                        .any(|r| r.contains_rect(pc_top_after.expand(-0.5))),
                "[{phase}] PC TextOnPath before/after missing: {dirty_rects_mm:?}"
            );
        }
        DocumentInvalidation::All => panic!("[{phase}] Expected Targeted for PowerClip move"),
    };
    check_pc_inv(hist.last_invalidation(), "powerclip execute");
    hist.undo(&mut doc).unwrap();
    check_pc_inv(hist.last_invalidation(), "powerclip undo");
    hist.redo(&mut doc).unwrap();
    check_pc_inv(hist.last_invalidation(), "powerclip redo");
}

#[test]
fn w1c_r4_open_subpaths_never_treated_as_filled_by_contains_point() {
    // Vertical open line (40, 10) -> (40, 70): kurbo::BezPath::winding without closed-subpath
    // filtering would return non-zero across the half-infinite strip x > 40, 10 < y < 70.
    let open_line = BezierPath::from_line(Point2D::new(40.0, 10.0), Point2D::new(40.0, 70.0));
    assert!(!open_line.contains_point(Point2D::new(100.0, 40.0)));
    assert!(!open_line.contains_point(Point2D::new(500.0, 40.0)));
    assert!(!open_line.contains_point(Point2D::new(10.0, 40.0)));
    assert!(!open_line.contains_point(Point2D::new(40.0, 40.0)));

    // Open curved subpath must also return false off-stroke
    let open_curve = BezierPath {
        subpaths: vec![Subpath {
            nodes: vec![
                PathNode::new(
                    Point2D::new(40.0, 10.0),
                    None,
                    Some(Point2D::new(50.0, 30.0)),
                    NodeKind::Cusp,
                ),
                PathNode::new(
                    Point2D::new(40.0, 70.0),
                    Some(Point2D::new(50.0, 50.0)),
                    None,
                    NodeKind::Cusp,
                ),
            ],
            closed: false,
        }],
    };
    assert!(!open_curve.contains_point(Point2D::new(100.0, 40.0)));

    // Mixed path (closed rect 0..20 x 0..20 + open vertical line at x=40): only the closed subpath
    // contributes to contains_point.
    let mut mixed = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0));
    mixed.subpaths.push(open_line.subpaths[0].clone());
    assert!(mixed.contains_point(Point2D::new(10.0, 10.0)));
    assert!(!mixed.contains_point(Point2D::new(100.0, 40.0)));

    // hit_test_point on a document with a filled-styled open line must hit on-stroke only.
    let mut doc = Document::new_a4("Open Path Hit Test");
    let lid = doc.pages()[0].layers[0].id;
    let line_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "OpenLine", NodePayload::Path(open_line))
                .with_fill(Fill::Solid(Color::BLACK))
                .with_outline(OutlineStyle::default()),
            Some(lid),
        )
        .unwrap();
    assert_eq!(
        coree_core::hit_test_point(&doc, 0, Point2D::new(100.0, 40.0), 2.0),
        None
    );
    assert_eq!(
        coree_core::hit_test_point(&doc, 0, Point2D::new(40.0, 40.0), 2.0),
        Some(line_id)
    );
}

#[test]
fn w1c_r6_nested_group_ancestor_invalidation_move_edit_reorder_delete_exec_undo_redo() {
    use coree_core::{
        ArtisticText, ConnectorObject, ConnectorStyle, Effect, SymbolDef, TextOnPath, TextStyle,
    };

    let mut doc = Document::new_a4("W1C-R6-Nested-Group-Ancestor-Invalidation");
    let lid = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::default();

    // InnerGrp([c1, c2]) at (10..30, 10..30) and (40..60, 10..30)
    let c1 = SceneObject::new(ObjectId(0), "C1", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(10.0, 10.0));
    hist.execute(&mut doc, CommandOp::create_object(c1, Some(lid)))
        .unwrap();
    let c1_id = *doc.objects().keys().max().unwrap();

    let c2 = SceneObject::new(ObjectId(0), "C2", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(40.0, 10.0));
    hist.execute(&mut doc, CommandOp::create_object(c2, Some(lid)))
        .unwrap();
    let c2_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![c1_id, c2_id],
            name: "InnerGrp".to_owned(),
        },
    )
    .unwrap();
    let inner_id = *doc.objects().keys().max().unwrap();

    // OuterGrp([InnerGrp, c3]) where c3 is at (10..30, 50..70)
    let c3 = SceneObject::new(ObjectId(0), "C3", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(10.0, 50.0));
    hist.execute(&mut doc, CommandOp::create_object(c3, Some(lid)))
        .unwrap();
    let c3_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![inner_id, c3_id],
            name: "OuterGrp".to_owned(),
        },
    )
    .unwrap();
    let outer_id = *doc.objects().keys().max().unwrap();

    // External Anchor at (180..200, 180..200)
    let anchor = SceneObject::new(ObjectId(0), "Anchor", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(180.0, 180.0));
    hist.execute(&mut doc, CommandOp::create_object(anchor, Some(lid)))
        .unwrap();
    let anchor_id = *doc.objects().keys().max().unwrap();

    // External Connector attached to OuterGrp
    let conn = SceneObject::new(
        ObjectId(0),
        "ConnToOuterGrp",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(outer_id),
            from_anchor: AnchorPoint::Left,
            from_fallback: Point2D::ZERO,
            to_obj: Some(anchor_id),
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(190.0, 190.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 2.0,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(conn, Some(lid)))
        .unwrap();
    let conn_id = *doc.objects().keys().max().unwrap();

    // External Blend targeting OuterGrp
    let mut blend_src = SceneObject::new(ObjectId(0), "BlendToOuterGrp", rect_shape(15.0, 15.0))
        .with_transform(Affine2D::translate(150.0, 40.0));
    blend_src.effects.push(Effect::Blend {
        target_id: outer_id,
        steps: 3,
        rotation_deg: 0.0,
        along_path: None,
        rotate_along_path: true,
    });
    hist.execute(&mut doc, CommandOp::create_object(blend_src, Some(lid)))
        .unwrap();
    let blend_id = *doc.objects().keys().max().unwrap();

    // External TextOnPath + SymbolInstance referencing OuterGrp
    let top_outer = SceneObject::new(
        ObjectId(0),
        "TopOuter",
        NodePayload::TextOnPath(TextOnPath {
            path_id: outer_id,
            text: ArtisticText::from_plain("Outer", TextStyle::default()),
            distance_mm: 2.0,
            offset_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(top_outer, Some(lid)))
        .unwrap();
    let top_outer_id = *doc.objects().keys().max().unwrap();

    let sym = SymbolDef::new(
        "OuterDepSym",
        vec![SceneObject::new(
            ObjectId(0),
            "SymConn",
            NodePayload::Connector(ConnectorObject {
                from_obj: Some(outer_id),
                from_anchor: AnchorPoint::Top,
                from_fallback: Point2D::ZERO,
                to_obj: Some(anchor_id),
                to_anchor: AnchorPoint::Top,
                to_fallback: Point2D::new(190.0, 180.0),
                style: ConnectorStyle::Straight,
                corner_radius_mm: 1.0,
            }),
        )],
    );
    let sym_id = sym.id;
    hist.execute(&mut doc, CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();
    let sym_inst = SceneObject::new(
        ObjectId(0),
        "SymInstOuter",
        NodePayload::SymbolInstance(sym_id),
    );
    hist.execute(&mut doc, CommandOp::create_object(sym_inst, Some(lid)))
        .unwrap();
    let sym_inst_id = *doc.objects().keys().max().unwrap();

    // Untouched sibling C2-only connector (inside InnerGrp) must NOT be polluted when C1 is deleted
    let conn_c2 = SceneObject::new(
        ObjectId(0),
        "ConnToC2",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(c2_id),
            from_anchor: AnchorPoint::Center,
            from_fallback: Point2D::ZERO,
            to_obj: Some(anchor_id),
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(190.0, 190.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 1.0,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(conn_c2, Some(lid)))
        .unwrap();
    let conn_c2_id = *doc.objects().keys().max().unwrap();

    // Untouched sibling C3-only connector must NOT be falsely pulled in when C1 moves
    let conn_c3 = SceneObject::new(
        ObjectId(0),
        "ConnToC3",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(c3_id),
            from_anchor: AnchorPoint::Center,
            from_fallback: Point2D::ZERO,
            to_obj: Some(anchor_id),
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(190.0, 190.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 1.0,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(conn_c3, Some(lid)))
        .unwrap();
    let conn_c3_id = *doc.objects().keys().max().unwrap();

    // 1. Move c1 left by (-60, 0): OuterGrp min_x shifts from 10 -> -50, pulling ConnToOuterGrp & BlendToOuterGrp!
    let outer_before = doc.visual_world_bounds(outer_id).unwrap();
    let conn_before = doc.visual_world_bounds(conn_id).unwrap();
    let blend_before = doc.visual_world_bounds(blend_id).unwrap();

    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![c1_id],
            affine: Affine2D::translate(-60.0, 0.0),
        },
    )
    .unwrap();

    let outer_after = doc.visual_world_bounds(outer_id).unwrap();
    let conn_after = doc.visual_world_bounds(conn_id).unwrap();
    let blend_after = doc.visual_world_bounds(blend_id).unwrap();

    let assert_move_inv = |inv: &DocumentInvalidation, phase: &str| match inv {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            for expected in [
                c1_id,
                inner_id,
                outer_id,
                conn_id,
                blend_id,
                top_outer_id,
                sym_inst_id,
            ] {
                assert!(
                    object_ids.contains(&expected),
                    "[{phase}] Expected {expected:?} in object_ids: {object_ids:?}"
                );
            }
            assert!(
                !object_ids.contains(&conn_c3_id),
                "[{phase}] Untouched sibling dependent {conn_c3_id:?} must NOT be pulled in: {object_ids:?}"
            );
            for (name, rect) in [
                ("outer_before", outer_before),
                ("outer_after", outer_after),
                ("conn_before", conn_before),
                ("conn_after", conn_after),
                ("blend_before", blend_before),
                ("blend_after", blend_after),
            ] {
                assert!(
                    dirty_rects_mm
                        .iter()
                        .any(|r| r.contains_rect(rect.expand(-0.5))),
                    "[{phase}] {name} {rect:?} not covered in dirty_rects_mm={dirty_rects_mm:?}"
                );
            }
        }
        DocumentInvalidation::All => panic!("[{phase}] Expected Targeted invalidation"),
    };
    assert_move_inv(hist.last_invalidation(), "move execute");
    hist.undo(&mut doc).unwrap();
    assert_move_inv(hist.last_invalidation(), "move undo");
    hist.redo(&mut doc).unwrap();
    assert_move_inv(hist.last_invalidation(), "move redo");

    // 2. EditShape + SetZOrder on c1 inside nested group invalidate OuterGrp & external dependents
    hist.execute(
        &mut doc,
        CommandOp::EditShape {
            id: c1_id,
            shape: ParametricShape::Rectangle {
                width: 35.0,
                height: 20.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            },
        },
    )
    .unwrap();
    assert!(matches!(
        hist.last_invalidation(),
        DocumentInvalidation::Targeted { object_ids, .. }
            if object_ids.contains(&inner_id)
                && object_ids.contains(&outer_id)
                && object_ids.contains(&conn_id)
                && object_ids.contains(&blend_id)
    ));

    hist.execute(
        &mut doc,
        CommandOp::SetZOrder {
            id: outer_id,
            op: coree_core::ZOrderOp::BringToFront,
        },
    )
    .unwrap();
    assert!(matches!(
        hist.last_invalidation(),
        DocumentInvalidation::Targeted { object_ids, .. }
            if object_ids.contains(&outer_id)
                && object_ids.contains(&conn_id)
                && object_ids.contains(&blend_id)
    ));

    // 3. Delete c1 (at min_x = -50) inside InnerGrp inside OuterGrp -> OuterGrp shrinks from -50 to 10!
    let outer_del_before = doc.visual_world_bounds(outer_id).unwrap();
    let conn_del_before = doc.visual_world_bounds(conn_id).unwrap();
    let blend_del_before = doc.visual_world_bounds(blend_id).unwrap();

    hist.execute(&mut doc, CommandOp::DeleteObjects { ids: vec![c1_id] })
        .unwrap();

    let outer_del_after = doc.visual_world_bounds(outer_id).unwrap();
    let conn_del_after = doc.visual_world_bounds(conn_id).unwrap();
    let blend_del_after = doc.visual_world_bounds(blend_id).unwrap();

    let assert_del_inv = |inv: &DocumentInvalidation, phase: &str| match inv {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            for expected in [c1_id, inner_id, outer_id, conn_id, blend_id] {
                assert!(
                    object_ids.contains(&expected),
                    "[{phase}] Expected {expected:?} in object_ids: {object_ids:?}"
                );
            }
            assert!(
                !object_ids.contains(&conn_c2_id),
                "[{phase}] Untouched sibling c2 dependent {conn_c2_id:?} must NOT be polluted on DeleteObjects([c1]): {object_ids:?}"
            );
            assert!(
                !object_ids.contains(&conn_c3_id),
                "[{phase}] Untouched sibling c3 dependent {conn_c3_id:?} must NOT be polluted on DeleteObjects([c1]): {object_ids:?}"
            );
            for (name, rect) in [
                ("outer_del_before", outer_del_before),
                ("outer_del_after", outer_del_after),
                ("conn_del_before", conn_del_before),
                ("conn_del_after", conn_del_after),
                ("blend_del_before", blend_del_before),
                ("blend_del_after", blend_del_after),
            ] {
                assert!(
                    dirty_rects_mm
                        .iter()
                        .any(|r| r.contains_rect(rect.expand(-0.5))),
                    "[{phase}] {name} {rect:?} not covered in dirty_rects_mm={dirty_rects_mm:?}"
                );
            }
        }
        DocumentInvalidation::All => panic!("[{phase}] Expected Targeted invalidation"),
    };
    assert_del_inv(hist.last_invalidation(), "delete execute");
    hist.undo(&mut doc).unwrap();
    assert_del_inv(hist.last_invalidation(), "delete undo");
    hist.redo(&mut doc).unwrap();
    assert_del_inv(hist.last_invalidation(), "delete redo");
}

#[test]
fn w1c_r7_connector_before_bounds_with_drop_shadow_and_nested_group() {
    use coree_core::{ConnectorObject, ConnectorStyle, Effect, FeatherDirection};

    let mut doc = Document::new_a4("W1C-R7-Connector-Shadow-Before-Bounds");
    let lid = doc.pages()[0].layers[0].id;
    let mut hist = CommandHistory::default();

    // 1. Standalone shape with large asymmetric DropShadow (dx=+40mm) + Connector attached to Top anchor
    let mut shadowed = SceneObject::new(ObjectId(0), "ShadowedShape", rect_shape(20.0, 40.0));
    shadowed.effects.push(Effect::DropShadow {
        dx_mm: 40.0,
        dy_mm: 0.0,
        blur_mm: 2.0,
        color: Color::BLACK,
        opacity: 0.5,
        feather_direction: FeatherDirection::Middle,
    });
    hist.execute(&mut doc, CommandOp::create_object(shadowed, Some(lid)))
        .unwrap();
    let shadowed_id = *doc.objects().keys().max().unwrap();

    let anchor_a = SceneObject::new(ObjectId(0), "AnchorA", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(50.0, 100.0));
    hist.execute(&mut doc, CommandOp::create_object(anchor_a, Some(lid)))
        .unwrap();
    let anchor_a_id = *doc.objects().keys().max().unwrap();

    let conn_shape = SceneObject::new(
        ObjectId(0),
        "ConnShadowShape",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(shadowed_id),
            from_anchor: AnchorPoint::Top,
            from_fallback: Point2D::ZERO,
            to_obj: Some(anchor_a_id),
            to_anchor: AnchorPoint::Top,
            to_fallback: Point2D::new(60.0, 100.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 1.0,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(conn_shape, Some(lid)))
        .unwrap();
    let conn_shape_id = *doc.objects().keys().max().unwrap();

    let conn_before_move = doc.visual_world_bounds(conn_shape_id).unwrap();
    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![shadowed_id],
            affine: Affine2D::translate(50.0, 0.0),
        },
    )
    .unwrap();
    let conn_after_move = doc.visual_world_bounds(conn_shape_id).unwrap();

    let assert_cov = |inv: &DocumentInvalidation, b: Rect2D, a: Option<Rect2D>, phase: &str| {
        match inv {
            DocumentInvalidation::Targeted {
                object_ids,
                dirty_rects_mm,
            } => {
                assert!(
                    object_ids.contains(&conn_shape_id),
                    "[{phase}] Expected {conn_shape_id:?} in object_ids"
                );
                assert!(
                    dirty_rects_mm.iter().any(|r| r.contains_rect(b)),
                    "[{phase}] conn_before {b:?} not covered by dirty_rects_mm={dirty_rects_mm:?}"
                );
                if let Some(after_r) = a {
                    assert!(
                        dirty_rects_mm.iter().any(|r| r.contains_rect(after_r)),
                        "[{phase}] conn_after {after_r:?} not covered by dirty_rects_mm={dirty_rects_mm:?}"
                    );
                }
            }
            DocumentInvalidation::All => panic!("[{phase}] Expected Targeted invalidation"),
        }
    };
    assert_cov(
        hist.last_invalidation(),
        conn_before_move,
        Some(conn_after_move),
        "shape move execute",
    );
    hist.undo(&mut doc).unwrap();
    assert_cov(
        hist.last_invalidation(),
        conn_before_move,
        Some(conn_after_move),
        "shape move undo",
    );
    hist.redo(&mut doc).unwrap();
    assert_cov(
        hist.last_invalidation(),
        conn_before_move,
        Some(conn_after_move),
        "shape move redo",
    );

    // 2. Nested Group with asymmetric DropShadow (dx=+40mm, dy=+20mm) + Connector(AnchorPoint::Top)
    let g_c1 = SceneObject::new(ObjectId(0), "GrpC1", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(10.0, 20.0));
    hist.execute(&mut doc, CommandOp::create_object(g_c1, Some(lid)))
        .unwrap();
    let g_c1_id = *doc.objects().keys().max().unwrap();

    let g_c2 = SceneObject::new(ObjectId(0), "GrpC2", rect_shape(20.0, 20.0))
        .with_transform(Affine2D::translate(10.0, 50.0));
    hist.execute(&mut doc, CommandOp::create_object(g_c2, Some(lid)))
        .unwrap();
    let g_c2_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![g_c1_id, g_c2_id],
            name: "InnerShadowGrp".to_owned(),
        },
    )
    .unwrap();
    let inner_grp_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::AddEffect {
            id: inner_grp_id,
            effect: Effect::DropShadow {
                dx_mm: 40.0,
                dy_mm: 20.0,
                blur_mm: 4.0,
                color: Color::BLACK,
                opacity: 0.6,
                feather_direction: FeatherDirection::Middle,
            },
        },
    )
    .unwrap();

    let g_c3 = SceneObject::new(ObjectId(0), "GrpC3", rect_shape(10.0, 10.0))
        .with_transform(Affine2D::translate(20.0, 10.0));
    hist.execute(&mut doc, CommandOp::create_object(g_c3, Some(lid)))
        .unwrap();
    let g_c3_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![inner_grp_id, g_c3_id],
            name: "OuterShadowGrp".to_owned(),
        },
    )
    .unwrap();
    let outer_grp_id = *doc.objects().keys().max().unwrap();

    let conn_grp = SceneObject::new(
        ObjectId(0),
        "ConnShadowGroup",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(outer_grp_id),
            from_anchor: AnchorPoint::Top,
            from_fallback: Point2D::ZERO,
            to_obj: Some(anchor_a_id),
            to_anchor: AnchorPoint::Top,
            to_fallback: Point2D::new(60.0, 100.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 1.0,
        }),
    );
    hist.execute(&mut doc, CommandOp::create_object(conn_grp, Some(lid)))
        .unwrap();
    let conn_grp_id = *doc.objects().keys().max().unwrap();

    let grp_conn_before = doc.visual_world_bounds(conn_grp_id).unwrap();
    hist.execute(&mut doc, CommandOp::DeleteObjects { ids: vec![g_c1_id] })
        .unwrap();
    let grp_conn_after = doc.visual_world_bounds(conn_grp_id).unwrap();

    let assert_grp_cov = |inv: &DocumentInvalidation, phase: &str| match inv {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            assert!(
                object_ids.contains(&conn_grp_id),
                "[{phase}] Expected {conn_grp_id:?} in object_ids"
            );
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.contains_rect(grp_conn_before)),
                "[{phase}] grp_conn_before {grp_conn_before:?} not covered by dirty_rects_mm={dirty_rects_mm:?}"
            );
            assert!(
                dirty_rects_mm
                    .iter()
                    .any(|r| r.contains_rect(grp_conn_after)),
                "[{phase}] grp_conn_after {grp_conn_after:?} not covered by dirty_rects_mm={dirty_rects_mm:?}"
            );
        }
        DocumentInvalidation::All => panic!("[{phase}] Expected Targeted invalidation"),
    };
    assert_grp_cov(hist.last_invalidation(), "nested group delete execute");
    hist.undo(&mut doc).unwrap();
    assert_grp_cov(hist.last_invalidation(), "nested group delete undo");
    hist.redo(&mut doc).unwrap();
    assert_grp_cov(hist.last_invalidation(), "nested group delete redo");
}

#[test]
fn w2_exit_r2_perspective_grid_one_point_and_isometric_planes() {
    use coree_core::{PerspectiveGridConfig, PerspectiveGridMode, PerspectivePlane, SymbolDef};

    let vp = Point2D::new(105.0, 80.0);
    let rect_right = Rect2D::from_xywh(120.0, 100.0, 30.0, 30.0);
    let rect_left = Rect2D::from_xywh(40.0, 100.0, 30.0, 30.0);

    // 1. OnePoint mode: single VP governs all receding planes regardless of rect side.
    for rect in [rect_right, rect_left] {
        // Top plane
        let pg_top = PerspectiveGridConfig {
            enabled: true,
            mode: PerspectiveGridMode::OnePoint,
            horizon_y_mm: 80.0,
            vanishing_points: vec![vp],
            density: 12,
            opacity: 0.35,
            active_plane: PerspectivePlane::Top,
        };
        let q_top = pg_top.plane_quad_for_rect(rect).expect("OnePoint Top quad");
        assert!(
            q_top[1].x - q_top[0].x < rect.width(),
            "OnePoint Top back width must shrink toward VP: {q_top:?}"
        );
        let c_left = (q_top[0] - q_top[3]).cross(vp - q_top[3]);
        let c_right = (q_top[1] - q_top[2]).cross(vp - q_top[2]);
        assert!(
            c_left.abs() < 1e-6 && c_right.abs() < 1e-6,
            "OnePoint Top receding edges must be collinear with VP: c_left={c_left}, c_right={c_right}"
        );
        assert!(pg_top.plane_homography_for_rect(rect).is_some());

        // Left & Right planes
        for plane in [PerspectivePlane::Left, PerspectivePlane::Right] {
            let pg_side = PerspectiveGridConfig {
                active_plane: plane,
                ..pg_top.clone()
            };
            let q_side = pg_side
                .plane_quad_for_rect(rect)
                .expect("OnePoint side quad");
            assert!(
                q_side[0].x < q_side[1].x && q_side[3].x < q_side[2].x,
                "OnePoint {plane:?} must not mirror horizontally: {q_side:?}"
            );
            assert!(
                q_side[0].y < q_side[3].y && q_side[1].y < q_side[2].y,
                "OnePoint {plane:?} must not invert vertically: {q_side:?}"
            );
            // Receding top & bottom edges must point toward `vp`.
            if rect.min_x > vp.x {
                assert!((q_side[0] - q_side[1]).cross(vp - q_side[1]).abs() < 1e-6);
                assert!((q_side[3] - q_side[2]).cross(vp - q_side[2]).abs() < 1e-6);
            } else {
                assert!((q_side[1] - q_side[0]).cross(vp - q_side[0]).abs() < 1e-6);
                assert!((q_side[2] - q_side[3]).cross(vp - q_side[3]).abs() < 1e-6);
            }
            assert!(pg_side.plane_homography_for_rect(rect).is_some());
        }

        // Front plane is orthogonal [tl, tr, br, bl]
        let pg_front = PerspectiveGridConfig {
            active_plane: PerspectivePlane::Front,
            ..pg_top
        };
        let q_front = pg_front
            .plane_quad_for_rect(rect)
            .expect("OnePoint Front quad");
        assert_eq!(q_front, rect.corners());
    }

    // 2. Isometric mode: parallel 30° / 150° / 90° edges, independent of horizon/VPs.
    for plane in [
        PerspectivePlane::Left,
        PerspectivePlane::Right,
        PerspectivePlane::Top,
    ] {
        let iso_a = PerspectiveGridConfig {
            enabled: true,
            mode: PerspectiveGridMode::Isometric,
            horizon_y_mm: 10.0,
            vanishing_points: Vec::new(),
            density: 12,
            opacity: 0.35,
            active_plane: plane,
        };
        let iso_b = PerspectiveGridConfig {
            horizon_y_mm: 240.0,
            vanishing_points: vec![Point2D::new(-50.0, 240.0), Point2D::new(300.0, 240.0)],
            ..iso_a.clone()
        };
        let q_a = iso_a
            .plane_quad_for_rect(rect_left)
            .expect("Isometric quad A");
        let q_b = iso_b
            .plane_quad_for_rect(rect_left)
            .expect("Isometric quad B");
        assert_eq!(
            q_a, q_b,
            "Isometric quad must be independent of horizon_y_mm / vanishing_points"
        );
        let top_edge = q_a[1] - q_a[0];
        let bot_edge = q_a[2] - q_a[3];
        let left_edge = q_a[3] - q_a[0];
        let right_edge = q_a[2] - q_a[1];
        assert!(
            top_edge.cross(bot_edge).abs() < 1e-9,
            "Isometric {plane:?} top/bottom edges must be parallel: {q_a:?}"
        );
        assert!(
            left_edge.cross(right_edge).abs() < 1e-9,
            "Isometric {plane:?} left/right edges must be parallel: {q_a:?}"
        );
        let expected_top_deg = match plane {
            PerspectivePlane::Left => 30.0_f64,
            PerspectivePlane::Right | PerspectivePlane::Top => -30.0_f64,
            _ => unreachable!(),
        };
        assert!(
            (top_edge.angle_rad().to_degrees() - expected_top_deg).abs() < 1e-6,
            "Isometric {plane:?} top edge angle mismatch: got {}",
            top_edge.angle_rad().to_degrees()
        );
        let h = iso_a
            .plane_homography_for_rect(rect_left)
            .expect("Isometric homography");
        assert!(
            h.m[6].abs() < 1e-9 && h.m[7].abs() < 1e-9,
            "Isometric homography must be affine: {:?}",
            h.m
        );
    }
    let iso_front = PerspectiveGridConfig {
        enabled: true,
        mode: PerspectiveGridMode::Isometric,
        horizon_y_mm: 10.0,
        vanishing_points: Vec::new(),
        density: 12,
        opacity: 0.35,
        active_plane: PerspectivePlane::Front,
    };
    assert_eq!(
        iso_front.plane_quad_for_rect(rect_left),
        Some(rect_left.corners())
    );

    // 3. visual_world_bounds on Group and SymbolInstance with Perspective3x3
    let mut doc = Document::new("VwbPersp", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let lid = doc.pages()[0].layers[0].id;
    let persp = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0),
        &[
            Point2D::new(40.0, 40.0),
            Point2D::new(90.0, 50.0),
            Point2D::new(85.0, 100.0),
            Point2D::new(35.0, 95.0),
        ],
    )
    .unwrap();
    let child = SceneObject::new(ObjectId(0), "C", rect_shape(20.0, 20.0));
    let cid = doc.add_object(child, Some(lid)).unwrap();
    let grp_id = doc.group_objects(&[cid], "PerspGrp").unwrap();
    apply_command(
        &mut doc,
        &CommandOp::SetObjectPerspective {
            ids: vec![grp_id],
            perspective: Some(persp),
        },
    )
    .unwrap();
    let grp_vwb = doc.visual_world_bounds(grp_id).unwrap();
    assert!(
        grp_vwb.max_x >= 89.0 && grp_vwb.max_y >= 99.0 && grp_vwb.min_x >= 34.0,
        "Group visual_world_bounds must include Perspective3x3: {grp_vwb:?}"
    );

    let sym = SymbolDef::new(
        "PerspSym",
        vec![SceneObject::new(ObjectId(0), "SC", rect_shape(20.0, 20.0))],
    );
    let sid = sym.id;
    doc.insert_symbol(sym);
    let mut inst = SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid));
    inst.perspective = Some(persp);
    let inst_id = doc.add_object(inst, Some(lid)).unwrap();
    let inst_vwb = doc.visual_world_bounds(inst_id).unwrap();
    assert!(
        inst_vwb.max_x >= 89.0 && inst_vwb.max_y >= 99.0 && inst_vwb.min_x >= 34.0,
        "SymbolInstance visual_world_bounds must include Perspective3x3: {inst_vwb:?}"
    );
}

#[test]
fn w2_exit_r5_hit_test_symbol_text_on_path_hosts_and_persp_connector_bounds() {
    use coree_core::hit_test::{
        hit_test_point, hit_test_rect, resolve_connector_endpoints_with_sym,
    };
    use std::collections::{HashMap, HashSet};

    for kind in ["BSpline", "ArtisticMedia", "Clone", "PerspectivePath"] {
        let mut doc = Document::new(kind, 200.0, 200.0, ColorMode::Rgb, 96.0);
        let lid = doc.pages()[0].layers[0].id;
        let master_id = if kind == "Clone" {
            let mut master = SceneObject::new(
                ObjectId(0),
                "MasterCurve",
                NodePayload::BSpline(BSplineCurve::from_control_points(
                    vec![
                        Point2D::new(5.0, 20.0),
                        Point2D::new(20.0, 20.0),
                        Point2D::new(35.0, 20.0),
                        Point2D::new(50.0, 20.0),
                    ],
                    3,
                    false,
                )),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::none());
            master.visible = false;
            Some(doc.add_object(master, Some(lid)).unwrap())
        } else {
            None
        };

        let mut sym_host = match kind {
            "BSpline" => SceneObject::new(
                ObjectId(10),
                "SymBSpline",
                NodePayload::BSpline(BSplineCurve::from_control_points(
                    vec![
                        Point2D::new(5.0, 20.0),
                        Point2D::new(20.0, 20.0),
                        Point2D::new(35.0, 20.0),
                        Point2D::new(50.0, 20.0),
                    ],
                    3,
                    false,
                )),
            ),
            "ArtisticMedia" => SceneObject::new(
                ObjectId(10),
                "SymMedia",
                NodePayload::ArtisticMedia {
                    spine: BezierPath::from_line(Point2D::new(5.0, 20.0), Point2D::new(50.0, 20.0)),
                    style: ArtisticMediaStyle::Calligraphic {
                        width_mm: 2.0,
                        angle_deg: 45.0,
                    },
                },
            ),
            "Clone" => SceneObject::new(
                ObjectId(10),
                "SymClone",
                NodePayload::Clone {
                    master_id: master_id.unwrap(),
                    link_fill: true,
                    link_outline: true,
                    link_shape: true,
                },
            ),
            "PerspectivePath" => {
                let mut host = SceneObject::new(
                    ObjectId(10),
                    "SymPerspHost",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 40.0, 20.0,
                    ))),
                );
                host.perspective = Some(
                    Perspective3x3::from_rect_to_quad(
                        Rect2D::from_xywh(0.0, 0.0, 40.0, 20.0),
                        &[
                            Point2D::new(0.0, 25.0),
                            Point2D::new(40.0, 25.0),
                            Point2D::new(40.0, 35.0),
                            Point2D::new(0.0, 35.0),
                        ],
                    )
                    .unwrap(),
                );
                host
            }
            _ => unreachable!(),
        };
        sym_host = sym_host
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::none());

        let sym_top = SceneObject::new(
            ObjectId(11),
            "SymTOP",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain(
                    "HIT",
                    TextStyle {
                        font_size_pt: 12.0,
                        ..Default::default()
                    },
                ),
                path_id: ObjectId(10),
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        );
        let sym = SymbolDef::new(format!("HitSym_{kind}"), vec![sym_host, sym_top]);
        let sid = sym.id;
        doc.insert_symbol(sym);
        let inst_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid))
                    .with_transform(Affine2D::translate(50.0, 60.0)),
                Some(lid),
            )
            .unwrap();

        let probe_pt = if kind == "PerspectivePath" {
            Point2D::new(70.0, 86.0)
        } else {
            Point2D::new(75.0, 80.0)
        };
        let hit = hit_test_point(&doc, 0, probe_pt, 1.0);
        assert_eq!(
            hit,
            Some(inst_id),
            "{kind}: hit_test_point must hit SymbolInstance TextOnPath at {probe_pt:?}"
        );
        let origin_miss = hit_test_point(&doc, 0, Point2D::new(51.0, 61.0), 0.5);
        assert_eq!(
            origin_miss, None,
            "{kind}: hit_test_point must NOT hit unwarped/origin fallback near (51,61)"
        );
        let rect_hits = hit_test_rect(
            &doc,
            0,
            Rect2D::from_xywh(probe_pt.x - 1.0, probe_pt.y - 1.0, 2.0, 2.0),
            false,
        );
        assert!(
            rect_hits.contains(&inst_id),
            "{kind}: hit_test_rect must match TextOnPath along {kind} host"
        );
    }

    // Connector endpoint inside SymbolDef targeting a Perspective3x3-warped Path
    let mut doc = Document::new("SymPerspConn", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let mut target_a = SceneObject::new(
        ObjectId(20),
        "PerspTarget",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 10.0, 10.0,
        ))),
    );
    target_a.perspective = Some(
        Perspective3x3::from_rect_to_quad(
            Rect2D::from_xywh(0.0, 0.0, 10.0, 10.0),
            &[
                Point2D::new(30.0, 40.0),
                Point2D::new(50.0, 40.0),
                Point2D::new(50.0, 60.0),
                Point2D::new(30.0, 60.0),
            ],
        )
        .unwrap(),
    );
    let conn = ConnectorObject::between_objects(
        ObjectId(20),
        AnchorPoint::Center,
        ObjectId(20),
        AnchorPoint::Right,
        ConnectorStyle::Straight,
    );
    let mut sym_map = HashMap::new();
    sym_map.insert(ObjectId(20), &target_a);
    let mut visiting = HashMap::new();
    let mut visited_syms = HashSet::new();
    let mut cache = HashMap::new();
    let ((a, b), _) = resolve_connector_endpoints_with_sym(
        &doc,
        &conn,
        &Affine2D::translate(10.0, 20.0),
        Some((&sym_map, Affine2D::translate(10.0, 20.0))),
        &mut visiting,
        &mut visited_syms,
        &mut cache,
        0,
    );
    assert!(
        (a.x - 50.0).abs() < 1e-6 && (a.y - 70.0).abs() < 1e-6,
        "expected warped Center (50, 70), got {a:?}"
    );
    assert!(
        (b.x - 60.0).abs() < 1e-6 && (b.y - 70.0).abs() < 1e-6,
        "expected warped Right (60, 70), got {b:?}"
    );
    let _ = &mut doc;
}

#[test]
fn w2_exit_r6_hit_test_persp_text_and_persp_containers_and_clone_text_convert() {
    // 1. C4: own obj.perspective on ArtisticText, ParagraphText, Unattached & Attached TextOnPath
    let persp_0_30_to_90_140 = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(0.0, 0.0, 30.0, 10.0),
        &[
            Point2D::new(90.0, 90.0),
            Point2D::new(140.0, 90.0),
            Point2D::new(140.0, 120.0),
            Point2D::new(90.0, 120.0),
        ],
    )
    .unwrap();
    let warped_pt = Point2D::new(115.0, 105.0);
    let unwarped_pt = Point2D::new(15.0, 5.0);
    let warped_rect = Rect2D::from_xywh(95.0, 95.0, 40.0, 20.0);
    let unwarped_rect = Rect2D::from_xywh(5.0, 2.0, 10.0, 5.0);

    for kind in [
        "ArtisticText",
        "ParagraphText",
        "UnattachedTOP",
        "AttachedTOP",
    ] {
        let mut doc = Document::new(kind, 200.0, 200.0, ColorMode::Rgb, 150.0);
        let style = TextStyle {
            font_size_pt: 16.0,
            fill: Color::rgba8(220, 20, 20, 255),
            ..Default::default()
        };
        let mut obj = match kind {
            "ArtisticText" => SceneObject::new(
                ObjectId(0),
                "Art",
                NodePayload::ArtisticText(ArtisticText::from_plain("WARPED", style)),
            ),
            "ParagraphText" => SceneObject::new(
                ObjectId(0),
                "Para",
                NodePayload::ParagraphText(ParagraphText::new(
                    "WARPED",
                    Vec2D::new(30.0, 10.0),
                    style,
                )),
            ),
            "UnattachedTOP" => SceneObject::new(
                ObjectId(0),
                "UnTOP",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("WARPED", style),
                    path_id: ObjectId(9999),
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            "AttachedTOP" => {
                let mut host = SceneObject::new(
                    ObjectId(0),
                    "Host",
                    NodePayload::Path(BezierPath::from_line(
                        Point2D::new(0.0, 5.0),
                        Point2D::new(30.0, 5.0),
                    )),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none());
                host.visible = false;
                let hid = doc.add_object(host, None).unwrap();
                SceneObject::new(
                    ObjectId(0),
                    "AttTOP",
                    NodePayload::TextOnPath(TextOnPath {
                        text: ArtisticText::from_plain("WARPED", style),
                        path_id: hid,
                        offset_mm: 0.0,
                        distance_mm: 0.0,
                        flip_vertical: false,
                        flip_horizontal: false,
                    }),
                )
            }
            _ => unreachable!(),
        };
        obj.perspective = Some(persp_0_30_to_90_140);
        let oid = doc.add_object(obj, None).unwrap();

        assert_eq!(
            hit_test_point(&doc, 0, warped_pt, 2.0),
            Some(oid),
            "{kind}: hit_test_point must hit warped coords"
        );
        assert_eq!(
            hit_test_point(&doc, 0, unwarped_pt, 1.0),
            None,
            "{kind}: hit_test_point must NOT ghost-hit unwarped coords"
        );
        assert!(
            hit_test_rect(&doc, 0, warped_rect, false).contains(&oid),
            "{kind}: hit_test_rect must intersect warped coords"
        );
        assert!(
            !hit_test_rect(&doc, 0, unwarped_rect, false).contains(&oid),
            "{kind}: hit_test_rect must NOT ghost-hit unwarped coords"
        );
    }

    // 2. C1: SymbolInstance(Translate(30, 30) + Perspective3x3) containing attached TextOnPath
    {
        let mut doc = Document::new("C1_SymPersp", 200.0, 200.0, ColorMode::Rgb, 150.0);
        let host = SceneObject::new(
            ObjectId(10),
            "Host",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(0.0, 20.0),
                Point2D::new(40.0, 20.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none());
        let top = SceneObject::new(
            ObjectId(11),
            "SymTOP",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain("SYM", TextStyle::default()),
                path_id: ObjectId(10),
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        );
        let sym = SymbolDef::new("C1Sym", vec![host, top]);
        let sid = sym.id;
        doc.insert_symbol(sym);
        let mut inst = SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid))
            .with_transform(Affine2D::translate(30.0, 30.0));
        inst.perspective = Some(
            Perspective3x3::from_rect_to_quad(
                Rect2D::from_xywh(0.0, 0.0, 40.0, 30.0),
                &[
                    Point2D::new(50.0, 60.0),
                    Point2D::new(110.0, 60.0),
                    Point2D::new(110.0, 90.0),
                    Point2D::new(50.0, 90.0),
                ],
            )
            .unwrap(),
        );
        let inst_id = doc.add_object(inst, None).unwrap();

        // Line y=20 in (0..40, 0..30) warps to y=80 in local (50..110, 60..90), + (30,30) = (80..140, 110)
        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(110.0, 110.0), 2.0),
            Some(inst_id),
            "C1: must hit warped SymbolInstance TextOnPath"
        );
        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(50.0, 50.0), 2.0),
            None,
            "C1: must NOT ghost-hit unwarped SymbolInstance coords"
        );
        assert!(
            hit_test_rect(&doc, 0, Rect2D::from_xywh(100.0, 105.0, 20.0, 10.0), false)
                .contains(&inst_id)
        );
        assert!(
            !hit_test_rect(&doc, 0, Rect2D::from_xywh(45.0, 48.0, 10.0, 4.0), false)
                .contains(&inst_id)
        );
    }

    // 3. A3: Group(Perspective3x3) & PowerClip(Perspective3x3) edit mode
    {
        let mut doc = Document::new("A3_GroupPersp", 200.0, 200.0, ColorMode::Rgb, 150.0);
        let rect = SceneObject::new(ObjectId(0), "ChildRect", rect_shape(20.0, 20.0))
            .with_fill(Fill::Solid(Color::rgba8(0, 0, 255, 255)));
        let rid = doc.add_object(rect, None).unwrap();
        let gid = doc.group_objects(&[rid], "PerspGrp").unwrap();
        let grp_persp = Perspective3x3::from_rect_to_quad(
            Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0),
            &[
                Point2D::new(80.0, 80.0),
                Point2D::new(120.0, 80.0),
                Point2D::new(120.0, 120.0),
                Point2D::new(80.0, 120.0),
            ],
        )
        .unwrap();
        let mut hist = CommandHistory::default();
        hist.execute(
            &mut doc,
            CommandOp::SetObjectPerspective {
                ids: vec![gid],
                perspective: Some(grp_persp),
            },
        )
        .unwrap();

        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(100.0, 100.0), 1.0),
            Some(gid),
            "A3: hit_test_point must hit Perspective3x3 Group at warped center"
        );
        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(10.0, 10.0), 1.0),
            None,
            "A3: hit_test_point must NOT ghost-hit unwarped Group coords"
        );
        assert!(
            hit_test_rect(&doc, 0, Rect2D::from_xywh(90.0, 90.0, 20.0, 20.0), false).contains(&gid)
        );
        assert!(
            !hit_test_rect(&doc, 0, Rect2D::from_xywh(5.0, 5.0, 10.0, 10.0), false).contains(&gid)
        );

        // PowerClip with Perspective3x3 edit-mode content hit_test
        let pc_child = SceneObject::new(ObjectId(0), "PCChild", rect_shape(20.0, 20.0))
            .with_fill(Fill::Solid(Color::rgba8(0, 200, 0, 255)));
        let pc_cid = doc.add_object(pc_child, None).unwrap();
        let pc_container = SceneObject::new(ObjectId(0), "PCCont", rect_shape(20.0, 20.0))
            .with_fill(Fill::Solid(Color::WHITE));
        let pc_cont_id = doc.add_object(pc_container, None).unwrap();
        let pc_id = doc.create_powerclip(pc_cont_id, &[pc_cid]).unwrap();
        hist.execute(
            &mut doc,
            CommandOp::SetObjectPerspective {
                ids: vec![pc_id],
                perspective: Some(grp_persp),
            },
        )
        .unwrap();
        assert_eq!(
            hit_test_powerclip_contents(&doc, pc_id, Point2D::new(100.0, 100.0), 1.0),
            Some(pc_cid)
        );
        assert_eq!(
            hit_test_powerclip_contents(&doc, pc_id, Point2D::new(10.0, 10.0), 1.0),
            None
        );
        assert!(hit_test_powerclip_contents_rect(
            &doc,
            pc_id,
            Rect2D::from_xywh(90.0, 90.0, 20.0, 20.0),
            false
        )
        .contains(&pc_cid));
    }

    // 4. REM-02: Document::convert_to_curves_with_outliner on Clone of ArtisticText/ParagraphText/TextOnPath
    {
        let mut doc = Document::new_a4("CloneTextCurves");
        let style = TextStyle {
            fill: Color::rgba8(220, 20, 20, 255),
            font_size_pt: 14.0,
            ..Default::default()
        };
        let art_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "MasterArt",
                    NodePayload::ArtisticText(ArtisticText::from_plain("ABC", style.clone())),
                )
                .with_fill(Fill::None),
                None,
            )
            .unwrap();
        let para_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "MasterPara",
                    NodePayload::ParagraphText(ParagraphText::new(
                        "DEF",
                        Vec2D::new(40.0, 20.0),
                        style.clone(),
                    )),
                )
                .with_fill(Fill::None),
                None,
            )
            .unwrap();
        let host_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Host",
                    NodePayload::Path(BezierPath::from_line(
                        Point2D::new(0.0, 0.0),
                        Point2D::new(40.0, 0.0),
                    )),
                ),
                None,
            )
            .unwrap();
        let top_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "MasterTOP",
                    NodePayload::TextOnPath(TextOnPath {
                        text: ArtisticText::from_plain("GHI", style),
                        path_id: host_id,
                        offset_mm: 0.0,
                        distance_mm: 0.0,
                        flip_vertical: false,
                        flip_horizontal: false,
                    }),
                )
                .with_fill(Fill::None),
                None,
            )
            .unwrap();

        for (name, mid) in [
            ("CloneArt", art_id),
            ("ClonePara", para_id),
            ("CloneTOP", top_id),
        ] {
            let cid = doc
                .add_object(
                    SceneObject::new(
                        ObjectId(0),
                        name,
                        NodePayload::Clone {
                            master_id: mid,
                            link_fill: true,
                            link_outline: true,
                            link_shape: true,
                        },
                    )
                    .with_fill(Fill::None),
                    None,
                )
                .unwrap();
            let mut outliner_called = false;
            doc.convert_to_curves_with_outliner(cid, |synth, _| {
                outliner_called = true;
                assert!(!matches!(synth.payload, NodePayload::Clone { .. }));
                Some(BezierPath::from_rect(Rect2D::from_xywh(
                    1.0, 1.0, 12.0, 5.0,
                )))
            })
            .unwrap();
            assert!(outliner_called, "{name}: text_outliner must be invoked");
            let converted = doc.object(cid).unwrap();
            assert!(
                matches!(converted.payload, NodePayload::Path(_)),
                "{name}: must convert to Path"
            );
            assert!(
                converted.fill.is_visible(),
                "{name}: run fill must propagate when converting Clone of text"
            );
        }
    }
}

#[test]
fn w2_exit_r7_powerclip_enclosed_symbol_member_persp_and_grouped_text_on_path_bounds() {
    let mut hist = CommandHistory::default();

    // 1. challenger_w2exit_r6_1-r6-1: hit_test_powerclip_contents_rect(..., fully_enclosed = true)
    //    under PowerClip(Perspective3x3) & hit_test_rect(..., true) on Perspective3x3/Effect::Perspective containers
    {
        let mut doc = Document::new("R7_PowerClipEnclosed", 200.0, 200.0, ColorMode::Rgb, 150.0);
        let child = SceneObject::new(ObjectId(0), "PCChild", rect_shape(20.0, 20.0))
            .with_fill(Fill::Solid(Color::rgba8(0, 180, 0, 255)));
        let cid = doc.add_object(child, None).unwrap();
        let container = SceneObject::new(ObjectId(0), "PCCont", rect_shape(20.0, 20.0))
            .with_fill(Fill::Solid(Color::WHITE));
        let cont_id = doc.add_object(container, None).unwrap();
        let pc_id = doc.create_powerclip(cont_id, &[cid]).unwrap();

        let pc_persp = Perspective3x3::from_rect_to_quad(
            Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0),
            &[
                Point2D::new(80.0, 80.0),
                Point2D::new(120.0, 80.0),
                Point2D::new(120.0, 120.0),
                Point2D::new(80.0, 120.0),
            ],
        )
        .unwrap();
        hist.execute(
            &mut doc,
            CommandOp::SetObjectPerspective {
                ids: vec![pc_id],
                perspective: Some(pc_persp),
            },
        )
        .unwrap();

        let warped_enclosing_rect = Rect2D::from_xywh(75.0, 75.0, 50.0, 50.0);
        let unwarped_enclosing_rect = Rect2D::from_xywh(-5.0, -5.0, 30.0, 30.0);
        assert_eq!(
            hit_test_powerclip_contents_rect(&doc, pc_id, warped_enclosing_rect, true),
            vec![cid],
            "r6-1: fully_enclosed hit_test_powerclip_contents_rect must hit warped child"
        );
        assert!(
            hit_test_powerclip_contents_rect(&doc, pc_id, unwarped_enclosing_rect, true).is_empty(),
            "r6-1: fully_enclosed hit_test_powerclip_contents_rect must NOT ghost-hit unwarped coords"
        );
        assert_eq!(
            hit_test_rect(&doc, 0, warped_enclosing_rect, true),
            vec![pc_id],
            "r6-1: fully_enclosed hit_test_rect must hit warped PowerClip"
        );
        assert!(
            hit_test_rect(&doc, 0, unwarped_enclosing_rect, true).is_empty(),
            "r6-1: fully_enclosed hit_test_rect must NOT ghost-hit unwarped PowerClip coords"
        );
    }

    // 2. challenger_w2exit_r6_1-r6-2: SymbolDef member with sub.perspective (both named & ObjectId(0) + Group)
    {
        let mut doc = Document::new("R7_SymMemberPersp", 200.0, 200.0, ColorMode::Rgb, 150.0);
        let member_persp = Perspective3x3::from_rect_to_quad(
            Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0),
            &[
                Point2D::new(80.0, 80.0),
                Point2D::new(120.0, 80.0),
                Point2D::new(120.0, 120.0),
                Point2D::new(80.0, 120.0),
            ],
        )
        .unwrap();
        let mut sub_rect = SceneObject::new(ObjectId(10), "SubRect", rect_shape(20.0, 20.0))
            .with_fill(Fill::Solid(Color::rgba8(20, 40, 220, 255)));
        sub_rect.perspective = Some(member_persp);

        let sym = SymbolDef::new("MemberPerspSym", vec![sub_rect]);
        let sid = sym.id;
        doc.insert_symbol(sym);
        let inst = SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid));
        let inst_id = doc.add_object(inst, None).unwrap();

        let wb = doc.world_object_bounds(inst_id).unwrap();
        assert!(
            (wb.min_x - 80.0).abs() < 1e-6
                && (wb.min_y - 80.0).abs() < 1e-6
                && (wb.max_x - 120.0).abs() < 1e-6
                && (wb.max_y - 120.0).abs() < 1e-6,
            "r6-2: world_object_bounds must reflect sub.perspective (80..120), got {wb:?}"
        );
        let vwb = doc.visual_world_bounds(inst_id).unwrap();
        assert!(
            vwb.min_x >= 79.0 && vwb.min_y >= 79.0 && vwb.max_x <= 121.0 && vwb.max_y <= 121.0,
            "r6-2: visual_world_bounds must reflect sub.perspective (80..120) without origin leak, got {vwb:?}"
        );
        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(100.0, 100.0), 1.0),
            Some(inst_id),
            "r6-2: hit_test_point must hit SymbolInstance at warped center (100,100)"
        );
        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(10.0, 10.0), 1.0),
            None,
            "r6-2: hit_test_point must NOT ghost-hit unwarped center (10,10)"
        );
        assert_eq!(
            hit_test_rect(&doc, 0, Rect2D::from_xywh(75.0, 75.0, 50.0, 50.0), true),
            vec![inst_id],
            "r6-2: fully_enclosed hit_test_rect must hit warped SymbolInstance"
        );

        // Also verify ObjectId(0) anonymous Group member inside SymbolDef with sub.perspective
        let child_zero = SceneObject::new(ObjectId(50), "InnerZero", rect_shape(20.0, 20.0))
            .with_fill(Fill::Solid(Color::CYAN));
        let mut zero_grp = SceneObject::new(
            ObjectId(0),
            "ZeroGrp",
            NodePayload::Group(vec![ObjectId(50)]),
        );
        zero_grp.perspective = Some(member_persp);
        let sym2 = SymbolDef::new("ZeroGrpPerspSym", vec![child_zero, zero_grp]);
        let sid2 = sym2.id;
        doc.insert_symbol(sym2);
        let inst2 = doc
            .add_object(
                SceneObject::new(ObjectId(0), "SymInst2", NodePayload::SymbolInstance(sid2))
                    .with_transform(Affine2D::translate(10.0, 10.0)),
                None,
            )
            .unwrap();
        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(110.0, 110.0), 1.0),
            Some(inst2),
            "r6-2: ObjectId(0) Group with sub.perspective in SymbolDef must be clickable"
        );
        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(20.0, 20.0), 1.0),
            None,
            "r6-2: ObjectId(0) Group with sub.perspective must NOT ghost-hit unwarped coords"
        );
    }

    // 3. challenger_w2exit_r6_1-r6-3: grouped attached TextOnPath with obj.perspective (no double transform)
    {
        let mut doc = Document::new("R7_GroupedTOPPersp", 300.0, 300.0, ColorMode::Rgb, 150.0);
        let mut host = SceneObject::new(
            ObjectId(0),
            "HostPath",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(0.0, 5.0),
                Point2D::new(30.0, 5.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none());
        host.visible = false;
        let host_id = doc.add_object(host, None).unwrap();

        let top_persp = Perspective3x3::from_rect_to_quad(
            Rect2D::from_xywh(0.0, 0.0, 30.0, 10.0),
            &[
                Point2D::new(80.0, 80.0),
                Point2D::new(120.0, 80.0),
                Point2D::new(120.0, 100.0),
                Point2D::new(80.0, 100.0),
            ],
        )
        .unwrap();
        let mut top = SceneObject::new(
            ObjectId(0),
            "GroupedTOP",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain("TEST", TextStyle::default()),
                path_id: host_id,
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        );
        top.perspective = Some(top_persp);
        let top_id = doc.add_object(top, None).unwrap();

        let gid = doc.group_objects(&[top_id], "TOPGroup").unwrap();
        hist.execute(
            &mut doc,
            CommandOp::TransformObjects {
                ids: vec![gid, host_id],
                affine: Affine2D::translate(40.0, 40.0),
            },
        )
        .unwrap();

        // Local host (0,5)-(30,5) -> top_persp -> (80,90)-(120,90) -> Group translate(40,40) -> (120,130)-(160,130)
        let wb = doc.world_object_bounds(gid).unwrap();
        assert!(
            (wb.min_x - 120.0).abs() < 1e-6
                && (wb.max_x - 160.0).abs() < 1e-6
                && (wb.min_y - 130.0).abs() < 1e-6
                && (wb.max_y - 130.0).abs() < 1e-6,
            "r6-3: world_object_bounds(gid) must be (120,130)-(160,130) without double-transform, got {wb:?}"
        );
        let tight_rendered_enclosing_rect = Rect2D::from_xywh(115.0, 117.0, 50.0, 20.0);
        assert_eq!(
            hit_test_rect(&doc, 0, tight_rendered_enclosing_rect, true),
            vec![gid],
            "r6-3: hit_test_rect(..., true) must enclose grouped TextOnPath(Perspective3x3)"
        );
        assert!(
            hit_test_rect(&doc, 0, Rect2D::from_xywh(170.0, 200.0, 50.0, 20.0), true).is_empty(),
            "r6-3: must NOT hit double-transformed coords (173..213, 210)"
        );
    }

    // 4. Sibling checks: Effect::Perspective on Group with hit_test_rect(..., true) and
    //    SymbolDef TextOnPath with sub.perspective
    {
        let mut doc = Document::new("R7_SiblingPersp", 300.0, 300.0, ColorMode::Rgb, 150.0);
        let r = SceneObject::new(ObjectId(0), "EffRect", rect_shape(20.0, 20.0))
            .with_fill(Fill::Solid(Color::CYAN));
        let rid = doc.add_object(r, None).unwrap();
        let gid = doc.group_objects(&[rid], "EffPerspGrp").unwrap();
        hist.execute(
            &mut doc,
            CommandOp::AddEffect {
                id: gid,
                effect: Effect::Perspective {
                    quad: [
                        Point2D::new(90.0, 90.0),
                        Point2D::new(130.0, 90.0),
                        Point2D::new(130.0, 130.0),
                        Point2D::new(90.0, 130.0),
                    ],
                },
            },
        )
        .unwrap();
        assert_eq!(
            hit_test_rect(&doc, 0, Rect2D::from_xywh(85.0, 85.0, 50.0, 50.0), true),
            vec![gid],
            "sibling: fully_enclosed hit_test_rect must hit Group with Effect::Perspective"
        );
        assert!(
            hit_test_rect(&doc, 0, Rect2D::from_xywh(-5.0, -5.0, 30.0, 30.0), true).is_empty(),
            "sibling: must NOT ghost-hit unwarped Effect::Perspective Group"
        );

        let sym_host = SceneObject::new(
            ObjectId(20),
            "SymHost",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(0.0, 5.0),
                Point2D::new(30.0, 5.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none());
        let mut sym_top = SceneObject::new(
            ObjectId(21),
            "SymTOP",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain("SYMTOP", TextStyle::default()),
                path_id: ObjectId(20),
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        );
        sym_top.perspective = Some(
            Perspective3x3::from_rect_to_quad(
                Rect2D::from_xywh(0.0, 0.0, 30.0, 10.0),
                &[
                    Point2D::new(80.0, 80.0),
                    Point2D::new(120.0, 80.0),
                    Point2D::new(120.0, 100.0),
                    Point2D::new(80.0, 100.0),
                ],
            )
            .unwrap(),
        );
        let sym_top_only = SymbolDef::new("SymTOPPersp", vec![sym_host, sym_top]);
        let stid = sym_top_only.id;
        doc.insert_symbol(sym_top_only);
        let inst = doc
            .add_object(
                SceneObject::new(ObjectId(0), "SymTOPInst", NodePayload::SymbolInstance(stid))
                    .with_transform(Affine2D::translate(40.0, 40.0)),
                None,
            )
            .unwrap();
        let sym_wb = doc.world_object_bounds(inst).unwrap();
        assert!(
            (sym_wb.max_x - 160.0).abs() < 1e-6 && (sym_wb.max_y - 130.0).abs() < 1e-6,
            "sibling: SymbolDef TextOnPath with sub.perspective must not double-transform: {sym_wb:?}"
        );
    }
}

#[test]
fn w2_exit_r8_symbol_group_top_persp_connector_and_zero_id_group_ancestors() {
    use coree_core::hit_test::{hit_test_point, hit_test_rect, sym_sub_world_bounds};
    use coree_core::{
        AnchorPoint, ArtisticText, ConnectorObject, ConnectorStyle, SymbolDef, TextOnPath,
        TextStyle,
    };
    use std::collections::{HashMap, HashSet};

    // (a) Probe P2: SymbolDef Group(translate(40,40)) + TextOnPath(sub.perspective) + Connector
    {
        let mut doc = Document::new("W2ExitR8_P2", 300.0, 300.0, ColorMode::Rgb, 150.0);
        let host = SceneObject::new(
            ObjectId(10),
            "Host",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(0.0, 5.0),
                Point2D::new(30.0, 5.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none());
        let mut top = SceneObject::new(
            ObjectId(11),
            "GroupedSymTOP",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain("WARP", TextStyle::default()),
                path_id: ObjectId(10),
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        );
        top.perspective = Some(
            Perspective3x3::from_rect_to_quad(
                Rect2D::from_xywh(0.0, 0.0, 30.0, 10.0),
                &[
                    Point2D::new(80.0, 80.0),
                    Point2D::new(120.0, 80.0),
                    Point2D::new(120.0, 100.0),
                    Point2D::new(80.0, 100.0),
                ],
            )
            .unwrap(),
        );
        let grp = SceneObject::new(
            ObjectId(12),
            "SymGrp",
            NodePayload::Group(vec![ObjectId(10), ObjectId(11)]),
        )
        .with_transform(Affine2D::translate(40.0, 40.0));
        let conn = SceneObject::new(
            ObjectId(13),
            "SymConn",
            NodePayload::Connector(ConnectorObject::between_objects(
                ObjectId(11),
                AnchorPoint::Center,
                ObjectId(999),
                AnchorPoint::Center,
                ConnectorStyle::Straight,
            )),
        )
        .with_outline(OutlineStyle::solid(Color::BLACK, 1.0));
        let mut conn_fixed = conn;
        if let NodePayload::Connector(ref mut c) = conn_fixed.payload {
            c.to_obj = None;
            c.to_fallback = Point2D::new(140.0, 180.0);
        }

        let sym = SymbolDef::new("P2Sym", vec![host, top, grp, conn_fixed]);
        let sid = sym.id;
        doc.insert_symbol(sym);
        let inst_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "P2Inst", NodePayload::SymbolInstance(sid)),
                None,
            )
            .unwrap();

        let sym_ref = doc.symbol(sid).unwrap();
        let sym_map: HashMap<ObjectId, &SceneObject> = sym_ref
            .objects
            .iter()
            .filter(|s| s.id.0 != 0)
            .map(|s| (s.id, s))
            .collect();
        let mut visiting = HashMap::new();
        let mut visited_syms = HashSet::new();
        let mut cache = HashMap::new();
        let (top_bb, _) = sym_sub_world_bounds(
            &doc,
            ObjectId(11),
            &sym_map,
            Affine2D::IDENTITY,
            &mut visiting,
            &mut visited_syms,
            &mut cache,
            0,
        );
        let top_bb = top_bb.expect("sym_sub_world_bounds for grouped TextOnPath");
        assert!(
            (top_bb.center().x - 140.0).abs() < 1e-4 && (top_bb.center().y - 130.0).abs() < 1e-4,
            "expected grouped TextOnPath center (140, 130), got {:?}",
            top_bb.center()
        );
        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(140.0, 155.0), 1.0),
            Some(inst_id),
            "Connector midpoint from grouped TextOnPath(sub.perspective) must hit at (140, 155)"
        );
        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(140.0, 130.0), 1.0),
            Some(inst_id),
            "Grouped TextOnPath(sub.perspective) must hit at (140, 130)"
        );
    }

    // (b) Probe P5: SymbolDef ObjectId(0) Group(translate(10,10), perspective) + TextOnPath(sub.perspective)
    {
        let mut doc = Document::new("W2ExitR8_P5", 300.0, 300.0, ColorMode::Rgb, 150.0);
        let mut host = SceneObject::new(
            ObjectId(20),
            "Host20",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(0.0, 5.0),
                Point2D::new(30.0, 5.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none());
        host.visible = false;
        let mut top = SceneObject::new(
            ObjectId(21),
            "Top21",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain("ZERO", TextStyle::default()),
                path_id: ObjectId(20),
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        );
        top.perspective = Some(
            Perspective3x3::from_rect_to_quad(
                Rect2D::from_xywh(0.0, 0.0, 30.0, 10.0),
                &[
                    Point2D::new(20.0, 20.0),
                    Point2D::new(60.0, 20.0),
                    Point2D::new(60.0, 40.0),
                    Point2D::new(20.0, 40.0),
                ],
            )
            .unwrap(),
        );
        let mut zero_grp = SceneObject::new(
            ObjectId(0),
            "ZeroGrp",
            NodePayload::Group(vec![ObjectId(20), ObjectId(21)]),
        )
        .with_transform(Affine2D::translate(10.0, 10.0));
        zero_grp.perspective = Some(
            Perspective3x3::from_rect_to_quad(
                Rect2D::from_xywh(20.0, 20.0, 40.0, 20.0),
                &[
                    Point2D::new(100.0, 100.0),
                    Point2D::new(180.0, 100.0),
                    Point2D::new(180.0, 140.0),
                    Point2D::new(100.0, 140.0),
                ],
            )
            .unwrap(),
        );

        let sym = SymbolDef::new("P5Sym", vec![zero_grp, host, top]);
        let sid = sym.id;
        doc.insert_symbol(sym);
        let inst_id = doc
            .add_object(
                SceneObject::new(ObjectId(0), "P5Inst", NodePayload::SymbolInstance(sid))
                    .with_transform(Affine2D::translate(20.0, 20.0)),
                None,
            )
            .unwrap();

        // Host (0,5)-(30,5) -> top.persp -> (20,30)-(60,30) -> zero_grp.persp -> (100,120)-(180,120)
        // -> zero_grp.translate(10,10) + inst.translate(20,20) => (130, 150)-(210, 150)
        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(150.0, 150.0), 5.0),
            Some(inst_id),
            "ObjectId(0) Group(translate, persp) + TextOnPath(sub.persp) must hit at (150, 150)"
        );
        assert_eq!(
            hit_test_point(&doc, 0, Point2D::new(120.0, 100.0), 5.0),
            None,
            "Must NOT ghost-hit shifted coords missing ObjectId(0) Group transform"
        );
        assert_eq!(
            hit_test_rect(&doc, 0, Rect2D::from_xywh(125.0, 135.0, 90.0, 30.0), true),
            vec![inst_id],
            "hit_test_rect(..., true) must enclose ObjectId(0) Group + TextOnPath at (125..215, 135..165)"
        );
    }
}

#[test]
fn w3_exit_r5_linked_paragraph_frame_targeted_invalidation_edit_unlink_transform_delete_cycle() {
    use coree_core::{
        CommandHistory, CommandOp, DocumentInvalidation, MasterLayerScope, ParagraphText,
        TextStyle, TextWrapMode, Vec2D,
    };

    let mut doc = Document::new("W3ExitR5LinkedPara", 320.0, 320.0, ColorMode::Rgb, 96.0);
    let page_id = doc.pages()[0].id;
    let lid1 = doc.pages()[0].layers[0].id;
    let lid2 = doc
        .add_layer(page_id, "Layer2", true, MasterLayerScope::None)
        .unwrap();
    let mut hist = CommandHistory::default();

    let style = TextStyle {
        font_size_pt: 14.0,
        fill: Color::rgba8(10, 20, 30, 255),
        ..TextStyle::default()
    };

    // Frame3 on Layer2 inside a Group, Frame2 on Layer2, Frame1 on Layer1 -> Frame2 -> Frame3
    let f3 = SceneObject::new(
        ObjectId(0),
        "Frame3",
        NodePayload::ParagraphText(ParagraphText::new(
            "",
            Vec2D::new(50.0, 50.0),
            style.clone(),
        )),
    )
    .with_transform(Affine2D::translate(80.0, 10.0));
    hist.execute(&mut doc, CommandOp::create_object(f3, Some(lid2)))
        .unwrap();
    let f3_id = *doc.objects().keys().max().unwrap();

    let sib = SceneObject::new(ObjectId(0), "Sib", rect_shape(5.0, 5.0))
        .with_transform(Affine2D::translate(80.0, 70.0));
    hist.execute(&mut doc, CommandOp::create_object(sib, Some(lid2)))
        .unwrap();
    let sib_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::Group {
            ids: vec![f3_id, sib_id],
            name: "Frame3Grp".to_owned(),
        },
    )
    .unwrap();
    let grp3_id = *doc.objects().keys().max().unwrap();

    let mut p2 = ParagraphText::new("", Vec2D::new(50.0, 50.0), style.clone());
    p2.linked_next_frame = Some(f3_id);
    let f2 = SceneObject::new(ObjectId(0), "Frame2", NodePayload::ParagraphText(p2))
        .with_transform(Affine2D::translate(10.0, 10.0));
    hist.execute(&mut doc, CommandOp::create_object(f2, Some(lid2)))
        .unwrap();
    let f2_id = *doc.objects().keys().max().unwrap();

    let mut p1 = ParagraphText::new(
        "FIRST WORD SECOND THIRD FOURTH FIFTH SIXTH SEVENTH",
        Vec2D::new(25.0, 12.0),
        style.clone(),
    );
    p1.linked_next_frame = Some(f2_id);
    let f1 = SceneObject::new(
        ObjectId(0),
        "Frame1",
        NodePayload::ParagraphText(p1.clone()),
    )
    .with_transform(Affine2D::translate(250.0, 250.0));
    hist.execute(&mut doc, CommandOp::create_object(f1, Some(lid1)))
        .unwrap();
    let f1_id = *doc.objects().keys().max().unwrap();

    let assert_chain_targeted = |inv: &DocumentInvalidation, expect_f1: bool, phase: &str| match inv
    {
        DocumentInvalidation::Targeted {
            object_ids,
            dirty_rects_mm,
        } => {
            if expect_f1 {
                assert!(
                    object_ids.contains(&f1_id),
                    "[{phase}] missing Frame1 {f1_id:?} in {object_ids:?}"
                );
            }
            assert!(
                object_ids.contains(&f2_id),
                "[{phase}] missing Frame2 {f2_id:?} in {object_ids:?}"
            );
            assert!(
                object_ids.contains(&f3_id),
                "[{phase}] missing Frame3 {f3_id:?} in {object_ids:?}"
            );
            assert!(
                object_ids.contains(&grp3_id),
                "[{phase}] missing enclosing Group {grp3_id:?} in {object_ids:?}"
            );
            let f2_bb = Rect2D::from_xywh(10.0, 10.0, 50.0, 50.0);
            let f3_bb = Rect2D::from_xywh(80.0, 10.0, 50.0, 50.0);
            assert!(
                dirty_rects_mm.iter().any(|r| r.contains_rect(f2_bb)),
                "[{phase}] missing Frame2 dirty rect {f2_bb:?}: {dirty_rects_mm:?}"
            );
            assert!(
                dirty_rects_mm.iter().any(|r| r.contains_rect(f3_bb)),
                "[{phase}] missing Frame3 dirty rect {f3_bb:?}: {dirty_rects_mm:?}"
            );
        }
        DocumentInvalidation::All => panic!("[{phase}] Expected Targeted invalidation"),
    };

    // 1. EditText on Frame1 (execute + undo + redo)
    let mut p1_short = ParagraphText::new("SHORT", Vec2D::new(25.0, 12.0), style.clone());
    p1_short.linked_next_frame = Some(f2_id);
    hist.execute(
        &mut doc,
        CommandOp::EditText {
            id: f1_id,
            payload: NodePayload::ParagraphText(p1_short.clone()),
        },
    )
    .unwrap();
    assert_chain_targeted(hist.last_invalidation(), true, "EditText execute");
    hist.undo(&mut doc).unwrap();
    assert_chain_targeted(hist.last_invalidation(), true, "EditText undo");
    hist.redo(&mut doc).unwrap();
    assert_chain_targeted(hist.last_invalidation(), true, "EditText redo");

    // 2. TransformObjects on Frame1 (execute + undo + redo)
    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![f1_id],
            affine: Affine2D::translate(5.0, 5.0),
        },
    )
    .unwrap();
    assert_chain_targeted(hist.last_invalidation(), true, "Transform execute");
    hist.undo(&mut doc).unwrap();
    assert_chain_targeted(hist.last_invalidation(), true, "Transform undo");
    hist.redo(&mut doc).unwrap();
    assert_chain_targeted(hist.last_invalidation(), true, "Transform redo");

    // 3. Unlink Frame1 (linked_next_frame: Some(f2_id) -> None) (execute + undo + redo)
    let mut p1_unlinked = p1_short;
    p1_unlinked.linked_next_frame = None;
    hist.execute(
        &mut doc,
        CommandOp::EditText {
            id: f1_id,
            payload: NodePayload::ParagraphText(p1_unlinked),
        },
    )
    .unwrap();
    assert_chain_targeted(hist.last_invalidation(), true, "Unlink execute");
    hist.undo(&mut doc).unwrap();
    assert_chain_targeted(hist.last_invalidation(), true, "Unlink undo");

    // 4. DeleteObjects on Frame1 (now linked again after undo) (execute + undo + redo)
    hist.execute(&mut doc, CommandOp::DeleteObjects { ids: vec![f1_id] })
        .unwrap();
    assert_chain_targeted(hist.last_invalidation(), true, "Delete execute");
    hist.undo(&mut doc).unwrap();
    assert_chain_targeted(hist.last_invalidation(), true, "Delete undo");
    hist.redo(&mut doc).unwrap();
    assert_chain_targeted(hist.last_invalidation(), true, "Delete redo");

    // 5. Cycle FrameA -> FrameB -> FrameA terminates cleanly without infinite loop
    let fa = SceneObject::new(
        ObjectId(0),
        "CycleA",
        NodePayload::ParagraphText(ParagraphText::new(
            "CYCLE A",
            Vec2D::new(30.0, 20.0),
            style.clone(),
        )),
    );
    hist.execute(&mut doc, CommandOp::create_object(fa, Some(lid1)))
        .unwrap();
    let fa_id = *doc.objects().keys().max().unwrap();
    let mut pb = ParagraphText::new("CYCLE B", Vec2D::new(30.0, 20.0), style.clone());
    pb.linked_next_frame = Some(fa_id);
    let fb = SceneObject::new(ObjectId(0), "CycleB", NodePayload::ParagraphText(pb))
        .with_transform(Affine2D::translate(40.0, 0.0));
    hist.execute(&mut doc, CommandOp::create_object(fb, Some(lid1)))
        .unwrap();
    let fb_id = *doc.objects().keys().max().unwrap();
    let mut pa_cycled = ParagraphText::new("CYCLE A2", Vec2D::new(30.0, 20.0), style.clone());
    pa_cycled.linked_next_frame = Some(fb_id);
    hist.execute(
        &mut doc,
        CommandOp::EditText {
            id: fa_id,
            payload: NodePayload::ParagraphText(pa_cycled),
        },
    )
    .unwrap();
    match hist.last_invalidation() {
        DocumentInvalidation::Targeted { object_ids, .. } => {
            assert!(object_ids.contains(&fa_id) && object_ids.contains(&fb_id));
        }
        DocumentInvalidation::All => panic!("Expected Targeted for linked cycle"),
    }

    // 6. TextWrapMode obstacle moving over FrameW1 -> invalidates FrameW1 AND linked FrameW2
    let fw2 = SceneObject::new(
        ObjectId(0),
        "WrapF2",
        NodePayload::ParagraphText(ParagraphText::new(
            "",
            Vec2D::new(40.0, 40.0),
            style.clone(),
        )),
    )
    .with_transform(Affine2D::translate(180.0, 120.0));
    hist.execute(&mut doc, CommandOp::create_object(fw2, Some(lid2)))
        .unwrap();
    let fw2_id = *doc.objects().keys().max().unwrap();

    let mut pw1 = ParagraphText::new(
        "WRAP TEXT OVERFLOW INTO SECOND FRAME",
        Vec2D::new(40.0, 40.0),
        style,
    );
    pw1.wrap_mode = TextWrapMode::SquareBoth;
    pw1.linked_next_frame = Some(fw2_id);
    let fw1 = SceneObject::new(ObjectId(0), "WrapF1", NodePayload::ParagraphText(pw1))
        .with_transform(Affine2D::translate(120.0, 120.0));
    hist.execute(&mut doc, CommandOp::create_object(fw1, Some(lid1)))
        .unwrap();
    let fw1_id = *doc.objects().keys().max().unwrap();

    let obs = SceneObject::new(ObjectId(0), "Obstacle", rect_shape(15.0, 15.0))
        .with_transform(Affine2D::translate(125.0, 125.0));
    hist.execute(&mut doc, CommandOp::create_object(obs, Some(lid1)))
        .unwrap();
    let obs_id = *doc.objects().keys().max().unwrap();

    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![obs_id],
            affine: Affine2D::translate(5.0, 5.0),
        },
    )
    .unwrap();
    match hist.last_invalidation() {
        DocumentInvalidation::Targeted { object_ids, .. } => {
            assert!(
                object_ids.contains(&fw1_id) && object_ids.contains(&fw2_id),
                "Obstacle move must invalidate WrapF1 and linked WrapF2: {object_ids:?}"
            );
        }
        DocumentInvalidation::All => panic!("Expected Targeted for TextWrapMode obstacle move"),
    }
}

#[test]
fn w5a_drawing_scale_page_bg_reorder_layers_and_autofield() {
    use coree_core::{
        ArtisticText, CommandHistory, CommandOp, DimensionKind, DimensionObject, Document,
        DrawingScale, Fill, MasterLayerScope, PageId, TextField, TextRun, TextStyle, Unit,
    };

    let mut doc = Document::new("W5A Page & Scale", 210.0, 297.0, ColorMode::Cmyk, 300.0);
    let mut hist = CommandHistory::new(32);

    let dim = DimensionObject::linear(
        DimensionKind::Horizontal,
        Point2D::new(20.0, 50.0),
        Point2D::new(100.0, 50.0),
        8.0,
    );
    assert_eq!(
        dim.compute_measurement_with_scale(Unit::Millimeter, 300.0, doc.drawing_scale()),
        "80.00 mm"
    );

    hist.execute(
        &mut doc,
        CommandOp::SetDocumentProperties {
            title: None,
            color_mode: None,
            default_unit: None,
            dpi: None,
            multipage_view: None,
            active_page_index: None,
            metadata: None,
            custom_icc_profile: None,
            clear_custom_icc: false,
            rgb_icc_profile: None,
            clear_rgb_icc: false,
            cmyk_icc_profile: None,
            clear_cmyk_icc: false,
            gray_icc_profile: None,
            clear_gray_icc: false,
            rendering_intent: None,
            black_point_compensation: None,
            drawing_scale: Some(DrawingScale::new(1.0, 10.0)),
        },
    )
    .unwrap();
    assert_eq!(doc.drawing_scale(), DrawingScale::new(1.0, 10.0));
    assert_eq!(
        dim.compute_measurement_with_scale(Unit::Millimeter, 300.0, doc.drawing_scale()),
        "800.00 mm"
    );
    hist.undo(&mut doc).unwrap();
    assert_eq!(doc.drawing_scale(), DrawingScale::default());
    hist.redo(&mut doc).unwrap();
    assert_eq!(doc.drawing_scale(), DrawingScale::new(1.0, 10.0));

    // Page background_fill & printable_background + ReorderLayers
    let p1_id: PageId = doc.pages()[0].id;
    let l1_id = doc.pages()[0].layers[0].id;
    let l2_id = doc
        .add_layer(p1_id, "Overlay", false, MasterLayerScope::None)
        .unwrap();
    let bg = Fill::Solid(Color::rgb(0.2, 0.4, 0.8));
    hist.execute(
        &mut doc,
        CommandOp::SetPageProperties {
            page_id: p1_id,
            name: None,
            width_mm: None,
            height_mm: None,
            bleed_mm: None,
            facing_pages: Some(true),
            background_fill: Some(bg.clone()),
            printable_background: Some(true),
        },
    )
    .unwrap();
    assert!(doc.pages()[0].printable_background);
    assert_eq!(doc.pages()[0].background_fill, bg);

    hist.execute(
        &mut doc,
        CommandOp::ReorderLayers {
            page_id: p1_id,
            layer_ids: vec![l2_id, l1_id],
        },
    )
    .unwrap();
    assert_eq!(doc.pages()[0].layers[0].id, l2_id);
    hist.undo(&mut doc).unwrap();
    assert_eq!(doc.pages()[0].layers[0].id, l1_id);

    // AutoPageNumber dynamic text field resolution
    doc.add_page("Page 2", 210.0, 297.0, 3.0);
    let at = ArtisticText {
        runs: vec![
            TextRun::new("Page ", TextStyle::default()),
            TextRun::new("#", TextStyle::default()).with_field(TextField::AutoPageNumber),
            TextRun::new(" / ", TextStyle::default()),
            TextRun::new("N", TextStyle::default()).with_field(TextField::PageCount),
        ],
        ..ArtisticText::default()
    };
    assert_eq!(
        doc.resolve_artistic_text_fields(0, &at).plain_text(),
        "Page 1 / 2"
    );
    assert_eq!(
        doc.resolve_artistic_text_fields(1, &at).plain_text(),
        "Page 2 / 2"
    );
}

#[test]
fn w5a_non_aabb_hit_test_and_5_geometric_snaps_precision() {
    use coree_core::{
        hit_test_node, hit_test_point, hit_test_point_exact, stroke_to_closed_path, ArtisticText,
        BSplineCurve, BezierPath, DynamicGuideKind, EllipseMode, OutlineStyle, ParametricShape,
        SnapEngine, SnapOptions, TextOnPath, TextStyle, VectorTable,
    };

    let mut doc = Document::new_a4("Precision Hit & Snaps");

    // 1. Unfilled Table: hit border, miss empty cell center
    let table_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "UnfilledTable",
                NodePayload::Table(VectorTable::new(2, 2, 20.0, 10.0)),
            )
            .with_transform(Affine2D::translate(10.0, 10.0)),
            None,
        )
        .unwrap();
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(20.0, 15.0), 0.5),
        None,
        "Empty interior of unfilled table cell must NOT hit"
    );
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(30.0, 15.0), 0.5),
        Some(table_id),
        "Table inner vertical border must hit"
    );
    assert_eq!(
        hit_test_point_exact(&doc, 0, Point2D::new(20.0, 15.0), 0.5, true),
        Some(table_id),
        "treat_all_filled=true hits table cell interior"
    );

    // 2. TextOnPath arc-length span: hits start of circle, misses opposite far side
    let host_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "CircleHost",
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: 40.0,
                    ry: 40.0,
                    mode: EllipseMode::Full,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
            )
            .with_transform(Affine2D::translate(120.0, 120.0)),
            None,
        )
        .unwrap();
    let top = TextOnPath {
        text: ArtisticText::from_plain("Hi", TextStyle::default()),
        path_id: host_id,
        offset_mm: 0.0,
        distance_mm: 0.0,
        flip_vertical: false,
        flip_horizontal: false,
    };
    let top_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "ShortTOP", NodePayload::TextOnPath(top)),
            None,
        )
        .unwrap();
    // Point on far opposite arc (x=80, y=120) must hit CircleHost (not ShortTOP)
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(80.0, 120.0), 0.5),
        Some(host_id),
        "TextOnPath must not claim the entire host path outside text span"
    );
    let _ = top_id;

    // 3. Multi-line ArtisticText: miss trailing whitespace on short line 2
    let at_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "MultiLineText",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "LONG FIRST LINE HERE\nA",
                    TextStyle::default(),
                )),
            )
            .with_transform(Affine2D::translate(10.0, 200.0)),
            None,
        )
        .unwrap();
    // Default font_size_pt=24pt -> 8.47mm, line_h ~ 10.16mm -> Line 2 is y ~ 210.2..220.3
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(50.0, 215.0), 0.2),
        None,
        "Empty whitespace right of short line 2 must not hit ArtisticText"
    );
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(11.5, 215.0), 0.2),
        Some(at_id)
    );

    // 4. BSpline hit_test_node includes control points
    let bs = BSplineCurve {
        control_points: vec![
            Point2D::new(10.0, 250.0),
            Point2D::new(30.0, 235.0),
            Point2D::new(50.0, 250.0),
        ],
        degree: 2,
        knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        closed: false,
    };
    let bs_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "BS", NodePayload::BSpline(bs)),
            None,
        )
        .unwrap();
    let nh = hit_test_node(&doc, &[bs_id], Point2D::new(30.2, 235.1), 1.0)
        .expect("BSpline control point must be hit-testable");
    assert!((nh.position_mm.x - 30.0).abs() < 1e-6);

    // 5. All 5 geometric snaps (< 0.01 mm accuracy)
    let opts = SnapOptions {
        nodes: false,
        objects: false,
        guidelines: false,
        page_edges: false,
        grid: false,
        tangent: true,
        perpendicular: true,
        quadrant: true,
        intersection: true,
        baseline: true,
        dynamic_guides: true,
        tolerance_mm: 5.0,
    };

    // Quadrant on CircleHost at (120, 120) r=40 -> top quadrant (120, 80)
    let (q_pt, q_guides) =
        SnapEngine::snap_point_with_reference(&doc, 0, Point2D::new(121.0, 81.0), None, &[], &opts);
    assert!(q_pt.distance_to(Point2D::new(120.0, 80.0)) < 0.01);
    assert!(q_guides
        .iter()
        .any(|g| g.kind == DynamicGuideKind::Quadrant));

    // Tangent from external point (120, 40) to CircleHost at (120, 120) r=40
    // d = 80, r = 40 -> sin(theta)=0.5 -> tangent points at (120 +/- 20*sqrt(3), 100)
    let expected_tx = 120.0 + 20.0 * 3.0_f64.sqrt();
    let (t_pt, t_guides) = SnapEngine::snap_point_with_reference(
        &doc,
        0,
        Point2D::new(expected_tx + 0.5, 100.5),
        Some(Point2D::new(120.0, 40.0)),
        &[],
        &opts,
    );
    assert!(
        t_pt.distance_to(Point2D::new(expected_tx, 100.0)) < 0.01,
        "Tangent error: got {t_pt:?}, expected ({expected_tx}, 100.0)"
    );
    assert!(t_guides.iter().any(|g| g.kind == DynamicGuideKind::Tangent));

    // Perpendicular & Intersection on two crossing lines
    let line_h = BezierPath::from_line(Point2D::new(10.0, 170.0), Point2D::new(70.0, 170.0));
    let line_v = BezierPath::from_line(Point2D::new(40.0, 140.0), Point2D::new(40.0, 195.0));
    doc.add_object(
        SceneObject::new(ObjectId(0), "LH", NodePayload::Path(line_h)),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(ObjectId(0), "LV", NodePayload::Path(line_v)),
        None,
    )
    .unwrap();

    let (i_pt, i_guides) =
        SnapEngine::snap_point_with_reference(&doc, 0, Point2D::new(41.0, 171.0), None, &[], &opts);
    assert!(i_pt.distance_to(Point2D::new(40.0, 170.0)) < 0.01);
    assert!(i_guides
        .iter()
        .any(|g| g.kind == DynamicGuideKind::Intersection));

    let (p_pt, p_guides) = SnapEngine::snap_point_with_reference(
        &doc,
        0,
        Point2D::new(25.5, 170.8),
        Some(Point2D::new(25.0, 150.0)),
        &[],
        &SnapOptions {
            intersection: false,
            ..opts
        },
    );
    assert!(
        p_pt.distance_to(Point2D::new(25.0, 170.0)) < 0.01,
        "Perp error: {p_pt:?}"
    );
    assert!(p_guides
        .iter()
        .any(|g| g.kind == DynamicGuideKind::Perpendicular));

    // Baseline on MultiLineText at y = 200 + font_mm * 0.85
    let (b_pt, b_guides) =
        SnapEngine::snap_point_with_reference(&doc, 0, Point2D::new(22.0, 203.0), None, &[], &opts);
    assert!(b_guides
        .iter()
        .any(|g| g.kind == DynamicGuideKind::Baseline));
    assert!((b_pt.y - (200.0 + TextStyle::default().font_size_mm() * 0.85)).abs() < 0.01);

    // 6. Calligraphy closed subpath emits outer + inner hole subpaths (#16)
    let closed_circle = ParametricShape::Ellipse {
        rx: 20.0,
        ry: 20.0,
        mode: EllipseMode::Full,
        start_deg: 0.0,
        end_deg: 360.0,
    }
    .to_bezier_path();
    let mut calli = OutlineStyle::solid(Color::BLACK, 4.0);
    calli.calligraphy_angle_deg = 45.0;
    calli.calligraphy_stretch = 0.25;
    let stroked = stroke_to_closed_path(&closed_circle, &calli, None).unwrap();
    assert!(
        stroked.subpaths.len() >= 2,
        "Closed calligraphy stroke must emit outer + inner hole subpaths, got {}",
        stroked.subpaths.len()
    );

    // 7. Unfilled ParagraphText: hits border & wrapped text lines, misses empty frame interior
    use coree_core::{hit_test_rect, ParagraphText, Perspective3x3};
    let mut para = ParagraphText::new("Short line", Vec2D::new(80.0, 50.0), TextStyle::default());
    para.runs[0].style.font_size_pt = 12.0; // ~4.23mm font -> ~5.08mm line height
    let para_obj = SceneObject::new(
        ObjectId(0),
        "UnfilledPara",
        NodePayload::ParagraphText(para),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(150.0, 10.0));
    let para_id = doc.add_object(para_obj, None).unwrap();

    // Empty lower region inside [150..230, 10..60] must NOT hit when unfilled
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(190.0, 45.0), 0.3),
        None,
        "Unfilled ParagraphText empty lower interior must NOT hit"
    );
    assert!(
        !hit_test_rect(&doc, 0, Rect2D::from_xywh(180.0, 40.0, 15.0, 12.0), false)
            .contains(&para_id),
        "Unfilled ParagraphText empty lower marquee must NOT hit"
    );
    // Point on wrapped text line at top-left hits
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(155.0, 13.0), 0.3),
        Some(para_id),
        "Unfilled ParagraphText wrapped text line must hit"
    );
    // Point on frame perimeter border (x=150, y=45) hits
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(150.0, 45.0), 0.3),
        Some(para_id),
        "Unfilled ParagraphText frame border must hit"
    );
    // treat_all_filled=true hits empty interior
    assert_eq!(
        hit_test_point_exact(&doc, 0, Point2D::new(190.0, 45.0), 0.3, true),
        Some(para_id),
        "treat_all_filled=true must hit ParagraphText interior"
    );

    // Unfilled ParagraphText with Effect::Perspective preserves full-frame homography
    let mut persp_para = ParagraphText::new("Top", Vec2D::new(80.0, 50.0), TextStyle::default());
    persp_para.runs[0].style.font_size_pt = 12.0;
    let quad = [
        Point2D::new(0.0, 0.0),
        Point2D::new(80.0, 0.0),
        Point2D::new(80.0, 50.0),
        Point2D::new(0.0, 50.0),
    ];
    let _ = Perspective3x3::from_rect_to_quad(Rect2D::from_xywh(0.0, 0.0, 80.0, 50.0), &quad);
    let mut p_obj = SceneObject::new(
        ObjectId(0),
        "PerspPara",
        NodePayload::ParagraphText(persp_para),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(250.0, 10.0));
    p_obj.effects.push(Effect::Perspective {
        quad: [
            Point2D::new(10.0, 0.0),
            Point2D::new(70.0, 0.0),
            Point2D::new(80.0, 50.0),
            Point2D::new(0.0, 50.0),
        ],
    });
    let p_para_id = doc.add_object(p_obj, None).unwrap();
    // Warped top line center (local ~20..60, y~2 -> world ~262, 12) hits
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(262.0, 12.5), 0.5),
        Some(p_para_id),
        "Warped ParagraphText top line must hit under Effect::Perspective"
    );
    // Warped lower interior (world ~290, 45) misses when unfilled
    assert_eq!(
        hit_test_point(&doc, 0, Point2D::new(290.0, 45.0), 0.3),
        None,
        "Warped unfilled ParagraphText lower empty region must NOT hit"
    );
}

#[test]
fn command_set_page_properties_and_reorder_layers_undo_redo_and_guards() {
    use coree_core::{CommandHistory, CommandOp};

    let mut doc = Document::new_a4("Command Page & Layer Ops");
    let mut hist = CommandHistory::new(32);
    let page_id = doc.pages()[0].id;
    let l1 = doc.pages()[0].layers[0].id;
    let l2 = doc
        .add_layer(
            page_id,
            "Artwork",
            false,
            coree_core::MasterLayerScope::None,
        )
        .unwrap();
    let l3 = doc
        .add_layer(page_id, "Cut", false, coree_core::MasterLayerScope::None)
        .unwrap();

    // 1. Guard rejections must fail cleanly without mutating document
    let bad_w = hist.execute(
        &mut doc,
        CommandOp::SetPageProperties {
            page_id,
            name: None,
            width_mm: Some(0.0),
            height_mm: None,
            bleed_mm: None,
            facing_pages: None,
            background_fill: None,
            printable_background: None,
        },
    );
    assert!(bad_w.is_err(), "non-positive width_mm must fail");

    let bad_bleed = hist.execute(
        &mut doc,
        CommandOp::SetPageProperties {
            page_id,
            name: None,
            width_mm: None,
            height_mm: None,
            bleed_mm: Some(-1.0),
            facing_pages: None,
            background_fill: None,
            printable_background: None,
        },
    );
    assert!(bad_bleed.is_err(), "negative bleed_mm must fail");

    let bad_reorder = hist.execute(
        &mut doc,
        CommandOp::ReorderLayers {
            page_id,
            layer_ids: vec![l1, l1, l3],
        },
    );
    assert!(bad_reorder.is_err(), "duplicate LayerId reorder must fail");
    assert_eq!(
        doc.pages()[0]
            .layers
            .iter()
            .map(|l| l.id)
            .collect::<Vec<_>>(),
        vec![l1, l2, l3]
    );

    // 2. Valid SetPageProperties + Undo/Redo
    let orig_w = doc.pages()[0].width_mm;
    let orig_h = doc.pages()[0].height_mm;
    let orig_facing = doc.pages()[0].facing_pages;
    let orig_bg = doc.pages()[0].background_fill.clone();
    let orig_print_bg = doc.pages()[0].printable_background;
    let bg_cream = Fill::Solid(Color::rgba8(255, 250, 235, 255));
    hist.execute(
        &mut doc,
        CommandOp::SetPageProperties {
            page_id,
            name: Some("SpreadCover".to_owned()),
            width_mm: Some(148.0),
            height_mm: Some(210.0),
            bleed_mm: Some(3.0),
            facing_pages: Some(true),
            background_fill: Some(bg_cream.clone()),
            printable_background: Some(true),
        },
    )
    .unwrap();
    assert_eq!(doc.pages()[0].name, "SpreadCover");
    assert!((doc.pages()[0].width_mm - 148.0).abs() < 1e-6);
    assert!((doc.pages()[0].height_mm - 210.0).abs() < 1e-6);
    assert!((doc.pages()[0].bleed_mm - 3.0).abs() < 1e-6);
    assert!(doc.pages()[0].facing_pages);
    assert_eq!(doc.pages()[0].background_fill, bg_cream);
    assert!(doc.pages()[0].printable_background);

    // 3. Valid ReorderLayers + Undo/Redo
    hist.execute(
        &mut doc,
        CommandOp::ReorderLayers {
            page_id,
            layer_ids: vec![l3, l1, l2],
        },
    )
    .unwrap();
    assert_eq!(
        doc.pages()[0]
            .layers
            .iter()
            .map(|l| l.id)
            .collect::<Vec<_>>(),
        vec![l3, l1, l2]
    );

    hist.undo(&mut doc).unwrap();
    assert_eq!(
        doc.pages()[0]
            .layers
            .iter()
            .map(|l| l.id)
            .collect::<Vec<_>>(),
        vec![l1, l2, l3]
    );
    hist.undo(&mut doc).unwrap();
    assert!((doc.pages()[0].width_mm - orig_w).abs() < 1e-6);
    assert!((doc.pages()[0].height_mm - orig_h).abs() < 1e-6);
    assert_eq!(doc.pages()[0].facing_pages, orig_facing);
    assert_eq!(doc.pages()[0].background_fill, orig_bg);
    assert_eq!(doc.pages()[0].printable_background, orig_print_bg);

    hist.redo(&mut doc).unwrap();
    assert_eq!(doc.pages()[0].name, "SpreadCover");
    assert!(doc.pages()[0].printable_background);
    hist.redo(&mut doc).unwrap();
    assert_eq!(
        doc.pages()[0]
            .layers
            .iter()
            .map(|l| l.id)
            .collect::<Vec<_>>(),
        vec![l3, l1, l2]
    );
}

#[test]
fn extreme_scale_bezier_curve_1e15_flattens_under_budget() {
    let mut kb = kurbo::BezPath::new();
    kb.move_to((0.0, 0.0));
    kb.curve_to((0.0, 10.0), (10.0, 10.0), (10.0, 0.0));
    kb.close_path();
    let curve = BezierPath::from_kurbo(&kb);
    let huge = curve.transformed(&Affine2D::new(1e15, 0.0, 0.0, 1e15, 1e15, -1e15));
    let t0 = std::time::Instant::now();
    let polys = huge.flatten(0.25);
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(200),
        "1e15 BezierPath::flatten took too long: {:?}",
        t0.elapsed()
    );
    let total_pts: usize = polys.iter().map(Vec::len).sum();
    assert!(
        total_pts > 0 && total_pts < 10_000,
        "expected finite bounded vertex count, got {total_pts}"
    );
    assert!(polys.iter().flatten().all(|p| p.is_finite()));

    let non_finite = BezierPath {
        subpaths: vec![Subpath {
            nodes: vec![
                PathNode::corner(Point2D::new(f64::INFINITY, 0.0)),
                PathNode::corner(Point2D::new(10.0, 0.0)),
                PathNode::corner(Point2D::new(10.0, 10.0)),
            ],
            closed: true,
        }],
    };
    assert!(non_finite.flatten(0.25).is_empty());
}

#[test]
fn degenerate_json_page_and_warped_symbol_powerclip_clone_bounds_hit_test() {
    let doc = Document::new("Tier5Degenerate", 100.0, 100.0, ColorMode::Rgb, 72.0);
    let mut val = serde_json::to_value(&doc).unwrap();
    val["pages"][0]["width_mm"] = serde_json::json!(0.0);
    val["pages"][0]["height_mm"] = serde_json::json!(-50.0);
    let round: Document = serde_json::from_value(val).unwrap();
    assert_eq!(round.pages()[0].width_mm, 0.0);
    assert_eq!(round.pages()[0].height_mm, -50.0);

    // Build a SymbolDef containing an Envelope-warped PowerClip(clip_even_odd=true) donut + Clone
    let mut doc2 = Document::new("WarpedSymPC", 200.0, 200.0, ColorMode::Rgb, 72.0);
    let mut outer_kb = kurbo::BezPath::new();
    outer_kb.move_to((0.0, 0.0));
    outer_kb.line_to((40.0, 0.0));
    outer_kb.line_to((40.0, 40.0));
    outer_kb.line_to((0.0, 40.0));
    outer_kb.close_path();
    // Same winding inner hole: relies on clip_even_odd = true to carve hole!
    outer_kb.move_to((12.0, 12.0));
    outer_kb.line_to((28.0, 12.0));
    outer_kb.line_to((28.0, 28.0));
    outer_kb.line_to((12.0, 28.0));
    outer_kb.close_path();
    let donut = BezierPath::from_kurbo(&outer_kb);

    let child_obj = SceneObject::new(
        ObjectId(201),
        "InnerFill",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 40.0, 40.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgba8(220, 40, 40, 255)));

    let mut pc_obj = SceneObject::new(
        ObjectId(202),
        "DonutPC",
        NodePayload::PowerClip {
            container: Box::new(donut),
            container_parametric: None,
            lock_contents: true,
            contents: vec![ObjectId(201)],
        },
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    pc_obj.clip_even_odd = true;

    let mut sym = SymbolDef::new("DonutSym", vec![child_obj, pc_obj]);
    sym.objects[1].effects.push(Effect::Envelope {
        mode: EnvelopeMode::Unconstrained,
        cage: vec![
            Point2D::new(0.0, 0.0),
            Point2D::new(40.0, 0.0),
            Point2D::new(40.0, 40.0),
            Point2D::new(0.0, 40.0),
        ],
    });
    let sym_id = sym.id;
    let mut hist = CommandHistory::new(32);
    hist.execute(&mut doc2, CommandOp::UpsertSymbol { symbol: sym })
        .unwrap();

    let l0 = doc2.pages()[0].layers[0].id;
    let mut inst = SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id))
        .with_transform(Affine2D::translate(50.0, 50.0));
    inst.perspective = Some(
        Perspective3x3::from_rect_to_quad(
            Rect2D::from_xywh(0.0, 0.0, 40.0, 40.0),
            &[
                Point2D::new(0.0, 0.0),
                Point2D::new(44.0, 4.0),
                Point2D::new(42.0, 42.0),
                Point2D::new(2.0, 38.0),
            ],
        )
        .unwrap(),
    );
    let inst_id = doc2.add_object(inst, Some(l0)).unwrap();

    // Clone referencing the SymbolInstance + singular transform + empty MeshFill object
    let clone_obj = SceneObject::new(
        ObjectId(0),
        "SymClone",
        NodePayload::Clone {
            master_id: inst_id,
            link_fill: true,
            link_outline: true,
            link_shape: true,
        },
    )
    .with_transform(Affine2D::translate(60.0, 0.0));
    let clone_id = doc2.add_object(clone_obj, Some(l0)).unwrap();

    let mut deg = SceneObject::new(
        ObjectId(0),
        "DegMesh",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 20.0, 20.0,
        ))),
    )
    .with_transform(Affine2D::new(0.0, 0.0, 0.0, 0.0, 10.0, 10.0))
    .with_fill(Fill::MeshFill {
        rows: 2,
        cols: 2,
        nodes: Vec::new(),
    });
    deg.effects.clear();
    let deg_id = doc2.add_object(deg, Some(l0)).unwrap();

    assert!(doc2
        .world_object_bounds(inst_id)
        .is_some_and(|b| b.is_finite()));
    assert!(doc2
        .world_object_bounds(clone_id)
        .is_some_and(|b| b.is_finite()));
    assert!(doc2
        .world_object_bounds(deg_id)
        .is_some_and(|b| b.is_finite()));

    // Donut hole at center (70, 70) must NOT hit inside clip_even_odd PowerClip, while ring (55, 70) MUST hit
    let hit_ring = coree_core::hit_test_point(&doc2, 0, Point2D::new(55.0, 70.0), 1.0);
    let hit_hole = coree_core::hit_test_point(&doc2, 0, Point2D::new(70.0, 70.0), 0.5);
    assert_eq!(hit_ring, Some(inst_id));
    assert_eq!(hit_hole, None, "clip_even_odd donut hole must not hit");

    // ConvertToCurves on SymbolInstance clone rejects cleanly without panic, and degenerate mesh succeeds cleanly
    assert!(hist
        .execute(&mut doc2, CommandOp::ConvertToCurves { id: clone_id })
        .is_err());
    hist.execute(&mut doc2, CommandOp::ConvertToCurves { id: deg_id })
        .unwrap();
}
