// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

use coree_cdr::{
    detect_format_with_limits, extract_cdr_preview_png, import_cdr_bytes_with_limits,
    inspect_cdr_bytes_with_limits, CdrFormat, CodecLimits,
};
use coree_core::{Color, Fill, NodePayload, TextDirection};

const FIXTURE_V03: &[u8] = include_bytes!("fixtures_real_cdr/cdr_v03_16bit.cdr");
const FIXTURE_V05: &[u8] = include_bytes!("fixtures_real_cdr/cdr_v05_16bit.cdr");
const FIXTURE_V09_ARABIC: &[u8] = include_bytes!("fixtures_real_cdr/cdr_v09_cp1256_arabic.cdr");
const FIXTURE_V12_CMPR: &[u8] = include_bytes!("fixtures_real_cdr/cdr_v12_riff_cmpr.cdr");
const FIXTURE_V14_X4: &[u8] = include_bytes!("fixtures_real_cdr/cdr_v14_x4_opc.cdr");
const FIXTURE_V16_X6: &[u8] = include_bytes!("fixtures_real_cdr/cdr_v16_x6_zcf.cdr");
const FIXTURE_V25_ZCF: &[u8] = include_bytes!("fixtures_real_cdr/cdr_v25_2025_zcf.cdr");

#[test]
fn all_real_cdr_fixtures_are_binary_clean_and_pass_format_detection_and_inspection() {
    let limits = CodecLimits::default();
    let fixtures: [(&str, &[u8]); 7] = [
        ("v03", FIXTURE_V03),
        ("v05", FIXTURE_V05),
        ("v09", FIXTURE_V09_ARABIC),
        ("v12", FIXTURE_V12_CMPR),
        ("v14", FIXTURE_V14_X4),
        ("v16", FIXTURE_V16_X6),
        ("v25", FIXTURE_V25_ZCF),
    ];

    for (label, bytes) in fixtures {
        let head = &bytes[..bytes.len().min(4096)];
        assert!(
            head.contains(&0u8),
            "fixture {label} must contain NUL bytes in first 4096 bytes"
        );
        let fmt = detect_format_with_limits(bytes, &limits)
            .unwrap_or_else(|e| panic!("detect_format failed on {label}: {e}"));
        let report = inspect_cdr_bytes_with_limits(bytes, &limits)
            .unwrap_or_else(|e| panic!("inspect failed on {label}: {e}"));
        assert!(
            report.page_count >= 1 && report.layer_count >= 1,
            "fixture {label} expected >=1 page & layer"
        );
        assert!(
            report.object_count >= 1,
            "fixture {label} expected >=1 object, got {} ({fmt:?})",
            report.object_count
        );
    }
}

#[test]
fn real_cdr_v3_v5_16bit_and_v9_cp1256_arabic_decode_faithfully() {
    let limits = CodecLimits::default();

    let v3 = import_cdr_bytes_with_limits(FIXTURE_V03, &limits).unwrap();
    assert!(matches!(
        v3.detected_format,
        CdrFormat::CdrRiff { version: 300 }
    ));
    assert_eq!(v3.document.object_count(), 1);
    let v3_oid = v3.document.pages()[0].layers[0].objects[0];
    let bb = v3.document.object_bounds(v3_oid).unwrap();
    assert!((bb.width() - 80.0).abs() < 0.25);

    let v5 = import_cdr_bytes_with_limits(FIXTURE_V05, &limits).unwrap();
    assert!(matches!(
        v5.detected_format,
        CdrFormat::CdrRiff { version: 500 }
    ));
    assert_eq!(v5.document.object_count(), 2);

    let v9 = import_cdr_bytes_with_limits(FIXTURE_V09_ARABIC, &limits).unwrap();
    assert!(matches!(
        v9.detected_format,
        CdrFormat::CdrRiff { version: 900 }
    ));
    let txt_oid = v9.document.pages()[0].layers[0].objects[0];
    match &v9.document.object(txt_oid).unwrap().payload {
        NodePayload::ArtisticText(t) => {
            assert_eq!(t.plain_text(), "تصميم كوري");
            assert_eq!(t.direction, TextDirection::Rtl);
        }
        other => panic!("expected ArtisticText in v9 Arabic fixture, got {other:?}"),
    }
    let png = extract_cdr_preview_png(FIXTURE_V09_ARABIC, &limits)
        .unwrap()
        .expect("v9 DISP preview");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
}

#[test]
fn real_cdr_v12_v14_v16_v25_preserve_complex_objects_spot_colors_icc_and_unknowns() {
    let limits = CodecLimits::default();

    let v12 = import_cdr_bytes_with_limits(FIXTURE_V12_CMPR, &limits).unwrap();
    assert_eq!(v12.document.pages()[0].unknown_chunks.len(), 1);
    assert_eq!(v12.document.pages()[0].layers[0].unknown_chunks.len(), 1);
    let v12_objs = &v12.document.pages()[0].layers[0].objects;
    assert!(matches!(
        v12.document.object(v12_objs[1]).unwrap().payload,
        NodePayload::Table(_)
    ));
    let v12_png = extract_cdr_preview_png(FIXTURE_V12_CMPR, &limits).unwrap();
    assert!(v12_png.is_some());

    let v14 = import_cdr_bytes_with_limits(FIXTURE_V14_X4, &limits).unwrap();
    assert_eq!(v14.detected_format, CdrFormat::CdrZipX4);
    assert_eq!(v14.document.object_count(), 1);

    let v16 = import_cdr_bytes_with_limits(FIXTURE_V16_X6, &limits).unwrap();
    assert_eq!(v16.detected_format, CdrFormat::CdrZipX6);
    let v16_oid = v16.document.pages()[0].layers[0].objects[0];
    match &v16.document.object(v16_oid).unwrap().fill {
        Fill::TwoColorPattern {
            fg: Color::Spot { fallback_cmyk, .. },
            pattern_name,
            tile_w,
            ..
        } => {
            assert_ne!(*fallback_cmyk, [0.0, 0.0, 0.0, 1.0]);
            assert_eq!(pattern_name, "CoreeGrid");
            assert!((*tile_w - 6.0).abs() < 0.05);
        }
        other => panic!("expected TwoColorPattern in v16 fixture, got {other:?}"),
    }
    assert!(extract_cdr_preview_png(FIXTURE_V16_X6, &limits)
        .unwrap()
        .is_some());

    let v25 = import_cdr_bytes_with_limits(FIXTURE_V25_ZCF, &limits).unwrap();
    assert_eq!(v25.detected_format, CdrFormat::CdrZipX6);
    assert!(v25.document.custom_icc_profile().is_some());
    assert_eq!(v25.document.symbols().len(), 1);
    assert_eq!(
        v25.document
            .unknown_data_files()
            .get("content/data/custom_unknown.dat"),
        Some(&vec![9u8, 8, 7, 6])
    );
    let v25_objs = &v25.document.pages()[0].layers[0].objects;
    assert!(matches!(
        v25.document.object(v25_objs[0]).unwrap().payload,
        NodePayload::Dimension(_)
    ));
    assert!(matches!(
        v25.document.object(v25_objs[1]).unwrap().payload,
        NodePayload::SymbolInstance(_)
    ));
    assert!(matches!(
        v25.document.object(v25_objs[2]).unwrap().payload,
        NodePayload::Raster(_)
    ));
    assert!(matches!(
        v25.document.object(v25_objs[3]).unwrap().payload,
        NodePayload::PowerClip { .. }
    ));
}
