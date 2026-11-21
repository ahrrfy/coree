// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

// =============================================================================
// 15 Toolbox Flyout Groups & Complete Tool Registry
// =============================================================================

/// Every interactive studio tool across all 15 Toolbox Flyout groups.
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

    /// Associated shortcut action if one exists.
    #[must_use]
    pub const fn shortcut_action(self) -> Option<ShortcutAction> {
        match self {
            Self::Pick => Some(ShortcutAction::SelectPick),
            Self::ShapeNode => Some(ShortcutAction::SelectShapeNode),
            Self::Zoom => Some(ShortcutAction::SelectZoom),
            Self::Pan => Some(ShortcutAction::SelectPan),
            Self::Freehand => Some(ShortcutAction::SelectFreehand),
            Self::Pen => Some(ShortcutAction::SelectPen),
            Self::Rectangle => Some(ShortcutAction::SelectRectangle),
            Self::Ellipse => Some(ShortcutAction::SelectEllipse),
            Self::Polygon => Some(ShortcutAction::SelectPolygon),
            Self::Spiral => Some(ShortcutAction::SelectSpiral),
            Self::GraphPaper => Some(ShortcutAction::SelectGraphPaper),
            Self::ArtisticText => Some(ShortcutAction::SelectArtisticText),
            Self::ColorEyedropper => Some(ShortcutAction::SelectEyedropper),
            Self::InteractiveFill => Some(ShortcutAction::SelectInteractiveFill),
            Self::MeshFill => Some(ShortcutAction::SelectMeshFill),
            _ => None,
        }
    }
}

// =============================================================================
// Dockable Studio Panels (`egui_dock`)
// =============================================================================

/// Rearrangeable / dockable / floatable studio panel identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DockerTab {
    Objects,
    Properties,
    Lens,
    Effects,
    TransformAlignShape,
    VectorTrace,
    BitmapStudio,
    Production,
    FindReplace,
    SymbolsStyles,
    FontManager,
    History,
    Macros,
    CdrInspector,
}

impl DockerTab {
    /// All 14 studio dockers in menu order.
    pub const ALL: [Self; 14] = [
        Self::Objects,
        Self::Properties,
        Self::Lens,
        Self::Effects,
        Self::TransformAlignShape,
        Self::VectorTrace,
        Self::BitmapStudio,
        Self::Production,
        Self::FindReplace,
        Self::SymbolsStyles,
        Self::FontManager,
        Self::History,
        Self::Macros,
        Self::CdrInspector,
    ];

    /// Short tab title displayed in `egui_dock`.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Objects => "Objects",
            Self::Properties => "Properties",
            Self::Lens => "Lens",
            Self::Effects => "Effects",
            Self::TransformAlignShape => "Transform",
            Self::VectorTrace => "Vector Trace",
            Self::BitmapStudio => "Bitmap Studio",
            Self::Production => "Production",
            Self::FindReplace => "Find & Replace",
            Self::SymbolsStyles => "Symbols",
            Self::FontManager => "Fonts",
            Self::History => "History",
            Self::Macros => "Macros",
            Self::CdrInspector => "CDR Inspector",
        }
    }
}

/// Builds the default right-side docker layout (`Objects` + `Properties` + `Lens` + `Effects`
/// stacked cleanly in a split top/bottom dock tree).
#[must_use]
pub fn default_dock_state() -> DockState<DockerTab> {
    let mut state = DockState::new(vec![
        DockerTab::Objects,
        DockerTab::Properties,
        DockerTab::Lens,
        DockerTab::Effects,
        DockerTab::Production,
    ]);
    let surface = state.main_surface_mut();
    surface.split_below(
        NodeIndex::root(),
        0.58,
        vec![
            DockerTab::TransformAlignShape,
            DockerTab::History,
            DockerTab::VectorTrace,
            DockerTab::BitmapStudio,
            DockerTab::Macros,
        ],
    );
    state
}

// =============================================================================
// Workspace Presets, Raster Brush Sub-Modes & Shape Smoothness Alias
// =============================================================================

/// Smoothness continuity rule for `F10` Shape/Node Property Bar controls.
pub type NodeSmoothness = NodeKind;

/// Raster studio brush sub-mode (keeps `ToolId` 100% backwards-compatible while
/// enabling Paint, Eraser, Clone Stamp, Smudge, Healing, Dodge, Burn, and Flood Fill).
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

