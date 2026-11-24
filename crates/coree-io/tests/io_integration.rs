// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! End-to-end integration tests for `coree-io`.

use coree_core::{
    AnchorPoint, ArtisticText, BezierPath, BlendMode, Color, ColorMapMode, ColorMode, ColorPalette,
    ColorSwatch, ConnectorObject, ConnectorStyle, CornerStyle, DimensionKind, DimensionObject,
    Document, Effect, FeatherDirection, Fill, GradientStop, GuideOrientation, LensConfig, LensKind,
    LineCap, LineJoin, MasterLayerScope, NodePayload, ObjectId, OutlineStyle, ParametricShape,
    Point2D, RasterAsset, RasterLayerData, Rect2D, SceneObject, SymbolDef, TextDirection,
    TextStyle, UnknownCdrChunk, VectorTable,
};
use coree_io::{
    decode_raster_bytes_with_limits, export_dxf, export_dxf_with_options, export_eps,
    export_palette_file, export_pdf, export_pdf_to_bytes, export_plt, export_plt_with_options,
    export_raster, export_raster_to_bytes, export_svg, import_palette_file, import_raster,
    import_svg, import_svg_str_with_limits, load_coree, load_coree_from_reader,
    read_coree_manifest, read_coree_preview, save_coree, save_coree_with_preview, DxfExportOptions,
    IoError, IoLimits, PdfColorSpace, PdfExportOptions, PltExportOptions, RasterExportOptions,
    RasterFormat,
};
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn build_all_features_document() -> Document {
    let mut doc = Document::new_a4("Coree Full Integration Spec");
    doc.metadata_mut().author = "coree QA".to_owned();

    let page1_id = doc.pages()[0].id;
    let master_layer = doc
        .add_layer(page1_id, "Master Header", false, MasterLayerScope::AllPages)
        .unwrap();
    let cut_layer = doc
        .add_layer(page1_id, "CutContour", true, MasterLayerScope::None)
        .unwrap();

    // Add second page to verify multipage & master layers
    let page2_id = doc.add_page("Page 2", 210.0, 297.0, 3.0);
    let page2_layer = doc.page_by_id(page2_id).unwrap().layers[0].id;

    // Externalized raster asset
    let asset =
        RasterAsset::new(4, 4, [20, 140, 230, 200].repeat(16), 300.0, ColorMode::Cmyk).unwrap();
    let asset_id = doc.register_asset(asset);

    // Register reusable symbol
    let sym = SymbolDef::new(
        "Logo Mark",
        vec![SceneObject::new(
            ObjectId(1),
            "Star",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 10.0, 10.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::YELLOW))],
    );
    let sym_id = sym.id;
    doc.insert_symbol(sym);

    // Create 11 objects carrying ALL 11 Lenses!
    let all_lenses = [
        LensKind::Brighten { rate_pct: 25.0 },
        LensKind::ColorAdd {
            color: Color::CYAN,
            rate_pct: 40.0,
        },
        LensKind::ColorLimit {
            color: Color::MAGENTA,
            rate_pct: 60.0,
        },
        LensKind::CustomColorMap {
            from_color: Color::BLACK,
            to_color: Color::YELLOW,
            palette_mode: ColorMapMode::ForwardRainbow,
        },
        LensKind::FishEye { rate_pct: 120.0 },
        LensKind::HeatMap {
            palette_rotation_deg: 45.0,
        },
        LensKind::Invert,
        LensKind::Magnify { amount: 2.5 },
        LensKind::TintedGrayscale { tint: Color::CYAN },
        LensKind::Transparency {
            color: Color::WHITE,
            rate_pct: 35.0,
        },
        LensKind::Wireframe {
            override_outline: Some(Color::BLACK),
            override_fill: None,
        },
    ];

    let mut lens_ids = Vec::new();
    for (idx, lens_kind) in all_lenses.into_iter().enumerate() {
        let mut obj = SceneObject::new(
            ObjectId(0),
            format!("Lens {}", idx + 1),
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 12.0,
                height: 12.0,
                corner_radii: [2.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.3))
        .with_transform(coree_core::Affine2D::translate(
            (idx as f64) * 14.0 + 5.0,
            10.0,
        ));
        obj.lens = Some(LensConfig::new(lens_kind));
        if idx == 0 {
            obj.effects.push(Effect::DropShadow {
                dx_mm: 2.0,
                dy_mm: 2.0,
                blur_mm: 3.0,
                opacity: 0.45,
                color: Color::BLACK,
                feather_direction: FeatherDirection::Outside,
            });
            obj.unknown_chunks.push(UnknownCdrChunk {
                fourcc: *b"objx",
                version_hint: 2400,
                raw_bytes: vec![0x01, 0x02, 0xFE, 0xFF],
            });
        }
        lens_ids.push(doc.add_object(obj, None).unwrap());
    }

    // CutContour silhouette on cut layer
    let cut_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Sticker Diecut",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 30.0, 90.0, 50.0,
                ))),
            )
            .with_fill(Fill::LinearGradient {
                start: Point2D::new(10.0, 30.0),
                end: Point2D::new(100.0, 80.0),
                stops: vec![
                    GradientStop::new(0.0, Color::CYAN),
                    GradientStop::new(1.0, Color::MAGENTA),
                ],
            })
            .with_outline(OutlineStyle::cut_contour()),
            Some(cut_layer),
        )
        .unwrap();

    // PowerClip containing a child shape
    let clip_child = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Clipped Stripe",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    15.0, 35.0, 40.0, 40.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::YELLOW)),
            None,
        )
        .unwrap();
    let clip_host = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "PowerClip Frame",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    12.0, 32.0, 30.0, 30.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::WHITE))
            .with_outline(OutlineStyle::solid(Color::BLACK, 0.4)),
            None,
        )
        .unwrap();
    doc.create_powerclip(clip_host, &[clip_child]).unwrap();

    // Vector table with merged cells + Arabic text
    let mut tbl = VectorTable::new(2, 2, 25.0, 10.0);
    tbl.cell_mut(0, 0).unwrap().text = "البند".to_owned();
    tbl.cell_mut(0, 1).unwrap().text = "Price".to_owned();
    tbl.merge_cells(1, 0, 1, 1).unwrap();
    tbl.cell_mut(1, 0).unwrap().text = "Total".to_owned();
    let tbl_id = doc
        .add_object(
            SceneObject::new(ObjectId(0), "Price Table", NodePayload::Table(tbl))
                .with_transform(coree_core::Affine2D::translate(10.0, 95.0)),
            None,
        )
        .unwrap();

    // Dimension & connector
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Width Measurement",
            NodePayload::Dimension(DimensionObject::linear(
                DimensionKind::Horizontal,
                Point2D::new(10.0, 30.0),
                Point2D::new(100.0, 30.0),
                6.0,
            )),
        ),
        None,
    )
    .unwrap();

    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Flow Connector",
            NodePayload::Connector(ConnectorObject::between_objects(
                cut_id,
                AnchorPoint::Bottom,
                tbl_id,
                AnchorPoint::Top,
                ConnectorStyle::RightAngle,
            )),
        ),
        None,
    )
    .unwrap();

    // Arabic & English texts + Symbol + Raster on Master & Page 2
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Arabic Headline",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "كوري للطباعة والتصميم",
                TextStyle {
                    direction: TextDirection::Auto,
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(coree_core::Affine2D::translate(15.0, 130.0)),
        Some(master_layer),
    )
    .unwrap();

    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "English Headline",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "COREE PRINT READY",
                TextStyle {
                    bold: true,
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(coree_core::Affine2D::translate(15.0, 150.0)),
        Some(master_layer),
    )
    .unwrap();

    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Logo Instance",
            NodePayload::SymbolInstance(sym_id),
        )
        .with_transform(coree_core::Affine2D::translate(170.0, 15.0)),
        Some(master_layer),
    )
    .unwrap();

    let mut photo = SceneObject::new(
        ObjectId(0),
        "Embedded Photo",
        NodePayload::Raster(RasterLayerData {
            asset_id,
            width_mm: 50.0,
            height_mm: 50.0,
            filters: Vec::new(),
            crop_rect: None,
        }),
    )
    .with_transform(coree_core::Affine2D::translate(20.0, 20.0));
    photo.opacity = 0.85;
    photo.blend_mode = BlendMode::Multiply;
    doc.add_object(photo, Some(page2_layer)).unwrap();

    // Group two of the lens objects
    doc.group_objects(&[lens_ids[0], lens_ids[1]], "Lens Pair")
        .unwrap();

    doc.add_guide(GuideOrientation::Vertical(105.0), true);
    doc.push_unknown_doc_chunk(UnknownCdrChunk {
        fourcc: *b"vrsn",
        version_hint: 2400,
        raw_bytes: vec![0xDE, 0xAD, 0xBE, 0xEF],
    });
    doc
}

#[test]
fn native_coree_preserves_all_11_lenses_effects_tables_chunks_assets_and_preview() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("complete.coree");
    let doc = build_all_features_document();
    let preview = export_raster_to_bytes(
        4,
        4,
        &[10, 20, 30, 255].repeat(16),
        &RasterExportOptions::new(RasterFormat::Png, 96.0),
    )
    .unwrap();

    save_coree_with_preview(&doc, Some(&preview), &file_path).unwrap();
    let manifest = read_coree_manifest(&file_path).unwrap();
    assert_eq!(manifest.page_count, 2);
    assert_eq!(manifest.asset_count, 1);
    assert!(manifest.has_preview);
    assert_eq!(read_coree_preview(&file_path).unwrap().unwrap(), preview);

    let loaded = load_coree(&file_path).unwrap();
    assert_eq!(loaded, doc);

    // Also test plain save_coree without preview
    let no_prev = dir.path().join("no_preview.coree");
    save_coree(&doc, &no_prev).unwrap();
    assert!(read_coree_preview(&no_prev).unwrap().is_none());
    assert_eq!(load_coree(&no_prev).unwrap(), doc);
}

#[test]
fn zip_slip_and_zip_bomb_attacks_are_blocked() {
    // 1. Windows backslash traversal
    for malicious_name in [
        "../../etc/shadow",
        "/etc/passwd",
        "assets\\..\\..\\pwn.png",
        "C:/Windows/win.ini",
    ] {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            zip.start_file(malicious_name, SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"payload").unwrap();
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        let err = load_coree_from_reader(cursor, &IoLimits::default()).unwrap_err();
        assert!(
            matches!(err, IoError::ZipSlip { .. }),
            "expected ZipSlip for {malicious_name}, got {err:?}"
        );
    }

    // 2. Deflate Zip-Bomb (3 MiB zeroes -> ~3 KiB compressed = ~1000x ratio)
    let mut bomb = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut bomb);
        zip.start_file(
            "document.json",
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&vec![0u8; 3 * 1024 * 1024]).unwrap();
        zip.finish().unwrap();
    }
    bomb.set_position(0);
    assert!(matches!(
        load_coree_from_reader(bomb, &IoLimits::default()),
        Err(IoError::ZipBomb { .. })
    ));

    // 3. Entry-count bomb
    let mut many = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut many);
        for i in 0..25 {
            zip.start_file(format!("f{i}.txt"), SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"x").unwrap();
        }
        zip.finish().unwrap();
    }
    many.set_position(0);
    let tight = IoLimits {
        max_zip_entries: 10,
        ..IoLimits::default()
    };
    assert!(matches!(
        load_coree_from_reader(many, &tight),
        Err(IoError::ZipBomb { .. })
    ));
}

#[test]
fn svg_pdf_eps_dxf_plt_all_export_valid_industry_files_from_complex_document() {
    let dir = tempfile::tempdir().unwrap();
    let doc = build_all_features_document();

    // 1. SVG export + re-import from disk
    let svg_path = dir.path().join("page1.svg");
    export_svg(&doc, 0, &svg_path).unwrap();
    let svg_content = std::fs::read_to_string(&svg_path).unwrap();
    assert!(svg_content.contains("data-layer=\"CutContour\""));
    assert!(svg_content.contains("direction=\"rtl\""));
    assert!(svg_content.contains("<clipPath"));
    let reimported = import_svg(&svg_path).unwrap();
    assert!(!reimported.objects().is_empty());

    // Verify SVG depth bomb rejection
    let deep_svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\">{}<rect width=\"1\" height=\"1\"/>{}</svg>",
        "<g>".repeat(50),
        "</g>".repeat(50)
    );
    assert!(matches!(
        import_svg_str_with_limits(
            &deep_svg,
            &IoLimits {
                max_svg_depth: 20,
                ..IoLimits::default()
            }
        ),
        Err(IoError::LimitExceeded { .. })
    ));

    // 2. Print-ready PDF 1.7 export + byte-exact xref verification for EVERY object
    let pdf_path = dir.path().join("print.pdf");
    export_pdf(&doc, &PdfExportOptions::print_ready(), &pdf_path).unwrap();
    let pdf_bytes = std::fs::read(&pdf_path).unwrap();
    assert!(pdf_bytes.starts_with(b"%PDF-1.7\n"));
    let pdf_str = String::from_utf8_lossy(&pdf_bytes);
    assert!(pdf_str.contains("/Count 2"));
    assert!(pdf_str.contains("W n\n")); // PowerClip clipping operator
    let startxref_marker = b"startxref\n";
    let startxref_pos = pdf_bytes
        .windows(startxref_marker.len())
        .rposition(|w| w == startxref_marker)
        .unwrap();
    let tail_str =
        std::str::from_utf8(&pdf_bytes[startxref_pos + startxref_marker.len()..]).unwrap();
    let xref_offset: usize = tail_str.lines().next().unwrap().trim().parse().unwrap();
    let xref_section = std::str::from_utf8(&pdf_bytes[xref_offset..startxref_pos]).unwrap();
    let mut lines = xref_section.lines();
    assert_eq!(lines.next().unwrap(), "xref");
    let header_parts: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
    let obj_total: usize = header_parts[1].parse().unwrap();
    assert!(obj_total >= 8);
    assert_eq!(lines.next().unwrap(), "0000000000 65535 f ");
    for obj_num in 1..obj_total {
        let entry = lines.next().unwrap();
        assert_eq!(
            entry.len(),
            19,
            "xref entry must be 19 chars + LF = 20 bytes"
        );
        let off: usize = entry[..10].parse().unwrap();
        let expected_prefix = format!("{obj_num} 0 obj");
        assert!(
            pdf_bytes[off..].starts_with(expected_prefix.as_bytes()),
            "xref entry for obj {obj_num} at offset {off} does not start with '{expected_prefix}'"
        );
    }

    // Also verify DeviceRGB mode
    let rgb_pdf = export_pdf_to_bytes(
        &doc,
        &PdfExportOptions {
            color_space: PdfColorSpace::DeviceRgb,
            ..PdfExportOptions::default()
        },
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&rgb_pdf).contains(" rg\n"));

    // 3. EPS Level 3 export
    let eps_path = dir.path().join("page1.eps");
    export_eps(&doc, 0, &eps_path).unwrap();
    let eps = std::fs::read_to_string(&eps_path).unwrap();
    assert!(eps.starts_with("%!PS-Adobe-3.0 EPSF-3.0\n"));
    assert!(eps.contains("%%BoundingBox:"));
    assert!(eps.contains("setcmykcolor"));
    assert!(eps.contains("clip newpath"));
    assert!(eps.ends_with("%%EOF\n"));

    // 4. DXF export (standard + cut-layers-only flattened)
    let dxf_path = dir.path().join("panel.dxf");
    export_dxf(&doc, 0, &dxf_path).unwrap();
    let dxf = std::fs::read_to_string(&dxf_path).unwrap();
    assert!(dxf.contains("SECTION\n2\nHEADER\n"));
    assert!(dxf.contains("2\nCutContour\n"));
    assert!(dxf.contains("0\nSPLINE\n"));
    assert!(dxf.contains("0\nLWPOLYLINE\n"));
    assert!(dxf.ends_with("0\nEOF\n"));

    let dxf_cut_path = dir.path().join("cut_only.dxf");
    export_dxf_with_options(
        &doc,
        0,
        &DxfExportOptions {
            flatten_curves: true,
            flatten_tolerance_mm: 0.1,
            cut_layers_only: true,
        },
        &dxf_cut_path,
    )
    .unwrap();
    let dxf_cut = std::fs::read_to_string(&dxf_cut_path).unwrap();
    assert!(!dxf_cut.contains("0\nSPLINE\n"));
    assert!(dxf_cut.contains("8\nCutContour\n"));

    // 5. PLT (HPGL) export
    let plt_path = dir.path().join("cut.plt");
    export_plt(&doc, 0, &plt_path).unwrap();
    let plt = std::fs::read_to_string(&plt_path).unwrap();
    assert!(plt.starts_with("IN;SP1;"));
    assert!(plt.ends_with("SP0;\n"));

    let plt_cut_path = dir.path().join("cut_only.plt");
    export_plt_with_options(
        &doc,
        0,
        &PltExportOptions {
            flatten_tolerance_mm: 0.05,
            cut_layers_only: true,
        },
        &plt_cut_path,
    )
    .unwrap();
    let plt_cut = std::fs::read_to_string(&plt_cut_path).unwrap();
    assert!(plt_cut.contains("PU400,")); // x = 10mm * 40 = 400 HPGL units
}

