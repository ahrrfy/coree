// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! End-to-end integration tests for `coree-cdr`:
//! - Round-trip fidelity across CDR RIFF v12, CDR X4 ZIP, CDR X6+ ZIP, CDT, and CMX (16/32-bit)
//! - Multi-layer CPT raster import (Raw, PackBits RLE, Zlib, Deflate, RGB/CMYK/Gray + Alpha Masks)
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
    format_report, generate_sample_document, import_cdr_bytes, import_cdr_bytes_with_limits,
    import_cdr_file, import_cdr_file_with_limits, import_cdt_bytes, inspect_cdr_bytes,
    inspect_cdr_file, inspect_cdr_file_with_limits,
    riff::{write_riff, RiffDocument, RiffEndian, RiffNode},
    CdrError, CdrFormat, CdrWriteVersion, CmxPrecision, CodecLimits,
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
    doc.objects()
        .values()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("expected object named '{name}' in document"))
}

fn assert_showcase_cdr_fidelity(orig: &Document, restored: &Document) {
    // 1. Multi-page structure
    assert_eq!(restored.pages().len(), 2);
    assert!((restored.pages()[0].width_mm - 210.0).abs() < 0.01);
    assert!((restored.pages()[0].height_mm - 297.0).abs() < 0.01);
    assert!((restored.pages()[1].width_mm - 215.9).abs() < 0.01);
    assert!((restored.pages()[1].height_mm - 279.4).abs() < 0.01);
    assert_eq!(restored.pages()[1].name, "Back Cover");

    // 2. Layers + CutContour flag
    assert_eq!(restored.pages()[0].layers.len(), 2);
    assert!(restored.pages()[0].layers[1].is_cut_layer);
    assert_eq!(restored.pages()[0].layers[1].name, "CutContour");

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
    let orig_asset = orig.assets().values().next().expect("orig asset");
    let rest_bmp_obj = find_obj_by_name(restored, "Embedded Swatch Bitmap");
    match &rest_bmp_obj.payload {
        NodePayload::Raster(r) => {
            let asset = restored.asset(r.asset_id).expect("restored asset");
            assert_eq!(asset.width, orig_asset.width);
            assert_eq!(asset.height, orig_asset.height);
            assert_eq!(asset.to_rgba8_vec(), orig_asset.to_rgba8_vec());
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
    assert_eq!(restored.unknown_doc_chunks().len(), 1);
    assert_eq!(&restored.unknown_doc_chunks()[0].fourcc, b"XDOC");
    assert_eq!(
        restored.unknown_doc_chunks()[0].raw_bytes,
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

    let report = import_cdr_bytes(&bytes).unwrap();
    assert_eq!(report.detected_format, CdrFormat::CdrRiff { version: 1200 });
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

    let report = import_cdr_bytes(&bytes).unwrap();
    assert_eq!(report.detected_format, CdrFormat::CdrZipX4);
    assert_eq!(report.document.metadata().author, "coree contributors");
    assert_eq!(
        report.document.metadata().subject,
        "Vector & Print Showcase"
    );
    assert_eq!(
        report.document.metadata().keywords,
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

    let report = import_cdr_bytes(&bytes).unwrap();
    assert_eq!(report.detected_format, CdrFormat::CdrZipX6);
    assert_showcase_cdr_fidelity(&orig, &report.document);
}

#[test]
fn cdt_template_riff_and_zip_roundtrip_plus_disk_io() {
    let orig = generate_sample_document();
    let tmp = TempDirGuard::new("disk_io");

    // CDT ZIP X4
    let cdt_x4_path = tmp.file("template_x4.cdt");
    export_cdt(&orig, &cdt_x4_path, CdrWriteVersion::ZipX4).unwrap();
    let rep_x4 = import_cdr_file(&cdt_x4_path).unwrap();
    assert_eq!(rep_x4.detected_format, CdrFormat::CdtZip);
    assert_showcase_cdr_fidelity(&orig, &rep_x4.document);

    // CDT ZIP X6
    let cdt_x6_bytes = export_cdt_bytes(&orig, CdrWriteVersion::ZipX6).unwrap();
    let rep_x6 = import_cdt_bytes(&cdt_x6_bytes, &CodecLimits::default()).unwrap();
    assert_eq!(rep_x6.detected_format, CdrFormat::CdtZip);
    assert_showcase_cdr_fidelity(&orig, &rep_x6.document);

    // CDT RIFF v12
    let cdt_riff_bytes = export_cdt_bytes(&orig, CdrWriteVersion::RiffV12).unwrap();
    let rep_riff = import_cdr_bytes(&cdt_riff_bytes).unwrap();
    assert_eq!(
        rep_riff.detected_format,
        CdrFormat::CdtRiff { version: 1200 }
    );
    assert_showcase_cdr_fidelity(&orig, &rep_riff.document);

    // Disk export_cdr + export_cmx + inspect_cdr_file
    let cdr_path = tmp.file("design.cdr");
    export_cdr(&orig, &cdr_path, CdrWriteVersion::ZipX6).unwrap();
    let inspected = inspect_cdr_file(&cdr_path).unwrap();
    assert_eq!(inspected.detected_format, CdrFormat::CdrZipX6);
    assert_eq!(inspected.page_count, 2);

    let cmx_path = tmp.file("exchange.cmx");
    export_cmx(&orig, &cmx_path).unwrap();
    let cmx_rep = import_cdr_file(&cmx_path).unwrap();
    assert_eq!(cmx_rep.detected_format, CdrFormat::Cmx32);
    assert_eq!(cmx_rep.document.pages().len(), 2);
}

#[test]
fn cmx_16bit_and_32bit_roundtrip() {
    let orig = generate_sample_document();

    for (precision, expected_fmt, bytes) in [
        (
            CmxPrecision::Bit16,
            CdrFormat::Cmx16,
            sample_cmx_16bit_bytes(),
        ),
        (
            CmxPrecision::Bit32,
            CdrFormat::Cmx32,
            sample_cmx_32bit_bytes(),
        ),
    ] {
        let rep = import_cdr_bytes(&bytes).unwrap();
        assert_eq!(rep.detected_format, expected_fmt, "precision {precision:?}");
        let doc = &rep.document;
        assert_eq!(doc.pages().len(), 2);
        assert_eq!(doc.pages()[0].layers.len(), 2);
        assert!(doc.pages()[0].layers[1].is_cut_layer);

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
                let asset = doc.asset(r.asset_id).expect("CMX rimg asset");
                assert_eq!(asset.width, 4);
                assert_eq!(asset.height, 4);
                let orig_asset = orig.assets().values().next().unwrap();
                assert_eq!(asset.to_rgba8_vec(), orig_asset.to_rgba8_vec());
            }
            other => panic!("expected Raster in CMX, got {other:?}"),
        }

        // Document-level unknown chunk passthrough in CMX
        assert_eq!(doc.unknown_doc_chunks().len(), 1);
        assert_eq!(&doc.unknown_doc_chunks()[0].fourcc, b"XDOC");
    }

    // Re-export CMX directly via export_cmx_bytes
    let re = export_cmx_bytes(&orig, CmxPrecision::Bit16).unwrap();
    assert_eq!(&re[8..12], b"CMX1");
}

#[test]
fn cpt_multilayer_raster_and_alpha_mask_import() {
    let bytes = sample_cpt_multilayer_bytes();
    let rep = import_cdr_bytes(&bytes).unwrap();
    assert_eq!(rep.detected_format, CdrFormat::Cpt { version: 9 });

    let doc = &rep.document;
    assert_eq!(doc.pages().len(), 1);
    assert_eq!(doc.pages()[0].layers.len(), 3);
    assert_eq!(doc.assets().len(), 3);

    // Layer 1: Background RGB (Raw)
    let bg = find_obj_by_name(doc, "Background RGB");
    assert_eq!(bg.blend_mode, BlendMode::Normal);
    if let NodePayload::Raster(r) = &bg.payload {
        let asset = &doc.assets()[&r.asset_id];
        assert_eq!((asset.width, asset.height), (8, 6));
        let rgba = asset.to_rgba8_vec();
        assert_eq!(&rgba[0..4], &[0, 0, 220, 255]);
        assert_eq!(&rgba[4..8], &[30, 0, 220, 255]);
    } else {
        panic!("expected Raster layer 1");
    }

    // Layer 2: Magenta CMYK Overlay (RLE strips + Zlib Alpha Mask = 128 + Multiply blend)
    let mag = find_obj_by_name(doc, "Magenta CMYK Overlay");
    assert_eq!(mag.blend_mode, BlendMode::Multiply);
    assert!((mag.opacity - (204.0 / 255.0)).abs() < 1e-4);
    assert!(mag.transform.m[4] > 0.0 && mag.transform.m[5] > 0.0); // offset_x=2, offset_y=1
    if let NodePayload::Raster(r) = &mag.payload {
        let asset = &doc.assets()[&r.asset_id];
        assert_eq!(asset.color_mode, ColorMode::Cmyk);
        // Process Magenta CMYK(0,1,0,0) -> RGB(255, 0, 255), alpha = 128 from Zlib mask
        assert_eq!(&asset.to_rgba8_vec()[0..4], &[255, 0, 255, 128]);
    } else {
        panic!("expected Raster layer 2");
    }

    // Layer 3: Grayscale Mask Layer (Deflate + RLE checker Alpha Mask 255/64 + Screen + locked)
    let gray = find_obj_by_name(doc, "Grayscale Mask Layer");
    assert_eq!(gray.blend_mode, BlendMode::Screen);
    assert!(gray.locked);
    if let NodePayload::Raster(r) = &gray.payload {
        let asset = &doc.assets()[&r.asset_id];
        let rgba = asset.to_rgba8_vec();
        assert_eq!(&rgba[0..4], &[200, 200, 200, 255]);
        assert_eq!(&rgba[4..8], &[200, 200, 200, 64]);
    } else {
        panic!("expected Raster layer 3");
    }

    // Also verify RIFF `CPT ` and CPT7/CPT8 magic variants
    let riff_cpt = import_cdr_bytes(&sample_cpt_riff_bytes()).unwrap();
    assert_eq!(riff_cpt.detected_format, CdrFormat::Cpt { version: 9 });
    let riff_obj = find_obj_by_name(&riff_cpt.document, "RIFF Raster");
    if let NodePayload::Raster(r) = &riff_obj.payload {
        let asset = &riff_cpt.document.assets()[&r.asset_id];
        assert_eq!(&asset.to_rgba8_vec()[0..4], &[100, 150, 200, 180]);
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
    let rep7 = import_cdr_bytes(&cpt7).unwrap();
    assert_eq!(rep7.detected_format, CdrFormat::Cpt { version: 7 });
}

#[test]
fn cp1256_arabic_legacy_cdr_decoding_explicit_and_autodetected() {
    let bytes = sample_cp1256_arabic_cdr_bytes();
    let rep = import_cdr_bytes(&bytes).unwrap();
    assert_eq!(rep.detected_format, CdrFormat::CdrRiff { version: 900 });

    let texts: Vec<(String, TextDirection)> = rep
        .document
        .objects()
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
    let step1 = import_cdr_bytes(&orig_bytes).unwrap().document;
    assert_eq!(step1.unknown_doc_chunks().len(), 1);
    assert_eq!(&step1.unknown_doc_chunks()[0].fourcc, b"XDOC");
    assert_eq!(step1.unknown_doc_chunks()[0].raw_bytes, expected_doc);
    let obj1 = find_obj_by_name(&step1, "Box With Custom Chunk");
    assert_eq!(obj1.unknown_chunks.len(), 1);
    assert_eq!(&obj1.unknown_chunks[0].fourcc, b"XOBJ");
    assert_eq!(obj1.unknown_chunks[0].raw_bytes, expected_obj);

    // Step 2: Export -> RiffV12 -> re-import
    let riff_re = export_cdr_bytes(&step1, CdrWriteVersion::RiffV12).unwrap();
    let step2 = import_cdr_bytes(&riff_re).unwrap().document;
    assert_eq!(step2.unknown_doc_chunks()[0].raw_bytes, expected_doc);
    assert_eq!(
        find_obj_by_name(&step2, "Box With Custom Chunk").unknown_chunks[0].raw_bytes,
        expected_obj
    );

    // Step 3: Export -> ZipX4 -> re-import
    let x4_re = export_cdr_bytes(&step2, CdrWriteVersion::ZipX4).unwrap();
    let step3 = import_cdr_bytes(&x4_re).unwrap().document;
    assert_eq!(step3.unknown_doc_chunks()[0].raw_bytes, expected_doc);
    assert_eq!(
        find_obj_by_name(&step3, "Box With Custom Chunk").unknown_chunks[0].raw_bytes,
        expected_obj
    );

    // Step 4: Export -> ZipX6 -> re-import
    let x6_re = export_cdr_bytes(&step3, CdrWriteVersion::ZipX6).unwrap();
    let step4 = import_cdr_bytes(&x6_re).unwrap().document;
    assert_eq!(step4.unknown_doc_chunks()[0].raw_bytes, expected_doc);
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
                MeshNode::new(0, 0, Point2D::new(0.0, 0.0), Color::CYAN),
                MeshNode::new(1, 1, Point2D::new(40.0, 30.0), Color::YELLOW),
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
    let restored = import_cdr_bytes(&bytes).unwrap().document;

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
    let riff_rep = inspect_cdr_bytes(&sample_cdr_riff_v12_bytes()).unwrap();
    assert_eq!(
        riff_rep.detected_format,
        CdrFormat::CdrRiff { version: 1200 }
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
    let x4_rep = inspect_cdr_bytes(&sample_cdr_zip_x4_bytes()).unwrap();
    assert_eq!(x4_rep.detected_format, CdrFormat::CdrZipX4);
    assert!(x4_rep
        .zip_entries
        .iter()
        .any(|e| e.name == "content/riffData.cdr"));

    let x6_rep = inspect_cdr_bytes(&sample_cdr_zip_x6_bytes()).unwrap();
    assert_eq!(x6_rep.detected_format, CdrFormat::CdrZipX6);
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
    let cdt_rep = inspect_cdr_bytes(&sample_cdt_bytes()).unwrap();
    assert_eq!(cdt_rep.detected_format, CdrFormat::CdtZip);

    let cmx_rep = inspect_cdr_bytes(&sample_cmx_32bit_bytes()).unwrap();
    assert_eq!(cmx_rep.detected_format, CdrFormat::Cmx32);
    assert_eq!(cmx_rep.page_count, 2);

    let cpt_rep = inspect_cdr_bytes(&sample_cpt_multilayer_bytes()).unwrap();
    assert_eq!(cpt_rep.detected_format, CdrFormat::Cpt { version: 9 });
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
    assert!(import_cdr_bytes(&[]).is_err());
    assert!(import_cdr_bytes(&malformed_truncated_header()).is_err());
    assert!(import_cdr_bytes(&malformed_garbage_bytes()).is_err());
    assert!(import_cdr_bytes(&malformed_truncated_chunk()).is_err());
    assert!(inspect_cdr_bytes(&malformed_truncated_header()).is_err());
    assert!(inspect_cdr_bytes(&malformed_garbage_bytes()).is_err());

    // 2. Zip-Slip (`../../etc/passwd`)
    let slip_err = import_cdr_bytes(&malformed_zip_slip_bytes()).unwrap_err();
    assert!(
        matches!(slip_err, CdrError::ZipSlipDetected { .. }),
        "expected ZipSlipDetected, got {slip_err:?}"
    );
    assert!(matches!(
        inspect_cdr_bytes(&malformed_zip_slip_bytes()).unwrap_err(),
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
        let err = import_cdr_bytes(&cursor.into_inner()).unwrap_err();
        assert!(
            matches!(err, CdrError::ZipSlipDetected { .. }),
            "path '{evil_name}' should fail with ZipSlipDetected, got {err:?}"
        );
    }

    // 3. Zip-Bomb (high compression ratio & bounded streaming size cap)
    let bomb_err = import_cdr_bytes(&malformed_zip_bomb_bytes()).unwrap_err();
    assert!(
        matches!(
            bomb_err,
            CdrError::CompressionRatioExceeded { .. }
                | CdrError::UncompressedSizeLimitExceeded { .. }
        ),
        "expected Zip-Bomb rejection, got {bomb_err:?}"
    );
    assert!(inspect_cdr_bytes(&malformed_zip_bomb_bytes()).is_err());

    // 4. Zlib Bomb hidden inside RIFF `LIST cmpr`
    let cmpr_bomb_err = import_cdr_bytes(&malformed_cmpr_zlib_bomb_bytes()).unwrap_err();
    assert!(
        matches!(
            cmpr_bomb_err,
            CdrError::CompressionRatioExceeded { .. }
                | CdrError::UncompressedSizeLimitExceeded { .. }
        ),
        "expected LIST cmpr bomb rejection, got {cmpr_bomb_err:?}"
    );

    // 5. 200-deep nested RIFF `LIST`
    let deep_err = import_cdr_bytes(&malformed_deep_nested_riff_bytes(200)).unwrap_err();
    assert!(
        matches!(deep_err, CdrError::DepthLimitExceeded { .. }),
        "expected DepthLimitExceeded, got {deep_err:?}"
    );

    // 6. Huge bitmap dimensions in CDR (`65536x65536`) and CPT (`100000x100000`)
    let bmp_err = import_cdr_bytes(&malformed_huge_bitmap_cdr_bytes()).unwrap_err();
    assert!(
        matches!(bmp_err, CdrError::InvalidBitmapDimensions { .. }),
        "expected InvalidBitmapDimensions for CDR, got {bmp_err:?}"
    );
    let cpt_err = import_cdr_bytes(&malformed_huge_bitmap_cpt_bytes()).unwrap_err();
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
    let parsed_evil = import_cdr_bytes(&evil_riff).unwrap();
    assert!(!parsed_evil.warnings.is_empty());

    // 8. Tight custom CodecLimits byte cap rejects normal X4 ZIP cleanly
    let tiny_limits = CodecLimits {
        max_uncompressed_bytes: 64,
        ..CodecLimits::default()
    };
    let cap_err =
        import_cdr_bytes_with_limits(&sample_cdr_zip_x4_bytes(), &tiny_limits).unwrap_err();
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
            let _ = import_cdr_bytes(&sample[..len]);
            let _ = inspect_cdr_bytes(&sample[..len]);
        }
        let step = (sample.len() / 32).max(1);
        for len in (0..sample.len()).step_by(step) {
            let _ = import_cdr_bytes(&sample[..len]);
        }
        // Deterministic single-byte mutations across key offsets
        for offset in (0..sample.len().min(256)).step_by(7) {
            let mut mutated = sample.clone();
            mutated[offset] ^= 0xFF;
            let _ = import_cdr_bytes(&mutated);
            let _ = inspect_cdr_bytes(&mutated);
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

    let err = import_cdr_bytes(&cpt).unwrap_err();
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

    let restored = import_cdr_bytes(&bytes).unwrap().document;
    assert_eq!(restored.assets().len(), 2);
    let o1 = find_obj_by_name(&restored, "Img One");
    let o2 = find_obj_by_name(&restored, "Img Two");
    if let (NodePayload::Raster(r1), NodePayload::Raster(r2)) = (&o1.payload, &o2.payload) {
        assert_eq!(
            restored.assets()[&r1.asset_id].to_rgba8_vec(),
            a1.to_rgba8_vec()
        );
        assert_eq!(
            restored.assets()[&r2.asset_id].to_rgba8_vec(),
            a2.to_rgba8_vec()
        );
    } else {
        panic!("expected both raster objects");
    }
}

#[test]
fn detect_format_matches_all_eight_cdr_formats() {
    use coree_cdr::detect_format;

    assert_eq!(
        detect_format(&sample_cdr_riff_v12_bytes()).unwrap(),
        CdrFormat::CdrRiff { version: 1200 }
    );
    assert_eq!(
        detect_format(&sample_cdr_zip_x4_bytes()).unwrap(),
        CdrFormat::CdrZipX4
    );
    assert_eq!(
        detect_format(&sample_cdr_zip_x6_bytes()).unwrap(),
        CdrFormat::CdrZipX6
    );
    assert_eq!(
        detect_format(
            &export_cdt_bytes(&generate_sample_document(), CdrWriteVersion::RiffV12).unwrap()
        )
        .unwrap(),
        CdrFormat::CdtRiff { version: 1200 }
    );
    assert_eq!(
        detect_format(&sample_cdt_bytes()).unwrap(),
        CdrFormat::CdtZip
    );
    assert_eq!(
        detect_format(&sample_cmx_16bit_bytes()).unwrap(),
        CdrFormat::Cmx16
    );
    assert_eq!(
        detect_format(&sample_cmx_32bit_bytes()).unwrap(),
        CdrFormat::Cmx32
    );
    assert_eq!(
        detect_format(&sample_cpt_multilayer_bytes()).unwrap(),
        CdrFormat::Cpt { version: 9 }
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
        import_cdr_file_with_limits(&cdr_path, &tiny).unwrap_err(),
        CdrError::UncompressedSizeLimitExceeded { limit: 256 }
    ));
    assert!(matches!(
        inspect_cdr_file_with_limits(&cdr_path, &tiny).unwrap_err(),
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
            import_cdr_bytes_with_limits(&zip_bytes, &cap_100k).unwrap_err(),
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
            import_cdr_bytes_with_limits(&multi_dib_cdr, &cap_200k).unwrap_err(),
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
        import_cdr_bytes_with_limits(&multi_cpt, &cap_100k).unwrap_err(),
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
    let dup_err = import_cdr_bytes(&dup_cur.into_inner()).unwrap_err();
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
                let mut repeated = String::from("data/data0.dat\n");
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
    // Tight budget (96 KiB): without deduplication, 2000x `data/0001.dat` (~32 MiB) blows past 96 KiB!
    let tight = CodecLimits {
        max_uncompressed_bytes: 96 * 1024,
        ..CodecLimits::default()
    };
    let amp_rep = import_cdr_bytes_with_limits(&amp_cur.into_inner(), &tight).unwrap();
    assert_eq!(amp_rep.detected_format, CdrFormat::CdrZipX6);
    assert_eq!(amp_rep.document.assets().len(), 1);

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
    let forged_err = import_cdr_bytes(&forged).unwrap_err();
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
        (CmxPrecision::Bit16, CdrFormat::Cmx16),
        (CmxPrecision::Bit32, CdrFormat::Cmx32),
    ] {
        let rifx = export_cmx_bytes_with_endian(&orig, prec, RiffEndian::Big).unwrap();
        assert_eq!(&rifx[0..4], b"RIFX");
        let rep = import_cdr_bytes(&rifx).unwrap();
        assert_eq!(rep.detected_format, expected_fmt);
        assert_eq!(rep.document.pages().len(), 2);
        assert_eq!(rep.document.pages()[0].layers.len(), 2);
        let ar = find_obj_by_name(&rep.document, "Arabic Headline");
        match &ar.payload {
            NodePayload::ArtisticText(t) => assert_eq!(t.plain_text(), "مرحباً بكم في كوري"),
            other => panic!("expected Arabic text in RIFX CMX, got {other:?}"),
        }
        assert_eq!(rep.document.assets().len(), 1);
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

    let parsed = import_cdr_bytes(&cmx_bytes).unwrap().document;
    assert_eq!(parsed.pages().len(), 2);
    // Page 1 must hold both groups on its layers (zero orphaned objects, zero leak into Page 2!)
    assert!(parsed.pages()[1]
        .layers
        .iter()
        .all(|l| l.objects.is_empty()));
    let attached_ids: Vec<ObjectId> = parsed
        .pages()
        .iter()
        .flat_map(|p| p.layers.iter())
        .flat_map(|l| l.objects.iter().copied())
        .collect();
    assert_eq!(attached_ids.len(), parsed.object_count());
    assert_eq!(parsed.object_count(), 2);
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

    let doc1 = import_cdr_bytes(&cdr).unwrap().document;
    let obj1 = doc1.objects().values().next().expect("resolved object");
    assert_eq!(
        obj1.fill,
        Fill::Solid(Color::CYAN),
        "compressed page must resolve subsequent sibling LIST fill"
    );
    assert_eq!(doc1.unknown_doc_chunks().len(), 1);
    assert_eq!(&doc1.unknown_doc_chunks()[0].fourcc, b"LIST");
    assert_eq!(&doc1.unknown_doc_chunks()[0].raw_bytes[0..4], b"XGRP");
    assert_eq!(obj1.unknown_chunks.len(), 1);
    assert_eq!(&obj1.unknown_chunks[0].fourcc, b"LIST");
    assert_eq!(&obj1.unknown_chunks[0].raw_bytes[0..4], b"XOPL");

    // Re-export to compressed RiffV12 AND ZipX6 and verify byte-for-byte nested LIST passthrough!
    let expected_doc_raw = doc1.unknown_doc_chunks()[0].raw_bytes.clone();
    let expected_obj_raw = obj1.unknown_chunks[0].raw_bytes.clone();

    let re_riff = export_cdr_bytes(&doc1, CdrWriteVersion::RiffV12).unwrap();
    let doc2 = import_cdr_bytes(&re_riff).unwrap().document;
    assert_eq!(doc2.unknown_doc_chunks()[0].raw_bytes, expected_doc_raw);
    assert_eq!(
        doc2.objects().values().next().unwrap().unknown_chunks[0].raw_bytes,
        expected_obj_raw
    );

    let re_x6 = export_cdr_bytes(&doc2, CdrWriteVersion::ZipX6).unwrap();
    let doc3 = import_cdr_bytes(&re_x6).unwrap().document;
    assert_eq!(doc3.unknown_doc_chunks()[0].raw_bytes, expected_doc_raw);
    assert_eq!(
        doc3.objects().values().next().unwrap().unknown_chunks[0].raw_bytes,
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
    assert!(import_cdr_bytes(trunc_riff).is_err());
    assert!(inspect_cdr_bytes(trunc_riff).is_err());

    // Truncated 8-byte CPT header must FAIL detect_format consistently
    assert!(detect_format(b"CPT9FILE").is_err());

    // RIFF `CDRC` carrying `vrsn = 1300` and `tmpt` marker must detect `CdtRiff { version: 1300 }`
    // consistently across detect_format, import_cdr_bytes, and inspect_cdr_bytes!
    let tmpt_riff = write_riff(&RiffDocument::new(
        *b"CDRC",
        vec![
            RiffNode::chunk(*b"vrsn", 1300u16.to_le_bytes().to_vec()),
            RiffNode::chunk(*b"tmpt", vec![1]),
            RiffNode::list(*b"doc ", Vec::new()),
        ],
    ))
    .unwrap();
    let expected = CdrFormat::CdtRiff { version: 1300 };
    assert_eq!(detect_format(&tmpt_riff).unwrap(), expected);
    assert_eq!(
        import_cdr_bytes(&tmpt_riff).unwrap().detected_format,
        expected
    );
    assert_eq!(
        inspect_cdr_bytes(&tmpt_riff).unwrap().detected_format,
        expected
    );
}

// =============================================================================
// Clean-Room ZCF v23 (X6–2025) Indirect Pointer, Preview PNG & Audit Tests
// =============================================================================

fn zcf_inline_ptr(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= 8);
    let mut out = vec![0u8; 16];
    out[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
    out[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    out[8..8 + payload.len()].copy_from_slice(payload);
    out
}

fn zcf_ext_ptr(file_idx: u32, blob: &mut Vec<u8>, payload: &[u8]) -> Vec<u8> {
    let off = blob.len() as u32;
    let len = payload.len() as u32;
    blob.extend_from_slice(payload);
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&file_idx.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&off.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// Builds an authentic 20-byte header ZCF `loda` chunk with reversed `types` table.
fn build_zcf_loda(chunk_type: u32, args: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let n = args.len();
    let args_off = 20usize;
    let offsets_table_len = (n + 1) * 4;
    let types_off = args_off + offsets_table_len;
    let data_start = types_off + n * 4;

    let mut offsets: Vec<u32> = Vec::with_capacity(n + 1);
    let mut payload_buf = Vec::new();
    for (_, seg) in args {
        offsets.push((data_start + payload_buf.len()) as u32);
        payload_buf.extend_from_slice(seg);
    }
    let total_len = (data_start + payload_buf.len()) as u32;
    offsets.push(total_len);

    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(&total_len.to_le_bytes());
    out.extend_from_slice(&(n as u32).to_le_bytes());
    out.extend_from_slice(&(args_off as u32).to_le_bytes());
    out.extend_from_slice(&(types_off as u32).to_le_bytes());
    out.extend_from_slice(&chunk_type.to_le_bytes());
    for o in offsets {
        out.extend_from_slice(&o.to_le_bytes());
    }
    // Authentic ZCF stores arg types in REVERSE order relative to offsets
    for (typ, _) in args.iter().rev() {
        out.extend_from_slice(&typ.to_le_bytes());
    }
    out.extend_from_slice(&payload_buf);
    out
}

/// Builds an authentic 96-byte ZCF v16–26 `trfd` matrix chunk (`[v0, v1, x0, v3, v4, y0]`).
fn build_zcf_trfd(v0: f64, v1: f64, x0_cdr: f64, v3: f64, v4: f64, y0_cdr: f64) -> Vec<u8> {
    let mut out = vec![0u8; 96];
    out[0..4].copy_from_slice(&96u32.to_le_bytes());
    out[4..8].copy_from_slice(&1u32.to_le_bytes());
    out[8..12].copy_from_slice(&16u32.to_le_bytes());
    out[12..16].copy_from_slice(&0xFFFFu32.to_le_bytes());
    out[16..20].copy_from_slice(&24u32.to_le_bytes());
    out[20..24].copy_from_slice(&96u32.to_le_bytes());
    out[24..28].copy_from_slice(&0x06CCu32.to_le_bytes());
    out[28..32].copy_from_slice(&0x38u32.to_le_bytes());
    out[32..34].copy_from_slice(&0x0008u16.to_le_bytes());
    for (i, val) in [v0, v1, x0_cdr, v3, v4, y0_cdr].iter().enumerate() {
        let base = 40 + i * 8;
        out[base..base + 8].copy_from_slice(&val.to_le_bytes());
    }
    out
}

/// Builds an authentic 213-byte ZCF v16–26 `outl` chunk.
fn build_zcf_outl(id: u32, model: u16, ch: [u8; 4], line_type: u16, width_cdr: u32) -> Vec<u8> {
    let mut out = vec![0u8; 213];
    out[0..4].copy_from_slice(&id.to_le_bytes());
    out[4..8].copy_from_slice(&3u32.to_le_bytes());
    out[8..12].copy_from_slice(&4u32.to_le_bytes());
    out[12..16].copy_from_slice(&45_000_000u32.to_le_bytes());
    out[16..20].copy_from_slice(&2u32.to_le_bytes());
    out[20..24].copy_from_slice(&43u32.to_le_bytes());
    out[24] = 0x01;
    out[25..29].copy_from_slice(&12u32.to_le_bytes());
    out[29..31].copy_from_slice(&model.to_le_bytes());
    out[31..33].copy_from_slice(&5u16.to_le_bytes());
    out[37..41].copy_from_slice(&ch);
    out[67..71].copy_from_slice(&1u32.to_le_bytes());
    out[71..75].copy_from_slice(&130u32.to_le_bytes());
    out[75..77].copy_from_slice(&line_type.to_le_bytes());
    out[81..85].copy_from_slice(&width_cdr.to_le_bytes());
    out[85..87].copy_from_slice(&100u16.to_le_bytes());
    out
}

fn build_zcf_c9(json: &str) -> Vec<u8> {
    let jb = json.as_bytes();
    let mut out = Vec::with_capacity(8 + jb.len());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(jb.len() as u32).to_le_bytes());
    out.extend_from_slice(jb);
    out
}

fn build_zcf_rect128(
    w_cdr: f64,
    h_cdr: f64,
    sx: f64,
    sy: f64,
    corner_fracs: [f64; 4],
    style: u32,
) -> Vec<u8> {
    let mut out = vec![0u8; 128];
    out[0..8].copy_from_slice(&w_cdr.to_le_bytes());
    out[8..16].copy_from_slice(&h_cdr.to_le_bytes());
    out[16..24].copy_from_slice(&sx.to_le_bytes());
    out[24..32].copy_from_slice(&sy.to_le_bytes());
    for (i, &frac) in corner_fracs.iter().enumerate() {
        let base = 32 + i * 24;
        out[base + 4..base + 8].copy_from_slice(&0x7FFFu32.to_le_bytes());
        out[base + 8..base + 16].copy_from_slice(&frac.to_le_bytes());
        out[base + 16..base + 20].copy_from_slice(&style.to_le_bytes());
    }
    out
}

fn build_zcf_v23_fixture_bytes() -> Vec<u8> {
    use coree_cdr::coord::encode_utf16le;

    let mut data1 = Vec::new();
    let mut master_dat = Vec::new();
    let mut page1_dat = Vec::new();
    let mut page2_dat = Vec::new();
    let mut page3_dat = Vec::new();

    // `mcfg` (v23: 7944-byte style header with 210x297 mm A4 at offset 12..20)
    let mut mcfg = vec![0u8; 128];
    mcfg[0..4].copy_from_slice(&2100u32.to_le_bytes());
    mcfg[4..8].copy_from_slice(&120u32.to_le_bytes());
    mcfg[12..16].copy_from_slice(&2_100_000i32.to_le_bytes());
    mcfg[16..20].copy_from_slice(&2_970_000i32.to_le_bytes());
    let mcfg_ptr = zcf_ext_ptr(0, &mut data1, &mcfg);

    // `fild` id=1 (None, 22 bytes)
    let mut fild_none = vec![0u8; 22];
    fild_none[0..4].copy_from_slice(&1u32.to_le_bytes());
    fild_none[4..8].copy_from_slice(&0x0514u32.to_le_bytes());
    fild_none[8..12].copy_from_slice(&2u32.to_le_bytes());
    let fild1_ptr = zcf_ext_ptr(0, &mut data1, &fild_none);

    // `outl`: id=1 Red CMYK255 hairline, id=2 None (line_type=1), id=3 Black CMYK100 hairline
    let outl1_ptr = zcf_ext_ptr(
        0,
        &mut data1,
        &build_zcf_outl(1, 3, [0, 255, 255, 0], 2, 762),
    );
    let outl2_ptr = zcf_ext_ptr(0, &mut data1, &build_zcf_outl(2, 2, [0, 0, 0, 100], 1, 50));
    let outl3_ptr = zcf_ext_ptr(0, &mut data1, &build_zcf_outl(3, 2, [0, 0, 0, 100], 2, 762));

    // Master Page (flgs = 0x9001_0040 -> Master bit set, empty Guides layer)
    let master_guides_loda = build_zcf_loda(0x0C, &[(0x03E8, encode_utf16le("Guides\0"))]);
    let master_page = RiffNode::list(
        *b"page",
        vec![
            RiffNode::chunk(*b"flgs", zcf_inline_ptr(&0x9001_0040u32.to_le_bytes())),
            RiffNode::list(
                *b"gobj",
                vec![RiffNode::list(
                    *b"layr",
                    vec![
                        RiffNode::chunk(*b"flgs", zcf_inline_ptr(&0x9801_000Au32.to_le_bytes())),
                        RiffNode::list(
                            *b"lgob",
                            vec![RiffNode::chunk(
                                *b"loda",
                                zcf_ext_ptr(1, &mut master_dat, &master_guides_loda),
                            )],
                        ),
                    ],
                )],
            ),
        ],
    );

    // Page 1: Group (with large group trfd that MUST NOT double-apply) containing:
    //  - Cubic Bezier curve (`ctype=0x03`) using authentic `0xC0` control points + Red hairline
    //  - Horizontally flipped (`sx = -1.3387`) Chamfered Rectangle (`ctype=0x01`) 130x230 mm
    //  - Plus an empty system layer (`Guides`, `ctype=0x0C`) that should be omitted cleanly
    let mut curve_arg = Vec::new();
    // 5 points: MoveTo(0,0), Ctrl(100000, 200000), Ctrl(300000, 200000), CurveTo(400000, 0), LineTo(0,0, closed)
    curve_arg.extend_from_slice(&5u32.to_le_bytes());
    for (x, y) in [
        (0i32, 0i32),
        (100_000, 200_000),
        (300_000, 200_000),
        (400_000, 0),
        (0, 0),
    ] {
        curve_arg.extend_from_slice(&x.to_le_bytes());
        curve_arg.extend_from_slice(&y.to_le_bytes());
    }
    curve_arg.extend_from_slice(&[0x0C, 0xC0, 0xC0, 0x84, 0x48]);

    let p1_curve_loda = build_zcf_loda(
        0x03,
        &[
            (
                0x00C9,
                build_zcf_c9(
                    r#"{"fill":{"primaryColor":"CMYK,USER,0,0,0,100,100,0000"},"outline":{"width":"762","color":"CMYK255,USER,0,255,255,0,100,0000"}}"#,
                ),
            ),
            (0x001E, curve_arg),
            (0x0014, 1u32.to_le_bytes().to_vec()),
            (0x000A, 1u32.to_le_bytes().to_vec()),
        ],
    );
    let p1_curve_trfd = build_zcf_trfd(1.0, 0.0, -200_000.0, 0.0, 1.0, -100_000.0);

    // Horizontally flipped chamfered rect: 130x230 mm centered at (-0.07, -0.01) mm -> [39.93..169.93] x [33.51..263.51]
    let p1_rect_arg = build_zcf_rect128(
        971_099.0,
        1_873_245.0,
        -1.3386942,
        1.2278159,
        [0.0, 35.0 / 65.0, 0.0, 0.0],
        2,
    );
    let p1_rect_loda = build_zcf_loda(
        0x01,
        &[
            (0x001E, p1_rect_arg),
            (0x0014, 1u32.to_le_bytes().to_vec()),
            (0x000A, 3u32.to_le_bytes().to_vec()),
        ],
    );
    let p1_rect_trfd = build_zcf_trfd(-1.3386942, 0.0, 649_287.0, 0.0, 1.2278159, -1_150_053.0);

    let p1_grp_loda = build_zcf_loda(0x00, &[(0x00C9, build_zcf_c9("{}"))]);
    let p1_grp_trfd = build_zcf_trfd(1.0, 0.0, 5_000_000.0, 0.0, 1.0, 2_000_000.0);
    let p1_layr_loda = build_zcf_loda(0x00, &[(0x03E8, encode_utf16le("Layer 1\0"))]);
    let p1_guides_loda = build_zcf_loda(0x0C, &[(0x03E8, encode_utf16le("Guides\0"))]);

    let page1 = RiffNode::list(
        *b"page",
        vec![
            RiffNode::chunk(*b"flgs", zcf_inline_ptr(&0x9000_0040u32.to_le_bytes())),
            RiffNode::list(
                *b"gobj",
                vec![
                    RiffNode::list(
                        *b"layr",
                        vec![RiffNode::list(
                            *b"lgob",
                            vec![RiffNode::chunk(
                                *b"loda",
                                zcf_ext_ptr(2, &mut page1_dat, &p1_guides_loda),
                            )],
                        )],
                    ),
                    RiffNode::list(
                        *b"layr",
                        vec![
                            RiffNode::chunk(
                                *b"flgs",
                                zcf_inline_ptr(&0x9800_0040u32.to_le_bytes()),
                            ),
                            RiffNode::list(
                                *b"lgob",
                                vec![RiffNode::chunk(
                                    *b"loda",
                                    zcf_ext_ptr(2, &mut page1_dat, &p1_layr_loda),
                                )],
                            ),
                            RiffNode::list(
                                *b"grp ",
                                vec![
                                    RiffNode::list(
                                        *b"lgob",
                                        vec![
                                            RiffNode::chunk(
                                                *b"loda",
                                                zcf_ext_ptr(2, &mut page1_dat, &p1_grp_loda),
                                            ),
                                            RiffNode::list(
                                                *b"trfl",
                                                vec![RiffNode::chunk(
                                                    *b"trfd",
                                                    zcf_ext_ptr(2, &mut page1_dat, &p1_grp_trfd),
                                                )],
                                            ),
                                        ],
                                    ),
                                    RiffNode::list(
                                        *b"obj ",
                                        vec![RiffNode::list(
                                            *b"lgob",
                                            vec![
                                                RiffNode::chunk(
                                                    *b"loda",
                                                    zcf_ext_ptr(2, &mut page1_dat, &p1_curve_loda),
                                                ),
                                                RiffNode::list(
                                                    *b"trfl",
                                                    vec![RiffNode::chunk(
                                                        *b"trfd",
                                                        zcf_ext_ptr(
                                                            2,
                                                            &mut page1_dat,
                                                            &p1_curve_trfd,
                                                        ),
                                                    )],
                                                ),
                                            ],
                                        )],
                                    ),
                                    RiffNode::list(
                                        *b"obj ",
                                        vec![RiffNode::list(
                                            *b"lgob",
                                            vec![
                                                RiffNode::chunk(
                                                    *b"loda",
                                                    zcf_ext_ptr(2, &mut page1_dat, &p1_rect_loda),
                                                ),
                                                RiffNode::list(
                                                    *b"trfl",
                                                    vec![RiffNode::chunk(
                                                        *b"trfd",
                                                        zcf_ext_ptr(
                                                            2,
                                                            &mut page1_dat,
                                                            &p1_rect_trfd,
                                                        ),
                                                    )],
                                                ),
                                            ],
                                        )],
                                    ),
                                ],
                            ),
                        ],
                    ),
                ],
            ),
        ],
    );

    // Page 2: Unflipped Chamfered Rectangle (TR chamfer = 35mm, 130x190 mm) + Solid-filled Ellipse
    let p2_rect_arg = build_zcf_rect128(
        971_099.0,
        1_873_245.0,
        1.3386942,
        1.0142827,
        [0.0, 35.0 / 65.0, 0.0, 0.0],
        2,
    );
    let p2_rect_loda = build_zcf_loda(
        0x01,
        &[
            (0x001E, p2_rect_arg),
            (0x0014, 1u32.to_le_bytes().to_vec()),
            (0x000A, 3u32.to_le_bytes().to_vec()),
        ],
    );
    let p2_rect_trfd = build_zcf_trfd(1.3386942, 0.0, -650_713.0, 0.0, 1.0142827, -950_053.0);

    let mut ell_arg = Vec::new();
    ell_arg.extend_from_slice(&400_000i32.to_le_bytes()); // w = 40mm -> rx = 20mm
    ell_arg.extend_from_slice(&200_000i32.to_le_bytes()); // h = 20mm -> ry = 10mm
    ell_arg.extend_from_slice(&0i32.to_le_bytes());
    ell_arg.extend_from_slice(&0i32.to_le_bytes());
    ell_arg.extend_from_slice(&0u32.to_le_bytes());
    let p2_ell_loda = build_zcf_loda(
        0x02,
        &[
            (
                0x00C9,
                build_zcf_c9(
                    r#"{"fill":{"type":"1","primaryColor":"CMYK,USER,0,0,100,0,100,0000"},"outline":{"width":"762","color":"CMYK,USER,0,0,0,100,100,0000"}}"#,
                ),
            ),
            (0x001E, ell_arg),
            (0x0014, 1u32.to_le_bytes().to_vec()),
            (0x000A, 2u32.to_le_bytes().to_vec()), // explicit none outl id=2 -> stays none!
        ],
    );
    let p2_ell_trfd = build_zcf_trfd(1.0, 0.0, -200_000.0, 0.0, 1.0, -100_000.0);
    let p2_layr_loda = build_zcf_loda(0x00, &[(0x03E8, encode_utf16le("Layer 1\0"))]);

    let page2 = RiffNode::list(
        *b"page",
        vec![
            RiffNode::chunk(*b"flgs", zcf_inline_ptr(&0x9000_0040u32.to_le_bytes())),
            RiffNode::list(
                *b"gobj",
                vec![RiffNode::list(
                    *b"layr",
                    vec![
                        RiffNode::list(
                            *b"lgob",
                            vec![RiffNode::chunk(
                                *b"loda",
                                zcf_ext_ptr(3, &mut page2_dat, &p2_layr_loda),
                            )],
                        ),
                        RiffNode::list(
                            *b"obj ",
                            vec![RiffNode::list(
                                *b"lgob",
                                vec![
                                    RiffNode::chunk(
                                        *b"loda",
                                        zcf_ext_ptr(3, &mut page2_dat, &p2_rect_loda),
                                    ),
                                    RiffNode::list(
                                        *b"trfl",
                                        vec![RiffNode::chunk(
                                            *b"trfd",
                                            zcf_ext_ptr(3, &mut page2_dat, &p2_rect_trfd),
                                        )],
                                    ),
                                ],
                            )],
                        ),
                        RiffNode::list(
                            *b"obj ",
                            vec![RiffNode::list(
                                *b"lgob",
                                vec![
                                    RiffNode::chunk(
                                        *b"loda",
                                        zcf_ext_ptr(3, &mut page2_dat, &p2_ell_loda),
                                    ),
                                    RiffNode::list(
                                        *b"trfl",
                                        vec![RiffNode::chunk(
                                            *b"trfd",
                                            zcf_ext_ptr(3, &mut page2_dat, &p2_ell_trfd),
                                        )],
                                    ),
                                ],
                            )],
                        ),
                    ],
                )],
            ),
        ],
    );

    // Page 3: Arabic UTF-16LE Artistic Text (`ctype=0x06` + real `txsm`)
    let arabic = "شكر وعرفان";
    let arabic_u16 = encode_utf16le(arabic);
    let mut txsm = Vec::new();
    txsm.extend_from_slice(&0u32.to_le_bytes()); // frame flag
    txsm.extend_from_slice(&42u32.to_le_bytes()); // text_id
    txsm.extend_from_slice(&1u32.to_le_bytes()); // encoding = 1 (UTF-16LE)
    txsm.extend_from_slice(&((arabic_u16.len() / 2) as u32).to_le_bytes());
    txsm.extend_from_slice(&arabic_u16);
    let p3_txt_loda = build_zcf_loda(0x06, &[(0x001E, 42u32.to_le_bytes().to_vec())]);
    let p3_txt_trfd = build_zcf_trfd(1.0, 0.0, 0.0, 0.0, 1.0, 500_000.0);
    let p3_layr_loda = build_zcf_loda(0x00, &[(0x03E8, encode_utf16le("Layer 1\0"))]);

    let page3 = RiffNode::list(
        *b"page",
        vec![
            RiffNode::chunk(*b"flgs", zcf_inline_ptr(&0x9000_0040u32.to_le_bytes())),
            RiffNode::list(
                *b"gobj",
                vec![RiffNode::list(
                    *b"layr",
                    vec![
                        RiffNode::list(
                            *b"lgob",
                            vec![RiffNode::chunk(
                                *b"loda",
                                zcf_ext_ptr(4, &mut page3_dat, &p3_layr_loda),
                            )],
                        ),
                        RiffNode::list(
                            *b"obj ",
                            vec![RiffNode::list(
                                *b"lgob",
                                vec![
                                    RiffNode::chunk(
                                        *b"loda",
                                        zcf_ext_ptr(4, &mut page3_dat, &p3_txt_loda),
                                    ),
                                    RiffNode::chunk(
                                        *b"txsm",
                                        zcf_ext_ptr(4, &mut page3_dat, &txsm),
                                    ),
                                    RiffNode::list(
                                        *b"trfl",
                                        vec![RiffNode::chunk(
                                            *b"trfd",
                                            zcf_ext_ptr(4, &mut page3_dat, &p3_txt_trfd),
                                        )],
                                    ),
                                ],
                            )],
                        ),
                    ],
                )],
            ),
        ],
    );

    let root_doc = RiffDocument::new(
        *b"CDRO",
        vec![
            RiffNode::chunk(
                *b"fver",
                zcf_inline_ptr(&[0xFC, 0x08, 0x01, 0x00, 0x00, 0x00, 0x17, 0x00]),
            ),
            RiffNode::chunk(*b"vrsn", zcf_inline_ptr(&2300u16.to_le_bytes())),
            RiffNode::list(
                *b"doc ",
                vec![
                    RiffNode::chunk(*b"mcfg", mcfg_ptr),
                    RiffNode::list(*b"filt", vec![RiffNode::chunk(*b"fild", fild1_ptr)]),
                    RiffNode::list(
                        *b"otlt",
                        vec![
                            RiffNode::chunk(*b"outl", outl1_ptr),
                            RiffNode::chunk(*b"outl", outl2_ptr),
                            RiffNode::chunk(*b"outl", outl3_ptr),
                        ],
                    ),
                ],
            ),
            master_page,
            page1,
            page2,
            page3,
        ],
    );
    let root_bytes = write_riff(&root_doc).unwrap();

    // Build minimal valid 2x2 PNG via `extract_cdr_preview_png` round-trip fixture
    let bmp_2x2 = coree_cdr::cdr_riff::encode_bmp_file_rgba(2, 2, &[220u8; 16]).unwrap();
    let mut temp_zip = Cursor::new(Vec::new());
    {
        let mut zw = ZipWriter::new(&mut temp_zip);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zw.start_file("preview/thumbnail.bmp", opts).unwrap();
        zw.write_all(&bmp_2x2).unwrap();
        zw.finish().unwrap();
    }
    let thumb_png =
        coree_cdr::extract_cdr_preview_png(&temp_zip.into_inner(), &CodecLimits::default())
            .unwrap()
            .unwrap();

    let mut cur = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cur);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"application/x-vnd.corel.zcf.draw.document+zip")
            .unwrap();
        zip.start_file("content/dataFileList.dat", deflated)
            .unwrap();
        zip.write_all(b"data1.dat\nmasterPage.dat\npage1.dat\npage2.dat\npage3.dat")
            .unwrap();
        zip.start_file("content/root.dat", deflated).unwrap();
        zip.write_all(&root_bytes).unwrap();
        zip.start_file("content/data/data1.dat", deflated).unwrap();
        zip.write_all(&data1).unwrap();
        zip.start_file("content/data/masterPage.dat", deflated)
            .unwrap();
        zip.write_all(&master_dat).unwrap();
        zip.start_file("content/data/page1.dat", deflated).unwrap();
        zip.write_all(&page1_dat).unwrap();
        zip.start_file("content/data/page2.dat", deflated).unwrap();
        zip.write_all(&page2_dat).unwrap();
        zip.start_file("content/data/page3.dat", deflated).unwrap();
        zip.write_all(&page3_dat).unwrap();

        zip.start_file("color/docPalette.xml", deflated).unwrap();
        zip.write_all(
            br#"<?xml version="1.0"?><palette name="Document Palette"><colors><page><color cs="CMYK" name="Black" tints="0,0,0,1"/><color cs="CMYK" name="Red" tints="0,1,1,0"/><color cs="CMYK" name="Dark Brown" tints="0,0.2,0.2,0.6"/><color cs="CMYK" name="White" tints="0,0,0,0"/><color cs="CMYK" name="Yellow" tints="0,0,1,0"/></page></colors></palette>"#,
        )
        .unwrap();
        zip.start_file("color/color.xml", deflated).unwrap();
        zip.write_all(b"<Color><ColorContext><ColorModel>Cmyk</ColorModel></ColorContext></Color>")
            .unwrap();
        zip.start_file("styles/document.cdss", deflated).unwrap();
        zip.write_all(
            br#"{"version":2300,"style":{"~graphic~":{"fill":{"primaryColor":"CMYK,USER,0,0,0,100,100,0000"},"outline":{"width":"762","color":"CMYK,USER,0,0,0,100,100,0000"}},"~artistic~":{"character":{"fill":{"type":"1","primaryColor":"CMYK,USER,0,0,0,100,100,0000"},"RTL":{"font":"Noto Naskh Arabic","size":"84667","charset":"178"}}}}}"#,
        )
        .unwrap();
        zip.start_file("META-INF/textinfo.xml", deflated).unwrap();
        zip.write_all(
            br#"<?xml version="1.0"?><x:xmpmeta><rdf:RDF><rdf:Description><LayerNames><rdf:Bag><rdf:li>Layer 1</rdf:li></rdf:Bag></LayerNames></rdf:Description></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        zip.start_file("META-INF/metadata.xml", deflated).unwrap();
        zip.write_all(
            br#"<?xml version="1.0"?><x:xmpmeta><rdf:RDF><rdf:Description><cdr:CoreVersion>2300</cdr:CoreVersion><cdrinfo:PageWidth>2100000</cdrinfo:PageWidth><cdrinfo:PageHeight>2970000</cdrinfo:PageHeight></rdf:Description></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        zip.start_file("previews/thumbnail.png", stored).unwrap();
        zip.write_all(&thumb_png).unwrap();
        zip.finish().unwrap();
    }
    cur.into_inner()
}

#[test]
fn zcf_v23_authentic_indirect_multipage_and_cdss_palette_verification() {
    use coree_cdr::cdr_zip::{parse_cdr_zip, CdrZipEra};
    use coree_cdr::detect_format;
    use coree_core::Affine2D;

    let bytes = build_zcf_v23_fixture_bytes();
    assert_eq!(detect_format(&bytes).unwrap(), CdrFormat::CdrZipX6);

    let parsed = parse_cdr_zip(&bytes, &CodecLimits::default()).unwrap();
    assert_eq!(parsed.era, CdrZipEra::X6);
    assert_eq!(parsed.version, 2300);
    assert!(!parsed.is_template);
    assert!(
        parsed.warnings.is_empty(),
        "unexpected warnings: {:?}",
        parsed.warnings
    );

    let doc = &parsed.document;
    assert_eq!(doc.color_mode(), ColorMode::Cmyk);
    // 3 real pages (Master Page excluded cleanly!), 1 user layer each (`Guides` omitted)
    assert_eq!(doc.pages().len(), 3);
    for p in doc.pages() {
        assert!((p.width_mm - 210.0).abs() < 0.01);
        assert!((p.height_mm - 297.0).abs() < 0.01);
        assert_eq!(p.layers.len(), 1);
        assert_eq!(p.layers[0].name, "Layer 1");
    }

    // Document palette: 5 authentic CMYK swatches
    let pal_names: Vec<&str> = doc
        .document_palette()
        .swatches
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        pal_names,
        vec!["Black", "Red", "Dark Brown", "White", "Yellow"]
    );

    // Page 1: Group has IDENTITY transform (not double-applied!), 2 children (cubic curve + flipped chamfered rect)
    assert_eq!(doc.pages()[0].layers[0].objects.len(), 1);
    let grp = &doc.objects()[&doc.pages()[0].layers[0].objects[0]];
    assert_eq!(grp.transform, Affine2D::IDENTITY);
    assert!(!grp.outline.is_visible());
    let NodePayload::Group(ref children) = grp.payload else {
        panic!("expected Group on Page 1");
    };
    assert_eq!(children.len(), 2);

    // P1 Child 0: Curve with authentic 0xC0 cubic Bezier control handles preserved + Red CMYK hairline + no fill
    let p1_curve = &doc.objects()[&children[0]];
    assert_eq!(p1_curve.fill, Fill::None);
    assert!(p1_curve.outline.is_visible());
    assert!((p1_curve.outline.width_mm - 0.0762).abs() < 1e-4);
    assert_eq!(p1_curve.outline.color, Color::cmyk(0.0, 1.0, 1.0, 0.0));
    let NodePayload::Path(ref path) = p1_curve.payload else {
        panic!("expected Path");
    };
    assert_eq!(path.subpaths.len(), 1);
    assert!(path.subpaths[0].closed);
    // Verify BOTH cubic Bezier handles (from 0xC0 flags) survived!
    assert!(path.subpaths[0].nodes[0].handle_out.is_some());
    assert!(path.subpaths[0].nodes[1].handle_in.is_some());
    let cbb = path.bounding_box().unwrap();
    assert!(cbb.min_x >= 0.0 && cbb.max_x <= 210.0 && cbb.min_y >= 0.0 && cbb.max_y <= 297.0);

    // P1 Child 1: Horizontally flipped Chamfered Rectangle -> Top-Left chamfer = 35mm!
    let p1_rect = &doc.objects()[&children[1]];
    assert_eq!(p1_rect.outline.color, Color::cmyk(0.0, 0.0, 0.0, 1.0));
    let NodePayload::Shape(ParametricShape::Rectangle {
        width,
        height,
        corner_radii,
        corner_style,
        ..
    }) = p1_rect.payload
    else {
        panic!("expected Rectangle on P1");
    };
    assert!((width - 130.0).abs() < 0.05);
    assert!((height - 230.0).abs() < 0.05);
    assert_eq!(corner_style, CornerStyle::Chamfered);
    assert!(
        (corner_radii[0] - 35.0).abs() < 0.05,
        "TL chamfer expected: {corner_radii:?}"
    );
    assert!(corner_radii[1].abs() < 1e-3);

    // Page 2: Unflipped Chamfered Rectangle (TR chamfer = 35mm) + Solid-Yellow Ellipse (explicit none outline)
    assert_eq!(doc.pages()[1].layers[0].objects.len(), 2);
    let p2_rect = &doc.objects()[&doc.pages()[1].layers[0].objects[0]];
    if let NodePayload::Shape(ParametricShape::Rectangle {
        width,
        height,
        corner_radii,
        corner_style,
        ..
    }) = p2_rect.payload
    {
        assert!((width - 130.0).abs() < 0.05);
        assert!((height - 190.0).abs() < 0.05);
        assert_eq!(corner_style, CornerStyle::Chamfered);
        assert!(corner_radii[0].abs() < 1e-3);
        assert!(
            (corner_radii[1] - 35.0).abs() < 0.05,
            "TR chamfer expected: {corner_radii:?}"
        );
    } else {
        panic!("expected Rectangle on P2");
    }
    let p2_ell = &doc.objects()[&doc.pages()[1].layers[0].objects[1]];
    assert_eq!(p2_ell.fill, Fill::Solid(Color::YELLOW));
    assert!(
        !p2_ell.outline.is_visible(),
        "explicit none outline id=2 must stay invisible"
    );
    assert!(matches!(
        p2_ell.payload,
        NodePayload::Shape(ParametricShape::Ellipse { rx, ry, .. })
            if (rx - 20.0).abs() < 0.05 && (ry - 10.0).abs() < 0.05
    ));

    // Page 3: Arabic UTF-16LE Artistic Text inheriting `~artistic~` CDSS defaults
    assert_eq!(doc.pages()[2].layers[0].objects.len(), 1);
    let p3_txt = &doc.objects()[&doc.pages()[2].layers[0].objects[0]];
    let NodePayload::ArtisticText(ref at) = p3_txt.payload else {
        panic!("expected ArtisticText on Page 3");
    };
    assert_eq!(at.plain_text(), "شكر وعرفان");
    assert_eq!(at.runs[0].style.direction, TextDirection::Rtl);
    assert_eq!(at.runs[0].style.font_family, "Noto Naskh Arabic");

    // Structural Inspector matches resolved ZCF metadata
    let rep = inspect_cdr_bytes(&bytes).unwrap();
    assert_eq!(rep.version_number, Some(2300));
    assert_eq!(rep.page_count, 3);
    assert_eq!(rep.layer_count, 3);
    assert_eq!(rep.object_count, 6); // 1 group + 2 P1 + 2 P2 + 1 P3
    assert_eq!(rep.color_models, vec!["CMYK"]);
    assert!(rep.warnings.is_empty());
}

#[test]
fn extract_cdr_preview_png_all_containers_and_security() {
    use coree_cdr::extract_cdr_preview_png;

    let limits = CodecLimits::default();

    // 1. ZCF ZIP with `previews/thumbnail.png`
    let zcf_bytes = build_zcf_v23_fixture_bytes();
    let png1 = extract_cdr_preview_png(&zcf_bytes, &limits)
        .unwrap()
        .unwrap();
    assert_eq!(&png1[0..8], b"\x89PNG\r\n\x1a\n");

    // 2. Synthetic ZIP X6 with `preview/thumbnail.bmp` -> decoded & encoded to PNG
    let x6_bytes = sample_cdr_zip_x6_bytes();
    let png2 = extract_cdr_preview_png(&x6_bytes, &limits)
        .unwrap()
        .unwrap();
    assert_eq!(&png2[0..8], b"\x89PNG\r\n\x1a\n");

    // 3. Classic RIFF CDR v12 with `DISP` DIB -> PNG
    let riff_bytes = sample_cdr_riff_v12_bytes();
    let png3 = extract_cdr_preview_png(&riff_bytes, &limits)
        .unwrap()
        .unwrap();
    assert_eq!(&png3[0..8], b"\x89PNG\r\n\x1a\n");

    // 4. Classic RIFF with 4-byte `CF_DIB` (`8u32`) prefix in `DISP` -> PNG
    let raw_dib = coree_cdr::cdr_riff::encode_dib_rgba(2, 2, &[100u8; 16]).unwrap();
    let mut cf_disp = Vec::with_capacity(4 + raw_dib.len());
    cf_disp.extend_from_slice(&8u32.to_le_bytes());
    cf_disp.extend_from_slice(&raw_dib);
    let cf_riff = write_riff(&RiffDocument::new(
        *b"CDRC",
        vec![
            RiffNode::chunk(*b"vrsn", 1200u16.to_le_bytes().to_vec()),
            RiffNode::chunk(*b"DISP", cf_disp),
        ],
    ))
    .unwrap();
    let png4 = extract_cdr_preview_png(&cf_riff, &limits).unwrap().unwrap();
    assert_eq!(&png4[0..8], b"\x89PNG\r\n\x1a\n");

    // 5. Security: PNG inside ZIP with oversized IHDR width > max_bitmap_dim must be rejected!
    let mut bad_png = png1.clone();
    bad_png[16..20].copy_from_slice(&(limits.max_bitmap_dim + 100).to_be_bytes());
    let mut bad_zip = Cursor::new(Vec::new());
    {
        let mut zw = ZipWriter::new(&mut bad_zip);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zw.start_file("previews/thumbnail.png", opts).unwrap();
        zw.write_all(&bad_png).unwrap();
        zw.finish().unwrap();
    }
    let err = extract_cdr_preview_png(&bad_zip.into_inner(), &limits).unwrap_err();
    assert!(matches!(err, CdrError::InvalidBitmapDimensions { .. }));
}

#[test]
fn audit_fixes_group_outline_none_cpt_rifx_and_cmx_rlin_guard() {
    use coree_cdr::cpt::parse_cpt_bytes;

    // 1. Groups in CDR and CMX must carry OutlineStyle::none() (no phantom default stroke)
    let doc = generate_sample_document();
    for bytes in [
        export_cdr_bytes(&doc, CdrWriteVersion::RiffV12).unwrap(),
        export_cmx_bytes(&doc, CmxPrecision::Bit32).unwrap(),
    ] {
        let rep = import_cdr_bytes(&bytes).unwrap();
        for obj in rep.document.objects().values() {
            if matches!(obj.payload, NodePayload::Group(_)) {
                assert!(
                    !obj.outline.is_visible(),
                    "Group should have no default outline"
                );
            }
        }
    }

    // 2. Big-endian `RIFX CPT ` parses cleanly with OutlineStyle::none() on raster layers
    let mut head = Vec::new();
    head.extend_from_slice(&2u32.to_be_bytes()); // w
    head.extend_from_slice(&2u32.to_be_bytes()); // h
    head.extend_from_slice(&2u16.to_be_bytes()); // Rgba32
    head.extend_from_slice(&300.0f32.to_be_bytes());
    head.extend_from_slice(&300.0f32.to_be_bytes());
    head.extend_from_slice(&0u16.to_be_bytes());

    let mut info = Vec::new();
    info.extend_from_slice(&0u16.to_be_bytes()); // name len
    info.extend_from_slice(&0i32.to_be_bytes());
    info.extend_from_slice(&0i32.to_be_bytes());
    info.extend_from_slice(&2u32.to_be_bytes());
    info.extend_from_slice(&2u32.to_be_bytes());
    info.push(255); // opacity
    info.push(0); // blend
    info.push(0x01); // visible
    info.extend_from_slice(&2u16.to_be_bytes()); // Rgba32

    let mut pxls = vec![0u8]; // Raw compression
    pxls.extend_from_slice(&[200u8; 16]);

    let rifx_cpt = write_riff(&RiffDocument {
        endian: RiffEndian::Big,
        form_type: *b"CPT ",
        children: vec![
            RiffNode::chunk(*b"head", head),
            RiffNode::list(
                *b"layr",
                vec![
                    RiffNode::chunk(*b"info", info),
                    RiffNode::chunk(*b"pxls", pxls),
                ],
            ),
        ],
    })
    .unwrap();
    let parsed_cpt = parse_cpt_bytes(&rifx_cpt, &CodecLimits::default()).unwrap();
    assert_eq!(parsed_cpt.document.objects().len(), 1);
    let raster_obj = parsed_cpt.document.objects().values().next().unwrap();
    assert!(!raster_obj.outline.is_visible());

    // 3. CMX `rlin` with bogus dash_count = 500 stops cleanly with warning and zero panics
    let mut bad_rlin = Vec::new();
    bad_rlin.extend_from_slice(&1u16.to_le_bytes()); // count = 1
    bad_rlin.push(0); // cap
    bad_rlin.push(0); // join
    bad_rlin.extend_from_slice(&500u16.to_le_bytes()); // invalid dash_count > 256
    let bad_cmx = write_riff(&RiffDocument::new(
        *b"CMX2",
        vec![RiffNode::chunk(*b"rlin", bad_rlin)],
    ))
    .unwrap();
    let cmx_rep = import_cdr_bytes(&bad_cmx).unwrap();
    assert!(cmx_rep
        .warnings
        .iter()
        .any(|w| w.contains("rlin dash_count")));
}

#[test]
fn regression_7_verifier_s1_s2_trfd_overflow_loda_amp_fild29_and_rdf_unescape() {
    use coree_cdr::zcf::{parse_c9_style_override, parse_textinfo_xml};

    // 7a. [S1] Extreme finite `trfd` (`1.5e308`) + zero-size legacy rect (`0x0`) AND chained
    // overflowing `trfd` transforms + ellipse division must NEVER panic or emit NaN/Inf!
    let huge_trfd = build_zcf_trfd(1.5e308, 1.5e308, 1.5e308, 1.5e308, 1.5e308, 1.5e308);
    let zero_rect_loda = build_zcf_loda(0x01, &[(0x001E, vec![0u8; 8])]);
    let ell_arg = {
        let mut v = Vec::new();
        v.extend_from_slice(&200_000i32.to_le_bytes());
        v.extend_from_slice(&100_000i32.to_le_bytes());
        v
    };
    let huge_ell_loda = build_zcf_loda(0x02, &[(0x001E, ell_arg)]);

    // Chained 2-arg trfd whose matrix product overflows to Inf/NaN
    let mut chained_trfd = vec![0u8; 20 + 8 + 72 * 2];
    let c_len = chained_trfd.len() as u32;
    chained_trfd[0..4].copy_from_slice(&c_len.to_le_bytes());
    chained_trfd[4..8].copy_from_slice(&2u32.to_le_bytes());
    chained_trfd[8..12].copy_from_slice(&16u32.to_le_bytes());
    chained_trfd[16..20].copy_from_slice(&28u32.to_le_bytes());
    chained_trfd[20..24].copy_from_slice(&100u32.to_le_bytes());
    for base in [28usize, 100usize] {
        chained_trfd[base + 8..base + 10].copy_from_slice(&0x0008u16.to_le_bytes());
        for i in 0..6 {
            let p = base + 16 + i * 8;
            chained_trfd[p..p + 8].copy_from_slice(&1.0e200_f64.to_le_bytes());
        }
    }

    let overflow_doc = RiffDocument::new(
        *b"CDRO",
        vec![
            RiffNode::chunk(*b"vrsn", 2300u16.to_le_bytes().to_vec()),
            RiffNode::list(*b"doc ", Vec::new()),
            RiffNode::list(
                *b"page",
                vec![RiffNode::list(
                    *b"gobj",
                    vec![RiffNode::list(
                        *b"layr",
                        vec![
                            RiffNode::list(
                                *b"obj ",
                                vec![RiffNode::list(
                                    *b"lgob",
                                    vec![
                                        RiffNode::chunk(*b"loda", zero_rect_loda),
                                        RiffNode::list(
                                            *b"trfl",
                                            vec![RiffNode::chunk(*b"trfd", huge_trfd)],
                                        ),
                                    ],
                                )],
                            ),
                            RiffNode::list(
                                *b"obj ",
                                vec![RiffNode::list(
                                    *b"lgob",
                                    vec![
                                        RiffNode::chunk(*b"loda", huge_ell_loda),
                                        RiffNode::list(
                                            *b"trfl",
                                            vec![RiffNode::chunk(*b"trfd", chained_trfd)],
                                        ),
                                    ],
                                )],
                            ),
                        ],
                    )],
                )],
            ),
        ],
    );
    let overflow_bytes = write_riff(&overflow_doc).unwrap();
    let rep = import_cdr_bytes(&overflow_bytes).unwrap();
    assert_eq!(rep.document.objects().len(), 2);
    for obj in rep.document.objects().values() {
        assert!(
            obj.transform.is_finite(),
            "transform must stay finite: {:?}",
            obj.transform
        );
        match &obj.payload {
            NodePayload::Shape(ParametricShape::Rectangle {
                width,
                height,
                corner_radii,
                ..
            }) => {
                assert!(width.is_finite() && height.is_finite());
                assert!(corner_radii.iter().all(|r| r.is_finite()));
            }
            NodePayload::Shape(ParametricShape::Ellipse { rx, ry, .. }) => {
                assert!(rx.is_finite() && ry.is_finite());
            }
            _ => {}
        }
    }

    // 7b. [S1] Overlapping/decreasing `loda` arg offsets (both synthetic & authentic ZCF)
    // must be rejected cleanly with zero amplification!
    // Synthetic decreasing offsets: 64 entries pointing back to offset 12
    let mut syn_amp = Vec::new();
    let num_args = 64u32;
    let fake_curve = vec![0u8; 64];
    let args_off = 12 + fake_curve.len() as u32;
    let types_off = args_off + num_args * 4;
    syn_amp.extend_from_slice(&num_args.to_le_bytes());
    syn_amp.extend_from_slice(&args_off.to_le_bytes());
    syn_amp.extend_from_slice(&types_off.to_le_bytes());
    syn_amp.extend_from_slice(&fake_curve);
    for i in 0..num_args {
        // Decreasing offsets [20, 12, 20, 12, ...]
        let off = if i % 2 == 0 { 20u32 } else { 12u32 };
        syn_amp.extend_from_slice(&off.to_le_bytes());
    }
    for _ in 0..num_args {
        syn_amp.extend_from_slice(&0x001Eu32.to_le_bytes());
    }

    // Authentic ZCF alternating offsets `[start, total_len, start, total_len, ...]`
    let n_real = 64usize;
    let r_args_off = 20usize;
    let r_types_off = r_args_off + n_real * 4;
    let r_data_start = r_types_off + n_real * 4;
    let r_payload = vec![0u8; 64];
    let r_total = r_data_start + r_payload.len();
    let mut real_amp = Vec::with_capacity(r_total);
    real_amp.extend_from_slice(&(r_total as u32).to_le_bytes());
    real_amp.extend_from_slice(&(n_real as u32).to_le_bytes());
    real_amp.extend_from_slice(&(r_args_off as u32).to_le_bytes());
    real_amp.extend_from_slice(&(r_types_off as u32).to_le_bytes());
    real_amp.extend_from_slice(&0x03u32.to_le_bytes());
    for i in 0..n_real {
        let off = if i % 2 == 0 { r_data_start } else { r_total };
        real_amp.extend_from_slice(&(off as u32).to_le_bytes());
    }
    for _ in 0..n_real {
        real_amp.extend_from_slice(&0x001Eu32.to_le_bytes());
    }
    real_amp.extend_from_slice(&r_payload);

    let amp_doc = RiffDocument::new(
        *b"CDRC",
        vec![
            RiffNode::chunk(*b"vrsn", 1200u16.to_le_bytes().to_vec()),
            RiffNode::list(
                *b"doc ",
                vec![RiffNode::list(
                    *b"page",
                    vec![RiffNode::list(
                        *b"layr",
                        vec![
                            RiffNode::list(*b"obj ", vec![RiffNode::chunk(*b"loda", syn_amp)]),
                            RiffNode::list(*b"obj ", vec![RiffNode::chunk(*b"loda", real_amp)]),
                        ],
                    )],
                )],
            ),
        ],
    );
    let amp_rep = import_cdr_bytes(&write_riff(&amp_doc).unwrap()).unwrap();
    assert_eq!(amp_rep.document.objects().len(), 0);
    assert!(
        amp_rep.warnings.len() >= 2,
        "expected malformed loda warnings for both overlapping chunks, got {:?}",
        amp_rep.warnings
    );

    // Oversized `0x00C9` JSON payload (> 256 KiB) rejected cleanly before JSON parsing
    let mut huge_c9 = Vec::with_capacity(8 + 300_000);
    huge_c9.extend_from_slice(&1u32.to_le_bytes());
    huge_c9.extend_from_slice(&300_000u32.to_le_bytes());
    huge_c9.push(b'{');
    huge_c9.resize(8 + 300_000 - 1, b' ');
    huge_c9.push(b'}');
    assert!(parse_c9_style_override(&huge_c9).is_none());

    // 7c. [S2] Minimal 29-byte v16 solid `fild` (`&data[12..]` len == 17) AND 31-byte solid `fild`
    // (`&data[12..]` len == 19, color block at tail `14..31`) must both decode `Solid(Color::CYAN)`!
    let mut fild29 = vec![0u8; 29];
    fild29[0..4].copy_from_slice(&7u32.to_le_bytes()); // fill_id = 7
    fild29[4..6].copy_from_slice(&1u16.to_le_bytes()); // fill_type = 1 (Solid, ZCF context)
    fild29[12] = 0x01;
    fild29[13..17].copy_from_slice(&12u32.to_le_bytes());
    fild29[17..19].copy_from_slice(&3u16.to_le_bytes()); // CMYK255
    fild29[19..21].copy_from_slice(&5u16.to_le_bytes());
    fild29[25..29].copy_from_slice(&[255, 0, 0, 0]); // Pure Cyan at exact tail `12..29`!

    let mut fild31 = vec![0u8; 31];
    fild31[0..4].copy_from_slice(&8u32.to_le_bytes()); // fill_id = 8
    fild31[4..8].copy_from_slice(&0x0514u32.to_le_bytes()); // v16+ sub_tag
    fild31[8..12].copy_from_slice(&2u32.to_le_bytes());
    fild31[12..14].copy_from_slice(&1u16.to_le_bytes()); // fill_type = 1 (Solid)
    fild31[14] = 0x01;
    fild31[15..19].copy_from_slice(&12u32.to_le_bytes());
    fild31[19..21].copy_from_slice(&3u16.to_le_bytes()); // CMYK255
    fild31[21..23].copy_from_slice(&5u16.to_le_bytes());
    fild31[27..31].copy_from_slice(&[255, 0, 0, 0]); // Pure Cyan at exact tail `14..31`!

    let rect_loda_7 = build_zcf_loda(
        0x01,
        &[
            (
                0x001E,
                build_zcf_rect128(100_000.0, 100_000.0, 1.0, 1.0, [0.0; 4], 0),
            ),
            (0x0014, 7u32.to_le_bytes().to_vec()),
        ],
    );
    let rect_loda_8 = build_zcf_loda(
        0x01,
        &[
            (
                0x001E,
                build_zcf_rect128(100_000.0, 100_000.0, 1.0, 1.0, [0.0; 4], 0),
            ),
            (0x0014, 8u32.to_le_bytes().to_vec()),
        ],
    );
    let fild_doc = RiffDocument::new(
        *b"CDRO",
        vec![
            RiffNode::chunk(*b"vrsn", 2300u16.to_le_bytes().to_vec()),
            RiffNode::list(
                *b"doc ",
                vec![RiffNode::list(
                    *b"filt",
                    vec![
                        RiffNode::chunk(*b"fild", fild29),
                        RiffNode::chunk(*b"fild", fild31),
                    ],
                )],
            ),
            RiffNode::list(
                *b"page",
                vec![RiffNode::list(
                    *b"gobj",
                    vec![RiffNode::list(
                        *b"layr",
                        vec![
                            RiffNode::list(
                                *b"obj ",
                                vec![RiffNode::list(
                                    *b"lgob",
                                    vec![RiffNode::chunk(*b"loda", rect_loda_7)],
                                )],
                            ),
                            RiffNode::list(
                                *b"obj ",
                                vec![RiffNode::list(
                                    *b"lgob",
                                    vec![RiffNode::chunk(*b"loda", rect_loda_8)],
                                )],
                            ),
                        ],
                    )],
                )],
            ),
        ],
    );
    let fild_rep = import_cdr_bytes(&write_riff(&fild_doc).unwrap()).unwrap();
    assert_eq!(fild_rep.document.objects().len(), 2);
    for rect_obj in fild_rep.document.objects().values() {
        assert_eq!(rect_obj.fill, Fill::Solid(Color::CYAN));
    }

    // 7d. [S2] Single-pass XML unescaping in `extract_rdf_li_list` (no double unescape or tag truncation!)
    let tricky_xml = r#"<x:xmpmeta><rdf:RDF><rdf:Description>
        <LayerNames><rdf:Bag>
            <rdf:li>Cut &lt;/rdf:li&gt; Layer</rdf:li>
            <rdf:li>A &amp;lt; B</rdf:li>
        </rdf:Bag></LayerNames>
    </rdf:Description></rdf:RDF></x:xmpmeta>"#;
    let ti = parse_textinfo_xml(tricky_xml);
    assert_eq!(ti.layer_names, vec!["Cut </rdf:li> Layer", "A &lt; B"]);
}

#[test]
fn zcf_x6_writer_indirects_all_chunks_including_16b_xobj_and_derefs_mcfg_layr() {
    use coree_cdr::cdr_zip::{parse_cdr_zip, write_cdr_zip};
    use coree_cdr::riff::parse_riff_with_budget;
    use coree_core::{UnknownCdrChunk, Vec2D};

    // (a) 16-byte unknown chunk (`XOBJ` = b"COREE_T4_XOBJ_v1") round-trips through write_cdr_zip(X6)
    let mut doc = Document::new_a4("ZCF All-Chunk Indirection");
    doc.set_title("Tier1ZcfTitle");
    let lid = doc.pages()[0].layers[0].id;
    let mut obj = SceneObject::new(
        ObjectId(0),
        "WithXobj",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 30.0, 20.0,
        ))),
    );
    obj.unknown_chunks.push(UnknownCdrChunk {
        fourcc: *b"XOBJ",
        version_hint: 2400,
        raw_bytes: b"COREE_T4_XOBJ_v1".to_vec(),
    });
    doc.add_object(obj, Some(lid)).unwrap();

    let zip_bytes = write_cdr_zip(&doc, CdrZipEra::X6, false).unwrap();

    // Inspect content/root.dat directly: every leaf chunk must be a valid 16B ZCF pointer
    let mut archive = ZipArchive::new(Cursor::new(&zip_bytes)).unwrap();
    let mut root_buf = Vec::new();
    std::io::Read::read_to_end(
        &mut archive.by_name("content/root.dat").unwrap(),
        &mut root_buf,
    )
    .unwrap();
    let mut data0_buf = Vec::new();
    std::io::Read::read_to_end(
        &mut archive.by_name("content/data/data0.dat").unwrap(),
        &mut data0_buf,
    )
    .unwrap();
    let mut budget = CodecLimits::default().max_uncompressed_bytes;
    let raw_tree =
        parse_riff_with_budget(&root_buf, &CodecLimits::default(), &mut budget, false).unwrap();

    fn verify_all_16b_zcf_pointers(nodes: &[RiffNode], data0_len: usize) {
        for node in nodes {
            match node {
                RiffNode::List { children, .. } => verify_all_16b_zcf_pointers(children, data0_len),
                RiffNode::Chunk { fourcc, data, .. } => {
                    assert_eq!(
                        data.len(),
                        16,
                        "chunk '{}' in CDRO root.dat must be a 16B ZCF pointer, got {}B",
                        String::from_utf8_lossy(fourcc),
                        data.len()
                    );
                    let fidx = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                    let dlen = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
                    let doff = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
                    if fidx == u32::MAX {
                        assert!(fourcc != b"loda" && dlen <= 8);
                    } else {
                        assert_eq!(fidx, 0);
                        assert!(dlen > 0 && doff + dlen <= data0_len);
                    }
                }
            }
        }
    }
    verify_all_16b_zcf_pointers(&raw_tree.children, data0_buf.len());

    let parsed = parse_cdr_zip(&zip_bytes, &CodecLimits::default()).unwrap();
    assert_eq!(parsed.document.title(), "Tier1ZcfTitle");
    let p_oid = parsed.document.pages()[0].layers[0].objects[0];
    let p_obj = parsed.document.object(p_oid).unwrap();
    assert_eq!(p_obj.unknown_chunks.len(), 1);
    assert_eq!(&p_obj.unknown_chunks[0].fourcc, b"XOBJ");
    assert_eq!(p_obj.unknown_chunks[0].raw_bytes, b"COREE_T4_XOBJ_v1");

    // (b) External 16-byte ZCF pointers on `mcfg` and `layr` dereference cleanly
    let mut doc_mcfg = Vec::new();
    doc_mcfg.extend_from_slice(&1_500_000i32.to_le_bytes()); // 150mm
    doc_mcfg.extend_from_slice(&2_000_000i32.to_le_bytes()); // 200mm
    doc_mcfg.push(0); // CMYK
    doc_mcfg.extend_from_slice(&300.0f32.to_le_bytes());
    doc_mcfg.extend_from_slice(&5u16.to_le_bytes());
    doc_mcfg.extend_from_slice(b"Tier1");

    let mut page_mcfg = Vec::new();
    page_mcfg.extend_from_slice(&1_500_000i32.to_le_bytes());
    page_mcfg.extend_from_slice(&2_000_000i32.to_le_bytes());
    page_mcfg.extend_from_slice(&30_000i32.to_le_bytes());
    page_mcfg.push(0);
    page_mcfg.extend_from_slice(&6u16.to_le_bytes());
    page_mcfg.extend_from_slice(b"Page 1");

    let mut layr_hdr = Vec::new();
    layr_hdr.push(0x05);
    layr_hdr.push(0);
    layr_hdr.extend_from_slice(&7u16.to_le_bytes());
    layr_hdr.extend_from_slice(b"Layer 1");

    let mut data_blob = Vec::new();
    let doc_mcfg_ptr = zcf_ext_ptr(0, &mut data_blob, &doc_mcfg);
    let page_mcfg_ptr = zcf_ext_ptr(0, &mut data_blob, &page_mcfg);
    let layr_ptr = zcf_ext_ptr(0, &mut data_blob, &layr_hdr);

    let root_zcf = RiffDocument::new(
        *b"CDRO",
        vec![
            RiffNode::chunk(*b"vrsn", zcf_inline_ptr(&1600u16.to_le_bytes())),
            RiffNode::list(
                *b"doc ",
                vec![
                    RiffNode::chunk(*b"mcfg", doc_mcfg_ptr),
                    RiffNode::list(
                        *b"page",
                        vec![
                            RiffNode::chunk(*b"mcfg", page_mcfg_ptr),
                            RiffNode::list(*b"layr", vec![RiffNode::chunk(*b"layr", layr_ptr)]),
                        ],
                    ),
                ],
            ),
        ],
    );
    let root_zcf_bytes = write_riff(&root_zcf).unwrap();

    let mut zcur = Cursor::new(Vec::new());
    {
        let mut zw = ZipWriter::new(&mut zcur);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zw.start_file("mimetype", stored).unwrap();
        zw.write_all(b"application/x-vnd.corel.zcf.draw.document+zip")
            .unwrap();
        zw.start_file("content/root.dat", deflated).unwrap();
        zw.write_all(&root_zcf_bytes).unwrap();
        zw.start_file("content/data/0000.dat", deflated).unwrap();
        zw.write_all(&data_blob).unwrap();
        zw.start_file("content/dataFileList.dat", deflated).unwrap();
        zw.write_all(b"data/0000.dat\n").unwrap();
        zw.finish().unwrap();
    }
    let zcf_rep = import_cdr_bytes(&zcur.into_inner()).unwrap();
    assert_eq!(zcf_rep.document.title(), "Tier1");
    let pg = &zcf_rep.document.pages()[0];
    assert!((pg.width_mm - 150.0).abs() < 0.05);
    assert!((pg.height_mm - 200.0).abs() < 0.05);
    assert_eq!(pg.layers[0].name, "Layer 1");
    let _ = Vec2D::ZERO;
}

#[test]
fn group_loda_unknown_args_and_cdr_shifted_blend_linked_frame_round_trip() {
    use coree_core::{Effect, ParagraphText, TextStyle, UnknownCdrChunk, Vec2D};

    let mut doc = Document::new_a4("Group LODA & Shifted Refs");
    let lid = doc.pages()[0].layers[0].id;

    // Delete an initial object so remaining ObjectIds start > 1 and shift on re-import
    let dummy = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "DummyToDelete",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 4.0, 4.0))),
            ),
            Some(lid),
        )
        .unwrap();
    let _ = doc.remove_object(dummy);

    let c1 = doc
        .insert_loaded_object(SceneObject::new(
            ObjectId(0),
            "GrpChild",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                5.0, 5.0, 10.0, 10.0,
            ))),
        ))
        .unwrap();
    let mut grp = SceneObject::new(ObjectId(0), "GrpWithLodaUnk", NodePayload::Group(vec![c1]));
    let mut raw_loda_unk = Vec::new();
    raw_loda_unk.extend_from_slice(&0x00FFu32.to_le_bytes());
    raw_loda_unk.extend_from_slice(b"GRP_UNK_LODA");
    grp.unknown_chunks.push(UnknownCdrChunk {
        fourcc: *b"LODA",
        version_hint: 1200,
        raw_bytes: raw_loda_unk.clone(),
    });
    doc.add_object(grp, Some(lid)).unwrap();

    let spine_oid = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "BlendSpine",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    15.0, 15.0, 30.0, 10.0,
                ))),
            ),
            Some(lid),
        )
        .unwrap();
    let target_oid = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "BlendTarget",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    50.0, 15.0, 15.0, 15.0,
                ))),
            ),
            Some(lid),
        )
        .unwrap();
    let mut blend_src = SceneObject::new(
        ObjectId(0),
        "BlendSource",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            15.0, 35.0, 12.0, 12.0,
        ))),
    );
    blend_src.effects.push(Effect::Blend {
        target_id: target_oid,
        steps: 6,
        rotation_deg: 0.0,
        along_path: Some(spine_oid),
        rotate_along_path: true,
    });
    doc.add_object(blend_src, Some(lid)).unwrap();

    let next_para_oid = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Para2",
                NodePayload::ParagraphText(ParagraphText::new(
                    "Frame two",
                    Vec2D::new(40.0, 20.0),
                    TextStyle::default(),
                )),
            ),
            Some(lid),
        )
        .unwrap();
    let mut p1 = ParagraphText::new("Frame one", Vec2D::new(40.0, 20.0), TextStyle::default());
    p1.linked_next_frame = Some(next_para_oid);
    doc.add_object(
        SceneObject::new(ObjectId(0), "Para1", NodePayload::ParagraphText(p1)),
        Some(lid),
    )
    .unwrap();

    let cdr_bytes = export_cdr_bytes(&doc, CdrWriteVersion::RiffV12).unwrap();
    let rep = import_cdr_bytes(&cdr_bytes).unwrap();
    let objs = &rep.document.pages()[0].layers[0].objects;
    assert_eq!(objs.len(), 6);

    let re_grp = rep.document.object(objs[0]).unwrap();
    assert!(
        re_grp
            .unknown_chunks
            .iter()
            .any(|u| &u.fourcc == b"LODA" && u.raw_bytes == raw_loda_unk),
        "Group LODA 0x00FF unknown arg must round-trip, got {:?}",
        re_grp.unknown_chunks
    );

    let new_spine = objs[1];
    let new_target = objs[2];
    let new_blend_src = objs[3];
    let new_para2 = objs[4];
    let new_para1 = objs[5];

    assert_ne!(new_target, target_oid, "IDs must shift on re-import");
    match rep.document.object(new_blend_src).unwrap().effects.first() {
        Some(Effect::Blend {
            target_id,
            along_path,
            ..
        }) => {
            assert_eq!(*target_id, new_target);
            assert_eq!(*along_path, Some(new_spine));
        }
        other => panic!("expected Blend effect, got {other:?}"),
    }
    match &rep.document.object(new_para1).unwrap().payload {
        NodePayload::ParagraphText(para) => {
            assert_eq!(para.linked_next_frame, Some(new_para2));
        }
        other => panic!("expected ParagraphText, got {other:?}"),
    }
}

