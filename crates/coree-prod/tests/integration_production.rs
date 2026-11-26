// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! End-to-end integration tests exercising all 8 `coree-prod` production modules together.

use coree_core::{
    Affine2D, AnchorPoint, ArtisticText, BezierPath, BlendMode, Color, ColorMode, CommandHistory,
    CommandOp, ConnectorObject, ConnectorStyle, CornerStyle, Document, Fill, LensConfig, LensKind,
    MasterLayerScope, NodePayload, ObjectId, OutlineStyle, ParametricShape, Point2D, RasterAsset,
    RasterLayerData, Rect2D, SceneObject, SymbolDef, TextDirection, TextOnPath, TextStyle,
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
    template.set_page_bleed(0, 3.0).unwrap();

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
    assert_eq!(merged.pages().len(), 4);
    assert_eq!(merged.pages()[0].name, "Label SN-0100");
    assert_eq!(merged.pages()[3].name, "Label SN-0103");

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
    assert_eq!(sheet.pages().len(), 1);

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
    assert_eq!(imposed.pages().len(), 4);
    assert!(imposed.pages()[0].name.contains("P8 | P1"));
    assert!(imposed.pages()[3].name.contains("P4 | P5"));

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
    assert!(doc.has_object(b_id));
    assert!(doc.has_object(q_id));
}

#[test]
fn find_replace_scope_locked_protection_and_preflight_scaled_dpi() {
    let mut doc = Document::new_a4("Inspect");
    doc.add_page("Page 2", 210.0, 297.0, 3.0);
    let p1_layer = doc.pages()[0].layers[0].id;
    let p2_layer = doc.pages()[1].layers[0].id;

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
                    filter_enabled: Vec::new(),
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
    assert_eq!(doc.objects()[&locked_id].fill, Fill::Solid(Color::CYAN));

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
        target.objects()[&obj_id].payload,
        NodePayload::Path(_)
    ));

    // Single undo reverts the entire macro transaction cleanly
    target_hist.undo(&mut target).unwrap();
    assert!(matches!(
        target.objects()[&obj_id].payload,
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
    let p_id = target.pages()[0].id;
    target
        .add_layer(p_id, "Temp Empty", false, MasterLayerScope::None)
        .unwrap();
    assert_eq!(target.pages()[0].layers.len(), 2);
    run_macro(
        &mut target,
        find_builtin_macro("clean-empty-layers").unwrap().script,
        &BTreeMap::new(),
        &MacroSandboxConfig::default(),
    )
    .unwrap();
    assert_eq!(target.pages()[0].layers.len(), 1);
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
    assert_eq!(merged.pages().len(), 2);
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
    doc.set_page_bleed(0, 3.0).unwrap();

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
    doc.set_object_transform(safe_grp, Affine2D::translate(50.0, 50.0))
        .unwrap();

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
    doc.set_object_transform(bleed_grp, Affine2D::translate(-51.0, 0.0))
        .unwrap();

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
    metrics_doc
        .set_object_transform(tg, Affine2D::translate(80.0, 90.0))
        .unwrap();

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
    let pid = doc.pages()[0].id;
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
    assert!(doc.has_object(oid));

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
    tpl.set_object_transform(grp, Affine2D::translate(30.0, 40.0))
        .unwrap();

    // Row 0 has empty OptCode -> must NOT fail! Row 1 has populated OptCode -> regenerates barcode!
    let csv = "Item,SKU,OptCode\nWidget A,SKU-001,\nWidget B,SKU-002,OPT-99\n";
    let table = PrintMergeTable::from_csv(csv).unwrap();
    let merged = apply_print_merge(&tpl, &table, &PrintMergeOptions::default()).unwrap();
    assert_eq!(merged.pages().len(), 2);

    // Page 0: verify Product Card group still holds 3 children, child[1] is replaced with vector barcode group,
    // and its world bounds reflect the parent group's translate(30, 40) + child local (0, 15) = (30, 55)!
    let p0_grp_id = merged.pages()[0].layers[0].objects[0];
    let NodePayload::Group(ref p0_children) = merged.objects()[&p0_grp_id].payload else {
        panic!("expected Product Card group on page 0");
    };
    assert_eq!(p0_children.len(), 3);
    assert_eq!(
        merged.objects()[&p0_children[1]].name,
        "Barcode:Code128:SKU-001"
    );
    assert!(matches!(
        merged.objects()[&p0_children[1]].payload,
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
    let p1_grp_id = merged.pages()[1].layers[0].objects[0];
    let NodePayload::Group(ref p1_children) = merged.objects()[&p1_grp_id].payload else {
        panic!("expected Product Card group on page 1");
    };
    assert_eq!(
        merged.objects()[&p1_children[2]].name,
        "Barcode:Code128:OPT-99"
    );
    assert!(matches!(
        merged.objects()[&p1_children[2]].payload,
        NodePayload::Group(_)
    ));
}

#[test]
fn w1b_r11_cql_leading_dot_float_and_master_layer_owning_page_filtering() {
    use coree_prod::evaluate_cql_query;

    let mut doc = Document::new_a4("CQL & Master Scope");
    let p1 = doc.pages()[0].id;
    let _p2 = doc.add_page("Page 2", 210.0, 297.0, 3.0);

    let mut half_alpha = make_rect(
        10.0,
        10.0,
        20.0,
        20.0,
        Fill::Solid(Color::CYAN),
        OutlineStyle::solid(Color::KEY_BLACK, 0.5),
    );
    half_alpha.opacity = 0.5;
    let half_id = doc.add_object(half_alpha, None).unwrap();

    let mut full_alpha = make_rect(
        40.0,
        10.0,
        20.0,
        20.0,
        Fill::Solid(Color::MAGENTA),
        OutlineStyle::solid(Color::KEY_BLACK, 0.1),
    );
    full_alpha.opacity = 1.0;
    let full_id = doc.add_object(full_alpha, None).unwrap();

    // Leading-dot float `.5` and `.25mm` in CQL
    let matched_opacity =
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@opacity == .5").unwrap();
    assert_eq!(matched_opacity, vec![half_id]);
    let matched_width =
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@outline.width >= .25mm").unwrap();
    assert_eq!(matched_width, vec![half_id]);
    let _ = full_id;

    // EvenPages master layer created on Page 1 must NOT be included on Page 1 metrics/imposition!
    let even_layer = doc
        .add_layer(p1, "EvenOnlyOnP1", false, MasterLayerScope::EvenPages)
        .unwrap();
    let even_obj = make_rect(
        180.0,
        260.0,
        20.0,
        20.0,
        Fill::Solid(Color::YELLOW),
        OutlineStyle::none(),
    );
    doc.add_object(even_obj, Some(even_layer)).unwrap();

    let imposed = impose_document(
        &doc,
        &ImpositionMode::NUp {
            cols: 1,
            rows: 1,
            gap_x_mm: 0.0,
            gap_y_mm: 0.0,
            sheet_w_mm: 210.0,
            sheet_h_mm: 297.0,
        },
    )
    .unwrap();
    // Page 1 sheet should only contain the 2 local Page 1 rects, NOT the EvenPages rect!
    let sheet1_obj_count: usize = imposed.pages()[0]
        .layers
        .iter()
        .map(|l| l.objects.len())
        .sum();
    assert_eq!(
        sheet1_obj_count, 2,
        "EvenPages master layer on Page 1 must not be copied onto Page 1 imposition sheet"
    );
    // Page 2 sheet SHOULD contain the EvenPages rect!
    let sheet2_obj_count: usize = imposed.pages()[1]
        .layers
        .iter()
        .map(|l| l.objects.len())
        .sum();
    assert_eq!(
        sheet2_obj_count, 1,
        "EvenPages master layer from Page 1 must appear on Page 2 imposition sheet"
    );

    // evaluate_cql_query with FindScope::CurrentPage must also respect EvenPages on Page 1 vs Page 2
    let cql_p1 = evaluate_cql_query(&doc, FindScope::CurrentPage, "@visible == true").unwrap();
    assert_eq!(
        cql_p1.len(),
        2,
        "EvenPages layer on Page 1 must not match CurrentPage(Page 1) CQL query"
    );
    doc.set_active_page_index(1).unwrap();
    let cql_p2 = evaluate_cql_query(&doc, FindScope::CurrentPage, "@visible == true").unwrap();
    assert_eq!(
        cql_p2.len(),
        1,
        "EvenPages layer from Page 1 must match CurrentPage(Page 2) CQL query"
    );
}

#[test]
fn w1b_r12_preflight_handles_branching_cyclic_symbol_and_symbol_local_text_on_path_connector() {
    // 1. SymbolDef containing symbol-local Path, TextOnPath, Shape, and Connector safely centered on Page 1
    let mut doc = Document::new_a4("Preflight Symbol Context");
    let path_id = ObjectId(9001);
    let shape_id = ObjectId(9002);
    let sym_path = SceneObject::new(
        path_id,
        "SymCenterPath",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(80.0, 120.0),
            Point2D::new(130.0, 120.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.5));
    let sym_top = SceneObject::new(
        ObjectId(9003),
        "SymTextOnPath",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain("CenteredLabel", TextStyle::default()),
            path_id,
            offset_mm: 0.0,
            distance_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    )
    .with_fill(Fill::Solid(Color::KEY_BLACK))
    .with_outline(OutlineStyle::none());
    let sym_shape = SceneObject::new(
        shape_id,
        "SymCenterBox",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            80.0, 150.0, 30.0, 20.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::CYAN))
    .with_outline(OutlineStyle::none());
    let sym_conn = SceneObject::new(
        ObjectId(9004),
        "SymConnector",
        NodePayload::Connector(ConnectorObject::between_objects(
            path_id,
            AnchorPoint::Center,
            shape_id,
            AnchorPoint::Center,
            ConnectorStyle::Straight,
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.5));

    let sym = SymbolDef::new(
        "CenteredWidget",
        vec![sym_path, sym_top, sym_shape, sym_conn],
    );
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "SymInstCentered",
            NodePayload::SymbolInstance(sym_id),
        ),
        None,
    )
    .unwrap();

    let report = run_preflight(&doc, &PreflightRules::default());
    assert!(
        !report
            .issues
            .iter()
            .any(|i| i.kind == PreflightIssueKind::InsufficientBleedCoverage),
        "symbol-local TextOnPath/Connector centered on page must not trigger bleed false positive: {:?}",
        report.issues
    );

    // 2. Branching self-cyclic SymbolDef (2 SymbolInstance(self) children) must terminate immediately
    let mut cyclic_doc = Document::new_a4("Branching Cyclic Symbol");
    let cyclic_id = coree_core::SymbolId::new();
    let cyclic_sym = SymbolDef {
        id: cyclic_id,
        name: "BranchingCycle".into(),
        objects: vec![
            SceneObject::new(ObjectId(1), "SelfA", NodePayload::SymbolInstance(cyclic_id)),
            SceneObject::new(ObjectId(2), "SelfB", NodePayload::SymbolInstance(cyclic_id)),
            SceneObject::new(
                ObjectId(3),
                "LeafRect",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    60.0, 60.0, 20.0, 20.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::CYAN))
            .with_outline(OutlineStyle::none()),
        ],
    };
    cyclic_doc.insert_symbol(cyclic_sym);
    cyclic_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "CyclicInst",
                NodePayload::SymbolInstance(cyclic_id),
            ),
            None,
        )
        .unwrap();

    let cyclic_report = run_preflight(&cyclic_doc, &PreflightRules::default());
    assert!(
        !cyclic_report
            .issues
            .iter()
            .any(|i| i.kind == PreflightIssueKind::InsufficientBleedCoverage),
        "branching cyclic SymbolDef must terminate cleanly with centered leaf bounds"
    );
}

