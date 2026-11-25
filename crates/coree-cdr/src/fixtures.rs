// SPDX-License-Identifier: MIT OR Apache-2.0

//! Programmatic clean-room binary fixture generators for valid CDR RIFF v12,
//! CDR X4 ZIP, CDR X6+ ZIP, CDT, CMX (16/32-bit), CPT (multi-layer & RIFF),
//! legacy CP1256 Arabic CDR, Unknown-Chunk Passthrough CDR, and adversarial
//! security test vectors (truncated, garbage, Zip-Slip, Zip-Bomb, deep RIFF,
//! huge bitmaps).

use crate::cdr_riff::{
    encode_legacy_cp1256_txsm, encode_stlt_chunk, write_cdr_riff, write_cdr_riff_with_options,
    RiffWriteOptions,
};
use crate::cdr_zip::{write_cdr_zip, CdrZipEra};
use crate::cdt::export_cdt_bytes;
use crate::cmx::{export_cmx_bytes, CmxPrecision};
use crate::coord::{encode_cp1256, CHARSET_ARABIC, CODEPAGE_ARABIC_CP1256};
use crate::cpt::{
    write_cpt_bytes, write_cpt_riff_bytes, CptColorModel, CptCompression, CptLayerSpec,
};
use crate::riff::{write_riff, RiffDocument, RiffEndian, RiffNode};
use crate::CdrWriteVersion;
use coree_core::{
    Affine2D, ArtisticText, BezierPath, BlendMode, Color, ColorMapMode, ColorMode, CornerStyle,
    Document, EllipseMode, Fill, GradientStop, LensConfig, LensKind, LineCap, LineJoin,
    MasterLayerScope, NodeKind, NodePayload, ObjectId, OutlineStyle, ParagraphText,
    ParametricShape, PathNode, Point2D, RasterAsset, RasterLayerData, SceneObject, Subpath,
    TextAlign, TextDirection, TextStyle, UnknownCdrChunk, Vec2D,
};
use flate2::{write::ZlibEncoder, Compression};
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// Builds a rich multi-page sample [`Document`] exercising paths (Cusp/Smooth/Symmetrical),
/// shapes, CMYK/RGB/Spot/Lab fills & gradients, outlines, transforms, groups, UTF-8 &
/// Arabic RTL + Kashida text, embedded bitmaps, all 11 Lenses, and Unknown-Chunk Passthrough.
#[must_use]
pub fn generate_sample_document() -> Document {
    let mut doc = Document::new_a4("coree Clean-Room Showcase");
    {
        let meta = doc.metadata_mut();
        meta.author = "coree contributors".to_owned();
        meta.subject = "Vector & Print Showcase".to_owned();
        meta.keywords = vec!["vector".to_owned(), "arabic".to_owned(), "cmyk".to_owned()];
        meta.notes = "Generated programmatically with zero proprietary assets.".to_owned();
    }

    let page1_id = doc.pages()[0].id;
    let artwork_layer = doc.pages()[0].layers[0].id;
    let cut_layer = doc
        .add_layer(page1_id, "CutContour", true, MasterLayerScope::None)
        .expect("page1 exists");

    // 1. Cubic Bézier path with Cusp, Smooth, and Symmetrical nodes (closed)
    let curve = BezierPath::from_subpath(Subpath {
        closed: true,
        nodes: vec![
            PathNode::new(
                Point2D::new(10.0, 10.0),
                Some(Point2D::new(5.0, 15.0)),
                Some(Point2D::new(15.0, 5.0)),
                NodeKind::Symmetrical,
            ),
            PathNode::new(
                Point2D::new(50.0, 10.0),
                Some(Point2D::new(40.0, 5.0)),
                Some(Point2D::new(60.0, 15.0)),
                NodeKind::Smooth,
            ),
            PathNode::new(Point2D::new(50.0, 45.0), None, None, NodeKind::Cusp),
            PathNode::new(
                Point2D::new(10.0, 45.0),
                Some(Point2D::new(20.0, 50.0)),
                Some(Point2D::new(5.0, 35.0)),
                NodeKind::Cusp,
            ),
        ],
    });
    let mut curve_obj = SceneObject::new(ObjectId(0), "Hero Curve", NodePayload::Path(curve))
        .with_fill(Fill::LinearGradient {
            start: Point2D::new(10.0, 10.0),
            end: Point2D::new(50.0, 45.0),
            stops: vec![
                GradientStop::new(0.0, Color::CYAN),
                GradientStop::new(0.5, Color::YELLOW),
                GradientStop::new(1.0, Color::MAGENTA),
            ],
        })
        .with_outline(OutlineStyle {
            color: Color::KEY_BLACK,
            width_mm: 0.75,
            cap: LineCap::Round,
            join: LineJoin::Bevel,
            dash_pattern: vec![3.0, 1.5],
            ..OutlineStyle::solid(Color::KEY_BLACK, 0.75)
        })
        .with_transform(Affine2D::translate(5.0, 7.5).compose(&Affine2D::rotate_deg(12.0)));
    curve_obj.unknown_chunks.push(UnknownCdrChunk {
        fourcc: *b"XOBJ",
        version_hint: 1200,
        raw_bytes: vec![0xDE, 0xAD, 0xBE, 0xEF, 0x42],
    });
    let _ = doc.add_object(curve_obj, Some(artwork_layer));

    // 2. Parametric Rounded Rectangle + Radial Gradient
    let rect_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Card Base",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 85.0,
                    height: 55.0,
                    corner_radii: [4.0, 4.0, 4.0, 4.0],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::RadialGradient {
                center: Point2D::new(42.5, 27.5),
                radius: 40.0,
                focal: Point2D::new(35.0, 20.0),
                stops: vec![
                    GradientStop::new(0.0, Color::rgb(0.95, 0.85, 0.2)),
                    GradientStop::new(1.0, Color::cmyk(0.1, 0.8, 0.9, 0.05)),
                ],
            })
            .with_transform(Affine2D::translate(70.0, 15.0)),
            Some(artwork_layer),
        )
        .expect("add rect");

    // 3. Parametric Ellipse + Conical Gradient
    let ell_id = doc
        .add_object(
            SceneObject::new(
                ObjectId(0),
                "Badge Circle",
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: 18.0,
                    ry: 14.0,
                    mode: EllipseMode::Full,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
            )
            .with_fill(Fill::ConicalGradient {
                center: Point2D::ZERO,
                angle_deg: 30.0,
                stops: vec![
                    GradientStop::new(0.0, Color::rgb(0.1, 0.4, 0.9)),
                    GradientStop::new(1.0, Color::rgb(0.9, 0.2, 0.5)),
                ],
            })
            .with_transform(Affine2D::translate(110.0, 95.0)),
            Some(artwork_layer),
        )
        .expect("add ellipse");

    // Group the rectangle and ellipse together
    let _ = doc.group_objects(&[rect_id, ell_id], "Logo Group");

    // 4. Parametric Polygon + Square Gradient
    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Hex Emblem",
            NodePayload::Shape(ParametricShape::Polygon {
                sides: 6,
                radius_x: 15.0,
                radius_y: 15.0,
            }),
        )
        .with_fill(Fill::SquareGradient {
            center: Point2D::ZERO,
            size: 30.0,
            angle_deg: 45.0,
            stops: vec![
                GradientStop::new(0.0, Color::lab(70.0, 20.0, -30.0)),
                GradientStop::new(1.0, Color::gray(0.2)),
            ],
        })
        .with_transform(Affine2D::translate(35.0, 110.0)),
        Some(artwork_layer),
    );

    // 5. English UTF-8 ArtisticText
    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Title English",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "coree Clean-Room Vector Studio",
                TextStyle {
                    font_family: "Noto Sans".to_owned(),
                    font_size_pt: 22.0,
                    bold: true,
                    fill: Color::rgb(0.1, 0.15, 0.25),
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(Affine2D::translate(15.0, 140.0)),
        Some(artwork_layer),
    );

    // 6. Arabic RTL ArtisticText with Kashida justification
    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Arabic Headline",
            NodePayload::ArtisticText(ArtisticText::from_plain(
                "مرحباً بكم في كوري",
                TextStyle {
                    font_family: "Noto Naskh Arabic".to_owned(),
                    font_size_pt: 24.0,
                    direction: TextDirection::Rtl,
                    align: TextAlign::ForceJustifyKashida,
                    kashida_stretch: 0.75,
                    fill: Color::cmyk(0.9, 0.3, 0.0, 0.1),
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(Affine2D::translate(15.0, 165.0)),
        Some(artwork_layer),
    );

    // 7. Paragraph Text Frame
    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Body Paragraph",
            NodePayload::ParagraphText(ParagraphText::new(
                "Precision print & signage vector artwork.",
                Vec2D::new(90.0, 30.0),
                TextStyle {
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_pt: 11.0,
                    ..TextStyle::default()
                },
            )),
        )
        .with_transform(Affine2D::translate(15.0, 190.0)),
        Some(artwork_layer),
    );

    // 8. Embedded 4x4 RGBA Bitmap Asset
    let mut pixels = Vec::with_capacity(4 * 4 * 4);
    for y in 0..4u8 {
        for x in 0..4u8 {
            pixels.extend_from_slice(&[x * 60 + 20, y * 60 + 20, 180, 255]);
        }
    }
    let asset = RasterAsset::new(4, 4, pixels, 300.0, ColorMode::Rgb).expect("valid 4x4 asset");
    let asset_id = doc.register_asset(asset);
    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Embedded Swatch Bitmap",
            NodePayload::Raster(RasterLayerData {
                asset_id,
                width_mm: 24.0,
                height_mm: 24.0,
                filters: Vec::new(),
                crop_rect: None,
            }),
        )
        .with_transform(Affine2D::translate(130.0, 150.0)),
        Some(artwork_layer),
    );

    // 9. ALL 11 Lenses (exercising frozen, viewpoint, and remove_face flags)
    let eleven_lenses = [
        LensConfig {
            kind: LensKind::Brighten { rate_pct: 35.0 },
            frozen: false,
            viewpoint: None,
            remove_face: false,
            frozen_snapshot: None,
        },
        LensConfig {
            kind: LensKind::ColorAdd {
                color: Color::rgb(0.2, 0.5, 0.8),
                rate_pct: 45.0,
            },
            frozen: true,
            viewpoint: None,
            remove_face: false,
            frozen_snapshot: None,
        },
        LensConfig {
            kind: LensKind::ColorLimit {
                color: Color::cmyk(0.0, 0.8, 0.5, 0.0),
                rate_pct: 70.0,
            },
            frozen: false,
            viewpoint: Some(Point2D::new(40.0, 60.0)),
            remove_face: false,
            frozen_snapshot: None,
        },
        LensConfig {
            kind: LensKind::CustomColorMap {
                from_color: Color::rgb(0.1, 0.0, 0.4),
                to_color: Color::rgb(1.0, 0.9, 0.2),
                palette_mode: ColorMapMode::ForwardRainbow,
            },
            frozen: false,
            viewpoint: None,
            remove_face: true,
            frozen_snapshot: None,
        },
        LensConfig::new(LensKind::FishEye { rate_pct: 120.0 }),
        LensConfig::new(LensKind::HeatMap {
            palette_rotation_deg: 60.0,
        }),
        LensConfig::new(LensKind::Invert),
        LensConfig {
            kind: LensKind::Magnify { amount: 2.5 },
            frozen: true,
            viewpoint: Some(Point2D::new(80.0, 90.0)),
            remove_face: true,
            frozen_snapshot: None,
        },
        LensConfig::new(LensKind::TintedGrayscale {
            tint: Color::rgb(0.3, 0.2, 0.1),
        }),
        LensConfig::new(LensKind::Transparency {
            color: Color::CYAN,
            rate_pct: 40.0,
        }),
        LensConfig::new(LensKind::Wireframe {
            override_outline: Some(Color::BLACK),
            override_fill: Some(Color::WHITE),
        }),
    ];
    for (idx, lens_cfg) in eleven_lenses.into_iter().enumerate() {
        let mut obj = SceneObject::new(
            ObjectId(0),
            format!("Lens {}", idx + 1),
            NodePayload::Shape(ParametricShape::Ellipse {
                rx: 6.0,
                ry: 6.0,
                mode: EllipseMode::Full,
                start_deg: 0.0,
                end_deg: 360.0,
            }),
        )
        .with_transform(Affine2D::translate(15.0 + (idx as f64) * 15.0, 240.0));
        obj.lens = Some(lens_cfg);
        let _ = doc.add_object(obj, Some(artwork_layer));
    }

    // 10. CutContour hairline on dedicated cut layer
    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Die Cut Outline",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 89.0,
                height: 59.0,
                corner_radii: [3.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_outline(OutlineStyle::cut_contour())
        .with_transform(Affine2D::translate(68.0, 13.0)),
        Some(cut_layer),
    );

    // 11. Page 2 (US Letter) for multi-page verification
    let page2_id = doc.add_page("Back Cover", 215.9, 279.4, 3.0);
    let page2_layer = doc
        .pages()
        .iter()
        .find(|p| p.id == page2_id)
        .expect("page2")
        .layers[0]
        .id;
    let _ = doc.add_object(
        SceneObject::new(
            ObjectId(0),
            "Page 2 Star",
            NodePayload::Shape(ParametricShape::Star {
                points: 5,
                outer_radius: 25.0,
                inner_ratio: 0.45,
                complex: false,
            }),
        )
        .with_fill(Fill::Solid(Color::MAGENTA))
        .with_transform(Affine2D::translate(100.0, 120.0)),
        Some(page2_layer),
    );

    // 12. Document-level Unknown-Chunk Passthrough
    doc.push_unknown_doc_chunk(UnknownCdrChunk {
        fourcc: *b"XDOC",
        version_hint: 1200,
        raw_bytes: b"coree-passthrough-doc-payload!".to_vec(),
    });

    doc.refresh_document_palette();
    doc
}