#[test]
fn cmx_interns_and_remaps_assets_across_powerclip_bitmap_pattern_table_and_symbols() {
    use coree_core::{RasterAsset, RasterLayerData, SymbolDef, VectorTable};

    let mut doc = Document::new_a4("CMX All ImageAssetId Contexts");
    let lid = doc.pages()[0].layers[0].id;

    // 5 distinct assets — each used ONLY in its respective context
    let px_pc_raster = [
        201, 10, 20, 255, 11, 22, 33, 255, 44, 55, 66, 255, 77, 88, 99, 255,
    ];
    let px_pc_pat = [
        10, 210, 30, 255, 12, 24, 36, 255, 48, 60, 72, 255, 84, 96, 108, 255,
    ];
    let px_top_pat = [
        20, 30, 220, 255, 15, 25, 35, 255, 45, 55, 65, 255, 75, 85, 95, 255,
    ];
    let px_tbl_pat = [
        230, 200, 10, 255, 18, 28, 38, 255, 48, 58, 68, 255, 78, 88, 98, 255,
    ];
    let px_sym_raster = [
        180, 20, 240, 255, 21, 31, 41, 255, 51, 61, 71, 255, 81, 91, 101, 255,
    ];

    let aid_pc_raster = doc.register_asset(
        RasterAsset::new(2, 2, px_pc_raster.to_vec(), 300.0, ColorMode::Rgb).unwrap(),
    );
    let aid_pc_pat = doc
        .register_asset(RasterAsset::new(2, 2, px_pc_pat.to_vec(), 150.0, ColorMode::Rgb).unwrap());
    let aid_top_pat = doc.register_asset(
        RasterAsset::new(2, 2, px_top_pat.to_vec(), 200.0, ColorMode::Rgb).unwrap(),
    );
    let aid_tbl_pat = doc.register_asset(
        RasterAsset::new(2, 2, px_tbl_pat.to_vec(), 250.0, ColorMode::Rgb).unwrap(),
    );
    let aid_sym_raster = doc.register_asset(
        RasterAsset::new(2, 2, px_sym_raster.to_vec(), 300.0, ColorMode::Rgb).unwrap(),
    );

    // (a) PowerClip containing NodePayload::Raster (asset not used outside)
    // (b) PowerClip containing Fill::BitmapPattern (asset not used outside)
    let pc_raster_child = doc
        .insert_loaded_object(SceneObject::new(
            ObjectId(0),
            "PcOnlyRaster",
            NodePayload::Raster(RasterLayerData {
                asset_id: aid_pc_raster,
                width_mm: 18.0,
                height_mm: 12.0,
                filters: Vec::new(),
                crop_rect: None,
            }),
        ))
        .unwrap();
    let pc_pat_child = doc
        .insert_loaded_object(
            SceneObject::new(
                ObjectId(0),
                "PcOnlyBitmapPat",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    12.0, 12.0, 14.0, 14.0,
                ))),
            )
            .with_fill(Fill::BitmapPattern {
                image_id: aid_pc_pat,
                tile_w: 6.0,
                tile_h: 6.0,
            }),
        )
        .unwrap();
    let pc = SceneObject::new(
        ObjectId(0),
        "PowerClipWithAssets",
        NodePayload::PowerClip {
            container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                10.0, 10.0, 40.0, 40.0,
            ))),
            container_parametric: None,
            lock_contents: true,
            contents: vec![pc_raster_child, pc_pat_child],
        },
    );
    doc.add_object(pc, Some(lid)).unwrap();

    // (c) Top-level Path with Fill::BitmapPattern
    let top_pat = SceneObject::new(
        ObjectId(0),
        "TopBitmapPat",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            60.0, 10.0, 25.0, 25.0,
        ))),
    )
    .with_fill(Fill::BitmapPattern {
        image_id: aid_top_pat,
        tile_w: 5.0,
        tile_h: 5.0,
    });
    doc.add_object(top_pat, Some(lid)).unwrap();

    // (d) VectorTable cell with Fill::BitmapPattern
    let mut tbl = VectorTable::new(2, 2, 30.0, 20.0);
    tbl.cells[1].fill = Fill::BitmapPattern {
        image_id: aid_tbl_pat,
        tile_w: 4.0,
        tile_h: 4.0,
    };
    doc.add_object(
        SceneObject::new(ObjectId(0), "TableBitmapPat", NodePayload::Table(tbl)),
        Some(lid),
    )
    .unwrap();

    // (e) SymbolDef containing NodePayload::Raster
    let sym = SymbolDef::new(
        "RasterSymbol",
        vec![SceneObject::new(
            ObjectId(1),
            "SymRaster",
            NodePayload::Raster(RasterLayerData {
                asset_id: aid_sym_raster,
                width_mm: 10.0,
                height_mm: 10.0,
                filters: Vec::new(),
                crop_rect: None,
            }),
        )],
    );
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    doc.add_object(
        SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id)),
        Some(lid),
    )
    .unwrap();

    for prec in [CmxPrecision::Bit32, CmxPrecision::Bit16] {
        let bytes = export_cmx_bytes(&doc, prec).unwrap();
        let rt = coree_cdr::cmx::parse_cmx_bytes(&bytes, &CodecLimits::default()).unwrap();
        assert_eq!(
            rt.document.assets().len(),
            5,
            "all 5 CMX assets must be emitted in rimg"
        );

        let rt_pc = find_obj_by_name(&rt.document, "PowerClipWithAssets");
        let NodePayload::PowerClip { contents, .. } = &rt_pc.payload else {
            panic!("expected PowerClipWithAssets");
        };
        assert_eq!(contents.len(), 2);

        let rt_pc_r = rt.document.object(contents[0]).unwrap();
        let NodePayload::Raster(r_pc) = &rt_pc_r.payload else {
            panic!("expected Raster inside PowerClip");
        };
        let a_pc_r = rt
            .document
            .asset(r_pc.asset_id)
            .expect("PowerClip Raster asset must resolve");
        assert_eq!((a_pc_r.width, a_pc_r.height), (2, 2));
        assert_eq!(a_pc_r.to_rgba8_vec(), px_pc_raster);

        let rt_pc_p = rt.document.object(contents[1]).unwrap();
        let Fill::BitmapPattern {
            image_id: pc_pat_id,
            ..
        } = rt_pc_p.fill
        else {
            panic!("expected BitmapPattern inside PowerClip");
        };
        let a_pc_p = rt
            .document
            .asset(pc_pat_id)
            .expect("PowerClip BitmapPattern asset must resolve");
        assert_eq!((a_pc_p.width, a_pc_p.height), (2, 2));
        assert_eq!(a_pc_p.to_rgba8_vec(), px_pc_pat);

        let rt_top = find_obj_by_name(&rt.document, "TopBitmapPat");
        let Fill::BitmapPattern {
            image_id: top_pat_id,
            ..
        } = rt_top.fill
        else {
            panic!("expected TopBitmapPat Fill::BitmapPattern");
        };
        let a_top = rt
            .document
            .asset(top_pat_id)
            .expect("top-level BitmapPattern asset must resolve");
        assert_eq!((a_top.width, a_top.height), (2, 2));
        assert_eq!(a_top.to_rgba8_vec(), px_top_pat);

        let rt_tbl = find_obj_by_name(&rt.document, "TableBitmapPat");
        let NodePayload::Table(tbl_rt) = &rt_tbl.payload else {
            panic!("expected TableBitmapPat");
        };
        let Fill::BitmapPattern {
            image_id: tbl_pat_id,
            ..
        } = tbl_rt.cells[1].fill
        else {
            panic!("expected Table cell BitmapPattern");
        };
        let a_tbl = rt
            .document
            .asset(tbl_pat_id)
            .expect("Table cell BitmapPattern asset must resolve");
        assert_eq!((a_tbl.width, a_tbl.height), (2, 2));
        assert_eq!(a_tbl.to_rgba8_vec(), px_tbl_pat);

        let rt_sym = rt
            .document
            .symbol(sym_id)
            .expect("SymbolDef must survive CMX round-trip");
        let NodePayload::Raster(r_sym) = &rt_sym.objects[0].payload else {
            panic!("expected Raster inside SymbolDef");
        };
        let a_sym = rt
            .document
            .asset(r_sym.asset_id)
            .expect("SymbolDef Raster asset must resolve");
        assert_eq!((a_sym.width, a_sym.height), (2, 2));
        assert_eq!(a_sym.to_rgba8_vec(), px_sym_raster);
    }
}

