// SPDX-License-Identifier: MPL-2.0

//! End-to-end integration tests for `coree-cdr`:
//! - Round-trip fidelity across CDR RIFF v12, CDR X4 ZIP, CDR X6+ ZIP, CDT, and CMX (16/32-bit)
//! - Multi-layer CPT PHOTO-PAINT import (Raw, PackBits RLE, Zlib, Deflate, RGB/CMYK/Gray + Alpha Masks)
//! - Legacy Arabic Windows-1256 (CP1256) explicit & auto-detected decoding
//! - Unknown-Chunk Passthrough byte-for-byte preservation across container generations
//! - CDR Structural Inspector accuracy & report formatting
//! - Hardened adversarial security & deterministic truncation/mutation fuzz suite (zero panics!)

use coree_cdr::{
    cdr_riff::{
        decode_dib_to_rgba, encode_bmp_file_rgba, encode_dib_rgba, write_cdr_riff_with_options,
        RiffWriteOptions,
    },
    cdr_zip::CdrZipEra,
    cpt::{write_cpt_bytes, CptColorModel, CptCompression, CptLayerSpec},
    export_cdr, export_cdr_bytes, export_cdt, export_cdt_bytes, export_cmx, export_cmx_bytes,
    export_cmx_bytes_with_endian,
    fixtures::{
        malformed_cmpr_zlib_bomb_bytes, malformed_deep_nested_riff_bytes, malformed_garbage_bytes,
        malformed_huge_bitmap_cdr_bytes, malformed_huge_bitmap_cpt_bytes,
        malformed_truncated_chunk, malformed_truncated_header, malformed_zip_bomb_bytes,
        malformed_zip_slip_bytes, sample_cdr_riff_v12_bytes, sample_cdr_zip_x4_bytes,
        sample_cdr_zip_x6_bytes, sample_cdt_bytes, sample_cmx_16bit_bytes, sample_cmx_32bit_bytes,
        sample_cp1256_arabic_cdr_bytes, sample_cpt_multilayer_bytes, sample_cpt_riff_bytes,
        sample_unknown_chunk_cdr_bytes,
    },
    format_report, generate_sample_document, import_cdt_bytes, import_corel_bytes,
    import_corel_bytes_with_limits, import_corel_file, import_corel_file_with_limits,
    inspect_corel_bytes, inspect_corel_file, inspect_corel_file_with_limits,
    riff::{write_riff, RiffDocument, RiffEndian, RiffNode},
    CdrError, CdrWriteVersion, CmxPrecision, CodecLimits, CorelFormat,
};
use coree_core::{
    BasicShapeKind, BezierPath, BlendMode, Color, ColorMapMode, ColorMode, CornerStyle, Document,
    Fill, ImpactStyle, LensKind, LineCap, LineJoin, MeshNode, NodeKind, NodePayload, ObjectId,
    ParametricShape, Point2D, Rect2D, SceneObject, TextAlign, TextDirection,
};
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "coree_cdr_test_{prefix}_{}_{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        Self { path: dir }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn find_obj_by_name<'a>(doc: &'a Document, name: &str) -> &'a SceneObject {
    doc.objects
        .values()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("expected object named '{name}' in document"))
}

fn assert_showcase_cdr_fidelity(orig: &Document, restored: &Document) {
    // 1. Multi-page structure
    assert_eq!(restored.pages.len(), 2);
    assert!((restored.pages[0].width_mm - 210.0).abs() < 0.01);
    assert!((restored.pages[0].height_mm - 297.0).abs() < 0.01);
    assert!((restored.pages[1].width_mm - 215.9).abs() < 0.01);
    assert!((restored.pages[1].height_mm - 279.4).abs() < 0.01);
    assert_eq!(restored.pages[1].name, "Back Cover");

    // 2. Layers + CutContour flag
    assert_eq!(restored.pages[0].layers.len(), 2);
    assert!(restored.pages[0].layers[1].is_cut_layer);
    assert_eq!(restored.pages[0].layers[1].name, "CutContour");

    // 3. Hero Curve: cubic handles, NodeKind (Symmetrical/Smooth/Cusp), closed flag, gradient, dash outline, affine transform
    let curve = find_obj_by_name(restored, "Hero Curve");
    match &curve.payload {
        NodePayload::Path(p) => {
            assert_eq!(p.subpaths.len(), 1);
            let sp = &p.subpaths[0];
            assert!(sp.closed);
            assert_eq!(sp.nodes.len(), 4);
            assert_eq!(sp.nodes[0].kind, NodeKind::Symmetrical);
            assert_eq!(sp.nodes[1].kind, NodeKind::Smooth);
            assert_eq!(sp.nodes[2].kind, NodeKind::Cusp);
            assert!(sp.nodes[0].handle_in.is_some());
            assert!(sp.nodes[0].handle_out.is_some());
            assert!(sp.nodes[1].handle_in.is_some());
            assert!(sp.nodes[1].handle_out.is_some());
            assert!(sp.nodes[2].handle_in.is_none());
            assert!(sp.nodes[2].handle_out.is_none());
            assert!((sp.nodes[0].pos.x - 10.0).abs() < 0.01);
            assert!((sp.nodes[1].pos.x - 50.0).abs() < 0.01);
        }
        other => panic!("expected Path for Hero Curve, got {other:?}"),
    }
    match &curve.fill {
        Fill::LinearGradient { stops, .. } => assert_eq!(stops.len(), 3),
        other => panic!("expected LinearGradient, got {other:?}"),
    }
    assert_eq!(curve.outline.cap, LineCap::Round);
    assert_eq!(curve.outline.join, LineJoin::Bevel);
    assert!((curve.outline.width_mm - 0.75).abs() < 0.01);
    assert_eq!(curve.outline.dash_pattern.len(), 2);
    assert!((curve.outline.dash_pattern[0] - 3.0).abs() < 0.01);
    let orig_curve = find_obj_by_name(orig, "Hero Curve");
    for i in 0..6 {
        assert!(
            (curve.transform.m[i] - orig_curve.transform.m[i]).abs() < 1e-4,
            "affine mismatch at index {i}"
        );
    }

    // 4. Group & nested ParametricShapes + Radial/Conical/Square gradients
    let grp = find_obj_by_name(restored, "Logo Group");
    match &grp.payload {
        NodePayload::Group(children) => assert_eq!(children.len(), 2),
        other => panic!("expected Group, got {other:?}"),
    }
    let rect = find_obj_by_name(restored, "Card Base");
    match &rect.payload {
        NodePayload::Shape(ParametricShape::Rectangle {
            width,
            height,
            corner_radii,
            corner_style,
            lock_corners,
        }) => {
            assert!((*width - 85.0).abs() < 0.01);
            assert!((*height - 55.0).abs() < 0.01);
            assert!((corner_radii[0] - 4.0).abs() < 0.01);
            assert_eq!(*corner_style, CornerStyle::Rounded);
            assert!(*lock_corners);
        }
        other => panic!("expected Rectangle, got {other:?}"),
    }
    assert!(matches!(rect.fill, Fill::RadialGradient { .. }));

    let ell = find_obj_by_name(restored, "Badge Circle");
    assert!(matches!(ell.fill, Fill::ConicalGradient { .. }));

    let hex = find_obj_by_name(restored, "Hex Emblem");
    assert!(matches!(hex.fill, Fill::SquareGradient { .. }));

    // 5. UTF-8 English & Arabic RTL + Kashida Text + Paragraph Text
    let en = find_obj_by_name(restored, "Title English");
    match &en.payload {
        NodePayload::ArtisticText(t) => {
            assert_eq!(t.plain_text(), "coree Clean-Room Vector Studio");
            assert!(t.runs[0].style.bold);
        }
        other => panic!("expected ArtisticText, got {other:?}"),
    }
    let ar = find_obj_by_name(restored, "Arabic Headline");
    match &ar.payload {
        NodePayload::ArtisticText(t) => {
            assert_eq!(t.plain_text(), "مرحباً بكم في كوري");
            assert_eq!(t.direction, TextDirection::Rtl);
            assert_eq!(t.align, TextAlign::ForceJustifyKashida);
            assert!((t.runs[0].style.kashida_stretch - 0.75).abs() < 1e-4);
        }
        other => panic!("expected Arabic ArtisticText, got {other:?}"),
    }
    let para = find_obj_by_name(restored, "Body Paragraph");
    match &para.payload {
        NodePayload::ParagraphText(p) => {
            assert_eq!(p.plain_text(), "Precision print & signage vector artwork.");
            assert!((p.frame_size.x - 90.0).abs() < 0.01);
            assert!((p.frame_size.y - 30.0).abs() < 0.01);
        }
        other => panic!("expected ParagraphText, got {other:?}"),
    }

    // 6. Embedded Bitmap pixel-for-pixel fidelity
    let orig_asset = orig.assets.values().next().expect("orig asset");
    let rest_bmp_obj = find_obj_by_name(restored, "Embedded Swatch Bitmap");
    match &rest_bmp_obj.payload {
        NodePayload::Raster(r) => {
            let asset = restored.assets.get(&r.asset_id).expect("restored asset");
            assert_eq!(asset.width, orig_asset.width);
            assert_eq!(asset.height, orig_asset.height);
            assert_eq!(asset.rgba_bytes, orig_asset.rgba_bytes);
            assert!((r.width_mm - 24.0).abs() < 0.01);
        }
        other => panic!("expected Raster, got {other:?}"),
    }

    // 7. ALL 11 Lenses + frozen / viewpoint / remove_face
    let mut lens_kinds_seen = Vec::new();
    for i in 1..=11 {
        let obj = find_obj_by_name(restored, &format!("Lens {i}"));
        let l = obj
            .lens
            .as_ref()
            .unwrap_or_else(|| panic!("Lens {i} missing LensConfig"));
        lens_kinds_seen.push(l.kind.name());
        if i == 2 {
            assert!(l.frozen);
        }
        if i == 3 {
            let vp = l.viewpoint.expect("Lens 3 viewpoint");
            assert!((vp.x - 40.0).abs() < 0.01);
            assert!((vp.y - 60.0).abs() < 0.01);
        }
        if i == 4 {
            assert!(l.remove_face);
            assert!(matches!(
                l.kind,
                LensKind::CustomColorMap {
                    palette_mode: ColorMapMode::ForwardRainbow,
                    ..
                }
            ));
        }
        if i == 8 {
            assert!(l.frozen && l.remove_face && l.viewpoint.is_some());
        }
    }
    assert_eq!(lens_kinds_seen.len(), LensKind::VARIANT_COUNT);
    assert_eq!(
        lens_kinds_seen,
        vec![
            "Brighten",
            "Color Add",
            "Color Limit",
            "Custom Color Map",
            "Fish Eye",
            "Heat Map",
            "Invert",
            "Magnify",
            "Tinted Grayscale",
            "Transparency",
            "Wireframe"
        ]
    );

    // 8. CutContour hairline spot outline
    let cut = find_obj_by_name(restored, "Die Cut Outline");
    assert!(cut.outline.is_hairline);
    assert!(cut.outline.overprint);
    assert!(matches!(
        &cut.outline.color,
        Color::Spot { name, .. } if name == "CutContour"
    ));

    // 9. Unknown-Chunk Passthrough on document and object
    assert_eq!(restored.unknown_doc_chunks.len(), 1);
    assert_eq!(&restored.unknown_doc_chunks[0].fourcc, b"XDOC");
    assert_eq!(
        restored.unknown_doc_chunks[0].raw_bytes,
        b"coree-passthrough-doc-payload!"
    );
    assert_eq!(curve.unknown_chunks.len(), 1);
    assert_eq!(&curve.unknown_chunks[0].fourcc, b"XOBJ");
    assert_eq!(
        curve.unknown_chunks[0].raw_bytes,
        vec![0xDE, 0xAD, 0xBE, 0xEF, 0x42]
    );
}