#[test]
fn w1b_r13_preflight_respects_master_layer_page_scope_and_dedups_layer_state() {
    // Page 1 (A4) owns an EvenPages master layer (locked) whose object crosses Page 2's (A5) right trim edge.
    let mut doc = Document::new_a4("R13 Master Preflight");
    let p1_id = doc.pages()[0].id;
    let p2_id = doc.add_page("Page 2 A5", 148.0, 210.0, 3.0);
    let p3_id = doc.add_page("Page 3 A4", 210.0, 297.0, 3.0);
    let p4_id = doc.add_page("Page 4 A5", 148.0, 210.0, 3.0);

    let even_master = doc
        .add_layer(p1_id, "EvenMaster", false, MasterLayerScope::EvenPages)
        .unwrap();
    let edge_rect = make_rect(
        147.0,
        20.0,
        2.0,
        20.0,
        Fill::Solid(Color::CYAN),
        OutlineStyle::none(),
    );
    doc.add_object(edge_rect, Some(even_master)).unwrap();
    doc.set_layer_flags(even_master, None, Some(true), None, None)
        .unwrap();

    let report = run_preflight(&doc, &PreflightRules::default());
    let bleed_pages: Vec<_> = report
        .issues
        .iter()
        .filter(|i| i.kind == PreflightIssueKind::InsufficientBleedCoverage)
        .filter_map(|i| i.page_id)
        .collect();
    assert!(bleed_pages.contains(&p2_id));
    assert!(bleed_pages.contains(&p4_id));
    assert!(!bleed_pages.contains(&p1_id));
    assert!(!bleed_pages.contains(&p3_id));

    let locked_layer_hits = report
        .issues
        .iter()
        .filter(|i| {
            i.kind == PreflightIssueKind::LockedLayerWithObjects && i.layer_id == Some(even_master)
        })
        .count();
    assert_eq!(
        locked_layer_hits, 1,
        "layer-level LockedLayerWithObjects must be deduplicated once per LayerId across even pages"
    );

    // 1-page document with an EvenPages master layer must NOT evaluate against Page 1
    let mut single_doc = Document::new_a4("Single Page EvenMaster");
    let sp1 = single_doc.pages()[0].id;
    let sp1_even = single_doc
        .add_layer(sp1, "EvenOnly", false, MasterLayerScope::EvenPages)
        .unwrap();
    single_doc
        .add_object(
            make_rect(
                209.0,
                20.0,
                2.0,
                20.0,
                Fill::Solid(Color::CYAN),
                OutlineStyle::none(),
            ),
            Some(sp1_even),
        )
        .unwrap();
    single_doc
        .set_layer_flags(sp1_even, None, Some(true), None, None)
        .unwrap();
    let single_report = run_preflight(&single_doc, &PreflightRules::default());
    assert!(
        single_report.issues.is_empty(),
        "EvenPages layer on 1-page doc must not report false positives on Page 1: {:?}",
        single_report.issues
    );
}