#[test]
fn raster_and_palette_roundtrip_integration() {
    let dir = tempfile::tempdir().unwrap();

    // Raster 600 DPI PNG + WebP + TIFF + BMP + JPEG
    let rgba = [40u8, 120, 200, 255].repeat(16);
    for (fmt, dpi) in [
        (RasterFormat::Png, 600.0),
        (RasterFormat::Jpeg, 300.0),
        (RasterFormat::WebP, 150.0),
        (RasterFormat::Tiff, 300.0),
        (RasterFormat::Bmp, 96.0),
    ] {
        let p = dir.path().join(format!("out.{}", fmt.extension()));
        export_raster(4, 4, &rgba, &RasterExportOptions::new(fmt, dpi), &p).unwrap();
        let asset = import_raster(&p).unwrap();
        assert_eq!((asset.width, asset.height), (4, 4));
        if fmt == RasterFormat::Png {
            assert!((asset.source_dpi - 600.0).abs() <= 1.0);
        }
    }

    // Enforce max_raster_dimension
    let png_bytes = export_raster_to_bytes(
        4,
        4,
        &rgba,
        &RasterExportOptions::new(RasterFormat::Png, 96.0),
    )
    .unwrap();
    assert!(matches!(
        decode_raster_bytes_with_limits(
            &png_bytes,
            &IoLimits {
                max_raster_dimension: 2,
                ..IoLimits::default()
            }
        ),
        Err(IoError::LimitExceeded { .. })
    ));

    // Palettes (.gpl, .ase, .pal)
    let mut pal = ColorPalette::new("Packaging Swatches", ColorMode::Cmyk);
    pal.add_swatch(ColorSwatch::new("Process Cyan", Color::CYAN));
    pal.add_swatch(ColorSwatch::new("Process Magenta", Color::MAGENTA));
    pal.add_swatch(ColorSwatch::new(
        "Brand Orange",
        Color::rgba8(245, 110, 20, 255),
    ));
    pal.add_swatch(ColorSwatch::new("Neutral Gray", Color::gray(0.4)));
    pal.add_swatch(ColorSwatch::new("CutContour", Color::cut_contour_spot()));

    for ext in ["gpl", "ase", "pal"] {
        let p = dir.path().join(format!("swatches.{ext}"));
        export_palette_file(&pal, &p).unwrap();
        let back = import_palette_file(&p).unwrap();
        assert_eq!(back.swatches.len(), 5);
        if ext == "ase" {
            assert!(matches!(
                &back.swatches[4].color,
                Color::Spot { name, .. } if name == "CutContour"
            ));
        }
    }

    // Verify stroke cap/join/dash preserved in SVG roundtrip
    let mut sdoc = Document::new("Stroke Test", 50.0, 50.0, ColorMode::Rgb, 96.0);
    let mut outline = OutlineStyle::solid(Color::rgba8(0, 128, 0, 255), 2.0);
    outline.cap = LineCap::Square;
    outline.join = LineJoin::Round;
    outline.dash_pattern = vec![3.0, 1.5];
    sdoc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Dashed",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(5.0, 5.0),
                Point2D::new(45.0, 45.0),
            )),
        )
        .with_outline(outline),
        None,
    )
    .unwrap();
    let spath = dir.path().join("stroke.svg");
    export_svg(&sdoc, 0, &spath).unwrap();
    let sback = import_svg(&spath).unwrap();
    let obj = sback.objects().values().next().unwrap();
    assert_eq!(obj.outline.cap, LineCap::Square);
    assert_eq!(obj.outline.join, LineJoin::Round);
    assert_eq!(obj.outline.dash_pattern.len(), 2);
}

#[test]
fn regression_1_coree_png_amplification_duplicate_asset_path_and_forged_compressed_size() {
    let mut doc = Document::new_a4("Budget Guard");
    let manifest_json = serde_json::to_vec(&coree_io::CoreeManifest {
        format_version: coree_io::COREE_FORMAT_VERSION,
        app: "coree".into(),
        app_version: "0.1.0".into(),
        document_id: doc.id(),
        title: doc.title().to_owned(),
        created_at: doc.metadata().created_at,
        modified_at: doc.metadata().modified_at,
        page_count: 1,
        asset_count: 1,
        has_preview: false,
    })
    .unwrap();
    let doc_json = serde_json::to_vec(&doc).unwrap();
    let tiny_png = export_raster_to_bytes(
        16,
        16,
        &[10u8, 20, 30, 255].repeat(256),
        &RasterExportOptions::new(RasterFormat::Png, 96.0),
    )
    .unwrap();

    // 1a. Asset path escaping `assets/` prefix must be rejected with ZipSlip
    {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let opts = SimpleFileOptions::default();
            zip.start_file("manifest.json", opts).unwrap();
            zip.write_all(&manifest_json).unwrap();
            zip.start_file("document.json", opts).unwrap();
            zip.write_all(&doc_json).unwrap();
            zip.start_file("assets/manifest.json", opts).unwrap();
            let bad_meta = format!(
                r#"[{{"id":"{}","width":16,"height":16,"source_dpi":96.0,"color_mode":"Rgb","path":"manifest.json"}}]"#,
                coree_core::ImageAssetId::new().0
            );
            zip.write_all(bad_meta.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        assert!(matches!(
            load_coree_from_reader(cursor, &IoLimits::default()),
            Err(IoError::ZipSlip { .. })
        ));
    }

    // 1b. Duplicate reference to same PNG in assets/manifest.json must be rejected
    {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zip.start_file("manifest.json", opts).unwrap();
            zip.write_all(&manifest_json).unwrap();
            zip.start_file("document.json", opts).unwrap();
            zip.write_all(&doc_json).unwrap();
            zip.start_file("assets/shared.png", opts).unwrap();
            zip.write_all(&tiny_png).unwrap();
            zip.start_file("assets/manifest.json", opts).unwrap();
            let dup_meta = format!(
                r#"[
                    {{"id":"{}","width":16,"height":16,"source_dpi":96.0,"color_mode":"Rgb","path":"assets/shared.png"}},
                    {{"id":"{}","width":16,"height":16,"source_dpi":96.0,"color_mode":"Rgb","path":"assets/shared.png"}}
                ]"#,
                coree_core::ImageAssetId::new().0,
                coree_core::ImageAssetId::new().0
            );
            zip.write_all(dup_meta.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        assert!(matches!(
            load_coree_from_reader(cursor, &IoLimits::default()),
            Err(IoError::InvalidFormat { .. })
        ));
    }

    // 1c. Multiple solid PNGs amplifying decoded RGBA RAM beyond cumulative budget must be rejected
    for _ in 0..4 {
        let a = RasterAsset::new(
            64,
            64,
            [255u8, 0, 0, 255].repeat(64 * 64),
            96.0,
            ColorMode::Rgb,
        )
        .unwrap();
        doc.register_asset(a);
    }
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("multi_png.coree");
    save_coree(&doc, &p).unwrap();
    // 4 * 64 * 64 = 16_384 pixels = 65_536 RGBA bytes; cap cumulative pixels at 10_000 and RGBA bytes at 40_000
    let tight_px = IoLimits {
        max_raster_pixels: 10_000,
        ..IoLimits::default()
    };
    assert!(matches!(
        coree_io::load_coree_with_limits(&p, &tight_px),
        Err(IoError::LimitExceeded { .. })
    ));
    let tight_ram = IoLimits {
        max_uncompressed_bytes: 40_000,
        ..IoLimits::default()
    };
    assert!(matches!(
        coree_io::load_coree_with_limits(&p, &tight_ram),
        Err(IoError::ZipBomb { .. })
    ));

    // 1d. Forged assets/manifest.json declaring 1x1 for a 16x16 PNG must be rejected BEFORE decode
    {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zip.start_file("manifest.json", opts).unwrap();
            zip.write_all(&manifest_json).unwrap();
            zip.start_file("document.json", opts).unwrap();
            zip.write_all(&doc_json).unwrap();
            zip.start_file("assets/lying.png", opts).unwrap();
            zip.write_all(&tiny_png).unwrap();
            zip.start_file("assets/manifest.json", opts).unwrap();
            let lying_meta = format!(
                r#"[{{"id":"{}","width":1,"height":1,"source_dpi":96.0,"color_mode":"Rgb","path":"assets/lying.png"}}]"#,
                coree_core::ImageAssetId::new().0
            );
            zip.write_all(lying_meta.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        assert!(matches!(
            load_coree_from_reader(cursor, &IoLimits::default()),
            Err(IoError::InvalidFormat { .. })
        ));
    }

    // 1e. Forged ZIP compressed_size larger than archive length must be rejected with ZipBomb
    {
        let mut raw_zip = std::fs::read(&p).unwrap();
        // Patch the first Central Directory File Header (`PK\x01\x02`) compressed_size (+20..+24)
        let cd_pos = raw_zip
            .windows(4)
            .position(|w| w == b"PK\x01\x02")
            .expect("central directory header");
        raw_zip[cd_pos + 20..cd_pos + 24].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
        assert!(matches!(
            load_coree_from_reader(Cursor::new(raw_zip), &IoLimits::default()),
            Err(IoError::ZipBomb { .. })
        ));
    }
}

#[test]
fn regression_2_svgz_ratio_bomb_text_no_double_scaling_cut_layer_and_grouped_world_paths() {
    use coree_core::{Affine2D, TextOnPath};
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let dir = tempfile::tempdir().unwrap();

    // 2a. SVGZ compression ratio bomb (>100x) must be rejected with ZipBomb
    let svgz_path = dir.path().join("bomb.svgz");
    {
        let mut gz = GzEncoder::new(Vec::new(), Compression::best());
        gz.write_all(b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\">")
            .unwrap();
        gz.write_all(&vec![b' '; 2 * 1024 * 1024]).unwrap();
        gz.write_all(b"</svg>").unwrap();
        let compressed = gz.finish().unwrap();
        std::fs::write(&svgz_path, compressed).unwrap();
    }
    assert!(matches!(
        coree_io::import_svg_with_limits(&svgz_path, &IoLimits::default()),
        Err(IoError::ZipBomb { .. })
    ));

    // 2b. Pixel-unit SVG <text> must NOT be double-scaled (16px == 12pt at 96 DPI, scale == 1.0)
    let px_svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96">
        <g id="wood" data-layer="Woodcut">
            <rect x="0" y="0" width="20" height="20" fill="#ff0000"/>
        </g>
        <g id="laser" data-layer="LaserPath" data-cut-layer="true">
            <rect x="0" y="0" width="10" height="10" stroke="#ff00ff" fill="none"/>
        </g>
        <text x="48" y="48" font-size="16" font-family="DejaVu Sans">Hello</text>
    </svg>"##;
    let imported = coree_io::import_svg_str(px_svg).unwrap();

    // 2c. Cut-layer detection must NOT false-positive on "Woodcut" and MUST honor data-cut-layer="true"
    let wood_layer = imported.pages()[0]
        .layers
        .iter()
        .find(|l| l.name == "Woodcut")
        .expect("Woodcut layer");
    assert!(
        !wood_layer.is_cut_layer,
        "Woodcut must not be falsely tagged as cut layer"
    );
    let laser_layer = imported.pages()[0]
        .layers
        .iter()
        .find(|l| l.name == "LaserPath")
        .expect("LaserPath layer");
    assert!(
        laser_layer.is_cut_layer,
        "data-cut-layer=\"true\" must set is_cut_layer"
    );

    // Verify imported <text> is 12pt (~4.233mm) with unit transform scale (not scale^2)
    let text_obj = imported
        .objects()
        .values()
        .find(|o| matches!(o.payload, NodePayload::ArtisticText(_)))
        .expect("imported text object");
    if let NodePayload::ArtisticText(ref t) = text_obj.payload {
        let pt = t.runs[0].style.font_size_pt;
        assert!(
            (pt - 12.0).abs() < 0.5,
            "expected ~12pt for 16px SVG text, got {pt}"
        );
    }
    let sx = text_obj.transform.m[0].hypot(text_obj.transform.m[1]);
    assert!(
        (sx - 1.0).abs() < 1e-4,
        "expected unscaled text transform (1.0), got {sx}"
    );

    // 2d. Grouped Connector & TextOnPath must NOT double-apply enclosing group transform
    let mut doc = Document::new("Grouped World", 200.0, 200.0, ColorMode::Rgb, 96.0);
    let host_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Host",
                NodePayload::Path(BezierPath::from_line(
                    Point2D::new(50.0, 10.0),
                    Point2D::new(90.0, 10.0),
                )),
            ),
            None,
        )
        .unwrap();
    let conn_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Link",
                NodePayload::Connector(ConnectorObject {
                    from_obj: None,
                    from_anchor: AnchorPoint::Center,
                    from_fallback: Point2D::new(50.0, 20.0),
                    to_obj: None,
                    to_anchor: AnchorPoint::Center,
                    to_fallback: Point2D::new(90.0, 20.0),
                    style: ConnectorStyle::Straight,
                    corner_radius_mm: 0.0,
                }),
            )
            .with_outline(OutlineStyle::solid(Color::BLACK, 0.5)),
            None,
        )
        .unwrap();
    let top_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "PathText",
                NodePayload::TextOnPath(TextOnPath {
                    text: ArtisticText::from_plain("Arc", TextStyle::default()),
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
    let grp_id = doc.group_objects(&[conn_id, top_id], "Shifted").unwrap();
    doc.set_object_transform(grp_id, Affine2D::translate(50.0, 0.0))
        .unwrap();

    let svg = coree_io::export_svg_to_string(&doc, 0).unwrap();
    // Inside `<g transform="matrix(1 0 0 1 50 0)">`, world x=50..90 must be emitted in local coords x=0..40!
    assert!(
        svg.contains("M 0.0000 20.0000 L 40.0000 20.0000"),
        "connector inside translated group was double-transformed: {svg}"
    );
    assert!(
        svg.contains("M 0.0000 10.0000 L 40.0000 10.0000"),
        "textPath host inside translated group was double-transformed: {svg}"
    );
}

#[test]
fn regression_3_master_layer_z_order_rotated_text_eps_arabic_and_png_checked_ihdr() {
    use coree_core::Affine2D;

    let mut doc = Document::new("ZOrder", 100.0, 100.0, ColorMode::Cmyk, 300.0);
    // Page 1 artwork
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "P1 Art",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                5.0, 5.0, 20.0, 20.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::CYAN)),
        None,
    )
    .unwrap();
    // Rotated ASCII text + Arabic RTL text on Page 1
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Rotated",
            NodePayload::ArtisticText(ArtisticText::from_plain("Spin", TextStyle::default())),
        )
        .with_transform(Affine2D::translate(30.0, 30.0).compose(&Affine2D::rotate_deg(45.0))),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Arabic",
            NodePayload::ArtisticText(ArtisticText::from_plain("مرحبا", TextStyle::default())),
        )
        .with_transform(Affine2D::translate(10.0, 60.0)),
        None,
    )
    .unwrap();

    // Page 2 with a MasterLayerScope::AllPages layer
    let p2_id = doc.add_page("Page 2", 100.0, 100.0, 0.0);
    let p2_master = doc
        .add_layer(p2_id, "GlobalMaster", false, MasterLayerScope::AllPages)
        .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Master Bg",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 100.0, 100.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::YELLOW)),
        Some(p2_master),
    )
    .unwrap();

    // Verify Master layer from Page 2 is emitted BEFORE Page 1's own Layer 1 across SVG, EPS, DXF, PDF
    let svg = coree_io::export_svg_to_string(&doc, 0).unwrap();
    assert!(
        svg.find("data-layer=\"GlobalMaster\"").unwrap()
            < svg.find("data-layer=\"Layer 1\"").unwrap(),
        "SVG must emit master layers before page layers"
    );

    let eps = coree_io::export_eps_to_string(&doc, 0).unwrap();
    assert!(
        eps.find("% Layer: GlobalMaster").unwrap() < eps.find("% Layer: Layer 1").unwrap(),
        "EPS must emit master layers before page layers"
    );
    assert!(
        eps.contains("concat /Helvetica"),
        "EPS must apply text transform"
    );
    // Arabic RTL text in EPS must emit vector outline fills (5 non-space Arabic chars -> multiple `fill`s)
    assert!(
        eps.matches("fill\n").count() >= 5,
        "EPS must emit vector outline fallback for Arabic text"
    );

    let dxf = coree_io::export_dxf_to_string(&doc, 0).unwrap();
    let entities = &dxf[dxf.find("ENTITIES").unwrap()..];
    assert!(
        entities.find("8\nGlobalMaster\n").unwrap() < entities.find("8\nLayer_1\n").unwrap(),
        "DXF must emit master layers before page layers"
    );

    let pdf_bytes = export_pdf_to_bytes(&doc, &PdfExportOptions::default()).unwrap();
    let pdf_str = String::from_utf8_lossy(&pdf_bytes);
    assert!(
        pdf_str.contains(" Tm (Spin) Tj"),
        "PDF must emit Tm matrix for rotated text"
    );
    // Yellow master bg (0 0 1 0 k) must appear before Cyan page art (1 0 0 0 k) on Page 1
    assert!(
        pdf_str.find("0.0000 0.0000 1.0000 0.0000 k").unwrap()
            < pdf_str.find("1.0000 0.0000 0.0000 0.0000 k").unwrap(),
        "PDF must emit master layers before page layers"
    );

    // Checked PNG IHDR length overflow must return InvalidFormat instead of panicking
    let mut forged_png = vec![137, 80, 78, 71, 13, 10, 26, 10];
    forged_png.extend_from_slice(&u32::MAX.to_be_bytes());
    forged_png.extend_from_slice(b"IHDR");
    forged_png.resize(40, 0);
    assert!(matches!(
        coree_io::inject_png_phys_chunk(&forged_png, 300.0),
        Err(IoError::InvalidFormat { .. })
    ));

    // Non-finite path coordinates must NEVER emit "NaN" or "inf" into SVG/PDF/EPS/DXF/PLT
    let mut bad_doc = Document::new("BadCoord Test", 100.0, 100.0, ColorMode::Rgb, 96.0);
    let mut bad_path = BezierPath::from_line(Point2D::new(5.0, 5.0), Point2D::new(25.0, 25.0));
    bad_path.subpaths[0]
        .nodes
        .push(coree_core::PathNode::corner(Point2D::new(
            f64::NAN,
            f64::INFINITY,
        )));
    bad_doc
        .add_object(
            SceneObject::new(ObjectId(0), "BadPts", NodePayload::Path(bad_path))
                .with_outline(OutlineStyle::solid(Color::BLACK, 0.5)),
            None,
        )
        .unwrap();
    let svg_bad = coree_io::export_svg_to_string(&bad_doc, 0).unwrap();
    assert!(!svg_bad.contains("NaN") && !svg_bad.contains("inf"));
    let eps_bad = coree_io::export_eps_to_string(&bad_doc, 0).unwrap();
    assert!(!eps_bad.contains("NaN") && !eps_bad.contains("inf"));
    let dxf_bad = coree_io::export_dxf_to_string(&bad_doc, 0).unwrap();
    assert!(!dxf_bad.contains("NaN") && !dxf_bad.contains("inf"));
    let plt_bad = coree_io::export_plt_to_string(&bad_doc, 0).unwrap();
    assert!(!plt_bad.contains("NaN") && !plt_bad.contains("inf"));
    let pdf_bad = export_pdf_to_bytes(&bad_doc, &PdfExportOptions::default()).unwrap();
    let pdf_bad_s = String::from_utf8_lossy(&pdf_bad);
    assert!(!pdf_bad_s.contains("NaN") && !pdf_bad_s.contains(" inf"));
}