#[test]
fn cdr_riff_v12_full_roundtrip() {
    let orig = generate_sample_document();
    let bytes = export_cdr_bytes(&orig, CdrWriteVersion::RiffV12).unwrap();
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"CDRC");

    let report = import_corel_bytes(&bytes).unwrap();
    assert_eq!(
        report.detected_format,
        CorelFormat::CdrRiff { version: 1200 }
    );
    assert!(
        report.warnings.is_empty(),
        "unexpected warnings: {:?}",
        report.warnings
    );
    assert_showcase_cdr_fidelity(&orig, &report.document);
}

#[test]
fn cdr_zip_x4_roundtrip_and_opc_layout() {
    let orig = generate_sample_document();
    let bytes = export_cdr_bytes(&orig, CdrWriteVersion::ZipX4).unwrap();

    // Verify expected X4–X5 ZIP entries exist
    let mut zip = ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().unwrap().into_owned())
        .collect();
    assert!(names.contains(&"mimetype".to_owned()));
    assert!(names.contains(&"content/riffData.cdr".to_owned()));
    assert!(names.contains(&"metadata/metadata.xml".to_owned()));
    assert!(names.contains(&"color/documentPalette.xml".to_owned()));
    assert!(names.contains(&"preview/thumbnail.bmp".to_owned()));

    let report = import_corel_bytes(&bytes).unwrap();
    assert_eq!(report.detected_format, CorelFormat::CdrZipX4);
    assert_eq!(report.document.metadata.author, "coree contributors");
    assert_eq!(report.document.metadata.subject, "Vector & Print Showcase");
    assert_eq!(
        report.document.metadata.keywords,
        vec!["vector", "arabic", "cmyk"]
    );
    assert_showcase_cdr_fidelity(&orig, &report.document);
}

#[test]
fn cdr_zip_x6_roundtrip_and_externalized_data_files() {
    let orig = generate_sample_document();
    let bytes = export_cdr_bytes(&orig, CdrWriteVersion::ZipX6).unwrap();

    // Verify authentic X6–2025 ZIP layout with externalized bitmap data/*.dat
    let mut zip = ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().unwrap().into_owned())
        .collect();
    assert!(names.contains(&"mimetype".to_owned()));
    assert!(names.contains(&"content/root.dat".to_owned()));
    assert!(names.contains(&"content/dataFileList.dat".to_owned()));
    assert!(names.contains(&"content/data/0001.dat".to_owned()));
    assert!(names.contains(&"styles/default.xml".to_owned()));
    assert!(names.contains(&"metadata/metadata.xml".to_owned()));
    assert!(names.contains(&"color/documentPalette.xml".to_owned()));
    assert!(names.contains(&"preview/thumbnail.bmp".to_owned()));

    let report = import_corel_bytes(&bytes).unwrap();
    assert_eq!(report.detected_format, CorelFormat::CdrZipX6);
    assert_showcase_cdr_fidelity(&orig, &report.document);
}

#[test]
fn cdt_template_riff_and_zip_roundtrip_plus_disk_io() {
    let orig = generate_sample_document();
    let tmp = TempDirGuard::new("disk_io");

    // CDT ZIP X4
    let cdt_x4_path = tmp.file("template_x4.cdt");
    export_cdt(&orig, &cdt_x4_path, CdrWriteVersion::ZipX4).unwrap();
    let rep_x4 = import_corel_file(&cdt_x4_path).unwrap();
    assert_eq!(rep_x4.detected_format, CorelFormat::CdtZip);
    assert_showcase_cdr_fidelity(&orig, &rep_x4.document);

    // CDT ZIP X6
    let cdt_x6_bytes = export_cdt_bytes(&orig, CdrWriteVersion::ZipX6).unwrap();
    let rep_x6 = import_cdt_bytes(&cdt_x6_bytes, &CodecLimits::default()).unwrap();
    assert_eq!(rep_x6.detected_format, CorelFormat::CdtZip);
    assert_showcase_cdr_fidelity(&orig, &rep_x6.document);

    // CDT RIFF v12
    let cdt_riff_bytes = export_cdt_bytes(&orig, CdrWriteVersion::RiffV12).unwrap();
    let rep_riff = import_corel_bytes(&cdt_riff_bytes).unwrap();
    assert_eq!(
        rep_riff.detected_format,
        CorelFormat::CdtRiff { version: 1200 }
    );
    assert_showcase_cdr_fidelity(&orig, &rep_riff.document);

    // Disk export_cdr + export_cmx + inspect_corel_file
    let cdr_path = tmp.file("design.cdr");
    export_cdr(&orig, &cdr_path, CdrWriteVersion::ZipX6).unwrap();
    let inspected = inspect_corel_file(&cdr_path).unwrap();
    assert_eq!(inspected.detected_format, CorelFormat::CdrZipX6);
    assert_eq!(inspected.page_count, 2);

    let cmx_path = tmp.file("exchange.cmx");
    export_cmx(&orig, &cmx_path).unwrap();
    let cmx_rep = import_corel_file(&cmx_path).unwrap();
    assert_eq!(cmx_rep.detected_format, CorelFormat::Cmx32);
    assert_eq!(cmx_rep.document.pages.len(), 2);
}

#[test]
fn cmx_16bit_and_32bit_roundtrip() {
    let orig = generate_sample_document();

    for (precision, expected_fmt, bytes) in [
        (
            CmxPrecision::Bit16,
            CorelFormat::Cmx16,
            sample_cmx_16bit_bytes(),
        ),
        (
            CmxPrecision::Bit32,
            CorelFormat::Cmx32,
            sample_cmx_32bit_bytes(),
        ),
    ] {
        let rep = import_corel_bytes(&bytes).unwrap();
        assert_eq!(rep.detected_format, expected_fmt, "precision {precision:?}");
        let doc = &rep.document;
        assert_eq!(doc.pages.len(), 2);
        assert_eq!(doc.pages[0].layers.len(), 2);
        assert!(doc.pages[0].layers[1].is_cut_layer);

        // Verify Hero Curve, Group, Arabic text, and DrawImage survived
        let curve = find_obj_by_name(doc, "Hero Curve");
        assert!(matches!(curve.payload, NodePayload::Path(_)));
        assert!(matches!(curve.fill, Fill::LinearGradient { .. }));
        assert_eq!(curve.outline.cap, LineCap::Round);
        assert_eq!(curve.outline.dash_pattern.len(), 2);

        let grp = find_obj_by_name(doc, "Logo Group");
        assert!(matches!(&grp.payload, NodePayload::Group(c) if c.len() == 2));

        let ar = find_obj_by_name(doc, "Arabic Headline");
        match &ar.payload {
            NodePayload::ArtisticText(t) => {
                assert_eq!(t.plain_text(), "مرحباً بكم في كوري");
                assert_eq!(t.direction, TextDirection::Rtl);
            }
            other => panic!("expected Arabic text in CMX, got {other:?}"),
        }

        let bmp_obj = find_obj_by_name(doc, "Embedded Swatch Bitmap");
        match &bmp_obj.payload {
            NodePayload::Raster(r) => {
                let asset = doc.assets.get(&r.asset_id).expect("CMX rimg asset");
                assert_eq!(asset.width, 4);
                assert_eq!(asset.height, 4);
                let orig_asset = orig.assets.values().next().unwrap();
                assert_eq!(asset.rgba_bytes, orig_asset.rgba_bytes);
            }
            other => panic!("expected Raster in CMX, got {other:?}"),
        }

        // Document-level unknown chunk passthrough in CMX
        assert_eq!(doc.unknown_doc_chunks.len(), 1);
        assert_eq!(&doc.unknown_doc_chunks[0].fourcc, b"XDOC");
    }

    // Re-export CMX directly via export_cmx_bytes
    let re = export_cmx_bytes(&orig, CmxPrecision::Bit16).unwrap();
    assert_eq!(&re[8..12], b"CMX1");
}

