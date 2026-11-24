// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Wave 4 End-to-End Cross-Crate Integration Test Suite for `coree`.
//!
//! Exercises flows `F01` through `F18` across all 7 workspace crates
//! (`coree-core`, `coree-cdr`, `coree-io`, `coree-storage`, `coree-render`,
//! `coree-prod`, `coree-app`) AND the real compiled `coree` CLI binary.

use coree_app::cdr_bridge::{
    import_and_inspect_binary_bytes, inspect_any_file, write_cdr_bytes_for_version, ContainerFormat,
};
use coree_app::state::{StudioApp, ToolId};
use coree_cdr::cdt::{export_cdt_bytes, import_cdt_bytes};
use coree_cdr::cmx::{export_cmx_bytes_with_endian, CmxPrecision};
use coree_cdr::fixtures::{
    generate_sample_document, malformed_cmpr_zlib_bomb_bytes, malformed_deep_nested_riff_bytes,
    malformed_garbage_bytes, malformed_huge_bitmap_cdr_bytes, malformed_huge_bitmap_cpt_bytes,
    malformed_truncated_chunk, malformed_truncated_header, malformed_zip_bomb_bytes,
    malformed_zip_slip_bytes, sample_cp1256_arabic_cdr_bytes, sample_cpt_multilayer_bytes,
    sample_cpt_riff_bytes, sample_unknown_chunk_cdr_bytes,
};
use coree_cdr::limits::CodecLimits;
use coree_cdr::riff::RiffEndian;
use coree_cdr::CdrWriteVersion;
use coree_core::{
    apply_boolean, knife_cut, virtual_segment_delete, Affine2D, AnchorPoint, ArtisticText,
    BezierPath, BlendMode, BooleanOp, Color, ColorMapMode, ColorMode, CommandHistory, CommandOp,
    ConnectorObject, ConnectorStyle, ContourMode, CornerStyle, DimensionKind, DimensionObject,
    DistortKind, Document, Effect, EllipseMode, EnvelopeMode, FeatherDirection, Fill, GradientStop,
    LensConfig, LensKind, MasterLayerScope, MeshNode, NodePayload, ObjectId, OutlineStyle,
    ParagraphText, ParametricShape, Point2D, RasterAsset, RasterFilter, Rect2D, SceneObject,
    Subpath, TextAlign, TextDirection, TextOnPath, TextStyle, TransparencyMode, Vec2D, VectorTable,
};
use coree_io::{
    export_dxf_to_string, export_eps_to_string, export_pdf_to_bytes, export_plt_to_string,
    export_svg_to_string, load_coree, save_coree, PdfColorSpace, PdfExportOptions,
};
use coree_prod::{
    add_barcode_to_document, add_qr_code_to_document, apply_print_merge, booklet_signature_order,
    collect_for_output, compute_production_metrics, compute_separations, ean13_check_digit,
    ean8_check_digit, find_and_replace, find_objects, generate_barcode, generate_cut_contour,
    generate_dieline, generate_print_marks, generate_qr_code, impose_document, run_macro,
    run_preflight, simulate_overprint_rgba, upca_check_digit, BarcodeOptions, BarcodeSymbology,
    CollectOptions, ColorMatch, DielineKind, FindQuery, ImpositionMode, MacroSandboxConfig,
    PreflightRules, PrintMarksOptions, PrintMergeOptions, PrintMergeTable, QrCodeOptions,
    QrErrorCorrection, ReplaceAction, SeparationChannel, SeparationOptions, TextMatchMode,
};
use coree_render::{
    apply_cutout_background_removal, apply_kashida_elongation, apply_raster_filter,
    clone_stamp_stroke, compute_page_layouts, compute_smart_fill, contains_arabic,
    create_smart_fill_object, erase_stroke, paint_brush_stroke, paragraph_to_curves,
    render_page_to_rgba, resample, smudge_stroke, text_on_path_to_curves, text_to_curves,
    trace_raster_to_mm, OutlinePreset, RasterMask, RenderOptions, TraceConfig, TraceMode, ViewMode,
};
use coree_storage::StorageDb;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("coree-e2e-{prefix}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create e2e temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn coree_bin() -> &'static str {
    env!("CARGO_BIN_EXE_coree")
}

fn run_coree_bin(args: &[&str]) -> String {
    let out = Command::new(coree_bin())
        .args(args)
        .output()
        .expect("spawn coree CLI binary");
    assert!(
        out.status.success(),
        "coree {:?} failed (status {:?}): stdout={} stderr={}",
        args,
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn count_differing_pixels(a: &[u8], b: &[u8]) -> usize {
    assert_eq!(a.len(), b.len());
    a.as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0.iter())
        .filter(|(pa, pb)| pa != pb)
        .count()
}

fn set_table_cell(table: &mut VectorTable, row: usize, col: usize, text: &str) {
    if let Some(cell) = table.cell_mut(row, col) {
        cell.text = text.to_owned();
    }
}

// =============================================================================
// 1. F01 / F15 / F16 — Binary Codecs, Passthrough, CP1256, Inspector & Security
// =============================================================================

#[test]
fn e2e_f01_f15_cdr_cdt_cmx_cpt_passthrough_cp1256_inspector_and_adversarial() {
    let tmp = TempDirGuard::new("f15-codecs");
    let limits = CodecLimits::default();

    // Real CLI binary --diagnostics verification
    let diag = run_coree_bin(&["--diagnostics"]);
    assert!(diag.contains("coree"));
    assert!(diag.contains("MIT OR Apache-2.0"));
    assert!(diag.contains("coree-core"));
    assert!(diag.contains("coree-cdr"));
    assert!(diag.contains("coree-render"));
    assert!(diag.contains("coree-prod"));
    assert!(diag.contains("#![forbid(unsafe_code)]"));

    // 1. Round-trip .cdr (RIFF v12, ZIP X4, ZIP X6+)
    let sample = generate_sample_document();
    for (ver, expected_era) in [
        (CdrWriteVersion::RiffV12, "CDR RIFF"),
        (CdrWriteVersion::ZipX4, "CDR ZIP/OPC X4-X5"),
        (CdrWriteVersion::ZipX6, "CDR ZIP/OPC X6-2025"),
    ] {
        let bytes = write_cdr_bytes_for_version(&sample, ver).expect("write cdr");
        let parsed = import_and_inspect_binary_bytes(&bytes, &limits).expect("parse cdr");
        assert!(
            parsed.report.format_era.contains(expected_era),
            "expected era '{expected_era}', got '{}'",
            parsed.report.format_era
        );
        assert!(!parsed.document.pages().is_empty());
        assert!(!parsed.document.objects().is_empty());
        assert!(!parsed.report.chunks.is_empty());
    }

    // 2. Round-trip .cdt (RIFF & ZIP)
    for ver in [CdrWriteVersion::RiffV12, CdrWriteVersion::ZipX6] {
        let cdt_bytes = export_cdt_bytes(&sample, ver).expect("export cdt");
        let rep = import_cdt_bytes(&cdt_bytes, &limits).expect("import cdt");
        assert!(!rep.document.objects().is_empty());
        let inspected = import_and_inspect_binary_bytes(&cdt_bytes, &limits).expect("inspect cdt");
        assert!(matches!(
            inspected.report.detected_format,
            ContainerFormat::CdtRiff { .. } | ContainerFormat::CdtZip
        ));
    }

    // 3. Round-trip .cmx (both little-endian RIFF and big-endian RIFX, 16-bit & 32-bit)
    for (prec, endian, expected_fmt) in [
        (
            CmxPrecision::Bit16,
            RiffEndian::Little,
            ContainerFormat::Cmx16,
        ),
        (
            CmxPrecision::Bit32,
            RiffEndian::Little,
            ContainerFormat::Cmx32,
        ),
        (CmxPrecision::Bit16, RiffEndian::Big, ContainerFormat::Cmx16),
        (CmxPrecision::Bit32, RiffEndian::Big, ContainerFormat::Cmx32),
    ] {
        let cmx_bytes = export_cmx_bytes_with_endian(&sample, prec, endian).expect("export cmx");
        if endian == RiffEndian::Big {
            assert_eq!(&cmx_bytes[0..4], b"RIFX");
        } else {
            assert_eq!(&cmx_bytes[0..4], b"RIFF");
        }
        let parsed = import_and_inspect_binary_bytes(&cmx_bytes, &limits).expect("import cmx");
        assert_eq!(parsed.report.detected_format, expected_fmt);
        assert!(!parsed.document.objects().is_empty());
    }

    // 4. Multi-layer .cpt (native CPT9 + RIFF CPT)
    for cpt_bytes in [sample_cpt_multilayer_bytes(), sample_cpt_riff_bytes()] {
        let parsed = import_and_inspect_binary_bytes(&cpt_bytes, &limits).expect("import cpt");
        assert!(matches!(
            parsed.report.detected_format,
            ContainerFormat::Cpt { .. }
        ));
        assert!(!parsed.document.assets().is_empty());
        assert!(!parsed.document.objects().is_empty());
    }

    // 5. Unknown-Chunk Passthrough byte-for-byte preservation across round-trip
    let unk_bytes = sample_unknown_chunk_cdr_bytes();
    let pass1 = import_and_inspect_binary_bytes(&unk_bytes, &limits).expect("parse unk pass1");
    let xdoc_orig = pass1
        .document
        .unknown_doc_chunks()
        .iter()
        .find(|c| &c.fourcc == b"XDOC")
        .expect("XDOC preserved on import")
        .raw_bytes
        .clone();
    let xobj_orig = pass1
        .document
        .objects()
        .values()
        .flat_map(|o| o.unknown_chunks.iter())
        .find(|c| &c.fourcc == b"XOBJ")
        .expect("XOBJ preserved on import")
        .raw_bytes
        .clone();
    assert_eq!(xdoc_orig, vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD]);
    assert_eq!(xobj_orig, vec![0xFE, 0xDC, 0xBA, 0x98, 0x76]);

    for ver in [CdrWriteVersion::RiffV12, CdrWriteVersion::ZipX6] {
        let rt_bytes = write_cdr_bytes_for_version(&pass1.document, ver).expect("re-export unk");
        let pass2 = import_and_inspect_binary_bytes(&rt_bytes, &limits).expect("re-import unk");
        let xdoc_rt = pass2
            .document
            .unknown_doc_chunks()
            .iter()
            .find(|c| &c.fourcc == b"XDOC")
            .expect("XDOC survives round-trip");
        let xobj_rt = pass2
            .document
            .objects()
            .values()
            .flat_map(|o| o.unknown_chunks.iter())
            .find(|c| &c.fourcc == b"XOBJ")
            .expect("XOBJ survives round-trip");
        assert_eq!(xdoc_rt.raw_bytes, xdoc_orig);
        assert_eq!(xobj_rt.raw_bytes, xobj_orig);
        assert!(
            pass2.report.unknown_chunks.len() >= 2,
            "Inspector must report both preserved unknown chunks"
        );
    }

    // 6. Legacy Arabic CP1256 decoding (both explicit codepage 1256 and heuristic auto-detect)
    let cp1256_bytes = sample_cp1256_arabic_cdr_bytes();
    let cp1256_parsed =
        import_and_inspect_binary_bytes(&cp1256_bytes, &limits).expect("import CP1256 CDR");
    let decoded_texts: Vec<String> = cp1256_parsed
        .document
        .objects()
        .values()
        .filter_map(|o| match &o.payload {
            NodePayload::ArtisticText(t) => Some(t.plain_text()),
            NodePayload::ParagraphText(p) => Some(p.plain_text()),
            _ => None,
        })
        .collect();
    assert!(
        decoded_texts
            .iter()
            .any(|s| s.contains("تصميم عربي كلاسيكي")),
        "explicit CP1256 text missing: {decoded_texts:?}"
    );
    assert!(
        decoded_texts
            .iter()
            .any(|s| s.contains("مرحبا بالعالم العربي")),
        "auto-detected CP1256 text missing: {decoded_texts:?}"
    );

    // 7. CDR Inspector via real `coree inspect --json` CLI binary
    let cdr_path = tmp.path().join("inspect_me.cdr");
    std::fs::write(&cdr_path, &unk_bytes).unwrap();
    let file_rep = inspect_any_file(&cdr_path).expect("inspect_any_file");
    assert!(file_rep.format_human().contains("Unknown Chunks Preserved"));
    let cli_json = run_coree_bin(&["inspect", &cdr_path.to_string_lossy(), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&cli_json).expect("valid inspect JSON");
    assert!(v["unknown_chunks"].as_array().unwrap().len() >= 2);

    // 8. Adversarial input rejection (API + real CLI non-zero graceful exit, zero panics)
    let adversarial_cases: [(&str, Vec<u8>); 8] = [
        ("zip_bomb.cdr", malformed_zip_bomb_bytes()),
        ("cmpr_zlib_bomb.cdr", malformed_cmpr_zlib_bomb_bytes()),
        ("zip_slip.cdr", malformed_zip_slip_bytes()),
        ("truncated_header.cdr", malformed_truncated_header()),
        ("truncated_chunk.cdr", malformed_truncated_chunk()),
        ("garbage.cdr", malformed_garbage_bytes()),
        ("deep_nest.cdr", malformed_deep_nested_riff_bytes(200)),
        ("huge_bmp.cdr", malformed_huge_bitmap_cdr_bytes()),
    ];
    for (name, bytes) in &adversarial_cases {
        let res = import_and_inspect_binary_bytes(bytes, &limits);
        assert!(
            res.is_err(),
            "adversarial fixture '{name}' MUST be rejected cleanly"
        );
    }
    assert!(import_and_inspect_binary_bytes(&malformed_huge_bitmap_cpt_bytes(), &limits).is_err());

    // Confirm real `coree inspect` binary rejects Zip-Bomb & Zip-Slip cleanly without crashing
    for (name, bytes) in [
        ("cli_zip_bomb.cdr", malformed_zip_bomb_bytes()),
        ("cli_zip_slip.cdr", malformed_zip_slip_bytes()),
    ] {
        let p = tmp.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        let out = Command::new(coree_bin())
            .args(["inspect", &p.to_string_lossy()])
            .output()
            .expect("spawn coree inspect on adversarial file");
        assert!(
            !out.status.success(),
            "CLI must reject adversarial file {name}"
        );
    }
}

// =============================================================================
// 2. F08 — All 11 Live Lenses Rendered Headlessly + Frozen/Viewpoint/RemoveFace
// =============================================================================

fn build_lens_backdrop_doc() -> Document {
    let mut doc = Document::new("Lens Test", 80.0, 60.0, ColorMode::Rgb, 150.0);
    // High-contrast striped multicolor background so every optical & color lens visibly alters pixels
    let colors = [
        Color::rgb(0.90, 0.18, 0.22),
        Color::rgb(0.15, 0.55, 0.95),
        Color::rgb(0.95, 0.78, 0.12),
        Color::rgb(0.12, 0.20, 0.35),
        Color::rgb(0.20, 0.78, 0.48),
        Color::rgb(0.82, 0.24, 0.72),
    ];
    for (i, col) in colors.iter().enumerate() {
        let rect = SceneObject::new(
            ObjectId(0),
            format!("Stripe {i}"),
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 12.0,
                height: 52.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(col.clone()))
        .with_outline(OutlineStyle::solid(Color::BLACK, 0.4))
        .with_transform(Affine2D::translate(4.0 + i as f64 * 12.0, 4.0));
        doc.add_object(rect, None).unwrap();
    }
    // Cross bar for geometric distortion visibility (FishEye / Magnify / Viewpoint)
    let bar = SceneObject::new(
        ObjectId(0),
        "Cross Bar",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 72.0,
            height: 8.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgb(0.96, 0.96, 0.96)))
    .with_outline(OutlineStyle::solid(Color::rgb(0.08, 0.1, 0.15), 0.6))
    .with_transform(Affine2D::translate(4.0, 26.0));
    doc.add_object(bar, None).unwrap();
    doc
}