#[test]
fn cdr_riff_and_zip_remap_symbol_raster_and_table_cell_bitmap_pattern() {
    use coree_core::{RasterAsset, RasterLayerData, SymbolDef, VectorTable};

    let mut doc = Document::new_a4("CDR Symbol & Table Asset Remap");
    let lid = doc.pages()[0].layers[0].id;

    let px_sym = [
        120, 40, 210, 255, 10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255,
    ];
    let px_tbl = [
        30, 190, 140, 255, 15, 25, 35, 255, 45, 55, 65, 255, 75, 85, 95, 255,
    ];

    let aid_sym =
        doc.register_asset(RasterAsset::new(2, 2, px_sym.to_vec(), 300.0, ColorMode::Rgb).unwrap());
    let aid_tbl =
        doc.register_asset(RasterAsset::new(2, 2, px_tbl.to_vec(), 240.0, ColorMode::Rgb).unwrap());

    let sym = SymbolDef::new(
        "LogoSym",
        vec![SceneObject::new(
            ObjectId(1),
            "SymBmp",
            NodePayload::Raster(RasterLayerData {
                asset_id: aid_sym,
                width_mm: 16.0,
                height_mm: 16.0,
                filters: Vec::new(),
                crop_rect: None,
            }),
        )],
    );
    let sym_id = sym.id;
    doc.insert_symbol(sym);
    doc.add_object(
        SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id)),
        Some(lid),
    )
    .unwrap();

    let mut tbl = VectorTable::new(2, 2, 30.0, 20.0);
    tbl.cells[2].fill = Fill::BitmapPattern {
        image_id: aid_tbl,
        tile_w: 5.0,
        tile_h: 5.0,
    };
    doc.add_object(
        SceneObject::new(ObjectId(0), "TblWithBmp", NodePayload::Table(tbl)),
        Some(lid),
    )
    .unwrap();

    for ver in [
        CdrWriteVersion::RiffV12,
        CdrWriteVersion::ZipX4,
        CdrWriteVersion::ZipX6,
    ] {
        let bytes = export_cdr_bytes(&doc, ver).unwrap();
        let rt = import_cdr_bytes(&bytes).unwrap();
        assert_eq!(
            rt.document.assets().len(),
            2,
            "both assets must survive {ver:?}"
        );

        let rt_sym = rt.document.symbol(sym_id).expect("SymbolDef must survive");
        let NodePayload::Raster(r_sym) = &rt_sym.objects[0].payload else {
            panic!("expected Raster inside SymbolDef");
        };
        let a_sym = rt
            .document
            .asset(r_sym.asset_id)
            .expect("SymbolDef Raster ImageAssetId must resolve");
        assert_eq!((a_sym.width, a_sym.height), (2, 2));
        assert_eq!(a_sym.to_rgba8_vec(), px_sym);

        let rt_tbl = find_obj_by_name(&rt.document, "TblWithBmp");
        let NodePayload::Table(t_rt) = &rt_tbl.payload else {
            panic!("expected Table");
        };
        let Fill::BitmapPattern {
            image_id: tbl_id, ..
        } = t_rt.cells[2].fill
        else {
            panic!("expected Table cell BitmapPattern");
        };
        let a_tbl = rt
            .document
            .asset(tbl_id)
            .expect("Table cell BitmapPattern ImageAssetId must resolve");
        assert_eq!((a_tbl.width, a_tbl.height), (2, 2));
        assert_eq!(a_tbl.to_rgba8_vec(), px_tbl);
    }
}