#[test]
fn cpt_multilayer_raster_and_alpha_mask_import() {
    let bytes = sample_cpt_multilayer_bytes();
    let rep = import_corel_bytes(&bytes).unwrap();
    assert_eq!(rep.detected_format, CorelFormat::Cpt { version: 9 });

    let doc = &rep.document;
    assert_eq!(doc.pages.len(), 1);
    assert_eq!(doc.pages[0].layers.len(), 3);
    assert_eq!(doc.assets.len(), 3);

    // Layer 1: Background RGB (Raw)
    let bg = find_obj_by_name(doc, "Background RGB");
    assert_eq!(bg.blend_mode, BlendMode::Normal);
    if let NodePayload::Raster(r) = &bg.payload {
        let asset = &doc.assets[&r.asset_id];
        assert_eq!((asset.width, asset.height), (8, 6));
        assert_eq!(&asset.rgba_bytes[0..4], &[0, 0, 220, 255]);
        assert_eq!(&asset.rgba_bytes[4..8], &[30, 0, 220, 255]);
    } else {
        panic!("expected Raster layer 1");
    }

    // Layer 2: Magenta CMYK Overlay (RLE strips + Zlib Alpha Mask = 128 + Multiply blend)
    let mag = find_obj_by_name(doc, "Magenta CMYK Overlay");
    assert_eq!(mag.blend_mode, BlendMode::Multiply);
    assert!((mag.opacity - (204.0 / 255.0)).abs() < 1e-4);
    assert!(mag.transform.m[4] > 0.0 && mag.transform.m[5] > 0.0); // offset_x=2, offset_y=1
    if let NodePayload::Raster(r) = &mag.payload {
        let asset = &doc.assets[&r.asset_id];
        assert_eq!(asset.color_mode, ColorMode::Cmyk);
        // Process Magenta CMYK(0,1,0,0) -> RGB(255, 0, 255), alpha = 128 from Zlib mask
        assert_eq!(&asset.rgba_bytes[0..4], &[255, 0, 255, 128]);
    } else {
        panic!("expected Raster layer 2");
    }

    // Layer 3: Grayscale Mask Layer (Deflate + RLE checker Alpha Mask 255/64 + Screen + locked)
    let gray = find_obj_by_name(doc, "Grayscale Mask Layer");
    assert_eq!(gray.blend_mode, BlendMode::Screen);
    assert!(gray.locked);
    if let NodePayload::Raster(r) = &gray.payload {
        let asset = &doc.assets[&r.asset_id];
        assert_eq!(&asset.rgba_bytes[0..4], &[200, 200, 200, 255]);
        assert_eq!(&asset.rgba_bytes[4..8], &[200, 200, 200, 64]);
    } else {
        panic!("expected Raster layer 3");
    }

    // Also verify RIFF `CPT ` and CPT7/CPT8 magic variants
    let riff_cpt = import_corel_bytes(&sample_cpt_riff_bytes()).unwrap();
    assert_eq!(riff_cpt.detected_format, CorelFormat::Cpt { version: 9 });
    let riff_obj = find_obj_by_name(&riff_cpt.document, "RIFF Raster");
    if let NodePayload::Raster(r) = &riff_obj.payload {
        let asset = &riff_cpt.document.assets[&r.asset_id];
        assert_eq!(&asset.rgba_bytes[0..4], &[100, 150, 200, 180]);
    } else {
        panic!("expected RIFF CPT raster");
    }

    let cpt7 = write_cpt_bytes(
        7,
        "Legacy CPT7",
        2,
        2,
        CptColorModel::Rgba32,
        150.0,
        &[CptLayerSpec {
            name: "L0".to_owned(),
            offset_x: 0,
            offset_y: 0,
            width: 2,
            height: 2,
            opacity: 255,
            blend_mode: BlendMode::Normal,
            visible: true,
            locked: false,
            color_model: CptColorModel::Rgba32,
            compression: CptCompression::Raw,
            raw_pixels: [10, 20, 30, 255].repeat(4),
            alpha_mask: None,
            rows_per_strip: 2,
        }],
    )
    .unwrap();
    let rep7 = import_corel_bytes(&cpt7).unwrap();
    assert_eq!(rep7.detected_format, CorelFormat::Cpt { version: 7 });
}

#[test]
fn cp1256_arabic_legacy_cdr_decoding_explicit_and_autodetected() {
    let bytes = sample_cp1256_arabic_cdr_bytes();
    let rep = import_corel_bytes(&bytes).unwrap();
    assert_eq!(rep.detected_format, CorelFormat::CdrRiff { version: 900 });

    let texts: Vec<(String, TextDirection)> = rep
        .document
        .objects
        .values()
        .filter_map(|o| match &o.payload {
            NodePayload::ArtisticText(t) => Some((t.plain_text(), t.effective_direction())),
            _ => None,
        })
        .collect();

    assert_eq!(texts.len(), 2);
    assert!(
        texts
            .iter()
            .any(|(s, d)| s == "تصميم عربي كلاسيكي" && *d == TextDirection::Rtl),
        "explicit CP1256 Arabic text missing: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|(s, d)| s == "مرحبا بالعالم العربي" && *d == TextDirection::Rtl),
        "auto-detected CP1256 Arabic text missing: {texts:?}"
    );
}

#[test]
fn unknown_chunk_passthrough_byte_for_byte_across_all_cdr_generations() {
    let orig_bytes = sample_unknown_chunk_cdr_bytes();
    let expected_doc = vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD];
    let expected_obj = vec![0xFE, 0xDC, 0xBA, 0x98, 0x76];

    // Step 1: Import original RIFF
    let step1 = import_corel_bytes(&orig_bytes).unwrap().document;
    assert_eq!(step1.unknown_doc_chunks.len(), 1);
    assert_eq!(&step1.unknown_doc_chunks[0].fourcc, b"XDOC");
    assert_eq!(step1.unknown_doc_chunks[0].raw_bytes, expected_doc);
    let obj1 = find_obj_by_name(&step1, "Box With Custom Chunk");
    assert_eq!(obj1.unknown_chunks.len(), 1);
    assert_eq!(&obj1.unknown_chunks[0].fourcc, b"XOBJ");
    assert_eq!(obj1.unknown_chunks[0].raw_bytes, expected_obj);

    // Step 2: Export -> RiffV12 -> re-import
    let riff_re = export_cdr_bytes(&step1, CdrWriteVersion::RiffV12).unwrap();
    let step2 = import_corel_bytes(&riff_re).unwrap().document;
    assert_eq!(step2.unknown_doc_chunks[0].raw_bytes, expected_doc);
    assert_eq!(
        find_obj_by_name(&step2, "Box With Custom Chunk").unknown_chunks[0].raw_bytes,
        expected_obj
    );

    // Step 3: Export -> ZipX4 -> re-import
    let x4_re = export_cdr_bytes(&step2, CdrWriteVersion::ZipX4).unwrap();
    let step3 = import_corel_bytes(&x4_re).unwrap().document;
    assert_eq!(step3.unknown_doc_chunks[0].raw_bytes, expected_doc);
    assert_eq!(
        find_obj_by_name(&step3, "Box With Custom Chunk").unknown_chunks[0].raw_bytes,
        expected_obj
    );

    // Step 4: Export -> ZipX6 -> re-import
    let x6_re = export_cdr_bytes(&step3, CdrWriteVersion::ZipX6).unwrap();
    let step4 = import_corel_bytes(&x6_re).unwrap().document;
    assert_eq!(step4.unknown_doc_chunks[0].raw_bytes, expected_doc);
    assert_eq!(
        find_obj_by_name(&step4, "Box With Custom Chunk").unknown_chunks[0].raw_bytes,
        expected_obj
    );
}

#[test]
fn powerclip_all_parametric_shapes_and_advanced_fills_roundtrip() {
    let mut doc = Document::new_a4("Advanced Features");
    let c1 = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Clipped Child",
                NodePayload::Shape(ParametricShape::Spiral {
                    revolutions: 4.0,
                    radius: 20.0,
                    logarithmic: true,
                    expansion: 2.5,
                }),
            )
            .with_fill(Fill::TwoColorPattern {
                fg: Color::CYAN,
                bg: Color::WHITE,
                pattern_name: "Hatch45".to_owned(),
                tile_w: 5.0,
                tile_h: 5.0,
            }),
            None,
        )
        .unwrap();
    let container = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Clip Frame",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    0.0, 0.0, 60.0, 60.0,
                ))),
            ),
            None,
        )
        .unwrap();
    doc.create_powerclip(container, &[c1]).unwrap();

    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Mesh Grid",
            NodePayload::Shape(ParametricShape::GraphPaper {
                cols: 4,
                rows: 3,
                width: 40.0,
                height: 30.0,
            }),
        )
        .with_fill(Fill::MeshFill {
            rows: 2,
            cols: 2,
            nodes: vec![
                MeshNode {
                    row: 0,
                    col: 0,
                    pos: Point2D::new(0.0, 0.0),
                    color: Color::CYAN,
                },
                MeshNode {
                    row: 1,
                    col: 1,
                    pos: Point2D::new(40.0, 30.0),
                    color: Color::YELLOW,
                },
            ],
        }),
        None,
    );

    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Impact Burst",
            NodePayload::Shape(ParametricShape::Impact {
                rays: 16,
                inner_radius: 5.0,
                outer_radius: 25.0,
                style: ImpactStyle::Radial,
            }),
        )
        .with_fill(Fill::TextureFill {
            preset: "Marble".to_owned(),
            seed: 0x1234_5678_9ABC_DEF0,
            fg: Color::BLACK,
            bg: Color::WHITE,
        }),
        None,
    );

    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Heart Icon",
            NodePayload::Shape(ParametricShape::BasicShape {
                kind: BasicShapeKind::Heart,
                width: 20.0,
                height: 18.0,
            }),
        ),
        None,
    );

    let bytes = export_cdr_bytes(&doc, CdrWriteVersion::ZipX6).unwrap();
    let restored = import_corel_bytes(&bytes).unwrap().document;

    let pc = find_obj_by_name(&restored, "Clip Frame");
    match &pc.payload {
        NodePayload::PowerClip { contents, .. } => assert_eq!(contents.len(), 1),
        other => panic!("expected PowerClip, got {other:?}"),
    }
    let mesh = find_obj_by_name(&restored, "Mesh Grid");
    assert!(matches!(
        &mesh.fill,
        Fill::MeshFill { rows: 2, cols: 2, nodes } if nodes.len() == 2
    ));
    let imp = find_obj_by_name(&restored, "Impact Burst");
    assert!(matches!(
        &imp.fill,
        Fill::TextureFill {
            seed: 0x1234_5678_9ABC_DEF0,
            ..
        }
    ));
    let heart = find_obj_by_name(&restored, "Heart Icon");
    assert!(matches!(
        &heart.payload,
        NodePayload::Shape(ParametricShape::BasicShape {
            kind: BasicShapeKind::Heart,
            ..
        })
    ));
}