#[test]
fn e2e_f08_all_eleven_lenses_headless_render_and_controls() {
    let base_doc = build_lens_backdrop_doc();
    let baseline = render_page_to_rgba(&base_doc, 0, 72.0, ViewMode::Enhanced)
        .expect("render baseline without lens");

    let all_eleven: Vec<LensKind> = vec![
        LensKind::Brighten { rate_pct: 55.0 },
        LensKind::ColorAdd {
            color: Color::rgb(0.95, 0.65, 0.1),
            rate_pct: 75.0,
        },
        LensKind::ColorLimit {
            color: Color::rgb(0.2, 0.85, 0.95),
            rate_pct: 80.0,
        },
        LensKind::CustomColorMap {
            from_color: Color::rgb(0.08, 0.10, 0.35),
            to_color: Color::rgb(0.98, 0.72, 0.18),
            palette_mode: ColorMapMode::ForwardRainbow,
        },
        LensKind::FishEye { rate_pct: 180.0 },
        LensKind::HeatMap {
            palette_rotation_deg: 25.0,
        },
        LensKind::Invert,
        LensKind::Magnify { amount: 2.4 },
        LensKind::TintedGrayscale {
            tint: Color::rgb(0.24, 0.65, 1.0),
        },
        LensKind::Transparency {
            color: Color::rgb(0.15, 0.18, 0.30),
            rate_pct: 65.0,
        },
        LensKind::Wireframe {
            override_outline: Some(Color::rgb(0.1, 0.45, 0.95)),
            override_fill: Some(Color::rgb(0.98, 0.99, 1.0)),
        },
    ];
    assert_eq!(all_eleven.len(), LensKind::VARIANT_COUNT);

    for kind in &all_eleven {
        let mut doc = base_doc.clone();
        let mut host = SceneObject::new(
            ObjectId(0),
            format!("Lens {}", kind.name()),
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 44.0,
                height: 36.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(18.0, 12.0));
        host.lens = Some(LensConfig::new(kind.clone()));
        doc.add_object(host, None).unwrap();

        let rendered =
            render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).expect("render lens page");
        let diff_px = count_differing_pixels(&baseline.rgba, &rendered.rgba);
        assert!(
            diff_px > 50,
            "Lens '{}' must visibly alter rendered pixels (got only {diff_px} changed px)",
            kind.name()
        );
    }

    // Verify Frozen + Viewpoint + RemoveFace lens controls
    let mut doc_vp_default = base_doc.clone();
    let mut host_default = SceneObject::new(
        ObjectId(0),
        "Magnify Default",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 26.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(25.0, 17.0));
    host_default.lens = Some(LensConfig {
        kind: LensKind::Magnify { amount: 2.0 },
        frozen: true,
        viewpoint: None,
        remove_face: false,
    });
    doc_vp_default.add_object(host_default, None).unwrap();

    let mut doc_vp_shifted = base_doc.clone();
    let mut host_shifted = SceneObject::new(
        ObjectId(0),
        "Magnify Viewpoint",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 26.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(25.0, 17.0));
    host_shifted.lens = Some(LensConfig {
        kind: LensKind::Magnify { amount: 2.0 },
        frozen: true,
        viewpoint: Some(Point2D::new(10.0, 10.0)),
        remove_face: false,
    });
    doc_vp_shifted.add_object(host_shifted, None).unwrap();

    let img_def = render_page_to_rgba(&doc_vp_default, 0, 72.0, ViewMode::Enhanced).unwrap();
    let img_vp = render_page_to_rgba(&doc_vp_shifted, 0, 72.0, ViewMode::Enhanced).unwrap();
    assert!(
        count_differing_pixels(&img_def.rgba, &img_vp.rgba) > 20,
        "Viewpoint offset must sample a different region of the document"
    );

    // RemoveFace over partial empty page region: compare remove_face=false vs remove_face=true
    let mut sparse = Document::new("RemoveFace", 80.0, 60.0, ColorMode::Rgb, 72.0);
    sparse
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Small Square",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 15.0,
                    height: 15.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::rgb(0.1, 0.4, 0.8)))
            .with_transform(Affine2D::translate(15.0, 15.0)),
            None,
        )
        .unwrap();
    let mut sparse_full = sparse.clone();
    let mut l_full = SceneObject::new(
        ObjectId(0),
        "Full Face Lens",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 50.0,
            height: 40.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(10.0, 10.0));
    l_full.lens = Some(LensConfig {
        kind: LensKind::Transparency {
            color: Color::rgb(0.95, 0.2, 0.2),
            rate_pct: 70.0,
        },
        frozen: false,
        viewpoint: None,
        remove_face: false,
    });
    sparse_full.add_object(l_full, None).unwrap();

    let mut sparse_rf = sparse.clone();
    let mut l_rf = SceneObject::new(
        ObjectId(0),
        "RemoveFace Lens",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 50.0,
            height: 40.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none())
    .with_transform(Affine2D::translate(10.0, 10.0));
    l_rf.lens = Some(LensConfig {
        kind: LensKind::Transparency {
            color: Color::rgb(0.95, 0.2, 0.2),
            rate_pct: 70.0,
        },
        frozen: false,
        viewpoint: None,
        remove_face: true,
    });
    sparse_rf.add_object(l_rf, None).unwrap();

    let img_full = render_page_to_rgba(&sparse_full, 0, 72.0, ViewMode::Enhanced).unwrap();
    let img_rf = render_page_to_rgba(&sparse_rf, 0, 72.0, ViewMode::Enhanced).unwrap();
    assert!(
        count_differing_pixels(&img_full.rgba, &img_rf.rgba) > 100,
        "remove_face=true must mask lens out over empty paper background"
    );
}