#[test]
fn regression_4_palette_nan_inf_rejection_and_clean_room_name() {
    // Binary ASE containing NaN channel must be rejected
    let mut ase = Vec::new();
    ase.extend_from_slice(b"ASEF");
    ase.extend_from_slice(&1u16.to_be_bytes());
    ase.extend_from_slice(&0u16.to_be_bytes());
    ase.extend_from_slice(&1u32.to_be_bytes()); // 1 block
    let mut payload = Vec::new();
    payload.extend_from_slice(&2u16.to_be_bytes()); // 1 char + NUL
    payload.extend_from_slice(&('X' as u16).to_be_bytes());
    payload.extend_from_slice(&0u16.to_be_bytes());
    payload.extend_from_slice(b"GRAY");
    payload.extend_from_slice(&f32::NAN.to_be_bytes());
    payload.extend_from_slice(&2u16.to_be_bytes());
    ase.extend_from_slice(&0x0001u16.to_be_bytes());
    ase.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    ase.extend_from_slice(&payload);
    assert!(matches!(
        coree_io::import_ase(&ase),
        Err(IoError::InvalidFormat { .. })
    ));

    // Named ASCII .pal containing NaN / Inf must be rejected
    assert!(matches!(
        coree_io::import_pal("\"Bad\" NaN 0 0 0\n"),
        Err(IoError::InvalidFormat { .. })
    ));
    assert!(matches!(
        coree_io::import_pal("\"BadInf\" inf 0 0 0\n"),
        Err(IoError::InvalidFormat { .. })
    ));

    // Clean-room runtime palette name
    let pal = coree_io::import_pal("\"Ink\" 50 0 0 0\n").unwrap();
    assert_eq!(pal.name, "Named CMYK Palette");
}

#[test]
fn all_four_new_importers_roundtrip_end_to_end_via_files() {
    let dir = tempfile::tempdir().unwrap();
    let doc = build_all_features_document();

    // 1. PDF & PDF-compatible `.ai` file round-trip
    let pdf_path = dir.path().join("brochure.pdf");
    let ai_path = dir.path().join("brochure.ai");
    let pdf_opts = PdfExportOptions {
        color_space: PdfColorSpace::DeviceCmyk,
        compress_streams: true,
        ..PdfExportOptions::default()
    };
    export_pdf(&doc, &pdf_opts, &pdf_path).unwrap();
    std::fs::copy(&pdf_path, &ai_path).unwrap();

    let imported_pdf = coree_io::import_pdf(&pdf_path).unwrap();
    let imported_ai = coree_io::import_pdf(&ai_path).unwrap();
    assert_eq!(imported_pdf.pages().len(), doc.pages().len());
    assert_eq!(imported_ai.pages().len(), doc.pages().len());
    assert!(!imported_pdf.objects().is_empty());
    assert!(!imported_pdf.assets().is_empty());
    assert_eq!(
        imported_pdf.metadata().source_format.as_deref(),
        Some("pdf")
    );

    // 2. EPS file round-trip
    let eps_path = dir.path().join("page1.eps");
    export_eps(&doc, 0, &eps_path).unwrap();
    let imported_eps = coree_io::import_eps(&eps_path).unwrap();
    assert_eq!(imported_eps.pages().len(), 1);
    assert!((imported_eps.pages()[0].width_mm - 210.0).abs() < 0.2);
    assert!((imported_eps.pages()[0].height_mm - 297.0).abs() < 0.2);
    assert!(!imported_eps.objects().is_empty());
    assert_eq!(
        imported_eps.metadata().source_format.as_deref(),
        Some("eps")
    );

    // 3. DXF file round-trip
    let dxf_path = dir.path().join("cut.dxf");
    export_dxf(&doc, 0, &dxf_path).unwrap();
    let imported_dxf = coree_io::import_dxf(&dxf_path).unwrap();
    assert_eq!(imported_dxf.pages().len(), 1);
    assert!(imported_dxf.pages()[0]
        .layers
        .iter()
        .any(|l| l.name.eq_ignore_ascii_case("CutContour") && l.is_cut_layer));
    assert!(!imported_dxf.objects().is_empty());
    assert_eq!(
        imported_dxf.metadata().source_format.as_deref(),
        Some("dxf")
    );

    // 4. HPGL `.plt` file round-trip
    let plt_path = dir.path().join("cut.plt");
    export_plt(&doc, 0, &plt_path).unwrap();
    let imported_plt = coree_io::import_plt(&plt_path).unwrap();
    assert_eq!(imported_plt.pages().len(), 1);
    assert!(!imported_plt.objects().is_empty());
    assert_eq!(
        imported_plt.metadata().source_format.as_deref(),
        Some("plt")
    );

    // 5. Minimal valid PSD file import via disk path
    let psd_path = dir.path().join("flat.psd");
    let mut psd = Vec::new();
    psd.extend_from_slice(b"8BPS");
    psd.extend_from_slice(&1u16.to_be_bytes());
    psd.extend_from_slice(&[0u8; 6]);
    psd.extend_from_slice(&3u16.to_be_bytes()); // RGB
    psd.extend_from_slice(&2u32.to_be_bytes()); // h=2
    psd.extend_from_slice(&2u32.to_be_bytes()); // w=2
    psd.extend_from_slice(&8u16.to_be_bytes()); // depth=8
    psd.extend_from_slice(&3u16.to_be_bytes()); // RGB mode
    psd.extend_from_slice(&0u32.to_be_bytes()); // color mode len
    psd.extend_from_slice(&0u32.to_be_bytes()); // img resources len
    psd.extend_from_slice(&0u32.to_be_bytes()); // layer/mask len
    psd.extend_from_slice(&0u16.to_be_bytes()); // composite RAW
    psd.extend_from_slice(&[255, 255, 255, 255]); // R
    psd.extend_from_slice(&[128, 128, 128, 128]); // G
    psd.extend_from_slice(&[64, 64, 64, 64]); // B
    std::fs::write(&psd_path, &psd).unwrap();

    let imported_psd = coree_io::import_psd(&psd_path).unwrap();
    assert_eq!(imported_psd.pages().len(), 1);
    assert_eq!(imported_psd.assets().len(), 1);
    assert_eq!(
        imported_psd.metadata().source_format.as_deref(),
        Some("psd")
    );
}

#[test]
fn optional_external_real_file_smoke_check() {
    if let Ok(pdf_path) = std::env::var("COREE_VERIFY_PDF") {
        let p = std::path::Path::new(&pdf_path);
        let t0 = std::time::Instant::now();
        let doc = coree_io::import_pdf(p).expect("real PDF must import cleanly");
        let elapsed = t0.elapsed();
        assert!(!doc.pages().is_empty());
        assert!(!doc.objects().is_empty());
        eprintln!(
            "[VERIFY PDF] pages={} objects={} assets={} page0={:.2}x{:.2}mm mode={:?} elapsed={:.2?}",
            doc.pages().len(),
            doc.object_count(),
            doc.assets().len(),
            doc.pages()[0].width_mm,
            doc.pages()[0].height_mm,
            doc.color_mode(),
            elapsed
        );
    }
    if let Ok(psd_path) = std::env::var("COREE_VERIFY_PSD") {
        let p = std::path::Path::new(&psd_path);
        let t0 = std::time::Instant::now();
        let doc = coree_io::import_psd(p).expect("real PSD must import cleanly");
        let elapsed = t0.elapsed();
        assert_eq!(doc.pages().len(), 1);
        assert!(!doc.objects().is_empty());
        assert!(!doc.assets().is_empty());
        let total_rgba: usize = doc.assets().values().map(|a| a.rgba_bytes.len()).sum();
        eprintln!(
            "[VERIFY PSD] layers={} objects={} assets={} page={:.2}x{:.2}mm dpi={:.1} total_rgba={:.1}MiB elapsed={:.2?}",
            doc.pages()[0].layers.len(),
            doc.object_count(),
            doc.assets().len(),
            doc.pages()[0].width_mm,
            doc.pages()[0].height_mm,
            doc.dpi(),
            (total_rgba as f64) / (1024.0 * 1024.0),
            elapsed
        );
    }
}

#[test]
fn w2b_r2_pdf_and_eps_export_inner_shadow_bevel_extrude3d() {
    use coree_core::{BevelStyle, ExtrudeLight};

    let effects = [
        (
            "InnerShadow",
            Effect::InnerShadow {
                dx_mm: 2.0,
                dy_mm: 2.0,
                blur_mm: 3.0,
                opacity: 0.7,
                color: Color::BLACK,
                feather_direction: FeatherDirection::Average,
            },
        ),
        (
            "Bevel",
            Effect::Bevel {
                style: BevelStyle::SoftEdge,
                distance_mm: 3.0,
                softness_mm: 1.5,
                light_angle_deg: 225.0,
                altitude_deg: 45.0,
                highlight_color: Color::WHITE,
                shadow_color: Color::BLACK,
                intensity: 0.7,
            },
        ),
        (
            "Extrude3D",
            Effect::Extrude3D {
                vanishing_point: Point2D::new(80.0, 20.0),
                depth: 8.0,
                bevel_depth: 1.0,
                shade_color: Color::rgba8(40, 120, 210, 255),
                rotation_3d: [15.0, -20.0, 0.0],
                lights: vec![ExtrudeLight {
                    dir: [0.5, 0.5, 0.7],
                    intensity: 1.0,
                }],
            },
        ),
    ];

    for (label, eff) in effects {
        let mut doc = Document::new_a4(label);
        let mut obj = SceneObject::new(
            ObjectId(0),
            label,
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 40.0,
                height: 30.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgba8(60, 180, 90, 255)));
        obj.transform = coree_core::Affine2D::translate(30.0, 30.0);
        obj.effects.push(eff);
        doc.add_object(obj, None).unwrap();

        let pdf_bytes = export_pdf_to_bytes(&doc, &PdfExportOptions::default()).unwrap();
        let pdf_text = String::from_utf8_lossy(&pdf_bytes);
        assert!(
            pdf_text.contains("/DynIm1 Do"),
            "{label} PDF export must emit raster composite XObject /DynIm1 Do"
        );

        let eps_text = coree_io::export_eps_to_string(&doc, 0).unwrap();
        assert!(
            eps_text.contains("/ImageType 1") && eps_text.contains("/ASCIIHexDecode filter"),
            "{label} EPS export must emit raster composite image"
        );
    }
}

#[test]
fn w2b_r2_svg_pdf_eps_apply_perspective_to_path_payload() {
    use coree_core::Perspective3x3;

    let persp = Perspective3x3::from_unit_square_to_quad(&[
        Point2D::new(0.0, 0.0),
        Point2D::new(100.0, 15.0),
        Point2D::new(85.0, 90.0),
        Point2D::new(10.0, 80.0),
    ])
    .expect("valid perspective quad");

    let rect_path = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 1.0, 1.0));
    let mut doc_plain = Document::new_a4("PlainPath");
    doc_plain
        .add_object(
            SceneObject::new(ObjectId(0), "Plain", NodePayload::Path(rect_path.clone()))
                .with_fill(Fill::Solid(Color::rgba8(220, 40, 40, 255))),
            None,
        )
        .unwrap();

    let mut doc_persp = Document::new_a4("PerspPath");
    let mut obj_persp = SceneObject::new(ObjectId(0), "Persp", NodePayload::Path(rect_path))
        .with_fill(Fill::Solid(Color::rgba8(220, 40, 40, 255)));
    obj_persp.perspective = Some(persp);
    doc_persp.add_object(obj_persp, None).unwrap();

    let svg_plain = coree_io::export_svg_to_string(&doc_plain, 0).unwrap();
    let svg_persp = coree_io::export_svg_to_string(&doc_persp, 0).unwrap();
    assert_ne!(svg_plain, svg_persp, "SVG must reflect Path perspective");
    let imported_svg = import_svg_str_with_limits(&svg_persp, &IoLimits::default()).unwrap();
    let svg_bb = imported_svg
        .object(imported_svg.pages()[0].layers[0].objects[0])
        .and_then(|o| imported_svg.world_object_bounds(o.id))
        .expect("imported perspective SVG object bounds");
    assert!(
        svg_bb.width() > 50.0 && svg_bb.height() > 50.0,
        "expected warped perspective bounds > 50mm, got {svg_bb:?}"
    );

    let pdf_plain = export_pdf_to_bytes(&doc_plain, &PdfExportOptions::default()).unwrap();
    let pdf_persp = export_pdf_to_bytes(&doc_persp, &PdfExportOptions::default()).unwrap();
    assert_ne!(pdf_plain, pdf_persp, "PDF must reflect Path perspective");

    let eps_plain = coree_io::export_eps_to_string(&doc_plain, 0).unwrap();
    let eps_persp = coree_io::export_eps_to_string(&doc_persp, 0).unwrap();
    assert_ne!(eps_plain, eps_persp, "EPS must reflect Path perspective");
    assert!(
        eps_persp.contains("curveto") || eps_persp.contains("lineto"),
        "EPS perspective path must emit warped geometry"
    );
}

#[test]
fn w2b_r3_pdf_cutcontour_shape_overlay_applies_perspective() {
    use coree_core::{Effect, FeatherDirection, Perspective3x3};

    let persp = Perspective3x3::from_unit_square_to_quad(&[
        Point2D::new(0.0, 0.0),
        Point2D::new(120.0, 20.0),
        Point2D::new(100.0, 110.0),
        Point2D::new(15.0, 95.0),
    ])
    .expect("valid perspective quad");

    let make_doc = |with_persp: bool| {
        let mut doc = Document::new_a4("CutContourShapePersp");
        let mut obj = SceneObject::new(
            ObjectId(0),
            "CutShape",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 1.0,
                height: 1.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgba8(200, 50, 50, 255)))
        .with_outline(OutlineStyle {
            color: Color::cut_contour_spot(),
            width_mm: 0.25,
            ..OutlineStyle::default()
        });
        obj.effects.push(Effect::DropShadow {
            dx_mm: 2.0,
            dy_mm: 2.0,
            blur_mm: 3.0,
            color: Color::rgba8(0, 0, 0, 128),
            opacity: 0.5,
            feather_direction: FeatherDirection::Outside,
        });
        if with_persp {
            obj.perspective = Some(persp);
        }
        doc.add_object(obj, None).unwrap();
        doc
    };

    let opts = PdfExportOptions {
        compress_streams: false,
        ..PdfExportOptions::default()
    };
    let pdf_plain = String::from_utf8_lossy(
        &export_pdf_to_bytes(&make_doc(false), &opts).expect("plain PDF export"),
    )
    .into_owned();
    let pdf_persp = String::from_utf8_lossy(
        &export_pdf_to_bytes(&make_doc(true), &opts).expect("persp PDF export"),
    )
    .into_owned();

    // Extract the CutContour vector overlay slice after the raster composite image draw (`Do`).
    let cut_slice = |s: &str| -> String {
        let idx = s.find("/DynIm1 Do").expect("raster composite image draw");
        s[idx..].to_owned()
    };
    assert_ne!(
        cut_slice(&pdf_plain),
        cut_slice(&pdf_persp),
        "CutContour vector overlay for NodePayload::Shape must apply obj.perspective"
    );
}