#[test]
fn cdr_inspector_accuracy_across_all_formats() {
    // 1. RIFF v12
    let riff_rep = inspect_corel_bytes(&sample_cdr_riff_v12_bytes()).unwrap();
    assert_eq!(
        riff_rep.detected_format,
        CorelFormat::CdrRiff { version: 1200 }
    );
    assert_eq!(riff_rep.page_count, 2);
    assert_eq!(riff_rep.layer_count, 3);
    assert!(riff_rep.object_count >= 18);
    assert_eq!(riff_rep.bitmap_count, 1);
    assert!(riff_rep.fonts.contains(&"Noto Sans".to_owned()));
    assert!(riff_rep.fonts.contains(&"Noto Naskh Arabic".to_owned()));
    assert!(riff_rep.color_models.contains(&"CMYK".to_owned()));
    assert!(riff_rep.color_models.contains(&"RGB".to_owned()));
    assert!(riff_rep.color_models.contains(&"Spot".to_owned()));
    assert!(riff_rep.chunks.iter().any(|c| c.compressed));
    assert_eq!(riff_rep.unknown_chunks.len(), 2); // XDOC + XOBJ
    let formatted = format_report(&riff_rep);
    assert!(formatted.contains("coree CDR Structural Inspector"));
    assert!(formatted.contains("[cmpr]"));
    assert!(formatted.contains("XDOC"));
    assert!(formatted.contains("XOBJ"));

    // 2. X4 & X6 ZIP
    let x4_rep = inspect_corel_bytes(&sample_cdr_zip_x4_bytes()).unwrap();
    assert_eq!(x4_rep.detected_format, CorelFormat::CdrZipX4);
    assert!(x4_rep
        .zip_entries
        .iter()
        .any(|e| e.name == "content/riffData.cdr"));

    let x6_rep = inspect_corel_bytes(&sample_cdr_zip_x6_bytes()).unwrap();
    assert_eq!(x6_rep.detected_format, CorelFormat::CdrZipX6);
    assert!(x6_rep
        .zip_entries
        .iter()
        .any(|e| e.name == "content/root.dat"));
    assert!(x6_rep
        .zip_entries
        .iter()
        .any(|e| e.name == "content/dataFileList.dat"));
    assert!(x6_rep.format_report().contains("ZIP Entries"));

    // 3. CDT, CMX, CPT
    let cdt_rep = inspect_corel_bytes(&sample_cdt_bytes()).unwrap();
    assert_eq!(cdt_rep.detected_format, CorelFormat::CdtZip);

    let cmx_rep = inspect_corel_bytes(&sample_cmx_32bit_bytes()).unwrap();
    assert_eq!(cmx_rep.detected_format, CorelFormat::Cmx32);
    assert_eq!(cmx_rep.page_count, 2);

    let cpt_rep = inspect_corel_bytes(&sample_cpt_multilayer_bytes()).unwrap();
    assert_eq!(cpt_rep.detected_format, CorelFormat::Cpt { version: 9 });
    assert_eq!(cpt_rep.layer_count, 3);
    assert_eq!(cpt_rep.bitmap_count, 3);
}

#[test]
fn dib_and_bmp_24bit_8bit_1bit_and_topdown_decoding() {
    let rgba = vec![
        255, 0, 0, 255, 0, 255, 0, 128, // row 0
        0, 0, 255, 64, 255, 255, 0, 255, // row 1
    ];
    let dib = encode_dib_rgba(2, 2, &rgba).unwrap();
    let (w, h, back) = decode_dib_to_rgba(&dib, &CodecLimits::default()).unwrap();
    assert_eq!((w, h), (2, 2));
    assert_eq!(back, rgba);

    let bmp = encode_bmp_file_rgba(2, 2, &rgba).unwrap();
    assert_eq!(&bmp[0..2], b"BM");
    let (w2, h2, back2) = decode_dib_to_rgba(&bmp, &CodecLimits::default()).unwrap();
    assert_eq!((w2, h2), (2, 2));
    assert_eq!(back2, rgba);
}

#[test]
fn adversarial_security_suite_rejects_all_attacks_with_zero_panics() {
    // 1. Empty, truncated header, garbage bytes, truncated chunk
    assert!(import_corel_bytes(&[]).is_err());
    assert!(import_corel_bytes(&malformed_truncated_header()).is_err());
    assert!(import_corel_bytes(&malformed_garbage_bytes()).is_err());
    assert!(import_corel_bytes(&malformed_truncated_chunk()).is_err());
    assert!(inspect_corel_bytes(&malformed_truncated_header()).is_err());
    assert!(inspect_corel_bytes(&malformed_garbage_bytes()).is_err());

    // 2. Zip-Slip (`../../etc/passwd`)
    let slip_err = import_corel_bytes(&malformed_zip_slip_bytes()).unwrap_err();
    assert!(
        matches!(slip_err, CdrError::ZipSlipDetected { .. }),
        "expected ZipSlipDetected, got {slip_err:?}"
    );
    assert!(matches!(
        inspect_corel_bytes(&malformed_zip_slip_bytes()).unwrap_err(),
        CdrError::ZipSlipDetected { .. }
    ));

    // Additional Zip-Slip variants: absolute path, backslash, Windows drive prefix
    for evil_name in ["/etc/passwd", "content\\evil.cdr", "C:/Windows/win.ini"] {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zip.start_file(evil_name, opts).unwrap();
            zip.write_all(b"evil").unwrap();
            zip.finish().unwrap();
        }
        let err = import_corel_bytes(&cursor.into_inner()).unwrap_err();
        assert!(
            matches!(err, CdrError::ZipSlipDetected { .. }),
            "path '{evil_name}' should fail with ZipSlipDetected, got {err:?}"
        );
    }

    // 3. Zip-Bomb (high compression ratio & bounded streaming size cap)
    let bomb_err = import_corel_bytes(&malformed_zip_bomb_bytes()).unwrap_err();
    assert!(
        matches!(
            bomb_err,
            CdrError::CompressionRatioExceeded { .. }
                | CdrError::UncompressedSizeLimitExceeded { .. }
        ),
        "expected Zip-Bomb rejection, got {bomb_err:?}"
    );
    assert!(inspect_corel_bytes(&malformed_zip_bomb_bytes()).is_err());

    // 4. Zlib Bomb hidden inside RIFF `LIST cmpr`
    let cmpr_bomb_err = import_corel_bytes(&malformed_cmpr_zlib_bomb_bytes()).unwrap_err();
    assert!(
        matches!(
            cmpr_bomb_err,
            CdrError::CompressionRatioExceeded { .. }
                | CdrError::UncompressedSizeLimitExceeded { .. }
        ),
        "expected LIST cmpr bomb rejection, got {cmpr_bomb_err:?}"
    );

    // 5. 200-deep nested RIFF `LIST`
    let deep_err = import_corel_bytes(&malformed_deep_nested_riff_bytes(200)).unwrap_err();
    assert!(
        matches!(deep_err, CdrError::DepthLimitExceeded { .. }),
        "expected DepthLimitExceeded, got {deep_err:?}"
    );

    // 6. Huge bitmap dimensions in CDR (`65536x65536`) and CPT (`100000x100000`)
    let bmp_err = import_corel_bytes(&malformed_huge_bitmap_cdr_bytes()).unwrap_err();
    assert!(
        matches!(bmp_err, CdrError::InvalidBitmapDimensions { .. }),
        "expected InvalidBitmapDimensions for CDR, got {bmp_err:?}"
    );
    let cpt_err = import_corel_bytes(&malformed_huge_bitmap_cpt_bytes()).unwrap_err();
    assert!(
        matches!(cpt_err, CdrError::InvalidBitmapDimensions { .. }),
        "expected InvalidBitmapDimensions for CPT, got {cpt_err:?}"
    );

    // 7. Malicious `loda` declaring `node_count = 0xFFFF_FFFF` in a tiny slice (must not OOM!)
    let mut evil_loda_curve = Vec::new();
    evil_loda_curve.extend_from_slice(&1u32.to_le_bytes()); // num_args = 1
    evil_loda_curve.extend_from_slice(&16u32.to_le_bytes()); // args_offset = 16
    evil_loda_curve.extend_from_slice(&20u32.to_le_bytes()); // types_offset = 20
    evil_loda_curve.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // huge point_count
    evil_loda_curve.extend_from_slice(&12u32.to_le_bytes()); // offset[0] = 12
    evil_loda_curve.extend_from_slice(&0x001Eu32.to_le_bytes()); // type[0] = CURVE
    let evil_riff = write_riff(&RiffDocument::new(
        *b"CDRC",
        vec![RiffNode::list(
            *b"doc ",
            vec![RiffNode::list(
                *b"page",
                vec![RiffNode::list(
                    *b"layr",
                    vec![RiffNode::list(
                        *b"obj ",
                        vec![RiffNode::chunk(*b"loda", evil_loda_curve)],
                    )],
                )],
            )],
        )],
    ))
    .unwrap();
    // Should never panic or OOM (records non-fatal warning on malformed loda)
    let parsed_evil = import_corel_bytes(&evil_riff).unwrap();
    assert!(!parsed_evil.warnings.is_empty());

    // 8. Tight custom CodecLimits byte cap rejects normal X4 ZIP cleanly
    let tiny_limits = CodecLimits {
        max_uncompressed_bytes: 64,
        ..CodecLimits::default()
    };
    let cap_err =
        import_corel_bytes_with_limits(&sample_cdr_zip_x4_bytes(), &tiny_limits).unwrap_err();
    assert!(matches!(
        cap_err,
        CdrError::UncompressedSizeLimitExceeded { .. }
    ));

    // 9. Exhaustive prefix-truncation & bit-flip fuzz sweep (ZERO panics!)
    let valid_corpus = [
        sample_cdr_riff_v12_bytes(),
        sample_cdr_zip_x4_bytes(),
        sample_cdr_zip_x6_bytes(),
        sample_cmx_16bit_bytes(),
        sample_cmx_32bit_bytes(),
        sample_cpt_multilayer_bytes(),
        sample_cpt_riff_bytes(),
        sample_cp1256_arabic_cdr_bytes(),
        write_cdr_riff_with_options(
            &generate_sample_document(),
            RiffWriteOptions {
                compress_doc: false,
                ..RiffWriteOptions::default()
            },
        )
        .unwrap()
        .riff_bytes,
    ];
    let _ = CdrZipEra::X4;

    for sample in &valid_corpus {
        // Every prefix length up to 128 + strided prefixes across full length
        for len in 0..sample.len().min(128) {
            let _ = import_corel_bytes(&sample[..len]);
            let _ = inspect_corel_bytes(&sample[..len]);
        }
        let step = (sample.len() / 32).max(1);
        for len in (0..sample.len()).step_by(step) {
            let _ = import_corel_bytes(&sample[..len]);
        }
        // Deterministic single-byte mutations across key offsets
        for offset in (0..sample.len().min(256)).step_by(7) {
            let mut mutated = sample.clone();
            mutated[offset] ^= 0xFF;
            let _ = import_corel_bytes(&mutated);
            let _ = inspect_corel_bytes(&mutated);
        }
    }
}