// =============================================================================
// 3. F02 / F04 / F06 / F07 — Effects, PowerClip, MeshFill, SmartFill, Knife, VSD & 9 Booleans
// =============================================================================

#[test]
fn e2e_f02_f04_f06_f07_effects_mesh_smartfill_knife_vsd_and_all_9_booleans() {
    let mut doc = Document::new("Effects & Shaping", 210.0, 160.0, ColorMode::Cmyk, 150.0);

    // Blend partner target
    let blend_target = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Blend End",
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: 10.0,
                    ry: 10.0,
                    mode: EllipseMode::Full,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
            )
            .with_fill(Fill::Solid(Color::MAGENTA))
            .with_transform(Affine2D::translate(185.0, 30.0)),
            None,
        )
        .unwrap();

    // All 10 live Vector Effects rendered cleanly on distinct shapes
    let effects: Vec<(&str, Effect)> = vec![
        (
            "DropShadow",
            Effect::DropShadow {
                dx_mm: 2.5,
                dy_mm: 2.5,
                blur_mm: 2.0,
                opacity: 0.6,
                color: Color::KEY_BLACK,
                feather_direction: FeatherDirection::Outside,
            },
        ),
        (
            "BlockShadow",
            Effect::BlockShadow {
                dx_mm: 4.0,
                dy_mm: 4.0,
                color: Color::cmyk(0.2, 0.2, 0.0, 0.8),
                simplify: true,
                overprint: false,
            },
        ),
        (
            "Contour",
            Effect::Contour {
                mode: ContourMode::Outside,
                steps: 3,
                offset_mm: 1.5,
                fill_color: Color::YELLOW,
                outline_color: Color::KEY_BLACK,
            },
        ),
        (
            "Blend",
            Effect::Blend {
                target_id: blend_target,
                steps: 5,
                rotation_deg: 15.0,
                along_path: None,
            },
        ),
        (
            "Distort",
            Effect::Distort {
                kind: DistortKind::Twister,
                amplitude: 25.0,
                frequency: 2.0,
                center: Point2D::new(10.0, 10.0),
            },
        ),
        (
            "Envelope",
            Effect::Envelope {
                mode: EnvelopeMode::SingleArc,
                cage: vec![
                    Point2D::new(0.0, 0.0),
                    Point2D::new(10.0, -3.0),
                    Point2D::new(20.0, 0.0),
                    Point2D::new(22.0, 10.0),
                    Point2D::new(20.0, 20.0),
                    Point2D::new(10.0, 23.0),
                    Point2D::new(0.0, 20.0),
                    Point2D::new(-2.0, 10.0),
                ],
            },
        ),
        (
            "Extrude25D",
            Effect::Extrude25D {
                vanishing_point: Point2D::new(35.0, -10.0),
                depth: 8.0,
                bevel_depth: 1.0,
                shade_color: Color::cmyk(0.6, 0.3, 0.0, 0.4),
                light_dir: [0.5, -0.5, 0.7],
            },
        ),
        (
            "Perspective",
            Effect::Perspective {
                quad: [
                    Point2D::new(2.0, 0.0),
                    Point2D::new(18.0, 2.0),
                    Point2D::new(20.0, 20.0),
                    Point2D::new(0.0, 18.0),
                ],
            },
        ),
        (
            "Symmetry",
            Effect::Symmetry {
                axes: 4,
                mirror: true,
                origin: Point2D::new(10.0, 10.0),
            },
        ),
        (
            "Transparency",
            Effect::Transparency {
                mode: TransparencyMode::Uniform(0.55),
                blend_mode: BlendMode::Multiply,
            },
        ),
    ];

    for (i, (label, eff)) in effects.into_iter().enumerate() {
        let col = (i % 5) as f64;
        let row = (i / 5) as f64;
        let mut obj = SceneObject::new(
            ObjectId(0),
            format!("Eff {label}"),
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 20.0,
                height: 20.0,
                corner_radii: [2.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.4))
        .with_transform(Affine2D::translate(15.0 + col * 36.0, 15.0 + row * 38.0));
        obj.effects.push(eff);
        doc.add_object(obj, None).unwrap();
    }

    // PowerClip container + content + nested containment & extraction
    let pc_container = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "PC Frame",
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: 18.0,
                    ry: 14.0,
                    mode: EllipseMode::Full,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
            )
            .with_fill(Fill::Solid(Color::rgb(0.92, 0.94, 0.98)))
            .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.5))
            .with_transform(Affine2D::translate(35.0, 115.0)),
            None,
        )
        .unwrap();
    let pc_child = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "PC Artwork",
                NodePayload::Shape(ParametricShape::Star {
                    points: 5,
                    outer_radius: 22.0,
                    inner_ratio: 0.42,
                    complex: false,
                }),
            )
            .with_fill(Fill::Solid(Color::rgb(0.96, 0.68, 0.24)))
            .with_transform(Affine2D::translate(35.0, 115.0)),
            None,
        )
        .unwrap();
    doc.create_powerclip(pc_container, &[pc_child])
        .expect("create PowerClip");
    assert!(matches!(
        doc.object(pc_container).unwrap().payload,
        NodePayload::PowerClip { .. }
    ));

    // MeshFill (3x3 Bezier grid)
    let mut mesh_nodes = Vec::new();
    for r in 0..3usize {
        for c in 0..3usize {
            let color = match (r, c) {
                (0, 0) => Color::rgb(0.24, 0.65, 1.0),
                (0, 2) => Color::rgb(0.96, 0.68, 0.24),
                (2, 0) => Color::rgb(0.22, 0.85, 0.66),
                (2, 2) => Color::rgb(1.0, 0.40, 0.77),
                _ => Color::rgb(0.95, 0.96, 0.98),
            };
            mesh_nodes.push(MeshNode {
                row: r,
                col: c,
                pos: Point2D::new((c as f64) * 20.0, (r as f64) * 14.0),
                color,
            });
        }
    }
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Mesh Patch",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 40.0,
                height: 28.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::MeshFill {
            rows: 3,
            cols: 3,
            nodes: mesh_nodes,
        })
        .with_transform(Affine2D::translate(80.0, 105.0)),
        None,
    )
    .unwrap();

    // Render full Effects + PowerClip + MeshFill document and assert non-empty artwork
    let rendered = render_page_to_rgba(&doc, 0, 72.0, ViewMode::Enhanced).expect("render effects");
    let non_white = rendered
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] < 250 || px[1] < 250 || px[2] < 250)
        .count();
    assert!(
        non_white > 1000,
        "rendered effects page must contain rich artwork"
    );

    // Extract PowerClip back cleanly
    let extracted = doc
        .extract_powerclip(pc_container)
        .expect("extract PowerClip");
    assert_eq!(extracted, vec![pc_child]);

    // SmartFill inside two intersecting rectangles
    let mut sf_doc = Document::new("SmartFill", 100.0, 100.0, ColorMode::Rgb, 150.0);
    sf_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "SF A",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 10.0, 40.0, 40.0,
                ))),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::solid(Color::BLACK, 0.5)),
            None,
        )
        .unwrap();
    sf_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "SF B",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    30.0, 20.0, 40.0, 40.0,
                ))),
            )
            .with_fill(Fill::None)
            .with_outline(OutlineStyle::solid(Color::BLACK, 0.5)),
            None,
        )
        .unwrap();
    let sf_path = compute_smart_fill(
        &sf_doc,
        0,
        Point2D::new(40.0, 35.0),
        Fill::Solid(Color::CYAN),
        OutlineStyle::none(),
    )
    .expect("SmartFill must detect bounded intersection pocket");
    assert!(sf_path.signed_area_mm2().abs() > 100.0);
    let sf_obj = create_smart_fill_object(
        &sf_doc,
        0,
        Point2D::new(40.0, 35.0),
        Fill::Solid(Color::CYAN),
        OutlineStyle::none(),
    )
    .expect("create_smart_fill_object");
    sf_doc.add_object(sf_obj, None).unwrap();

    // Knife cut + Virtual Segment Delete
    let rect_path = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 40.0, 20.0));
    let slices = knife_cut(
        &rect_path,
        Point2D::new(20.0, -5.0),
        Point2D::new(20.0, 25.0),
        true,
    );
    assert_eq!(
        slices.len(),
        2,
        "knife_cut must split rectangle into 2 halves"
    );

    let divider = BezierPath::from_line(Point2D::new(20.0, -5.0), Point2D::new(20.0, 25.0));
    let vsd = virtual_segment_delete(&rect_path, &[divider], Point2D::new(0.0, 10.0), 3.0);
    assert!(
        vsd.path_length_mm() < rect_path.path_length_mm() - 10.0,
        "virtual_segment_delete must remove the clicked bounded segment"
    );

    // ALL 9 BooleanOp variants on overlapping squares (A = [0..20, 0..20], B = [10..30, 0..20])
    let sq_a = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0));
    let sq_b = BezierPath::from_rect(Rect2D::from_xywh(10.0, 0.0, 20.0, 20.0));
    let all_9_ops = [
        (BooleanOp::Weld, 600.0, 1usize),
        (BooleanOp::Trim, 200.0, 1usize),
        (BooleanOp::Intersect, 200.0, 1usize),
        (BooleanOp::Simplify, 600.0, 2usize),
        (BooleanOp::FrontMinusBack, 200.0, 1usize),
        (BooleanOp::BackMinusFront, 200.0, 1usize),
        (BooleanOp::CreateBoundary, 600.0, 1usize),
        (BooleanOp::Combine, 800.0, 1usize),
        (BooleanOp::BreakApart, 800.0, 2usize),
    ];
    for (op, expected_area, expected_count) in all_9_ops {
        let res = apply_boolean(op, &sq_a, &sq_b).expect("boolean op succeeds");
        assert_eq!(res.len(), expected_count, "BooleanOp::{op:?} output count");
        let total_area: f64 = res.iter().map(|p| p.signed_area_mm2().abs()).sum();
        assert!(
            (total_area - expected_area).abs() < 10.0,
            "BooleanOp::{op:?} area {total_area:.1} != expected {expected_area:.1}"
        );
    }
}

// =============================================================================
// 4. F03 / F05 — Arabic RTL/Kashida, Text-on-Path, Curves, Table, Dims, Connectors & Multipage
// =============================================================================

