// SPDX-License-Identifier: MPL-2.0

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
    doc.metadata.author = "coree QA".to_owned();

    let page1_id = doc.pages[0].id;
    let master_layer = doc
        .add_layer(page1_id, "Master Header", false, MasterLayerScope::AllPages)
        .unwrap();
    let cut_layer = doc
        .add_layer(page1_id, "CutContour", true, MasterLayerScope::None)
        .unwrap();

    // Add second page to verify multipage & master layers
    let page2_id = doc.add_page("Page 2", 210.0, 297.0, 3.0);
    let page2_layer = doc.pages.iter().find(|p| p.id == page2_id).unwrap().layers[0].id;

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
    doc.symbols.insert(sym_id, sym);

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
    doc.unknown_doc_chunks.push(UnknownCdrChunk {
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
    assert!(!reimported.objects.is_empty());

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
    let obj = sback.objects.values().next().unwrap();
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
        document_id: doc.id,
        title: doc.title.clone(),
        created_at: doc.metadata.created_at,
        modified_at: doc.metadata.modified_at,
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
    let wood_layer = imported.pages[0]
        .layers
        .iter()
        .find(|l| l.name == "Woodcut")
        .expect("Woodcut layer");
    assert!(
        !wood_layer.is_cut_layer,
        "Woodcut must not be falsely tagged as cut layer"
    );
    let laser_layer = imported.pages[0]
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
        .objects
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
    doc.objects.get_mut(&grp_id).unwrap().transform = Affine2D::translate(50.0, 0.0);

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
    assert_eq!(imported_pdf.pages.len(), doc.pages.len());
    assert_eq!(imported_ai.pages.len(), doc.pages.len());
    assert!(!imported_pdf.objects.is_empty());
    assert!(!imported_pdf.assets.is_empty());
    assert_eq!(imported_pdf.metadata.source_format.as_deref(), Some("pdf"));

    // 2. EPS file round-trip
    let eps_path = dir.path().join("page1.eps");
    export_eps(&doc, 0, &eps_path).unwrap();
    let imported_eps = coree_io::import_eps(&eps_path).unwrap();
    assert_eq!(imported_eps.pages.len(), 1);
    assert!((imported_eps.pages[0].width_mm - 210.0).abs() < 0.2);
    assert!((imported_eps.pages[0].height_mm - 297.0).abs() < 0.2);
    assert!(!imported_eps.objects.is_empty());
    assert_eq!(imported_eps.metadata.source_format.as_deref(), Some("eps"));

    // 3. DXF file round-trip
    let dxf_path = dir.path().join("cut.dxf");
    export_dxf(&doc, 0, &dxf_path).unwrap();
    let imported_dxf = coree_io::import_dxf(&dxf_path).unwrap();
    assert_eq!(imported_dxf.pages.len(), 1);
    assert!(imported_dxf.pages[0]
        .layers
        .iter()
        .any(|l| l.name.eq_ignore_ascii_case("CutContour") && l.is_cut_layer));
    assert!(!imported_dxf.objects.is_empty());
    assert_eq!(imported_dxf.metadata.source_format.as_deref(), Some("dxf"));

    // 4. HPGL `.plt` file round-trip
    let plt_path = dir.path().join("cut.plt");
    export_plt(&doc, 0, &plt_path).unwrap();
    let imported_plt = coree_io::import_plt(&plt_path).unwrap();
    assert_eq!(imported_plt.pages.len(), 1);
    assert!(!imported_plt.objects.is_empty());
    assert_eq!(imported_plt.metadata.source_format.as_deref(), Some("plt"));

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
    assert_eq!(imported_psd.pages.len(), 1);
    assert_eq!(imported_psd.assets.len(), 1);
    assert_eq!(imported_psd.metadata.source_format.as_deref(), Some("psd"));
}

#[test]
fn optional_external_real_file_smoke_check() {
    if let Ok(pdf_path) = std::env::var("COREE_VERIFY_PDF") {
        let p = std::path::Path::new(&pdf_path);
        let t0 = std::time::Instant::now();
        let doc = coree_io::import_pdf(p).expect("real PDF must import cleanly");
        let elapsed = t0.elapsed();
        assert!(!doc.pages.is_empty());
        assert!(!doc.objects.is_empty());
        eprintln!(
            "[VERIFY PDF] pages={} objects={} assets={} page0={:.2}x{:.2}mm mode={:?} elapsed={:.2?}",
            doc.pages.len(),
            doc.objects.len(),
            doc.assets.len(),
            doc.pages[0].width_mm,
            doc.pages[0].height_mm,
            doc.color_mode,
            elapsed
        );
    }
    if let Ok(psd_path) = std::env::var("COREE_VERIFY_PSD") {
        let p = std::path::Path::new(&psd_path);
        let t0 = std::time::Instant::now();
        let doc = coree_io::import_psd(p).expect("real PSD must import cleanly");
        let elapsed = t0.elapsed();
        assert_eq!(doc.pages.len(), 1);
        assert!(!doc.objects.is_empty());
        assert!(!doc.assets.is_empty());
        let total_rgba: usize = doc.assets.values().map(|a| a.rgba_bytes.len()).sum();
        eprintln!(
            "[VERIFY PSD] layers={} objects={} assets={} page={:.2}x{:.2}mm dpi={:.1} total_rgba={:.1}MiB elapsed={:.2?}",
            doc.pages[0].layers.len(),
            doc.objects.len(),
            doc.assets.len(),
            doc.pages[0].width_mm,
            doc.pages[0].height_mm,
            doc.dpi,
            (total_rgba as f64) / (1024.0 * 1024.0),
            elapsed
        );
    }
}