#[test]
fn dib_24bit_8bit_1bit_topdown_and_i32_min_height_safety() {
    let limits = CodecLimits::default();

    // 1. 24-bit BGR odd width (3x2 = 9 bytes/row -> padded to 12 bytes/row), top-down (height = -2)
    let mut dib24 = Vec::new();
    dib24.extend_from_slice(&40u32.to_le_bytes());
    dib24.extend_from_slice(&3i32.to_le_bytes());
    dib24.extend_from_slice(&(-2i32).to_le_bytes()); // top-down
    dib24.extend_from_slice(&1u16.to_le_bytes());
    dib24.extend_from_slice(&24u16.to_le_bytes());
    dib24.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    dib24.extend_from_slice(&24u32.to_le_bytes());
    dib24.extend_from_slice(&0i32.to_le_bytes());
    dib24.extend_from_slice(&0i32.to_le_bytes());
    dib24.extend_from_slice(&0u32.to_le_bytes());
    dib24.extend_from_slice(&0u32.to_le_bytes());
    // Row 0: (R=255,G=0,B=0), (0,255,0), (0,0,255) + 3 pad bytes
    dib24.extend_from_slice(&[0, 0, 255, 0, 255, 0, 255, 0, 0, 0, 0, 0]);
    // Row 1: (10,20,30), (40,50,60), (70,80,90) + 3 pad bytes
    dib24.extend_from_slice(&[30, 20, 10, 60, 50, 40, 90, 80, 70, 0, 0, 0]);
    let (w, h, rgba) = decode_dib_to_rgba(&dib24, &limits).unwrap();
    assert_eq!((w, h), (3, 2));
    assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
    assert_eq!(&rgba[4..8], &[0, 255, 0, 255]);
    assert_eq!(&rgba[8..12], &[0, 0, 255, 255]);
    assert_eq!(&rgba[12..16], &[10, 20, 30, 255]);

    // 2. 8-bit indexed palette (2x1)
    let mut dib8 = Vec::new();
    dib8.extend_from_slice(&40u32.to_le_bytes());
    dib8.extend_from_slice(&2i32.to_le_bytes());
    dib8.extend_from_slice(&1i32.to_le_bytes());
    dib8.extend_from_slice(&1u16.to_le_bytes());
    dib8.extend_from_slice(&8u16.to_le_bytes());
    dib8.extend_from_slice(&0u32.to_le_bytes());
    dib8.extend_from_slice(&4u32.to_le_bytes());
    dib8.extend_from_slice(&0i32.to_le_bytes());
    dib8.extend_from_slice(&0i32.to_le_bytes());
    dib8.extend_from_slice(&2u32.to_le_bytes()); // biClrUsed = 2
    dib8.extend_from_slice(&0u32.to_le_bytes());
    dib8.extend_from_slice(&[0, 0, 255, 0]); // idx 0 = Red (BGR0)
    dib8.extend_from_slice(&[255, 0, 0, 0]); // idx 1 = Blue (BGR0)
    dib8.extend_from_slice(&[0, 1, 0, 0]); // 2 pixels + 2 pad bytes
    let (w8, h8, rgba8) = decode_dib_to_rgba(&dib8, &limits).unwrap();
    assert_eq!((w8, h8), (2, 1));
    assert_eq!(&rgba8[0..8], &[255, 0, 0, 255, 0, 0, 255, 255]);

    // 3. 1-bit monochrome (2x1)
    let mut dib1 = Vec::new();
    dib1.extend_from_slice(&40u32.to_le_bytes());
    dib1.extend_from_slice(&2i32.to_le_bytes());
    dib1.extend_from_slice(&1i32.to_le_bytes());
    dib1.extend_from_slice(&1u16.to_le_bytes());
    dib1.extend_from_slice(&1u16.to_le_bytes());
    dib1.extend_from_slice(&0u32.to_le_bytes());
    dib1.extend_from_slice(&4u32.to_le_bytes());
    dib1.extend_from_slice(&0i32.to_le_bytes());
    dib1.extend_from_slice(&0i32.to_le_bytes());
    dib1.extend_from_slice(&2u32.to_le_bytes());
    dib1.extend_from_slice(&0u32.to_le_bytes());
    dib1.extend_from_slice(&[0, 0, 0, 0]); // idx 0 = Black
    dib1.extend_from_slice(&[255, 255, 255, 0]); // idx 1 = White
    dib1.extend_from_slice(&[0b1000_0000, 0, 0, 0]); // px0=1 (white), px1=0 (black)
    let (w1, h1, rgba1) = decode_dib_to_rgba(&dib1, &limits).unwrap();
    assert_eq!((w1, h1), (2, 1));
    assert_eq!(&rgba1[0..8], &[255, 255, 255, 255, 0, 0, 0, 255]);

    // 4. biHeight = i32::MIN must NOT panic on abs() and must return InvalidBitmapDimensions
    let mut dib_min = dib24.clone();
    dib_min[8..12].copy_from_slice(&i32::MIN.to_le_bytes());
    let err = decode_dib_to_rgba(&dib_min, &limits).unwrap_err();
    assert!(matches!(err, CdrError::InvalidBitmapDimensions { .. }));
}

#[test]
fn cpt_rle_bomb_rejected_before_allocating() {
    // 256 x 256 Gray8 = 65,536 bytes of zeros compresses via PackBits RLE to ~1,024 bytes (~64x),
    // so craft a tiny 10-byte RLE stream claiming a 200x200x3 (120,000 byte) layer (> 100x ratio!)
    let mut cpt = Vec::new();
    cpt.extend_from_slice(b"CPT9FILE");
    cpt.extend_from_slice(&28u32.to_le_bytes());
    cpt.extend_from_slice(&200u32.to_le_bytes());
    cpt.extend_from_slice(&200u32.to_le_bytes());
    cpt.extend_from_slice(&(CptColorModel::Rgb24 as u16).to_le_bytes());
    cpt.extend_from_slice(&24u16.to_le_bytes());
    cpt.extend_from_slice(&300.0f32.to_le_bytes());
    cpt.extend_from_slice(&300.0f32.to_le_bytes());
    cpt.extend_from_slice(&1u32.to_le_bytes()); // 1 layer
    cpt.extend_from_slice(&0u16.to_le_bytes()); // empty title
                                                // Layer block
    cpt.extend_from_slice(b"LAYR");
    cpt.extend_from_slice(&0u16.to_le_bytes()); // name len
    cpt.extend_from_slice(&0i32.to_le_bytes()); // ox
    cpt.extend_from_slice(&0i32.to_le_bytes()); // oy
    cpt.extend_from_slice(&200u32.to_le_bytes()); // lw
    cpt.extend_from_slice(&200u32.to_le_bytes()); // lh
    cpt.push(255); // opacity
    cpt.push(0); // blend
    cpt.push(1); // visible
    cpt.extend_from_slice(&(CptColorModel::Rgb24 as u16).to_le_bytes());
    cpt.push(CptCompression::Rle as u8);
    cpt.extend_from_slice(&1u32.to_le_bytes()); // 1 strip
    cpt.extend_from_slice(&200u32.to_le_bytes()); // 200 rows -> expected 120,000 bytes
    cpt.extend_from_slice(&10u32.to_le_bytes()); // only 10 encoded bytes -> 12,000:1 ratio!
    cpt.extend_from_slice(&[0x81, 0x00, 0x81, 0x00, 0x81, 0x00, 0x81, 0x00, 0x81, 0x00]);
    cpt.push(0); // no mask

    let err = import_corel_bytes(&cpt).unwrap_err();
    assert!(
        matches!(err, CdrError::CompressionRatioExceeded { .. }),
        "expected CompressionRatioExceeded for RLE bomb, got {err:?}"
    );
}