#[test]
fn e2e_f03_f05_arabic_rtl_kashida_text_on_path_curves_table_dims_connectors_multipage() {
    let tmp = TempDirGuard::new("f03-f05");

    // Arabic detection & Kashida justification elongation
    let ar_sample = "تصميم متجهي احترافي";
    assert!(contains_arabic(ar_sample));
    let elongated = apply_kashida_elongation(ar_sample, 2);
    assert!(
        elongated.contains('\u{0640}'),
        "Kashida elongation must insert Arabic Tatweel U+0640: {elongated}"
    );

    // ArtisticText & ParagraphText -> real glyph curves
    let ar_text = ArtisticText::from_plain(
        "مرحبا coree 2026",
        TextStyle {
            font_family: "Noto Sans Arabic".to_owned(),
            font_size_pt: 20.0,
            direction: TextDirection::Rtl,
            align: TextAlign::ForceJustifyKashida,
            kashida_stretch: 0.6,
            ..TextStyle::default()
        },
    );
    let art_curves = text_to_curves(&ar_text);
    assert!(
        art_curves.subpaths.len() > 2,
        "ArtisticText -> curves must emit multi-glyph outlines"
    );

    let para = ParagraphText::new(
        "Studio layout engine with bilingual Arabic + English typography.",
        Vec2D::new(80.0, 40.0),
        TextStyle::default(),
    );
    let para_curves = paragraph_to_curves(&para);
    assert!(
        para_curves.subpaths.len() > 4,
        "ParagraphText -> curves must emit multi-glyph outlines"
    );

    // Fit Text to Path
    let arc_path = BezierPath::from_subpath(Subpath::from_points(
        [
            Point2D::new(10.0, 60.0),
            Point2D::new(50.0, 35.0),
            Point2D::new(90.0, 60.0),
        ],
        false,
    ));
    let top_spec = TextOnPath {
        text: ArtisticText::from_plain("CURVED TEXT PATH", TextStyle::default()),
        path_id: ObjectId(1),
        offset_mm: 2.0,
        distance_mm: 1.5,
        flip_vertical: false,
        flip_horizontal: false,
    };
    let top_curves = text_on_path_to_curves(&top_spec, &arc_path);
    assert!(
        !top_curves.is_empty(),
        "Fit Text to Path must generate positioned glyph curves along host path"
    );

    // VectorTable + Dimensions + Connectors + Multipage + Master Layer
    let mut doc = build_showcase_multipage_doc();
    assert_eq!(doc.pages().len(), 3);

    // Verify VectorTable cell merge + insert_row invariant safety
    let mut table = VectorTable::new(3, 3, 25.0, 10.0);
    table.merge_cells(0, 0, 0, 2).expect("merge header row");
    set_table_cell(&mut table, 0, 0, "Specification Header");
    set_table_cell(&mut table, 1, 0, "Width");
    set_table_cell(&mut table, 1, 1, "210 mm");
    table.insert_row(1, 10.0).expect("insert row");
    assert_eq!(table.rows, 4);
    assert!(table.cell_rect(0, 0).is_some());

    // Add DimensionObject + ConnectorObject on Page 1
    let box_a = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Node A",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 20.0,
                    height: 15.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_transform(Affine2D::translate(15.0, 60.0)),
            None,
        )
        .unwrap();
    let box_b = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Node B",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 20.0,
                    height: 15.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_transform(Affine2D::translate(75.0, 60.0)),
            None,
        )
        .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Connector AB",
            NodePayload::Connector(ConnectorObject::between_objects(
                box_a,
                AnchorPoint::Right,
                box_b,
                AnchorPoint::Left,
                ConnectorStyle::RoundedRightAngle,
            )),
        ),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Span Dim",
            NodePayload::Dimension(DimensionObject::linear(
                DimensionKind::Horizontal,
                Point2D::new(15.0, 60.0),
                Point2D::new(95.0, 60.0),
                6.0,
            )),
        ),
        None,
    )
    .unwrap();

    // Multipage View layouts & real CLI `coree render --multipage`
    let layouts = compute_page_layouts(&doc, true, RenderOptions::export().multipage_gap_mm);
    assert_eq!(layouts.len(), 3);
    assert!(layouts[1].origin_mm.x > layouts[0].origin_mm.x);
    assert!(layouts[2].origin_mm.x > layouts[1].origin_mm.x);

    let multi_coree = tmp.path().join("multi.coree");
    let multi_png = tmp.path().join("multi.png");
    save_coree(&doc, &multi_coree).expect("save multipage .coree");
    let msg = run_coree_bin(&[
        "render",
        &multi_coree.to_string_lossy(),
        &multi_png.to_string_lossy(),
        "--multipage",
        "--dpi",
        "72",
    ]);
    assert!(msg.contains("Rendered multipage"));
    let decoded = coree_io::import_raster(&multi_png).expect("decode multipage PNG");
    assert!(
        decoded.width > decoded.height * 2,
        "3 landscape pages side-by-side must produce wide multipage canvas ({}x{})",
        decoded.width,
        decoded.height
    );
}

// =============================================================================
// 5. F09 / F10 — Integrated Bitmap Studio & Vector Trace (Quick, Outline, Centerline)
// =============================================================================

#[test]
fn e2e_f09_f10_bitmap_studio_and_vector_trace() {
    let tmp = TempDirGuard::new("f09-f10");

    // 1. Create 64x64 white canvas asset and paint vivid geometric artwork via Bitmap Studio
    let mut asset =
        RasterAsset::new(64, 64, vec![255u8; 64 * 64 * 4], 150.0, ColorMode::Rgb).unwrap();
    paint_brush_stroke(
        &mut asset,
        &[
            Point2D::new(12.0, 12.0),
            Point2D::new(52.0, 12.0),
            Point2D::new(52.0, 52.0),
        ],
        6.0,
        0.9,
        Color::rgb(0.15, 0.45, 0.92),
        1.0,
    );
    let after_paint = asset.rgba_bytes.clone();

    // Clone stamp + smudge + eraser
    clone_stamp_stroke(
        &mut asset,
        Point2D::new(16.0, 12.0),
        &[Point2D::new(16.0, 44.0), Point2D::new(34.0, 44.0)],
        5.0,
        0.8,
    );
    assert_ne!(
        asset.rgba_bytes, after_paint,
        "clone stamp must modify pixels"
    );

    let before_smudge = asset.rgba_bytes.clone();
    smudge_stroke(
        &mut asset,
        &[Point2D::new(12.0, 12.0), Point2D::new(26.0, 26.0)],
        4.0,
        0.7,
    );
    assert_ne!(asset.rgba_bytes, before_smudge, "smudge must blend pixels");

    erase_stroke(&mut asset, &[Point2D::new(32.0, 32.0)], 4.0, 1.0);
    let center_alpha = asset.rgba_bytes[(32 * 64 + 32) * 4 + 3];
    assert!(
        center_alpha < 128,
        "eraser must reduce alpha at stroke center"
    );

    // Raster Filters + Cutout + Resample
    let blurred = apply_raster_filter(&asset, &RasterFilter::GaussianBlur { radius_px: 2.0 });
    assert_ne!(blurred.rgba_bytes, asset.rgba_bytes);
    let inverted = apply_raster_filter(&asset, &RasterFilter::Invert);
    assert_ne!(inverted.rgba_bytes, asset.rgba_bytes);
    let posterized = apply_raster_filter(&asset, &RasterFilter::Posterize { levels: 4 });
    assert_ne!(posterized.rgba_bytes, asset.rgba_bytes);

    let mut cutout = asset.clone();
    let fg_mask = RasterMask::from_rectangle(64, 64, Rect2D::from_xywh(8.0, 8.0, 48.0, 48.0));
    apply_cutout_background_removal(&mut cutout, &fg_mask, 1.5);
    assert_eq!(
        cutout.rgba_bytes[3], 0,
        "corner pixel outside mask must become transparent"
    );

    let resized = resample(&asset, 96, 96).expect("resample 96x96");
    assert_eq!((resized.width, resized.height), (96, 96));

    // 2. Vector Trace in ALL 3 modes (Quick, Outline, Centerline) via API + real CLI binary
    let mut logo_asset =
        RasterAsset::new(64, 64, vec![255u8; 64 * 64 * 4], 150.0, ColorMode::Rgb).unwrap();
    paint_brush_stroke(
        &mut logo_asset,
        &[Point2D::new(10.0, 32.0), Point2D::new(54.0, 32.0)],
        5.0,
        1.0,
        Color::rgb(0.1, 0.2, 0.8),
        1.0,
    );
    paint_brush_stroke(
        &mut logo_asset,
        &[Point2D::new(32.0, 10.0), Point2D::new(32.0, 54.0)],
        5.0,
        1.0,
        Color::rgb(0.9, 0.3, 0.1),
        1.0,
    );

    for mode in [
        TraceMode::QuickTrace,
        TraceMode::OutlineTrace(OutlinePreset::Logo),
        TraceMode::CenterlineTrace,
    ] {
        let cfg = TraceConfig {
            mode,
            max_colors: 4,
            ..TraceConfig::default()
        };
        let curves = trace_raster_to_mm(&logo_asset, 40.0, 40.0, &cfg);
        assert!(
            !curves.is_empty(),
            "trace mode {mode:?} must produce vector curves"
        );
    }

    // Export logo PNG and run real CLI `coree trace --mode quick|outline|centerline`
    let logo_png = tmp.path().join("logo.png");
    coree_io::export_raster(
        logo_asset.width,
        logo_asset.height,
        &logo_asset.rgba_bytes,
        &coree_io::RasterExportOptions::new(coree_io::RasterFormat::Png, 150.0),
        &logo_png,
    )
    .unwrap();

    for (mode_str, ext) in [
        ("quick", "svg"),
        ("outline", "coree"),
        ("centerline", "dxf"),
    ] {
        let out_path = tmp.path().join(format!("traced_{mode_str}.{ext}"));
        let msg = run_coree_bin(&[
            "trace",
            &logo_png.to_string_lossy(),
            &out_path.to_string_lossy(),
            "--mode",
            mode_str,
            "--colors",
            "4",
        ]);
        assert!(msg.contains("Traced"));
        assert!(out_path.is_file());
    }
}

// =============================================================================
// 6. F11–F14 / F16 / F17 — Full Commercial Print & Packaging Production Pipeline
// =============================================================================