#[test]
fn w1b_r17_print_merge_container_barcode_preserves_connector_world_fallback() {
    let mut tpl = Document::new_a4("R17 PrintMerge Container Barcode");
    let lid = tpl.pages()[0].layers[0].id;

    // 1. BARCODE placeholder inside a rotated+translated Group
    let grp_bc = tpl
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "BARCODE:QR:<<Code>>",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "<<Code>>",
                    TextStyle::default(),
                )),
            )
            .with_transform(Affine2D::translate(10.0, 5.0)),
            Some(lid),
        )
        .unwrap();
    let grp_id = tpl.group_objects(&[grp_bc], "BarcodeGroup").unwrap();
    tpl.transform_object(
        grp_id,
        Affine2D::translate(80.0, 90.0).compose(&Affine2D::rotate_deg(45.0)),
    )
    .unwrap();

    // 2. BARCODE placeholder inside a rotated+translated PowerClip
    let pc_bc = tpl
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "BARCODE:CODE128:<<Sku>>",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "<<Sku>>",
                    TextStyle::default(),
                )),
            )
            .with_transform(Affine2D::translate(6.0, 8.0)),
            Some(lid),
        )
        .unwrap();
    let pc_rect = tpl
        .add_object(
            make_rect(
                0.0,
                0.0,
                60.0,
                40.0,
                Fill::Solid(Color::WHITE),
                OutlineStyle::none(),
            ),
            Some(lid),
        )
        .unwrap();
    let pc_id = tpl.create_powerclip(pc_rect, &[pc_bc]).unwrap();
    tpl.transform_object(
        pc_id,
        Affine2D::translate(120.0, 150.0).compose(&Affine2D::rotate_deg(30.0)),
    )
    .unwrap();

    let expected_grp_anchor = AnchorPoint::Center.resolve(tpl.world_object_bounds(grp_bc).unwrap());
    let expected_pc_anchor = AnchorPoint::Center.resolve(tpl.world_object_bounds(pc_bc).unwrap());

    // 3. Connectors referencing both container-nested BARCODE placeholders
    let conn_grp = tpl
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ConnGrp",
                NodePayload::Connector(ConnectorObject::between_objects(
                    grp_bc,
                    AnchorPoint::Center,
                    pc_rect,
                    AnchorPoint::Center,
                    ConnectorStyle::Straight,
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let conn_pc = tpl
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "ConnPc",
                NodePayload::Connector(ConnectorObject::between_objects(
                    pc_bc,
                    AnchorPoint::Center,
                    grp_id,
                    AnchorPoint::Center,
                    ConnectorStyle::Straight,
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let _ = (conn_grp, conn_pc);

    let table = PrintMergeTable {
        headers: vec!["Code".into(), "Sku".into()],
        rows: vec![vec!["ABC-123".into(), "SKU-999".into()]],
    };
    let merged = apply_print_merge(&tpl, &table, &PrintMergeOptions::default()).unwrap();
    assert_eq!(merged.pages().len(), 1);

    let mut found_conn_grp = None;
    let mut found_conn_pc = None;
    for obj in merged.objects().values() {
        if obj.name == "ConnGrp" {
            if let NodePayload::Connector(ref c) = obj.payload {
                found_conn_grp = Some(c.clone());
            }
        } else if obj.name == "ConnPc" {
            if let NodePayload::Connector(ref c) = obj.payload {
                found_conn_pc = Some(c.clone());
            }
        }
    }
    let cg = found_conn_grp.expect("merged ConnGrp");
    assert!(cg.from_obj.is_none());
    assert!(
        cg.from_fallback.distance_to(expected_grp_anchor) < 1e-3,
        "Group BARCODE connector fallback {:?} must match world anchor {:?}",
        cg.from_fallback,
        expected_grp_anchor
    );

    let cp = found_conn_pc.expect("merged ConnPc");
    assert!(cp.from_obj.is_none());
    assert!(
        cp.from_fallback.distance_to(expected_pc_anchor) < 1e-3,
        "PowerClip BARCODE connector fallback {:?} must match world anchor {:?}",
        cp.from_fallback,
        expected_pc_anchor
    );
}

#[test]
fn w1b_r17_cut_contour_text_on_path_inside_rotated_group_and_powerclip() {
    let mut doc = Document::new_a4("R17 CutContour TOP");
    let lid = doc.pages()[0].layers[0].id;

    // 1. TextOnPath inside translated + 45°-rotated Group
    let host1 = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Host1",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 0.0),
                    Point2D::new(40.0, 0.0),
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let top1 = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Top1",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("GROUP_TOP", TextStyle::default()),
                    path_id: host1,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(lid),
        )
        .unwrap();
    let grp = doc.group_objects(&[host1, top1], "TopGroup").unwrap();
    doc.transform_object(
        grp,
        Affine2D::translate(100.0, 120.0).compose(&Affine2D::rotate_deg(45.0)),
    )
    .unwrap();

    let world_top1 = doc.world_object_bounds(top1).unwrap();
    let cut1 = generate_cut_contour(&mut doc, &[top1], 2.0, "CutContour").unwrap();
    let cut1_bb = doc.world_object_bounds(cut1.object_id).unwrap();
    assert!(
        (cut1_bb.min_x - (world_top1.min_x - 2.0)).abs() < 0.5
            && (cut1_bb.min_y - (world_top1.min_y - 2.0)).abs() < 0.5
            && (cut1_bb.max_x - (world_top1.max_x + 2.0)).abs() < 0.5
            && (cut1_bb.max_y - (world_top1.max_y + 2.0)).abs() < 0.5,
        "Group CutContour {cut1_bb:?} must match world TextOnPath bounds {world_top1:?} expanded by 2mm"
    );

    // 2. TextOnPath inside translated + 30°-rotated PowerClip
    let host2 = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Host2",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(0.0, 0.0),
                    Point2D::new(35.0, 0.0),
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let top2 = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Top2",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("PC_TOP", TextStyle::default()),
                    path_id: host2,
                    offset_mm: 0.0,
                    distance_mm: 0.0,
                    flip_vertical: false,
                    flip_horizontal: false,
                }),
            ),
            Some(lid),
        )
        .unwrap();
    let pc_container = doc
        .add_object(
            make_rect(
                0.0,
                0.0,
                60.0,
                40.0,
                Fill::Solid(Color::WHITE),
                OutlineStyle::none(),
            ),
            Some(lid),
        )
        .unwrap();
    let pc = doc.create_powerclip(pc_container, &[host2, top2]).unwrap();
    doc.transform_object(
        pc,
        Affine2D::translate(50.0, 180.0).compose(&Affine2D::rotate_deg(30.0)),
    )
    .unwrap();

    let world_top2 = doc.world_object_bounds(top2).unwrap();
    let cut2 = generate_cut_contour(&mut doc, &[top2], 2.0, "CutContour").unwrap();
    let cut2_bb = doc.world_object_bounds(cut2.object_id).unwrap();
    assert!(
        (cut2_bb.min_x - (world_top2.min_x - 2.0)).abs() < 0.5
            && (cut2_bb.min_y - (world_top2.min_y - 2.0)).abs() < 0.5
            && (cut2_bb.max_x - (world_top2.max_x + 2.0)).abs() < 0.5
            && (cut2_bb.max_y - (world_top2.max_y + 2.0)).abs() < 0.5,
        "PowerClip CutContour {cut2_bb:?} must match world TextOnPath bounds {world_top2:?} expanded by 2mm"
    );
}

