// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tool identifiers, flyout groups, handle discriminants, and immutable tool settings snapshots.

use coree_core::{
    ArtisticWidthProfile, BasicShapeKind, BlendMode, Color, ContourMode, CornerStyle, DistortKind,
    Effect, EllipseMode, EnvelopeMode, ImpactStyle, NodeKind, Point2D, RasterMaskMode,
};
use serde::{Deserialize, Serialize};

/// Every interactive studio tool across all 15 Toolbox Flyout groups (`65` tools).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ToolId {
    // Group 0: Pick
    #[default]
    Pick,
    FreehandPick,
    FreeTransform,

    // Group 1: Shape / Node & Sculpting
    ShapeNode,
    Smooth,
    Smear,
    Twirl,
    AttractRepel,
    Roughen,

    // Group 2: Crop & Cutting
    Crop,
    Knife,
    VirtualSegmentDelete,
    Eraser,

    // Group 3: Zoom & Pan
    Zoom,
    Pan,

    // Group 4: Curves & Drawing
    Freehand,
    TwoPointLine,
    Bezier,
    Pen,
    BSpline,
    Polyline,
    ThreePointCurve,
    SmartDrawing,
    LiveSketch,
    ArtisticMedia,

    // Group 5: Rectangle
    Rectangle,
    ThreePointRectangle,

    // Group 6: Ellipse
    Ellipse,
    ThreePointEllipse,

    // Group 7: Object / Parametric Shapes
    Polygon,
    Star,
    ComplexStar,
    Spiral,
    GraphPaper,
    Impact,
    BasicShapes,

    // Group 8: Text & Table
    ArtisticText,
    ParagraphText,
    Table,

    // Group 9: Dimensions & Connectors
    ParallelDimension,
    HvDimension,
    AngularDimension,
    SegmentDimension,
    ThreePointCallout,
    StraightConnector,
    RightAngleConnector,

    // Group 10: Interactive Vector Effects
    DropShadow,
    BlockShadow,
    Contour,
    Blend,
    Distort,
    Envelope,
    Extrude25D,
    Transparency,

    // Group 11: Eyedroppers
    ColorEyedropper,
    AttributesEyedropper,

    // Group 12: Interactive & Smart Fills
    InteractiveFill,
    MeshFill,
    SmartFill,

    // Group 13: Lens Tool
    LensTool,

    // Group 14: Integrated Bitmap Studio
    BitmapBrush,
    BitmapEraser,
    CloneStamp,
    Smudge,
    MaskSelect,
}

/// Static definition of all 15 Toolbox Flyout groups.
pub const TOOL_GROUPS: [&[ToolId]; 15] = [
    &[ToolId::Pick, ToolId::FreehandPick, ToolId::FreeTransform],
    &[
        ToolId::ShapeNode,
        ToolId::Smooth,
        ToolId::Smear,
        ToolId::Twirl,
        ToolId::AttractRepel,
        ToolId::Roughen,
    ],
    &[
        ToolId::Crop,
        ToolId::Knife,
        ToolId::VirtualSegmentDelete,
        ToolId::Eraser,
    ],
    &[ToolId::Zoom, ToolId::Pan],
    &[
        ToolId::Freehand,
        ToolId::TwoPointLine,
        ToolId::Bezier,
        ToolId::Pen,
        ToolId::BSpline,
        ToolId::Polyline,
        ToolId::ThreePointCurve,
        ToolId::SmartDrawing,
        ToolId::LiveSketch,
        ToolId::ArtisticMedia,
    ],
    &[ToolId::Rectangle, ToolId::ThreePointRectangle],
    &[ToolId::Ellipse, ToolId::ThreePointEllipse],
    &[
        ToolId::Polygon,
        ToolId::Star,
        ToolId::ComplexStar,
        ToolId::Spiral,
        ToolId::GraphPaper,
        ToolId::Impact,
        ToolId::BasicShapes,
    ],
    &[ToolId::ArtisticText, ToolId::ParagraphText, ToolId::Table],
    &[
        ToolId::ParallelDimension,
        ToolId::HvDimension,
        ToolId::AngularDimension,
        ToolId::SegmentDimension,
        ToolId::ThreePointCallout,
        ToolId::StraightConnector,
        ToolId::RightAngleConnector,
    ],
    &[
        ToolId::DropShadow,
        ToolId::BlockShadow,
        ToolId::Contour,
        ToolId::Blend,
        ToolId::Distort,
        ToolId::Envelope,
        ToolId::Extrude25D,
        ToolId::Transparency,
    ],
    &[ToolId::ColorEyedropper, ToolId::AttributesEyedropper],
    &[ToolId::InteractiveFill, ToolId::MeshFill, ToolId::SmartFill],
    &[ToolId::LensTool],
    &[
        ToolId::BitmapBrush,
        ToolId::BitmapEraser,
        ToolId::CloneStamp,
        ToolId::Smudge,
        ToolId::MaskSelect,
    ],
];