#[test]
fn cdr_zip_x6_raster_and_colliding_unknown_0001_dat_both_survive_without_duplicate_zip_error() {
    use coree_core::{RasterAsset, RasterLayerData};

    let mut doc = Document::new_a4("X6 Filename Collision Guard");
    let lid = doc.pages()[0].layers[0].id;
    let px = [
        255, 80, 40, 255, 10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255,
    ];
    let aid =
        doc.register_asset(RasterAsset::new(2, 2, px.to_vec(), 300.0, ColorMode::Rgb).unwrap());
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "BmpObj",
            NodePayload::Raster(RasterLayerData {
                asset_id: aid,
                width_mm: 20.0,
                height_mm: 20.0,
                filters: Vec::new(),
                crop_rect: None,
            }),
        ),
        Some(lid),
    )
    .unwrap();

    let unknown_0001 = b"OPAQUE_X6_UNKNOWN_0001_PAYLOAD".to_vec();
    let unknown_bmp_0001 = b"OPAQUE_X6_UNKNOWN_BMP_0001_PAYLOAD".to_vec();
    doc.insert_unknown_data_file("content/data/0001.dat", unknown_0001.clone());
    doc.insert_unknown_data_file("content/data/bmp_0001.dat", unknown_bmp_0001.clone());

    let bytes = export_cdr_bytes(&doc, CdrWriteVersion::ZipX6)
        .expect("ZipX6 export must avoid Duplicate filename collision");
    let rt = import_cdr_bytes(&bytes).unwrap();
    assert_eq!(
        rt.document
            .unknown_data_files()
            .get("content/data/0001.dat"),
        Some(&unknown_0001)
    );
    assert_eq!(
        rt.document
            .unknown_data_files()
            .get("content/data/bmp_0001.dat"),
        Some(&unknown_bmp_0001)
    );

    let bmp_obj = find_obj_by_name(&rt.document, "BmpObj");
    let NodePayload::Raster(r) = &bmp_obj.payload else {
        panic!("expected Raster");
    };
    let asset = rt
        .document
        .asset(r.asset_id)
        .expect("Raster asset must survive collision avoidance");
    assert_eq!((asset.width, asset.height), (2, 2));
    assert_eq!(asset.to_rgba8_vec(), px);
}