/// Switchable clean-room studio workspace presets (exportable/importable as `.coreews` JSON).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum WorkspacePreset {
    #[default]
    DefaultClassic,
    Lite,
    Illustration,
    PageLayout,
    LaserSignStudio,
    TouchCompact,
}

impl WorkspacePreset {
    /// All 6 built-in workspace presets.
    pub const ALL: [Self; 6] = [
        Self::DefaultClassic,
        Self::Lite,
        Self::Illustration,
        Self::PageLayout,
        Self::LaserSignStudio,
        Self::TouchCompact,
    ];

    /// Human-readable workspace name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DefaultClassic => "Default Classic",
            Self::Lite => "Lite",
            Self::Illustration => "Illustration",
            Self::PageLayout => "Page Layout",
            Self::LaserSignStudio => "Laser & Sign Studio",
            Self::TouchCompact => "Touch Compact",
        }
    }

    /// Short description for Preferences & Workspace switcher.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::DefaultClassic => "Balanced vector graphics, color & prepress studio layout.",
            Self::Lite => "Minimal uncluttered canvas with Objects and Properties only.",
            Self::Illustration => {
                "Focused drawing workspace with Effects, Lens, Symbols & History."
            }
            Self::PageLayout => "Multi-page publishing with Fonts, Find & Replace & Production.",
            Self::LaserSignStudio => {
                "Cut/engrave workflow with Production, Transform & CDR Inspector."
            }
            Self::TouchCompact => "Two-column toolbox and compact dockers for smaller displays.",
        }
    }
}

const fn default_true_persist() -> bool {
    true
}

/// Persisted toolbox & ergonomics subset serialized into `WorkspaceLayoutRecord::toolbox_config_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ToolboxPersistConfig {
    #[serde(default)]
    pub(crate) preset: WorkspacePreset,
    #[serde(default)]
    pub(crate) toolbox_two_column: bool,
    #[serde(default = "default_true_persist")]
    pub(crate) show_dockers: bool,
    #[serde(default)]
    pub(crate) palette_show_names: bool,
    #[serde(default)]
    pub(crate) treat_all_objects_as_filled: bool,
    #[serde(default)]
    pub(crate) object_origin_anchor: (u8, u8),
    #[serde(default)]
    pub(crate) group_active_tools: Option<[ToolId; 15]>,
}

/// Serializable `.coreews` workspace configuration bundle (portable across headless & desktop sessions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkspaceBundle {
    pub(crate) version: u32,
    pub(crate) preset: WorkspacePreset,
    pub(crate) theme: StudioTheme,
    pub(crate) shortcut_profile: ShortcutProfile,
    pub(crate) toolbox_two_column: bool,
    pub(crate) show_dockers: bool,
    pub(crate) palette_show_names: bool,
    pub(crate) treat_all_objects_as_filled: bool,
    pub(crate) object_origin_anchor: (u8, u8),
    pub(crate) open_dockers: Vec<DockerTab>,
    #[serde(default)]
    pub(crate) ui_language: StudioLanguage,
    #[serde(default)]
    pub(crate) dock_state: Option<DockState<DockerTab>>,
    #[serde(default)]
    pub(crate) group_active_tools: Option<[ToolId; 15]>,
    #[serde(default)]
    pub(crate) custom_shortcuts: CustomShortcutMap,
}

