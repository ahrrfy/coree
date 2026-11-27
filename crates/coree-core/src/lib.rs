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
pub mod find_replace;
pub mod geometry;
pub mod hit_test;
pub mod icc;
pub mod ids;
pub mod lens;
pub mod math;
pub mod resources;
pub mod selection;
pub mod style;
pub mod stylus;
pub mod table;
pub mod text;
pub mod viewport;

/// Crate identifier used during workspace initialization and diagnostics.
pub const CRATE_NAME: &str = "coree-core";

// Ergonomic top-level re-exports for downstream crates
pub use boolean::{
    apply_boolean, apply_boolean_many, arrowhead_geometry, offset_polygon, stroke_to_closed_path,
    BooleanOp,
};
pub use brand::{
    BrandInfo, APP_NAME, DISPLAY_NAME, LICENSE, NATIVE_EXT, REPO_URL, TRADEMARK_NOTICE,
};
pub use color::{
    cmyk_to_rgb_icc, default_cmyk_palette, default_rgb_palette, Color, ColorHarmony, ColorMode,
    ColorPalette, ColorStyle, ColorStyleId, ColorSwatch,
};
pub use command::{
    apply_command, rhai_array_to_ids, run_rhai_macro, CommandHistory, CommandOp, DeltaWalHints,
    DocumentInvalidation,
};
pub use dimension::{AnchorPoint, ConnectorObject, ConnectorStyle, DimensionKind, DimensionObject};
pub use document::{
    decode_base64, encode_base64, AlignOp, ClipMask, CowTile, DistributeOp, Document,
    DocumentMetadata, DrawingScale, GridConfig, Guide, GuideOrientation, Layer, MasterLayerScope,
    NodePayload, Page, PathMetrics, PerspectiveGridConfig, PerspectiveGridMode, PerspectivePlane,
    PixelDepth, PixelLayer, PowerClipFitMode, RasterAsset, RasterLayerData, SceneObject,
    SpatialIndex, SpatialObjectEntry, SymbolDef, UnknownCdrChunk, ZOrderOp, RASTER_TILE_DIM,
};
pub use effects::{
    BevelStyle, ContourMode, DistortKind, Effect, EnvelopeMode, ExtrudeLight, FeatherDirection,
    RasterFilter, TransparencyMode,
};
pub use error::{CoreError, CoreResult};
pub use find_replace::{
    extract_searchable_text, find_and_replace, find_objects, preview_find_and_replace,
    scoped_page_indices, text_runs_ref, ColorMatch, FindQuery, FindReplaceReport, FindScope,
    LensKindFilter, ObjectKindFilter, ReplaceAction, TextMatchMode,
};
pub use geometry::{
    closed_subpaths_contain_even_odd, erase_circle, fit_cubic_bezier_subpath, knife_cut,
    segment_min_dist_to_point, virtual_segment_delete, ArtisticMedia, ArtisticMediaStyle,
    ArtisticWidthProfile, BSplineCurve, BasicShapeKind, BezierPath, CornerStyle, EllipseMode,
    ImpactStyle, LiveSketch, NodeKind, ParametricShape, PathNode, RecognizedShape, SmartDrawing,
    Subpath,
};
pub use hit_test::{
    hit_test_node, hit_test_object_warped_world_bounds, hit_test_object_warped_world_path,
    hit_test_point, hit_test_point_exact, hit_test_powerclip_contents,
    hit_test_powerclip_contents_exact, hit_test_powerclip_contents_rect, hit_test_rect,
    DynamicGuideKind, DynamicGuideLine, NodeHit, SnapEngine, SnapOptions,
};
pub use icc::{
    adapt_xyz_bradford, apply_soft_proof_rgba, apply_soft_proof_rgba_with_profile,
    bradford_adaptation_matrix, convert_cmyk_to_rgb_icc, convert_rgb_between_builtin_profiles,
    convert_rgb_to_cmyk_icc, generate_minimal_cmyk_icc_v2, generate_minimal_display_p3_icc_v2,
    generate_minimal_srgb_icc_v2, is_out_of_cmyk_gamut, soft_proof_srgb_through_cmyk,
    BuiltinIccProfile, DisplayTransformLut3D, IccColorPipeline, IccColorSpace, IccLut, IccPcs,
    IccProfile, IccProfileClass, IccProfileHeader, IccTagEntry, IccToneCurve, IccV4Lut,
    RenderingIntent, SoftProofLut3D, BRADFORD_M, BRADFORD_M_INV, MAX_ICC_PROFILE_BYTES,
};
pub use ids::{ImageAssetId, LayerId, ObjectId, ObjectStyleId, PageId, SymbolId};
pub use lens::{ColorMapMode, LensConfig, LensKind};
pub use resources::{
    HostResourceProfile, ResourceBudget, ResourceGovernorMode, WORLD_TILE_BYTES,
};
pub use math::{Affine2D, AffineDecomposed, Perspective3x3, Point2D, Rect2D, Unit, Vec2D};
pub use selection::{
    NodeRef, NodeSelection, RasterMaskMode, RasterMaskSelection, RasterMaskShape, SelectionState,
    TransformMode,
};
pub use style::{
    ArrowheadStyle, BlendMode, Fill, GradientStop, LineCap, LineJoin, MeshNode, ObjectStyle,
    OutlineStyle, PostScriptFillPreset, HAIRLINE_WIDTH_MM,
};
pub use stylus::{PressureCurve, StylusSample};
pub use table::{TableCell, VectorTable};
pub use text::{
    ArtisticText, DropCapConfig, OpenTypeFeatures, ParagraphListStyle, ParagraphTabAlign,
    ParagraphTabStop, ParagraphText, TextAlign, TextDirection, TextField, TextOnPath, TextRun,
    TextStyle, TextWrapMode,
};
pub use viewport::{ViewMode, Viewport};

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
        let page_id = doc.pages()[0].id;
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
        assert_eq!(restored.objects().len(), 5);
        assert!(restored.has_object(text_id));
        assert!(restored.has_object(dim_id));
        assert!(restored.has_object(conn_id));
    }
}