#[test]
fn zcf_cdro_partial_0x00c9_without_fill_inherits_0x00c8_cdss_fill() {
    let mut curve_arg = Vec::new();
    curve_arg.extend_from_slice(&4u32.to_le_bytes());
    for (x, y) in [(0i32, 0i32), (100_000, 0), (100_000, 100_000), (0, 100_000)] {
        curve_arg.extend_from_slice(&x.to_le_bytes());
        curve_arg.extend_from_slice(&y.to_le_bytes());
    }
    curve_arg.extend_from_slice(&[0x0C, 0x08, 0x08, 0x48]);

    let loda_inherit = build_zcf_loda(
        0x03,
        &[
            (0x00C8, 100u32.to_le_bytes().to_vec()),
            (
                0x00C9,
                build_zcf_c9(r#"{"outline":{"width":"20000","color":"RGB255,USER,0,0,255,100"}}"#),
            ),
            (0x001E, curve_arg.clone()),
        ],
    );
    let loda_explicit_none = build_zcf_loda(
        0x03,
        &[
            (0x00C8, 100u32.to_le_bytes().to_vec()),
            (0x00C9, build_zcf_c9(r#"{"fill":{"type":"0"}}"#)),
            (0x001E, curve_arg),
        ],
    );

    let mut data0 = Vec::new();
    let ptr_inherit = zcf_ext_ptr(0, &mut data0, &loda_inherit);
    let ptr_explicit_none = zcf_ext_ptr(0, &mut data0, &loda_explicit_none);

    let root = RiffDocument::new(
        *b"CDRO",
        vec![
            RiffNode::chunk(*b"vrsn", zcf_inline_ptr(&2400u16.to_le_bytes())),
            RiffNode::list(*b"doc ", vec![]),
            RiffNode::list(
                *b"page",
                vec![RiffNode::list(
                    *b"layr",
                    vec![
                        RiffNode::list(*b"obj ", vec![RiffNode::chunk(*b"loda", ptr_inherit)]),
                        RiffNode::list(
                            *b"obj ",
                            vec![RiffNode::chunk(*b"loda", ptr_explicit_none)],
                        ),
                    ],
                )],
            ),
        ],
    );
    let root_bytes = write_riff(&root).unwrap();

    let cdss = r#"{
        "version": 24,
        "style": {
            "100": {
                "fill": {"type": "1", "primaryColor": "RGB255,USER,220,40,40,100"}
            }
        }
    }"#;

    let mut zcur = Cursor::new(Vec::new());
    {
        let mut zw = ZipWriter::new(&mut zcur);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zw.start_file("mimetype", stored).unwrap();
        zw.write_all(b"application/x-vnd.corel.zcf.draw.document+zip")
            .unwrap();
        zw.start_file("content/root.dat", deflated).unwrap();
        zw.write_all(&root_bytes).unwrap();
        zw.start_file("content/data/data0.dat", deflated).unwrap();
        zw.write_all(&data0).unwrap();
        zw.start_file("content/dataFileList.dat", deflated).unwrap();
        zw.write_all(b"data/data0.dat\n").unwrap();
        zw.start_file("styles/document.cdss", deflated).unwrap();
        zw.write_all(cdss.as_bytes()).unwrap();
        zw.finish().unwrap();
    }

    let rep = import_cdr_bytes(&zcur.into_inner()).unwrap();
    let objs = &rep.document.pages()[0].layers[0].objects;
    assert_eq!(objs.len(), 2);

    let o_inherit = rep.document.object(objs[0]).unwrap();
    match &o_inherit.fill {
        Fill::Solid(Color::Rgba { r, g, b, .. }) => {
            assert!((*r - 220.0 / 255.0).abs() < 0.01);
            assert!((*g - 40.0 / 255.0).abs() < 0.01);
            assert!((*b - 40.0 / 255.0).abs() < 0.01);
        }
        other => panic!("expected inherited red Solid fill from 0x00C8 cdss, got {other:?}"),
    }
    assert!((o_inherit.outline.width_mm - 2.0).abs() < 0.01);

    let o_none = rep.document.object(objs[1]).unwrap();
    assert!(
        matches!(o_none.fill, Fill::None),
        "explicit 0x00C9 fill type=0 must override 0x00C8 cdss, got {:?}",
        o_none.fill
    );
}

#[test]
fn cmx_roundtrip_preserves_group_transform_and_object_flags_opacity_blend() {
    use coree_core::{
        Affine2D, ArtisticText, ParagraphText, RasterAsset, RasterLayerData, TextStyle, Vec2D,
    };

    let mut doc = Document::new_a4("CMX Transform & Attrs Roundtrip");
    let lid = doc.pages()[0].layers[0].id;

    let px = [
        200, 50, 75, 255, 20, 40, 60, 255, 80, 100, 120, 255, 140, 160, 180, 255,
    ];
    let aid =
        doc.register_asset(RasterAsset::new(2, 2, px.to_vec(), 300.0, ColorMode::Rgb).unwrap());

    let grp_xf = Affine2D::translate(15.0, 25.0).compose(&Affine2D::rotate(0.5));
    let raster_xf = Affine2D::translate(20.0, 30.0).compose(&Affine2D::rotate(-0.25));
    let art_xf = Affine2D::translate(40.0, 60.0).compose(&Affine2D::rotate(0.3));
    let para_xf = Affine2D::translate(50.0, 90.0).compose(&Affine2D::rotate(-0.4));

    // 1. Transformed Group with default flags/opacity/blend (pure CMD_BEGIN_GROUP Y-up transform)
    let child_plain_id = doc
        .insert_loaded_object(
            SceneObject::new(
                ObjectId(0),
                "GroupPlainChild",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    5.0, 5.0, 20.0, 20.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::rgba(0.2, 0.6, 0.8, 1.0))),
        )
        .unwrap();
    let mut grp_plain = SceneObject::new(
        ObjectId(0),
        "GroupPlainTransformed",
        NodePayload::Group(vec![child_plain_id]),
    );
    grp_plain.transform = grp_xf;
    doc.add_object(grp_plain, Some(lid)).unwrap();

    // 2. Transformed Group with hidden/locked/non-printable/opacity/Multiply
    let child_attr_id = doc
        .insert_loaded_object(
            SceneObject::new(
                ObjectId(0),
                "GroupAttrChild",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 10.0, 30.0, 15.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::rgba(0.9, 0.2, 0.1, 1.0))),
        )
        .unwrap();
    let mut grp_attr = SceneObject::new(
        ObjectId(0),
        "GroupWithAttrs",
        NodePayload::Group(vec![child_attr_id]),
    );
    grp_attr.transform = grp_xf;
    grp_attr.visible = false;
    grp_attr.locked = true;
    grp_attr.printable = false;
    grp_attr.opacity = 0.4;
    grp_attr.blend_mode = BlendMode::Multiply;
    doc.add_object(grp_attr, Some(lid)).unwrap();

    // 3. Raster (DrawImage) with transform + flags/opacity/Screen
    let mut img = SceneObject::new(
        ObjectId(0),
        "RasterWithAttrs",
        NodePayload::Raster(RasterLayerData {
            asset_id: aid,
            width_mm: 24.0,
            height_mm: 18.0,
            filters: Vec::new(),
            crop_rect: None,
        }),
    );
    img.transform = raster_xf;
    img.visible = false;
    img.locked = true;
    img.printable = false;
    img.opacity = 0.5;
    img.blend_mode = BlendMode::Screen;
    doc.add_object(img, Some(lid)).unwrap();

    // 4. ArtisticText with transform + flags/opacity/Overlay
    let mut art = SceneObject::new(
        ObjectId(0),
        "ArtisticWithAttrs",
        NodePayload::ArtisticText(ArtisticText::from_plain("CMX Art", TextStyle::default())),
    );
    art.transform = art_xf;
    art.visible = false;
    art.locked = true;
    art.printable = false;
    art.opacity = 0.65;
    art.blend_mode = BlendMode::Overlay;
    doc.add_object(art, Some(lid)).unwrap();

    // 5. ParagraphText with transform + flags/opacity/Darken
    let mut para = SceneObject::new(
        ObjectId(0),
        "ParagraphWithAttrs",
        NodePayload::ParagraphText(ParagraphText::new(
            "CMX Para",
            Vec2D::new(60.0, 35.0),
            TextStyle::default(),
        )),
    );
    para.transform = para_xf;
    para.visible = false;
    para.locked = true;
    para.printable = false;
    para.opacity = 0.75;
    para.blend_mode = BlendMode::Darken;
    doc.add_object(para, Some(lid)).unwrap();

    // 6. Path (CMD_POLY_CURVE) with flags/opacity/Difference
    let mut path_obj = SceneObject::new(
        ObjectId(0),
        "PathWithAttrs",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            12.0, 14.0, 22.0, 16.0,
        ))),
    );
    path_obj.visible = false;
    path_obj.locked = true;
    path_obj.printable = false;
    path_obj.opacity = 0.35;
    path_obj.blend_mode = BlendMode::Difference;
    doc.add_object(path_obj, Some(lid)).unwrap();

    let xf_err = |a: &Affine2D, b: &Affine2D| -> f64 {
        a.m.iter()
            .zip(b.m.iter())
            .map(|(x, y)| (x - y).abs())
            .fold(0.0_f64, f64::max)
    };

    for (prec, endian) in [
        (CmxPrecision::Bit32, RiffEndian::Little),
        (CmxPrecision::Bit32, RiffEndian::Big),
        (CmxPrecision::Bit16, RiffEndian::Little),
    ] {
        let bytes = export_cmx_bytes_with_endian(&doc, prec, endian).unwrap();
        let rt = import_cdr_bytes(&bytes).unwrap();

        let rt_grp_plain = find_obj_by_name(&rt.document, "GroupPlainTransformed");
        let err_plain = xf_err(&rt_grp_plain.transform, &grp_xf);
        assert!(
            err_plain < 1e-3,
            "{prec:?}/{endian:?} GroupPlainTransformed xf err={err_plain}"
        );
        let NodePayload::Group(plain_children) = &rt_grp_plain.payload else {
            panic!("expected GroupPlainTransformed to be Group");
        };
        assert_eq!(plain_children.len(), 1);
        assert_eq!(
            rt.document.object(plain_children[0]).unwrap().name,
            "GroupPlainChild"
        );

        let rt_grp_attr = find_obj_by_name(&rt.document, "GroupWithAttrs");
        let err_attr = xf_err(&rt_grp_attr.transform, &grp_xf);
        assert!(
            err_attr < 1e-3,
            "{prec:?}/{endian:?} GroupWithAttrs xf err={err_attr}"
        );
        assert!(!rt_grp_attr.visible);
        assert!(rt_grp_attr.locked);
        assert!(!rt_grp_attr.printable);
        assert!((rt_grp_attr.opacity - 0.4).abs() < 1e-4);
        assert_eq!(rt_grp_attr.blend_mode, BlendMode::Multiply);
        let NodePayload::Group(attr_children) = &rt_grp_attr.payload else {
            panic!("expected GroupWithAttrs to be Group");
        };
        assert_eq!(attr_children.len(), 1);
        assert_eq!(
            rt.document.object(attr_children[0]).unwrap().name,
            "GroupAttrChild"
        );

        let rt_img = find_obj_by_name(&rt.document, "RasterWithAttrs");
        assert!(xf_err(&rt_img.transform, &raster_xf) < 1e-3);
        assert!(!rt_img.visible);
        assert!(rt_img.locked);
        assert!(!rt_img.printable);
        assert!((rt_img.opacity - 0.5).abs() < 1e-4);
        assert_eq!(rt_img.blend_mode, BlendMode::Screen);
        let NodePayload::Raster(r_rt) = &rt_img.payload else {
            panic!("expected RasterWithAttrs to be Raster");
        };
        let asset_rt = rt.document.asset(r_rt.asset_id).unwrap();
        assert_eq!(asset_rt.to_rgba8_vec(), px);

        let rt_art = find_obj_by_name(&rt.document, "ArtisticWithAttrs");
        assert!(xf_err(&rt_art.transform, &art_xf) < 1e-3);
        assert!(!rt_art.visible);
        assert!(rt_art.locked);
        assert!(!rt_art.printable);
        assert!((rt_art.opacity - 0.65).abs() < 1e-4);
        assert_eq!(rt_art.blend_mode, BlendMode::Overlay);
        let NodePayload::ArtisticText(art_rt) = &rt_art.payload else {
            panic!("expected ArtisticWithAttrs to be ArtisticText");
        };
        assert_eq!(art_rt.plain_text(), "CMX Art");

        let rt_para = find_obj_by_name(&rt.document, "ParagraphWithAttrs");
        assert!(xf_err(&rt_para.transform, &para_xf) < 1e-3);
        assert!(!rt_para.visible);
        assert!(rt_para.locked);
        assert!(!rt_para.printable);
        assert!((rt_para.opacity - 0.75).abs() < 1e-4);
        assert_eq!(rt_para.blend_mode, BlendMode::Darken);
        let NodePayload::ParagraphText(para_rt) = &rt_para.payload else {
            panic!("expected ParagraphWithAttrs to be ParagraphText");
        };
        assert_eq!(para_rt.plain_text(), "CMX Para");
        assert_eq!(para_rt.frame_size, Vec2D::new(60.0, 35.0));

        let rt_path = find_obj_by_name(&rt.document, "PathWithAttrs");
        assert!(!rt_path.visible);
        assert!(rt_path.locked);
        assert!(!rt_path.printable);
        assert!((rt_path.opacity - 0.35).abs() < 1e-4);
        assert_eq!(rt_path.blend_mode, BlendMode::Difference);
    }
}