impl ToolId {
    /// Human-readable clean-room label for tooltips, flyouts, and Command Palette.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pick => "Pick Tool",
            Self::FreehandPick => "Freehand Pick",
            Self::FreeTransform => "Free Transform",
            Self::ShapeNode => "Shape (Node Editor)",
            Self::Smooth => "Smooth Sculpt",
            Self::Smear => "Smear Sculpt",
            Self::Twirl => "Twirl Sculpt",
            Self::AttractRepel => "Attract & Repel",
            Self::Roughen => "Roughen Edge",
            Self::Crop => "Crop",
            Self::Knife => "Knife Cut",
            Self::VirtualSegmentDelete => "Virtual Segment Delete",
            Self::Eraser => "Vector Eraser",
            Self::Zoom => "Zoom",
            Self::Pan => "Pan",
            Self::Freehand => "Freehand Curve",
            Self::TwoPointLine => "2-Point Line",
            Self::Bezier => "Bézier Curve",
            Self::Pen => "Pen Tool",
            Self::BSpline => "B-Spline",
            Self::Polyline => "Polyline",
            Self::ThreePointCurve => "3-Point Curve",
            Self::SmartDrawing => "Smart Drawing",
            Self::LiveSketch => "LiveSketch",
            Self::ArtisticMedia => "Artistic Media",
            Self::Rectangle => "Rectangle",
            Self::ThreePointRectangle => "3-Point Rectangle",
            Self::Ellipse => "Ellipse",
            Self::ThreePointEllipse => "3-Point Ellipse",
            Self::Polygon => "Polygon",
            Self::Star => "Star",
            Self::ComplexStar => "Complex Star",
            Self::Spiral => "Spiral",
            Self::GraphPaper => "Graph Paper",
            Self::Impact => "Impact Rays",
            Self::BasicShapes => "Basic Shapes",
            Self::ArtisticText => "Artistic Text",
            Self::ParagraphText => "Paragraph Text",
            Self::Table => "Vector Table",
            Self::ParallelDimension => "Parallel Dimension",
            Self::HvDimension => "Horizontal/Vertical Dimension",
            Self::AngularDimension => "Angular Dimension",
            Self::SegmentDimension => "Segment Dimension",
            Self::ThreePointCallout => "3-Point Callout",
            Self::StraightConnector => "Straight Connector",
            Self::RightAngleConnector => "Right-Angle Connector",
            Self::DropShadow => "Drop Shadow",
            Self::BlockShadow => "Block Shadow",
            Self::Contour => "Contour",
            Self::Blend => "Blend",
            Self::Distort => "Distort",
            Self::Envelope => "Envelope",
            Self::Extrude25D => "Extrude 2.5D",
            Self::Transparency => "Transparency",
            Self::ColorEyedropper => "Color Eyedropper",
            Self::AttributesEyedropper => "Attributes Eyedropper",
            Self::InteractiveFill => "Interactive Fill",
            Self::MeshFill => "Mesh Fill",
            Self::SmartFill => "Smart Fill",
            Self::LensTool => "Interactive Lens",
            Self::BitmapBrush => "Bitmap Brush",
            Self::BitmapEraser => "Bitmap Eraser",
            Self::CloneStamp => "Clone Stamp",
            Self::Smudge => "Smudge Brush",
            Self::MaskSelect => "Mask Select",
        }
    }

    /// Returns which of the 15 Toolbox Flyout groups (`0..15`) owns this tool.
    #[must_use]
    pub fn group_index(self) -> usize {
        for (idx, group) in TOOL_GROUPS.iter().enumerate() {
            if group.contains(&self) {
                return idx;
            }
        }
        0
    }
}

/// Smoothness continuity rule for `F10` Shape/Node Property Bar controls.
pub type NodeSmoothness = NodeKind;

/// Raster studio brush sub-mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RasterBrushOp {
    #[default]
    Paint,
    Eraser,
    CloneStamp,
    Smudge,
    Healing,
    Dodge,
    Burn,
    FloodFill,
}

impl RasterBrushOp {
    /// All 8 interactive raster brush / fill operations.
    pub const ALL: [Self; 8] = [
        Self::Paint,
        Self::Eraser,
        Self::CloneStamp,
        Self::Smudge,
        Self::Healing,
        Self::Dodge,
        Self::Burn,
        Self::FloodFill,
    ];

    /// Display label for toolbars and Bitmap Studio controls.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Paint => "Paint Brush",
            Self::Eraser => "Bitmap Eraser",
            Self::CloneStamp => "Clone Stamp",
            Self::Smudge => "Smudge",
            Self::Healing => "Healing Brush",
            Self::Dodge => "Dodge (Lighten)",
            Self::Burn => "Burn (Darken)",
            Self::FloodFill => "Flood Fill",
        }
    }
}

/// Identifies an interactive parametric shape handle dragged in `F10` Shape mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParametricHandleKind {
    RectCorner(usize),
    EllipseAngle { is_end: bool },
    PolygonVertex,
    StarOuter,
    StarInner,
}