/// Generates a valid clean-room CDR RIFF v12 file (including `LIST cmpr` + `CPng`).
#[must_use]
pub fn sample_cdr_riff_v12_bytes() -> Vec<u8> {
    write_cdr_riff(&generate_sample_document()).expect("write_cdr_riff")
}

/// Generates a valid clean-room CDR X4–X5 ZIP/OPC container.
#[must_use]
pub fn sample_cdr_zip_x4_bytes() -> Vec<u8> {
    write_cdr_zip(&generate_sample_document(), CdrZipEra::X4, false).expect("write_cdr_zip X4")
}

/// Generates a valid clean-room CDR X6–2025 ZIP/OPC container (with `root.dat` + `data/*.dat`).
#[must_use]
pub fn sample_cdr_zip_x6_bytes() -> Vec<u8> {
    write_cdr_zip(&generate_sample_document(), CdrZipEra::X6, false).expect("write_cdr_zip X6")
}

/// Generates a valid clean-room CDR Template (`.cdt` ZIP) container.
#[must_use]
pub fn sample_cdt_bytes() -> Vec<u8> {
    export_cdt_bytes(&generate_sample_document(), CdrWriteVersion::ZipX4).expect("export_cdt")
}

/// Generates a valid clean-room 16-bit `.cmx` (`CMX1`) binary.
#[must_use]
pub fn sample_cmx_16bit_bytes() -> Vec<u8> {
    export_cmx_bytes(&generate_sample_document(), CmxPrecision::Bit16).expect("export_cmx 16")
}