#[test]
fn w1b_r18_macros_resize_align_grid_and_cut_contour_symbol_hierarchy() {
    use coree_core::{
        apply_command, ArtisticText, BezierPath, CommandHistory, CommandOp, Point2D, SymbolDef,
        TextOnPath, TextStyle,
    };
    use coree_prod::{plan_macro_ops, MacroSandboxConfig};
    use std::collections::BTreeMap;

    // 1. macros::resize_objects and align_grid on mixed top-level + nested scaled/rotated Group & PowerClip
    let mut doc = Document::new_a4("R18 Macros Nested");
    let lid = doc.pages()[0].layers[0].id;
    let top_r = doc
        .add_object(
            make_rect(
                10.0,
                10.0,
                20.0,
                10.0,
                Fill::Solid(Color::CYAN),
                OutlineStyle::none(),
            ),
            Some(lid),
        )
        .unwrap();
    let grp_child = doc
        .add_object(
            make_rect(
                0.0,
                0.0,
                20.0,
                10.0,
                Fill::Solid(Color::MAGENTA),
                OutlineStyle::none(),
            ),
            Some(lid),
        )
        .unwrap();
    let grp = doc.group_objects(&[grp_child], "ScaledGroup").unwrap();
    doc.transform_object(
        grp,
        Affine2D::translate(100.0, 50.0).compose(&Affine2D::scale(2.0, 2.0)),
    )
    .unwrap();

    let pc_child = doc
        .add_object(
            make_rect(
                0.0,
                0.0,
                20.0,
                10.0,
                Fill::Solid(Color::YELLOW),
                OutlineStyle::none(),
            ),
            Some(lid),
        )
        .unwrap();
    let pc_host = doc
        .add_object(
            make_rect(
                0.0,
                0.0,
                100.0,
                100.0,
                Fill::Solid(Color::WHITE),
                OutlineStyle::none(),
            ),
            Some(lid),
        )
        .unwrap();
    let pc = doc.create_powerclip(pc_host, &[pc_child]).unwrap();
    doc.transform_object(
        pc,
        Affine2D::translate(80.0, 120.0).compose(&Affine2D::rotate_deg(90.0)),
    )
    .unwrap();

    let cfg = MacroSandboxConfig::default();
    let resize_script = format!(
        "resize_objects([{}, {}, {}], 30.0, 15.0);",
        top_r.0, grp_child.0, pc_child.0
    );
    let init_doc = doc.clone();
    let (_res, resize_ops) = plan_macro_ops(&doc, &resize_script, &BTreeMap::new(), &cfg).unwrap();
    let mut hist = CommandHistory::default();
    hist.execute(&mut doc, CommandOp::Batch(resize_ops))
        .unwrap();

    for (label, id) in [
        ("top", top_r),
        ("group_child", grp_child),
        ("powerclip_child", pc_child),
    ] {
        let bb = doc.world_object_bounds(id).unwrap();
        assert!(
            (bb.width() - 30.0).abs() < 1e-4 && (bb.height() - 15.0).abs() < 1e-4,
            "{label} world size after resize_objects must be 30x15, got {bb:?}"
        );
    }

    let align_script = format!(
        "align_grid([{}, {}, {}], 2, 10.0, 10.0, 5.0, 5.0);",
        top_r.0, grp_child.0, pc_child.0
    );
    let (_res2, align_ops) = plan_macro_ops(&doc, &align_script, &BTreeMap::new(), &cfg).unwrap();
    hist.execute(&mut doc, CommandOp::Batch(align_ops)).unwrap();

    let bb0 = doc.world_object_bounds(top_r).unwrap();
    let bb1 = doc.world_object_bounds(grp_child).unwrap();
    let bb2 = doc.world_object_bounds(pc_child).unwrap();
    assert!(
        (bb0.min_x - 5.0).abs() < 1e-4 && (bb0.min_y - 5.0).abs() < 1e-4,
        "top_r slot (0,0) expected (5,5), got {bb0:?}"
    );
    assert!(
        (bb1.min_x - 45.0).abs() < 1e-4 && (bb1.min_y - 5.0).abs() < 1e-4,
        "grp_child slot (0,1) expected (45,5), got {bb1:?}"
    );
    assert!(
        (bb2.min_x - 5.0).abs() < 1e-4 && (bb2.min_y - 30.0).abs() < 1e-4,
        "pc_child slot (1,0) expected (5,30), got {bb2:?}"
    );

    // Undo restores initial doc cleanly
    hist.undo(&mut doc).unwrap();
    hist.undo(&mut doc).unwrap();
    assert_eq!(doc.objects(), init_doc.objects());
    let _ = apply_command;

    // 2. generate_cut_contour on SymbolInstance containing translated+rotated Group and TextOnPath
    let mut sdoc = Document::new_a4("R18 CutContour Symbol");
    let slid = sdoc.pages()[0].layers[0].id;
    let s_rect_id = ObjectId(7001);
    let s_grp_id = ObjectId(7002);
    let s_path_id = ObjectId(7003);
    let s_top_id = ObjectId(7004);

    let s_rect = SceneObject::new(
        s_rect_id,
        "SymRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 20.0, 10.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::CYAN))
    .with_outline(OutlineStyle::none());
    let s_grp = SceneObject::new(s_grp_id, "SymGrp", NodePayload::Group(vec![s_rect_id]))
        .with_transform(Affine2D::translate(100.0, 80.0).compose(&Affine2D::rotate_deg(45.0)));

    let sym1 = SymbolDef::new("SymGroupedRect", vec![s_rect, s_grp]);
    let sym1_id = sym1.id;
    sdoc.insert_symbol(sym1);
    let inst1 = sdoc
        .add_object(
            SceneObject::new(ObjectId(0), "Inst1", NodePayload::SymbolInstance(sym1_id)),
            Some(slid),
        )
        .unwrap();
    let expected1 = sdoc.world_object_bounds(inst1).unwrap();
    let cut1 = generate_cut_contour(&mut sdoc, &[inst1], 2.0, "CutContour").unwrap();
    let cut1_bb = sdoc.world_object_bounds(cut1.object_id).unwrap();
    assert!(
        cut1_bb.min_x < expected1.min_x - 1.0
            && cut1_bb.min_y < expected1.min_y - 1.0
            && cut1_bb.max_x > expected1.max_x + 1.0
            && cut1_bb.max_y > expected1.max_y + 1.0
            && (cut1_bb.center().x - expected1.center().x).abs() < 0.25
            && (cut1_bb.center().y - expected1.center().y).abs() < 0.25,
        "SymbolInstance(Group) CutContour {cut1_bb:?} must cleanly surround world bounds {expected1:?}"
    );

    let s_path = SceneObject::new(
        s_path_id,
        "SymHost",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(0.0, 0.0),
            Point2D::new(40.0, 0.0),
        )),
    );
    let s_top = SceneObject::new(
        s_top_id,
        "SymTop",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain("SYM_CUT", TextStyle::default()),
            path_id: s_path_id,
            offset_mm: 0.0,
            distance_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    let sym2 = SymbolDef::new("SymWithTop", vec![s_path, s_top]);
    let sym2_id = sym2.id;
    sdoc.insert_symbol(sym2);
    let inst2 = sdoc
        .add_object(
            SceneObject::new(ObjectId(0), "Inst2", NodePayload::SymbolInstance(sym2_id))
                .with_transform(
                    Affine2D::translate(30.0, 50.0).compose(&Affine2D::rotate_deg(30.0)),
                ),
            Some(slid),
        )
        .unwrap();
    let expected2 = sdoc.world_object_bounds(inst2).unwrap();
    let cut2 = generate_cut_contour(&mut sdoc, &[inst2], 2.0, "CutContour").unwrap();
    let cut2_bb = sdoc.world_object_bounds(cut2.object_id).unwrap();
    assert!(
        (cut2_bb.min_x - (expected2.min_x - 2.0)).abs() < 0.25
            && (cut2_bb.min_y - (expected2.min_y - 2.0)).abs() < 0.25
            && (cut2_bb.max_x - (expected2.max_x + 2.0)).abs() < 0.25
            && (cut2_bb.max_y - (expected2.max_y + 2.0)).abs() < 0.25,
        "SymbolInstance(TextOnPath) CutContour {cut2_bb:?} must wrap world bounds {expected2:?} + 2mm"
    );
}