/// Which resize handle is being dragged on the selection bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

/// Which component of a Bézier node is being dragged in `F10` Shape/Node mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeDragPart {
    Anchor,
    HandleIn,
    HandleOut,
}

/// Read-only snapshot of tool parameters passed to [`ToolCore`](crate::ToolCore).
#[derive(Debug, Clone)]
pub struct ToolSettingsSnapshot {
    pub rect_corner_radius_mm: f64,
    pub rect_corner_style: CornerStyle,
    pub ellipse_mode: EllipseMode,
    pub polygon_sides: u32,
    pub star_points: u32,
    pub star_inner_ratio: f64,
    pub spiral_revolutions: f64,
    pub spiral_logarithmic: bool,
    pub graph_cols: u32,
    pub graph_rows: u32,
    pub impact_rays: u32,
    pub impact_style: ImpactStyle,
    pub basic_shape_kind: BasicShapeKind,
    pub table_rows: usize,
    pub table_cols: usize,
    pub artistic_media_width_mm: f64,
    pub artistic_media_profile: ArtisticWidthProfile,
    pub sculpt_radius_mm: f64,
    pub sculpt_strength: f64,
    pub eraser_radius_mm: f64,
    pub brush_radius_px: f32,
    pub raster_op: RasterBrushOp,
    pub clone_source_mm: Option<Point2D>,
    pub mask_mode: RasterMaskMode,
    pub mask_feather_px: f32,
    pub magic_wand_tolerance: f32,
    pub magic_wand_contiguous: bool,
    pub mask_inverted: bool,
    pub eyedropper_apply_mode: bool,
    pub sampled_effects: Vec<Effect>,
    pub text_insert_draft: String,
    pub dimension_offset_mm: f64,
    pub grad_start_color: Color,
    pub grad_end_color: Color,
    pub mesh_rows: usize,
    pub mesh_cols: usize,
    pub effect_dx_mm: f64,
    pub effect_dy_mm: f64,
    pub effect_blur_mm: f64,
    pub effect_opacity: f32,
    pub effect_color: Color,
    pub effect_contour_mode: ContourMode,
    pub effect_steps: u32,
    pub effect_distort_kind: DistortKind,
    pub effect_frequency: f64,
    pub effect_envelope_mode: EnvelopeMode,
    pub effect_blend_mode: BlendMode,
    pub symmetry_enabled: bool,
    pub symmetry_folds: u32,
    pub symmetry_mirror: bool,
    pub symmetry_center_mm: Option<Point2D>,
    pub draw_in_perspective: bool,
    pub pressure_curve_json: String,
}

impl Default for ToolSettingsSnapshot {
    fn default() -> Self {
        Self {
            rect_corner_radius_mm: 0.0,
            rect_corner_style: CornerStyle::Rounded,
            ellipse_mode: EllipseMode::Full,
            polygon_sides: 5,
            star_points: 5,
            star_inner_ratio: 0.45,
            spiral_revolutions: 4.0,
            spiral_logarithmic: false,
            graph_cols: 4,
            graph_rows: 3,
            impact_rays: 16,
            impact_style: ImpactStyle::Radial,
            basic_shape_kind: BasicShapeKind::ArrowRight,
            table_rows: 3,
            table_cols: 3,
            artistic_media_width_mm: 4.0,
            artistic_media_profile: ArtisticWidthProfile::TaperBoth,
            sculpt_radius_mm: 10.0,
            sculpt_strength: 0.35,
            eraser_radius_mm: 5.0,
            brush_radius_px: 12.0,
            raster_op: RasterBrushOp::Paint,
            clone_source_mm: None,
            mask_mode: RasterMaskMode::Rectangle,
            mask_feather_px: 0.0,
            magic_wand_tolerance: 24.0,
            magic_wand_contiguous: true,
            mask_inverted: false,
            eyedropper_apply_mode: false,
            sampled_effects: Vec::new(),
            text_insert_draft: "coree Studio".to_owned(),
            dimension_offset_mm: 8.0,
            grad_start_color: Color::CYAN,
            grad_end_color: Color::MAGENTA,
            mesh_rows: 2,
            mesh_cols: 2,
            effect_dx_mm: 3.0,
            effect_dy_mm: 3.0,
            effect_blur_mm: 4.0,
            effect_opacity: 0.6,
            effect_color: Color::BLACK,
            effect_contour_mode: ContourMode::Outside,
            effect_steps: 3,
            effect_distort_kind: DistortKind::Twister,
            effect_frequency: 2.0,
            effect_envelope_mode: EnvelopeMode::SingleArc,
            effect_blend_mode: BlendMode::Multiply,
            symmetry_enabled: false,
            symmetry_folds: 4,
            symmetry_mirror: true,
            symmetry_center_mm: None,
            draw_in_perspective: false,
            pressure_curve_json: "[[0.0,0.0],[1.0,1.0]]".to_owned(),
        }
    }
}
