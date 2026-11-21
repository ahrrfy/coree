// SPDX-License-Identifier: MIT OR Apache-2.0

//! End-to-end integration tests exercising all 8 `coree-prod` production modules together.

use coree_core::{
    Affine2D, ArtisticText, BlendMode, Color, ColorMode, CommandHistory, CommandOp, CornerStyle,
    Document, Fill, LensConfig, LensKind, MasterLayerScope, NodePayload, ObjectId, OutlineStyle,
    ParametricShape, Point2D, RasterAsset, RasterLayerData, SceneObject, TextDirection, TextStyle,
};
use coree_prod::{
    add_barcode_to_document, add_qr_code_to_document, apply_print_merge, booklet_signature_order,
    builtin_macros, collect_for_output, compute_production_metrics, compute_separations,
    ean13_check_digit, ean8_check_digit, find_and_replace, find_builtin_macro, find_objects,
    generate_barcode, generate_cut_contour, generate_dieline, generate_print_marks,
    generate_qr_code, impose_document, run_macro, run_preflight, run_recorded_macro,
    simulate_overprint_rgba, upca_check_digit, BarcodeOptions, BarcodeSymbology, CollectOptions,
    ColorMatch, DielineKind, FindQuery, FindScope, ImpositionMode, LensKindFilter,
    MacroSandboxConfig, MacroValue, ObjectKindFilter, PreflightIssueKind, PreflightRules,
    PrintMarksOptions, PrintMergeOptions, PrintMergeTable, ProductionManifest, QrCodeOptions,
    QrErrorCorrection, ReplaceAction, SeparationChannel, SeparationOptions, SerialNumberConfig,
    TextMatchMode,
};
use std::collections::BTreeMap;
use std::fs;

fn make_rect(x: f64, y: f64, w: f64, h: f64, fill: Fill, outline: OutlineStyle) -> SceneObject {
    SceneObject::new(
        ObjectId(0),
        "Rect",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: w,
            height: h,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(fill)
    .with_outline(outline)
    .with_transform(Affine2D::translate(x, y))
}

#[test]
fn end_to_end_print_merge_imposition_marks_preflight_separations_and_collect() {
    // 1. Build an 85x55mm product label template with Arabic + English placeholders + EAN-13 + QR
    let mut template = Document::new("Product Label", 85.0, 55.0, ColorMode::Cmyk, 300.0);
    template.pages[0].bleed_mm = 3.0;

    // Full-bleed CMYK background (-3..-3 to 88..58)
    template
        .add_object(
            make_rect(
                -3.0,
                -3.0,
                91.0,
                61.0,
                Fill::Solid(Color::cmyk(0.05, 0.0, 0.05, 0.0)),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();

    // Accent strip initially in RGB (to verify preflight catches it & macro fixes it!)
    template
        .add_object(
            make_rect(
                5.0,
                5.0,
                75.0,
                8.0,
                Fill::Solid(Color::rgb(0.1, 0.4, 0.8)),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();

    // Bilingual text placeholder
    template
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Label Title",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "<<Product>> - <<Serial>>",
                    TextStyle {
                        font_family: "Noto Sans Arabic".to_owned(),
                        font_size_pt: 12.0,
                        fill: Color::KEY_BLACK,
                        direction: TextDirection::Auto,
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(6.0, 15.0))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();

    // Dynamic EAN-13 barcode placeholder
    template
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Barcode:EAN13:<<EAN>>",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "<<EAN>>",
                    TextStyle::default(),
                )),
            )
            .with_transform(Affine2D::translate(6.0, 26.0))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();

    // Dynamic QR code placeholder
    template
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Barcode:QR:<<URL>>",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "<<URL>>",
                    TextStyle::default(),
                )),
            )
            .with_transform(Affine2D::translate(58.0, 26.0))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();

    // 2. Parse bilingual CSV with quoted commas and run Print Merge with automatic serials
    let csv = concat!(
        "Product,EAN,URL\n",
        "\"زيت زيتون، ممتاز\",400638133393,https://example.com/1\n",
        "\"Coffee, Roast\",590123412345,https://example.com/2\n",
        "\"تمر سكري فاخر\",400638133393,https://example.com/3\n",
        "\"Honey, Raw\",590123412345,https://example.com/4\n",
    );
    let table = PrintMergeTable::from_csv(csv).unwrap();
    assert_eq!(table.row_count(), 4);

    let merge_opts = PrintMergeOptions {
        serial: Some(SerialNumberConfig {
            column_name: "Serial".to_owned(),
            start: 100,
            step: 1,
            pad_width: 4,
            prefix: "SN-".to_owned(),
            suffix: String::new(),
        }),
        qr_options: QrCodeOptions {
            size_mm: 20.0,
            ..QrCodeOptions::default()
        },
        page_name_pattern: "Label <<Serial>>".to_owned(),
        ..PrintMergeOptions::default()
    };

    let mut merged = apply_print_merge(&template, &table, &merge_opts).unwrap();
    assert_eq!(merged.pages.len(), 4);
    assert_eq!(merged.pages[0].name, "Label SN-0100");
    assert_eq!(merged.pages[3].name, "Label SN-0103");

    // 3. Preflight catches the RGB accent strip inside the CMYK document
    let pre_before = run_preflight(&merged, &PreflightRules::default());
    assert!(pre_before.has_issue_kind(PreflightIssueKind::RgbInCmykDocument));

    // 4. Fix RGB -> CMYK using built-in sandboxed Rhai macro `recolor-cmyk`
    let recolor = find_builtin_macro("recolor-cmyk").unwrap();
    let macro_res = run_macro(
        &mut merged,
        recolor.script,
        &BTreeMap::new(),
        &MacroSandboxConfig::default(),
    )
    .unwrap();
    assert!(macro_res.commands_executed >= 1);

    let pre_after = run_preflight(&merged, &PreflightRules::default());
    assert!(!pre_after.has_issue_kind(PreflightIssueKind::RgbInCmykDocument));

    // 5. Impose 4 merged labels 2x2 onto an A4 press sheet
    let mut sheet = impose_document(
        &merged,
        &ImpositionMode::NUp {
            cols: 2,
            rows: 2,
            gap_x_mm: 8.0,
            gap_y_mm: 8.0,
            sheet_w_mm: 210.0,
            sheet_h_mm: 297.0,
        },
    )
    .unwrap();
    assert_eq!(sheet.pages.len(), 1);

    // 6. Add full vector Print Marks to the imposed press sheet
    let marks = generate_print_marks(&mut sheet, 0, &PrintMarksOptions::default()).unwrap();
    assert_eq!(marks.crop_mark_ids.len(), 8);
    assert_eq!(marks.registration_target_ids.len(), 4);
    assert!(marks.page_slug_id.is_some());

    // 7. Compute CMYK separations and simulate overprint preview
    let sep = compute_separations(
        &sheet,
        0,
        &SeparationOptions {
            dpi: 36.0,
            max_dimension_px: 300,
            include_cut_layers: true,
        },
    )
    .unwrap();
    assert!(sep.plates.len() >= 4);
    assert!(sep.plate(&SeparationChannel::Black).unwrap().inked_area_pct > 0.0);
    let preview = simulate_overprint_rgba(&sep);
    assert_eq!(preview.len(), (sep.width * sep.height * 4) as usize);

    // 8. Package complete Collect-for-Output handoff folder
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().join("press_handoff");
    let collect_report = collect_for_output(&sheet, &out_dir, &CollectOptions::default()).unwrap();
    assert!(collect_report.manifest_path.is_file());
    assert!(collect_report
        .document_json_path
        .as_ref()
        .unwrap()
        .is_file());
    assert!(collect_report
        .preflight_txt_path
        .as_ref()
        .unwrap()
        .is_file());
    assert!(collect_report
        .preflight_json_path
        .as_ref()
        .unwrap()
        .is_file());
    assert!(collect_report
        .used_fonts
        .iter()
        .any(|f| f.family == "Noto Sans Arabic"));

    let manifest_json = fs::read_to_string(&collect_report.manifest_path).unwrap();
    let manifest: ProductionManifest = serde_json::from_str(&manifest_json).unwrap();
    assert_eq!(manifest.pages.len(), 1);
    assert!(manifest.production_metrics.total_vector_length_mm > 0.0);
}