/// Generates a valid clean-room 32-bit `.cmx` (`CMX2`) binary.
#[must_use]
pub fn sample_cmx_32bit_bytes() -> Vec<u8> {
    export_cmx_bytes(&generate_sample_document(), CmxPrecision::Bit32).expect("export_cmx 32")
}

/// Generates a multi-layer `.cpt` (`CPT9FILE`) binary featuring:
/// - Layer 1: RGB24 (Raw)
/// - Layer 2: CMYK32 (PackBits RLE strips) + Zlib Alpha Mask channel
/// - Layer 3: Gray8 (Deflate) + RLE Alpha Mask channel
#[must_use]
pub fn sample_cpt_multilayer_bytes() -> Vec<u8> {
    let w = 8u32;
    let h = 6u32;
    let px = (w * h) as usize;

    // Layer 1: RGB24 solid sky-blue gradient
    let mut rgb_raw = Vec::with_capacity(px * 3);
    for y in 0..h {
        for x in 0..w {
            rgb_raw.extend_from_slice(&[(x * 30) as u8, (y * 40) as u8, 220]);
        }
    }

    // Layer 2: CMYK32 process magenta with semi-transparent alpha mask (128)
    let mut cmyk_raw = Vec::with_capacity(px * 4);
    for _ in 0..px {
        cmyk_raw.extend_from_slice(&[0, 255, 0, 0]);
    }
    let mask_zlib = vec![128u8; px];

    // Layer 3: Gray8 luminance 200 with checker alpha mask
    let gray_raw = vec![200u8; px];
    let mut mask_rle = Vec::with_capacity(px);
    for i in 0..px {
        mask_rle.push(if i % 2 == 0 { 255 } else { 64 });
    }

    let layers = vec![
        CptLayerSpec {
            name: "Background RGB".to_owned(),
            offset_x: 0,
            offset_y: 0,
            width: w,
            height: h,
            opacity: 255,
            blend_mode: BlendMode::Normal,
            visible: true,
            locked: false,
            color_model: CptColorModel::Rgb24,
            compression: CptCompression::Raw,
            raw_pixels: rgb_raw,
            alpha_mask: None,
            rows_per_strip: 3,
        },
        CptLayerSpec {
            name: "Magenta CMYK Overlay".to_owned(),
            offset_x: 2,
            offset_y: 1,
            width: w,
            height: h,
            opacity: 204,
            blend_mode: BlendMode::Multiply,
            visible: true,
            locked: false,
            color_model: CptColorModel::Cmyk32,
            compression: CptCompression::Rle,
            raw_pixels: cmyk_raw,
            alpha_mask: Some((CptCompression::Zlib, mask_zlib)),
            rows_per_strip: 2,
        },
        CptLayerSpec {
            name: "Grayscale Mask Layer".to_owned(),
            offset_x: 0,
            offset_y: 0,
            width: w,
            height: h,
            opacity: 255,
            blend_mode: BlendMode::Screen,
            visible: true,
            locked: true,
            color_model: CptColorModel::Gray8,
            compression: CptCompression::Deflate,
            raw_pixels: gray_raw,
            alpha_mask: Some((CptCompression::Rle, mask_rle)),
            rows_per_strip: 6,
        },
    ];

    write_cpt_bytes(
        9,
        "Multi-Layer CPT Fixture",
        w,
        h,
        CptColorModel::Rgb24,
        300.0,
        &layers,
    )
    .expect("write_cpt_bytes")
}