#[test]
fn w2b_r4_pdf_cutcontour_scaled_perspective_respects_scale_with_object() {
    use coree_core::{Affine2D, Effect, FeatherDirection, Perspective3x3};

    let persp = Perspective3x3::from_unit_square_to_quad(&[
        Point2D::new(2.0, 1.0),
        Point2D::new(19.0, 3.0),
        Point2D::new(16.0, 14.0),
        Point2D::new(3.0, 12.0),
    ])
    .expect("valid perspective quad");

    let mut doc = Document::new(
        "CutContourScaledPersp",
        210.0,
        297.0,
        ColorMode::Cmyk,
        300.0,
    );
    let mut obj = SceneObject::new(
        ObjectId(0),
        "CutShapeScaled",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 1.0,
            height: 1.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgba8(40, 140, 210, 255)))
    .with_outline(OutlineStyle {
        color: Color::cut_contour_spot(),
        width_mm: 1.0,
        scale_with_object: true,
        ..OutlineStyle::default()
    });
    obj.transform = Affine2D::translate(10.0, 10.0).compose(&Affine2D::scale(2.0, 2.0));
    obj.perspective = Some(persp);
    obj.effects.push(Effect::InnerShadow {
        dx_mm: 1.0,
        dy_mm: 1.0,
        blur_mm: 1.5,
        opacity: 0.6,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Inside,
    });
    doc.add_object(obj, None).unwrap();

    let opts = PdfExportOptions {
        compress_streams: false,
        ..PdfExportOptions::default()
    };
    let pdf = String::from_utf8_lossy(&export_pdf_to_bytes(&doc, &opts).expect("PDF export"))
        .into_owned();
    let idx = pdf.find("/DynIm1 Do").expect("raster composite image draw");
    let cut_slice = &pdf[idx..];

    assert!(
        cut_slice.contains("/Sep1 CS 1.0000 SCN"),
        "CutContour overlay must select Spot separation colorspace: {cut_slice}"
    );
    assert!(
        cut_slice.contains("5.6693 w"),
        "CutContour overlay must scale 1.0mm width by 2.0x to 2.0mm (5.6693 pt): {cut_slice}"
    );
    // Expected world corners: [(14,12), (48,16), (42,38), (16,34)] mm -> PDF pt (with 3mm bleed)
    let pt = 72.0 / 25.4;
    let bleed = 3.0;
    for (x_mm, y_mm) in [(14.0, 12.0), (48.0, 16.0), (42.0, 38.0), (16.0, 34.0)] {
        let px = format!("{:.4}", (x_mm + bleed) * pt);
        let py = format!("{:.4}", (297.0 + bleed - y_mm) * pt);
        assert!(
            cut_slice.contains(&format!("{px} {py}")),
            "missing expected perspective corner ({x_mm}, {y_mm}) mm -> ({px} {py}) in {cut_slice}"
        );
    }
}

fn assert_gs_cutcontour_tiffsep(pdf_bytes: &[u8], tag: &str) {
    let gs_path = std::path::Path::new("/usr/bin/gs");
    if !gs_path.exists() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let pdf_path = dir.path().join(format!("{tag}.pdf"));
    std::fs::write(&pdf_path, pdf_bytes).unwrap();
    let out_tmpl = dir.path().join(format!("{tag}_sep.tif"));
    let status = std::process::Command::new(gs_path)
        .args([
            "-q",
            "-dNOPAUSE",
            "-dBATCH",
            "-sDEVICE=tiffsep",
            "-r150",
            &format!("-sOutputFile={}", out_tmpl.display()),
            &pdf_path.display().to_string(),
        ])
        .status()
        .expect("invoke gs");
    assert!(status.success(), "gs tiffsep failed for {tag}");
    let cut_plate = dir.path().join(format!("{tag}_sep(CutContour).tif"));
    assert!(
        cut_plate.exists(),
        "expected Ghostscript CutContour separation plate {} for {tag}",
        cut_plate.display()
    );
    let img = image::open(&cut_plate).expect("decode CutContour separation TIFF");
    let luma = img.to_luma8();
    let mut min_v = u8::MAX;
    let mut max_v = u8::MIN;
    for p in luma.pixels() {
        min_v = min_v.min(p.0[0]);
        max_v = max_v.max(p.0[0]);
    }
    assert!(
        min_v < max_v,
        "CutContour separation plate for {tag} must be non-blank (min={min_v}, max={max_v})"
    );
}

#[test]
fn w2b_r5_pdf_cutcontour_clone_inherited_spot_under_inner_shadow() {
    use coree_core::{Affine2D, ArtisticMediaStyle, Effect, FeatherDirection};

    for (scale_with_object, expected_w) in [(true, "5.6693 w"), (false, "2.8346 w")] {
        let mut doc = Document::new("CloneCutContour", 210.0, 297.0, ColorMode::Cmyk, 300.0);
        let mut master = SceneObject::new(
            ObjectId(0),
            "HiddenMaster",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 25.0,
                height: 20.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgba8(40, 140, 210, 255)))
        .with_outline(OutlineStyle {
            color: Color::cut_contour_spot(),
            width_mm: 1.0,
            scale_with_object,
            ..OutlineStyle::default()
        });
        master.visible = false;
        let master_id = doc.add_object(master, None).unwrap();

        let mut clone_obj = SceneObject::new(
            ObjectId(0),
            "CloneWithInnerShadow",
            NodePayload::Clone {
                master_id,
                link_fill: true,
                link_outline: true,
                link_shape: true,
            },
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none());
        clone_obj.transform = Affine2D::translate(20.0, 20.0).compose(&Affine2D::scale(2.0, 2.0));
        clone_obj.effects.push(Effect::InnerShadow {
            dx_mm: 1.5,
            dy_mm: 1.5,
            blur_mm: 2.0,
            opacity: 0.7,
            color: Color::BLACK,
            feather_direction: FeatherDirection::Inside,
        });
        doc.add_object(clone_obj, None).unwrap();

        let opts = PdfExportOptions {
            compress_streams: false,
            ..PdfExportOptions::default()
        };
        let pdf_bytes = export_pdf_to_bytes(&doc, &opts).expect("PDF export");
        let pdf_text = String::from_utf8_lossy(&pdf_bytes);
        assert!(
            pdf_text.contains("/Separation /CutContour /DeviceCMYK"),
            "Clone CutContour must emit /Separation /CutContour resource"
        );
        let idx = pdf_text
            .find("/DynIm1 Do")
            .expect("raster composite image draw");
        let cut_slice = &pdf_text[idx..];
        assert!(
            cut_slice.contains("/Sep1 CS 1.0000 SCN"),
            "Clone CutContour overlay must emit /Sep1 CS 1.0000 SCN: {cut_slice}"
        );
        assert!(
            cut_slice.contains(expected_w),
            "Clone CutContour overlay must emit {expected_w} (scale_with_object={scale_with_object}): {cut_slice}"
        );
        assert_gs_cutcontour_tiffsep(&pdf_bytes, &format!("clone_{scale_with_object}"));
    }

    // Also verify a Clone whose hidden master is an ArtisticMedia payload
    let mut doc = Document::new(
        "CloneArtisticMediaCut",
        210.0,
        297.0,
        ColorMode::Cmyk,
        300.0,
    );
    let spine = BezierPath::from_line(Point2D::new(0.0, 0.0), Point2D::new(30.0, 10.0));
    let mut am_master = SceneObject::new(
        ObjectId(0),
        "HiddenArtisticMaster",
        NodePayload::ArtisticMedia {
            spine,
            style: ArtisticMediaStyle::Preset {
                profile: coree_core::ArtisticWidthProfile::TaperBoth,
                max_width_mm: 4.0,
            },
        },
    )
    .with_fill(Fill::Solid(Color::CYAN))
    .with_outline(OutlineStyle {
        color: Color::cut_contour_spot(),
        width_mm: 1.0,
        scale_with_object: true,
        ..OutlineStyle::default()
    });
    am_master.visible = false;
    let am_master_id = doc.add_object(am_master, None).unwrap();

    let mut am_clone = SceneObject::new(
        ObjectId(0),
        "ArtisticClone",
        NodePayload::Clone {
            master_id: am_master_id,
            link_fill: true,
            link_outline: true,
            link_shape: true,
        },
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    am_clone.transform = Affine2D::translate(30.0, 30.0).compose(&Affine2D::scale(2.0, 2.0));
    am_clone.effects.push(Effect::InnerShadow {
        dx_mm: 1.0,
        dy_mm: 1.0,
        blur_mm: 1.5,
        opacity: 0.6,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Inside,
    });
    doc.add_object(am_clone, None).unwrap();

    let opts = PdfExportOptions {
        compress_streams: false,
        ..PdfExportOptions::default()
    };
    let pdf_bytes = export_pdf_to_bytes(&doc, &opts).expect("PDF export");
    let pdf_text = String::from_utf8_lossy(&pdf_bytes);
    let idx = pdf_text
        .find("/DynIm1 Do")
        .expect("raster composite image draw");
    let cut_slice = &pdf_text[idx..];
    assert!(cut_slice.contains("/Sep1 CS 1.0000 SCN"));
    assert!(cut_slice.contains("5.6693 w"));
    assert_gs_cutcontour_tiffsep(&pdf_bytes, "clone_artistic_media");
}

#[test]
fn w2b_r5_pdf_cutcontour_artistic_media_under_inner_shadow() {
    use coree_core::{Affine2D, ArtisticMediaStyle, Effect, FeatherDirection};

    for (scale_with_object, expected_w) in [(true, "5.6693 w"), (false, "2.8346 w")] {
        let mut doc = Document::new(
            "ArtisticMediaCutContour",
            210.0,
            297.0,
            ColorMode::Cmyk,
            300.0,
        );
        let spine = BezierPath::from_line(Point2D::new(0.0, 0.0), Point2D::new(35.0, 12.0));
        let mut obj = SceneObject::new(
            ObjectId(0),
            "ArtisticMediaCut",
            NodePayload::ArtisticMedia {
                spine,
                style: ArtisticMediaStyle::Calligraphic {
                    width_mm: 4.0,
                    angle_deg: 35.0,
                },
            },
        )
        .with_fill(Fill::Solid(Color::rgba8(180, 60, 120, 255)))
        .with_outline(OutlineStyle {
            color: Color::cut_contour_spot(),
            width_mm: 1.0,
            scale_with_object,
            ..OutlineStyle::default()
        });
        obj.transform = Affine2D::translate(25.0, 25.0).compose(&Affine2D::scale(2.0, 2.0));
        obj.effects.push(Effect::InnerShadow {
            dx_mm: 1.5,
            dy_mm: 1.5,
            blur_mm: 2.0,
            opacity: 0.7,
            color: Color::BLACK,
            feather_direction: FeatherDirection::Inside,
        });
        doc.add_object(obj, None).unwrap();

        let opts = PdfExportOptions {
            compress_streams: false,
            ..PdfExportOptions::default()
        };
        let pdf_bytes = export_pdf_to_bytes(&doc, &opts).expect("PDF export");
        let pdf_text = String::from_utf8_lossy(&pdf_bytes);
        assert!(
            pdf_text.contains("/Separation /CutContour /DeviceCMYK"),
            "ArtisticMedia CutContour must emit /Separation /CutContour resource"
        );
        let idx = pdf_text
            .find("/DynIm1 Do")
            .expect("raster composite image draw");
        let cut_slice = &pdf_text[idx..];
        assert!(
            cut_slice.contains("/Sep1 CS 1.0000 SCN"),
            "ArtisticMedia CutContour overlay must emit /Sep1 CS 1.0000 SCN: {cut_slice}"
        );
        assert!(
            cut_slice.contains(expected_w),
            "ArtisticMedia CutContour overlay must emit {expected_w} (scale_with_object={scale_with_object}): {cut_slice}"
        );
        assert_gs_cutcontour_tiffsep(&pdf_bytes, &format!("artistic_media_{scale_with_object}"));
    }
}

#[test]
fn w2_exit_r2_group_and_symbol_instance_export_effects_cutcontour_and_perspective() {
    use coree_core::{
        apply_command, Affine2D, BevelStyle, CommandOp, Effect, ExtrudeLight, FeatherDirection,
        Perspective3x3, SymbolDef,
    };
    use coree_io::{export_eps_to_string, export_svg_to_string};

    let persp = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(10.0, 10.0, 30.0, 30.0),
        &[
            Point2D::new(16.0, 12.0),
            Point2D::new(38.0, 15.0),
            Point2D::new(35.0, 42.0),
            Point2D::new(12.0, 39.0),
        ],
    )
    .expect("valid perspective quad");

    for is_symbol in [false, true] {
        let kind = if is_symbol { "SymbolInstance" } else { "Group" };
        let build_doc = |eff: Option<Effect>, p: Option<Perspective3x3>| {
            let mut doc = Document::new("ContainerExport", 120.0, 120.0, ColorMode::Cmyk, 300.0);
            let child = SceneObject::new(
                ObjectId(0),
                "ChildBox",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 30.0,
                    height: 30.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_transform(Affine2D::translate(10.0, 10.0))
            .with_fill(Fill::Solid(Color::rgba8(50, 140, 220, 255)))
            .with_outline(OutlineStyle {
                color: Color::cut_contour_spot(),
                width_mm: 0.8,
                scale_with_object: true,
                ..OutlineStyle::default()
            });

            let target_id = if is_symbol {
                let sym = SymbolDef::new("CutSym", vec![child]);
                let sid = sym.id;
                doc.insert_symbol(sym);
                doc.add_object(
                    SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid)),
                    None,
                )
                .unwrap()
            } else {
                let cid = doc.add_object(child, None).unwrap();
                doc.group_objects(&[cid], "CutGrp").unwrap()
            };
            if let Some(e) = eff {
                apply_command(
                    &mut doc,
                    &CommandOp::AddEffect {
                        id: target_id,
                        effect: e,
                    },
                )
                .unwrap();
            }
            if p.is_some() {
                apply_command(
                    &mut doc,
                    &CommandOp::SetObjectPerspective {
                        ids: vec![target_id],
                        perspective: p,
                    },
                )
                .unwrap();
            }
            doc
        };

        // 1. Raster-effect containers (InnerShadow, Bevel, Extrude3D) -> PDF (/DynIm + /Separation /CutContour), SVG (<image>), EPS (image)
        for (fx_name, eff) in [
            (
                "InnerShadow",
                Effect::InnerShadow {
                    dx_mm: 2.0,
                    dy_mm: 2.0,
                    blur_mm: 2.0,
                    opacity: 0.8,
                    color: Color::BLACK,
                    feather_direction: FeatherDirection::Inside,
                },
            ),
            (
                "Bevel",
                Effect::Bevel {
                    style: BevelStyle::SoftEdge,
                    distance_mm: 2.5,
                    softness_mm: 0.6,
                    light_angle_deg: 135.0,
                    altitude_deg: 45.0,
                    highlight_color: Color::WHITE,
                    shadow_color: Color::BLACK,
                    intensity: 0.85,
                },
            ),
            (
                "Extrude3D",
                Effect::Extrude3D {
                    vanishing_point: Point2D::new(70.0, 10.0),
                    depth: 10.0,
                    bevel_depth: 0.8,
                    shade_color: Color::rgba8(20, 25, 50, 255),
                    rotation_3d: [15.0, -20.0, 0.0],
                    lights: vec![ExtrudeLight {
                        dir: [0.4, -0.6, 0.7],
                        intensity: 1.0,
                    }],
                },
            ),
        ] {
            let doc = build_doc(Some(eff), None);
            let pdf_opts = PdfExportOptions {
                color_space: PdfColorSpace::DeviceCmyk,
                compress_streams: false,
                ..PdfExportOptions::default()
            };
            let pdf_bytes = export_pdf_to_bytes(&doc, &pdf_opts).expect("PDF export");
            let pdf_text = String::from_utf8_lossy(&pdf_bytes);
            assert!(
                pdf_text.contains("/DynIm1 Do"),
                "{kind} + {fx_name} PDF must emit raster composite /DynIm1 Do"
            );
            assert!(
                pdf_text.contains("/Separation /CutContour /DeviceCMYK")
                    && pdf_text.contains("/Sep1 CS 1.0000 SCN"),
                "{kind} + {fx_name} PDF must preserve vector CutContour spot overlay"
            );
            assert_gs_cutcontour_tiffsep(&pdf_bytes, &format!("{kind}_{fx_name}"));

            let svg_str = export_svg_to_string(&doc, 0).expect("SVG export");
            assert!(
                svg_str.contains("<image ") && svg_str.contains("data:image/png;base64,"),
                "{kind} + {fx_name} SVG must emit raster composite <image>: {svg_str}"
            );

            let eps_str = export_eps_to_string(&doc, 0).expect("EPS export");
            assert!(
                eps_str.contains(">> image"),
                "{kind} + {fx_name} EPS must emit raster composite image"
            );
        }

        // 2. Non-raster Perspective3x3 on Group / SymbolInstance -> PDF/SVG/EPS vector warp
        let doc_plain = build_doc(None, None);
        let doc_persp = build_doc(None, Some(persp));

        let pdf_opts = PdfExportOptions {
            color_space: PdfColorSpace::DeviceCmyk,
            compress_streams: false,
            ..PdfExportOptions::default()
        };
        let pdf_plain =
            String::from_utf8_lossy(&export_pdf_to_bytes(&doc_plain, &pdf_opts).unwrap())
                .into_owned();
        let pdf_persp =
            String::from_utf8_lossy(&export_pdf_to_bytes(&doc_persp, &pdf_opts).unwrap())
                .into_owned();
        assert_ne!(pdf_plain, pdf_persp, "{kind} Perspective3x3 must alter PDF");
        let pt = 72.0 / 25.4;
        let bleed = 3.0;
        let px16 = format!("{:.4}", (16.0 + bleed) * pt);
        let py12 = format!("{:.4}", (120.0 + bleed - 12.0) * pt);
        assert!(
            pdf_persp.contains(&format!("{px16} {py12}")),
            "{kind} Perspective3x3 PDF must emit warped corner ({px16} {py12})"
        );

        let svg_plain = export_svg_to_string(&doc_plain, 0).unwrap();
        let svg_persp = export_svg_to_string(&doc_persp, 0).unwrap();
        assert_ne!(svg_plain, svg_persp, "{kind} Perspective3x3 must alter SVG");
        assert!(
            svg_persp.contains("16.0000 12.0000")
                && svg_persp.contains("38.0000 15.0000")
                && svg_persp.contains("35.0000 42.0000")
                && svg_persp.contains("12.0000 39.0000"),
            "{kind} Perspective3x3 SVG must emit warped corners: {svg_persp}"
        );

        let eps_plain = export_eps_to_string(&doc_plain, 0).unwrap();
        let eps_persp = export_eps_to_string(&doc_persp, 0).unwrap();
        assert_ne!(eps_plain, eps_persp, "{kind} Perspective3x3 must alter EPS");
    }
}

