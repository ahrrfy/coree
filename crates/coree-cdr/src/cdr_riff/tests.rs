// SPDX-License-Identifier: MIT OR Apache-2.0

use super::fild_outl::{
    parse_arrw_list, parse_clrs_list, parse_outl_chunk_with_ctx, parse_real_fild_v16,
    parse_real_outl_v16, parse_vpat_list, rescale_normalized_fill_to_bounds,
};
use super::loda::{encode_loda_args, parse_loda_chunk};
use super::parser::ParseContext;
use super::text_style::parse_font_list;
use super::wavelet_wi::{decode_wi_to_rgba_with_budget, encode_wi_rgba, is_wi_bitmap};
use super::*;
use crate::riff::{RiffEndian, RiffNode};
use coree_core::{
    Affine2D, AnchorPoint, ArrowheadStyle, ArtisticText, BezierPath, Color, ColorMode,
    ConnectorObject, ConnectorStyle, CornerStyle, DimensionKind, DimensionObject, Effect,
    EllipseMode, FeatherDirection, Fill, GradientStop, Layer, LayerId, NodePayload, ObjectId, Page,
    PageId, ParametricShape, Point2D, Rect2D, SceneObject, SymbolDef, TextOnPath, TextStyle,
    UnknownCdrChunk, VectorTable,
};
use std::collections::{HashMap, HashSet};

#[test]
fn normalized_zcf_gradient_rescales_onto_object_local_millimeter_bounds() {
    let mut fill = Fill::LinearGradient {
        start: Point2D::new(0.0, 0.5),
        end: Point2D::new(1.0, 0.5),
        stops: vec![
            GradientStop::new(0.0, Color::rgb(1.0, 0.0, 0.0)),
            GradientStop::new(1.0, Color::rgb(0.0, 0.0, 1.0)),
        ],
    };
    let bb = Rect2D::from_xywh(10.0, 20.0, 120.0, 60.0);
    rescale_normalized_fill_to_bounds(&mut fill, bb);
    match fill {
        Fill::LinearGradient { start, end, .. } => {
            assert!((start.x - 10.0).abs() < 1e-6);
            assert!((start.y - 50.0).abs() < 1e-6);
            assert!((end.x - 130.0).abs() < 1e-6);
            assert!((end.y - 50.0).abs() < 1e-6);
        }
        _ => panic!("expected LinearGradient"),
    }
}

#[test]
fn parse_real_fild_v16_and_outl_v16_handle_subtypes_and_truncated_inputs_safely() {
    let mut chunk = vec![0u8; 64];
    chunk[0..4].copy_from_slice(&7u32.to_le_bytes());
    chunk[4..6].copy_from_slice(&0x0514u16.to_le_bytes());
    chunk[12..14].copy_from_slice(&2u16.to_le_bytes());
    chunk[16..18].copy_from_slice(&1u16.to_le_bytes());
    chunk[20..25].copy_from_slice(&[0x01, 0x0C, 0, 0, 0]);
    chunk[25..27].copy_from_slice(&5u16.to_le_bytes());
    chunk[33..37].copy_from_slice(&[255, 0, 0, 255]);
    chunk[40..45].copy_from_slice(&[0x01, 0x0C, 0, 0, 0]);
    chunk[45..47].copy_from_slice(&5u16.to_le_bytes());
    chunk[53..57].copy_from_slice(&[0, 0, 255, 255]);

    let (id, fill, is_norm) = parse_real_fild_v16(&chunk).unwrap();
    assert_eq!(id, 7);
    assert!(is_norm);
    assert!(matches!(fill, Fill::RadialGradient { .. }));

    let short_outl = vec![0u8; 16];
    assert!(parse_real_outl_v16(&short_outl, None).is_err());
}