#[test]
fn overprint_vs_knockout_and_spot_separations_accuracy() {
    let mut doc = Document::new("Overprint Proof", 80.0, 40.0, ColorMode::Cmyk, 300.0);
    // Yellow base covering full 80x40
    doc.add_object(
        make_rect(
            0.0,
            0.0,
            80.0,
            40.0,
            Fill::Solid(Color::YELLOW),
            OutlineStyle::none(),
        ),
        None,
    )
    .unwrap();
    // Left half [0..40]: Cyan overprinting Yellow (Multiply) => C=1, Y=1 => Green (0, 255, 0)
    let mut left_cyan = make_rect(
        0.0,
        0.0,
        40.0,
        40.0,
        Fill::Solid(Color::CYAN),
        OutlineStyle::none(),
    );
    left_cyan.blend_mode = BlendMode::Multiply;
    doc.add_object(left_cyan, None).unwrap();

    // Right half [40..80]: Cyan knocking out Yellow (Normal) => C=1, Y=0 => Cyan (0, 255, 255)
    doc.add_object(
        make_rect(
            40.0,
            0.0,
            40.0,
            40.0,
            Fill::Solid(Color::CYAN),
            OutlineStyle::none(),
        ),
        None,
    )
    .unwrap();

    // Spot gold rectangle in center
    doc.add_object(
        make_rect(
            30.0,
            10.0,
            20.0,
            20.0,
            Fill::Solid(Color::spot("MetallicGold", 1.0, [0.1, 0.25, 0.7, 0.1])),
            OutlineStyle::none(),
        ),
        None,
    )
    .unwrap();

    let sep = compute_separations(
        &doc,
        0,
        &SeparationOptions {
            dpi: 25.4, // 1px/mm => 80x40
            max_dimension_px: 256,
            include_cut_layers: true,
        },
    )
    .unwrap();

    assert_eq!(sep.width, 80);
    assert_eq!(sep.height, 40);
    assert!(sep.spot_plate("MetallicGold").is_some());

    let rgba = simulate_overprint_rgba(&sep);
    // Sample left overprint pixel (10, 20): Green (0, 255, 0)
    let left_idx = (20 * 80 + 10) * 4;
    assert_eq!(
        (rgba[left_idx], rgba[left_idx + 1], rgba[left_idx + 2]),
        (0, 255, 0)
    );
    // Sample right knockout pixel (70, 20): Cyan (0, 255, 255)
    let right_idx = (20 * 80 + 70) * 4;
    assert_eq!(
        (rgba[right_idx], rgba[right_idx + 1], rgba[right_idx + 2]),
        (0, 255, 255)
    );
}

#[test]
fn booklet_4_6_8_page_signatures_and_tiling_overlap_marks() {
    // Verify 4, 6, 8 page booklet signature sequences
    assert_eq!(booklet_signature_order(4).len(), 2);
    assert_eq!(booklet_signature_order(6).len(), 4);
    assert_eq!(booklet_signature_order(8).len(), 4);
    assert_eq!(booklet_signature_order(8)[0], (Some(7), Some(0)));
    assert_eq!(booklet_signature_order(8)[1], (Some(1), Some(6)));
    assert_eq!(booklet_signature_order(8)[2], (Some(5), Some(2)));
    assert_eq!(booklet_signature_order(8)[3], (Some(3), Some(4)));

    // 8-page booklet imposition
    let mut doc = Document::new("Manual", 148.5, 210.0, ColorMode::Cmyk, 300.0);
    for i in 1..8 {
        doc.add_page(format!("Page {}", i + 1), 148.5, 210.0, 0.0);
    }
    let imposed = impose_document(
        &doc,
        &ImpositionMode::BookletSaddleStitch {
            sheet_w_mm: 297.0,
            sheet_h_mm: 210.0,
        },
    )
    .unwrap();
    assert_eq!(imposed.pages.len(), 4);
    assert!(imposed.pages[0].name.contains("P8 | P1"));
    assert!(imposed.pages[3].name.contains("P4 | P5"));

    // Invalid tiling overlap >= tile width must error cleanly
    assert!(impose_document(
        &doc,
        &ImpositionMode::Tiling {
            tile_w_mm: 100.0,
            tile_h_mm: 100.0,
            overlap_mm: 100.0,
        }
    )
    .is_err());
}