/// Live on-canvas inline text editing state (`ArtisticText` / `ParagraphText`).
#[derive(Debug, Clone)]
pub struct InlineTextEditState {
    pub object_id: Option<ObjectId>,
    pub world_pos_mm: Point2D,
    pub buffer: String,
    pub is_paragraph: bool,
    pub box_size_mm: Vec2D,
    pub just_opened: bool,
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

// =============================================================================
// Interactive Canvas Drag Gesture State
// =============================================================================

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

/// Active interactive gesture on the central canvas.
#[derive(Debug, Clone, Default)]
pub enum CanvasDragState {
    #[default]
    Idle,
    Panning {
        start_screen: [f32; 2],
        orig_pan_mm: Point2D,
    },
    MarqueeSelect {
        start_mm: Point2D,
        current_mm: Point2D,
    },
    LassoSelect {
        points_mm: Vec<Point2D>,
    },
    MovingSelection {
        start_mm: Point2D,
        last_mm: Point2D,
        accum_dx: f64,
        accum_dy: f64,
    },
    ScalingSelection {
        handle: ScaleHandle,
        orig_bounds: Rect2D,
        current_mm: Point2D,
        orig_transforms: Vec<(ObjectId, Affine2D)>,
        last_affine: Affine2D,
    },
    RotatingSelection {
        pivot_mm: Point2D,
        start_angle_rad: f64,
        current_angle_rad: f64,
        orig_transforms: Vec<(ObjectId, Affine2D)>,
        last_affine: Affine2D,
    },
    SkewingSelection {
        horizontal: bool,
        pivot_mm: Point2D,
        start_mm: Point2D,
        current_mm: Point2D,
        orig_transforms: Vec<(ObjectId, Affine2D)>,
        last_affine: Affine2D,
    },
    FreeTransforming {
        anchor_mm: Point2D,
        start_mm: Point2D,
        current_mm: Point2D,
        orig_transforms: Vec<(ObjectId, Affine2D)>,
        last_affine: Affine2D,
    },
    DraggingPivot,
    DraggingNode {
        node: NodeRef,
        handle_part: NodeDragPart,
        orig_paths: Vec<(ObjectId, BezierPath)>,
        last_world_mm: Point2D,
    },
    NodeMarqueeSelect {
        start_mm: Point2D,
        current_mm: Point2D,
    },
    DraggingCurveSegment {
        object_id: ObjectId,
        subpath_index: usize,
        seg_index: usize,
        t: f64,
        last_local_mm: Point2D,
        orig_path: BezierPath,
    },
    DraggingParametricHandle {
        object_id: ObjectId,
        handle: ParametricHandleKind,
        orig_shape: ParametricShape,
    },
    Sculpting {
        object_id: ObjectId,
        orig_payload: NodePayload,
        last_mm: Point2D,
        points_mm: Vec<Point2D>,
        step_counter: u64,
    },
    ZoomMarquee {
        start_mm: Point2D,
        current_mm: Point2D,
    },
    CropBoxHandle {
        handle: ScaleHandle,
        orig_rect: Rect2D,
    },
    PullingBezierHandle {
        anchor_mm: Point2D,
        handle_out_mm: Point2D,
        closing: bool,
    },
    DraggingGradientHandle {
        object_id: ObjectId,
        is_end: bool,
        orig_fill: Fill,
    },
    DraggingGradientFocal {
        object_id: ObjectId,
        orig_fill: Fill,
    },
    DraggingGradientStop {
        object_id: ObjectId,
        stop_index: usize,
        orig_fill: Fill,
    },
    DraggingEffectHandle {
        object_id: ObjectId,
        effect_index: usize,
        orig_effect: Effect,
    },
    DraggingCanvasGuide {
        guide_id: u64,
        orig_orientation: GuideOrientation,
    },
    DraggingTransparencyHandle {
        object_id: ObjectId,
        effect_index: usize,
        is_end: bool,
        orig_effect: Effect,
    },
    DraggingInteractiveEffect {
        object_id: ObjectId,
        start_mm: Point2D,
        current_mm: Point2D,
    },
    DraggingEnvelopeHandle {
        object_id: ObjectId,
        effect_index: usize,
        handle_index: usize,
        orig_effect: Effect,
    },
    DraggingMeshNode {
        object_id: ObjectId,
        node_index: usize,
        orig_fill: Fill,
    },
    DrawingBox {
        start_mm: Point2D,
        current_mm: Point2D,
    },
    DrawingStroke {
        points_mm: Vec<Point2D>,
    },
    DraggingRulerGuide {
        horizontal: bool,
        position_mm: f64,
    },
}

/// Which component of a Bézier node is being dragged in `F10` Shape/Node mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeDragPart {
    Anchor,
    HandleIn,
    HandleOut,
}

// =============================================================================
// Tool, Docker & Dialog Form States
// =============================================================================

/// Configurable parameters for active drawing & sculpting tools.
#[derive(Debug, Clone)]
pub struct ToolSettings {
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
    pub brush_hardness: f32,
    pub brush_color: Color,
    pub smudge_strength: f32,
    pub raster_op: RasterBrushOp,
    pub dodge_burn_tone: ToneRange,
    pub dodge_burn_exposure: f32,
    pub flood_tolerance: u8,
    pub clone_source_mm: Option<Point2D>,
    pub eyedropper_apply_mode: bool,
    pub sampled_effects: Vec<Effect>,
    pub shape_reduce_tolerance_mm: f64,
    pub text_insert_draft: String,
    pub dimension_offset_mm: f64,
}