#[test]
fn w1b_r20_generate_cut_contour_and_metrics_on_grouped_and_powerclipped_child() {
    let mut doc = Document::new_a4("R20 CutContour Child");
    let lid = doc.pages()[0].layers[0].id;

    let red = Color::rgba8(255, 0, 0, 255);
    let blue = Color::rgba8(0, 0, 255, 255);

    // 1. Direct child inside a translated + rotated Group
    let child = doc
        .add_object(
            make_rect(
                0.0,
                0.0,
                20.0,
                20.0,
                Fill::Solid(red.clone()),
                OutlineStyle::none(),
            ),
            Some(lid),
        )
        .unwrap();
    let grp = doc.group_objects(&[child], "MovedGroup").unwrap();
    doc.set_object_transform(
        grp,
        Affine2D::translate(100.0, 150.0).compose(&Affine2D::rotate_deg(30.0)),
    )
    .unwrap();

    let expected_wb = doc.world_object_bounds(child).unwrap();
    let cut = generate_cut_contour(&mut doc, &[child], 1.0, "CutContour").unwrap();
    let cut_bb = doc.world_object_bounds(cut.object_id).unwrap();
    assert!(
        (cut_bb.center().x - expected_wb.center().x).abs() < 0.25
            && (cut_bb.center().y - expected_wb.center().y).abs() < 0.25
            && cut_bb.min_x < expected_wb.min_x - 0.5
            && cut_bb.max_x > expected_wb.max_x + 0.5,
        "CutContour {cut_bb:?} for grouped child must align with world bounds {expected_wb:?} (NOT page origin)"
    );

    let sel_metrics = compute_production_metrics(&doc, Some(&[child]));
    let m_bb = sel_metrics.bounding_box.unwrap();
    assert!(
        (m_bb.min_x - expected_wb.min_x).abs() < 0.25
            && (m_bb.min_y - expected_wb.min_y).abs() < 0.25
            && (m_bb.max_x - expected_wb.max_x).abs() < 0.25
            && (m_bb.max_y - expected_wb.max_y).abs() < 0.25,
        "compute_production_metrics(Some(&[child])) bbox {m_bb:?} must match {expected_wb:?}"
    );

    // 2. Direct child inside a translated PowerClip
    let pc_child = doc
        .add_object(
            make_rect(
                5.0,
                5.0,
                20.0,
                20.0,
                Fill::Solid(blue),
                OutlineStyle::none(),
            ),
            Some(lid),
        )
        .unwrap();
    let pc_container = doc
        .add_object(
            make_rect(0.0, 0.0, 40.0, 40.0, Fill::Solid(red), OutlineStyle::none())
                .with_transform(Affine2D::translate(80.0, 40.0)),
            Some(lid),
        )
        .unwrap();
    let _pc_id = doc.create_powerclip(pc_container, &[pc_child]).unwrap();
    let pc_child_wb = doc.world_object_bounds(pc_child).unwrap();
    let pc_cut = generate_cut_contour(&mut doc, &[pc_child], 1.5, "CutContourPC").unwrap();
    let pc_cut_bb = doc.world_object_bounds(pc_cut.object_id).unwrap();
    assert!(
        (pc_cut_bb.min_x - (pc_child_wb.min_x - 1.5)).abs() < 0.25
            && (pc_cut_bb.min_y - (pc_child_wb.min_y - 1.5)).abs() < 0.25
            && (pc_cut_bb.max_x - (pc_child_wb.max_x + 1.5)).abs() < 0.25
            && (pc_cut_bb.max_y - (pc_child_wb.max_y + 1.5)).abs() < 0.25,
        "PowerClip child CutContour {pc_cut_bb:?} must wrap world bounds {pc_child_wb:?} + 1.5mm"
    );
}

#[test]
fn w1b_r21_compute_production_metrics_expands_symbol_instance() {
    let mut doc = Document::new_a4("R21 Symbol Metrics");
    let page_id = doc.pages()[0].id;
    let cut_lid = doc
        .add_layer(page_id, "Cut", true, MasterLayerScope::None)
        .unwrap();

    let red = Color::rgba8(255, 0, 0, 255);
    let mut leaf = make_rect(0.0, 0.0, 10.0, 20.0, Fill::Solid(red), OutlineStyle::none());
    leaf.id = ObjectId(1);
    let grp = SceneObject::new(ObjectId(2), "SymGrp", NodePayload::Group(vec![ObjectId(1)]));
    let sym = SymbolDef::new("CutSym", vec![leaf, grp]);
    let sym_id = sym.id;
    doc.insert_symbol(sym);

    let inst = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "CutSymInst",
                NodePayload::SymbolInstance(sym_id),
            )
            .with_transform(Affine2D::scale(2.0, 2.0)),
            Some(cut_lid),
        )
        .unwrap();

    // Invisible instance must be ignored
    let mut hidden_obj = SceneObject::new(
        ObjectId(0),
        "HiddenSymInst",
        NodePayload::SymbolInstance(sym_id),
    )
    .with_transform(Affine2D::translate(50.0, 50.0));
    hidden_obj.visible = false;
    let hidden_inst = doc.add_object(hidden_obj, Some(cut_lid)).unwrap();

    // 1. Selected SymbolInstance on Cut layer: world 20x40 => cut_length_mm=120, filled_area_mm2=800
    let sel = compute_production_metrics(&doc, Some(&[inst]));
    assert!(
        (sel.cut_length_mm - 120.0).abs() < 0.5
            && (sel.filled_area_mm2 - 800.0).abs() < 1.5
            && sel.node_count == 4
            && sel.subpath_count == 1,
        "compute_production_metrics(Some(&[sym_inst])) must expand SymbolInstance->Group (exp len=120, area=800, nodes=4), got {sel:?}"
    );

    // 2. Whole document (selection=None) matches and ignores hidden_inst
    let full = compute_production_metrics(&doc, None);
    assert!(
        (full.cut_length_mm - 120.0).abs() < 0.5 && (full.filled_area_mm2 - 800.0).abs() < 1.5,
        "compute_production_metrics(&doc, None) must measure visible Cut SymbolInstance and skip hidden {hidden_inst:?}, got {full:?}"
    );
}