#[test]
fn cdr_zip_x6_multiple_external_bitmaps_roundtrip() {
    let mut doc = Document::new_a4("Multi-Asset X6");
    let a1 = coree_core::RasterAsset::new(2, 2, [255, 0, 0, 255].repeat(4), 300.0, ColorMode::Rgb)
        .unwrap();
    let a2 =
        coree_core::RasterAsset::new(3, 2, [0, 255, 128, 200].repeat(6), 150.0, ColorMode::Cmyk)
            .unwrap();
    let id1 = doc.register_asset(a1.clone());
    let id2 = doc.register_asset(a2.clone());
    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Img One",
            NodePayload::Raster(coree_core::RasterLayerData {
                asset_id: id1,
                width_mm: 10.0,
                height_mm: 10.0,
                filters: Vec::new(),
                crop_rect: None,
            }),
        ),
        None,
    );
    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Img Two",
            NodePayload::Raster(coree_core::RasterLayerData {
                asset_id: id2,
                width_mm: 15.0,
                height_mm: 10.0,
                filters: Vec::new(),
                crop_rect: None,
            }),
        ),
        None,
    );

    let bytes = export_cdr_bytes(&doc, CdrWriteVersion::ZipX6).unwrap();
    let mut zip = ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().unwrap().into_owned())
        .collect();
    assert!(names.contains(&"content/data/0001.dat".to_owned()));
    assert!(names.contains(&"content/data/0002.dat".to_owned()));

    let restored = import_corel_bytes(&bytes).unwrap().document;
    assert_eq!(restored.assets.len(), 2);
    let o1 = find_obj_by_name(&restored, "Img One");
    let o2 = find_obj_by_name(&restored, "Img Two");
    if let (NodePayload::Raster(r1), NodePayload::Raster(r2)) = (&o1.payload, &o2.payload) {
        assert_eq!(restored.assets[&r1.asset_id].rgba_bytes, a1.rgba_bytes);
        assert_eq!(restored.assets[&r2.asset_id].rgba_bytes, a2.rgba_bytes);
    } else {
        panic!("expected both raster objects");
    }
}

#[test]
fn detect_format_matches_all_eight_corel_formats() {
    use coree_cdr::detect_format;

    assert_eq!(
        detect_format(&sample_cdr_riff_v12_bytes()).unwrap(),
        CorelFormat::CdrRiff { version: 1200 }
    );
    assert_eq!(
        detect_format(&sample_cdr_zip_x4_bytes()).unwrap(),
        CorelFormat::CdrZipX4
    );
    assert_eq!(
        detect_format(&sample_cdr_zip_x6_bytes()).unwrap(),
        CorelFormat::CdrZipX6
    );
    assert_eq!(
        detect_format(
            &export_cdt_bytes(&generate_sample_document(), CdrWriteVersion::RiffV12).unwrap()
        )
        .unwrap(),
        CorelFormat::CdtRiff { version: 1200 }
    );
    assert_eq!(
        detect_format(&sample_cdt_bytes()).unwrap(),
        CorelFormat::CdtZip
    );
    assert_eq!(
        detect_format(&sample_cmx_16bit_bytes()).unwrap(),
        CorelFormat::Cmx16
    );
    assert_eq!(
        detect_format(&sample_cmx_32bit_bytes()).unwrap(),
        CorelFormat::Cmx32
    );
    assert_eq!(
        detect_format(&sample_cpt_multilayer_bytes()).unwrap(),
        CorelFormat::Cpt { version: 9 }
    );
}

// =============================================================================
// Regression Tests for Verifier Audit Findings (#23)
// =============================================================================

#[test]
fn regression_1_bounded_disk_read_and_cumulative_zip_cmpr_multidib_budget() {
    // 1a. Bounded disk reads reject oversized files on disk before parsing
    let tmp = TempDirGuard::new("bounded_disk");
    let cdr_path = tmp.file("oversized.cdr");
    let valid_bytes = sample_cdr_riff_v12_bytes();
    assert!(valid_bytes.len() > 256);
    std::fs::write(&cdr_path, &valid_bytes).unwrap();
    let tiny = CodecLimits {
        max_uncompressed_bytes: 256,
        ..CodecLimits::default()
    };
    assert!(matches!(
        import_corel_file_with_limits(&cdr_path, &tiny).unwrap_err(),
        CdrError::UncompressedSizeLimitExceeded { limit: 256 }
    ));
    assert!(matches!(
        inspect_corel_file_with_limits(&cdr_path, &tiny).unwrap_err(),
        CdrError::UncompressedSizeLimitExceeded { limit: 256 }
    ));

    // 1b. Cumulative ZIP + `LIST cmpr` budget:
    // Build a RIFF with a ~55 KiB uncompressed padding chunk + a `LIST cmpr` expanding to ~70 KiB
    // of pseudo-random/varied bytes (low ratio so ratio check doesn't trigger).
    // Individually each is < 100_000 bytes, but cumulatively ZIP(riffData) + cmpr > 100_000!
    let mut varied = Vec::with_capacity(70_000);
    let mut state: u32 = 0x1234_5678;
    for _ in 0..70_000 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        varied.push((state >> 24) as u8);
    }
    let riff = write_riff(&RiffDocument::new(
        *b"CDRC",
        vec![
            RiffNode::chunk(*b"vrsn", 1200u16.to_le_bytes().to_vec()),
            RiffNode::cmpr_list(vec![RiffNode::list(
                *b"doc ",
                vec![RiffNode::chunk(*b"XBIG", varied)],
            )]),
        ],
    ))
    .unwrap();
    // Wrap riffData.cdr (which is ~70 KiB) inside ZIP so ZIP entry (~70 KiB) + decompressed cmpr (~70 KiB) = ~140 KiB > 100_000
    assert!(riff.len() > 50_000 && riff.len() < 90_000);
    let mut zcur = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut zcur);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("content/riffData.cdr", opts).unwrap();
        zip.write_all(&riff).unwrap();
        zip.finish().unwrap();
    }
    let zip_bytes = zcur.into_inner();
    let cap_100k = CodecLimits {
        max_uncompressed_bytes: 100_000,
        ..CodecLimits::default()
    };
    assert!(
        matches!(
            import_corel_bytes_with_limits(&zip_bytes, &cap_100k).unwrap_err(),
            CdrError::UncompressedSizeLimitExceeded { limit: 100_000 }
        ),
        "cumulative ZIP + LIST cmpr must exceed 100_000 budget"
    );

    // 1c. Multiple 1-bit DIBs (32x RGBA expansion each) must deduct from cumulative budget
    let make_1bit_dib = |w: u32, h: u32| -> Vec<u8> {
        let row_stride = w.div_ceil(32) * 4;
        let pixel_bytes = (row_stride * h) as usize;
        let mut d = Vec::with_capacity(40 + 8 + pixel_bytes);
        d.extend_from_slice(&40u32.to_le_bytes());
        d.extend_from_slice(&(w as i32).to_le_bytes());
        d.extend_from_slice(&(h as i32).to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes()); // 1-bpp
        d.extend_from_slice(&0u32.to_le_bytes());
        d.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
        d.extend_from_slice(&0i32.to_le_bytes());
        d.extend_from_slice(&0i32.to_le_bytes());
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes());
        d.extend_from_slice(&[0, 0, 0, 0, 255, 255, 255, 0]);
        d.resize(40 + 8 + pixel_bytes, 0xAA);
        d
    };
    // Each 256x64 1-bit DIB is only ~2 KiB on disk, but expands to 65_536 RGBA bytes!
    // 6 of them = 393_216 RGBA bytes > 200_000 budget!
    let dib_node = make_1bit_dib(256, 64);
    let mut bmp_chunks = Vec::new();
    for _ in 0..6 {
        bmp_chunks.push(RiffNode::chunk(*b"bmp ", dib_node.clone()));
    }
    let multi_dib_cdr = write_riff(&RiffDocument::new(
        *b"CDRC",
        vec![RiffNode::list(
            *b"doc ",
            vec![RiffNode::list(*b"bmps", bmp_chunks)],
        )],
    ))
    .unwrap();
    let cap_200k = CodecLimits {
        max_uncompressed_bytes: 200_000,
        ..CodecLimits::default()
    };
    assert!(
        matches!(
            import_corel_bytes_with_limits(&multi_dib_cdr, &cap_200k).unwrap_err(),
            CdrError::UncompressedSizeLimitExceeded { limit: 200_000 }
        ),
        "cumulative multi-DIB RGBA allocations must be bounded by max_uncompressed_bytes"
    );

    // 1d. CPT multi-layer cumulative RGBA budget (Gray8 expands 4x to RGBA)
    let cpt_layers: Vec<CptLayerSpec> = (0..4)
        .map(|i| CptLayerSpec {
            name: format!("G{i}"),
            offset_x: 0,
            offset_y: 0,
            width: 128,
            height: 64, // 8 KiB raw -> 32 KiB RGBA per layer; 4 layers = 160 KiB > 100 KiB
            opacity: 255,
            blend_mode: BlendMode::Normal,
            visible: true,
            locked: false,
            color_model: CptColorModel::Gray8,
            compression: CptCompression::Raw,
            raw_pixels: vec![128u8; 128 * 64],
            alpha_mask: None,
            rows_per_strip: 64,
        })
        .collect();
    let multi_cpt = write_cpt_bytes(
        9,
        "CPT Budget",
        128,
        64,
        CptColorModel::Gray8,
        300.0,
        &cpt_layers,
    )
    .unwrap();
    assert!(matches!(
        import_corel_bytes_with_limits(&multi_cpt, &cap_100k).unwrap_err(),
        CdrError::UncompressedSizeLimitExceeded { limit: 100_000 }
    ));
}