impl Default for ToolSettings {
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
            brush_hardness: 0.8,
            brush_color: Color::CYAN,
            smudge_strength: 0.5,
            raster_op: RasterBrushOp::Paint,
            dodge_burn_tone: ToneRange::Midtones,
            dodge_burn_exposure: 0.35,
            flood_tolerance: 24,
            clone_source_mm: None,
            eyedropper_apply_mode: false,
            sampled_effects: Vec::new(),
            shape_reduce_tolerance_mm: 0.5,
            text_insert_draft: "coree Studio".to_owned(),
            dimension_offset_mm: 8.0,
        }
    }
}

/// Active sub-tab in the Properties docker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PropertiesSubTab {
    #[default]
    Fill,
    Outline,
    Transparency,
    Text,
}

/// State for the Properties Docker (`S06`).
#[derive(Debug, Clone)]
pub struct PropertiesDockerState {
    pub active_tab: PropertiesSubTab,
    pub color_space_tab: usize, // 0=CMYK, 1=RGB, 2=HSL, 3=Spot
    pub hex_input: String,
    pub spot_name: String,
    pub spot_tint: f32,
    pub grad_start_color: Color,
    pub grad_end_color: Color,
    pub mesh_rows: usize,
    pub mesh_cols: usize,
}

impl Default for PropertiesDockerState {
    fn default() -> Self {
        Self {
            active_tab: PropertiesSubTab::Fill,
            color_space_tab: 0,
            hex_input: "#3ea6ff".to_owned(),
            spot_name: "CutContour".to_owned(),
            spot_tint: 1.0,
            grad_start_color: Color::CYAN,
            grad_end_color: Color::MAGENTA,
            mesh_rows: 2,
            mesh_cols: 2,
        }
    }
}

/// State for the 11-Lens Inspector Docker (`S09`).
#[derive(Debug, Clone)]
pub struct LensDockerState {
    pub selected_index: usize, // 0..11 across all 11 LensKind variants
    pub rate_pct: f32,
    pub magnify_amount: f32,
    pub heat_rotation_deg: f32,
    pub tint_color: Color,
    pub from_color: Color,
    pub to_color: Color,
    pub map_mode: ColorMapMode,
    pub wireframe_outline: Color,
    pub wireframe_fill: Color,
    pub use_wireframe_fill: bool,
    pub frozen: bool,
    pub use_viewpoint: bool,
    pub viewpoint_x: f64,
    pub viewpoint_y: f64,
    pub remove_face: bool,
}

impl Default for LensDockerState {
    fn default() -> Self {
        Self {
            selected_index: 7, // Magnify
            rate_pct: 50.0,
            magnify_amount: 2.0,
            heat_rotation_deg: 0.0,
            tint_color: Color::CYAN,
            from_color: Color::rgb(0.1, 0.05, 0.4),
            to_color: Color::rgb(1.0, 0.85, 0.2),
            map_mode: ColorMapMode::Direct,
            wireframe_outline: Color::BLACK,
            wireframe_fill: Color::WHITE,
            use_wireframe_fill: false,
            frozen: false,
            use_viewpoint: false,
            viewpoint_x: 50.0,
            viewpoint_y: 50.0,
            remove_face: false,
        }
    }
}

impl LensDockerState {
    /// All 11 lens display names in exact order.
    pub const NAMES: [&'static str; 11] = [
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
        "Wireframe",
    ];

    /// Builds a [`LensConfig`] from the current docker controls.
    #[must_use]
    pub fn build_config(&self) -> LensConfig {
        let kind = match self.selected_index {
            0 => LensKind::Brighten {
                rate_pct: self.rate_pct.clamp(-100.0, 100.0),
            },
            1 => LensKind::ColorAdd {
                color: self.tint_color.clone(),
                rate_pct: self.rate_pct.clamp(0.0, 100.0),
            },
            2 => LensKind::ColorLimit {
                color: self.tint_color.clone(),
                rate_pct: self.rate_pct.clamp(0.0, 100.0),
            },
            3 => LensKind::CustomColorMap {
                from_color: self.from_color.clone(),
                to_color: self.to_color.clone(),
                palette_mode: self.map_mode,
            },
            4 => LensKind::FishEye {
                rate_pct: self.rate_pct.clamp(-1000.0, 1000.0),
            },
            5 => LensKind::HeatMap {
                palette_rotation_deg: self.heat_rotation_deg,
            },
            6 => LensKind::Invert,
            7 => LensKind::Magnify {
                amount: self.magnify_amount.clamp(1.0, 100.0),
            },
            8 => LensKind::TintedGrayscale {
                tint: self.tint_color.clone(),
            },
            9 => LensKind::Transparency {
                color: self.tint_color.clone(),
                rate_pct: self.rate_pct.clamp(0.0, 100.0),
            },
            _ => LensKind::Wireframe {
                override_outline: Some(self.wireframe_outline.clone()),
                override_fill: self.use_wireframe_fill.then(|| self.wireframe_fill.clone()),
            },
        };
        LensConfig {
            kind: kind.clamped(),
            frozen: self.frozen,
            viewpoint: self
                .use_viewpoint
                .then(|| Point2D::new(self.viewpoint_x, self.viewpoint_y)),
            remove_face: self.remove_face,
        }
    }