#[test]
fn w1b_r26_symbol_zero_id_metrics_and_cql_table_cell_predicates() {
    use coree_core::VectorTable;
    use coree_prod::evaluate_cql_query;

    // 1. challenger1-r25-1: ObjectId(0) Group in SymbolDef propagates translate(50,30)
    //    into compute_production_metrics for Connector & bbox.
    let mut doc = Document::new_a4("R26 ZeroId Prod Metrics");
    let lid = doc.pages()[0].layers[0].id;
    let sym_rect = SceneObject::new(
        ObjectId(10),
        "SymRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 20.0, 10.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::CYAN))
    .with_outline(OutlineStyle::none());
    let sym_conn = SceneObject::new(
        ObjectId(11),
        "SymConn",
        NodePayload::Connector(ConnectorObject {
            from_obj: Some(ObjectId(10)),
            from_anchor: AnchorPoint::TopLeft,
            from_fallback: Point2D::ZERO,
            to_obj: None,
            to_anchor: AnchorPoint::Center,
            to_fallback: Point2D::new(20.0, 10.0),
            style: ConnectorStyle::Straight,
            corner_radius_mm: 0.0,
        }),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.2));
    let zero_grp = SceneObject::new(
        ObjectId(0),
        "ZeroGrp",
        NodePayload::Group(vec![ObjectId(10), ObjectId(11)]),
    )
    .with_transform(Affine2D::translate(50.0, 30.0));
    let sym = SymbolDef::new("ZeroMetricsSym", vec![sym_rect, sym_conn, zero_grp]);
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    doc.add_object(
        SceneObject::new(ObjectId(0), "Inst", NodePayload::SymbolInstance(sym_id)),
        Some(lid),
    )
    .unwrap();

    let metrics = compute_production_metrics(&doc, None);
    let expected_len = 60.0 + 20.0_f64.hypot(10.0);
    assert!(
        (metrics.artwork_length_mm - expected_len).abs() < 0.2,
        "artwork_length_mm expected ~{expected_len:.4}, got {}",
        metrics.artwork_length_mm
    );
    let m_bb = metrics.bounding_box.unwrap();
    assert!(
        (m_bb.min_x - 50.0).abs() < 0.1
            && (m_bb.min_y - 30.0).abs() < 0.1
            && (m_bb.max_x - 70.0).abs() < 0.1
            && (m_bb.max_y - 40.0).abs() < 0.1,
        "metrics bbox expected [50..70, 30..40], got {m_bb:?}"
    );

    // 2. challenger1-r25-3: CQL evaluates @color, @fill.model, and @font / @text.font across VectorTable cells
    let mut tbl = VectorTable::new(1, 2, 20.0, 10.0);
    tbl.cells[0].fill = Fill::Solid(Color::spot("Pantone185", 1.0, [0.0, 1.0, 1.0, 0.0]));
    tbl.cells[1].text = "Label".into();
    tbl.cells[1].fill = Fill::None;
    tbl.cells[1].text_style.font_family = "CourierTest".into();
    tbl.cells[1].text_style.fill = Color::cmyk(1.0, 0.0, 0.0, 0.0);
    let tbl_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "SpotTable", NodePayload::Table(tbl))
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none()),
            Some(lid),
        )
        .unwrap();

    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@color == 'Pantone185'").unwrap(),
        vec![tbl_id]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@fill.model == 'spot'").unwrap(),
        vec![tbl_id]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@fill.model == 'cmyk'").unwrap(),
        vec![tbl_id]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@font == 'CourierTest'").unwrap(),
        vec![tbl_id]
    );
    assert_eq!(
        evaluate_cql_query(
            &doc,
            FindScope::WholeDocument,
            "@text.font == 'CourierTest'"
        )
        .unwrap(),
        vec![tbl_id]
    );
    assert!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@font == 'MissingFont'")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn w1b_r27_cql_fill_model_table_and_text_with_outer_fill() {
    use coree_core::VectorTable;
    use coree_prod::evaluate_cql_query;

    let mut doc = Document::new_a4("R27 CQL Fill Model Outer Fill");
    let lid = doc.pages()[0].layers[0].id;

    // 1. VectorTable with non-None outer obj.fill (CMYK Cyan) + Spot cell fill + RGB cell text
    let mut tbl = VectorTable::new(1, 2, 20.0, 10.0);
    tbl.cells[0].fill = Fill::Solid(Color::spot("Pantone185", 1.0, [0.0, 1.0, 1.0, 0.0]));
    tbl.cells[1].text = "CellText".into();
    tbl.cells[1].fill = Fill::None;
    tbl.cells[1].text_style.fill = Color::rgb(0.1, 0.2, 0.3);
    let tbl_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "OuterCmykTable", NodePayload::Table(tbl))
                .with_fill(Fill::Solid(Color::CYAN))
                .with_outline(OutlineStyle::none()),
            Some(lid),
        )
        .unwrap();

    // 2. ArtisticText with non-None outer obj.fill (CMYK Cyan) + Spot text run
    let txt_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "OuterCmykText",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "SpotRun",
                    TextStyle {
                        font_family: "Cairo".into(),
                        fill: Color::spot("Pantone300", 1.0, [1.0, 0.4, 0.0, 0.0]),
                        ..TextStyle::default()
                    },
                )),
            )
            .with_fill(Fill::Solid(Color::CYAN))
            .with_outline(OutlineStyle::none()),
            Some(lid),
        )
        .unwrap();

    // 3. VectorTable with non-None outer obj.fill (CMYK Cyan) + Spot cell text fill (empty cell fill)
    let mut tbl_text_spot = VectorTable::new(1, 1, 20.0, 10.0);
    tbl_text_spot.cells[0].fill = Fill::None;
    tbl_text_spot.cells[0].text = "SpotCellText".into();
    tbl_text_spot.cells[0].text_style.fill = Color::spot("Pantone485", 1.0, [0.0, 0.9, 1.0, 0.0]);
    let tbl_text_spot_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "OuterCmykCellTextSpotTable",
                NodePayload::Table(tbl_text_spot),
            )
            .with_fill(Fill::Solid(Color::CYAN))
            .with_outline(OutlineStyle::none()),
            Some(lid),
        )
        .unwrap();

    // 4. Control CMYK-only rect (must match @fill.model != 'spot', NOT @fill.model == 'spot')
    let cmyk_rect_id = doc
        .add_object(
            make_rect(
                0.0,
                0.0,
                10.0,
                10.0,
                Fill::Solid(Color::CYAN),
                OutlineStyle::none(),
            ),
            Some(lid),
        )
        .unwrap();

    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@fill.model == 'spot'").unwrap(),
        vec![tbl_id, txt_id, tbl_text_spot_id]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@fill.model != 'spot'").unwrap(),
        vec![cmyk_rect_id]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@fill.model == 'cmyk'").unwrap(),
        vec![tbl_id, txt_id, tbl_text_spot_id, cmyk_rect_id]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@fill.model == 'rgb'").unwrap(),
        vec![tbl_id]
    );
}

#[test]
fn w1b_r28_cql_table_border_style_fill_type_and_trimmed_font() {
    use coree_core::{GradientStop, Point2D, VectorTable};
    use coree_prod::evaluate_cql_query;

    let mut doc = Document::new_a4("R28 CQL Table & Text Parity");
    let lid = doc.pages()[0].layers[0].id;

    let mut tbl = VectorTable::new(1, 2, 20.0, 10.0);
    tbl.border_style = OutlineStyle::cut_contour();
    tbl.cells[0].fill = Fill::LinearGradient {
        start: Point2D::ZERO,
        end: Point2D::new(20.0, 0.0),
        stops: vec![
            GradientStop::new(0.0, Color::spot("Pantone185", 1.0, [0.0, 1.0, 1.0, 0.0])),
            GradientStop::new(1.0, Color::WHITE),
        ],
    };
    let tid = doc
        .add_object(
            SceneObject::new(ObjectId(0), "CutGradTable", NodePayload::Table(tbl))
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none()),
            Some(lid),
        )
        .unwrap();

    let art_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "PaddedFontText",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "Hello",
                    TextStyle {
                        font_family: " Arial  ".into(),
                        fill: Color::CYAN,
                        ..TextStyle::default()
                    },
                )),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::none()),
            Some(lid),
        )
        .unwrap();

    let empty_id = doc
        .add_object(
            make_rect(0.0, 0.0, 10.0, 10.0, Fill::None, OutlineStyle::none()),
            Some(lid),
        )
        .unwrap();

    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@color == 'CutContour'").unwrap(),
        vec![tid]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@outline.model == 'spot'").unwrap(),
        vec![tid]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@outline.hairline == true").unwrap(),
        vec![tid]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@outline.overprint == true").unwrap(),
        vec![tid]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@outline.width > 0mm").unwrap(),
        vec![tid]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@outline.model == 'none'").unwrap(),
        vec![art_id, empty_id]
    );

    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@fill.type == 'linear'").unwrap(),
        vec![tid]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@fill.type == 'solid'").unwrap(),
        vec![art_id]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@fill.type != 'none'").unwrap(),
        vec![tid, art_id]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@fill.type == 'none'").unwrap(),
        vec![empty_id]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@font == 'Arial'").unwrap(),
        vec![art_id]
    );
    assert_eq!(
        evaluate_cql_query(&doc, FindScope::WholeDocument, "@text.font == 'Arial'").unwrap(),
        vec![art_id]
    );
}