#[test]
fn regression_2_zip_duplicates_datafilelist_amplification_and_forged_compressed_size() {
    let valid_riff = sample_cdr_riff_v12_bytes();

    // 2a. Duplicate case-insensitive ZIP entries must be rejected
    let mut dup_cur = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut dup_cur);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("content/riffData.cdr", opts).unwrap();
        zip.write_all(&valid_riff).unwrap();
        zip.start_file("content/RIFFDATA.CDR", opts).unwrap();
        zip.write_all(&valid_riff).unwrap();
        zip.finish().unwrap();
    }
    let dup_err = import_corel_bytes(&dup_cur.into_inner()).unwrap_err();
    assert!(
        matches!(dup_err, CdrError::ZipError(ref msg) if msg.contains("duplicate")),
        "expected duplicate ZIP entry rejection, got {dup_err:?}"
    );

    // 2b. `dataFileList.dat` repeating `data/0001.dat` 2000x + listing `content/root.dat`
    // must deduplicate and parse cleanly within a tight budget (no CPU/budget amplification!)
    let base_x6 = sample_cdr_zip_x6_bytes();
    let mut src_zip = ZipArchive::new(Cursor::new(&base_x6)).unwrap();
    let mut amp_cur = Cursor::new(Vec::new());
    {
        let mut dst = ZipWriter::new(&mut amp_cur);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for i in 0..src_zip.len() {
            let mut f = src_zip.by_index(i).unwrap();
            let name = f.name().unwrap().into_owned();
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut buf).unwrap();
            if name == "content/dataFileList.dat" {
                // Store uncompressed (1:1 ratio) so ratio guard passes and deduplication is exercised directly!
                dst.start_file(&name, stored).unwrap();
                let mut repeated = String::from("content/root.dat\n");
                for _ in 0..2000 {
                    repeated.push_str("data/0001.dat\n");
                }
                dst.write_all(repeated.as_bytes()).unwrap();
            } else {
                dst.start_file(&name, deflated).unwrap();
                dst.write_all(&buf).unwrap();
            }
        }
        dst.finish().unwrap();
    }
    // Tight budget (64 KiB): without deduplication, 2000x `data/0001.dat` (~240 KiB) blows past 64 KiB!
    let tight = CodecLimits {
        max_uncompressed_bytes: 64 * 1024,
        ..CodecLimits::default()
    };
    let amp_rep = import_corel_bytes_with_limits(&amp_cur.into_inner(), &tight).unwrap();
    assert_eq!(amp_rep.detected_format, CorelFormat::CdrZipX6);
    assert_eq!(amp_rep.document.assets.len(), 1);

    // 2c. Forged central-directory `compressed_size` > archive length must be rejected
    let mut forged = sample_cdr_zip_x4_bytes();
    // Locate first Central Directory File Header signature `PK\x01\x02` (`0x02014b50`)
    let cd_pos = forged
        .windows(4)
        .position(|w| w == b"PK\x01\x02")
        .expect("central directory header");
    // In ZIP CDFH, `compressed_size` (u32 LE) is at offset +20 from `PK\x01\x02`
    let huge_comp: u32 = 16 * 1024 * 1024;
    forged[cd_pos + 20..cd_pos + 24].copy_from_slice(&huge_comp.to_le_bytes());
    let forged_err = import_corel_bytes(&forged).unwrap_err();
    assert!(
        matches!(forged_err, CdrError::ZipError(ref msg) if msg.contains("forged")),
        "expected forged compressed_size rejection, got {forged_err:?}"
    );
}

#[test]
fn regression_3_cmx_rifx_big_endian_and_open_group_flush() {
    // 3a. Big-endian `RIFX` CMX (both 16-bit and 32-bit) round-trips cleanly!
    let orig = generate_sample_document();
    for (prec, expected_fmt) in [
        (CmxPrecision::Bit16, CorelFormat::Cmx16),
        (CmxPrecision::Bit32, CorelFormat::Cmx32),
    ] {
        let rifx = export_cmx_bytes_with_endian(&orig, prec, RiffEndian::Big).unwrap();
        assert_eq!(&rifx[0..4], b"RIFX");
        let rep = import_corel_bytes(&rifx).unwrap();
        assert_eq!(rep.detected_format, expected_fmt);
        assert_eq!(rep.document.pages.len(), 2);
        assert_eq!(rep.document.pages[0].layers.len(), 2);
        let ar = find_obj_by_name(&rep.document, "Arabic Headline");
        match &ar.payload {
            NodePayload::ArtisticText(t) => assert_eq!(t.plain_text(), "مرحباً بكم في كوري"),
            other => panic!("expected Arabic text in RIFX CMX, got {other:?}"),
        }
        assert_eq!(rep.document.assets.len(), 1);
    }

    // 3b. Unclosed `CMD_BEGIN_GROUP` across `CMD_BEGIN_LAYER` / `CMD_BEGIN_PAGE`, AND
    // `CMD_BEGIN_GROUP -> CMD_END_GROUP` before any `CMD_BEGIN_LAYER` must NEVER leak or orphan groups!
    let emit_cmd_le = |stream: &mut Vec<u8>, id: u16, body: &[u8]| {
        stream.extend_from_slice(&id.to_le_bytes());
        stream.extend_from_slice(&(body.len() as u32).to_le_bytes());
        stream.extend_from_slice(body);
    };
    let mut rstr = Vec::new();
    rstr.extend_from_slice(&3u16.to_le_bytes());
    for s in ["OrphanGroup", "Layer1Group", "Layer2"] {
        rstr.extend_from_slice(&65001u16.to_le_bytes());
        rstr.extend_from_slice(&(s.len() as u16).to_le_bytes());
        rstr.extend_from_slice(s.as_bytes());
    }

    let mut page_stream = Vec::new();
    // BeginGroup(0="OrphanGroup") -> EndGroup BEFORE any BeginPage/BeginLayer!
    emit_cmd_le(&mut page_stream, 5, &[0, 0, 0]); // BeginGroup idx=0, has_xf=0
    emit_cmd_le(&mut page_stream, 6, &[]); // EndGroup -> must attach to auto-created layer!
                                           // BeginLayer(flags=5, ms=0, name=2)
    emit_cmd_le(&mut page_stream, 3, &[5, 0, 2, 0]);
    // Unclosed BeginGroup(1="Layer1Group") right before a second BeginPage!
    emit_cmd_le(&mut page_stream, 5, &[1, 0, 0]);
    // BeginPage(210mm, 297mm, 3mm, name=2) -> must flush Layer1Group into Layer2 BEFORE Page 2!
    let mut bp = Vec::new();
    bp.extend_from_slice(&2_100_000i32.to_le_bytes());
    bp.extend_from_slice(&2_970_000i32.to_le_bytes());
    bp.extend_from_slice(&30_000i32.to_le_bytes());
    bp.extend_from_slice(&2u16.to_le_bytes());
    emit_cmd_le(&mut page_stream, 1, &bp);
    emit_cmd_le(&mut page_stream, 2, &[]);

    let mut cont = Vec::new();
    cont.extend_from_slice(b"CMX2");
    cont.extend_from_slice(&4u16.to_le_bytes());
    cont.extend_from_slice(&0x0064u16.to_le_bytes());
    cont.extend_from_slice(&2u16.to_le_bytes());
    let cmx_bytes = write_riff(&RiffDocument::new(
        *b"CMX2",
        vec![
            RiffNode::chunk(*b"cont", cont),
            RiffNode::chunk(*b"rstr", rstr),
            RiffNode::chunk(*b"page", page_stream),
        ],
    ))
    .unwrap();

    let parsed = import_corel_bytes(&cmx_bytes).unwrap().document;
    assert_eq!(parsed.pages.len(), 2);
    // Page 1 must hold both groups on its layers (zero orphaned objects, zero leak into Page 2!)
    assert!(parsed.pages[1].layers.iter().all(|l| l.objects.is_empty()));
    let attached_ids: Vec<ObjectId> = parsed
        .pages
        .iter()
        .flat_map(|p| p.layers.iter())
        .flat_map(|l| l.objects.iter().copied())
        .collect();
    assert_eq!(attached_ids.len(), parsed.objects.len());
    assert_eq!(parsed.objects.len(), 2);
}