#[test]
fn e2e_f11_f12_f13_f14_f16_f17_full_production_pipeline_and_rhai_macros() {
    let tmp = TempDirGuard::new("f11-f17-prod");

    // 1. CMYK + Spot Separations & Overprint simulation
    let mut sep_doc = Document::new("Separations", 100.0, 100.0, ColorMode::Cmyk, 300.0);
    let cyan_id = sep_doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Cyan Base",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 60.0,
                    height: 60.0,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::CYAN))
            .with_transform(Affine2D::translate(15.0, 15.0)),
            None,
        )
        .unwrap();
    let mut yellow_top = SceneObject::new(
        ObjectId(0),
        "Yellow Overprint",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 40.0,
            height: 40.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::YELLOW))
    .with_transform(Affine2D::translate(35.0, 35.0));
    yellow_top.blend_mode = BlendMode::Normal;
    let yellow_id = sep_doc.add_object(yellow_top, None).unwrap();

    // One-click CutContour around artwork
    let cut_res = generate_cut_contour(&mut sep_doc, &[cyan_id, yellow_id], 2.0, "CutContour")
        .expect("generate CutContour");
    assert!(cut_res.cut_length_mm > 100.0);

    let sep_knockout = compute_separations(&sep_doc, 0, &SeparationOptions::default()).unwrap();
    assert!(sep_knockout.plate(&SeparationChannel::Cyan).is_some());
    assert!(sep_knockout.plate(&SeparationChannel::Yellow).is_some());
    assert!(sep_knockout.spot_plate("CutContour").is_some());
    let rgba_knockout = simulate_overprint_rgba(&sep_knockout);

    let mut sep_hist = coree_core::CommandHistory::default();
    sep_hist
        .execute(
            &mut sep_doc,
            coree_core::CommandOp::SetBlendMode {
                ids: vec![yellow_id],
                blend_mode: BlendMode::Multiply,
            },
        )
        .expect("set blend mode");
    let sep_overprint = compute_separations(&sep_doc, 0, &SeparationOptions::default()).unwrap();
    let rgba_overprint = simulate_overprint_rgba(&sep_overprint);
    assert!(
        count_differing_pixels(&rgba_knockout, &rgba_overprint) > 20,
        "overprint fill must produce distinct subtractive composite vs knockout"
    );

    // 2. All 3 Imposition modes: N-Up, Booklet Saddle-Stitch (with odd-page auto-pad), Tiling
    let mut multi_doc = Document::new("Booklet Src", 105.0, 148.0, ColorMode::Cmyk, 300.0);
    for p in 2..=6 {
        multi_doc.add_page(format!("Page {p}"), 105.0, 148.0, 3.0);
    }
    let sig = booklet_signature_order(6);
    assert_eq!(sig.len(), 4); // 6 pages -> padded to 8 -> 4 spreads (2 double-sided sheets)

    let nup = impose_document(
        &sep_doc,
        &ImpositionMode::NUp {
            cols: 2,
            rows: 2,
            gap_x_mm: 5.0,
            gap_y_mm: 5.0,
            sheet_w_mm: 240.0,
            sheet_h_mm: 240.0,
        },
    )
    .expect("N-Up imposition");
    assert_eq!(nup.pages().len(), 1);

    let booklet = impose_document(
        &multi_doc,
        &ImpositionMode::BookletSaddleStitch {
            sheet_w_mm: 210.0,
            sheet_h_mm: 148.0,
        },
    )
    .expect("Booklet imposition");
    assert_eq!(booklet.pages().len(), 4);

    let banner = Document::new("Banner", 600.0, 400.0, ColorMode::Cmyk, 150.0);
    let tiled = impose_document(
        &banner,
        &ImpositionMode::Tiling {
            tile_w_mm: 250.0,
            tile_h_mm: 250.0,
            overlap_mm: 15.0,
        },
    )
    .expect("Tiling imposition");
    assert!(tiled.pages().len() >= 4);

    let marks = generate_print_marks(&mut sep_doc, 0, &PrintMarksOptions::default())
        .expect("generate print marks");
    assert!(marks.total_objects() > 0);

    // 3. Preflight + Collect for Output
    let pf_report = run_preflight(&sep_doc, &PreflightRules::default());
    assert!(pf_report.to_plain_text().contains("PREFLIGHT REPORT"));

    let collect_dir = tmp.path().join("collect_bundle");
    let collect_rep =
        collect_for_output(&sep_doc, &collect_dir, &CollectOptions::default()).unwrap();
    assert!(collect_rep.total_files_written >= 3);
    assert!(collect_dir.join("manifest.json").is_file());
    assert!(collect_dir.join("preflight.txt").is_file());

    // 4. All 4 Parametric Packaging Dielines + Path Length & Area Meter
    let dieline_kinds = [
        DielineKind::TuckEndBox {
            length_mm: 50.0,
            width_mm: 25.0,
            height_mm: 70.0,
            glue_flap_mm: 10.0,
            tuck_flap_mm: 12.0,
        },
        DielineKind::Envelope {
            width_mm: 160.0,
            height_mm: 110.0,
            flap_mm: 30.0,
        },
        DielineKind::ShoppingBag {
            width_mm: 120.0,
            depth_mm: 60.0,
            height_mm: 160.0,
            fold_mm: 25.0,
        },
        DielineKind::StickerSheet {
            label_w_mm: 40.0,
            label_h_mm: 25.0,
            corner_r_mm: 3.0,
            cols: 3,
            rows: 4,
            gap_mm: 4.0,
        },
    ];
    for kind in &dieline_kinds {
        let mut ddoc = Document::new("Dieline", 450.0, 350.0, ColorMode::Cmyk, 300.0);
        let dres = generate_dieline(&mut ddoc, 0, kind, Point2D::new(10.0, 10.0))
            .expect("generate dieline");
        assert!(!dres.cut_object_ids.is_empty());
        assert!(dres.cut_length_mm > 50.0);
        let metrics = compute_production_metrics(&ddoc, None);
        assert!(metrics.cut_length_mm > 50.0);
        assert!(metrics.bounding_sheet_area_mm2 > 100.0);
    }

    // 5. QR Code + ALL 5 1D Barcodes (EAN-13, EAN-8, UPC-A, Code128, Code39) + bad checksum rejection
    assert_eq!(ean13_check_digit("590123412345").unwrap(), 7);
    assert_eq!(ean8_check_digit("9638507").unwrap(), 4);
    assert_eq!(upca_check_digit("03600029145").unwrap(), 2);

    let qr = generate_qr_code("https://github.com/ahrrfy/coree", &QrCodeOptions::default())
        .expect("generate QR");
    assert!(!qr.dark_modules_path.is_empty());

    let mut bc_doc = Document::new("Barcodes", 200.0, 200.0, ColorMode::Cmyk, 300.0);
    add_qr_code_to_document(
        &mut bc_doc,
        "https://github.com/ahrrfy/coree",
        Point2D::new(10.0, 10.0),
        &QrCodeOptions::default(),
        None,
    )
    .unwrap();
    for (i, (sym, payload)) in [
        (BarcodeSymbology::Ean13, "5901234123457"),
        (BarcodeSymbology::Ean8, "96385074"),
        (BarcodeSymbology::UpcA, "036000291452"),
        (BarcodeSymbology::Code128, "COREE-2026"),
        (BarcodeSymbology::Code39, "STUDIO39"),
    ]
    .into_iter()
    .enumerate()
    {
        let out = generate_barcode(sym, payload, &BarcodeOptions::default()).expect("barcode");
        assert!(!out.bars_path.is_empty());
        add_barcode_to_document(
            &mut bc_doc,
            sym,
            payload,
            Point2D::new(10.0, 45.0 + i as f64 * 28.0),
            &BarcodeOptions::default(),
            None,
        )
        .unwrap();
    }
    // Wrong EAN-13 check digit MUST fail cleanly
    assert!(generate_barcode(
        BarcodeSymbology::Ean13,
        "5901234123450",
        &BarcodeOptions::default()
    )
    .is_err());

    // 6. Print Merge with Bilingual Arabic + English CSV (API + real CLI binary)
    let mut tpl = Document::new("Invite", 100.0, 60.0, ColorMode::Cmyk, 300.0);
    tpl.add_object(
        SceneObject::new(
            ObjectId(0),
            "Guest Line",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "Guest: <<Name>> | الضيف: <<ArabicName>>",
                TextStyle::default(),
            )),
        ),
        None,
    )
    .unwrap();
    let csv = "Name,ArabicName\nAhmed,أحمد\nLayla,ليلى\n";
    let table = PrintMergeTable::from_csv(csv).unwrap();
    let merged = apply_print_merge(&tpl, &table, &PrintMergeOptions::default()).unwrap();
    assert_eq!(merged.pages().len(), 2);
    let all_merged_text: String = merged
        .objects()
        .values()
        .filter_map(|o| match &o.payload {
            NodePayload::ArtisticText(t) => Some(t.plain_text()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all_merged_text.contains("Ahmed") && all_merged_text.contains("أحمد"));
    assert!(all_merged_text.contains("Layla") && all_merged_text.contains("ليلى"));

    // 7. Find & Replace (Exact Spot vs Process separation + Text Replace)
    let q_spot = FindQuery {
        outline_color: Some(ColorMatch::exact(Color::spot(
            "CutContour",
            1.0,
            [0.0, 1.0, 0.0, 0.0],
        ))),
        ..FindQuery::default()
    };
    let spot_hits = find_objects(&sep_doc, &q_spot);
    assert!(!spot_hits.is_empty(), "must find CutContour spot object");

    let q_text = FindQuery {
        text_pattern: Some((
            "Ahmed".to_owned(),
            TextMatchMode::Contains {
                case_sensitive: true,
            },
        )),
        ..FindQuery::default()
    };
    let mut merged_mut = merged.clone();
    let rep = find_and_replace(
        &mut merged_mut,
        &q_text,
        &ReplaceAction::ReplaceText {
            find: "Ahmed".to_owned(),
            replace_with: "Tariq".to_owned(),
            case_sensitive: true,
        },
    );
    assert_eq!(rep.replacements_applied, 1);

    // 8. Export DXF, PLT (HPGL), PDF (CMYK + Crop Marks), EPS, SVG + real CLI convert
    let dxf = export_dxf_to_string(&sep_doc, 0).expect("export DXF");
    assert!(dxf.contains("SECTION") && dxf.contains("EOF"));
    let plt = export_plt_to_string(&sep_doc, 0).expect("export PLT");
    assert!(plt.starts_with("IN;"));
    let pdf = export_pdf_to_bytes(
        &sep_doc,
        &PdfExportOptions {
            color_space: PdfColorSpace::DeviceCmyk,
            crop_marks: true,
            registration_marks: true,
            ..PdfExportOptions::default()
        },
    )
    .expect("export CMYK PDF");
    assert!(pdf.starts_with(b"%PDF-1.7"));
    let eps = export_eps_to_string(&sep_doc, 0).expect("export EPS");
    assert!(eps.starts_with("%!PS-Adobe"));
    let svg = export_svg_to_string(&sep_doc, 0).expect("export SVG");
    assert!(svg.contains("<svg") && svg.contains("</svg>"));

    let doc_file = tmp.path().join("prod.coree");
    save_coree(&sep_doc, &doc_file).unwrap();
    let pdf_cli = tmp.path().join("cli_cmyk.pdf");
    run_coree_bin(&[
        "convert",
        &doc_file.to_string_lossy(),
        &pdf_cli.to_string_lossy(),
        "--cmyk",
        "--crop-marks",
    ]);
    assert!(std::fs::read(&pdf_cli).unwrap().starts_with(b"%PDF-1.7"));

    // 9. Rhai Macro Recording + Sandboxed Execution + Infinite-Loop Sandbox Abort
    let mut macro_doc = Document::new("Macro Target", 120.0, 120.0, ColorMode::Cmyk, 300.0);
    let mut history = CommandHistory::new(64);
    history.start_macro_recording();
    let rect = SceneObject::new(
        ObjectId(0),
        "Macro Box",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 30.0,
            height: 20.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::rgb(0.2, 0.6, 0.9)));
    history
        .execute(&mut macro_doc, CommandOp::create_object(rect, None))
        .unwrap();
    let recorded_script = history.stop_macro_to_rhai();
    assert!(!recorded_script.trim().is_empty());

    let exec_res = run_macro(
        &mut macro_doc,
        r#"let ids = all_object_ids(); log("objs=" + ids.len());"#,
        &BTreeMap::new(),
        &MacroSandboxConfig::default(),
    )
    .expect("run sandboxed Rhai script");
    assert!(exec_res.logs.iter().any(|l| l.contains("objs=1")));

    // Infinite loop MUST be terminated safely by Rhai op-count sandbox limit
    let loop_res = run_macro(
        &mut macro_doc,
        "let x = 0; while true { x += 1; }",
        &BTreeMap::new(),
        &MacroSandboxConfig {
            max_operations: 5_000,
            ..MacroSandboxConfig::default()
        },
    );
    assert!(
        loop_res.is_err(),
        "infinite loop Rhai macro MUST be aborted by sandbox op limit"
    );

    // Real CLI `coree run-macro` built-in macro execution
    let macro_out = tmp.path().join("macro_cmyk.coree");
    let m_msg = run_coree_bin(&[
        "run-macro",
        &doc_file.to_string_lossy(),
        "recolor-cmyk",
        &macro_out.to_string_lossy(),
    ]);
    assert!(m_msg.contains("Macro executed"));
    assert!(load_coree(&macro_out).is_ok());
}