#[test]
fn legacy_arrw_vpat_clrs_and_v7_outl_fallbacks_decode_cleanly() {
    let mut doc = Document::new_a4("Legacy CDR Tables");
    let limits = CodecLimits::default();
    let mut budget = limits.max_uncompressed_bytes;
    let zcf = ZcfContext::default();
    let mut ctx = ParseContext {
        limits: &limits,
        budget_remaining: &mut budget,
        endian: RiffEndian::Little,
        version: 1200,
        is_zcf: false,
        is_v3_v5: false,
        doc_page_width_mm: 210.0,
        doc_page_height_mm: 297.0,
        current_page_w_mm: 210.0,
        current_page_h_mm: 297.0,
        zcf: &zcf,
        fills: HashMap::new(),
        normalized_fills: HashSet::new(),
        outlines: HashMap::new(),
        outlines_explicit_none: HashSet::new(),
        styles: HashMap::new(),
        fonts: HashMap::new(),
        bitmaps: HashMap::new(),
        asset_id_remap: HashMap::new(),
        oid_remap: HashMap::new(),
        arrows: HashMap::new(),
        pattern_bitmaps: HashMap::new(),
        vector_patterns: HashMap::new(),
        color_styles: HashMap::new(),
        text_streams: HashMap::new(),
        master_layers: Vec::new(),
        warnings: Vec::new(),
    };

    let mut arrw_bytes = Vec::new();
    arrw_bytes.extend_from_slice(&12u32.to_le_bytes());
    arrw_bytes.extend_from_slice(&4u16.to_le_bytes());
    arrw_bytes.extend_from_slice(&[0u8; 6]);
    parse_arrw_list(
        &[RiffNode::Chunk {
            fourcc: *b"arrw",
            compressed: false,
            data: arrw_bytes,
        }],
        &mut ctx,
    );
    assert_eq!(ctx.arrows.get(&12), Some(&ArrowheadStyle::Diamond));

    let mut vpat_bytes = vec![0u8; 24];
    vpat_bytes[0..4].copy_from_slice(&5u32.to_le_bytes());
    parse_vpat_list(
        &[RiffNode::Chunk {
            fourcc: *b"vpat",
            compressed: false,
            data: vpat_bytes,
        }],
        &mut doc,
        &mut ctx,
    );
    assert!(matches!(
        ctx.vector_patterns.get(&5),
        Some(Fill::VectorPattern { .. })
    ));

    let mut clrs_bytes = vec![0u8; 16];
    clrs_bytes[0..4].copy_from_slice(&9u32.to_le_bytes());
    clrs_bytes[4..6].copy_from_slice(&5u16.to_le_bytes());
    clrs_bytes[12..16].copy_from_slice(&[20, 140, 220, 255]);
    parse_clrs_list(
        &[RiffNode::Chunk {
            fourcc: *b"clrs",
            compressed: false,
            data: clrs_bytes,
        }],
        &mut doc,
        &mut ctx,
    );
    assert!(ctx.color_styles.contains_key(&9));
    assert!(!doc.document_palette().swatches.is_empty());

    let mut leg_outl = vec![0u8; 28];
    leg_outl[0..4].copy_from_slice(&33u32.to_le_bytes());
    leg_outl[4..6].copy_from_slice(&0x0012u16.to_le_bytes());
    leg_outl[12..16].copy_from_slice(&10_000i32.to_le_bytes());
    leg_outl[24..28].copy_from_slice(&12u32.to_le_bytes());
    let (oid, outl, is_none) = parse_outl_chunk_with_ctx(&leg_outl, false, Some(&ctx)).unwrap();
    assert_eq!(oid, 33);
    assert!(!is_none);
    assert!(outl.behind_fill);
    assert_eq!(outl.end_arrow, ArrowheadStyle::Diamond);
}

#[test]
fn loda_20b_reverse_order_roundtrips_and_preserves_unknown_args() {
    let args = vec![
        (0x000Au32, 42u32.to_le_bytes().to_vec()),
        (0x00FFu32, vec![1, 2, 3, 4, 5]),
        (0x0014u32, 99u32.to_le_bytes().to_vec()),
    ];
    let encoded = encode_loda_args(0x01, &args);
    let chunk_len = u32::from_le_bytes(encoded[0..4].try_into().unwrap()) as usize;
    let num_args = u32::from_le_bytes(encoded[4..8].try_into().unwrap()) as usize;
    let args_off = u32::from_le_bytes(encoded[8..12].try_into().unwrap()) as usize;
    let types_off = u32::from_le_bytes(encoded[12..16].try_into().unwrap()) as usize;
    let ctype = u32::from_le_bytes(encoded[16..20].try_into().unwrap());
    assert_eq!(chunk_len, encoded.len());
    assert_eq!(num_args, 3);
    assert_eq!(args_off, 20);
    assert_eq!(ctype, 0x01);
    let t0 = u32::from_le_bytes(encoded[types_off..types_off + 4].try_into().unwrap());
    let t1 = u32::from_le_bytes(encoded[types_off + 4..types_off + 8].try_into().unwrap());
    let t2 = u32::from_le_bytes(encoded[types_off + 8..types_off + 12].try_into().unwrap());
    assert_eq!((t0, t1, t2), (0x0014, 0x00FF, 0x000A));

    let parsed = parse_loda_chunk(&encoded).unwrap();
    assert_eq!(parsed.outline_id, Some(42));
    assert_eq!(parsed.fill_id, Some(99));
    assert_eq!(parsed.unknown_args.len(), 1);
    assert_eq!(parsed.unknown_args[0].0, 0x00FF);
    assert_eq!(parsed.unknown_args[0].1, vec![1, 2, 3, 4, 5]);
}