/// Generates a RIFF-wrapped `CPT ` container fixture.
#[must_use]
pub fn sample_cpt_riff_bytes() -> Vec<u8> {
    let w = 4u32;
    let h = 4u32;
    let layer = CptLayerSpec {
        name: "RIFF Raster".to_owned(),
        offset_x: 0,
        offset_y: 0,
        width: w,
        height: h,
        opacity: 255,
        blend_mode: BlendMode::Normal,
        visible: true,
        locked: false,
        color_model: CptColorModel::Rgb24,
        compression: CptCompression::Zlib,
        raw_pixels: [100, 150, 200].repeat((w * h) as usize),
        alpha_mask: Some((CptCompression::Raw, vec![180u8; (w * h) as usize])),
        rows_per_strip: 4,
    };
    write_cpt_riff_bytes(
        "RIFF CPT Fixture",
        w,
        h,
        CptColorModel::Rgb24,
        300.0,
        &[layer],
    )
    .expect("write_cpt_riff_bytes")
}

/// Generates an authentic legacy RIFF CDR v9 file containing TWO Arabic text objects
/// encoded in raw **Windows-1256 (CP1256)** bytes:
/// 1. Explicit `stlt` style with `codepage = 1256` & `charset = 178` (`ARABIC_CHARSET`)
/// 2. Legacy `txsm` with `codepage = 0` & `charset = 0` verifying heuristic CP1256 auto-detection!
#[must_use]
pub fn sample_cp1256_arabic_cdr_bytes() -> Vec<u8> {
    let arabic_explicit = "تصميم عربي كلاسيكي";
    let arabic_autodetect = "مرحبا بالعالم العربي";

    let (bytes_explicit, _) = encode_cp1256(arabic_explicit);
    let (bytes_auto, _) = encode_cp1256(arabic_autodetect);

    let style = TextStyle {
        font_family: "Traditional Arabic".to_owned(),
        font_size_pt: 28.0,
        direction: TextDirection::Rtl,
        align: TextAlign::Right,
        ..TextStyle::default()
    };

    let stlt_chunk = encode_stlt_chunk(1, &style, CODEPAGE_ARABIC_CP1256, CHARSET_ARABIC);
    let txsm_explicit = encode_legacy_cp1256_txsm(1, &bytes_explicit, true, CHARSET_ARABIC);
    let txsm_auto = encode_legacy_cp1256_txsm(0, &bytes_auto, false, 0);

    let riff_doc = RiffDocument {
        endian: RiffEndian::Little,
        form_type: *b"CDR9",
        children: vec![
            RiffNode::chunk(*b"vrsn", 900u16.to_le_bytes().to_vec()),
            RiffNode::list(
                *b"doc ",
                vec![
                    RiffNode::list(*b"stlt", vec![RiffNode::chunk(*b"stlt", stlt_chunk)]),
                    RiffNode::list(
                        *b"page",
                        vec![RiffNode::list(
                            *b"layr",
                            vec![
                                RiffNode::list(
                                    *b"obj ",
                                    vec![RiffNode::chunk(*b"txsm", txsm_explicit)],
                                ),
                                RiffNode::list(
                                    *b"obj ",
                                    vec![RiffNode::chunk(*b"txsm", txsm_auto)],
                                ),
                            ],
                        )],
                    ),
                ],
            ),
        ],
    };
    write_riff(&riff_doc).expect("write CP1256 CDR")
}