// =============================================================================
// 7. F18 — SQLite WAL Crash Recovery, Post-Save Rotation & Undo Checkpointing
// =============================================================================

#[test]
fn e2e_f18_sqlite_wal_crash_recovery_post_save_and_undo_checkpoint() {
    let tmp = TempDirGuard::new("f18-wal");
    let db_path = tmp.path().join("e2e_recovery.db");
    let save_path = tmp.path().join("saved_project.coree");

    // Phase 1: Create 3 objects -> simulated crash -> verify exact replay restores all 3
    let doc_id_1: String;
    {
        let db = StorageDb::open(&db_path).expect("open WAL db");
        assert_eq!(db.journal_mode().unwrap().to_ascii_lowercase(), "wal");
        let mut app = StudioApp::new_with_storage(db);
        doc_id_1 = app.active_tab().unwrap().document.id().to_string();
        app.select_tool(ToolId::Rectangle);
        app.commit_drawn_box(Rect2D::from_xywh(10.0, 10.0, 30.0, 20.0));
        app.commit_drawn_box(Rect2D::from_xywh(50.0, 10.0, 30.0, 20.0));
        app.commit_drawn_box(Rect2D::from_xywh(90.0, 10.0, 30.0, 20.0));
        assert_eq!(app.active_tab().unwrap().document.objects().len(), 3);
        // Simulate sudden crash (drop without shutdown_cleanly)
    }
    {
        let db = StorageDb::open(&db_path).expect("reopen WAL db");
        let mut app = StudioApp::new_with_storage(db);
        let sess = app
            .unclean_sessions
            .iter()
            .find(|s| s.document_id == doc_id_1)
            .cloned()
            .expect("unclean session 1 detected");
        app.recover_unclean_session(&sess.document_id, &sess.session_id)
            .expect("recover session 1");
        assert_eq!(app.active_tab().unwrap().document.objects().len(), 3);

        // Phase 2: Save -> Post-save edit (rotates WAL session) -> Undo -> Redo -> Post-undo crash
        app.save_active_as(&save_path).expect("save active");
        app.select_tool(ToolId::Ellipse);
        app.commit_drawn_box(Rect2D::from_xywh(10.0, 40.0, 25.0, 25.0)); // 4th object
        app.commit_drawn_box(Rect2D::from_xywh(45.0, 40.0, 25.0, 25.0)); // 5th object
        assert_eq!(app.active_tab().unwrap().document.objects().len(), 5);
        app.undo().expect("undo 5th object -> checkpoint snapshot");
        assert_eq!(app.active_tab().unwrap().document.objects().len(), 4);
        // Crash right after post-save edit + undo!
    }
    {
        let db = StorageDb::open(&db_path).expect("reopen WAL db phase 3");
        let mut app = StudioApp::new_with_storage(db);
        let sess = app
            .unclean_sessions
            .iter()
            .find(|s| s.document_id == doc_id_1)
            .cloned()
            .expect("post-save+undo unclean session detected");
        app.recover_unclean_session(&sess.document_id, &sess.session_id)
            .expect("recover post-save+undo session");
        assert_eq!(
            app.active_tab().unwrap().document.objects().len(),
            4,
            "must preserve post-save 4th object AND NOT resurrect undone 5th object"
        );

        // Clean shutdown clears remaining sessions
        app.shutdown_cleanly();
    }
    {
        let db = StorageDb::open(&db_path).expect("reopen after clean shutdown");
        let app = StudioApp::new_with_storage(db);
        assert!(
            app.unclean_sessions
                .iter()
                .all(|s| s.document_id != doc_id_1),
            "clean shutdown must leave zero unclean sessions for document"
        );
    }
}

// =============================================================================
// 8. Rich Showcase Document Builders & Deterministic Visual Rendering Check
// =============================================================================