#[test]
fn wavelet_wi_codec_roundtrips_rgba_and_font_list_decodes() {
    let mut rgba = vec![0u8; 8 * 8 * 4];
    for y in 0..8 {
        for x in 0..8 {
            let idx = (y * 8 + x) * 4;
            rgba[idx] = (x * 30) as u8;
            rgba[idx + 1] = (y * 30) as u8;
            rgba[idx + 2] = 180;
            rgba[idx + 3] = 255;
        }
    }
    let wi = encode_wi_rgba(8, 8, &rgba).unwrap();
    assert!(is_wi_bitmap(&wi));
    let limits = CodecLimits::default();
    let mut budget = limits.max_uncompressed_bytes;
    let (w, h, decoded) = decode_wi_to_rgba_with_budget(&wi, &limits, &mut budget).unwrap();
    assert_eq!((w, h), (8, 8));
    assert_eq!(decoded, rgba);

    let mut font_chunk = Vec::new();
    font_chunk.extend_from_slice(&7u32.to_le_bytes());
    font_chunk.extend_from_slice(&1251u16.to_le_bytes());
    font_chunk.push(204u8);
    font_chunk.extend_from_slice(&400u16.to_le_bytes());
    let family = b"DejaVu Sans";
    font_chunk.extend_from_slice(&(family.len() as u16).to_le_bytes());
    font_chunk.extend_from_slice(family);
    let mut styles = HashMap::new();
    let mut fonts = HashMap::new();
    parse_font_list(
        &[RiffNode::Chunk {
            fourcc: *b"font",
            compressed: false,
            data: font_chunk,
        }],
        &mut styles,
        &mut fonts,
    );
    assert_eq!(fonts.get(&7).unwrap().family, "DejaVu Sans");
    assert_eq!(fonts.get(&7).unwrap().codepage, Some(1251));
}