#[test]
fn w2_exit_r3_svg_powerclip_and_nested_group_perspective_ctm_and_dimension_label() {
    use coree_core::{apply_command, Affine2D, CommandOp, Perspective3x3};
    use coree_io::export_svg_to_string;

    // 1. Translated PowerClip with Perspective3x3 in SVG: clipPath, fill, stroke, and contents must all align.
    let mut doc = Document::new("SvgPowerClipPersp", 160.0, 160.0, ColorMode::Rgb, 300.0);
    let inner = SceneObject::new(
        ObjectId(0),
        "ClippedRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            52.0, 42.0, 20.0, 20.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::CYAN));
    let inner_id = doc.add_object(inner, None).unwrap();

    let persp = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(0.0, 0.0, 24.0, 24.0),
        &[
            Point2D::new(2.0, 1.0),
            Point2D::new(23.0, 3.0),
            Point2D::new(21.0, 24.0),
            Point2D::new(0.0, 22.0),
        ],
    )
    .unwrap();
    let pc = SceneObject::new(
        ObjectId(0),
        "PerspClip",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 24.0, 24.0,
        ))),
    )
    .with_transform(Affine2D::translate(50.0, 40.0))
    .with_fill(Fill::Solid(Color::YELLOW))
    .with_outline(OutlineStyle::solid(Color::BLACK, 0.5));
    let pc_id = doc.add_object(pc, None).unwrap();
    apply_command(
        &mut doc,
        &CommandOp::CreatePowerClip {
            container_id: pc_id,
            content_ids: vec![inner_id],
        },
    )
    .unwrap();
    apply_command(
        &mut doc,
        &CommandOp::SetObjectPerspective {
            ids: vec![pc_id],
            perspective: Some(persp),
        },
    )
    .unwrap();

    let svg = export_svg_to_string(&doc, 0).expect("SVG export");
    // Expected translated+warped container corners: (52,41), (73,43), (71,64), (50,62)
    let clip_line = svg
        .lines()
        .find(|l| l.contains("<clipPath id=\"clip-1\">"))
        .expect("PowerClip clipPath");
    for corner in [
        "52.0000 41.0000",
        "73.0000 43.0000",
        "71.0000 64.0000",
        "50.0000 62.0000",
    ] {
        assert!(
            clip_line.contains(corner),
            "Translated PowerClip + Perspective3x3 clipPath missing corner {corner}: {clip_line}"
        );
    }
    let fill_line = svg
        .lines()
        .find(|l| l.contains("fill=\"#ffff00\" stroke=\"none\""))
        .expect("PowerClip fill path");
    for corner in [
        "52.0000 41.0000",
        "73.0000 43.0000",
        "71.0000 64.0000",
        "50.0000 62.0000",
    ] {
        assert!(
            fill_line.contains(corner),
            "Translated PowerClip + Perspective3x3 fill missing corner {corner}: {fill_line}"
        );
    }
    let stroke_line = svg
        .lines()
        .find(|l| l.contains("fill=\"none\" stroke=\"#000000\""))
        .expect("PowerClip stroke path");
    for corner in [
        "52.0000 41.0000",
        "73.0000 43.0000",
        "71.0000 64.0000",
        "50.0000 62.0000",
    ] {
        assert!(
            stroke_line.contains(corner),
            "Translated PowerClip + Perspective3x3 stroke missing corner {corner}: {stroke_line}"
        );
    }
    assert!(
        svg.contains("53.5833 42.9167") && !svg.contains("M 2.0000 1.0000"),
        "PowerClip contents and container must both be translated away from origin: {svg}"
    );

    // 2. Nested OuterGroup(translate(50,30)) -> InnerGroup(translate(10,10), Perspective3x3) + Dimension child
    let mut doc2 = Document::new("NestedSvgPersp", 200.0, 200.0, ColorMode::Rgb, 300.0);
    let leaf = SceneObject::new(
        ObjectId(0),
        "LeafRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 24.0, 24.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::MAGENTA));
    let leaf_id = doc2.add_object(leaf, None).unwrap();

    let dim_obj = SceneObject::new(
        ObjectId(0),
        "DimInsidePersp",
        NodePayload::Dimension(DimensionObject::linear(
            DimensionKind::Horizontal,
            Point2D::new(2.0, 12.0),
            Point2D::new(22.0, 12.0),
            4.0,
        )),
    )
    .with_outline(OutlineStyle::solid(Color::BLACK, 0.3));
    let dim_id = doc2.add_object(dim_obj, None).unwrap();

    let inner_grp_id = doc2
        .group_objects(&[leaf_id, dim_id], "InnerPerspGrp")
        .unwrap();
    apply_command(
        &mut doc2,
        &CommandOp::TransformObjects {
            ids: vec![inner_grp_id],
            affine: Affine2D::translate(10.0, 10.0),
        },
    )
    .unwrap();
    apply_command(
        &mut doc2,
        &CommandOp::SetObjectPerspective {
            ids: vec![inner_grp_id],
            perspective: Some(persp),
        },
    )
    .unwrap();
    let outer_grp_id = doc2.group_objects(&[inner_grp_id], "OuterGrp").unwrap();
    apply_command(
        &mut doc2,
        &CommandOp::TransformObjects {
            ids: vec![outer_grp_id],
            affine: Affine2D::translate(50.0, 30.0),
        },
    )
    .unwrap();

    let svg2 = export_svg_to_string(&doc2, 0).expect("SVG export");
    // OuterGroup emits transform="matrix(1 0 0 1 50.0000 30.0000)"; InnerGroup's leaf must be at T_inner + warped = (12, 11), NOT (62, 41)!
    assert!(
        svg2.contains("matrix(1.000000 0.000000 0.000000 1.000000 50.0000 30.0000)"),
        "OuterGroup must emit SVG transform matrix(1 0 0 1 50 30): {svg2}"
    );
    let leaf_line = svg2
        .lines()
        .find(|l| l.contains(&format!("id=\"obj-{}\"", leaf_id.0)))
        .expect("InnerGroup leaf path");
    for corner in [
        "12.0000 11.0000",
        "33.0000 13.0000",
        "31.0000 34.0000",
        "10.0000 32.0000",
    ] {
        assert!(
            leaf_line.contains(corner),
            "InnerGroup path inside OuterGroup missing {corner} (must NOT double-apply OuterGroup's (50,30) transform): {leaf_line}"
        );
    }
    assert!(
        !svg2.contains("62.0000 41.0000"),
        "Double-applied OuterGroup transform (62,41) must not appear inside <g transform=\"... 50 30\">: {svg2}"
    );
    // Dimension inside parent_persp must emit both its dimension line <path> AND outlined label glyph <path>s
    let dim_tag = format!("<g id=\"obj-{}\">", dim_id.0);
    let dim_start = svg2.find(&dim_tag).expect("Dimension group in SVG");
    let dim_end = svg2[dim_start..].find("</g>").unwrap() + dim_start;
    let dim_slice = &svg2[dim_start..dim_end];
    let path_count = dim_slice.matches("<path ").count();
    assert!(
        path_count >= 2,
        "Dimension under parent_persp in SVG must emit both line and outlined label paths (found {path_count}): {dim_slice}"
    );
}

#[test]
fn w2_exit_r3_text_on_path_and_connector_inside_translated_perspective_group() {
    use coree_core::{apply_command, Affine2D, CommandOp, Perspective3x3};
    use coree_io::{export_eps_to_string, export_svg_to_string};

    let persp = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(0.0, 0.0, 50.0, 50.0),
        &[
            Point2D::new(4.0, 2.0),
            Point2D::new(48.0, 5.0),
            Point2D::new(45.0, 47.0),
            Point2D::new(2.0, 44.0),
        ],
    )
    .unwrap();

    // 1. TextOnPath inside translated Group(40, 35) with Perspective3x3 across PDF / SVG / EPS
    let mut doc_top = Document::new("TopPersp", 160.0, 160.0, ColorMode::Rgb, 300.0);
    let host = SceneObject::new(
        ObjectId(0),
        "HostLine",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(5.0, 20.0),
            Point2D::new(45.0, 20.0),
        )),
    )
    .with_outline(OutlineStyle::solid(Color::BLACK, 0.3));
    let host_id = doc_top.add_object(host, None).unwrap();

    let top_obj = SceneObject::new(
        ObjectId(0),
        "TopObj",
        NodePayload::TextOnPath(coree_core::TextOnPath {
            path_id: host_id,
            text: ArtisticText::from_plain(
                "AB",
                TextStyle {
                    font_size_pt: 18.0,
                    fill: Color::rgba8(200, 20, 20, 255),
                    ..TextStyle::default()
                },
            ),
            distance_mm: 0.0,
            offset_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    let top_id = doc_top.add_object(top_obj, None).unwrap();
    let grp_id = doc_top.group_objects(&[host_id, top_id], "TopGrp").unwrap();
    apply_command(
        &mut doc_top,
        &CommandOp::TransformObjects {
            ids: vec![grp_id],
            affine: Affine2D::translate(40.0, 35.0),
        },
    )
    .unwrap();
    apply_command(
        &mut doc_top,
        &CommandOp::SetObjectPerspective {
            ids: vec![grp_id],
            perspective: Some(persp),
        },
    )
    .unwrap();

    // SVG: glyph paths must stay inside translated+warped group bbox [38..92, 33..85]
    let svg_top = export_svg_to_string(&doc_top, 0).expect("SVG TextOnPath");
    let top_marker = format!("id=\"obj-{}-g0\"", top_id.0);
    assert!(
        svg_top.contains(&top_marker),
        "SVG must outline TextOnPath under Perspective3x3: {svg_top}"
    );
    for line in svg_top
        .lines()
        .filter(|l| l.contains(&format!("id=\"obj-{}-g", top_id.0)))
    {
        let d_start = line.find(" d=\"").unwrap() + 4;
        let d_end = line[d_start..].find('"').unwrap() + d_start;
        let nums: Vec<f64> = line[d_start..d_end]
            .split_whitespace()
            .filter_map(|tok| tok.parse::<f64>().ok())
            .collect();
        assert!(!nums.is_empty(), "glyph path must have coords: {line}");
        for pair in nums.as_chunks::<2>().0 {
            assert!(
                (38.0..=92.0).contains(&pair[0]) && (33.0..=85.0).contains(&pair[1]),
                "SVG TextOnPath glyph coord ({:.2}, {:.2}) out of expected group range [38..92, 33..85] (double parent_xf?): {line}",
                pair[0],
                pair[1]
            );
        }
    }

    // PDF & EPS: verify TextOnPath doesn't double-apply (40, 35)
    let pdf_opts = PdfExportOptions {
        color_space: PdfColorSpace::DeviceRgb,
        compress_streams: false,
        ..PdfExportOptions::default()
    };
    let pdf_top =
        String::from_utf8_lossy(&export_pdf_to_bytes(&doc_top, &pdf_opts).unwrap()).into_owned();
    let pt_per_mm = 72.0 / 25.4;
    let bleed_mm = 3.0;
    let mut pdf_checked_pts = 0usize;
    for line in pdf_top.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() >= 3 && matches!(toks.last().copied(), Some("m" | "l")) {
            if let (Ok(x_pt), Ok(y_pt)) = (toks[0].parse::<f64>(), toks[1].parse::<f64>()) {
                let x_mm = x_pt / pt_per_mm - bleed_mm;
                let y_mm = 160.0 + bleed_mm - y_pt / pt_per_mm;
                if x_mm > 10.0 && y_mm > 10.0 {
                    assert!(
                        (38.0..=92.0).contains(&x_mm) && (33.0..=85.0).contains(&y_mm),
                        "PDF TextOnPath/Host coord ({x_mm:.2}, {y_mm:.2}) out of expected group range [38..92, 33..85]: {line}"
                    );
                    pdf_checked_pts += 1;
                }
            }
        }
    }
    assert!(
        pdf_checked_pts >= 4,
        "expected PDF path coords for TextOnPath"
    );

    let eps_top = export_eps_to_string(&doc_top, 0).unwrap();
    let mut eps_checked_pts = 0usize;
    for line in eps_top.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() == 3 && matches!(toks[2], "moveto" | "lineto") {
            if let (Ok(x_pt), Ok(y_pt)) = (toks[0].parse::<f64>(), toks[1].parse::<f64>()) {
                let x_mm = x_pt / pt_per_mm;
                let y_mm = 160.0 - y_pt / pt_per_mm;
                assert!(
                    (38.0..=92.0).contains(&x_mm) && (33.0..=85.0).contains(&y_mm),
                    "EPS TextOnPath/Host coord ({x_mm:.2}, {y_mm:.2}) out of expected group range [38..92, 33..85]: {line}"
                );
                eps_checked_pts += 1;
            }
        }
    }
    assert!(
        eps_checked_pts >= 4,
        "expected EPS path coords for TextOnPath"
    );

    // 2. Connector inside translated Group(40, 40) with Perspective3x3 across PDF / SVG / EPS
    let mut doc_conn = Document::new("ConnPersp", 160.0, 160.0, ColorMode::Rgb, 300.0);
    let box_a = SceneObject::new(
        ObjectId(0),
        "BoxA",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 10.0,
            height: 10.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_transform(Affine2D::translate(5.0, 5.0))
    .with_fill(Fill::Solid(Color::CYAN));
    let box_b = SceneObject::new(
        ObjectId(0),
        "BoxB",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 10.0,
            height: 10.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_transform(Affine2D::translate(30.0, 30.0))
    .with_fill(Fill::Solid(Color::YELLOW));
    let id_a = doc_conn.add_object(box_a, None).unwrap();
    let id_b = doc_conn.add_object(box_b, None).unwrap();
    let conn = SceneObject::new(
        ObjectId(0),
        "LinkAB",
        NodePayload::Connector(ConnectorObject::between_objects(
            id_a,
            AnchorPoint::Center,
            id_b,
            AnchorPoint::Center,
            ConnectorStyle::Straight,
        )),
    )
    .with_outline(OutlineStyle::solid(Color::rgba8(255, 0, 0, 255), 1.0));
    let conn_id = doc_conn.add_object(conn, None).unwrap();
    let cgrp_id = doc_conn
        .group_objects(&[id_a, id_b, conn_id], "ConnPerspGrp")
        .unwrap();
    apply_command(
        &mut doc_conn,
        &CommandOp::TransformObjects {
            ids: vec![cgrp_id],
            affine: Affine2D::translate(40.0, 40.0),
        },
    )
    .unwrap();
    apply_command(
        &mut doc_conn,
        &CommandOp::SetObjectPerspective {
            ids: vec![cgrp_id],
            perspective: Some(persp),
        },
    )
    .unwrap();

    let svg_conn = export_svg_to_string(&doc_conn, 0).expect("SVG Connector");
    let conn_line = svg_conn
        .lines()
        .find(|l| l.contains(&format!("id=\"obj-{}\"", conn_id.0)))
        .expect("SVG Connector path");
    let d_start = conn_line.find(" d=\"").unwrap() + 4;
    let d_end = conn_line[d_start..].find('"').unwrap() + d_start;
    let svg_conn_nums: Vec<f64> = conn_line[d_start..d_end]
        .split_whitespace()
        .filter_map(|tok| tok.parse::<f64>().ok())
        .collect();
    assert!(svg_conn_nums.len() >= 4);
    for pair in svg_conn_nums.as_chunks::<2>().0 {
        assert!(
            (42.0..=88.0).contains(&pair[0]) && (42.0..=88.0).contains(&pair[1]),
            "SVG Connector coord ({:.2}, {:.2}) out of [42..88]: {conn_line}",
            pair[0],
            pair[1]
        );
    }

    let pdf_conn =
        String::from_utf8_lossy(&export_pdf_to_bytes(&doc_conn, &pdf_opts).unwrap()).into_owned();
    let mut red_conn_pts = 0usize;
    let mut in_red_conn = false;
    for line in pdf_conn.lines() {
        if line.contains("1.0000 0.0000 0.0000 RG") {
            in_red_conn = true;
            continue;
        }
        if in_red_conn {
            let toks: Vec<&str> = line.split_whitespace().collect();
            if toks.len() == 3 && matches!(toks[2], "m" | "l") {
                let x_mm = toks[0].parse::<f64>().unwrap() / pt_per_mm - bleed_mm;
                let y_mm = 160.0 + bleed_mm - toks[1].parse::<f64>().unwrap() / pt_per_mm;
                assert!(
                    (42.0..=88.0).contains(&x_mm) && (42.0..=88.0).contains(&y_mm),
                    "PDF Connector coord ({x_mm:.2}, {y_mm:.2}) escaped warped group [42..88]: {line}"
                );
                red_conn_pts += 1;
            } else if line.trim() == "S" {
                in_red_conn = false;
            }
        }
    }
    assert!(red_conn_pts >= 2, "must check PDF red Connector endpoints");

    let eps_conn = export_eps_to_string(&doc_conn, 0).unwrap();
    let mut eps_red_pts = 0usize;
    let mut in_eps_red = false;
    for line in eps_conn.lines() {
        if line.contains("1.0000 0.0000 0.0000 setrgbcolor") {
            in_eps_red = true;
            continue;
        }
        if in_eps_red {
            let toks: Vec<&str> = line.split_whitespace().collect();
            if toks.len() == 3 && matches!(toks[2], "moveto" | "lineto") {
                let x_mm = toks[0].parse::<f64>().unwrap() / pt_per_mm;
                let y_mm = 160.0 - toks[1].parse::<f64>().unwrap() / pt_per_mm;
                assert!(
                    (42.0..=88.0).contains(&x_mm) && (42.0..=88.0).contains(&y_mm),
                    "EPS Connector coord ({x_mm:.2}, {y_mm:.2}) escaped warped group [42..88]: {line}"
                );
                eps_red_pts += 1;
            } else if line.trim() == "stroke" {
                in_eps_red = false;
            }
        }
    }
    assert!(eps_red_pts >= 2, "must check EPS red Connector endpoints");
}

#[test]
fn w2_exit_r3_recursive_raster_composite_for_persp_group_and_symbol_child_effects() {
    use coree_core::{apply_command, Affine2D, CommandOp, Perspective3x3, SymbolDef};
    use coree_io::{export_eps_to_string, export_svg_to_string};

    let persp = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(10.0, 10.0, 30.0, 30.0),
        &[
            Point2D::new(14.0, 11.0),
            Point2D::new(39.0, 14.0),
            Point2D::new(36.0, 41.0),
            Point2D::new(11.0, 38.0),
        ],
    )
    .unwrap();

    for case in ["PerspGroup_ChildDropShadow", "SymbolDef_ChildDropShadow"] {
        let mut doc = Document::new(case, 120.0, 120.0, ColorMode::Cmyk, 300.0);
        let mut child = SceneObject::new(
            ObjectId(0),
            "ShadowChild",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 30.0,
                height: 30.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_transform(Affine2D::translate(10.0, 10.0))
        .with_fill(Fill::Solid(Color::rgba8(60, 130, 210, 255)))
        .with_outline(OutlineStyle {
            color: Color::cut_contour_spot(),
            width_mm: 0.8,
            scale_with_object: true,
            ..OutlineStyle::default()
        });
        child.effects.push(Effect::DropShadow {
            dx_mm: 3.0,
            dy_mm: 3.0,
            blur_mm: 2.0,
            opacity: 0.75,
            color: Color::BLACK,
            feather_direction: FeatherDirection::Outside,
        });

        if case == "SymbolDef_ChildDropShadow" {
            let sym = SymbolDef::new("ShadowSym", vec![child]);
            let sid = sym.id;
            doc.insert_symbol(sym);
            doc.add_object(
                SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid)),
                None,
            )
            .unwrap();
        } else {
            let cid = doc.add_object(child, None).unwrap();
            let gid = doc.group_objects(&[cid], "PerspGrp").unwrap();
            apply_command(
                &mut doc,
                &CommandOp::SetObjectPerspective {
                    ids: vec![gid],
                    perspective: Some(persp),
                },
            )
            .unwrap();
        }

        let pdf_opts = PdfExportOptions {
            color_space: PdfColorSpace::DeviceCmyk,
            compress_streams: false,
            ..PdfExportOptions::default()
        };
        let pdf_bytes = export_pdf_to_bytes(&doc, &pdf_opts).expect("PDF export");
        let pdf_text = String::from_utf8_lossy(&pdf_bytes);
        assert!(
            pdf_text.contains("/DynIm1 Do"),
            "{case} PDF must emit /DynIm1 Do raster composite for child DropShadow"
        );
        assert!(
            pdf_text.contains("/Separation /CutContour /DeviceCMYK")
                && pdf_text.contains("/Sep1 CS 1.0000 SCN"),
            "{case} PDF must preserve /CutContour spot overlay"
        );
        assert_gs_cutcontour_tiffsep(&pdf_bytes, case);

        let svg = export_svg_to_string(&doc, 0).expect("SVG export");
        assert!(
            svg.contains("<image ") && svg.contains("data:image/png;base64,"),
            "{case} SVG must emit <image raster composite for child DropShadow"
        );

        let eps = export_eps_to_string(&doc, 0).expect("EPS export");
        assert!(
            eps.contains(">> image"),
            "{case} EPS must emit >> image raster composite for child DropShadow"
        );
    }
}