fn build_showcase_lenses_vectors_doc() -> Document {
    let mut doc = Document::new(
        "coree Showcase — Vectors, Mesh Fill & Live Lenses",
        240.0,
        160.0,
        ColorMode::Rgb,
        150.0,
    );

    // Top studio header bar (Neutral Graphite #111318 + Electric Cobalt #3ea6ff + Studio Amber #f6ad3c)
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Header Bar",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 240.0,
                height: 22.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgb(0.067, 0.075, 0.094)))
        .with_outline(OutlineStyle::none()),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Cobalt Accent Strip",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 160.0,
                height: 2.2,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgb(0.243, 0.651, 1.0)))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(0.0, 22.0)),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Amber Accent Strip",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 80.0,
                height: 2.2,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgb(0.965, 0.678, 0.235)))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(160.0, 22.0)),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Header Title",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "coree  •  Vector Shapes, Mesh Fill & 5 Live Optical Lenses",
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 15.0,
                    bold: true,
                    fill: Color::rgb(0.945, 0.957, 0.976),
                    ..TextStyle::default()
                },
            )),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(10.0, 14.5)),
        None,
    )
    .unwrap();

    // Panel 1 (top-left): 4x4 Interactive Mesh Fill card
    let mut mesh_nodes = Vec::new();
    let mesh_palette = [
        [
            Color::rgb(0.10, 0.24, 0.58),
            Color::rgb(0.24, 0.65, 1.0),
            Color::rgb(0.22, 0.85, 0.66),
            Color::rgb(0.96, 0.68, 0.24),
        ],
        [
            Color::rgb(0.20, 0.38, 0.82),
            Color::rgb(0.45, 0.82, 1.0),
            Color::rgb(0.65, 0.92, 0.55),
            Color::rgb(1.0, 0.52, 0.28),
        ],
        [
            Color::rgb(0.48, 0.22, 0.72),
            Color::rgb(0.86, 0.32, 0.68),
            Color::rgb(1.0, 0.44, 0.48),
            Color::rgb(0.98, 0.78, 0.22),
        ],
        [
            Color::rgb(0.12, 0.14, 0.24),
            Color::rgb(0.38, 0.18, 0.52),
            Color::rgb(0.82, 0.24, 0.52),
            Color::rgb(0.95, 0.42, 0.34),
        ],
    ];
    for (r, row_cols) in mesh_palette.iter().enumerate() {
        for (c, col) in row_cols.iter().enumerate() {
            mesh_nodes.push(MeshNode {
                row: r,
                col: c,
                pos: Point2D::new(c as f64 * 22.0, r as f64 * 14.0),
                color: col.clone(),
            });
        }
    }
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Mesh Fill Card",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 66.0,
                height: 42.0,
                corner_radii: [4.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::MeshFill {
            rows: 4,
            cols: 4,
            nodes: mesh_nodes,
        })
        .with_outline(OutlineStyle::solid(Color::rgb(0.18, 0.20, 0.27), 0.5))
        .with_transform(Affine2D::translate(12.0, 32.0)),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Mesh Label",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "4×4 Bezier Mesh Fill",
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 9.0,
                    bold: true,
                    fill: Color::rgb(0.15, 0.18, 0.24),
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(Affine2D::translate(14.0, 81.0)),
        None,
    )
    .unwrap();

    // Panel 2 (top-center): Fountain gradients + DropShadow Star
    let mut grad_card = SceneObject::new(
        ObjectId(0),
        "Linear Gradient Card",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 66.0,
            height: 42.0,
            corner_radii: [5.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::LinearGradient {
        start: Point2D::new(0.0, 0.0),
        end: Point2D::new(66.0, 42.0),
        stops: vec![
            GradientStop::new(0.0, Color::rgb(0.11, 0.16, 0.32)),
            GradientStop::new(0.5, Color::rgb(0.243, 0.651, 1.0)),
            GradientStop::new(1.0, Color::rgb(0.22, 0.85, 0.66)),
        ],
    })
    .with_outline(OutlineStyle::solid(Color::rgb(0.18, 0.20, 0.27), 0.5))
    .with_transform(Affine2D::translate(88.0, 32.0));
    grad_card.effects.push(Effect::DropShadow {
        dx_mm: 2.0,
        dy_mm: 2.0,
        blur_mm: 2.2,
        opacity: 0.45,
        color: Color::rgb(0.05, 0.07, 0.12),
        feather_direction: FeatherDirection::Outside,
    });
    doc.add_object(grad_card, None).unwrap();

    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Star Emblem",
            NodePayload::Shape(ParametricShape::Star {
                points: 8,
                outer_radius: 15.0,
                inner_ratio: 0.46,
                complex: false,
            }),
        )
        .with_fill(Fill::RadialGradient {
            center: Point2D::ZERO,
            radius: 15.0,
            focal: Point2D::new(-4.0, -4.0),
            stops: vec![
                GradientStop::new(0.0, Color::rgb(1.0, 0.94, 0.65)),
                GradientStop::new(1.0, Color::rgb(0.965, 0.678, 0.235)),
            ],
        })
        .with_outline(OutlineStyle::solid(Color::rgb(0.11, 0.13, 0.18), 0.45))
        .with_transform(Affine2D::translate(121.0, 53.0)),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Grad Label",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "Fountain Gradients + Drop Shadow",
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 9.0,
                    bold: true,
                    fill: Color::rgb(0.15, 0.18, 0.24),
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(Affine2D::translate(88.0, 81.0)),
        None,
    )
    .unwrap();

    // Panel 3 (top-right): Conical Gradient + Contour Vector Badge
    let mut contour_hex = SceneObject::new(
        ObjectId(0),
        "Contour Polygon",
        NodePayload::Shape(ParametricShape::Polygon {
            sides: 6,
            radius_x: 14.0,
            radius_y: 14.0,
        }),
    )
    .with_fill(Fill::ConicalGradient {
        center: Point2D::ZERO,
        angle_deg: 25.0,
        stops: vec![
            GradientStop::new(0.0, Color::rgb(0.243, 0.651, 1.0)),
            GradientStop::new(0.5, Color::rgb(1.0, 0.40, 0.77)),
            GradientStop::new(1.0, Color::rgb(0.965, 0.678, 0.235)),
        ],
    })
    .with_outline(OutlineStyle::solid(Color::rgb(0.11, 0.13, 0.18), 0.5))
    .with_transform(Affine2D::translate(196.0, 53.0));
    contour_hex.effects.push(Effect::Contour {
        mode: ContourMode::Outside,
        steps: 3,
        offset_mm: 2.0,
        fill_color: Color::rgb(0.84, 0.92, 1.0),
        outline_color: Color::rgb(0.243, 0.651, 1.0),
    });
    doc.add_object(contour_hex, None).unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Contour Label",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "Conical + Stepped Contour",
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 9.0,
                    bold: true,
                    fill: Color::rgb(0.15, 0.18, 0.24),
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(Affine2D::translate(166.0, 81.0)),
        None,
    )
    .unwrap();

    // Bottom Showcase Band: High-contrast vector stripe & checkerboard backdrop + 5 Live Lenses!
    let band_y = 92.0;
    let band_h = 46.0;
    let stripe_colors = [
        Color::rgb(0.11, 0.16, 0.28),
        Color::rgb(0.243, 0.651, 1.0),
        Color::rgb(0.965, 0.678, 0.235),
        Color::rgb(0.22, 0.85, 0.66),
        Color::rgb(1.0, 0.40, 0.77),
        Color::rgb(0.94, 0.28, 0.34),
    ];
    for idx in 0..24 {
        let col = &stripe_colors[idx % stripe_colors.len()];
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                format!("Backdrop Stripe {idx}"),
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 9.0,
                    height: band_h,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(col.clone()))
            .with_outline(OutlineStyle::none())
            .with_transform(Affine2D::translate(12.0 + idx as f64 * 9.0, band_y)),
            None,
        )
        .unwrap();
    }
    // Horizontal cross-bars so geometric distortion (FishEye/Magnify) pops immediately
    for row in 0..3 {
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                format!("Crossbar {row}"),
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 216.0,
                    height: 3.5,
                    corner_radii: [0.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::rgb(0.97, 0.98, 1.0)))
            .with_outline(OutlineStyle::solid(Color::rgb(0.1, 0.12, 0.18), 0.35))
            .with_transform(Affine2D::translate(12.0, band_y + 8.0 + row as f64 * 13.0)),
            None,
        )
        .unwrap();
    }

    // Place 5 circular live Lenses across the backdrop band
    let lenses: [(&str, LensKind); 5] = [
        ("FishEye", LensKind::FishEye { rate_pct: 200.0 }),
        ("Magnify 2.2×", LensKind::Magnify { amount: 2.2 }),
        ("Invert", LensKind::Invert),
        (
            "HeatMap",
            LensKind::HeatMap {
                palette_rotation_deg: 20.0,
            },
        ),
        (
            "Wireframe",
            LensKind::Wireframe {
                override_outline: Some(Color::rgb(0.10, 0.46, 0.92)),
                override_fill: Some(Color::rgb(0.97, 0.98, 1.0)),
            },
        ),
    ];
    for (i, (title, kind)) in lenses.into_iter().enumerate() {
        let cx = 33.0 + i as f64 * 43.5;
        let cy = band_y + band_h * 0.5;
        let mut lens_obj = SceneObject::new(
            ObjectId(0),
            format!("Lens {title}"),
            NodePayload::Shape(ParametricShape::Ellipse {
                rx: 17.5,
                ry: 17.5,
                mode: EllipseMode::Full,
                start_deg: 0.0,
                end_deg: 360.0,
            }),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::solid(Color::rgb(0.07, 0.09, 0.14), 1.0))
        .with_transform(Affine2D::translate(cx, cy));
        lens_obj.lens = Some(LensConfig::new(kind));
        doc.add_object(lens_obj, None).unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                format!("Label {title}"),
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    title,
                    TextStyle {
                        font_family: "DejaVu Sans".to_owned(),
                        font_size_pt: 9.5,
                        bold: true,
                        fill: Color::rgb(0.11, 0.13, 0.18),
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(cx - 14.0, 149.0)),
            None,
        )
        .unwrap();
    }

    doc
}

fn build_showcase_arabic_packaging_doc() -> Document {
    let mut doc = Document::new(
        "coree Showcase — Arabic RTL & Parametric Packaging",
        280.0,
        200.0,
        ColorMode::Cmyk,
        150.0,
    );

    // Header banner
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Packaging Header",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 280.0,
                height: 28.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgb(0.067, 0.075, 0.094)))
        .with_outline(OutlineStyle::none()),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Header Accent",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 280.0,
                height: 2.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::spot(
            "CutContour",
            1.0,
            [0.0, 1.0, 0.0, 0.0],
        )))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(0.0, 28.0)),
        None,
    )
    .unwrap();

    // Arabic RTL + Kashida headline
    let ar_heading = apply_kashida_elongation("استوديو كوري للتغليف والطباعة الاحترافية", 1);
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Arabic RTL Title",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                ar_heading,
                TextStyle {
                    font_family: "Noto Sans Arabic".to_owned(),
                    font_size_pt: 16.0,
                    bold: true,
                    direction: TextDirection::Rtl,
                    align: TextAlign::ForceJustifyKashida,
                    kashida_stretch: 0.55,
                    fill: Color::rgb(0.965, 0.678, 0.235),
                    ..TextStyle::default()
                },
            )),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(16.0, 12.5)),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "English Subtitle",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "Parametric Tuck-End Box Dieline (CutContour + Crease)  •  Vector QR & EAN-13  •  Live Dimensions",
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 9.5,
                    fill: Color::rgb(0.92, 0.94, 0.98),
                    ..TextStyle::default()
                },
            )),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(16.0, 22.5)),
        None,
    )
    .unwrap();

    // Parametric Tuck-End Box Dieline (CutContour solid magenta + Crease dashed orange)
    let box_origin = Point2D::new(26.0, 40.0);
    let dieline = generate_dieline(
        &mut doc,
        0,
        &DielineKind::TuckEndBox {
            length_mm: 46.0,
            width_mm: 24.0,
            height_mm: 62.0,
            glue_flap_mm: 10.0,
            tuck_flap_mm: 12.0,
        },
        box_origin,
    )
    .expect("generate TuckEndBox dieline");

    // Artwork printed inside front face panel of box
    let front_x = box_origin.x + 10.0 + 2.5;
    let front_y = box_origin.y + 12.0 + 24.0 + 3.0;
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Carton Brand Panel",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 41.0,
                height: 56.0,
                corner_radii: [2.5; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::LinearGradient {
            start: Point2D::ZERO,
            end: Point2D::new(41.0, 56.0),
            stops: vec![
                GradientStop::new(0.0, Color::rgb(0.09, 0.14, 0.26)),
                GradientStop::new(1.0, Color::rgb(0.18, 0.48, 0.85)),
            ],
        })
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(front_x, front_y)),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Carton Arabic Label",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                apply_kashida_elongation("منتج فاخر", 1),
                TextStyle {
                    font_family: "Noto Sans Arabic".to_owned(),
                    font_size_pt: 13.5,
                    bold: true,
                    direction: TextDirection::Rtl,
                    fill: Color::rgb(0.965, 0.678, 0.235),
                    ..TextStyle::default()
                },
            )),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(front_x + 5.0, front_y + 24.0)),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Carton Brand Mark",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "coree PACK",
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 10.5,
                    bold: true,
                    fill: Color::rgb(0.95, 0.97, 1.0),
                    ..TextStyle::default()
                },
            )),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(front_x + 5.5, front_y + 42.0)),
        None,
    )
    .unwrap();

    // Live Horizontal & Vertical Dimensions positioned cleanly OUTSIDE the flat dieline
    let fb = dieline.flat_bounds;
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Blank Width Dimension",
            NodePayload::Dimension(DimensionObject::linear(
                DimensionKind::Horizontal,
                Point2D::new(fb.min_x, fb.max_y + 2.0),
                Point2D::new(fb.max_x, fb.max_y + 2.0),
                -10.0,
            )),
        ),
        None,
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Blank Height Dimension",
            NodePayload::Dimension(DimensionObject::linear(
                DimensionKind::Vertical,
                Point2D::new(fb.min_x - 2.0, fb.min_y),
                Point2D::new(fb.min_x - 2.0, fb.max_y),
                11.0,
            )),
        ),
        None,
    )
    .unwrap();

    // Right column card: QR Code + EAN-13 Barcode + Production Metrics summary
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Spec Card",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 82.0,
                height: 146.0,
                corner_radii: [4.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgb(0.96, 0.97, 0.99)))
        .with_outline(OutlineStyle::solid(Color::rgb(0.76, 0.80, 0.88), 0.4))
        .with_transform(Affine2D::translate(188.0, 42.0)),
        None,
    )
    .unwrap();

    add_qr_code_to_document(
        &mut doc,
        "https://github.com/ahrrfy/coree",
        Point2D::new(209.0, 50.0),
        &QrCodeOptions {
            size_mm: 40.0,
            ec_level: QrErrorCorrection::M,
            ..QrCodeOptions::default()
        },
        None,
    )
    .unwrap();

    add_barcode_to_document(
        &mut doc,
        BarcodeSymbology::Ean13,
        "5901234123457",
        Point2D::new(199.0, 102.0),
        &BarcodeOptions {
            module_width_mm: 0.52,
            bar_height_mm: 24.0,
            ..BarcodeOptions::default()
        },
        None,
    )
    .unwrap();

    let metrics = compute_production_metrics(&doc, None);
    let summary = format!(
        "Cut: {:.1} mm  |  Crease: {:.1} mm\nBlank: {:.0}×{:.0} mm  •  Spot: CutContour",
        dieline.cut_length_mm,
        dieline.crease_length_mm,
        fb.width(),
        fb.height(),
    );
    assert!(metrics.cut_length_mm > 100.0);
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Metrics Caption",
            NodePayload::ParagraphText(ParagraphText::new(
                summary,
                Vec2D::new(74.0, 32.0),
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 8.5,
                    fill: Color::rgb(0.12, 0.15, 0.22),
                    ..TextStyle::default()
                },
            )),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(193.0, 145.0)),
        None,
    )
    .unwrap();

    doc
}