#[test]
fn packaging_dielines_cut_contour_and_path_meter() {
    let mut doc = Document::new("Packaging", 650.0, 450.0, ColorMode::Cmyk, 300.0);

    // TuckEndBox
    let tuck = generate_dieline(
        &mut doc,
        0,
        &DielineKind::TuckEndBox {
            length_mm: 80.0,
            width_mm: 40.0,
            height_mm: 120.0,
            glue_flap_mm: 15.0,
            tuck_flap_mm: 20.0,
        },
        Point2D::new(20.0, 20.0),
    )
    .unwrap();
    assert!((tuck.flat_bounds.width() - (15.0 + 160.0 + 80.0)).abs() < 1e-5);
    assert!((tuck.flat_bounds.height() - (120.0 + 80.0 + 40.0)).abs() < 1e-5);

    // Compute metrics on the generated dieline
    let metrics = compute_production_metrics(&doc, None);
    assert!((metrics.cut_length_mm - tuck.cut_length_mm).abs() < 1e-4);
    assert!((metrics.crease_length_mm - tuck.crease_length_mm).abs() < 1e-4);
    assert!(metrics.cut_length_m > 0.5);
    assert!(metrics.crease_length_m > 0.5);
    assert!(metrics.material_utilization_pct > 40.0);

    // CutContour around two overlapping shapes
    let mut sticker_doc = Document::new_a4("Decal");
    let s1 = sticker_doc
        .add_object(
            make_rect(
                20.0,
                20.0,
                30.0,
                30.0,
                Fill::Solid(Color::CYAN),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();
    let s2 = sticker_doc
        .add_object(
            make_rect(
                40.0,
                20.0,
                30.0,
                30.0,
                Fill::Solid(Color::YELLOW),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();
    let cut = generate_cut_contour(&mut sticker_doc, &[s1, s2], 3.0, "CutContour").unwrap();
    let cut_bb = sticker_doc.object_bounds(cut.object_id).unwrap();
    // Union 50x30 + 3mm offset on all sides => ~56x36
    assert!((cut_bb.width() - 56.0).abs() < 0.3);
    assert!((cut_bb.height() - 36.0).abs() < 0.3);
}

#[test]
fn barcodes_and_qr_all_symbologies() {
    let qr = generate_qr_code(
        "https://coree.example/verify?id=42",
        &QrCodeOptions {
            size_mm: 24.0,
            quiet_zone_modules: 4,
            ec_level: QrErrorCorrection::Q,
            weld_modules: true,
            ..QrCodeOptions::default()
        },
    )
    .unwrap();
    assert!(!qr.dark_modules_path.is_empty());
    assert!((qr.bounds.width() - 24.0).abs() < 1e-9);

    assert_eq!(ean13_check_digit("590123412345").unwrap(), 7);
    assert_eq!(ean8_check_digit("5512345").unwrap(), 7);
    assert_eq!(upca_check_digit("04210000526").unwrap(), 4);

    let opts = BarcodeOptions::default();
    for (sym, payload) in [
        (BarcodeSymbology::Ean13, "5901234123457"),
        (BarcodeSymbology::Ean8, "55123457"),
        (BarcodeSymbology::UpcA, "042100005264"),
        (BarcodeSymbology::Code128, "Batch#0099-OK"),
        (BarcodeSymbology::Code39, "PROD-42"),
    ] {
        let out = generate_barcode(sym, payload, &opts).unwrap();
        assert!(!out.bars_path.is_empty(), "empty bars for {sym:?}");
        assert!(out.bounds.width() > 10.0);
        assert!(out.human_readable_text.is_some());
    }

    let mut doc = Document::new_a4("Barcodes");
    let b_id = add_barcode_to_document(
        &mut doc,
        BarcodeSymbology::UpcA,
        "04210000526",
        Point2D::new(10.0, 10.0),
        &opts,
        None,
    )
    .unwrap();
    let q_id = add_qr_code_to_document(
        &mut doc,
        "Hello QR",
        Point2D::new(80.0, 10.0),
        &QrCodeOptions::default(),
        None,
    )
    .unwrap();
    assert!(doc.objects.contains_key(&b_id));
    assert!(doc.objects.contains_key(&q_id));
}

#[test]
fn find_replace_scope_locked_protection_and_preflight_scaled_dpi() {
    let mut doc = Document::new_a4("Inspect");
    doc.add_page("Page 2", 210.0, 297.0, 3.0);
    let p1_layer = doc.pages[0].layers[0].id;
    let p2_layer = doc.pages[1].layers[0].id;

    // 300x300 px asset placed at 25.4x25.4 mm, then scaled 2x -> effective 150 DPI (< 300 DPI!)
    let asset =
        RasterAsset::new(300, 300, vec![255u8; 300 * 300 * 4], 300.0, ColorMode::Cmyk).unwrap();
    let aid = doc.register_asset(asset);
    let scaled_img = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Scaled Image",
                NodePayload::Raster(RasterLayerData {
                    asset_id: aid,
                    width_mm: 25.4,
                    height_mm: 25.4,
                    filters: Vec::new(),
                    crop_rect: None,
                }),
            )
            .with_transform(Affine2D::translate(30.0, 30.0).compose(&Affine2D::scale(2.0, 2.0)))
            .with_outline(OutlineStyle::none()),
            Some(p1_layer),
        )
        .unwrap();

    let report = run_preflight(&doc, &PreflightRules::default());
    let low_dpi_issue = report
        .issues
        .iter()
        .find(|i| {
            i.kind == PreflightIssueKind::LowResolutionImage && i.object_id == Some(scaled_img)
        })
        .expect("scaled image should report ~150 DPI");
    assert!((low_dpi_issue.measured_value.unwrap() - 150.0).abs() < 1.0);

    // Add locked object on page 2 with HeatMap lens
    let mut locked_obj = make_rect(
        10.0,
        10.0,
        20.0,
        20.0,
        Fill::Solid(Color::CYAN),
        OutlineStyle::solid(Color::KEY_BLACK, 0.3),
    );
    locked_obj.locked = true;
    locked_obj.lens = Some(LensConfig::new(LensKind::HeatMap {
        palette_rotation_deg: 0.0,
    }));
    let locked_id = doc.add_object(locked_obj, Some(p2_layer)).unwrap();

    // Find on CurrentPage (page 0) does NOT see page 2's lens object
    let q_curr = FindQuery {
        scope: FindScope::CurrentPage,
        lens_kind: Some(LensKindFilter::HeatMap),
        ..FindQuery::default()
    };
    assert!(find_objects(&doc, &q_curr).is_empty());

    // Find on WholeDocument DOES find it, but replace respects `locked == true`
    let q_all = FindQuery {
        scope: FindScope::WholeDocument,
        object_kind: Some(ObjectKindFilter::Shape),
        fill_color: Some(ColorMatch::exact(Color::CYAN)),
        lens_kind: Some(LensKindFilter::HeatMap),
        ..FindQuery::default()
    };
    let rep = find_and_replace(
        &mut doc,
        &q_all,
        &ReplaceAction::ReplaceFillColor {
            color: Color::MAGENTA,
        },
    );
    assert_eq!(rep.matched_ids, vec![locked_id]);
    assert!(rep.modified_ids.is_empty());
    assert_eq!(doc.objects[&locked_id].fill, Fill::Solid(Color::CYAN));

    // Verify wildcard/regex text matching
    let t = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "SKU",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "SKU-90210",
                    TextStyle::default(),
                )),
            ),
            Some(p1_layer),
        )
        .unwrap();
    let q_rx = FindQuery {
        scope: FindScope::WholeDocument,
        text_pattern: Some((
            r"^SKU-\d+$".to_owned(),
            TextMatchMode::RegexLike {
                case_sensitive: true,
            },
        )),
        ..FindQuery::default()
    };
    assert_eq!(find_objects(&doc, &q_rx), vec![t]);
}