#[test]
fn cmx_roundtrip_preserves_color_styles_object_styles_live_cascade_and_perspective_grid() {
    use coree_core::{
        ColorStyle, CommandHistory, CommandOp, Guide, GuideOrientation, ObjectStyle,
        PerspectiveGridConfig, PerspectiveGridMode, PerspectivePlane, RasterAsset,
    };

    let mut doc = Document::new_a4("CMX Styles & Perspective Grid");
    let pattern_px = vec![
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
    ];
    let pat_aid = doc
        .register_asset(RasterAsset::new(2, 2, pattern_px.clone(), 300.0, ColorMode::Rgb).unwrap());

    let initial_color = Color::rgba8(20, 140, 220, 255);
    let cs = ColorStyle::new("BrandBlue", initial_color.clone(), None);
    let cs_id = cs.id;
    doc.upsert_color_style(cs);

    let mut os_solid = ObjectStyle::new("SolidStyled");
    os_solid.fill = Some(Fill::Solid(initial_color.clone()));
    os_solid.color_style_id = Some(cs_id);
    let os_solid_id = os_solid.id;
    doc.upsert_object_style(os_solid);

    let mut os_pat = ObjectStyle::new("BitmapPatternStyled");
    os_pat.fill = Some(Fill::BitmapPattern {
        image_id: pat_aid,
        tile_w: 12.0,
        tile_h: 12.0,
    });
    let os_pat_id = os_pat.id;
    doc.upsert_object_style(os_pat);

    let mut styled_obj = SceneObject::new(
        ObjectId(0),
        "StyledRect",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            10.0, 10.0, 40.0, 30.0,
        ))),
    )
    .with_fill(Fill::Solid(initial_color));
    styled_obj.style_id = Some(os_solid_id);
    styled_obj.color_style_id = Some(cs_id);
    doc.add_object(styled_obj, None).unwrap();

    doc.replace_guides(vec![Guide {
        id: 1,
        orientation: GuideOrientation::Vertical(42.5),
        locked: true,
    }]);
    let mut grid = doc.grid();
    grid.visible = true;
    grid.snap = true;
    grid.spacing_x_mm = 12.5;
    doc.set_grid(grid);
    doc.set_perspective_grid(PerspectiveGridConfig {
        enabled: true,
        mode: PerspectiveGridMode::ThreePoint,
        horizon_y_mm: 85.0,
        vanishing_points: vec![
            Point2D::new(-30.0, 85.0),
            Point2D::new(240.0, 85.0),
            Point2D::new(105.0, -60.0),
        ],
        density: 16,
        opacity: 0.45,
        active_plane: PerspectivePlane::Left,
    });

    for (prec, endian) in [
        (CmxPrecision::Bit32, RiffEndian::Little),
        (CmxPrecision::Bit16, RiffEndian::Little),
    ] {
        let bytes = export_cmx_bytes_with_endian(&doc, prec, endian).unwrap();
        let mut rt = import_cdr_bytes(&bytes).unwrap();

        assert!(
            !rt.document
                .unknown_doc_chunks()
                .iter()
                .any(|c| &c.fourcc == b"stly" || &c.fourcc == b"guid"),
            "{prec:?}: stly/guid must not fall into unknown_doc_chunks"
        );
        assert_eq!(rt.document.color_styles().len(), 1);
        assert_eq!(rt.document.object_styles().len(), 2);
        assert_eq!(rt.document.guides().len(), 1);
        assert!(rt.document.grid().visible && rt.document.grid().snap);
        assert!((rt.document.grid().spacing_x_mm - 12.5).abs() < 1e-6);
        assert!(rt.document.perspective_grid().enabled);
        assert_eq!(
            rt.document.perspective_grid().mode,
            PerspectiveGridMode::ThreePoint
        );
        assert_eq!(
            rt.document.perspective_grid().active_plane,
            PerspectivePlane::Left
        );

        let rt_pat_style = rt
            .document
            .object_style(os_pat_id)
            .expect("BitmapPattern ObjectStyle preserved");
        if let Some(Fill::BitmapPattern { image_id, .. }) = rt_pat_style.fill {
            let asset = rt
                .document
                .asset(image_id)
                .expect("remapped BitmapPattern asset in ObjectStyle");
            assert_eq!(asset.to_rgba8_vec(), pattern_px);
        } else {
            panic!("expected Fill::BitmapPattern on ObjectStyle");
        }

        let rt_obj = find_obj_by_name(&rt.document, "StyledRect");
        assert_eq!(rt_obj.style_id, Some(os_solid_id));
        assert_eq!(rt_obj.color_style_id, Some(cs_id));

        // Verify live ColorStyle cascade via CommandOp::UpsertColorStyle on imported doc
        let updated_color = Color::rgba8(240, 40, 80, 255);
        let updated_cs = ColorStyle {
            id: cs_id,
            name: "BrandBlue".to_owned(),
            base_color: updated_color.clone(),
            harmony: None,
            derived_colors: Vec::new(),
        };
        let mut history = CommandHistory::new(16);
        history
            .execute(
                &mut rt.document,
                CommandOp::UpsertColorStyle { style: updated_cs },
            )
            .expect("UpsertColorStyle on CMX-imported doc");

        let cascaded_obj = find_obj_by_name(&rt.document, "StyledRect");
        assert_eq!(
            cascaded_obj.fill,
            Fill::Solid(updated_color.clone()),
            "{prec:?}: ColorStyle cascade must update CMX-imported object fill"
        );
        let cascaded_os = rt.document.object_style(os_solid_id).unwrap();
        assert_eq!(cascaded_os.fill, Some(Fill::Solid(updated_color)));
    }
}