#[test]
fn w2_exit_r6_metrics_and_laser_include_bspline_artistic_media_clone_persp_and_symbol() {
    use coree_core::{ArtisticMediaStyle, BSplineCurve, Perspective3x3};
    use coree_prod::{plan_laser_toolpath, LaserJobConfig};

    let mut doc = Document::new_a4("W2 Prod BSpline Media Clone");
    let lid = doc.pages()[0].layers[0].id;

    // 1. Shape 20x20 mm = 80 mm CutContour
    let shape_id = doc
        .add_object(
            make_rect(
                10.0,
                10.0,
                20.0,
                20.0,
                Fill::None,
                OutlineStyle::cut_contour(),
            ),
            Some(lid),
        )
        .unwrap();

    // 2. Closed BSpline CutContour
    let bs = BSplineCurve::from_control_points(
        vec![
            Point2D::new(40.0, 10.0),
            Point2D::new(70.0, 10.0),
            Point2D::new(70.0, 40.0),
            Point2D::new(40.0, 40.0),
        ],
        2,
        true,
    );
    doc.add_object(
        SceneObject::new(ObjectId(0), "BS_Cut", NodePayload::BSpline(bs))
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::cut_contour()),
        Some(lid),
    )
    .unwrap();

    // 3. ArtisticMedia CutContour
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "AM_Cut",
            NodePayload::ArtisticMedia {
                spine: BezierPath::from_line(Point2D::new(10.0, 50.0), Point2D::new(50.0, 50.0)),
                style: ArtisticMediaStyle::Calligraphic {
                    width_mm: 2.0,
                    angle_deg: 45.0,
                },
            },
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::cut_contour()),
        Some(lid),
    )
    .unwrap();

    // 4. Clone of Shape with link_outline=true (own outline=none, inheriting CutContour) + Perspective3x3
    let mut clone_obj = SceneObject::new(
        ObjectId(0),
        "Clone_Cut",
        NodePayload::Clone {
            master_id: shape_id,
            link_fill: true,
            link_outline: true,
            link_shape: true,
        },
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(80.0, 10.0));
    clone_obj.perspective = Some(
        Perspective3x3::from_rect_to_quad(
            Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0),
            &[
                Point2D::new(0.0, 0.0),
                Point2D::new(30.0, 0.0),
                Point2D::new(30.0, 30.0),
                Point2D::new(0.0, 30.0),
            ],
        )
        .unwrap(),
    );
    doc.add_object(clone_obj, Some(lid)).unwrap();

    // 5. SymbolInstance containing BSpline, ArtisticMedia, and Clone (all CutContour)
    let sym_bs = SceneObject::new(
        ObjectId(101),
        "SymBS",
        NodePayload::BSpline(BSplineCurve::from_control_points(
            vec![
                Point2D::new(0.0, 0.0),
                Point2D::new(15.0, 0.0),
                Point2D::new(15.0, 15.0),
                Point2D::new(0.0, 15.0),
            ],
            2,
            true,
        )),
    )
    .with_outline(OutlineStyle::cut_contour());
    let sym_am = SceneObject::new(
        ObjectId(102),
        "SymAM",
        NodePayload::ArtisticMedia {
            spine: BezierPath::from_line(Point2D::new(20.0, 0.0), Point2D::new(40.0, 0.0)),
            style: ArtisticMediaStyle::Calligraphic {
                width_mm: 2.0,
                angle_deg: 0.0,
            },
        },
    )
    .with_outline(OutlineStyle::cut_contour());
    let sym_cl = SceneObject::new(
        ObjectId(103),
        "SymClone",
        NodePayload::Clone {
            master_id: shape_id,
            link_fill: true,
            link_outline: true,
            link_shape: true,
        },
    )
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(45.0, 0.0));
    let sym = SymbolDef::new("LaserSymW2", vec![sym_bs, sym_am, sym_cl]);
    let sid = sym.id;
    doc.insert_symbol(sym);
    doc.add_object(
        SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid))
            .with_transform(Affine2D::translate(10.0, 80.0)),
        Some(lid),
    )
    .unwrap();

    let metrics = compute_production_metrics(&doc, None);
    // Shape(80) + BSpline(>70) + ArtisticMedia(>80) + PerspectiveClone(120) + SymbolInstance(BS+AM+Clone > 120)
    assert!(
        metrics.cut_length_mm > 400.0,
        "expected cut_length_mm > 400 mm across Shape+BSpline+ArtisticMedia+Clone(Persp)+SymbolInstance, got {}",
        metrics.cut_length_mm
    );
    assert!(metrics.object_count >= 7);

    let plan = plan_laser_toolpath(&doc, 0, None, &LaserJobConfig::default()).unwrap();
    assert!(
        plan.cut_count >= 7,
        "expected >= 7 Cut segments (4 top-level + 3 inside SymbolInstance), got {}",
        plan.cut_count
    );
    assert!(
        plan.total_cut_mm > 400.0,
        "expected total_cut_mm > 400 mm, got {}",
        plan.total_cut_mm
    );
}

#[test]
fn w2_exit_r8_generate_cut_contour_applies_perspective3x3_and_effect_perspective() {
    use coree_core::{Effect, Perspective3x3};
    use coree_prod::{plan_laser_toolpath, LaserJobConfig};

    // 1. Direct Perspective3x3 on 20x20 shape mapped to (100..140, 100..140)
    {
        let mut doc = Document::new_a4("CutContourPersp3x3");
        let lid = doc.pages()[0].layers[0].id;
        let mut rect = make_rect(
            0.0,
            0.0,
            20.0,
            20.0,
            Fill::Solid(Color::CYAN),
            OutlineStyle::cut_contour(),
        );
        rect.perspective = Some(
            Perspective3x3::from_rect_to_quad(
                Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0),
                &[
                    Point2D::new(100.0, 100.0),
                    Point2D::new(140.0, 100.0),
                    Point2D::new(140.0, 140.0),
                    Point2D::new(100.0, 140.0),
                ],
            )
            .unwrap(),
        );
        let oid = doc.add_object(rect, Some(lid)).unwrap();

        let metrics = compute_production_metrics(&doc, None);
        let plan = plan_laser_toolpath(&doc, 0, None, &LaserJobConfig::default()).unwrap();
        let cut = generate_cut_contour(&mut doc, &[oid], 0.0, "CutContour").unwrap();
        let cut_bb = doc.world_object_bounds(cut.object_id).unwrap();

        assert!(
            (cut_bb.min_x - 100.0).abs() < 0.2
                && (cut_bb.min_y - 100.0).abs() < 0.2
                && (cut_bb.max_x - 140.0).abs() < 0.2
                && (cut_bb.max_y - 140.0).abs() < 0.2,
            "CutContour bounds {cut_bb:?} must match warped (100..140, 100..140)"
        );
        assert!(
            (cut.cut_length_mm - 160.0).abs() < 0.5
                && (cut.cut_length_mm - plan.total_cut_mm).abs() < 0.5
                && (cut.cut_length_mm - metrics.cut_length_mm).abs() < 0.5,
            "CutContour length {:.2} must match laser {:.2} and metrics {:.2}",
            cut.cut_length_mm,
            plan.total_cut_mm,
            metrics.cut_length_mm
        );
    }

    // 2. Group with Effect::Perspective wrapping child shape
    {
        let mut doc = Document::new_a4("CutContourGroupEffPersp");
        let lid = doc.pages()[0].layers[0].id;
        let child = make_rect(
            0.0,
            0.0,
            20.0,
            20.0,
            Fill::Solid(Color::MAGENTA),
            OutlineStyle::none(),
        );
        let cid = doc.add_object(child, Some(lid)).unwrap();
        let gid = doc.group_objects(&[cid], "EffGrp").unwrap();
        let mut hist = CommandHistory::default();
        hist.execute(
            &mut doc,
            CommandOp::AddEffect {
                id: gid,
                effect: Effect::Perspective {
                    quad: [
                        Point2D::new(100.0, 100.0),
                        Point2D::new(140.0, 100.0),
                        Point2D::new(140.0, 140.0),
                        Point2D::new(100.0, 140.0),
                    ],
                },
            },
        )
        .unwrap();

        let cut = generate_cut_contour(&mut doc, &[cid], 0.0, "CutContour").unwrap();
        let cut_bb = doc.world_object_bounds(cut.object_id).unwrap();
        assert!(
            (cut_bb.min_x - 100.0).abs() < 0.2
                && (cut_bb.min_y - 100.0).abs() < 0.2
                && (cut_bb.max_x - 140.0).abs() < 0.2
                && (cut_bb.max_y - 140.0).abs() < 0.2,
            "CutContour on child inside Group(Effect::Perspective) must be warped (100..140), got {cut_bb:?}"
        );
        assert!(
            (cut.cut_length_mm - 160.0).abs() < 0.5,
            "expected cut_length_mm ~ 160.0, got {}",
            cut.cut_length_mm
        );
    }
}