#[test]
fn sandboxed_rhai_macro_library_and_recorded_replay() {
    assert!(builtin_macros().len() >= 5);

    let mut doc = Document::new_a4("Macro Replay");
    let mut hist = CommandHistory::default();
    let obj_id = doc
        .add_object(
            make_rect(
                10.0,
                10.0,
                20.0,
                15.0,
                Color::CYAN.into_fill(),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();

    hist.start_macro_recording();
    hist.execute(
        &mut doc,
        CommandOp::TransformObjects {
            ids: vec![obj_id],
            affine: Affine2D::translate(5.0, 10.0),
        },
    )
    .unwrap();
    hist.execute(&mut doc, CommandOp::ConvertToCurves { id: obj_id })
        .unwrap();
    let script = hist.stop_macro_to_rhai();

    // Replay into a fresh document via run_recorded_macro and test Undo!
    let mut target = Document::new_a4("Fresh");
    let mut fresh_obj = make_rect(
        10.0,
        10.0,
        20.0,
        15.0,
        Fill::Solid(Color::CYAN),
        OutlineStyle::none(),
    );
    fresh_obj.id = obj_id;
    target.add_object(fresh_obj, None).unwrap();
    let mut target_hist = CommandHistory::default();

    let exec = run_recorded_macro(
        &mut target,
        &mut target_hist,
        &script,
        &MacroSandboxConfig::default(),
    )
    .unwrap();
    assert_eq!(exec.commands_executed, 2);
    assert!(matches!(
        target.objects[&obj_id].payload,
        NodePayload::Path(_)
    ));

    // Single undo reverts the entire macro transaction cleanly
    target_hist.undo(&mut target).unwrap();
    assert!(matches!(
        target.objects[&obj_id].payload,
        NodePayload::Shape(_)
    ));

    // Sandbox op-limit stops infinite loop immediately
    let err = run_macro(
        &mut target,
        "let i = 0; while true { i += 1; }",
        &BTreeMap::new(),
        &MacroSandboxConfig {
            max_operations: 2_000,
            ..MacroSandboxConfig::default()
        },
    );
    assert!(err.is_err());

    // Custom parameterized script with MacroValue scope vars
    let mut vars = BTreeMap::new();
    vars.insert("dx".to_owned(), MacroValue::Float(12.5));
    vars.insert("dy".to_owned(), MacroValue::Float(7.5));
    let before_bb = target.object_bounds(obj_id).unwrap();
    run_macro(
        &mut target,
        "translate_objects(all_object_ids(), dx, dy);",
        &vars,
        &MacroSandboxConfig::default(),
    )
    .unwrap();
    let after_bb = target.object_bounds(obj_id).unwrap();
    assert!((after_bb.min_x - (before_bb.min_x + 12.5)).abs() < 1e-6);
    assert!((after_bb.min_y - (before_bb.min_y + 7.5)).abs() < 1e-6);

    // Extra layer cleanup test
    let p_id = target.pages[0].id;
    target
        .add_layer(p_id, "Temp Empty", false, MasterLayerScope::None)
        .unwrap();
    assert_eq!(target.pages[0].layers.len(), 2);
    run_macro(
        &mut target,
        find_builtin_macro("clean-empty-layers").unwrap().script,
        &BTreeMap::new(),
        &MacroSandboxConfig::default(),
    )
    .unwrap();
    assert_eq!(target.pages[0].layers.len(), 1);
}

trait IntoFillExt {
    fn into_fill(self) -> Fill;
}

impl IntoFillExt for Color {
    fn into_fill(self) -> Fill {
        Fill::Solid(self)
    }
}

// =============================================================================
// Regression Tests for Verifier Audit (#19)
// =============================================================================

#[test]
fn regression_1_utf8_multibyte_object_names_never_panic_in_print_merge() {
    let mut tpl = Document::new_a4("UTF-8 Names");
    // Multibyte Arabic & emoji names whose UTF-8 sequences cross byte index 8 ("barcode:".len())
    for name in ["1. مرحباً", "Aأحمد علي", "🔥📦🎯✨", "صورة_المنتج"]
    {
        tpl.add_object(
            SceneObject::new(
                ObjectId(0),
                name,
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "مرحباً <<Name>>",
                    TextStyle::default(),
                )),
            ),
            None,
        )
        .unwrap();
    }
    let table = PrintMergeTable::from_csv("Name\nأحمد\nسارة\n").unwrap();
    let merged = apply_print_merge(&tpl, &table, &PrintMergeOptions::default()).unwrap();
    assert_eq!(merged.pages.len(), 2);
}