/// Generates a CDR RIFF file with explicit document-level (`XDOC`) and object-level
/// (`XOBJ`, odd byte length) unknown chunks for Unknown-Chunk Passthrough verification.
#[must_use]
pub fn sample_unknown_chunk_cdr_bytes() -> Vec<u8> {
    let mut doc = Document::new_a4("Unknown Chunk Passthrough Test");
    doc.push_unknown_doc_chunk(UnknownCdrChunk {
        fourcc: *b"XDOC",
        version_hint: 1200,
        raw_bytes: vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD],
    });
    let mut obj = SceneObject::new(
        ObjectId(0),
        "Box With Custom Chunk",
        NodePayload::Shape(ParametricShape::Rectangle {
            width: 40.0,
            height: 25.0,
            corner_radii: [0.0; 4],
            corner_style: CornerStyle::Rounded,
            lock_corners: true,
        }),
    )
    .with_fill(Fill::Solid(Color::CYAN));
    obj.unknown_chunks.push(UnknownCdrChunk {
        fourcc: *b"XOBJ",
        version_hint: 1200,
        raw_bytes: vec![0xFE, 0xDC, 0xBA, 0x98, 0x76],
    });
    let _ = doc.add_object(obj, None);

    write_cdr_riff_with_options(
        &doc,
        RiffWriteOptions {
            form_type: *b"CDRC",
            version: 1200,
            compress_doc: true,
            externalize_bitmaps: false,
        },
    )
    .expect("write unknown chunk CDR")
    .riff_bytes
}