#[test]
fn w5a_coree_prod_pdf417_quad_persp_text_effects_preflight() {
    // 1. #20g PDF417 > 1063 payload returns Err cleanly instead of panicking on underflow
    let huge_payload = "A".repeat(2500);
    let pdf417_err = generate_barcode(
        BarcodeSymbology::Pdf417,
        &huge_payload,
        &BarcodeOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(pdf417_err, coree_prod::ProdError::BarcodeError(_)));

    // 2. #20i CMYK preflight detects RGB color inside Effect::DropShadow
    let mut cmyk_doc = Document::new("CmykFxPreflight", 100.0, 100.0, ColorMode::Cmyk, 300.0);
    let mut rect = make_rect(
        10.0,
        10.0,
        30.0,
        30.0,
        Fill::Solid(Color::cmyk(1.0, 0.0, 0.0, 0.0)),
        OutlineStyle::none(),
    );
    rect.effects.push(coree_core::Effect::DropShadow {
        dx_mm: 2.0,
        dy_mm: 2.0,
        blur_mm: 1.5,
        opacity: 0.6,
        color: Color::rgba8(255, 0, 0, 255),
        feather_direction: coree_core::FeatherDirection::Average,
    });
    cmyk_doc.add_object(rect, None).unwrap();
    let report = run_preflight(&cmyk_doc, &PreflightRules::default());
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.kind == PreflightIssueKind::RgbInCmykDocument),
        "CMYK preflight must flag RGB color in DropShadow effect"
    );

    // 3. #20h Separation applies obj.perspective to text separation and exports plates PDF cleanly
    let mut sep_doc = Document::new("SepQuadPersp", 120.0, 120.0, ColorMode::Cmyk, 150.0);
    let mut ptext = SceneObject::new(
        ObjectId(0),
        "PerspText",
        NodePayload::ArtisticText(ArtisticText::from_plain(
            "HI",
            TextStyle {
                fill: Color::cmyk(0.0, 1.0, 0.0, 0.0),
                font_size_pt: 24.0,
                ..TextStyle::default()
            },
        )),
    );
    ptext.perspective = coree_core::Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(0.0, 0.0, 20.0, 10.0),
        &[
            Point2D::new(60.0, 60.0),
            Point2D::new(100.0, 60.0),
            Point2D::new(100.0, 90.0),
            Point2D::new(60.0, 90.0),
        ],
    );
    sep_doc.add_object(ptext, None).unwrap();

    let (sep_pdf_bytes, _) =
        coree_prod::export_separated_plates_pdf_bytes(&sep_doc, 0, &SeparationOptions::default())
            .unwrap();
    assert!(!sep_pdf_bytes.is_empty());

    let sep_res = compute_separations(&sep_doc, 0, &SeparationOptions::default()).unwrap();
    let magenta = sep_res
        .plates
        .iter()
        .find(|p| p.channel == SeparationChannel::Magenta)
        .unwrap();
    let idx = |x_mm: f64, y_mm: f64| -> usize {
        let px = ((x_mm * f64::from(magenta.width)) / 120.0).round() as usize;
        let py = ((y_mm * f64::from(magenta.height)) / 120.0).round() as usize;
        py.min(magenta.height as usize - 1) * magenta.width as usize
            + px.min(magenta.width as usize - 1)
    };
    let mut max_warped = 0u8;
    for xm in [70.0, 80.0, 90.0] {
        for ym in [68.0, 75.0, 82.0] {
            max_warped = max_warped.max(magenta.ink_map[idx(xm, ym)]);
        }
    }
    assert!(
        max_warped > 50,
        "perspective ArtisticText must render inside warped quad [60..100,60..90]"
    );
    assert_eq!(
        magenta.ink_map[idx(5.0, 5.0)],
        0,
        "perspective ArtisticText must not render at unwarped origin (5,5)"
    );
}

#[test]
fn tier5_rhai_sandbox_infinite_loop_and_deep_cql() {
    // 1. Rhai infinite loop must terminate via max_operations with atomic rollback (zero doc mutation)
    let mut doc = Document::new("Sandbox", 200.0, 200.0, ColorMode::Rgb, 300.0);
    let rid = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "OriginalBox",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 10.0, 30.0, 30.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::CYAN)),
            None,
        )
        .unwrap();
    let err = run_macro(
        &mut doc,
        "set_fill_rgb(all_object_ids(), 1.0, 0.0, 0.0); loop {}",
        &std::collections::BTreeMap::new(),
        &coree_prod::MacroSandboxConfig::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, coree_prod::ProdError::MacroError(_)),
        "expected MacroError on infinite loop sandbox abort, got {err:?}"
    );
    assert_eq!(
        doc.object(rid).unwrap().fill,
        Fill::Solid(Color::CYAN),
        "aborted macro must atomically roll back partial fill mutation"
    );

    // 2. 5,000-level nested CQL parentheses must reject cleanly via MAX_CQL_DEPTH without stack overflow
    let deep_cql = format!("{}name = \"X\"{}", "(".repeat(5000), ")".repeat(5000));
    let cql_err = coree_prod::parse_cql_query(&deep_cql).unwrap_err();
    assert!(
        matches!(cql_err, coree_prod::ProdError::InvalidParameter(_)),
        "expected InvalidParameter on 5,000-deep CQL nesting, got {cql_err:?}"
    );
}

#[test]
fn tier5_print_merge_streaming_malformed_quotes_and_10k_rows() {
    let mut template = Document::new("Merge10k", 100.0, 60.0, ColorMode::Rgb, 300.0);
    template
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "BadgeField",
                NodePayload::ArtisticText(coree_core::ArtisticText::from_plain(
                    "Hi {NAME}!",
                    coree_core::TextStyle::default(),
                )),
            ),
            None,
        )
        .unwrap();

    // 1. Streaming 10,000 rows + mismatched column count finishes cleanly
    let mut csv = String::with_capacity(220_000);
    csv.push_str("NAME,CODE\n");
    for i in 0..9999 {
        csv.push_str(&format!("User_{i},C{i:05}\n"));
    }
    csv.push_str("ShortRowOnlyOneCol\n");

    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("merge_10k.csv");
    let pdf_path = dir.path().join("merge_10k.pdf");
    std::fs::write(&csv_path, csv.as_bytes()).unwrap();

    let count = coree_prod::stream_print_merge_csv_to_pdf(
        &template,
        &csv_path,
        &pdf_path,
        &coree_prod::PrintMergeOptions::default(),
    )
    .expect("streaming CSV print merge must handle 10k rows and mismatched columns cleanly");
    assert_eq!(count, 10_000);
    let out_pdf = std::fs::read(&pdf_path).unwrap();
    assert!(out_pdf.starts_with(b"%PDF-"));

    // 2. Unterminated quoted CSV field rejects with CsvParseError (never panics or loops)
    let bad_csv_path = dir.path().join("unclosed.csv");
    let bad_pdf_path = dir.path().join("unclosed.pdf");
    std::fs::write(&bad_csv_path, b"NAME,CODE\n\"UnclosedQuoteRow,C99999\n").unwrap();
    let bad_err = coree_prod::stream_print_merge_csv_to_pdf(
        &template,
        &bad_csv_path,
        &bad_pdf_path,
        &coree_prod::PrintMergeOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(bad_err, coree_prod::ProdError::CsvParseError { .. }),
        "expected CsvParseError on unterminated quote, got {bad_err:?}"
    );
}