#[test]
fn w2_exit_r4_powerclip_perspective_pixel_parity_with_and_without_dropshadow() {
    use coree_core::{apply_command, Affine2D, CommandOp, Perspective3x3};
    use coree_io::export_eps_to_string;
    use coree_io::export_svg_to_string;
    use coree_render::{render_page_to_rgba, render_svg_to_rgba, ViewMode};

    let red_bbox = |rgba: &[u8], w: u32, h: u32| -> (usize, (f64, f64, f64, f64)) {
        let px_per_mm = 150.0 / 25.4;
        let mut count = 0usize;
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                let [r, g, b, a] = [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]];
                if a > 128 && r > 170 && g < 90 && b < 90 {
                    count += 1;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        assert!(count > 0, "expected non-empty red pixels");
        (
            count,
            (
                f64::from(min_x) / px_per_mm,
                f64::from(min_y) / px_per_mm,
                f64::from(max_x + 1) / px_per_mm,
                f64::from(max_y + 1) / px_per_mm,
            ),
        )
    };

    let persp = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(0.0, 0.0, 24.0, 24.0),
        &[
            Point2D::new(8.0, 0.0),
            Point2D::new(16.0, 0.0),
            Point2D::new(24.0, 24.0),
            Point2D::new(0.0, 24.0),
        ],
    )
    .unwrap();

    for with_shadow in [false, true] {
        let mut doc = Document::new("PCPerspIO", 160.0, 160.0, ColorMode::Rgb, 150.0);
        let lid = doc.pages()[0].layers[0].id;
        let inner = SceneObject::new(
            ObjectId(0),
            "RedInner",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                52.0, 42.0, 20.0, 8.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(220, 20, 20, 255)));
        let inner_id = doc.add_object(inner, Some(lid)).unwrap();
        let pc = SceneObject::new(
            ObjectId(0),
            "TrapezoidPC",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 24.0, 24.0,
            ))),
        )
        .with_transform(Affine2D::translate(50.0, 40.0))
        .with_fill(Fill::Solid(Color::rgba8(245, 220, 40, 255)));
        let pc_id = doc.add_object(pc, Some(lid)).unwrap();
        doc.create_powerclip(pc_id, &[inner_id]).unwrap();
        apply_command(
            &mut doc,
            &CommandOp::SetObjectPerspective {
                ids: vec![pc_id],
                perspective: Some(persp),
            },
        )
        .unwrap();
        if with_shadow {
            apply_command(
                &mut doc,
                &CommandOp::AddEffect {
                    id: pc_id,
                    effect: Effect::DropShadow {
                        dx_mm: 3.0,
                        dy_mm: 3.0,
                        blur_mm: 2.0,
                        opacity: 0.6,
                        color: Color::BLACK,
                        feather_direction: FeatherDirection::Outside,
                    },
                },
            )
            .unwrap();
        }

        let render_img = render_page_to_rgba(&doc, 0, 150.0, ViewMode::Enhanced).unwrap();
        let (r_cnt, r_bb) = red_bbox(&render_img.rgba, render_img.width, render_img.height);

        let svg = export_svg_to_string(&doc, 0).unwrap();
        let svg_buf = render_svg_to_rgba(svg.as_bytes(), render_img.width, render_img.height)
            .expect("resvg render");
        let (s_cnt, s_bb) = red_bbox(&svg_buf.rgba, svg_buf.width, svg_buf.height);
        assert!(
            (r_cnt as isize - s_cnt as isize).abs() < 120
                && (r_bb.0 - s_bb.0).abs() < 0.8
                && (r_bb.1 - s_bb.1).abs() < 0.8
                && (r_bb.2 - s_bb.2).abs() < 0.8
                && (r_bb.3 - s_bb.3).abs() < 0.8,
            "with_shadow={with_shadow}: Render ({r_cnt} px, {r_bb:?}) vs SVG ({s_cnt} px, {s_bb:?})"
        );

        let pdf_bytes = export_pdf_to_bytes(
            &doc,
            &PdfExportOptions {
                color_space: PdfColorSpace::DeviceRgb,
                include_bleed: false,
                ..PdfExportOptions::default()
            },
        )
        .unwrap();
        if std::path::Path::new("/usr/bin/pdftoppm").exists() {
            let dir = tempfile::tempdir().unwrap();
            let pdf_path = dir.path().join("pc.pdf");
            let png_prefix = dir.path().join("pc_page");
            std::fs::write(&pdf_path, &pdf_bytes).unwrap();
            let st = std::process::Command::new("/usr/bin/pdftoppm")
                .args([
                    "-png",
                    "-r",
                    "150",
                    "-cropbox",
                    "-singlefile",
                    &pdf_path.display().to_string(),
                    &png_prefix.display().to_string(),
                ])
                .status()
                .unwrap();
            assert!(st.success());
            let img = image::open(dir.path().join("pc_page.png"))
                .unwrap()
                .to_rgba8();
            let (p_cnt, p_bb) = red_bbox(img.as_raw(), img.width(), img.height());
            assert!(
                (r_cnt as isize - p_cnt as isize).abs() < 120
                    && (r_bb.0 - p_bb.0).abs() < 0.8
                    && (r_bb.1 - p_bb.1).abs() < 0.8
                    && (r_bb.2 - p_bb.2).abs() < 0.8
                    && (r_bb.3 - p_bb.3).abs() < 0.8,
                "with_shadow={with_shadow}: Render ({r_cnt} px, {r_bb:?}) vs PDF ({p_cnt} px, {p_bb:?})"
            );
        }

        let eps = export_eps_to_string(&doc, 0).unwrap();
        if std::path::Path::new("/usr/bin/gs").exists() {
            let dir = tempfile::tempdir().unwrap();
            let eps_path = dir.path().join("pc.eps");
            let png_path = dir.path().join("pc_eps.png");
            std::fs::write(&eps_path, eps.as_bytes()).unwrap();
            let st = std::process::Command::new("/usr/bin/gs")
                .args([
                    "-q",
                    "-dNOPAUSE",
                    "-dBATCH",
                    "-dEPSCrop",
                    "-sDEVICE=png16m",
                    "-r150",
                    &format!("-sOutputFile={}", png_path.display()),
                    &eps_path.display().to_string(),
                ])
                .status()
                .unwrap();
            assert!(st.success());
            let img = image::open(&png_path).unwrap().to_rgba8();
            let (e_cnt, e_bb) = red_bbox(img.as_raw(), img.width(), img.height());
            assert!(
                (r_cnt as isize - e_cnt as isize).abs() < 150
                    && (r_bb.0 - e_bb.0).abs() < 0.8
                    && (r_bb.1 - e_bb.1).abs() < 0.8
                    && (r_bb.2 - e_bb.2).abs() < 0.8
                    && (r_bb.3 - e_bb.3).abs() < 0.8,
                "with_shadow={with_shadow}: Render ({r_cnt} px, {r_bb:?}) vs EPS ({e_cnt} px, {e_bb:?})"
            );
        }
    }

    // Nested Group(Perspective3x3) -> PowerClip(Perspective3x3) across Render & SVG
    let mut nested_doc = Document::new("NestedPCIO", 160.0, 160.0, ColorMode::Rgb, 150.0);
    let lid = nested_doc.pages()[0].layers[0].id;
    let inner = SceneObject::new(
        ObjectId(0),
        "RedInner",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            2.0, 2.0, 20.0, 8.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgba8(220, 20, 20, 255)));
    let inner_id = nested_doc.add_object(inner, Some(lid)).unwrap();
    let pc = SceneObject::new(
        ObjectId(0),
        "NestedPC",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 24.0, 24.0,
        ))),
    )
    .with_fill(Fill::Solid(Color::rgba8(245, 220, 40, 255)));
    let pc_id = nested_doc.add_object(pc, Some(lid)).unwrap();
    nested_doc.create_powerclip(pc_id, &[inner_id]).unwrap();
    apply_command(
        &mut nested_doc,
        &CommandOp::SetObjectPerspective {
            ids: vec![pc_id],
            perspective: Some(persp),
        },
    )
    .unwrap();
    let gid = nested_doc.group_objects(&[pc_id], "OuterGrp").unwrap();
    apply_command(
        &mut nested_doc,
        &CommandOp::TransformObjects {
            ids: vec![gid],
            affine: Affine2D::translate(50.0, 40.0),
        },
    )
    .unwrap();
    let grp_persp = Perspective3x3::from_rect_to_quad(
        Rect2D::from_xywh(0.0, 0.0, 24.0, 24.0),
        &[
            Point2D::new(2.0, 2.0),
            Point2D::new(22.0, 2.0),
            Point2D::new(24.0, 24.0),
            Point2D::new(0.0, 24.0),
        ],
    )
    .unwrap();
    apply_command(
        &mut nested_doc,
        &CommandOp::SetObjectPerspective {
            ids: vec![gid],
            perspective: Some(grp_persp),
        },
    )
    .unwrap();

    let n_render = render_page_to_rgba(&nested_doc, 0, 150.0, ViewMode::Enhanced).unwrap();
    let (nr_cnt, nr_bb) = red_bbox(&n_render.rgba, n_render.width, n_render.height);
    let n_svg = export_svg_to_string(&nested_doc, 0).unwrap();
    let n_svg_buf = render_svg_to_rgba(n_svg.as_bytes(), n_render.width, n_render.height).unwrap();
    let (ns_cnt, ns_bb) = red_bbox(&n_svg_buf.rgba, n_svg_buf.width, n_svg_buf.height);
    assert!(
        (nr_cnt as isize - ns_cnt as isize).abs() < 120
            && (nr_bb.0 - ns_bb.0).abs() < 0.8
            && (nr_bb.1 - ns_bb.1).abs() < 0.8
            && (nr_bb.2 - ns_bb.2).abs() < 0.8
            && (nr_bb.3 - ns_bb.3).abs() < 0.8,
        "Nested Group(Persp)->PowerClip(Persp): Render ({nr_cnt} px, {nr_bb:?}) vs SVG ({ns_cnt} px, {ns_bb:?})"
    );
}