    /// Loads an existing [`LensConfig`] into the docker controls.
    pub fn load_from(&mut self, cfg: &LensConfig) {
        self.frozen = cfg.frozen;
        self.remove_face = cfg.remove_face;
        if let Some(vp) = cfg.viewpoint {
            self.use_viewpoint = true;
            self.viewpoint_x = vp.x;
            self.viewpoint_y = vp.y;
        } else {
            self.use_viewpoint = false;
        }
        match &cfg.kind {
            LensKind::Brighten { rate_pct } => {
                self.selected_index = 0;
                self.rate_pct = *rate_pct;
            }
            LensKind::ColorAdd { color, rate_pct } => {
                self.selected_index = 1;
                self.tint_color = color.clone();
                self.rate_pct = *rate_pct;
            }
            LensKind::ColorLimit { color, rate_pct } => {
                self.selected_index = 2;
                self.tint_color = color.clone();
                self.rate_pct = *rate_pct;
            }
            LensKind::CustomColorMap {
                from_color,
                to_color,
                palette_mode,
            } => {
                self.selected_index = 3;
                self.from_color = from_color.clone();
                self.to_color = to_color.clone();
                self.map_mode = *palette_mode;
            }
            LensKind::FishEye { rate_pct } => {
                self.selected_index = 4;
                self.rate_pct = *rate_pct;
            }
            LensKind::HeatMap {
                palette_rotation_deg,
            } => {
                self.selected_index = 5;
                self.heat_rotation_deg = *palette_rotation_deg;
            }
            LensKind::Invert => {
                self.selected_index = 6;
            }
            LensKind::Magnify { amount } => {
                self.selected_index = 7;
                self.magnify_amount = *amount;
            }
            LensKind::TintedGrayscale { tint } => {
                self.selected_index = 8;
                self.tint_color = tint.clone();
            }
            LensKind::Transparency { color, rate_pct } => {
                self.selected_index = 9;
                self.tint_color = color.clone();
                self.rate_pct = *rate_pct;
            }
            LensKind::Wireframe {
                override_outline,
                override_fill,
            } => {
                self.selected_index = 10;
                if let Some(o) = override_outline {
                    self.wireframe_outline = o.clone();
                }
                if let Some(f) = override_fill {
                    self.use_wireframe_fill = true;
                    self.wireframe_fill = f.clone();
                } else {
                    self.use_wireframe_fill = false;
                }
            }
        }
    }
}

/// State for the Interactive Vector Effects Docker (`S08`).
#[derive(Debug, Clone)]
pub struct EffectsDockerState {
    pub selected_effect_kind: usize,
    pub dx_mm: f64,
    pub dy_mm: f64,
    pub blur_mm: f64,
    pub opacity: f32,
    pub color: Color,
    pub contour_mode: ContourMode,
    pub steps: u32,
    pub offset_mm: f64,
    pub distort_kind: DistortKind,
    pub amplitude: f64,
    pub frequency: f64,
    pub envelope_mode: EnvelopeMode,
    pub extrude_depth: f64,
    pub symmetry_axes: u32,
    pub symmetry_mirror: bool,
    pub blend_mode: BlendMode,
}

impl Default for EffectsDockerState {
    fn default() -> Self {
        Self {
            selected_effect_kind: 0,
            dx_mm: 3.0,
            dy_mm: 3.0,
            blur_mm: 4.0,
            opacity: 0.6,
            color: Color::BLACK,
            contour_mode: ContourMode::Outside,
            steps: 3,
            offset_mm: 2.0,
            distort_kind: DistortKind::Twister,
            amplitude: 15.0,
            frequency: 2.0,
            envelope_mode: EnvelopeMode::SingleArc,
            extrude_depth: 12.0,
            symmetry_axes: 4,
            symmetry_mirror: true,
            blend_mode: BlendMode::Multiply,
        }
    }
}

