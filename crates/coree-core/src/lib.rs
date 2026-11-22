// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Headless Pure-Rust document, vector geometry, color, 11-lens, effect, text,
//! table, dimension, selection, spatial-index, and deterministic serializable
//! command engine for `coree`.

pub mod boolean;
pub mod brand;
pub mod color;
pub mod command;
pub mod dimension;
pub mod document;
pub mod effects;
pub mod error;
pub mod geometry;
pub mod icc;
pub mod ids;
pub mod lens;
pub mod math;
pub mod selection;
pub mod style;
pub mod table;
pub mod text;

/// Crate identifier used during workspace initialization and diagnostics.
pub const CRATE_NAME: &str = "coree-core";

// Ergonomic top-level re-exports for downstream crates
pub use boolean::{
    apply_boolean, apply_boolean_many, offset_polygon, stroke_to_closed_path, BooleanOp,
};
pub use brand::{
    BrandInfo, APP_NAME, DISPLAY_NAME, LICENSE, NATIVE_EXT, REPO_URL, TRADEMARK_NOTICE,
};
pub use color::{
    default_cmyk_palette, default_rgb_palette, Color, ColorHarmony, ColorMode, ColorPalette,
    ColorStyle, ColorStyleId, ColorSwatch,
};
pub use command::{apply_command, rhai_array_to_ids, run_rhai_macro, CommandHistory, CommandOp};
pub use dimension::{AnchorPoint, ConnectorObject, ConnectorStyle, DimensionKind, DimensionObject};
pub use document::{
    decode_base64, encode_base64, AlignOp, DistributeOp, Document, DocumentMetadata, GridConfig,
    Guide, GuideOrientation, Layer, MasterLayerScope, NodePayload, Page, PathMetrics,
    PerspectiveGridConfig, PerspectiveGridMode, RasterAsset, RasterLayerData, SceneObject,
    SpatialIndex, SymbolDef, UnknownCdrChunk, ZOrderOp,
};
pub use effects::{
    ContourMode, DistortKind, Effect, EnvelopeMode, FeatherDirection, RasterFilter,
    TransparencyMode,
};
pub use error::{CoreError, CoreResult};
pub use geometry::{
    erase_circle, knife_cut, virtual_segment_delete, ArtisticMedia, ArtisticMediaStyle,
    ArtisticWidthProfile, BasicShapeKind, BezierPath, CornerStyle, EllipseMode, ImpactStyle,
    LiveSketch, NodeKind, ParametricShape, PathNode, RecognizedShape, SmartDrawing, Subpath,
};
pub use icc::{
    apply_soft_proof_rgba, apply_soft_proof_rgba_with_profile, convert_cmyk_to_rgb_icc,
    convert_rgb_between_builtin_profiles, convert_rgb_to_cmyk_icc, generate_minimal_cmyk_icc_v2,
    generate_minimal_srgb_icc_v2, is_out_of_cmyk_gamut, soft_proof_srgb_through_cmyk,
    BuiltinIccProfile, IccColorSpace, IccLut, IccPcs, IccProfile, IccProfileClass,
    IccProfileHeader, IccTagEntry, IccToneCurve, RenderingIntent, MAX_ICC_PROFILE_BYTES,
};
pub use ids::{ImageAssetId, LayerId, ObjectId, ObjectStyleId, PageId, SymbolId};
pub use lens::{ColorMapMode, LensConfig, LensKind};
pub use math::{Affine2D, AffineDecomposed, Point2D, Rect2D, Unit, Vec2D};
pub use selection::{NodeRef, NodeSelection, SelectionState, TransformMode};
pub use style::{
    ArrowheadStyle, BlendMode, Fill, GradientStop, LineCap, LineJoin, MeshNode, ObjectStyle,
    OutlineStyle, PostScriptFillPreset, HAIRLINE_WIDTH_MM,
};
pub use table::{TableCell, VectorTable};
pub use text::{
    ArtisticText, DropCapConfig, OpenTypeFeatures, ParagraphText, TextAlign, TextDirection,
    TextOnPath, TextRun, TextStyle, TextWrapMode,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_metadata_is_valid() {
        assert_eq!(CRATE_NAME, "coree-core");
        assert_eq!(APP_NAME, "coree");
    }

    #[test]
    fn end_to_end_document_serialization_with_all_node_types() {
        let mut doc = Document::new_a4("Full Feature Stress Test");
        let page_id = doc.pages[0].id;
        let cut_layer = doc
            .add_layer(page_id, "CutContour", true, MasterLayerScope::None)
            .unwrap();

        let rect_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Card Base",
                    NodePayload::Shape(ParametricShape::Rectangle {
                        width: 85.0,
                        height: 55.0,
                        corner_radii: [3.0; 4],
                        corner_style: CornerStyle::Rounded,
                        lock_corners: true,
                    }),
                )
                .with_fill(Fill::LinearGradient {
                    start: Point2D::ZERO,
                    end: Point2D::new(85.0, 55.0),
                    stops: vec![
                        GradientStop::new(0.0, Color::CYAN),
                        GradientStop::new(1.0, Color::MAGENTA),
                    ],
                })
                .with_outline(OutlineStyle::cut_contour()),
                Some(cut_layer),
            )
            .unwrap();

        let text_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Arabic Heading",
                    NodePayload::ArtisticText(ArtisticText::from_plain(
                        "استوديو التصميم",
                        TextStyle {
                            direction: TextDirection::Auto,
                            align: TextAlign::ForceJustifyKashida,
                            ..TextStyle::default()
                        },
                    )),
                ),
                None,
            )
            .unwrap();

        let table_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Spec Table",
                    NodePayload::Table(VectorTable::new(2, 2, 20.0, 8.0)),
                ),
                None,
            )
            .unwrap();

        let dim_id = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Width Dim",
                    NodePayload::Dimension(DimensionObject::linear(
                        DimensionKind::Horizontal,
                        Point2D::ZERO,
                        Point2D::new(85.0, 0.0),
                        6.0,
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
                    NodePayload::Connector(ConnectorObject::between_objects(
                        rect_id,
                        AnchorPoint::Right,
                        table_id,
                        AnchorPoint::Left,
                        ConnectorStyle::RightAngle,
                    )),
                ),
                None,
            )
            .unwrap();

        let json = serde_json::to_string(&doc).unwrap();
        let restored: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.objects.len(), 5);
        assert!(restored.objects.contains_key(&text_id));
        assert!(restored.objects.contains_key(&dim_id));
        assert!(restored.objects.contains_key(&conn_id));
    }
}