#[test]
fn regression_2_multibyte_utf8_barcode_inputs_error_without_panicking() {
    let opts = BarcodeOptions::default();
    // Exact byte lengths matching 13 (EAN-13), 8 (EAN-8), and 12 (UPC-A) with multibyte chars
    let utf8_13 = "12345مرحبا"; // 5 ASCII + 4*2? Let's construct exact 13, 8, 12 byte strings!
    assert_eq!("123مرحبا".len(), 13);
    assert_eq!("12مرح".len(), 8);
    assert_eq!("12مرحبا".len(), 12);

    assert!(generate_barcode(BarcodeSymbology::Ean13, "123مرحبا", &opts).is_err());
    assert!(generate_barcode(BarcodeSymbology::Ean13, utf8_13, &opts).is_err());
    assert!(generate_barcode(BarcodeSymbology::Ean8, "12مرح", &opts).is_err());
    assert!(generate_barcode(BarcodeSymbology::UpcA, "12مرحبا", &opts).is_err());
}

#[test]
fn regression_3_oom_and_overflow_caps_reject_pathological_inputs_cleanly() {
    // 3a. Regex-like wide Unicode range [\u{0}-\u{10FFFF}] stays O(1) memory and works
    let mut doc = Document::new_a4("Regex Range");
    let tid = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Arabic",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "مرحباً 2026",
                    TextStyle::default(),
                )),
            ),
            None,
        )
        .unwrap();
    let hits = find_objects(
        &doc,
        &FindQuery {
            scope: FindScope::WholeDocument,
            text_pattern: Some((
                "[\u{0}-\u{10FFFF}]+".to_owned(),
                TextMatchMode::RegexLike {
                    case_sensitive: true,
                },
            )),
            ..FindQuery::default()
        },
    );
    assert_eq!(hits, vec![tid]);

    // 3b. QR quiet zone cap, size cap, and huge valid size without i64 overflow
    assert!(generate_qr_code(
        "hi",
        &QrCodeOptions {
            quiet_zone_modules: 1_000_000,
            ..QrCodeOptions::default()
        }
    )
    .is_err());
    assert!(generate_qr_code(
        "hi",
        &QrCodeOptions {
            size_mm: 1e12,
            ..QrCodeOptions::default()
        }
    )
    .is_err());
    let large_qr = generate_qr_code(
        "OK",
        &QrCodeOptions {
            size_mm: 50_000.0,
            weld_modules: true,
            ..QrCodeOptions::default()
        },
    )
    .unwrap();
    assert!(!large_qr.dark_modules_path.is_empty());

    // 3c. StickerSheet huge grid cap
    assert!(generate_dieline(
        &mut doc,
        0,
        &DielineKind::StickerSheet {
            label_w_mm: 10.0,
            label_h_mm: 10.0,
            corner_r_mm: 0.0,
            cols: 500,
            rows: 500,
            gap_mm: 1.0,
        },
        Point2D::ZERO
    )
    .is_err());

    // 3d. Imposition N-Up & Tiling caps
    assert!(impose_document(
        &doc,
        &ImpositionMode::NUp {
            cols: 200,
            rows: 200,
            gap_x_mm: 0.0,
            gap_y_mm: 0.0,
            sheet_w_mm: 210.0,
            sheet_h_mm: 297.0,
        }
    )
    .is_err());
    assert!(impose_document(
        &doc,
        &ImpositionMode::Tiling {
            tile_w_mm: 0.01,
            tile_h_mm: 0.01,
            overlap_mm: 0.0,
        }
    )
    .is_err());
}

#[test]
fn regression_4_gradient_separations_respect_object_world_transform() {
    use coree_core::GradientStop;

    let mut doc = Document::new("Grad Sep", 100.0, 100.0, ColorMode::Cmyk, 300.0);
    // 40x40 rect at local [0..40, 0..40] with linear gradient Cyan (x=0) -> Yellow (x=40),
    // translated by (50, 10) -> world x in [50..90], y in [10..50].
    let grad_rect = make_rect(
        50.0,
        10.0,
        40.0,
        40.0,
        Fill::LinearGradient {
            start: Point2D::new(0.0, 0.0),
            end: Point2D::new(40.0, 0.0),
            stops: vec![
                GradientStop::new(0.0, Color::CYAN),
                GradientStop::new(1.0, Color::YELLOW),
            ],
        },
        OutlineStyle::none(),
    );
    doc.add_object(grad_rect, None).unwrap();

    // Radial gradient Magenta (center) -> Yellow (edge r=15) translated to (20, 70) -> center at (20, 70)
    let rad_rect = make_rect(
        5.0,
        55.0,
        30.0,
        30.0,
        Fill::RadialGradient {
            center: Point2D::new(15.0, 15.0),
            radius: 15.0,
            focal: Point2D::new(15.0, 15.0),
            stops: vec![
                GradientStop::new(0.0, Color::MAGENTA),
                GradientStop::new(1.0, Color::YELLOW),
            ],
        },
        OutlineStyle::none(),
    );
    doc.add_object(rad_rect, None).unwrap();

    let sep = compute_separations(
        &doc,
        0,
        &SeparationOptions {
            dpi: 25.4, // 1px = 1mm => 100x100
            max_dimension_px: 256,
            include_cut_layers: true,
        },
    )
    .unwrap();

    let c = sep.plate(&SeparationChannel::Cyan).unwrap();
    let m = sep.plate(&SeparationChannel::Magenta).unwrap();
    let y = sep.plate(&SeparationChannel::Yellow).unwrap();

    // Linear gradient near translated left edge (x=51, y=30): almost pure Cyan, almost zero Yellow
    let left_px = 30 * 100 + 51;
    assert!(
        c.ink_map[left_px] > 230,
        "left C was {}",
        c.ink_map[left_px]
    );
    assert!(y.ink_map[left_px] < 25, "left Y was {}", y.ink_map[left_px]);

    // Linear gradient near translated right edge (x=88, y=30): almost zero Cyan, almost pure Yellow
    let right_px = 30 * 100 + 88;
    assert!(
        c.ink_map[right_px] < 25,
        "right C was {}",
        c.ink_map[right_px]
    );
    assert!(
        y.ink_map[right_px] > 230,
        "right Y was {}",
        y.ink_map[right_px]
    );

    // Radial gradient at translated center (x=20, y=70): pure Magenta!
    let rad_center = 70 * 100 + 20;
    assert!(
        m.ink_map[rad_center] > 235,
        "center M was {}",
        m.ink_map[rad_center]
    );
    assert!(
        y.ink_map[rad_center] < 20,
        "center Y was {}",
        y.ink_map[rad_center]
    );
}