#[test]
fn w2_exit_r4_symbol_def_text_on_path_and_connector_svg_pdf_eps_parity() {
    use coree_core::{Affine2D, Perspective3x3, TextOnPath};
    use coree_io::{export_eps_to_string, export_svg_to_string};
    use coree_render::{render_page_to_rgba, render_svg_to_rgba, ViewMode};

    let green_bbox = |rgba: &[u8], w: u32, h: u32| -> (usize, (f64, f64, f64, f64)) {
        let px_per_mm = 150.0 / 25.4;
        let mut count = 0usize;
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                let [r, g, b, a] = [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]];
                if a > 128 && g > 140 && r < 80 && b < 80 {
                    count += 1;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        assert!(count > 0, "expected green connector pixels");
        (
            count,
            (
                f64::from(min_x) / px_per_mm,
                f64::from(min_y) / px_per_mm,
                f64::from(max_x + 1) / px_per_mm,
                f64::from(max_y + 1) / px_per_mm,
            ),
        )
    };

    let red_bbox = |rgba: &[u8], w: u32, h: u32| -> (usize, (f64, f64, f64, f64)) {
        let px_per_mm = 150.0 / 25.4;
        let mut count = 0usize;
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                let [r, g, b, a] = [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]];
                if a > 128 && r > 160 && g < 80 && b < 80 {
                    count += 1;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        assert!(count > 0, "expected red TextOnPath pixels");
        (
            count,
            (
                f64::from(min_x) / px_per_mm,
                f64::from(min_y) / px_per_mm,
                f64::from(max_x + 1) / px_per_mm,
                f64::from(max_y + 1) / px_per_mm,
            ),
        )
    };

    for with_persp in [false, true] {
        let mut doc = Document::new("SymTOPConn", 180.0, 180.0, ColorMode::Rgb, 150.0);
        let sym_host = SceneObject::new(
            ObjectId(10),
            "SymHost",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(5.0, 20.0),
                Point2D::new(45.0, 20.0),
            )),
        )
        .with_outline(OutlineStyle::solid(Color::BLACK, 0.5));
        let top_style = TextStyle {
            fill: Color::rgba8(220, 20, 20, 255),
            font_size_pt: 14.0,
            ..Default::default()
        };
        let sym_top = SceneObject::new(
            ObjectId(11),
            "SymTOP",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain("SYM", top_style),
                path_id: ObjectId(10),
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        );
        let sym_a = SceneObject::new(
            ObjectId(12),
            "SymA",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                5.0, 35.0, 10.0, 10.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(40, 100, 220, 255)));
        let sym_b = SceneObject::new(
            ObjectId(13),
            "SymB",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                35.0, 35.0, 10.0, 10.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(40, 100, 220, 255)));
        let sym_conn = SceneObject::new(
            ObjectId(14),
            "SymConn",
            NodePayload::Connector(ConnectorObject::between_objects(
                ObjectId(12),
                AnchorPoint::Center,
                ObjectId(13),
                AnchorPoint::Center,
                ConnectorStyle::Straight,
            )),
        )
        .with_outline(OutlineStyle::solid(Color::rgba8(20, 180, 40, 255), 1.2));
        // Wrap SymTOP and SymConn in a nested Group inside SymbolDef to verify sym_root_xf preservation!
        let sym_grp = SceneObject::new(
            ObjectId(15),
            "SymInnerGrp",
            NodePayload::Group(vec![ObjectId(11), ObjectId(14)]),
        )
        .with_transform(Affine2D::translate(2.0, 0.0));

        let sym = SymbolDef::new(
            "WidgetSym",
            vec![sym_host, sym_a, sym_b, sym_grp, sym_top, sym_conn],
        );
        let sid = sym.id;
        doc.insert_symbol(sym);

        let mut inst = SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid))
            .with_transform(Affine2D::translate(50.0, 40.0));
        if with_persp {
            inst.perspective = Some(
                Perspective3x3::from_rect_to_quad(
                    Rect2D::from_xywh(0.0, 0.0, 50.0, 50.0),
                    &[
                        Point2D::new(4.0, 2.0),
                        Point2D::new(46.0, 4.0),
                        Point2D::new(48.0, 48.0),
                        Point2D::new(2.0, 46.0),
                    ],
                )
                .unwrap(),
            );
        }
        doc.add_object(inst, None).unwrap();

        let render_img = render_page_to_rgba(&doc, 0, 150.0, ViewMode::Enhanced).unwrap();
        let (_, r_green_bb) = green_bbox(&render_img.rgba, render_img.width, render_img.height);
        let (_, r_red_bb) = red_bbox(&render_img.rgba, render_img.width, render_img.height);
        // Connector must live inside symbol around x=58..92, y=75..85 (NEVER at page origin 0..20!)
        assert!(
            r_green_bb.0 > 50.0 && r_green_bb.1 > 70.0,
            "with_persp={with_persp}: SceneRenderer green connector bbox {r_green_bb:?}"
        );
        assert!(
            r_red_bb.0 > 50.0 && r_red_bb.1 > 50.0,
            "with_persp={with_persp}: SceneRenderer red TOP bbox {r_red_bb:?}"
        );

        let svg = export_svg_to_string(&doc, 0).unwrap();
        assert!(
            !svg.contains("M -50.0000 -40.0000"),
            "with_persp={with_persp}: SVG Connector must NOT teleport to page origin: {svg}"
        );
        if !with_persp {
            assert!(
                svg.contains("<textPath href=\"#textpath-")
                    && svg.contains("d=\"M 5.0000 20.0000 L 45.0000 20.0000\""),
                "SVG TextOnPath inside SymbolDef must emit <textPath> along SymHost (5..45, 20): {svg}"
            );
        }
        let svg_buf =
            render_svg_to_rgba(svg.as_bytes(), render_img.width, render_img.height).expect("resvg");
        let (_, s_green_bb) = green_bbox(&svg_buf.rgba, svg_buf.width, svg_buf.height);
        assert!(
            (r_green_bb.0 - s_green_bb.0).abs() < 1.5
                && (r_green_bb.1 - s_green_bb.1).abs() < 1.5
                && (r_green_bb.2 - s_green_bb.2).abs() < 1.5
                && (r_green_bb.3 - s_green_bb.3).abs() < 1.5,
            "with_persp={with_persp}: Connector Render {r_green_bb:?} vs SVG {s_green_bb:?}"
        );
        if with_persp {
            let (_, s_red_bb) = red_bbox(&svg_buf.rgba, svg_buf.width, svg_buf.height);
            assert!(
                s_red_bb.0 > 50.0 && s_red_bb.1 > 50.0 && (r_red_bb.1 - s_red_bb.1).abs() < 2.5,
                "with_persp={with_persp}: TOP Render {r_red_bb:?} vs SVG {s_red_bb:?}"
            );
        }

        let pdf_bytes = export_pdf_to_bytes(
            &doc,
            &PdfExportOptions {
                color_space: PdfColorSpace::DeviceRgb,
                include_bleed: false,
                ..PdfExportOptions::default()
            },
        )
        .unwrap();
        if std::path::Path::new("/usr/bin/pdftoppm").exists() {
            let dir = tempfile::tempdir().unwrap();
            let pdf_path = dir.path().join("sym.pdf");
            let png_prefix = dir.path().join("sym_page");
            std::fs::write(&pdf_path, &pdf_bytes).unwrap();
            let st = std::process::Command::new("/usr/bin/pdftoppm")
                .args([
                    "-png",
                    "-r",
                    "150",
                    "-cropbox",
                    "-singlefile",
                    &pdf_path.display().to_string(),
                    &png_prefix.display().to_string(),
                ])
                .status()
                .unwrap();
            assert!(st.success());
            let img = image::open(dir.path().join("sym_page.png"))
                .unwrap()
                .to_rgba8();
            let (_, p_green_bb) = green_bbox(img.as_raw(), img.width(), img.height());
            let (_, p_red_bb) = red_bbox(img.as_raw(), img.width(), img.height());
            assert!(
                (r_green_bb.0 - p_green_bb.0).abs() < 1.5
                    && (r_green_bb.1 - p_green_bb.1).abs() < 1.5
                    && (r_green_bb.2 - p_green_bb.2).abs() < 1.5
                    && (r_green_bb.3 - p_green_bb.3).abs() < 1.5,
                "with_persp={with_persp}: Connector Render {r_green_bb:?} vs PDF {p_green_bb:?}"
            );
            assert!(
                p_red_bb.0 > 50.0 && p_red_bb.1 > 50.0 && (r_red_bb.1 - p_red_bb.1).abs() < 2.5,
                "with_persp={with_persp}: TOP Render {r_red_bb:?} vs PDF {p_red_bb:?}"
            );
        }

        let eps = export_eps_to_string(&doc, 0).unwrap();
        if std::path::Path::new("/usr/bin/gs").exists() {
            let dir = tempfile::tempdir().unwrap();
            let eps_path = dir.path().join("sym.eps");
            let png_path = dir.path().join("sym_eps.png");
            std::fs::write(&eps_path, eps.as_bytes()).unwrap();
            let st = std::process::Command::new("/usr/bin/gs")
                .args([
                    "-q",
                    "-dNOPAUSE",
                    "-dBATCH",
                    "-dEPSCrop",
                    "-sDEVICE=png16m",
                    "-r150",
                    &format!("-sOutputFile={}", png_path.display()),
                    &eps_path.display().to_string(),
                ])
                .status()
                .unwrap();
            assert!(st.success());
            let img = image::open(&png_path).unwrap().to_rgba8();
            let (_, e_green_bb) = green_bbox(img.as_raw(), img.width(), img.height());
            let (_, e_red_bb) = red_bbox(img.as_raw(), img.width(), img.height());
            assert!(
                (r_green_bb.0 - e_green_bb.0).abs() < 1.5
                    && (r_green_bb.1 - e_green_bb.1).abs() < 1.5
                    && (r_green_bb.2 - e_green_bb.2).abs() < 1.5
                    && (r_green_bb.3 - e_green_bb.3).abs() < 1.5,
                "with_persp={with_persp}: Connector Render {r_green_bb:?} vs EPS {e_green_bb:?}"
            );
            assert!(
                e_red_bb.0 > 50.0 && e_red_bb.1 > 50.0 && (r_red_bb.1 - e_red_bb.1).abs() < 2.5,
                "with_persp={with_persp}: TOP Render {r_red_bb:?} vs EPS {e_red_bb:?}"
            );
        }
    }
}

#[test]
fn w2_exit_r5_symbol_text_on_path_bspline_media_clone_persp_and_unattached_cross_oracle() {
    use coree_core::{Affine2D, ArtisticMediaStyle, BSplineCurve, Perspective3x3, TextOnPath};
    use coree_io::{export_eps_to_string, export_svg_to_string};
    use coree_render::{render_page_to_rgba, render_svg_to_rgba, ViewMode};

    let red_bbox = |rgba: &[u8], w: u32, h: u32| -> (usize, (f64, f64, f64, f64)) {
        let px_per_mm = 150.0 / 25.4;
        let mut count = 0usize;
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                let [r, g, b, a] = [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]];
                if a > 128 && r > 160 && g < 80 && b < 80 {
                    count += 1;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        assert!(count > 0, "expected red TextOnPath pixels");
        (
            count,
            (
                f64::from(min_x) / px_per_mm,
                f64::from(min_y) / px_per_mm,
                f64::from(max_x + 1) / px_per_mm,
                f64::from(max_y + 1) / px_per_mm,
            ),
        )
    };

    let assert_bbox_parity = |label: &str, a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| {
        let diff = (a.0 - b.0)
            .abs()
            .max((a.1 - b.1).abs())
            .max((a.2 - b.2).abs())
            .max((a.3 - b.3).abs());
        assert!(
            diff <= 1.5,
            "{label}: bbox mismatch {a:?} vs {b:?} (max_diff={diff:.3} mm > 1.5 mm)"
        );
    };

    let render_pdf_bb = |doc: &Document| -> Option<(f64, f64, f64, f64)> {
        if !std::path::Path::new("/usr/bin/pdftoppm").exists() {
            return None;
        }
        let pdf_bytes = export_pdf_to_bytes(
            doc,
            &PdfExportOptions {
                color_space: PdfColorSpace::DeviceRgb,
                include_bleed: false,
                ..PdfExportOptions::default()
            },
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pdf_path = dir.path().join("t.pdf");
        let png_prefix = dir.path().join("p");
        std::fs::write(&pdf_path, &pdf_bytes).unwrap();
        let st = std::process::Command::new("/usr/bin/pdftoppm")
            .args([
                "-png",
                "-r",
                "150",
                "-cropbox",
                "-singlefile",
                &pdf_path.display().to_string(),
                &png_prefix.display().to_string(),
            ])
            .status()
            .unwrap();
        assert!(st.success());
        let img = image::open(dir.path().join("p.png")).unwrap().to_rgba8();
        Some(red_bbox(img.as_raw(), img.width(), img.height()).1)
    };

    let render_eps_bb = |doc: &Document| -> Option<(f64, f64, f64, f64)> {
        if !std::path::Path::new("/usr/bin/gs").exists() {
            return None;
        }
        let eps = export_eps_to_string(doc, 0).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let eps_path = dir.path().join("t.eps");
        let png_path = dir.path().join("e.png");
        std::fs::write(&eps_path, eps.as_bytes()).unwrap();
        let st = std::process::Command::new("/usr/bin/gs")
            .args([
                "-q",
                "-dNOPAUSE",
                "-dBATCH",
                "-dEPSCrop",
                "-sDEVICE=png16m",
                "-r150",
                &format!("-sOutputFile={}", png_path.display()),
                &eps_path.display().to_string(),
            ])
            .status()
            .unwrap();
        assert!(st.success());
        let img = image::open(&png_path).unwrap().to_rgba8();
        Some(red_bbox(img.as_raw(), img.width(), img.height()).1)
    };

    let top_style = TextStyle {
        fill: Color::rgba8(220, 20, 20, 255),
        font_size_pt: 14.0,
        ..Default::default()
    };

    // Part A: SymbolDef containing TextOnPath attached to BSpline, ArtisticMedia, Clone, and Perspective3x3 Path
    for kind in ["BSpline", "ArtisticMedia", "Clone", "PerspectivePath"] {
        let mut doc = Document::new(kind, 180.0, 180.0, ColorMode::Rgb, 150.0);
        let master_id = if kind == "Clone" {
            let mut master = SceneObject::new(
                ObjectId(0),
                "MasterLine",
                NodePayload::BSpline(BSplineCurve::from_control_points(
                    vec![
                        Point2D::new(5.0, 20.0),
                        Point2D::new(20.0, 18.0),
                        Point2D::new(35.0, 22.0),
                        Point2D::new(50.0, 20.0),
                    ],
                    3,
                    false,
                )),
            )
            .with_outline(OutlineStyle::none())
            .with_fill(Fill::None);
            master.visible = false;
            Some(doc.add_object(master, None).unwrap())
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
                            Point2D::new(0.0, 10.0),
                            Point2D::new(40.0, 0.0),
                            Point2D::new(40.0, 20.0),
                            Point2D::new(0.0, 20.0),
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
                text: ArtisticText::from_plain("HOST", top_style.clone()),
                path_id: ObjectId(10),
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        )
        .with_outline(OutlineStyle::none());

        let sym = SymbolDef::new(format!("Sym_{kind}"), vec![sym_host, sym_top]);
        let sid = sym.id;
        doc.insert_symbol(sym);
        let inst = SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid))
            .with_transform(Affine2D::translate(50.0, 60.0));
        doc.add_object(inst, None).unwrap();

        let render_img = render_page_to_rgba(&doc, 0, 150.0, ViewMode::Enhanced).unwrap();
        let (_, r_bb) = red_bbox(&render_img.rgba, render_img.width, render_img.height);
        assert!(
            r_bb.0 >= 47.0 && r_bb.1 >= 62.0,
            "{kind}: SceneRenderer must NOT fall back to (50,60) origin: {r_bb:?}"
        );

        let svg = export_svg_to_string(&doc, 0).unwrap();
        assert!(
            svg.contains("<textPath href=\"#textpath-"),
            "{kind}: SVG must emit <textPath> for attached SymbolDef host: {svg}"
        );

        if let Some(p_bb) = render_pdf_bb(&doc) {
            assert_bbox_parity(&format!("{kind} Render vs PDF"), r_bb, p_bb);
        }
        if let Some(e_bb) = render_eps_bb(&doc) {
            assert_bbox_parity(&format!("{kind} Render vs EPS"), r_bb, e_bb);
        }
    }

    // Part B: SymbolDef containing unattached TextOnPath inside SymbolInstance(Translate + Perspective3x3)
    {
        let mut doc = Document::new("SymUnattachedPerspTOP", 180.0, 180.0, ColorMode::Rgb, 150.0);
        let sym_top = SceneObject::new(
            ObjectId(11),
            "SymUnattachedTOP",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain(
                    "FALLBACK",
                    TextStyle {
                        fill: Color::rgba8(220, 20, 20, 255),
                        font_size_pt: 18.0,
                        ..Default::default()
                    },
                ),
                path_id: ObjectId(9999),
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(5.0, 10.0));
        let sym = SymbolDef::new("UnattachedSym", vec![sym_top]);
        let sid = sym.id;
        doc.insert_symbol(sym);

        let mut inst = SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid))
            .with_transform(Affine2D::translate(50.0, 40.0));
        inst.perspective = Some(
            Perspective3x3::from_rect_to_quad(
                Rect2D::from_xywh(0.0, 0.0, 45.0, 25.0),
                &[
                    Point2D::new(4.0, 2.0),
                    Point2D::new(42.0, 6.0),
                    Point2D::new(44.0, 24.0),
                    Point2D::new(2.0, 22.0),
                ],
            )
            .unwrap(),
        );
        doc.add_object(inst, None).unwrap();

        let render_img = render_page_to_rgba(&doc, 0, 150.0, ViewMode::Enhanced).unwrap();
        let (_, r_bb) = red_bbox(&render_img.rgba, render_img.width, render_img.height);
        assert!(
            r_bb.0 > 50.0 && r_bb.1 > 48.0,
            "Unattached TOP inside SymInst(Persp) render bbox unexpected: {r_bb:?}"
        );

        let svg = export_svg_to_string(&doc, 0).unwrap();
        assert!(
            !svg.contains("<text "),
            "Unattached TOP inside SymInst(Persp) must outline warped glyph paths, not emit untransformed <text>: {svg}"
        );
        let svg_buf =
            render_svg_to_rgba(svg.as_bytes(), render_img.width, render_img.height).expect("resvg");
        let (_, s_bb) = red_bbox(&svg_buf.rgba, svg_buf.width, svg_buf.height);
        assert_bbox_parity("Unattached TOP Sym(Persp) Render vs SVG", r_bb, s_bb);

        if let Some(p_bb) = render_pdf_bb(&doc) {
            assert_bbox_parity("Unattached TOP Sym(Persp) Render vs PDF", r_bb, p_bb);
        }
        if let Some(e_bb) = render_eps_bb(&doc) {
            assert_bbox_parity("Unattached TOP Sym(Persp) Render vs EPS", r_bb, e_bb);
        }
    }

    // Part C: Standalone unattached TextOnPath with own obj.perspective
    {
        let mut doc = Document::new("OwnPerspUnattachedTOP", 180.0, 180.0, ColorMode::Rgb, 150.0);
        let mut top_obj = SceneObject::new(
            ObjectId(0),
            "OwnPerspTOP",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain(
                    "WARP",
                    TextStyle {
                        fill: Color::rgba8(220, 20, 20, 255),
                        font_size_pt: 20.0,
                        ..Default::default()
                    },
                ),
                path_id: ObjectId(9999),
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(40.0, 30.0));
        top_obj.perspective = Some(
            Perspective3x3::from_rect_to_quad(
                Rect2D::from_xywh(0.0, 0.0, 30.0, 10.0),
                &[
                    Point2D::new(2.0, 4.0),
                    Point2D::new(28.0, 0.0),
                    Point2D::new(30.0, 14.0),
                    Point2D::new(0.0, 10.0),
                ],
            )
            .unwrap(),
        );
        doc.add_object(top_obj, None).unwrap();

        let render_img = render_page_to_rgba(&doc, 0, 150.0, ViewMode::Enhanced).unwrap();
        let (_, r_bb) = red_bbox(&render_img.rgba, render_img.width, render_img.height);
        let svg = export_svg_to_string(&doc, 0).unwrap();
        assert!(
            !svg.contains("<text "),
            "Own-perspective unattached TOP must outline warped glyphs in SVG: {svg}"
        );
        let svg_buf =
            render_svg_to_rgba(svg.as_bytes(), render_img.width, render_img.height).expect("resvg");
        let (_, s_bb) = red_bbox(&svg_buf.rgba, svg_buf.width, svg_buf.height);
        assert_bbox_parity("OwnPersp Unattached TOP Render vs SVG", r_bb, s_bb);

        if let Some(p_bb) = render_pdf_bb(&doc) {
            assert_bbox_parity("OwnPersp Unattached TOP Render vs PDF", r_bb, p_bb);
        }
        if let Some(e_bb) = render_eps_bb(&doc) {
            assert_bbox_parity("OwnPersp Unattached TOP Render vs EPS", r_bb, e_bb);
        }
    }
}