/// Numeric Transformations, Align & Shaping Docker state.
#[derive(Debug, Clone)]
pub struct TransformDockerState {
    pub move_dx_mm: f64,
    pub move_dy_mm: f64,
    pub rotate_deg: f64,
    pub scale_x_pct: f64,
    pub scale_y_pct: f64,
    pub skew_x_deg: f64,
    pub skew_y_deg: f64,
    pub copies: u32,
}

impl Default for TransformDockerState {
    fn default() -> Self {
        Self {
            move_dx_mm: 10.0,
            move_dy_mm: 0.0,
            rotate_deg: 15.0,
            scale_x_pct: 100.0,
            scale_y_pct: 100.0,
            skew_x_deg: 0.0,
            skew_y_deg: 0.0,
            copies: 0,
        }
    }
}

/// Vector Trace Docker state (`S11`).
#[derive(Debug, Clone)]
pub struct TraceDockerState {
    pub mode_idx: usize, // 0=Quick, 1=Outline, 2=Centerline
    pub preset: OutlinePreset,
    pub max_colors: usize,
    pub smoothing: f64,
    pub detail: f64,
    pub remove_background: bool,
    pub corner_threshold: f64,
}

impl Default for TraceDockerState {
    fn default() -> Self {
        Self {
            mode_idx: 1,
            preset: OutlinePreset::Logo,
            max_colors: 8,
            smoothing: 0.5,
            detail: 0.75,
            remove_background: true,
            corner_threshold: 60.0,
        }
    }
}

impl TraceDockerState {
    /// Converts UI controls into `coree_render::TraceConfig`.
    #[must_use]
    pub fn to_config(&self) -> TraceConfig {
        let mode = match self.mode_idx {
            0 => TraceMode::QuickTrace,
            2 => TraceMode::CenterlineTrace,
            _ => TraceMode::OutlineTrace(self.preset),
        };
        TraceConfig {
            mode,
            max_colors: self.max_colors.clamp(2, 64),
            smoothing: self.smoothing.clamp(0.0, 1.0),
            detail: self.detail.clamp(0.0, 1.0),
            remove_background: self.remove_background,
            corner_threshold: self.corner_threshold.clamp(0.0, 180.0),
        }
    }
}

/// Bitmap Studio Docker state (`S10`) covering all 19 non-destructive `RasterFilter` variants.
#[derive(Debug, Clone)]
pub struct BitmapDockerState {
    pub filter_idx: usize,
    pub param_a: f32,
    pub param_b: f32,
    pub param_c: f32,
    pub resample_w: u32,
    pub resample_h: u32,
    pub convert_dpi: f64,
}

impl Default for BitmapDockerState {
    fn default() -> Self {
        Self {
            filter_idx: 0,
            param_a: 0.15,
            param_b: 0.15,
            param_c: 1.0,
            resample_w: 512,
            resample_h: 512,
            convert_dpi: 300.0,
        }
    }
}

impl BitmapDockerState {
    /// All 19 non-destructive raster filters.
    pub const FILTER_NAMES: [&'static str; 19] = [
        "Brightness / Contrast",
        "Tone Curve",
        "Levels",
        "HSL Adjust",
        "Color Balance",
        "Replace Color",
        "Gamma",
        "Desaturate",
        "Invert",
        "Posterize",
        "Threshold",
        "Gaussian Blur",
        "Motion Blur",
        "Unsharp Mask",
        "Sharpen",
        "Noise",
        "Vignette",
        "Emboss",
        "Edge Detect",
    ];