#[test]
fn cdr_mesh_fill_preserves_alpha_and_nsew_handles_across_riff_and_x6() {
    let mut doc = Document::new("MeshExtCdr", 120.0, 100.0, ColorMode::Rgb, 300.0);
    let n0 = MeshNode {
        row: 0,
        col: 0,
        pos: Point2D::new(10.0, 10.0),
        color: Color::CYAN,
        alpha: 0.35,
        handle_n: Some(Point2D::new(1.5, -6.25)),
        handle_s: Some(Point2D::new(-2.0, 12.5)),
        handle_e: Some(Point2D::new(15.0, 3.0)),
        handle_w: None,
    };
    let n1 = MeshNode {
        row: 0,
        col: 1,
        pos: Point2D::new(70.0, 10.0),
        color: Color::MAGENTA,
        alpha: 0.8,
        handle_n: None,
        handle_s: Some(Point2D::new(0.0, 14.0)),
        handle_e: None,
        handle_w: Some(Point2D::new(-16.0, -4.5)),
    };
    let n2 = MeshNode {
        row: 1,
        col: 0,
        pos: Point2D::new(10.0, 60.0),
        color: Color::YELLOW,
        alpha: 1.0,
        handle_n: Some(Point2D::new(0.0, -14.0)),
        handle_s: None,
        handle_e: Some(Point2D::new(18.0, 0.0)),
        handle_w: None,
    };
    let n3 = MeshNode::new(1, 1, Point2D::new(70.0, 60.0), Color::KEY_BLACK);

    let obj = SceneObject::new(
        ObjectId(0),
        "MeshWithHandles",
        NodePayload::Shape(ParametricShape::GraphPaper {
            cols: 2,
            rows: 2,
            width: 60.0,
            height: 50.0,
        }),
    )
    .with_fill(Fill::MeshFill {
        rows: 2,
        cols: 2,
        nodes: vec![n0, n1, n2, n3],
    });
    doc.add_object(obj, None).unwrap();

    for ver in [CdrWriteVersion::RiffV12, CdrWriteVersion::ZipX6] {
        let bytes = export_cdr_bytes(&doc, ver).unwrap();
        let restored = import_cdr_bytes(&bytes).unwrap().document;
        let rt = find_obj_by_name(&restored, "MeshWithHandles");
        let Fill::MeshFill { rows, cols, nodes } = &rt.fill else {
            panic!("expected MeshFill after {ver:?} roundtrip");
        };
        assert_eq!((*rows, *cols, nodes.len()), (2, 2, 4));
        assert!(
            (nodes[0].alpha - 0.35).abs() < 1e-5,
            "node 0 alpha mismatch: {}",
            nodes[0].alpha
        );
        assert!((nodes[1].alpha - 0.8).abs() < 1e-5);
        assert!((nodes[3].alpha - 1.0).abs() < 1e-5);
        assert!(
            nodes[0]
                .handle_n
                .unwrap()
                .distance_to(Point2D::new(1.5, -6.25))
                < 1e-3
        );
        assert!(
            nodes[0]
                .handle_s
                .unwrap()
                .distance_to(Point2D::new(-2.0, 12.5))
                < 1e-3
        );
        assert!(
            nodes[0]
                .handle_e
                .unwrap()
                .distance_to(Point2D::new(15.0, 3.0))
                < 1e-3
        );
        assert!(nodes[0].handle_w.is_none());
        assert!(
            nodes[1]
                .handle_w
                .unwrap()
                .distance_to(Point2D::new(-16.0, -4.5))
                < 1e-3
        );
        assert!(nodes[3].handle_n.is_none());
    }
}