#[test]
fn w2_exit_r6_symbol_persp_attached_text_on_path_with_symmetry_cross_oracle() {
    use coree_core::{Affine2D, Perspective3x3, TextOnPath};
    use coree_io::{export_eps_to_string, export_svg_to_string};
    use coree_render::{render_page_to_rgba, render_svg_to_rgba, ViewMode};

    let red_bbox = |rgba: &[u8], w: u32, h: u32| -> (usize, (f64, f64, f64, f64)) {
        let px_per_mm = 150.0 / 25.4;
        let mut count = 0usize;
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                let [r, g, b, a] = [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]];
                if a > 128 && r > 160 && g < 80 && b < 80 {
                    count += 1;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        assert!(count > 0, "expected red TextOnPath pixels");
        (
            count,
            (
                f64::from(min_x) / px_per_mm,
                f64::from(min_y) / px_per_mm,
                f64::from(max_x + 1) / px_per_mm,
                f64::from(max_y + 1) / px_per_mm,
            ),
        )
    };

    let assert_bbox_parity = |label: &str, a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)| {
        let diff = (a.0 - b.0)
            .abs()
            .max((a.1 - b.1).abs())
            .max((a.2 - b.2).abs())
            .max((a.3 - b.3).abs());
        assert!(
            diff <= 1.5,
            "{label}: bbox mismatch {a:?} vs {b:?} (max_diff={diff:.3} mm > 1.5 mm)"
        );
    };

    // C3: SymbolInstance(Translate + Perspective3x3) containing attached TextOnPath with Effect::Symmetry
    for nested_group in [false, true] {
        let mut doc = Document::new(
            if nested_group {
                "C3_nested"
            } else {
                "C3_direct"
            },
            180.0,
            180.0,
            ColorMode::Rgb,
            150.0,
        );
        let sym_host = SceneObject::new(
            ObjectId(10),
            "SymHost",
            NodePayload::Path(BezierPath::from_line(
                Point2D::new(5.0, 15.0),
                Point2D::new(40.0, 15.0),
            )),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none());

        let mut sym_top = SceneObject::new(
            ObjectId(11),
            "SymTOP",
            NodePayload::TextOnPath(TextOnPath {
                text: ArtisticText::from_plain(
                    "SYM",
                    TextStyle {
                        fill: Color::rgba8(220, 20, 20, 255),
                        font_size_pt: 14.0,
                        ..Default::default()
                    },
                ),
                path_id: ObjectId(10),
                offset_mm: 0.0,
                distance_mm: 0.0,
                flip_vertical: false,
                flip_horizontal: false,
            }),
        )
        .with_outline(OutlineStyle::none());
        sym_top.effects.push(Effect::Symmetry {
            axes: 2,
            origin: Point2D::new(25.0, 35.0),
            mirror: false,
        });

        let sym_objects = if nested_group {
            let grp = SceneObject::new(
                ObjectId(12),
                "InnerGrp",
                NodePayload::Group(vec![ObjectId(10), ObjectId(11)]),
            )
            .with_transform(Affine2D::translate(3.0, 4.0));
            vec![sym_host, sym_top, grp]
        } else {
            vec![sym_host, sym_top]
        };
        let sym = SymbolDef::new("C3Sym", sym_objects);
        let sid = sym.id;
        doc.insert_symbol(sym);

        let mut inst = SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sid))
            .with_transform(Affine2D::translate(30.0, 30.0));
        inst.perspective = Some(
            Perspective3x3::from_rect_to_quad(
                Rect2D::from_xywh(0.0, 0.0, 50.0, 70.0),
                &[
                    Point2D::new(5.0, 5.0),
                    Point2D::new(48.0, 0.0),
                    Point2D::new(60.0, 75.0),
                    Point2D::new(0.0, 65.0),
                ],
            )
            .unwrap(),
        );
        doc.add_object(inst, None).unwrap();

        let render_img = render_page_to_rgba(&doc, 0, 150.0, ViewMode::Enhanced).unwrap();
        let (r_cnt, r_bb) = red_bbox(&render_img.rgba, render_img.width, render_img.height);
        assert!(
            (r_bb.3 - r_bb.1) > 25.0 && r_cnt > 400,
            "nested={nested_group}: SceneRenderer must render BOTH symmetry branches, got cnt={r_cnt}, bb={r_bb:?}"
        );

        let svg = export_svg_to_string(&doc, 0).unwrap();
        let svg_buf =
            render_svg_to_rgba(svg.as_bytes(), render_img.width, render_img.height).expect("resvg");
        let (_, s_bb) = red_bbox(&svg_buf.rgba, svg_buf.width, svg_buf.height);
        assert_bbox_parity(
            &format!("C3 (nested={nested_group}) Render vs SVG"),
            r_bb,
            s_bb,
        );

        if std::path::Path::new("/usr/bin/pdftoppm").exists() {
            let pdf_bytes = export_pdf_to_bytes(
                &doc,
                &PdfExportOptions {
                    color_space: PdfColorSpace::DeviceRgb,
                    include_bleed: false,
                    ..PdfExportOptions::default()
                },
            )
            .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let pdf_path = dir.path().join("c3.pdf");
            let png_prefix = dir.path().join("c3");
            std::fs::write(&pdf_path, &pdf_bytes).unwrap();
            let st = std::process::Command::new("/usr/bin/pdftoppm")
                .args([
                    "-png",
                    "-r",
                    "150",
                    "-cropbox",
                    "-singlefile",
                    &pdf_path.display().to_string(),
                    &png_prefix.display().to_string(),
                ])
                .status()
                .unwrap();
            assert!(st.success());
            let img = image::open(dir.path().join("c3.png")).unwrap().to_rgba8();
            let (_, p_bb) = red_bbox(img.as_raw(), img.width(), img.height());
            assert_bbox_parity(
                &format!("C3 (nested={nested_group}) Render vs PDF"),
                r_bb,
                p_bb,
            );
        }

        if std::path::Path::new("/usr/bin/gs").exists() {
            let eps = export_eps_to_string(&doc, 0).unwrap();
            let dir = tempfile::tempdir().unwrap();
            let eps_path = dir.path().join("c3.eps");
            let png_path = dir.path().join("c3_eps.png");
            std::fs::write(&eps_path, eps.as_bytes()).unwrap();
            let st = std::process::Command::new("/usr/bin/gs")
                .args([
                    "-q",
                    "-dNOPAUSE",
                    "-dBATCH",
                    "-dEPSCrop",
                    "-sDEVICE=png16m",
                    "-r150",
                    &format!("-sOutputFile={}", png_path.display()),
                    &eps_path.display().to_string(),
                ])
                .status()
                .unwrap();
            assert!(st.success());
            let img = image::open(&png_path).unwrap().to_rgba8();
            let (_, e_bb) = red_bbox(img.as_raw(), img.width(), img.height());
            assert_bbox_parity(
                &format!("C3 (nested={nested_group}) Render vs EPS"),
                r_bb,
                e_bb,
            );
        }
    }
}

#[test]
fn w2_exit_r8_symbol_zero_id_persp_group_text_on_path_svg_pdf_eps_parity() {
    use coree_core::{Affine2D, ArtisticText, Perspective3x3, SymbolDef, TextOnPath, TextStyle};
    use coree_io::{export_eps_to_string, export_svg_to_string};

    let mut doc = Document::new("W2ExitR8_P5_IO", 240.0, 240.0, ColorMode::Rgb, 150.0);
    let host = SceneObject::new(
        ObjectId(20),
        "Host20",
        NodePayload::Path(BezierPath::from_line(
            Point2D::new(0.0, 5.0),
            Point2D::new(30.0, 5.0),
        )),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    let mut top = SceneObject::new(
        ObjectId(21),
        "Top21",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain(
                "ZERO",
                TextStyle {
                    fill: Color::rgba8(220, 20, 20, 255),
                    font_size_pt: 14.0,
                    ..Default::default()
                },
            ),
            path_id: ObjectId(20),
            offset_mm: 0.0,
            distance_mm: 0.0,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    )
    .with_outline(OutlineStyle::none());
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
    let sym = SymbolDef::new("P5IOSym", vec![zero_grp, host, top]);
    let sid = sym.id;
    doc.insert_symbol(sym);
    doc.add_object(
        SceneObject::new(ObjectId(0), "P5Inst", NodePayload::SymbolInstance(sid))
            .with_transform(Affine2D::translate(20.0, 20.0)),
        None,
    )
    .unwrap();

    // SVG: SymbolInstance emits outer translate(20, 20) group, so inner warped glyph coords sit in (108..192, 108..135)
    // which compose with translate(20, 20) to (128..212, 128..155) matching PDF/EPS!
    let svg = export_svg_to_string(&doc, 0).expect("SVG export");
    assert!(
        svg.contains("transform=\"matrix(1.000000 0.000000 0.000000 1.000000 20.0000 20.0000)\""),
        "SVG must emit SymbolInstance translate(20, 20) group: {svg}"
    );
    let mut svg_pts = 0usize;
    for line in svg.lines().filter(|l| l.contains("id=\"obj-21-g")) {
        let d_start = line.find(" d=\"").unwrap() + 4;
        let d_end = line[d_start..].find('"').unwrap() + d_start;
        let nums: Vec<f64> = line[d_start..d_end]
            .split_whitespace()
            .filter_map(|t| t.parse::<f64>().ok())
            .collect();
        for pair in nums.as_chunks::<2>().0 {
            let world_x = pair[0] + 20.0;
            let world_y = pair[1] + 20.0;
            assert!(
                (128.0..=212.0).contains(&world_x) && (128.0..=155.0).contains(&world_y),
                "SVG glyph world coord ({world_x:.2}, {world_y:.2}) out of expected ObjectId(0) Group range [128..212, 128..155]"
            );
            svg_pts += 1;
        }
    }
    assert!(svg_pts >= 4, "expected SVG glyph coords for Top21");

    // PDF + pdftoppm oracle
    let pdf_bytes = export_pdf_to_bytes(
        &doc,
        &PdfExportOptions {
            color_space: PdfColorSpace::DeviceRgb,
            compress_streams: false,
            ..PdfExportOptions::default()
        },
    )
    .unwrap();
    if std::path::Path::new("/usr/bin/pdftoppm").exists() {
        let dir = tempfile::tempdir().unwrap();
        let pdf_path = dir.path().join("p5.pdf");
        let png_prefix = dir.path().join("p5");
        std::fs::write(&pdf_path, &pdf_bytes).unwrap();
        let st = std::process::Command::new("/usr/bin/pdftoppm")
            .args([
                "-png",
                "-r",
                "150",
                "-cropbox",
                "-singlefile",
                &pdf_path.display().to_string(),
                &png_prefix.display().to_string(),
            ])
            .status()
            .unwrap();
        assert!(st.success());
        let img = image::open(dir.path().join("p5.png")).unwrap().to_rgba8();
        let px_per_mm = 150.0 / 25.4;
        let bleed_mm = 3.0;
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        for (idx, px) in img.pixels().enumerate() {
            if px[3] > 128 && px[0] > 150 && px[1] < 90 && px[2] < 90 {
                let x = (idx as u32) % img.width();
                let y = (idx as u32) / img.width();
                min_x = min_x.min(f64::from(x) / px_per_mm - bleed_mm);
                min_y = min_y.min(f64::from(y) / px_per_mm - bleed_mm);
            }
        }
        assert!(
            min_x >= 128.0 && min_y >= 128.0,
            "PDF pdftoppm red glyph min=({min_x:.2}, {min_y:.2}) must not drop ObjectId(0) Group transform"
        );
    }

    // EPS + gs oracle
    let eps = export_eps_to_string(&doc, 0).unwrap();
    if std::path::Path::new("/usr/bin/gs").exists() {
        let dir = tempfile::tempdir().unwrap();
        let eps_path = dir.path().join("p5.eps");
        let png_path = dir.path().join("p5_eps.png");
        std::fs::write(&eps_path, eps.as_bytes()).unwrap();
        let st = std::process::Command::new("/usr/bin/gs")
            .args([
                "-q",
                "-dNOPAUSE",
                "-dBATCH",
                "-dEPSCrop",
                "-sDEVICE=png16m",
                "-r150",
                &format!("-sOutputFile={}", png_path.display()),
                &eps_path.display().to_string(),
            ])
            .status()
            .unwrap();
        assert!(st.success());
        let img = image::open(&png_path).unwrap().to_rgba8();
        let px_per_mm = 150.0 / 25.4;
        let mut min_x = f64::INFINITY;
        for (idx, px) in img.pixels().enumerate() {
            if px[3] > 128 && px[0] > 150 && px[1] < 90 && px[2] < 90 {
                let x = (idx as u32) % img.width();
                min_x = min_x.min(f64::from(x) / px_per_mm);
            }
        }
        assert!(
            min_x >= 128.0,
            "EPS gs red glyph min_x={min_x:.2} must include ObjectId(0) Group transform"
        );
    }
}