#[test]
fn regression_5_group_transform_and_powerclip_bleed_and_metrics() {
    let mut doc = Document::new_a4("Group & PowerClip");
    doc.pages[0].bleed_mm = 3.0;

    // 5a. Child shape at local (0, 0) (would look like touching trim if parent transform were ignored!),
    // grouped and translated safely into the interior (50, 50) -> MUST NOT trigger bleed warning!
    let child_safe = doc
        .add_object(
            make_rect(
                0.0,
                0.0,
                30.0,
                30.0,
                Fill::Solid(Color::CYAN),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();
    let safe_grp = doc.group_objects(&[child_safe], "Safe Group").unwrap();
    doc.objects.get_mut(&safe_grp).unwrap().transform = Affine2D::translate(50.0, 50.0);

    // 5b. Child shape at local (50, 50) translated by group to (-1, 50) -> touches trim without full 3mm bleed -> MUST flag!
    let child_bleed = doc
        .add_object(
            make_rect(
                50.0,
                50.0,
                30.0,
                30.0,
                Fill::Solid(Color::YELLOW),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();
    let bleed_grp = doc.group_objects(&[child_bleed], "Bleed Group").unwrap();
    doc.objects.get_mut(&bleed_grp).unwrap().transform = Affine2D::translate(-51.0, 0.0);

    // 5c. PowerClip container safely inside [90..150, 90..150] clipping an oversized child [-1..200, 90..150]
    // -> clipped child MUST NOT falsely trigger bleed warning!
    let pc_container = doc
        .add_object(
            make_rect(
                90.0,
                90.0,
                60.0,
                60.0,
                Fill::None,
                OutlineStyle::solid(Color::KEY_BLACK, 0.3),
            ),
            None,
        )
        .unwrap();
    let pc_child = doc
        .add_object(
            make_rect(
                -1.0,
                90.0,
                200.0,
                60.0,
                Fill::Solid(Color::CYAN),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();
    let _pc = doc.create_powerclip(pc_container, &[pc_child]).unwrap();

    let report = run_preflight(&doc, &PreflightRules::default());
    assert!(
        !report
            .issues
            .iter()
            .any(|i| i.kind == PreflightIssueKind::InsufficientBleedCoverage
                && i.object_id == Some(child_safe)),
        "interior grouped child must not trigger bleed warning"
    );
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.kind == PreflightIssueKind::InsufficientBleedCoverage
                && i.object_id == Some(child_bleed)),
        "grouped child shifted across trim without bleed must trigger bleed warning"
    );
    assert!(
        !report
            .issues
            .iter()
            .any(|i| i.kind == PreflightIssueKind::InsufficientBleedCoverage
                && i.object_id == Some(pc_child)),
        "PowerClipped child outside container must not trigger false bleed warning"
    );

    // 5d. Grouped non-path object (ArtisticText) bounds reflect group transform in production metrics
    let mut metrics_doc = Document::new_a4("Grouped Text Metrics");
    let t = metrics_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Label",
                NodePayload::ArtisticText(ArtisticText::from_plain("ABC", TextStyle::default())),
            ),
            None,
        )
        .unwrap();
    let tg = metrics_doc.group_objects(&[t], "TextGrp").unwrap();
    metrics_doc.objects.get_mut(&tg).unwrap().transform = Affine2D::translate(80.0, 90.0);

    let m = compute_production_metrics(&metrics_doc, None);
    let bb = m.bounding_box.expect("metrics bbox");
    assert!((bb.min_x - 80.0).abs() < 1e-4, "min_x was {}", bb.min_x);
    assert!((bb.min_y - 90.0).abs() < 1e-4, "min_y was {}", bb.min_y);
}

#[test]
fn regression_6_exact_color_match_never_cross_matches_spot_and_process_cmyk() {
    let mut doc = Document::new_a4("Spot vs Process");
    let process_mag = doc
        .add_object(
            make_rect(
                10.0,
                10.0,
                30.0,
                30.0,
                Fill::Solid(Color::MAGENTA),
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();
    let spot_cut = doc
        .add_object(
            make_rect(
                50.0,
                10.0,
                30.0,
                30.0,
                Fill::Solid(Color::cut_contour_spot()), // fallback CMYK = [0, 1, 0, 0] == MAGENTA!
                OutlineStyle::none(),
            ),
            None,
        )
        .unwrap();

    // Exact match for CutContour must ONLY hit spot_cut, NOT process_mag
    let q_spot = FindQuery {
        scope: FindScope::WholeDocument,
        fill_color: Some(ColorMatch::exact(Color::cut_contour_spot())),
        ..FindQuery::default()
    };
    assert_eq!(find_objects(&doc, &q_spot), vec![spot_cut]);

    // Exact match for process MAGENTA must ONLY hit process_mag, NOT spot_cut
    let q_proc = FindQuery {
        scope: FindScope::WholeDocument,
        fill_color: Some(ColorMatch::exact(Color::MAGENTA)),
        ..FindQuery::default()
    };
    assert_eq!(find_objects(&doc, &q_proc), vec![process_mag]);
}

#[test]
fn regression_7_collect_for_output_font_collisions_tables_dimensions_and_spot_inks() {
    use coree_core::{DimensionKind, DimensionObject, VectorTable};

    let tmp = tempfile::tempdir().unwrap();
    // Two distinct font files in different folders whose sanitized names both collide to "Brand_Font.ttf"
    let dir_a = tmp.path().join("vault_a");
    let dir_b = tmp.path().join("vault_b");
    fs::create_dir_all(&dir_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();
    fs::write(dir_a.join("Alpha Font.ttf"), b"FONT_A_BYTES").unwrap();
    fs::write(dir_b.join("Alpha_Font.ttf"), b"FONT_B_BYTES").unwrap();
    // Actually test distinct normalized stems that sanitize to the SAME filename:
    // e.g. "Foo!Bar.ttf" -> key "foobar", sanitized "Foo_Bar.ttf"
    // and  "Foo@Bar.ttf" -> wait, both normalize to "foobar"; use two distinct keys with same sanitized name:
    // "Sans+One.ttf" -> key "sansone" -> sanitized "Sans_One.ttf" vs
    // "Sans_One.ttf" -> wait, normalize strips non-alphanumeric so both are "sansone".
    // Better: "MyFont!.ttf" in dir_a vs "MyFont?.ttf" won't collide in index if keys differ:
    // "FontA.ttf" in dir_a vs wait: sanitize replaces non-alnum with '_':
    // dir_a/"Noto+Sans.ttf" (normalized "notosans") vs dir_b/"Noto Sans.ttf" (same normalized key).
    // Two DIFFERENT normalized keys that sanitize to the same filename:
    // Actually sanitize preserves ASCII alnum/'-'/'_' and maps other chars to '_' (including Arabic/UTF-8!).
    // So dir_a/"font_1.ttf" (key "font1") vs... even better: UTF-8 stems like "العربية.ttf" (fallback "font.ttf")
    // and "نسخ.ttf" (fallback "font.ttf") OR "A_B.ttf" and "A B.ttf" when matched via different family names!
    // Let's create two distinct files that sanitize to "font.ttf": wait - normalize_font_key strips non-ASCII,
    // so let's create two distinct ASCII files whose filenames collide after 64-char truncation!
    let long_prefix = "A".repeat(64);
    let file_a = format!("{long_prefix}One.ttf");
    let file_b = format!("{long_prefix}Two.ttf");
    fs::write(dir_a.join(&file_a), b"FONT_ONE_PAYLOAD").unwrap();
    fs::write(dir_b.join(&file_b), b"FONT_TWO_PAYLOAD").unwrap();

    let mut doc = Document::new_a4("Complete Collect");
    // Text using font A + Spot text fill
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "TextA",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "Hello",
                TextStyle {
                    font_family: format!("{long_prefix}One"),
                    fill: Color::spot("SpotGold", 1.0, [0.1, 0.2, 0.8, 0.0]),
                    ..TextStyle::default()
                },
            )),
        ),
        None,
    )
    .unwrap();

    // Table with font B + cell Spot fill + border Spot ink
    let mut tbl = VectorTable::new(1, 1, 40.0, 12.0);
    tbl.border_style =
        OutlineStyle::solid(Color::spot("SpotBorder", 1.0, [1.0, 0.0, 0.0, 0.0]), 0.3);
    tbl.cells[0].text = "Spec".to_owned();
    tbl.cells[0].text_style.font_family = format!("{long_prefix}Two");
    tbl.cells[0].fill = Fill::Solid(Color::spot("SpotCell", 1.0, [0.0, 1.0, 0.0, 0.0]));
    doc.add_object(
        SceneObject::new(ObjectId(0), "SpecTable", NodePayload::Table(tbl)),
        None,
    )
    .unwrap();

    // Dimension object with its own font family & Spot text fill
    let mut dim = DimensionObject::linear(
        DimensionKind::Horizontal,
        Point2D::new(0.0, 0.0),
        Point2D::new(50.0, 0.0),
        6.0,
    );
    dim.text_style.font_family = "TechMono".to_owned();
    dim.text_style.fill = Color::spot("SpotDim", 1.0, [0.0, 0.0, 1.0, 0.0]);
    doc.add_object(
        SceneObject::new(ObjectId(0), "Dim1", NodePayload::Dimension(dim)),
        None,
    )
    .unwrap();

    let bundle = tmp.path().join("bundle");
    let opts = CollectOptions {
        extra_font_search_dirs: vec![dir_a, dir_b],
        ..CollectOptions::default()
    };
    let report = collect_for_output(&doc, &bundle, &opts).unwrap();

    // Both colliding font files must be copied to DISTINCT paths without overwriting each other!
    let f1 = report
        .used_fonts
        .iter()
        .find(|f| f.family == format!("{long_prefix}One"))
        .unwrap();
    let f2 = report
        .used_fonts
        .iter()
        .find(|f| f.family == format!("{long_prefix}Two"))
        .unwrap();
    let p1 = f1.copied_relative_path.as_ref().expect("font 1 copied");
    let p2 = f2.copied_relative_path.as_ref().expect("font 2 copied");
    assert_ne!(
        p1, p2,
        "colliding sanitized font filenames must be disambiguated"
    );
    assert_eq!(fs::read(bundle.join(p1)).unwrap(), b"FONT_ONE_PAYLOAD");
    assert_eq!(fs::read(bundle.join(p2)).unwrap(), b"FONT_TWO_PAYLOAD");

    // Table & Dimension fonts and all 4 Spot inks must be captured!
    assert!(report.used_fonts.iter().any(|f| f.family == "TechMono"));
    for spot in ["SpotGold", "SpotBorder", "SpotCell", "SpotDim"] {
        assert!(
            report.spot_inks.contains(&spot.to_owned()),
            "missing spot ink {spot} in {:?}",
            report.spot_inks
        );
    }
}

#[test]
fn regression_8_macro_nan_validation_and_locked_layer_preservation() {
    let mut doc = Document::new_a4("Macro Hardening");
    let pid = doc.pages[0].id;
    let oid = doc
        .add_object(
            make_rect(
                10.0,
                10.0,
                20.0,
                20.0,
                Fill::Solid(Color::CYAN),
                OutlineStyle::solid(Color::KEY_BLACK, 0.3),
            ),
            None,
        )
        .unwrap();
    let locked_empty = doc
        .add_layer(pid, "Locked Empty", false, MasterLayerScope::None)
        .unwrap();
    doc.set_layer_flags(locked_empty, None, Some(true), None, None)
        .unwrap();
    let unlocked_empty = doc
        .add_layer(pid, "Unlocked Empty", false, MasterLayerScope::None)
        .unwrap();

    let cfg = MacroSandboxConfig::default();

    // NaN / negative gap in align_grid fails cleanly without mutating doc
    let snap = doc.clone();
    assert!(run_macro(
        &mut doc,
        "align_grid(all_object_ids(), 2, 0.0/0.0, 5.0, 10.0, 10.0);",
        &BTreeMap::new(),
        &cfg
    )
    .is_err());
    assert!(run_macro(
        &mut doc,
        "align_grid(all_object_ids(), 2, -5.0, 5.0, 10.0, 10.0);",
        &BTreeMap::new(),
        &cfg
    )
    .is_err());
    // NaN / negative width in set_outline_mm fails cleanly
    assert!(run_macro(
        &mut doc,
        "set_outline_mm(all_object_ids(), 0.0/0.0);",
        &BTreeMap::new(),
        &cfg
    )
    .is_err());
    assert!(run_macro(
        &mut doc,
        "set_outline_mm(all_object_ids(), -1.0);",
        &BTreeMap::new(),
        &cfg
    )
    .is_err());
    // NaN CMYK fill fails cleanly
    assert!(run_macro(
        &mut doc,
        "set_cmyk_fill(all_object_ids(), 0.0/0.0, 0.0, 0.0, 0.0);",
        &BTreeMap::new(),
        &cfg
    )
    .is_err());
    assert_eq!(doc, snap);
    assert!(doc.objects.contains_key(&oid));

    // clean_empty_layers removes unlocked empty layer while preserving locked empty layer!
    run_macro(
        &mut doc,
        find_builtin_macro("clean-empty-layers").unwrap().script,
        &BTreeMap::new(),
        &cfg,
    )
    .unwrap();
    assert!(
        doc.find_layer(locked_empty).is_some(),
        "locked empty layer must stay"
    );
    assert!(
        doc.find_layer(unlocked_empty).is_none(),
        "unlocked empty layer must be removed"
    );
}

#[test]
fn regression_9_print_merge_grouped_barcodes_and_empty_resolved_guard() {
    let mut tpl = Document::new_a4("Grouped Barcode Merge");

    // Create a barcode placeholder and an empty-resolving barcode placeholder inside a translated Group
    let label = tpl
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Item Name",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "<<Item>>",
                    TextStyle::default(),
                )),
            )
            .with_transform(Affine2D::translate(0.0, 0.0)),
            None,
        )
        .unwrap();
    let bc_ph = tpl
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Barcode:Code128:<<SKU>>",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "<<SKU>>",
                    TextStyle::default(),
                )),
            )
            .with_transform(Affine2D::translate(0.0, 15.0)),
            None,
        )
        .unwrap();
    let opt_bc_ph = tpl
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Barcode:Code128",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "<<OptCode>>",
                    TextStyle::default(),
                )),
            )
            .with_transform(Affine2D::translate(0.0, 45.0)),
            None,
        )
        .unwrap();

    let grp = tpl
        .group_objects(&[label, bc_ph, opt_bc_ph], "Product Card")
        .unwrap();
    tpl.objects.get_mut(&grp).unwrap().transform = Affine2D::translate(30.0, 40.0);

    // Row 0 has empty OptCode -> must NOT fail! Row 1 has populated OptCode -> regenerates barcode!
    let csv = "Item,SKU,OptCode\nWidget A,SKU-001,\nWidget B,SKU-002,OPT-99\n";
    let table = PrintMergeTable::from_csv(csv).unwrap();
    let merged = apply_print_merge(&tpl, &table, &PrintMergeOptions::default()).unwrap();
    assert_eq!(merged.pages.len(), 2);

    // Page 0: verify Product Card group still holds 3 children, child[1] is replaced with vector barcode group,
    // and its world bounds reflect the parent group's translate(30, 40) + child local (0, 15) = (30, 55)!
    let p0_grp_id = merged.pages[0].layers[0].objects[0];
    let NodePayload::Group(ref p0_children) = merged.objects[&p0_grp_id].payload else {
        panic!("expected Product Card group on page 0");
    };
    assert_eq!(p0_children.len(), 3);
    assert_eq!(
        merged.objects[&p0_children[1]].name,
        "Barcode:Code128:SKU-001"
    );
    assert!(matches!(
        merged.objects[&p0_children[1]].payload,
        NodePayload::Group(_)
    ));
    // Verify parent group preserved its translation
    let grp_bb = merged.object_bounds(p0_grp_id).unwrap();
    assert!(
        (grp_bb.min_x - 30.0).abs() < 0.5,
        "group min_x was {}",
        grp_bb.min_x
    );
    assert!(
        (grp_bb.min_y - 40.0).abs() < 0.5,
        "group min_y was {}",
        grp_bb.min_y
    );

    // Page 1: OptCode WAS populated so child[2] also became a regenerated vector barcode group!
    let p1_grp_id = merged.pages[1].layers[0].objects[0];
    let NodePayload::Group(ref p1_children) = merged.objects[&p1_grp_id].payload else {
        panic!("expected Product Card group on page 1");
    };
    assert_eq!(
        merged.objects[&p1_children[2]].name,
        "Barcode:Code128:OPT-99"
    );
    assert!(matches!(
        merged.objects[&p1_children[2]].payload,
        NodePayload::Group(_)
    ));
}