// =============================================================================
// Adversarial Security Test Fixtures
// =============================================================================

/// Truncated RIFF header (only 6 bytes).
#[must_use]
pub fn malformed_truncated_header() -> Vec<u8> {
    b"RIFF\x20\x00".to_vec()
}

/// Arbitrary non-CDR garbage bytes.
#[must_use]
pub fn malformed_garbage_bytes() -> Vec<u8> {
    vec![
        0xFF, 0x00, 0x13, 0x37, 0xAB, 0xCD, 0xEF, 0x99, 0x01, 0x02, 0x03, 0x04,
    ]
}

/// Valid RIFF header whose inner chunk declares a payload larger than EOF.
#[must_use]
pub fn malformed_truncated_chunk() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&20u32.to_le_bytes()); // claims 20 bytes after size
    b.extend_from_slice(b"CDRC");
    b.extend_from_slice(b"loda");
    b.extend_from_slice(&9999u32.to_le_bytes()); // truncated inner chunk
    b.extend_from_slice(&[1, 2, 3, 4]);
    b
}

/// Malicious ZIP container embedding a Zip-Slip traversal entry (`../../etc/passwd`).
#[must_use]
pub fn malformed_zip_slip_bytes() -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("../../etc/passwd", opts).expect("zip start");
        zip.write_all(b"root:x:0:0:root:/root:/bin/sh")
            .expect("zip write");
        zip.start_file("content/riffData.cdr", opts)
            .expect("zip start 2");
        zip.write_all(&sample_cdr_riff_v12_bytes())
            .expect("zip write 2");
        zip.finish().expect("zip finish");
    }
    cursor.into_inner()
}