fn build_showcase_multipage_doc() -> Document {
    let mut doc = Document::new(
        "coree Multipage Studio",
        148.0,
        105.0,
        ColorMode::Cmyk,
        150.0,
    );
    doc.set_multipage_view(true);
    let p1_id = doc.pages()[0].id;
    doc.rename_page(p1_id, "Page 1 — Cover").unwrap();
    let p2_id = doc.add_page("Page 2 — Specs", 148.0, 105.0, 0.0);
    let p3_id = doc.add_page("Page 3 — Cut & QR", 148.0, 105.0, 0.0);

    // Master Layer across ALL pages: bottom cobalt strip + studio footer
    let master_layer = doc
        .add_layer(p1_id, "Master Footer", false, MasterLayerScope::AllPages)
        .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Master Strip",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 148.0,
                height: 7.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgb(0.067, 0.075, 0.094)))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(0.0, 98.0)),
        Some(master_layer),
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Master Line",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 148.0,
                height: 1.5,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgb(0.243, 0.651, 1.0)))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(0.0, 96.5)),
        Some(master_layer),
    )
    .unwrap();

    // Page 1 — Dark Graphite Cover
    let p1_layer = doc.pages()[0].layers[0].id;
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Cover BG",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 148.0,
                height: 96.5,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::LinearGradient {
            start: Point2D::ZERO,
            end: Point2D::new(148.0, 96.5),
            stops: vec![
                GradientStop::new(0.0, Color::rgb(0.067, 0.075, 0.094)),
                GradientStop::new(1.0, Color::rgb(0.14, 0.18, 0.28)),
            ],
        })
        .with_outline(OutlineStyle::none()),
        Some(p1_layer),
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Cover Emblem Outer",
            NodePayload::Shape(ParametricShape::Ellipse {
                rx: 26.0,
                ry: 26.0,
                mode: EllipseMode::Full,
                start_deg: 0.0,
                end_deg: 360.0,
            }),
        )
        .with_fill(Fill::None)
        .with_outline(OutlineStyle::solid(Color::rgb(0.243, 0.651, 1.0), 3.0))
        .with_transform(Affine2D::translate(110.0, 48.0)),
        Some(p1_layer),
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Cover Emblem Inner",
            NodePayload::Shape(ParametricShape::Star {
                points: 6,
                outer_radius: 15.0,
                inner_ratio: 0.5,
                complex: false,
            }),
        )
        .with_fill(Fill::Solid(Color::rgb(0.965, 0.678, 0.235)))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(110.0, 48.0)),
        Some(p1_layer),
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Cover Title",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "coree Studio",
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 22.0,
                    bold: true,
                    fill: Color::rgb(0.95, 0.96, 0.98),
                    ..TextStyle::default()
                },
            )),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(14.0, 42.0)),
        Some(p1_layer),
    )
    .unwrap();
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Cover Sub",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "Page 1 • Multipage View",
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 11.5,
                    bold: true,
                    fill: Color::rgb(0.35, 0.72, 1.0),
                    ..TextStyle::default()
                },
            )),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(14.0, 58.0)),
        Some(p1_layer),
    )
    .unwrap();

    // Page 2 — Vector Table & Graphic Specs
    let p2_layer = doc.pages().iter().find(|p| p.id == p2_id).unwrap().layers[0].id;
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "P2 Header",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "Page 2 • Vector Table & Color Swatches",
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 12.0,
                    bold: true,
                    fill: Color::rgb(0.09, 0.12, 0.18),
                    ..TextStyle::default()
                },
            )),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(10.0, 16.0)),
        Some(p2_layer),
    )
    .unwrap();
    let mut tbl = VectorTable::new(4, 3, 42.0, 12.0);
    set_table_cell(&mut tbl, 0, 0, "Channel");
    set_table_cell(&mut tbl, 0, 1, "Model");
    set_table_cell(&mut tbl, 0, 2, "Status");
    set_table_cell(&mut tbl, 1, 0, "Process CMYK");
    set_table_cell(&mut tbl, 1, 1, "300 DPI");
    set_table_cell(&mut tbl, 1, 2, "Press Ready");
    set_table_cell(&mut tbl, 2, 0, "CutContour");
    set_table_cell(&mut tbl, 2, 1, "Spot Hairline");
    set_table_cell(&mut tbl, 2, 2, "Overprint");
    set_table_cell(&mut tbl, 3, 0, "Arabic RTL");
    set_table_cell(&mut tbl, 3, 1, "Kashida");
    set_table_cell(&mut tbl, 3, 2, "Verified");
    doc.add_object(
        SceneObject::new(ObjectId(0), "Spec Table", NodePayload::Table(tbl))
            .with_transform(Affine2D::translate(10.0, 24.0)),
        Some(p2_layer),
    )
    .unwrap();
    for (i, c) in [
        Color::CYAN,
        Color::MAGENTA,
        Color::YELLOW,
        Color::KEY_BLACK,
        Color::spot("CutContour", 1.0, [0.0, 1.0, 0.0, 0.0]),
    ]
    .into_iter()
    .enumerate()
    {
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                format!("Swatch {i}"),
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 22.0,
                    height: 11.0,
                    corner_radii: [2.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(c))
            .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.3))
            .with_transform(Affine2D::translate(10.0 + i as f64 * 26.0, 78.0)),
            Some(p2_layer),
        )
        .unwrap();
    }

    // Page 3 — Kiss-Cut Sticker Sheet & Vector QR Code
    let p3_layer = doc.pages().iter().find(|p| p.id == p3_id).unwrap().layers[0].id;
    doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "P3 Header",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "Page 3 • Sticker Dieline & QR",
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 12.0,
                    bold: true,
                    fill: Color::rgb(0.09, 0.12, 0.18),
                    ..TextStyle::default()
                },
            )),
        )
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(10.0, 16.0)),
        Some(p3_layer),
    )
    .unwrap();
    generate_dieline(
        &mut doc,
        2,
        &DielineKind::StickerSheet {
            label_w_mm: 36.0,
            label_h_mm: 24.0,
            corner_r_mm: 4.0,
            cols: 2,
            rows: 2,
            gap_mm: 6.0,
        },
        Point2D::new(12.0, 26.0),
    )
    .unwrap();
    add_qr_code_to_document(
        &mut doc,
        "https://github.com/ahrrfy/coree",
        Point2D::new(98.0, 32.0),
        &QrCodeOptions {
            size_mm: 38.0,
            ..QrCodeOptions::default()
        },
        Some(p3_layer),
    )
    .unwrap();

    doc
}

#[test]
fn e2e_deterministic_rendering_byte_and_pixel_identical() {
    let tmp = TempDirGuard::new("determinism");

    let lenses_doc = build_showcase_lenses_vectors_doc();
    let packaging_doc = build_showcase_arabic_packaging_doc();
    let multipage_doc = build_showcase_multipage_doc();

    // Optional hook for release showcase PNG generation script/step (never runs during normal cargo test)
    if let Ok(out_dir) = std::env::var("COREE_EXPORT_SHOWCASE_DIR") {
        let dir = PathBuf::from(out_dir);
        std::fs::create_dir_all(&dir).expect("create showcase export dir");
        save_coree(&lenses_doc, &dir.join("showcase_lenses_vectors.coree")).unwrap();
        save_coree(&packaging_doc, &dir.join("showcase_arabic_packaging.coree")).unwrap();
        save_coree(&multipage_doc, &dir.join("showcase_multipage.coree")).unwrap();
    }

    // 1. Render lenses_doc TWICE in-process -> assert 100% pixel-identical RGBA & PNG bytes
    let run_a = render_page_to_rgba(&lenses_doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    let run_b = render_page_to_rgba(&lenses_doc, 0, 96.0, ViewMode::Enhanced).unwrap();
    assert_eq!((run_a.width, run_a.height), (run_b.width, run_b.height));
    assert_eq!(
        run_a.rgba, run_b.rgba,
        "two independent renders of the same rich document MUST be 100% pixel-identical"
    );
    assert_eq!(
        run_a.encode_png().unwrap(),
        run_b.encode_png().unwrap(),
        "encoded PNG bytes MUST be 100% byte-for-byte identical"
    );

    // 2. Render via TWO independent invocations of the real `coree` CLI binary -> byte-identical!
    let doc_path = tmp.path().join("det_lenses.coree");
    save_coree(&lenses_doc, &doc_path).unwrap();
    let png_1 = tmp.path().join("det_run1.png");
    let png_2 = tmp.path().join("det_run2.png");
    run_coree_bin(&[
        "render",
        &doc_path.to_string_lossy(),
        &png_1.to_string_lossy(),
        "--dpi",
        "96",
    ]);
    run_coree_bin(&[
        "render",
        &doc_path.to_string_lossy(),
        &png_2.to_string_lossy(),
        "--dpi",
        "96",
    ]);
    let b1 = std::fs::read(&png_1).unwrap();
    let b2 = std::fs::read(&png_2).unwrap();
    assert_eq!(
        b1, b2,
        "two independent CLI renders must produce byte-for-byte identical PNG output"
    );

    // 3. Also verify packaging_doc renders cleanly and deterministically
    let pkg_a = render_page_to_rgba(&packaging_doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    let pkg_b = render_page_to_rgba(&packaging_doc, 0, 72.0, ViewMode::Enhanced).unwrap();
    assert_eq!(pkg_a.rgba, pkg_b.rgba);
}