#[test]
fn regression_4_cmpr_resource_pass_order_and_unknown_list_passthrough() {
    // 4a. Page inside `LIST cmpr` referencing a sibling `LIST fill` appearing AFTER `LIST cmpr`!
    // Build loda with RECTANGLE + FILL_ID=7
    let mut rect_arg = Vec::new();
    rect_arg.extend_from_slice(&300_000i32.to_le_bytes()); // 30 mm
    rect_arg.extend_from_slice(&200_000i32.to_le_bytes()); // 20 mm
    rect_arg.extend_from_slice(&[0u8; 18]);
    let fill_id_arg = 7u32.to_le_bytes().to_vec();
    // LODA header: 2 args at offsets 12 and 12+rect_arg.len()
    let mut loda = Vec::new();
    let off0 = 12u32;
    let off1 = off0 + rect_arg.len() as u32;
    let args_off = off1 + fill_id_arg.len() as u32;
    let types_off = args_off + 8;
    loda.extend_from_slice(&2u32.to_le_bytes());
    loda.extend_from_slice(&args_off.to_le_bytes());
    loda.extend_from_slice(&types_off.to_le_bytes());
    loda.extend_from_slice(&rect_arg);
    loda.extend_from_slice(&fill_id_arg);
    loda.extend_from_slice(&off0.to_le_bytes());
    loda.extend_from_slice(&off1.to_le_bytes());
    loda.extend_from_slice(&0x0001u32.to_le_bytes()); // RECTANGLE
    loda.extend_from_slice(&0x0014u32.to_le_bytes()); // FILL_ID

    // Build fild id=7, Solid(Color::CYAN) = CMYK255 (255, 0, 0, 0)
    let mut fild = Vec::new();
    fild.extend_from_slice(&7u32.to_le_bytes());
    fild.extend_from_slice(&1u16.to_le_bytes()); // Solid
    fild.extend_from_slice(&3u16.to_le_bytes()); // Cmyk255
    fild.extend_from_slice(&4u16.to_le_bytes());
    fild.extend_from_slice(&[255, 0, 0, 0]);

    // Unknown nested LIST inside `LIST doc ` AND inside `LIST obj `
    let unk_nested_list = RiffNode::list(
        *b"XGRP",
        vec![
            RiffNode::chunk(*b"XSUB", vec![1, 2, 3]),
            RiffNode::list(*b"XINN", vec![RiffNode::chunk(*b"XVAL", vec![9, 8, 7])]),
        ],
    );
    let unk_obj_list = RiffNode::list(*b"XOPL", vec![RiffNode::chunk(*b"XDAT", vec![42, 43, 44])]);

    let cdr = write_riff(&RiffDocument::new(
        *b"CDRC",
        vec![
            RiffNode::chunk(*b"vrsn", 1200u16.to_le_bytes().to_vec()),
            RiffNode::list(
                *b"doc ",
                vec![
                    // Compressed page BEFORE sibling `LIST fill`!
                    RiffNode::cmpr_list(vec![RiffNode::list(
                        *b"page",
                        vec![RiffNode::list(
                            *b"layr",
                            vec![RiffNode::list(
                                *b"obj ",
                                vec![RiffNode::chunk(*b"loda", loda), unk_obj_list],
                            )],
                        )],
                    )]),
                    // Resource table appears AFTER the compressed page!
                    RiffNode::list(*b"fill", vec![RiffNode::chunk(*b"fild", fild)]),
                    // Unknown nested LIST inside `LIST doc `
                    unk_nested_list,
                ],
            ),
        ],
    ))
    .unwrap();

    let doc1 = import_corel_bytes(&cdr).unwrap().document;
    let obj1 = doc1.objects.values().next().expect("resolved object");
    assert_eq!(
        obj1.fill,
        Fill::Solid(Color::CYAN),
        "compressed page must resolve subsequent sibling LIST fill"
    );
    assert_eq!(doc1.unknown_doc_chunks.len(), 1);
    assert_eq!(&doc1.unknown_doc_chunks[0].fourcc, b"LIST");
    assert_eq!(&doc1.unknown_doc_chunks[0].raw_bytes[0..4], b"XGRP");
    assert_eq!(obj1.unknown_chunks.len(), 1);
    assert_eq!(&obj1.unknown_chunks[0].fourcc, b"LIST");
    assert_eq!(&obj1.unknown_chunks[0].raw_bytes[0..4], b"XOPL");

    // Re-export to compressed RiffV12 AND ZipX6 and verify byte-for-byte nested LIST passthrough!
    let expected_doc_raw = doc1.unknown_doc_chunks[0].raw_bytes.clone();
    let expected_obj_raw = obj1.unknown_chunks[0].raw_bytes.clone();

    let re_riff = export_cdr_bytes(&doc1, CdrWriteVersion::RiffV12).unwrap();
    let doc2 = import_corel_bytes(&re_riff).unwrap().document;
    assert_eq!(doc2.unknown_doc_chunks[0].raw_bytes, expected_doc_raw);
    assert_eq!(
        doc2.objects.values().next().unwrap().unknown_chunks[0].raw_bytes,
        expected_obj_raw
    );

    let re_x6 = export_cdr_bytes(&doc2, CdrWriteVersion::ZipX6).unwrap();
    let doc3 = import_corel_bytes(&re_x6).unwrap().document;
    assert_eq!(doc3.unknown_doc_chunks[0].raw_bytes, expected_doc_raw);
    assert_eq!(
        doc3.objects.values().next().unwrap().unknown_chunks[0].raw_bytes,
        expected_obj_raw
    );
}

#[test]
fn regression_5_dib_bi_bitfields_32bit_and_invalid_bpp() {
    let limits = CodecLimits::default();

    // 5a. Standard 40-byte BITMAPINFOHEADER + BI_BITFIELDS (compression=3) without "BM" wrapper:
    // 12-byte R/G/B masks follow the header immediately (RGBA order: R=0xFF000000, G=0x00FF0000, B=0x0000FF00)
    let mut dib_bf = Vec::new();
    dib_bf.extend_from_slice(&40u32.to_le_bytes()); // biSize = 40
    dib_bf.extend_from_slice(&2i32.to_le_bytes()); // width = 2
    dib_bf.extend_from_slice(&(-1i32).to_le_bytes()); // top-down height = 1
    dib_bf.extend_from_slice(&1u16.to_le_bytes()); // planes
    dib_bf.extend_from_slice(&32u16.to_le_bytes()); // bpp = 32
    dib_bf.extend_from_slice(&3u32.to_le_bytes()); // BI_BITFIELDS
    dib_bf.extend_from_slice(&8u32.to_le_bytes()); // image size
    dib_bf.extend_from_slice(&0i32.to_le_bytes());
    dib_bf.extend_from_slice(&0i32.to_le_bytes());
    dib_bf.extend_from_slice(&0u32.to_le_bytes());
    dib_bf.extend_from_slice(&0u32.to_le_bytes());
    // 12-byte masks: R=0xFF000000, G=0x00FF0000, B=0x0000FF00
    dib_bf.extend_from_slice(&0xFF00_0000u32.to_le_bytes());
    dib_bf.extend_from_slice(&0x00FF_0000u32.to_le_bytes());
    dib_bf.extend_from_slice(&0x0000_FF00u32.to_le_bytes());
    // 2 pixels: px0 = (R=0x11, G=0x22, B=0x33), px1 = (R=0xAA, G=0xBB, B=0xCC)
    dib_bf.extend_from_slice(&0x1122_3300u32.to_le_bytes());
    dib_bf.extend_from_slice(&0xAABB_CC00u32.to_le_bytes());

    let (w, h, rgba) = decode_dib_to_rgba(&dib_bf, &limits).unwrap();
    assert_eq!((w, h), (2, 1));
    assert_eq!(rgba, vec![0x11, 0x22, 0x33, 255, 0xAA, 0xBB, 0xCC, 255]);

    // 5b. BITMAPV4HEADER (108 bytes) with BI_BITFIELDS + explicit Alpha mask
    let mut dib_v4 = vec![0u8; 108];
    dib_v4[0..4].copy_from_slice(&108u32.to_le_bytes());
    dib_v4[4..8].copy_from_slice(&1i32.to_le_bytes());
    dib_v4[8..12].copy_from_slice(&(-1i32).to_le_bytes());
    dib_v4[12..14].copy_from_slice(&1u16.to_le_bytes());
    dib_v4[14..16].copy_from_slice(&32u16.to_le_bytes());
    dib_v4[16..20].copy_from_slice(&3u32.to_le_bytes()); // BI_BITFIELDS
    dib_v4[40..44].copy_from_slice(&0x0000_00FFu32.to_le_bytes()); // R
    dib_v4[44..48].copy_from_slice(&0x0000_FF00u32.to_le_bytes()); // G
    dib_v4[48..52].copy_from_slice(&0x00FF_0000u32.to_le_bytes()); // B
    dib_v4[52..56].copy_from_slice(&0xFF00_0000u32.to_le_bytes()); // A
    dib_v4.extend_from_slice(&0x8033_2211u32.to_le_bytes()); // R=0x11, G=0x22, B=0x33, A=0x80
    let (_, _, rgba_v4) = decode_dib_to_rgba(&dib_v4, &limits).unwrap();
    assert_eq!(rgba_v4, vec![0x11, 0x22, 0x33, 0x80]);

    // 5c. BI_BITFIELDS with bpp != 32 (e.g. 24-bpp) must be rejected cleanly
    let mut bad_bf = dib_bf.clone();
    bad_bf[14..16].copy_from_slice(&24u16.to_le_bytes());
    assert!(matches!(
        decode_dib_to_rgba(&bad_bf, &limits).unwrap_err(),
        CdrError::InvalidChunk { .. }
    ));
}

#[test]
fn regression_6_detect_format_truncation_and_vrsn_tmpt_consistency() {
    use coree_cdr::detect_format;

    // Truncated 12-byte RIFF header must FAIL detect_format (consistent with import/inspect!)
    let trunc_riff = b"RIFF\xFF\x00\x00\x00CDRC";
    assert!(detect_format(trunc_riff).is_err());
    assert!(import_corel_bytes(trunc_riff).is_err());
    assert!(inspect_corel_bytes(trunc_riff).is_err());

    // Truncated 8-byte CPT header must FAIL detect_format consistently
    assert!(detect_format(b"CPT9FILE").is_err());

    // RIFF `CDRC` carrying `vrsn = 1300` and `tmpt` marker must detect `CdtRiff { version: 1300 }`
    // consistently across detect_format, import_corel_bytes, and inspect_corel_bytes!
    let tmpt_riff = write_riff(&RiffDocument::new(
        *b"CDRC",
        vec![
            RiffNode::chunk(*b"vrsn", 1300u16.to_le_bytes().to_vec()),
            RiffNode::chunk(*b"tmpt", vec![1]),
            RiffNode::list(*b"doc ", Vec::new()),
        ],
    ))
    .unwrap();
    let expected = CorelFormat::CdtRiff { version: 1300 };
    assert_eq!(detect_format(&tmpt_riff).unwrap(), expected);
    assert_eq!(
        import_corel_bytes(&tmpt_riff).unwrap().detected_format,
        expected
    );
    assert_eq!(
        inspect_corel_bytes(&tmpt_riff).unwrap().detected_format,
        expected
    );
}