    /// Builds a [`RasterFilter`] matching the selected index and slider parameters.
    #[must_use]
    pub fn build_filter(&self) -> RasterFilter {
        match self.filter_idx {
            0 => RasterFilter::BrightnessContrast {
                brightness: self.param_a.clamp(-1.0, 1.0),
                contrast: self.param_b.clamp(-1.0, 1.0),
            },
            1 => RasterFilter::ToneCurve {
                points: vec![
                    (0.0, 0.0),
                    (0.5, (0.5 + self.param_a * 0.4).clamp(0.05, 0.95)),
                    (1.0, 1.0),
                ],
            },
            2 => RasterFilter::Levels {
                in_black: self.param_a.clamp(0.0, 0.45),
                in_gamma: self.param_c.clamp(0.2, 4.0),
                in_white: (1.0 - self.param_b.clamp(0.0, 0.45)),
                out_black: 0.0,
                out_white: 1.0,
            },
            3 => RasterFilter::Hsl {
                hue_deg: self.param_a * 180.0,
                saturation: self.param_b.clamp(-1.0, 1.0),
                lightness: (self.param_c - 1.0).clamp(-1.0, 1.0),
            },
            4 => RasterFilter::ColorBalance {
                cyan_red: self.param_a.clamp(-1.0, 1.0),
                magenta_green: self.param_b.clamp(-1.0, 1.0),
                yellow_blue: (self.param_c - 1.0).clamp(-1.0, 1.0),
            },
            5 => RasterFilter::ReplaceColor {
                target: Color::WHITE,
                replacement: Color::CYAN,
                tolerance: self.param_a.abs().clamp(0.05, 1.0),
            },
            6 => RasterFilter::Gamma {
                gamma: self.param_c.clamp(0.2, 4.0),
            },
            7 => RasterFilter::Desaturate,
            8 => RasterFilter::Invert,
            9 => RasterFilter::Posterize {
                levels: (self.param_c.round() as u8).clamp(2, 16),
            },
            10 => RasterFilter::Threshold {
                cutoff: ((self.param_a * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0) as u8,
            },
            11 => RasterFilter::GaussianBlur {
                radius_px: self.param_c.clamp(0.5, 25.0),
            },
            12 => RasterFilter::MotionBlur {
                distance_px: (self.param_c * 4.0).clamp(1.0, 40.0),
                angle_deg: self.param_a * 180.0,
            },
            13 => RasterFilter::UnsharpMask {
                amount: self.param_c.clamp(0.1, 3.0),
                radius_px: self.param_b.abs().clamp(0.5, 10.0),
                threshold: 4,
            },
            14 => RasterFilter::Sharpen {
                amount: self.param_c.clamp(0.1, 3.0),
            },
            15 => RasterFilter::Noise {
                amount: self.param_a.abs().clamp(0.01, 0.8),
                monochrome: false,
                seed: 42,
            },
            16 => RasterFilter::Vignette {
                strength: self.param_a.abs().clamp(0.1, 1.0),
                radius: 0.75,
                feather: 0.35,
            },
            17 => RasterFilter::Emboss {
                angle_deg: self.param_a * 180.0,
                depth: self.param_c.clamp(0.5, 5.0),
            },
            _ => RasterFilter::EdgeDetect {
                sensitivity: self.param_c.clamp(0.2, 4.0),
            },
        }
    }
}

/// Production & Prepress Docker state (`S12`, `S13`, `S14`).
#[derive(Debug, Clone)]
pub struct ProductionDockerState {
    pub sub_section: usize, // 0=Preflight, 1=Separations/Imposition, 2=Cut/Dielines, 3=Barcode/QR, 4=Merge/Collect
    pub preflight_min_dpi: f64,
    pub cut_offset_mm: f64,
    pub cut_spot_name: String,
    pub dieline_type: usize, // 0=TuckEndBox, 1=Envelope, 2=ShoppingBag, 3=StickerSheet
    pub box_l_mm: f64,
    pub box_w_mm: f64,
    pub box_h_mm: f64,
    pub sticker_cols: u32,
    pub sticker_rows: u32,
    pub barcode_symbology: BarcodeSymbology,
    pub barcode_payload: String,
    pub qr_payload: String,
    pub qr_size_mm: f64,
    pub qr_ec: QrErrorCorrection,
    pub imposition_kind: usize, // 0=NUp, 1=Booklet, 2=Tiling
    pub nup_cols: u32,
    pub nup_rows: u32,
    pub sheet_w_mm: f64,
    pub sheet_h_mm: f64,
    pub tile_w_mm: f64,
    pub tile_h_mm: f64,
    pub tile_overlap_mm: f64,
    pub merge_csv_text: String,
    pub collect_dir_text: String,
    pub last_production_note: String,
}

impl Default for ProductionDockerState {
    fn default() -> Self {
        Self {
            sub_section: 0,
            preflight_min_dpi: 300.0,
            cut_offset_mm: 2.0,
            cut_spot_name: "CutContour".to_owned(),
            dieline_type: 0,
            box_l_mm: 80.0,
            box_w_mm: 40.0,
            box_h_mm: 100.0,
            sticker_cols: 3,
            sticker_rows: 4,
            barcode_symbology: BarcodeSymbology::Ean13,
            barcode_payload: "590123412345".to_owned(),
            qr_payload: coree_core::brand::REPO_URL.to_owned(),
            qr_size_mm: 28.0,
            qr_ec: QrErrorCorrection::M,
            imposition_kind: 0,
            nup_cols: 2,
            nup_rows: 2,
            sheet_w_mm: 320.0,
            sheet_h_mm: 450.0,
            tile_w_mm: 150.0,
            tile_h_mm: 200.0,
            tile_overlap_mm: 10.0,
            merge_csv_text: "Name,Code\nAhmed,1001\nSara,1002\nOmar,1003".to_owned(),
            collect_dir_text: "coree_output_bundle".to_owned(),
            last_production_note: String::new(),
        }
    }
}

/// Find & Replace Docker state.
#[derive(Debug, Clone)]
pub struct FindReplaceState {
    pub scope: FindScope,
    pub object_kind: Option<ObjectKindFilter>,
    pub lens_filter: Option<LensKindFilter>,
    pub text_query: String,
    pub replace_text: String,
    pub case_sensitive: bool,
    pub filter_hairline: Option<bool>,
    pub font_query: String,
    pub replace_font: String,
    pub replace_fill_color: Color,
    pub last_report: Option<FindReplaceReport>,
}

impl Default for FindReplaceState {
    fn default() -> Self {
        Self {
            scope: FindScope::WholeDocument,
            object_kind: None,
            lens_filter: None,
            text_query: String::new(),
            replace_text: String::new(),
            case_sensitive: false,
            filter_hairline: None,
            font_query: String::new(),
            replace_font: "Noto Sans".to_owned(),
            replace_fill_color: Color::CYAN,
            last_report: None,
        }
    }
}

/// Font Manager Docker state (`S07`).
#[derive(Debug, Clone, Default)]
pub struct FontManagerState {
    pub search_query: String,
    pub arabic_only: bool,
    pub kashida_only: bool,
    pub favorites_only: bool,
    pub indexed_fonts: Vec<FontRecord>,
    pub scanned_once: bool,
}

/// Macro Studio Docker state (`S17`).
#[derive(Debug, Clone)]
pub struct MacroDockerState {
    pub script_source: String,
    pub output_logs: Vec<String>,
    pub last_error: Option<String>,
}

impl Default for MacroDockerState {
    fn default() -> Self {
        let default_script = builtin_macros()
            .first()
            .map(|m| m.script.trim().to_owned())
            .unwrap_or_else(|| "// Write Rhai macro here\nlog(\"Hello coree!\");".to_owned());
        Self {
            script_source: default_script,
            output_logs: Vec::new(),
            last_error: None,
        }
    }
}

/// New Document dialog configuration (`S02`).
#[derive(Debug, Clone)]
pub struct NewDocDialogState {
    pub open: bool,
    pub preset_index: usize,
    pub title: String,
    pub width_mm: f64,
    pub height_mm: f64,
    pub unit: Unit,
    pub color_mode: ColorMode,
    pub dpi: f32,
    pub bleed_mm: f64,
    pub pages: usize,
}

impl Default for NewDocDialogState {
    fn default() -> Self {
        Self {
            open: false,
            preset_index: 0,
            title: "Untitled Document".to_owned(),
            width_mm: 210.0,
            height_mm: 297.0,
            unit: Unit::Millimeter,
            color_mode: ColorMode::Cmyk,
            dpi: 300.0,
            bleed_mm: 3.0,
            pages: 1,
        }
    }
}

/// Export dialog configuration (`S15`).
#[derive(Debug, Clone)]
pub struct ExportDialogState {
    pub open: bool,
    pub format_index: usize,
    pub output_path: String,
    pub options: UniversalExportOptions,
}

impl Default for ExportDialogState {
    fn default() -> Self {
        Self {
            open: false,
            format_index: 0,
            output_path: "export.pdf".to_owned(),
            options: UniversalExportOptions::default(),
        }
    }
}

/// Command Palette modal state (`S18`).
#[derive(Debug, Clone, Default)]
pub struct CommandPaletteState {
    pub open: bool,
    pub query: String,
    pub selected_index: usize,
    pub just_opened: bool,
}

/// Non-blocking status toast notification.
#[derive(Debug, Clone)]
pub struct StudioToast {
    pub message: String,
    pub is_error: bool,
    pub created_at: Instant,
}

/// Active non-blocking background job tracked on the status bar.
pub struct ActiveBackgroundJob {
    pub handle: JobHandle<String>,
}

/// Descriptor for a searchable Command Palette action.
#[derive(Debug, Clone)]
pub struct PaletteCommand {
    pub id: String,
    pub title: String,
    pub category: String,
    pub shortcut: String,
}