#[test]
fn non_flattening_objects_and_page_layer_unknown_chunks_roundtrip() {
    let mut doc = Document::new_a4("Complex W2A");
    let mut icc = vec![0u8; 132];
    icc[0..4].copy_from_slice(&(132u32).to_be_bytes());
    icc[16..20].copy_from_slice(b"RGB ");
    icc[20..24].copy_from_slice(b"XYZ ");
    icc[36..40].copy_from_slice(b"acsp");
    doc.set_custom_icc_profile(icc.clone()).unwrap();

    let sym = SymbolDef::new(
        "Badge",
        vec![SceneObject::new(
            ObjectId(2),
            "BadgeRect",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 20.0,
                height: 20.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::default(),
                lock_corners: false,
            }),
        )],
    );
    let sym_id = sym.id;
    doc.insert_symbol(sym);

    let layer_id = doc.pages()[0].layers[0].id;

    let mut tbl_spec = VectorTable::new(2, 2, 40.0, 20.0);
    tbl_spec.cells[0].text = "Hello".into();
    tbl_spec.cells[0].fill = Fill::Solid(Color::rgb(0.9, 0.9, 1.0));
    let mut tbl = SceneObject::new(ObjectId(0), "TableObj", NodePayload::Table(tbl_spec));
    tbl.effects.push(Effect::DropShadow {
        dx_mm: 2.0,
        dy_mm: 2.0,
        blur_mm: 3.0,
        opacity: 0.5,
        color: Color::BLACK,
        feather_direction: FeatherDirection::Middle,
    });
    let tbl_oid = doc.add_object(tbl, Some(layer_id)).unwrap();

    let dim = SceneObject::new(
        ObjectId(0),
        "DimObj",
        NodePayload::Dimension(DimensionObject::linear(
            DimensionKind::Parallel,
            Point2D::new(10.0, 10.0),
            Point2D::new(60.0, 10.0),
            5.0,
        )),
    );
    let dim_oid = doc.add_object(dim, Some(layer_id)).unwrap();

    let conn = SceneObject::new(
        ObjectId(0),
        "ConnObj",
        NodePayload::Connector(ConnectorObject::between_objects(
            tbl_oid,
            AnchorPoint::Right,
            dim_oid,
            AnchorPoint::Left,
            ConnectorStyle::RightAngle,
        )),
    );
    doc.add_object(conn, Some(layer_id)).unwrap();

    let mut sym_inst =
        SceneObject::new(ObjectId(0), "SymInst", NodePayload::SymbolInstance(sym_id));
    sym_inst.transform = Affine2D::translate(30.0, 30.0);
    doc.add_object(sym_inst, Some(layer_id)).unwrap();

    let host = SceneObject::new(
        ObjectId(0),
        "HostPath",
        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
            0.0, 0.0, 50.0, 20.0,
        ))),
    );
    let host_oid = doc.add_object(host, Some(layer_id)).unwrap();

    let top = SceneObject::new(
        ObjectId(0),
        "TopObj",
        NodePayload::TextOnPath(TextOnPath {
            text: ArtisticText::from_plain("Curved", TextStyle::default()),
            path_id: host_oid,
            offset_mm: 5.0,
            distance_mm: 2.5,
            flip_vertical: false,
            flip_horizontal: false,
        }),
    );
    doc.add_object(top, Some(layer_id)).unwrap();

    let clip_child = SceneObject::new(
        ObjectId(0),
        "Inside",
        NodePayload::Shape(ParametricShape::Ellipse {
            rx: 10.0,
            ry: 10.0,
            mode: EllipseMode::Full,
            start_deg: 0.0,
            end_deg: 360.0,
        }),
    );
    let clip_child_id = doc.insert_loaded_object(clip_child).unwrap();
    let pc = SceneObject::new(
        ObjectId(0),
        "PowerClipObj",
        NodePayload::PowerClip {
            container: Box::new(BezierPath::from_rect(Rect2D::from_xywh(
                0.0, 0.0, 30.0, 30.0,
            ))),
            container_parametric: Some(ParametricShape::Rectangle {
                width: 30.0,
                height: 30.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::default(),
                lock_corners: false,
            }),
            lock_contents: true,
            contents: vec![clip_child_id],
        },
    );
    doc.add_object(pc, Some(layer_id)).unwrap();

    doc.page_mut(0)
        .unwrap()
        .unknown_chunks
        .push(UnknownCdrChunk {
            fourcc: *b"XPAG",
            version_hint: 1200,
            raw_bytes: vec![0xAA, 0xBB, 0xCC],
        });
    doc.page_mut(0).unwrap().layers[0]
        .unknown_chunks
        .push(UnknownCdrChunk {
            fourcc: *b"XLAY",
            version_hint: 1200,
            raw_bytes: vec![0x11, 0x22],
        });

    let bytes = write_cdr_riff(&doc).unwrap();
    let limits = CodecLimits::default();
    let parsed = parse_cdr_riff(&bytes, &limits, &HashMap::new()).unwrap();
    let rt = &parsed.document;
    assert_eq!(rt.custom_icc_profile(), Some(icc.as_slice()));
    assert_eq!(rt.symbols().len(), 1);
    assert_eq!(rt.pages()[0].unknown_chunks.len(), 1);
    assert_eq!(rt.pages()[0].unknown_chunks[0].fourcc, *b"XPAG");
    assert_eq!(rt.pages()[0].layers[0].unknown_chunks.len(), 1);
    assert_eq!(rt.pages()[0].layers[0].unknown_chunks[0].fourcc, *b"XLAY");

    let objs = &rt.pages()[0].layers[0].objects;
    assert!(matches!(
        rt.object(objs[0]).unwrap().payload,
        NodePayload::Table(_)
    ));
    assert!(!rt.object(objs[0]).unwrap().effects.is_empty());
    assert!(matches!(
        rt.object(objs[1]).unwrap().payload,
        NodePayload::Dimension(_)
    ));
    assert!(matches!(
        rt.object(objs[2]).unwrap().payload,
        NodePayload::Connector(_)
    ));
    assert!(matches!(
        rt.object(objs[3]).unwrap().payload,
        NodePayload::SymbolInstance(_)
    ));
    assert!(matches!(
        rt.object(objs[5]).unwrap().payload,
        NodePayload::TextOnPath(_)
    ));
    match &rt.object(objs[6]).unwrap().payload {
        NodePayload::PowerClip {
            container_parametric,
            lock_contents,
            contents,
            ..
        } => {
            assert!(*lock_contents);
            assert!(container_parametric.is_some());
            assert_eq!(contents.len(), 1);
        }
        other => panic!("expected PowerClip, got {other:?}"),
    }
    let _ = (
        ColorMode::Cmyk,
        Page::new(PageId(1), "p", 10.0, 10.0, 0.0, LayerId(1)),
        Layer::new(LayerId(1), "l"),
    );
}