/// Malicious ZIP container containing a high-ratio Zip-Bomb (`> 100:1` compression ratio).
#[must_use]
pub fn malformed_zip_bomb_bytes() -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        let opts = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(9));
        zip.start_file("content/riffData.cdr", opts)
            .expect("zip start");
        // 2 MiB of zeros compresses to ~2 KiB (~1000:1 ratio > MAX_COMPRESSION_RATIO=100)
        let zeros = vec![0u8; 2 * 1024 * 1024];
        zip.write_all(&zeros).expect("zip write");
        zip.finish().expect("zip finish");
    }
    cursor.into_inner()
}

/// Malicious RIFF CDR embedding a Zlib bomb inside `LIST cmpr`.
#[must_use]
pub fn malformed_cmpr_zlib_bomb_bytes() -> Vec<u8> {
    let zeros = vec![0u8; 1024 * 1024];
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
    enc.write_all(&zeros).expect("zlib write");
    let compressed = enc.finish().expect("zlib finish");

    let mut cmpr_payload = Vec::new();
    cmpr_payload.extend_from_slice(b"cmpr");
    cmpr_payload.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
    cmpr_payload.extend_from_slice(&(zeros.len() as u32).to_le_bytes());
    cmpr_payload.extend_from_slice(&0u32.to_le_bytes()); // no CPng table
    cmpr_payload.extend_from_slice(b"CPng");
    cmpr_payload.extend_from_slice(&1u16.to_le_bytes());
    cmpr_payload.extend_from_slice(&0u16.to_le_bytes());
    cmpr_payload.extend_from_slice(&compressed);

    let mut out = Vec::new();
    let list_total = cmpr_payload.len() as u32;
    let riff_body = 4u32 + 8 + list_total + (list_total % 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_body.to_le_bytes());
    out.extend_from_slice(b"CDRC");
    out.extend_from_slice(b"LIST");
    out.extend_from_slice(&list_total.to_le_bytes());
    out.extend_from_slice(&cmpr_payload);
    if list_total % 2 == 1 {
        out.push(0);
    }
    out
}

/// Malicious RIFF CDR with `depth` nested `LIST grp ` containers (e.g., `200` > `MAX_TREE_DEPTH`).
#[must_use]
pub fn malformed_deep_nested_riff_bytes(depth: usize) -> Vec<u8> {
    let mut node = RiffNode::chunk(*b"loda", vec![0, 0, 0, 0]);
    for _ in 0..depth {
        node = RiffNode::list(*b"grp ", vec![node]);
    }
    write_riff(&RiffDocument::new(*b"CDRC", vec![node])).expect("write deep riff")
}

/// Malicious CDR containing a `bmp ` DIB claiming `65_536 × 65_536` dimensions.
#[must_use]
pub fn malformed_huge_bitmap_cdr_bytes() -> Vec<u8> {
    let mut dib = Vec::new();
    dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
    dib.extend_from_slice(&65_536i32.to_le_bytes()); // biWidth > MAX_BITMAP_DIM
    dib.extend_from_slice(&65_536i32.to_le_bytes()); // biHeight > MAX_BITMAP_DIM
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression
    dib.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&0i32.to_le_bytes());
    dib.extend_from_slice(&0i32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());

    let doc = RiffDocument::new(
        *b"CDRC",
        vec![RiffNode::list(
            *b"doc ",
            vec![RiffNode::chunk(*b"bmp ", dib)],
        )],
    );
    write_riff(&doc).expect("write huge bmp CDR")
}

/// Malicious CPT file claiming `100_000 × 100_000` canvas dimensions.
#[must_use]
pub fn malformed_huge_bitmap_cpt_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"CPT9FILE");
    out.extend_from_slice(&28u32.to_le_bytes());
    out.extend_from_slice(&100_000u32.to_le_bytes());
    out.extend_from_slice(&100_000u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // RGB24
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&300.0f32.to_le_bytes());
    out.extend_from_slice(&300.0f32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}
