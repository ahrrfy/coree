// SPDX-License-Identifier: MPL-2.0

//! Complete headless-drivable studio application state, multi-document tab manager,
//! all 15 Toolbox Flyout groups, rearrangeable `egui_dock` layout state, SQLite
//! autosave/WAL crash recovery, `JobPool` background jobs, and studio actions.

use crate::cdr_bridge::{inspect_any_file, inspect_in_memory_document, FileInspectionReport};
use crate::io_bridge::{
    export_any_document, import_into_document, open_any_document, UniversalExportOptions,
};
use crate::shortcuts::{shortcut_label, ShortcutAction, ShortcutProfile};
use crate::theme::StudioTheme;
use coree_cdr::generate_sample_document;
use coree_core::{
    Affine2D, AlignOp, ArtisticMediaStyle, ArtisticText, ArtisticWidthProfile, BasicShapeKind,
    BezierPath, BlendMode, BooleanOp, Color, ColorMapMode, ColorMode, ColorPalette, ColorSwatch,
    CommandHistory, CommandOp, ContourMode, CornerStyle, DimensionKind, DistortKind, DistributeOp,
    Document, Effect, EllipseMode, EnvelopeMode, FeatherDirection, Fill, GuideOrientation,
    ImpactStyle, LayerId, LensConfig, LensKind, MasterLayerScope, MeshNode, NodePayload, NodeRef,
    ObjectId, OutlineStyle, PageId, ParametricShape, Point2D, RasterFilter, Rect2D, SceneObject,
    SelectionState, TextOnPath, TextStyle, TransparencyMode, Unit, Vec2D, VectorTable, ZOrderOp,
};
use coree_io::import_palette_file;
use coree_prod::{
    add_barcode_to_document, add_qr_code_to_document, apply_print_merge, builtin_macros,
    collect_for_output, compute_production_metrics, compute_separations, find_and_replace,
    find_objects, generate_cut_contour, generate_dieline, generate_print_marks, impose_document,
    run_macro, run_preflight, run_recorded_macro, BarcodeOptions, BarcodeSymbology, CollectOptions,
    DielineKind, FindQuery, FindReplaceReport, FindScope, ImpositionMode, LensKindFilter,
    MacroSandboxConfig, ObjectKindFilter, PreflightReport, PreflightRules, PrintMarksOptions,
    PrintMergeOptions, PrintMergeTable, ProductionMetrics, QrCodeOptions, QrErrorCorrection,
    ReplaceAction, SeparationOptions, SeparationResult, TextMatchMode,
};
use coree_render::{
    compute_page_layouts, convert_object_to_bitmap, paragraph_to_curves, resample,
    text_on_path_to_curves, text_to_curves, trace_raster_to_mm, DirtyTileCache, DynamicGuideLine,
    OutlinePreset, RenderOptions, SceneRenderer, SnapOptions, TraceConfig, TraceMode, ViewMode,
    Viewport,
};
use coree_storage::{
    FontQuery, FontRecord, JobHandle, JobOutcome, JobPool, RecentDocumentRecord, StorageDb,
    UncleanSession, WorkspaceLayoutRecord,
};
use egui_dock::{DockState, NodeIndex, NodePath, SurfaceIndex};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

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
    },
    RotatingSelection {
        pivot_mm: Point2D,
        start_angle_rad: f64,
        current_angle_rad: f64,
    },
    SkewingSelection {
        horizontal: bool,
        pivot_mm: Point2D,
        start_mm: Point2D,
        current_mm: Point2D,
    },
    DraggingPivot,
    DraggingNode {
        node: NodeRef,
        handle_part: NodeDragPart,
        orig_path: BezierPath,
    },
    DraggingGradientHandle {
        object_id: ObjectId,
        is_end: bool,
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
// Multi-Document Tab Model
// =============================================================================

/// Independent document session tab holding scene graph, history, viewport, and render cache.
pub struct DocumentTab {
    pub id: Uuid,
    pub document: Document,
    pub history: CommandHistory,
    pub selection: SelectionState,
    pub viewport: Viewport,
    pub view_mode: ViewMode,
    pub render_options: RenderOptions,
    pub tile_cache: DirtyTileCache,
    pub renderer: SceneRenderer,
    pub snap_options: SnapOptions,
    pub active_guides: Vec<DynamicGuideLine>,
    pub dirty: bool,
    pub source_path: Option<PathBuf>,
    pub last_autosave_at: Instant,
    pub journal_seq: u64,
    pub wal_session_open: bool,
    pub last_rendered_texture: Option<egui::TextureHandle>,
    pub needs_rerender: bool,
    pub viewport_initialized: bool,
    pub drag_state: CanvasDragState,
    pub poly_points_mm: Vec<Point2D>,
    pub live_sketch_strokes: Vec<Vec<Point2D>>,
    pub cursor_world_mm: Point2D,
    pub inspection_report: Option<FileInspectionReport>,
    pub overprint_preview: bool,
    pub separations_preview: Option<SeparationResult>,
    pub active_separation_channel: Option<usize>,
    pub powerclip_edit_id: Option<ObjectId>,
    pub last_metrics: ProductionMetrics,
    pub metrics_dirty: bool,
    pub preflight_report: Option<PreflightReport>,
    pub nudge_mm: f64,
    pub duplicate_offset_mm: Vec2D,
    pub lock_aspect: bool,
    pub inline_renaming_page: Option<usize>,
    pub inline_page_name_buf: String,
    pub active_layer_id: Option<LayerId>,
}

impl DocumentTab {
    /// Creates a new interactive tab wrapping `document`.
    #[must_use]
    pub fn new(document: Document, source_path: Option<PathBuf>, theme: StudioTheme) -> Self {
        let mut render_options = RenderOptions::interactive();
        render_options.canvas_bg = theme.tokens().canvas_well_core_color();
        let active_layer_id = document
            .active_page()
            .and_then(|p| p.layers.last())
            .map(|l| l.id);
        let metrics = compute_production_metrics(&document, None);
        Self {
            id: document.id,
            document,
            history: CommandHistory::default(),
            selection: SelectionState::new(),
            viewport: Viewport::new(1024, 720),
            view_mode: ViewMode::Enhanced,
            render_options,
            tile_cache: DirtyTileCache::new(1024, 720),
            renderer: SceneRenderer::new(),
            snap_options: SnapOptions::default(),
            active_guides: Vec::new(),
            dirty: false,
            source_path,
            last_autosave_at: Instant::now(),
            journal_seq: 0,
            wal_session_open: false,
            last_rendered_texture: None,
            needs_rerender: true,
            viewport_initialized: false,
            drag_state: CanvasDragState::Idle,
            poly_points_mm: Vec::new(),
            live_sketch_strokes: Vec::new(),
            cursor_world_mm: Point2D::ZERO,
            inspection_report: None,
            overprint_preview: false,
            separations_preview: None,
            active_separation_channel: None,
            powerclip_edit_id: None,
            last_metrics: metrics,
            metrics_dirty: false,
            preflight_report: None,
            nudge_mm: 1.0,
            duplicate_offset_mm: Vec2D::new(5.0, 5.0),
            lock_aspect: true,
            inline_renaming_page: None,
            inline_page_name_buf: String::new(),
            active_layer_id,
        }
    }

    /// Ensures `active_page_index` and `active_layer_id` always reference an existing page
    /// and a valid layer belonging to that active page.
    pub fn sync_active_layer(&mut self) {
        if self.document.pages.is_empty() {
            self.active_layer_id = None;
            return;
        }
        if self.document.active_page_index >= self.document.pages.len() {
            self.document.active_page_index = self.document.pages.len() - 1;
        }
        let Some(page) = self.document.active_page() else {
            self.active_layer_id = None;
            return;
        };
        let still_valid = self
            .active_layer_id
            .is_some_and(|lid| page.layers.iter().any(|l| l.id == lid));
        if !still_valid {
            self.active_layer_id = page
                .layers
                .iter()
                .rev()
                .find(|l| !l.locked)
                .or_else(|| page.layers.last())
                .map(|l| l.id);
        }
    }

    /// Marks the canvas cache and live production metrics dirty after any scene edit.
    pub fn mark_modified(&mut self) {
        self.dirty = true;
        self.needs_rerender = true;
        self.metrics_dirty = true;
        self.separations_preview = None;
        self.tile_cache.mark_all_dirty();
        self.selection.prune_missing(&self.document);
        self.sync_active_layer();
    }

    /// Refreshes live path length & area metrics if dirty.
    pub fn ensure_metrics_fresh(&mut self) {
        if self.metrics_dirty {
            let sel = if self.selection.is_empty() {
                None
            } else {
                Some(self.selection.objects.as_slice())
            };
            self.last_metrics = compute_production_metrics(&self.document, sel);
            self.metrics_dirty = false;
        }
    }

    /// Tab label shown in the top document tab strip.
    #[must_use]
    pub fn tab_title(&self) -> String {
        if self.dirty {
            format!("{} *", self.document.title)
        } else {
            self.document.title.clone()
        }
    }

    /// Fits the active page cleanly inside the current viewport.
    pub fn fit_active_page(&mut self) {
        let layouts = compute_page_layouts(
            &self.document,
            self.document.multipage_view,
            self.render_options.multipage_gap_mm,
        );
        let rect = if self.document.multipage_view {
            let mut acc: Option<Rect2D> = None;
            for l in &layouts {
                let r = l.rect_mm();
                acc = Some(match acc {
                    Some(prev) => prev.union(r),
                    None => r,
                });
            }
            acc.unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0))
        } else {
            self.document.active_page().map_or_else(
                || Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0),
                |p| p.trim_rect(),
            )
        };
        self.viewport.fit_rect(rect, 36.0);
        self.needs_rerender = true;
    }

    /// Fits all visible objects on the active page (or the active page if empty).
    pub fn fit_all_objects(&mut self) {
        let all_ids: Vec<ObjectId> = self
            .document
            .active_page()
            .map(|p| {
                p.layers
                    .iter()
                    .filter(|l| l.visible)
                    .flat_map(|l| l.objects.iter().copied())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(bb) = self.document.combined_bounds(&all_ids) {
            self.viewport.fit_rect(bb.expand(5.0), 40.0);
            self.needs_rerender = true;
        } else {
            self.fit_active_page();
        }
    }
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

// =============================================================================
// StudioApp — Root Desktop & Headless Application Engine
// =============================================================================

/// Complete desktop studio application state (`eframe::App` + headless test API).
pub struct StudioApp {
    pub tabs: Vec<DocumentTab>,
    pub active_tab: usize,
    pub show_welcome: bool,
    pub theme: StudioTheme,
    pub theme_dirty: bool,
    pub fonts_configured: bool,
    pub shortcut_profile: ShortcutProfile,

    // Toolbox & Dockers
    pub active_tool: ToolId,
    pub previous_non_pick_tool: ToolId,
    pub group_active_tools: [ToolId; 15],
    pub open_flyout_group: Option<usize>,
    pub dock_state: DockState<DockerTab>,
    pub show_dockers: bool,
    pub toolbox_two_column: bool,

    // Offline local storage & async job pool
    pub storage: Option<StorageDb>,
    pub job_pool: Option<JobPool>,
    pub recent_docs: Vec<RecentDocumentRecord>,
    pub unclean_sessions: Vec<UncleanSession>,
    pub active_jobs: Vec<ActiveBackgroundJob>,
    pub autosave_interval: Duration,

    // Styling defaults & Palettes
    pub default_fill: Fill,
    pub default_outline: OutlineStyle,
    pub default_text_style: TextStyle,
    pub active_palette_index: usize, // 0=CMYK, 1=RGB, 2=Document, 3=CutContour, 4+=Custom
    pub custom_palettes: Vec<ColorPalette>,
    pub palette_show_names: bool,

    // Panel / Dialog States
    pub tool_settings: ToolSettings,
    pub properties_state: PropertiesDockerState,
    pub lens_state: LensDockerState,
    pub effects_state: EffectsDockerState,
    pub transform_state: TransformDockerState,
    pub trace_state: TraceDockerState,
    pub bitmap_state: BitmapDockerState,
    pub production_state: ProductionDockerState,
    pub find_replace_state: FindReplaceState,
    pub font_manager_state: FontManagerState,
    pub macro_state: MacroDockerState,
    pub new_doc_dialog: NewDocDialogState,
    pub export_dialog: ExportDialogState,
    pub command_palette: CommandPaletteState,
    pub show_about_dialog: bool,
    pub show_preferences_dialog: bool,
    pub show_print_preview_dialog: bool,

    // Feedback
    pub status_message: String,
    pub toasts: Vec<StudioToast>,
}

impl StudioApp {
    /// Creates a completely headless `StudioApp` backed by an in-memory SQLite database
    /// (ideal for unit & integration tests without touching user config directories).
    #[must_use]
    pub fn new_headless() -> Self {
        let storage = StorageDb::open_in_memory().ok();
        Self::from_storage(storage, Vec::new(), false)
    }

    /// Creates a `StudioApp` attached to an explicit [`StorageDb`] instance.
    #[must_use]
    pub fn new_with_storage(storage: StorageDb) -> Self {
        Self::from_storage(Some(storage), Vec::new(), false)
    }

    /// Creates the desktop `StudioApp` opening local SQLite persistence, scanning for
    /// crash-recovery sessions, loading preferences, and opening any CLI-supplied files.
    #[must_use]
    pub fn new_desktop(initial_files: Vec<PathBuf>) -> Self {
        let storage = StorageDb::open_default().ok();
        Self::from_storage(storage, initial_files, true)
    }

    fn from_storage(
        storage: Option<StorageDb>,
        initial_files: Vec<PathBuf>,
        open_welcome_when_empty: bool,
    ) -> Self {
        let mut group_active_tools = [ToolId::Pick; 15];
        for (i, group) in TOOL_GROUPS.iter().enumerate() {
            if let Some(&first) = group.first() {
                group_active_tools[i] = first;
            }
        }

        // Load saved preferences from SQLite if present
        let saved_theme = storage
            .as_ref()
            .and_then(|db| db.get_preference::<StudioTheme>("ui.theme").ok().flatten())
            .unwrap_or_default();
        let saved_profile = storage
            .as_ref()
            .and_then(|db| {
                db.get_preference::<ShortcutProfile>("ui.shortcut_profile")
                    .ok()
                    .flatten()
            })
            .unwrap_or_default();
        let saved_dock = storage
            .as_ref()
            .and_then(|db| db.get_active_workspace_layout().ok().flatten())
            .and_then(|rec| serde_json::from_str::<DockState<DockerTab>>(&rec.dock_tree_json).ok())
            .unwrap_or_else(default_dock_state);

        let recent_docs = storage
            .as_ref()
            .and_then(|db| db.list_recent_documents(20).ok())
            .unwrap_or_default();
        let unclean_sessions = storage
            .as_ref()
            .and_then(|db| db.list_unclean_sessions().ok())
            .unwrap_or_default();

        let job_pool = JobPool::default_pool().ok();

        let mut app = Self {
            tabs: Vec::new(),
            active_tab: 0,
            show_welcome: open_welcome_when_empty && initial_files.is_empty(),
            theme: saved_theme,
            theme_dirty: true,
            fonts_configured: false,
            shortcut_profile: saved_profile,
            active_tool: ToolId::Pick,
            previous_non_pick_tool: ToolId::Rectangle,
            group_active_tools,
            open_flyout_group: None,
            dock_state: saved_dock,
            show_dockers: true,
            toolbox_two_column: false,
            storage,
            job_pool,
            recent_docs,
            unclean_sessions,
            active_jobs: Vec::new(),
            autosave_interval: Duration::from_secs(30),
            default_fill: Fill::Solid(Color::cmyk(0.75, 0.15, 0.0, 0.0)),
            default_outline: OutlineStyle::solid(Color::KEY_BLACK, 0.3),
            default_text_style: TextStyle::default(),
            active_palette_index: 0,
            custom_palettes: Vec::new(),
            palette_show_names: false,
            tool_settings: ToolSettings::default(),
            properties_state: PropertiesDockerState::default(),
            lens_state: LensDockerState::default(),
            effects_state: EffectsDockerState::default(),
            transform_state: TransformDockerState::default(),
            trace_state: TraceDockerState::default(),
            bitmap_state: BitmapDockerState::default(),
            production_state: ProductionDockerState::default(),
            find_replace_state: FindReplaceState::default(),
            font_manager_state: FontManagerState::default(),
            macro_state: MacroDockerState::default(),
            new_doc_dialog: NewDocDialogState::default(),
            export_dialog: ExportDialogState::default(),
            command_palette: CommandPaletteState::default(),
            show_about_dialog: false,
            show_preferences_dialog: false,
            show_print_preview_dialog: false,
            status_message: "Ready · 100% Offline Local Studio".to_owned(),
            toasts: Vec::new(),
        };

        for path in initial_files {
            if let Err(err) = app.open_file_path(&path) {
                app.push_toast(format!("Open failed: {err}"), true);
            }
        }

        if app.tabs.is_empty() {
            app.new_document(Document::new_a4("Untitled-1"));
        }

        app
    }

    // -------------------------------------------------------------------------
    // Notifications & Storage Helpers
    // -------------------------------------------------------------------------

    /// Appends a non-blocking status toast and updates the status bar message.
    pub fn push_toast(&mut self, msg: impl Into<String>, is_error: bool) {
        let text = msg.into();
        self.status_message = text.clone();
        self.toasts.push(StudioToast {
            message: text,
            is_error,
            created_at: Instant::now(),
        });
        if self.toasts.len() > 6 {
            self.toasts.remove(0);
        }
    }

    /// Prunes expired toasts (> 5s old).
    pub fn prune_toasts(&mut self) {
        let ttl = Duration::from_secs(5);
        self.toasts.retain(|t| t.created_at.elapsed() < ttl);
    }

    /// Refreshes cached recent documents and unclean sessions from SQLite.
    pub fn refresh_storage_lists(&mut self) {
        if let Some(ref db) = self.storage {
            if let Ok(recents) = db.list_recent_documents(20) {
                self.recent_docs = recents;
            }
            if let Ok(unclean) = db.list_unclean_sessions() {
                self.unclean_sessions = unclean;
            }
        }
    }

    /// Active document tab reference.
    #[must_use]
    pub fn active_tab(&self) -> Option<&DocumentTab> {
        self.tabs.get(self.active_tab)
    }

    /// Mutable active document tab reference.
    pub fn active_tab_mut(&mut self) -> Option<&mut DocumentTab> {
        self.tabs.get_mut(self.active_tab)
    }

    // -------------------------------------------------------------------------
    // Document Tab Lifecycle & File I/O
    // -------------------------------------------------------------------------

    /// Opens a new document tab and selects it.
    pub fn new_document(&mut self, doc: Document) {
        let tab = DocumentTab::new(doc, None, self.theme);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.show_welcome = false;
        self.push_toast("Created new document", false);
    }

    /// Opens a fresh showcase document generated by `coree_cdr::generate_sample_document`.
    pub fn new_sample_document(&mut self) {
        let doc = generate_sample_document();
        let tab = DocumentTab::new(doc, None, self.theme);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.show_welcome = false;
        self.push_toast(
            "Opened Showcase Sample (Shapes, Arabic RTL, 11 Lenses, CutContour)",
            false,
        );
    }

    /// Opens any supported document file (`.coree`, `.cdr`, `.cdt`, `.cmx`, `.cpt`, `.svg`, raster).
    pub fn open_file_path(&mut self, path: &Path) -> Result<(), String> {
        let opened = open_any_document(path)?;
        let title = opened.document.title.clone();
        let page_count = opened.document.pages.len() as u32;
        let color_mode = match opened.document.color_mode {
            ColorMode::Cmyk => "cmyk",
            ColorMode::Rgb => "rgb",
        }
        .to_owned();
        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        let mut tab = DocumentTab::new(opened.document, Some(path.to_path_buf()), self.theme);
        tab.inspection_report = opened.inspection;
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.show_welcome = false;

        if let Some(ref db) = self.storage {
            let rec = RecentDocumentRecord {
                id: Uuid::new_v4().to_string(),
                file_path: path.to_string_lossy().into_owned(),
                display_name: title.clone(),
                file_format: opened.format_tag,
                cdr_version: opened.cdr_version,
                page_count: page_count.max(1),
                color_mode,
                thumbnail_png: None,
                file_size_bytes: file_size,
                pinned: false,
                last_opened_at: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
            };
            let _ = db.upsert_recent_document(&rec);
            self.refresh_storage_lists();
        }

        if opened.warnings.is_empty() {
            self.push_toast(format!("Opened '{title}'"), false);
        } else {
            self.push_toast(
                format!("Opened '{title}' with {} notice(s)", opened.warnings.len()),
                false,
            );
        }
        Ok(())
    }

    /// Imports an external vector/raster file into the active document page.
    pub fn import_file_path(&mut self, path: &Path) -> Result<usize, String> {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return Err("no active document".to_owned());
        };
        let ids = import_into_document(&mut tab.document, path)?;
        let count = ids.len();
        if !ids.is_empty() {
            tab.selection.select_many(&tab.document, &ids);
        }
        tab.mark_modified();
        self.checkpoint_wal_snapshot_active();
        self.push_toast(
            format!("Imported {count} object(s) from {}", path.display()),
            false,
        );
        Ok(count)
    }

    /// Saves the active document to its current path (or `fallback_path` if untitled).
    pub fn save_active(&mut self, fallback_path: Option<&Path>) -> Result<PathBuf, String> {
        let target = {
            let tab = self
                .tabs
                .get(self.active_tab)
                .ok_or_else(|| "no active document".to_owned())?;
            tab.source_path
                .clone()
                .or_else(|| fallback_path.map(Path::to_path_buf))
                .ok_or_else(|| "document has no file path; use Save As".to_owned())?
        };
        self.save_active_as(&target)?;
        Ok(target)
    }

    /// Saves the active document to `path`, clears `dirty`, records in Recent Documents, and marks clean autosave.
    pub fn save_active_as(&mut self, path: &Path) -> Result<(), String> {
        let (doc_id_str, title, page_count, color_mode_str, msg) = {
            let tab = self
                .tabs
                .get_mut(self.active_tab)
                .ok_or_else(|| "no active document".to_owned())?;
            let msg = export_any_document(&tab.document, path, &UniversalExportOptions::default())?;
            tab.source_path = Some(path.to_path_buf());
            tab.dirty = false;
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                tab.document.title = stem.to_owned();
            }
            (
                tab.document.id.to_string(),
                tab.document.title.clone(),
                tab.document.pages.len() as u32,
                match tab.document.color_mode {
                    ColorMode::Cmyk => "cmyk",
                    ColorMode::Rgb => "rgb",
                }
                .to_owned(),
                msg,
            )
        };

        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_else(|| "coree".to_owned());
        let file_format = match ext.as_str() {
            "cdr" | "cdt" | "cmx" | "cpt" | "svg" | "pdf" | "eps" | "dxf" | "plt" | "png"
            | "jpg" | "tiff" | "psd" | "coree" => ext,
            _ => "coree".to_owned(),
        };

        if let Some(ref db) = self.storage {
            let _ = db.mark_clean_close(&doc_id_str);
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                tab.journal_seq = 0;
                tab.wal_session_open = false;
            }
            let rec = RecentDocumentRecord {
                id: Uuid::new_v4().to_string(),
                file_path: path.to_string_lossy().into_owned(),
                display_name: title,
                file_format,
                cdr_version: None,
                page_count: page_count.max(1),
                color_mode: color_mode_str,
                thumbnail_png: None,
                file_size_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
                pinned: false,
                last_opened_at: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
            };
            let _ = db.upsert_recent_document(&rec);
            self.refresh_storage_lists();
        }
        self.push_toast(msg, false);
        Ok(())
    }

    /// Exports the active document using explicit [`UniversalExportOptions`].
    pub fn export_active(
        &mut self,
        output_path: &Path,
        opts: &UniversalExportOptions,
    ) -> Result<String, String> {
        let doc = self
            .active_tab()
            .ok_or_else(|| "no active document".to_owned())?
            .document
            .clone();
        let msg = export_any_document(&doc, output_path, opts)?;
        self.push_toast(msg.clone(), false);
        Ok(msg)
    }

    /// Exports the active document asynchronously on `JobPool` so the GUI stays at 60fps.
    pub fn export_active_async(&mut self, output_path: PathBuf, opts: UniversalExportOptions) {
        let Some(doc) = self.active_tab().map(|t| t.document.clone()) else {
            return;
        };
        if let Some(ref pool) = self.job_pool {
            let label = format!("Export {}", output_path.display());
            let handle = pool.submit(label, move |ctx| {
                ctx.set_progress(15);
                if ctx.is_cancelled() {
                    return Err("cancelled".to_owned());
                }
                let res = export_any_document(&doc, &output_path, &opts)?;
                ctx.set_progress(100);
                Ok(res)
            });
            self.active_jobs.push(ActiveBackgroundJob { handle });
            self.push_toast("Export started in background...", false);
        } else if let Err(err) = self.export_active(&output_path, &opts) {
            self.push_toast(err, true);
        }
    }

    /// Closes document tab `idx` and marks its autosave snapshots cleanly closed.
    pub fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        let doc_id = self.tabs[idx].document.id.to_string();
        if let Some(ref db) = self.storage {
            let _ = db.mark_clean_close(&doc_id);
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.show_welcome = true;
            self.active_tab = 0;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
    }

    /// Marks every open tab as cleanly closed and persists workspace layout & preferences.
    pub fn shutdown_cleanly(&mut self) {
        let dock_json = serde_json::to_string(&self.dock_state).ok();
        if let Some(ref mut db) = self.storage {
            for tab in &self.tabs {
                let _ = db.mark_clean_close(&tab.document.id.to_string());
            }
            let _ = db.set_preference("ui.theme", "appearance", &self.theme);
            let _ = db.set_preference("ui.shortcut_profile", "shortcuts", &self.shortcut_profile);
            if let Some(json) = dock_json {
                let rec = WorkspaceLayoutRecord {
                    id: "coree-last-session".to_owned(),
                    name: "Last Session".to_owned(),
                    description: "Automatically saved workspace layout".to_owned(),
                    dock_tree_json: json,
                    toolbox_config_json: "{}".to_owned(),
                    shortcuts_json: "{}".to_owned(),
                    is_builtin: false,
                    is_active: true,
                };
                let _ = db.upsert_workspace_layout(&rec);
                let _ = db.set_active_workspace_layout(&rec.id);
            }
        }
    }

    // -------------------------------------------------------------------------
    // Autosave, WAL Command Journal & Crash Recovery
    // -------------------------------------------------------------------------

    /// Saves an atomic crash-recovery snapshot of the active tab into SQLite.
    pub fn perform_autosave_active(&mut self) -> Result<(), String> {
        let Some(ref db) = self.storage else {
            return Ok(());
        };
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return Ok(());
        };
        let doc_id = tab.document.id.to_string();
        db.save_snapshot(
            &doc_id,
            &tab.document.title,
            tab.source_path.as_deref(),
            &tab.document,
        )
        .map_err(|e| format!("autosave failed: {e}"))?;
        tab.wal_session_open = true;
        tab.last_autosave_at = Instant::now();
        Ok(())
    }

    /// Writes a crash-recovery WAL checkpoint snapshot for the active tab (advancing
    /// `last_command_seq` past prior journal entries so undo/redo and non-`CommandOp`
    /// mutations survive crashes accurately without resurrecting undone operations).
    pub fn checkpoint_wal_snapshot_active(&mut self) {
        let _ = self.perform_autosave_active();
    }

    /// Periodically writes autosave snapshots for dirty open tabs.
    pub fn tick_autosave(&mut self) {
        let interval = self.autosave_interval;
        let needs_save = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|t| t.dirty && t.last_autosave_at.elapsed() >= interval);
        if needs_save {
            let _ = self.perform_autosave_active();
        }
    }

    /// Recovers an uncleanly terminated session from its snapshot + replayed command journal.
    pub fn recover_unclean_session(
        &mut self,
        doc_id: &str,
        session_id: &str,
    ) -> Result<(), String> {
        let (doc, orig_path) = {
            let db = self
                .storage
                .as_ref()
                .ok_or_else(|| "local storage unavailable".to_owned())?;
            let orig = self
                .unclean_sessions
                .iter()
                .find(|s| s.document_id == doc_id && s.session_id == session_id)
                .and_then(|s| s.original_file_path.as_ref())
                .map(PathBuf::from);
            let recovered = db
                .recover_session(doc_id, session_id)
                .map_err(|e| format!("recovery failed: {e}"))?;
            let _ = db.mark_session_clean_close(doc_id, session_id);
            (recovered, orig)
        };

        let title = doc.title.clone();
        let mut tab = DocumentTab::new(doc, orig_path, self.theme);
        tab.dirty = true;
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.show_welcome = false;
        self.refresh_storage_lists();
        self.push_toast(format!("Recovered session '{title}'"), false);
        Ok(())
    }

    /// Discards an unclean crash-recovery session cleanly.
    pub fn discard_unclean_session(&mut self, doc_id: &str, session_id: &str) {
        if let Some(ref db) = self.storage {
            let _ = db.mark_session_clean_close(doc_id, session_id);
            self.refresh_storage_lists();
        }
        self.push_toast("Discarded recovery snapshot", false);
    }

    /// Polls non-blocking background jobs and emits completion toasts.
    pub fn tick_background_jobs(&mut self) {
        let mut finished_msgs = Vec::new();
        self.active_jobs.retain(|job| {
            if let Some(outcome) = job.handle.try_recv() {
                match outcome {
                    JobOutcome::Completed(msg) => finished_msgs.push((msg, false)),
                    JobOutcome::Cancelled => {
                        finished_msgs.push((format!("Cancelled {}", job.handle.name()), false));
                    }
                    JobOutcome::Failed(err) => {
                        finished_msgs.push((format!("{} failed: {err}", job.handle.name()), true));
                    }
                }
                false
            } else {
                true
            }
        });
        for (msg, is_err) in finished_msgs {
            self.push_toast(msg, is_err);
        }
    }

    // -------------------------------------------------------------------------
    // Deterministic Command Execution & Undo/Redo
    // -------------------------------------------------------------------------

    /// Executes `op` through the active tab's [`CommandHistory`] and appends it to SQLite WAL journal.
    pub fn execute_command(&mut self, op: CommandOp) -> Result<(), String> {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return Err("no active document".to_owned());
        };
        let doc_id = tab.document.id.to_string();

        // Ensure an active unclean baseline snapshot exists before journaling commands
        // (including after `save_active_as` marks the previous session cleanly closed).
        if !tab.wal_session_open {
            if let Some(ref db) = self.storage {
                if db
                    .save_snapshot(
                        &doc_id,
                        &tab.document.title,
                        tab.source_path.as_deref(),
                        &tab.document,
                    )
                    .is_ok()
                {
                    tab.wal_session_open = true;
                    tab.last_autosave_at = Instant::now();
                }
            }
        }

        tab.history
            .execute(&mut tab.document, op.clone())
            .map_err(|e| e.to_string())?;
        tab.journal_seq = tab.journal_seq.saturating_add(1);
        let seq = tab.journal_seq;
        tab.mark_modified();

        if let Some(ref db) = self.storage {
            let _ = db.append_command(&doc_id, seq, &op);
        }
        Ok(())
    }

    /// Undoes the last command on the active tab and checkpoints the WAL snapshot
    /// so a subsequent crash cannot resurrect the undone edit.
    pub fn undo(&mut self) -> Result<String, String> {
        let label = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let label = tab
                .history
                .undo(&mut tab.document)
                .map_err(|e| e.to_string())?;
            tab.mark_modified();
            label
        };
        self.checkpoint_wal_snapshot_active();
        self.push_toast(format!("Undo: {label}"), false);
        Ok(label)
    }

    /// Redoes the last undone command on the active tab and checkpoints the WAL snapshot.
    pub fn redo(&mut self) -> Result<String, String> {
        let label = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let label = tab
                .history
                .redo(&mut tab.document)
                .map_err(|e| e.to_string())?;
            tab.mark_modified();
            label
        };
        self.checkpoint_wal_snapshot_active();
        self.push_toast(format!("Redo: {label}"), false);
        Ok(label)
    }

    // -------------------------------------------------------------------------
    // Core Scene Operations (Selection, Transform, Shaping, Fill, Outline, Lens, Effects)
    // -------------------------------------------------------------------------

    /// Duplicates all currently selected objects by `tab.duplicate_offset_mm` via
    /// undoable, WAL-journaled [`CommandOp::Batch`].
    pub fn duplicate_selection(&mut self) -> Result<Vec<ObjectId>, String> {
        let offset = self
            .active_tab()
            .map(|t| t.duplicate_offset_mm)
            .unwrap_or(Vec2D::new(5.0, 5.0));
        self.duplicate_selection_with_offset(offset)
    }

    /// Duplicates all currently selected objects with an explicit `offset` (used by
    /// both `Ctrl+D` and Transform Docker "Apply to Duplicate" with zero offset).
    pub fn duplicate_selection_with_offset(
        &mut self,
        offset: Vec2D,
    ) -> Result<Vec<ObjectId>, String> {
        let (ops, created) = {
            let Some(tab) = self.tabs.get(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            if tab.selection.is_empty() {
                return Ok(Vec::new());
            }
            let ids = tab.selection.objects.clone();
            let mut next_id = tab.document.next_id.max(1);
            let mut id_map = HashMap::new();
            let mut ops = Vec::new();
            let mut created = Vec::new();
            let delta_xf = Affine2D::translate(offset.x, offset.y);
            for src_id in ids {
                let layer_id = tab.document.layer_of_object(src_id).or(tab.active_layer_id);
                let mut visiting = HashSet::new();
                if let Some(new_id) = build_duplicate_tree_ops(
                    &tab.document,
                    src_id,
                    layer_id,
                    delta_xf,
                    0,
                    &mut next_id,
                    &mut visiting,
                    &mut id_map,
                    &mut ops,
                ) {
                    created.push(new_id);
                }
            }
            remap_duplicate_ops(&mut ops, &id_map);
            (ops, created)
        };
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        self.execute_command(CommandOp::Batch(ops))?;
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.selection.select_many(&tab.document, &created);
            tab.metrics_dirty = true;
        }
        self.push_toast(format!("Duplicated {} object(s)", created.len()), false);
        Ok(created)
    }

    /// Duplicates page `src_idx`, cloning its size, bleed, layers, and every object subtree
    /// via a single undoable [`CommandOp::Batch`].
    pub fn duplicate_page(&mut self, src_idx: usize) {
        let plan = self.active_tab().and_then(|tab| {
            let src_page = tab.document.pages.get(src_idx)?;
            let mut next_id = tab.document.next_id.max(1);
            let new_page_id = PageId(next_id);
            next_id = next_id.saturating_add(1);
            let first_layer_id = LayerId(next_id);
            next_id = next_id.saturating_add(1);

            let mut ops = vec![CommandOp::AddPage {
                name: format!("{} Copy", src_page.name),
                width_mm: src_page.width_mm,
                height_mm: src_page.height_mm,
                bleed_mm: src_page.bleed_mm,
            }];

            // Map each source layer onto a corresponding target layer on the new page
            let mut layer_targets: Vec<(LayerId, Vec<ObjectId>)> = Vec::new();
            for (l_idx, src_layer) in src_page.layers.iter().enumerate() {
                let dst_lid = if l_idx == 0 {
                    ops.push(CommandOp::SetLayerFlags {
                        layer_id: first_layer_id,
                        visible: Some(src_layer.visible),
                        locked: Some(false), // keep unlocked while inserting cloned objects
                        printable: Some(src_layer.printable),
                        is_cut_layer: Some(src_layer.is_cut_layer),
                    });
                    first_layer_id
                } else {
                    let lid = LayerId(next_id);
                    next_id = next_id.saturating_add(1);
                    ops.push(CommandOp::AddLayer {
                        page_id: new_page_id,
                        name: src_layer.name.clone(),
                        is_cut_layer: src_layer.is_cut_layer,
                        master_scope: src_layer.master_scope,
                    });
                    ops.push(CommandOp::SetLayerFlags {
                        layer_id: lid,
                        visible: Some(src_layer.visible),
                        locked: Some(false),
                        printable: Some(src_layer.printable),
                        is_cut_layer: Some(src_layer.is_cut_layer),
                    });
                    lid
                };
                layer_targets.push((dst_lid, src_layer.objects.clone()));
            }

            // Clone every object subtree on every layer
            let mut id_map = HashMap::new();
            for (dst_lid, objs) in &layer_targets {
                for &src_oid in objs {
                    let mut visiting = HashSet::new();
                    let _ = build_duplicate_tree_ops(
                        &tab.document,
                        src_oid,
                        Some(*dst_lid),
                        Affine2D::IDENTITY,
                        0,
                        &mut next_id,
                        &mut visiting,
                        &mut id_map,
                        &mut ops,
                    );
                }
            }
            remap_duplicate_ops(&mut ops, &id_map);

            // Restore locked flags for any locked source layers after inserting objects
            for (l_idx, src_layer) in src_page.layers.iter().enumerate() {
                if src_layer.locked {
                    let dst_lid = layer_targets[l_idx].0;
                    ops.push(CommandOp::SetLayerFlags {
                        layer_id: dst_lid,
                        visible: None,
                        locked: Some(true),
                        printable: None,
                        is_cut_layer: None,
                    });
                }
            }

            let first_layer_name = src_page.layers.first().map(|l| l.name.clone());
            Some((ops, first_layer_id, first_layer_name))
        });

        if let Some((ops, first_layer_id, first_layer_name)) = plan {
            if let Err(err) = self.execute_command(CommandOp::Batch(ops)) {
                self.push_toast(format!("Duplicate page failed: {err}"), true);
                return;
            }
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                if let Some(name) = first_layer_name {
                    if let Some(layer) = tab.document.find_layer_mut(first_layer_id) {
                        layer.name = name;
                    }
                }
                if !tab.document.pages.is_empty() {
                    tab.document.active_page_index = tab.document.pages.len() - 1;
                }
                tab.selection.clear();
                tab.sync_active_layer();
                tab.needs_rerender = true;
            }
            self.push_toast("Duplicated page with layers & objects", false);
        }
    }

    /// Deletes currently selected objects via undoable [`CommandOp::DeleteObjects`].
    pub fn delete_selection(&mut self) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            return;
        }
        match self.execute_command(CommandOp::DeleteObjects { ids }) {
            Ok(()) => self.push_toast("Deleted selection", false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Selects all visible, unlocked top-level objects on the active page.
    pub fn select_all(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let ids: Vec<ObjectId> = tab
            .document
            .active_page()
            .map(|p| {
                p.layers
                    .iter()
                    .filter(|l| l.visible && !l.locked)
                    .flat_map(|l| l.objects.iter().copied())
                    .filter(|oid| {
                        tab.document
                            .objects
                            .get(oid)
                            .is_some_and(|o| o.visible && !o.locked)
                    })
                    .collect()
            })
            .unwrap_or_default();
        tab.selection.select_many(&tab.document, &ids);
        tab.metrics_dirty = true;
    }

    /// Nudges selected objects by `(dx_steps, dy_steps) * nudge_mm * mult`.
    pub fn nudge_selection(&mut self, dx_steps: i32, dy_steps: i32, mult: i32) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        if tab.selection.is_empty() {
            return;
        }
        let step = tab.nudge_mm * f64::from(mult.max(1));
        let ids = tab.selection.objects.clone();
        let affine = Affine2D::translate(f64::from(dx_steps) * step, f64::from(dy_steps) * step);
        let _ = self.execute_command(CommandOp::TransformObjects { ids, affine });
    }

    /// Groups the current multi-object selection.
    pub fn group_selection(&mut self) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.len() < 2 {
            self.push_toast("Select at least 2 objects to Group", false);
            return;
        }
        if let Err(e) = self.execute_command(CommandOp::Group {
            ids,
            name: "Group".to_owned(),
        }) {
            self.push_toast(e, true);
        } else {
            self.push_toast("Grouped objects", false);
        }
    }

    /// Ungroups the selected Group object(s).
    pub fn ungroup_selection(&mut self) {
        let group_ids: Vec<ObjectId> = self
            .active_tab()
            .map(|t| {
                t.selection
                    .objects
                    .iter()
                    .copied()
                    .filter(|id| {
                        t.document
                            .objects
                            .get(id)
                            .is_some_and(|o| matches!(o.payload, NodePayload::Group(_)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if group_ids.is_empty() {
            self.push_toast("Select a Group to Ungroup", false);
            return;
        }
        let ops = group_ids
            .into_iter()
            .map(|group_id| CommandOp::Ungroup { group_id })
            .collect();
        if let Err(e) = self.execute_command(CommandOp::Batch(ops)) {
            self.push_toast(e, true);
        } else {
            self.push_toast("Ungrouped selection", false);
        }
    }

    /// Applies any of the 9 vector Boolean/Shaping ops to the selection.
    pub fn apply_boolean_op(&mut self, op: BooleanOp) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.push_toast("Select vector objects first", false);
            return;
        }
        match self.execute_command(CommandOp::BooleanOp { op, ids }) {
            Ok(()) => self.push_toast(format!("Applied {op:?}"), false),
            Err(e) => self.push_toast(format!("{op:?} failed: {e}"), true),
        }
    }

    /// Aligns selected objects (`AlignOp`).
    pub fn apply_align(&mut self, op: AlignOp) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            return;
        }
        let _ = self.execute_command(CommandOp::AlignObjects { ids, op });
    }

    /// Distributes 3+ selected objects (`DistributeOp`).
    pub fn apply_distribute(&mut self, op: DistributeOp) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.len() < 3 {
            self.push_toast("Select at least 3 objects to Distribute", false);
            return;
        }
        let _ = self.execute_command(CommandOp::DistributeObjects { ids, op });
    }

    /// Changes Z-order of selected objects.
    pub fn apply_z_order(&mut self, op: ZOrderOp) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            return;
        }
        let ops = ids
            .into_iter()
            .map(|id| CommandOp::SetZOrder { id, op })
            .collect();
        let _ = self.execute_command(CommandOp::Batch(ops));
    }

    /// Converts all selected shapes/text/tables to editable [`NodePayload::Path`] curves (`Ctrl+Q`).
    /// For text objects, extracts real glyph vector outlines via `coree_render` (falling back to
    /// `CommandOp::ConvertToCurves` if glyph outlines are empty).
    pub fn convert_selection_to_curves(&mut self) {
        let ops: Vec<CommandOp> = {
            let Some(tab) = self.active_tab() else {
                return;
            };
            if tab.selection.objects.is_empty() {
                return;
            }
            tab.selection
                .objects
                .iter()
                .copied()
                .map(|id| {
                    if let Some(obj) = tab.document.objects.get(&id) {
                        let glyph_path = match &obj.payload {
                            NodePayload::ArtisticText(art) => Some(text_to_curves(art)),
                            NodePayload::ParagraphText(para) => Some(paragraph_to_curves(para)),
                            NodePayload::TextOnPath(top) => {
                                tab.document.world_path_of(top.path_id).map(|wp| {
                                    let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                                    text_on_path_to_curves(top, &wp.transformed(&inv))
                                })
                            }
                            _ => None,
                        };
                        if let Some(path) = glyph_path {
                            if !path.is_empty() && path.is_finite() {
                                return CommandOp::EditPath { id, path };
                            }
                        }
                    }
                    CommandOp::ConvertToCurves { id }
                })
                .collect()
        };
        match self.execute_command(CommandOp::Batch(ops)) {
            Ok(()) => self.push_toast("Converted selection to curves", false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Creates a PowerClip using the last-selected object as container and earlier ones as contents.
    pub fn create_powerclip_from_selection(&mut self) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.len() < 2 {
            self.push_toast(
                "Select content object(s) + container shape last for PowerClip",
                false,
            );
            return;
        }
        let container_id = *ids.last().unwrap();
        let content_ids = ids[..ids.len() - 1].to_vec();
        match self.execute_command(CommandOp::CreatePowerClip {
            container_id,
            content_ids,
        }) {
            Ok(()) => self.push_toast("Created PowerClip container", false),
            Err(e) => self.push_toast(format!("PowerClip error: {e}"), true),
        }
    }

    /// Extracts clipped contents from the selected PowerClip container.
    pub fn extract_selected_powerclip(&mut self) {
        let pc_id = self.active_tab().and_then(|t| {
            t.selection.objects.iter().copied().find(|id| {
                t.document
                    .objects
                    .get(id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::PowerClip { .. }))
            })
        });
        let Some(powerclip_id) = pc_id else {
            self.push_toast("Select a PowerClip container to extract", false);
            return;
        };
        match self.execute_command(CommandOp::ExtractPowerClip { powerclip_id }) {
            Ok(()) => {
                if let Some(tab) = self.active_tab_mut() {
                    if tab.powerclip_edit_id == Some(powerclip_id) {
                        tab.powerclip_edit_id = None;
                    }
                }
                self.push_toast("Extracted PowerClip contents", false);
            }
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Applies `fill` to selected objects (or updates `default_fill` if nothing is selected).
    pub fn apply_fill(&mut self, fill: Fill) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.default_fill = fill;
            self.push_toast("Updated default Fill", false);
            return;
        }
        // Record solid color into Document Palette automatically
        if let Fill::Solid(ref col) = fill {
            if let Some(tab) = self.active_tab_mut() {
                if !tab
                    .document
                    .document_palette
                    .swatches
                    .iter()
                    .any(|s| s.color == *col)
                {
                    tab.document
                        .document_palette
                        .swatches
                        .push(ColorSwatch::new(col.to_hex(), col.clone()));
                }
            }
        }
        let _ = self.execute_command(CommandOp::SetFill { ids, fill });
    }

    /// Applies a solid color fill (`Left-click` on Palette Bar swatch).
    pub fn apply_fill_color(&mut self, color: Color) {
        self.apply_fill(Fill::Solid(color));
    }

    /// Applies an outline stroke color (`Right-click` on Palette Bar swatch), enabling 0.3mm width if previously none.
    pub fn apply_outline_color(&mut self, color: Color) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.default_outline.color = color;
            if !self.default_outline.is_visible() {
                self.default_outline.width_mm = 0.3;
            }
            self.push_toast("Updated default Outline color", false);
            return;
        }
        let mut ops = Vec::with_capacity(ids.len());
        if let Some(tab) = self.active_tab() {
            for &id in &ids {
                let mut outline = tab
                    .document
                    .objects
                    .get(&id)
                    .map(|o| o.outline.clone())
                    .unwrap_or_default();
                outline.color = color.clone();
                if !outline.is_visible() {
                    outline.width_mm = 0.3;
                }
                ops.push(CommandOp::SetOutline {
                    ids: vec![id],
                    outline,
                });
            }
        }
        let _ = self.execute_command(CommandOp::Batch(ops));
    }

    /// Removes fill (`Fill::None`) from selected objects (Left-click top `X` swatch).
    pub fn remove_fill(&mut self) {
        self.apply_fill(Fill::None);
    }

    /// Removes outline (`OutlineStyle::none()`) from selected objects (Right-click top `X` swatch).
    pub fn remove_outline(&mut self) {
        self.apply_outline(OutlineStyle::none());
    }

    /// Applies a complete [`OutlineStyle`] to selected objects (or sets default outline).
    pub fn apply_outline(&mut self, outline: OutlineStyle) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.default_outline = outline;
            return;
        }
        let _ = self.execute_command(CommandOp::SetOutline { ids, outline });
    }

    /// Applies a [`LensConfig`] (any of the 11 Lenses) to the selected object(s).
    pub fn apply_lens_to_selection(&mut self, lens: Option<LensConfig>) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.push_toast("Select a closed vector shape to apply a Lens", false);
            return;
        }
        let name = lens.as_ref().map_or("None", |l| l.kind.name());
        match self.execute_command(CommandOp::SetLens { ids, lens }) {
            Ok(()) => self.push_toast(format!("Applied Lens: {name}"), false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Adds a non-destructive [`Effect`] to all selected objects.
    pub fn add_effect_to_selection(&mut self, effect: Effect) {
        let ids = self
            .active_tab()
            .map(|t| t.selection.objects.clone())
            .unwrap_or_default();
        if ids.is_empty() {
            self.push_toast("Select an object to add an Effect", false);
            return;
        }
        let ops = ids
            .into_iter()
            .map(|id| CommandOp::AddEffect {
                id,
                effect: effect.clone(),
            })
            .collect();
        match self.execute_command(CommandOp::Batch(ops)) {
            Ok(()) => self.push_toast("Added live vector effect", false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Removes effect `effect_index` from `id`.
    pub fn remove_effect_on(&mut self, id: ObjectId, effect_index: usize) {
        let _ = self.execute_command(CommandOp::RemoveEffect { id, effect_index });
    }

    /// Fits a selected ArtisticText object onto a selected vector path/shape (`Text > Fit Text to Path`).
    pub fn fit_selected_text_to_path(&mut self) {
        let pair = self.active_tab().and_then(|tab| {
            let mut text_obj: Option<(ObjectId, ArtisticText)> = None;
            let mut path_id: Option<ObjectId> = None;
            for &id in &tab.selection.objects {
                if let Some(obj) = tab.document.objects.get(&id) {
                    match &obj.payload {
                        NodePayload::ArtisticText(t) if text_obj.is_none() => {
                            text_obj = Some((id, t.clone()));
                        }
                        NodePayload::Path(_) | NodePayload::Shape(_) if path_id.is_none() => {
                            path_id = Some(id);
                        }
                        _ => {}
                    }
                }
            }
            text_obj.zip(path_id)
        });

        let Some(((text_id, text), host_path_id)) = pair else {
            self.push_toast("Select one Artistic Text object and one curve/shape", false);
            return;
        };

        let payload = NodePayload::TextOnPath(TextOnPath {
            text,
            path_id: host_path_id,
            offset_mm: 0.0,
            distance_mm: 1.5,
            flip_vertical: false,
            flip_horizontal: false,
        });
        match self.execute_command(CommandOp::EditText {
            id: text_id,
            payload,
        }) {
            Ok(()) => self.push_toast("Fitted text to path", false),
            Err(e) => self.push_toast(e, true),
        }
    }

    /// Inserts a fresh [`VectorTable`] on the active page.
    pub fn insert_table(&mut self, rows: usize, cols: usize) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let center = tab
            .document
            .active_page()
            .map_or(Point2D::new(40.0, 40.0), |p| {
                Point2D::new(p.width_mm * 0.2, p.height_mm * 0.2)
            });
        let table = VectorTable::new(rows.max(1), cols.max(1), 24.0, 10.0);
        let obj = SceneObject::new(
            ObjectId(0),
            format!("Table {rows}x{cols}"),
            NodePayload::Table(table),
        )
        .with_transform(Affine2D::translate(center.x, center.y));
        let _ = self.execute_command(CommandOp::create_object(obj, tab.active_layer_id));
        self.push_toast(format!("Inserted {rows}x{cols} table"), false);
    }

    // -------------------------------------------------------------------------
    // Production, Prepress, Trace, Bitmap Studio, Find & Replace, Macros
    // -------------------------------------------------------------------------

    /// Runs `coree_prod::run_preflight` on the active document.
    pub fn run_preflight_active(&mut self) -> Option<PreflightReport> {
        let min_dpi = self.production_state.preflight_min_dpi;
        let tab = self.tabs.get_mut(self.active_tab)?;
        let rules = PreflightRules {
            min_dpi,
            ..PreflightRules::default()
        };
        let report = run_preflight(&tab.document, &rules);
        let summary = format!(
            "Preflight: {} error(s), {} warning(s), {} info",
            report.error_count(),
            report.warning_count(),
            report.info_count()
        );
        tab.preflight_report = Some(report.clone());
        self.push_toast(summary, report.has_errors());
        Some(report)
    }

    /// Computes CMYK + Spot color separations on the active page.
    pub fn compute_separations_active(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let page_idx = tab.document.active_page_index;
        match compute_separations(&tab.document, page_idx, &SeparationOptions::default()) {
            Ok(sep) => {
                let plate_count = sep.plates.len();
                let tac = sep.max_tac_pct;
                tab.separations_preview = Some(sep);
                self.push_toast(
                    format!("Computed {plate_count} separation plate(s) · Max TAC {tac:.1}%"),
                    false,
                );
            }
            Err(e) => self.push_toast(format!("Separations failed: {e}"), true),
        }
    }

    /// One-click welded `CutContour` spot hairline around selected (or all) objects.
    pub fn generate_cut_contour_active(&mut self) {
        let offset = self.production_state.cut_offset_mm;
        let spot = self.production_state.cut_spot_name.clone();
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let targets: Vec<ObjectId> = if !tab.selection.is_empty() {
            tab.selection.objects.clone()
        } else {
            tab.document
                .active_page()
                .map(|p| {
                    p.layers
                        .iter()
                        .filter(|l| !l.is_cut_layer && l.visible)
                        .flat_map(|l| l.objects.iter().copied())
                        .collect()
                })
                .unwrap_or_default()
        };
        match generate_cut_contour(&mut tab.document, &targets, offset, &spot) {
            Ok(res) => {
                tab.selection.select_single(&tab.document, res.object_id);
                tab.mark_modified();
                self.checkpoint_wal_snapshot_active();
                self.push_toast(
                    format!(
                        "Created {} ({:.1} mm cut path)",
                        res.spot_name, res.cut_length_mm
                    ),
                    false,
                );
            }
            Err(e) => self.push_toast(format!("CutContour failed: {e}"), true),
        }
    }

    /// Generates a parametric packaging dieline (`TuckEndBox`, `Envelope`, `ShoppingBag`, `StickerSheet`).
    pub fn generate_dieline_active(&mut self) {
        let ps = &self.production_state;
        let kind = match ps.dieline_type {
            1 => DielineKind::Envelope {
                width_mm: ps.box_l_mm.max(40.0),
                height_mm: ps.box_h_mm.max(40.0),
                flap_mm: (ps.box_w_mm * 0.6).max(15.0),
            },
            2 => DielineKind::ShoppingBag {
                width_mm: ps.box_l_mm.max(50.0),
                depth_mm: ps.box_w_mm.max(25.0),
                height_mm: ps.box_h_mm.max(60.0),
                fold_mm: 20.0,
            },
            3 => DielineKind::StickerSheet {
                label_w_mm: ps.box_l_mm.clamp(15.0, 120.0),
                label_h_mm: ps.box_w_mm.clamp(15.0, 120.0),
                corner_r_mm: 3.0,
                cols: ps.sticker_cols.clamp(1, 20),
                rows: ps.sticker_rows.clamp(1, 20),
                gap_mm: 4.0,
            },
            _ => DielineKind::TuckEndBox {
                length_mm: ps.box_l_mm.max(20.0),
                width_mm: ps.box_w_mm.max(15.0),
                height_mm: ps.box_h_mm.max(20.0),
                glue_flap_mm: 12.0,
                tuck_flap_mm: 14.0,
            },
        };
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let page_idx = tab.document.active_page_index;
        match generate_dieline(&mut tab.document, page_idx, &kind, Point2D::new(10.0, 10.0)) {
            Ok(res) => {
                tab.mark_modified();
                self.checkpoint_wal_snapshot_active();
                self.push_toast(
                    format!(
                        "Dieline generated: Cut {:.0} mm | Crease {:.0} mm",
                        res.cut_length_mm, res.crease_length_mm
                    ),
                    false,
                );
            }
            Err(e) => self.push_toast(format!("Dieline error: {e}"), true),
        }
    }

    /// Inserts a crisp 1D vector barcode onto the active page.
    pub fn insert_barcode_active(&mut self) {
        let sym = self.production_state.barcode_symbology;
        let payload = self.production_state.barcode_payload.clone();
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let layer_id = tab.active_layer_id;
        match add_barcode_to_document(
            &mut tab.document,
            sym,
            &payload,
            Point2D::new(20.0, 20.0),
            &BarcodeOptions::default(),
            layer_id,
        ) {
            Ok(oid) => {
                tab.selection.select_single(&tab.document, oid);
                tab.mark_modified();
                self.checkpoint_wal_snapshot_active();
                self.push_toast(format!("Inserted {sym:?} barcode"), false);
            }
            Err(e) => self.push_toast(format!("Barcode error: {e}"), true),
        }
    }

    /// Inserts a crisp vector QR code onto the active page.
    pub fn insert_qr_active(&mut self) {
        let payload = self.production_state.qr_payload.clone();
        let opts = QrCodeOptions {
            size_mm: self.production_state.qr_size_mm.clamp(8.0, 500.0),
            ec_level: self.production_state.qr_ec,
            ..QrCodeOptions::default()
        };
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let layer_id = tab.active_layer_id;
        match add_qr_code_to_document(
            &mut tab.document,
            &payload,
            Point2D::new(25.0, 25.0),
            &opts,
            layer_id,
        ) {
            Ok(oid) => {
                tab.selection.select_single(&tab.document, oid);
                tab.mark_modified();
                self.checkpoint_wal_snapshot_active();
                self.push_toast("Inserted vector QR Code", false);
            }
            Err(e) => self.push_toast(format!("QR error: {e}"), true),
        }
    }

    /// Generates vector print marks (crop, bleed, registration, color bars, page slug) on the active page.
    pub fn add_print_marks_active(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let page_idx = tab.document.active_page_index;
        match generate_print_marks(&mut tab.document, page_idx, &PrintMarksOptions::default()) {
            Ok(res) => {
                tab.mark_modified();
                self.checkpoint_wal_snapshot_active();
                self.push_toast(
                    format!("Added {} print mark object(s)", res.total_objects()),
                    false,
                );
            }
            Err(e) => self.push_toast(format!("Print marks failed: {e}"), true),
        }
    }

    /// Runs Imposition (`N-Up`, `Booklet`, `Tiling`) and opens the imposed result in a new document tab.
    pub fn run_imposition_active(&mut self) {
        let ps = &self.production_state;
        let mode = match ps.imposition_kind {
            1 => ImpositionMode::BookletSaddleStitch {
                sheet_w_mm: ps.sheet_w_mm.max(50.0),
                sheet_h_mm: ps.sheet_h_mm.max(50.0),
            },
            2 => ImpositionMode::Tiling {
                tile_w_mm: ps.tile_w_mm.max(30.0),
                tile_h_mm: ps.tile_h_mm.max(30.0),
                overlap_mm: ps.tile_overlap_mm.max(0.0),
            },
            _ => ImpositionMode::NUp {
                cols: ps.nup_cols.clamp(1, 20),
                rows: ps.nup_rows.clamp(1, 20),
                gap_x_mm: 4.0,
                gap_y_mm: 4.0,
                sheet_w_mm: ps.sheet_w_mm.max(50.0),
                sheet_h_mm: ps.sheet_h_mm.max(50.0),
            },
        };
        let Some(doc) = self.active_tab().map(|t| t.document.clone()) else {
            return;
        };
        match impose_document(&doc, &mode) {
            Ok(imposed) => {
                let n = imposed.pages.len();
                self.new_document(imposed);
                self.push_toast(format!("Imposed into {n} press sheet(s)"), false);
            }
            Err(e) => self.push_toast(format!("Imposition error: {e}"), true),
        }
    }

    /// Runs CSV Print Merge on the active document and opens the merged document in a new tab.
    pub fn run_print_merge_active(&mut self) {
        let csv_text = self.production_state.merge_csv_text.clone();
        let Some(doc) = self.active_tab().map(|t| t.document.clone()) else {
            return;
        };
        let table = match PrintMergeTable::from_csv(&csv_text) {
            Ok(t) => t,
            Err(e) => {
                self.push_toast(format!("CSV error: {e}"), true);
                return;
            }
        };
        match apply_print_merge(&doc, &table, &PrintMergeOptions::default()) {
            Ok(merged) => {
                let pages = merged.pages.len();
                self.new_document(merged);
                self.push_toast(format!("Print Merge generated {pages} page(s)"), false);
            }
            Err(e) => self.push_toast(format!("Print Merge failed: {e}"), true),
        }
    }

    /// Runs `Collect for Output` packaging into `output_dir`.
    pub fn run_collect_for_output(&mut self, output_dir: &Path) {
        let Some(doc) = self.active_tab().map(|t| t.document.clone()) else {
            return;
        };
        match collect_for_output(&doc, output_dir, &CollectOptions::default()) {
            Ok(rep) => {
                self.production_state.last_production_note = format!(
                    "Bundled {} file(s) to {}",
                    rep.total_files_written,
                    rep.output_dir.display()
                );
                self.push_toast(self.production_state.last_production_note.clone(), false);
            }
            Err(e) => self.push_toast(format!("Collect failed: {e}"), true),
        }
    }

    /// Traces the selected raster object via `coree_render::trace_raster_to_mm` and adds grouped vector curves.
    pub fn trace_selected_raster(&mut self) -> Result<usize, String> {
        let cfg = self.trace_state.to_config();
        let is_centerline = matches!(cfg.mode, TraceMode::CenterlineTrace);
        let tab = self
            .tabs
            .get_mut(self.active_tab)
            .ok_or_else(|| "no active document".to_owned())?;

        // Find selected raster object (or first raster on active page)
        let target_id = tab
            .selection
            .objects
            .iter()
            .copied()
            .find(|id| {
                tab.document
                    .objects
                    .get(id)
                    .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
            })
            .or_else(|| {
                tab.document
                    .objects
                    .iter()
                    .find(|(_, o)| matches!(o.payload, NodePayload::Raster(_)))
                    .map(|(&id, _)| id)
            })
            .ok_or_else(|| "select a raster bitmap object to run Vector Trace".to_owned())?;

        let (asset, w_mm, h_mm, xf) = {
            let obj = tab
                .document
                .objects
                .get(&target_id)
                .ok_or_else(|| "raster object missing".to_owned())?;
            let NodePayload::Raster(ref r) = obj.payload else {
                return Err("object is not a raster".to_owned());
            };
            let asset = tab
                .document
                .assets
                .get(&r.asset_id)
                .cloned()
                .ok_or_else(|| "embedded raster asset missing".to_owned())?;
            (asset, r.width_mm, r.height_mm, obj.transform)
        };

        let traced = trace_raster_to_mm(&asset, w_mm, h_mm, &cfg);
        let count = traced.len();
        if count == 0 {
            return Err("trace produced 0 contours (adjust detail or colors)".to_owned());
        }

        let mut created_ids = Vec::with_capacity(count);
        for (idx, (path, color)) in traced.into_iter().enumerate() {
            let mut obj = SceneObject::new(
                ObjectId(0),
                format!("Traced Curve {}", idx + 1),
                NodePayload::Path(path),
            )
            .with_transform(xf);
            if is_centerline {
                obj.fill = Fill::None;
                obj.outline = OutlineStyle::solid(color, 0.25);
            } else {
                obj.fill = Fill::Solid(color);
                obj.outline = OutlineStyle::none();
            }
            if let Ok(nid) = tab.document.add_object(obj, tab.active_layer_id) {
                created_ids.push(nid);
            }
        }
        if created_ids.len() > 1 {
            if let Ok(gid) = tab
                .document
                .group_objects(&created_ids, "Vector Trace Result")
            {
                tab.selection.select_single(&tab.document, gid);
            }
        } else if let Some(&single) = created_ids.first() {
            tab.selection.select_single(&tab.document, single);
        }
        tab.mark_modified();
        self.checkpoint_wal_snapshot_active();
        self.push_toast(format!("Vector Trace created {count} curve(s)"), false);
        Ok(count)
    }

    /// Converts the first selected vector object into an embedded raster bitmap at `dpi`.
    /// Matches `coree_render::convert_object_to_bitmap`'s 4.0 mm stroke/shadow padding so
    /// the rasterized object aligns 1:1 in world space with zero shrink or shift.
    pub fn convert_selection_to_bitmap(&mut self) {
        let dpi = self.bitmap_state.convert_dpi.clamp(36.0, 1200.0);
        let outcome = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return;
            };
            let Some(&target_id) = tab.selection.objects.first() else {
                self.push_toast("Select an object to Convert to Bitmap", false);
                return;
            };
            let Some(bb) = tab.document.object_bounds(target_id) else {
                return;
            };
            let padded = bb.expand(4.0);
            match convert_object_to_bitmap(&tab.document, target_id, dpi as f32) {
                Ok(asset) => {
                    let aid = asset.id;
                    tab.document.assets.insert(aid, asset);
                    let raster_obj = SceneObject::new(
                        ObjectId(0),
                        "Rasterized Bitmap",
                        NodePayload::Raster(coree_core::RasterLayerData {
                            asset_id: aid,
                            width_mm: padded.width().max(1.0),
                            height_mm: padded.height().max(1.0),
                            filters: Vec::new(),
                            crop_rect: None,
                        }),
                    )
                    .with_fill(Fill::None)
                    .with_outline(OutlineStyle::none())
                    .with_transform(Affine2D::translate(padded.min_x, padded.min_y));
                    if let Ok(nid) = tab.document.add_object(raster_obj, tab.active_layer_id) {
                        tab.selection.select_single(&tab.document, nid);
                        tab.mark_modified();
                        Ok(())
                    } else {
                        Err("failed to attach rasterized bitmap".to_owned())
                    }
                }
                Err(e) => Err(format!("Convert to Bitmap failed: {e}")),
            }
        };
        match outcome {
            Ok(()) => {
                self.checkpoint_wal_snapshot_active();
                self.push_toast(format!("Converted to bitmap at {dpi:.0} DPI"), false);
            }
            Err(err) => self.push_toast(err, true),
        }
    }

    /// Appends a non-destructive [`RasterFilter`] to the selected raster object.
    pub fn apply_raster_filter_to_selection(&mut self, filter: RasterFilter) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let target = tab.selection.objects.iter().copied().find(|id| {
            tab.document
                .objects
                .get(id)
                .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
        });
        let Some(id) = target else {
            self.push_toast("Select a raster bitmap first", false);
            return;
        };
        let mut data = match &tab.document.objects[&id].payload {
            NodePayload::Raster(r) => r.clone(),
            _ => return,
        };
        data.filters.push(filter);
        let _ = self.execute_command(CommandOp::UpdateRaster { id, data });
        self.push_toast("Applied non-destructive bitmap filter", false);
    }

    /// Resamples the selected raster object's underlying bitmap to `(new_w, new_h)`.
    pub fn resample_selected_bitmap(&mut self, new_w: u32, new_h: u32) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let target = tab.selection.objects.iter().copied().find(|id| {
            tab.document
                .objects
                .get(id)
                .is_some_and(|o| matches!(o.payload, NodePayload::Raster(_)))
        });
        let Some(id) = target else {
            self.push_toast("Select a raster bitmap to Resample", false);
            return;
        };
        let asset_id = match &tab.document.objects[&id].payload {
            NodePayload::Raster(r) => r.asset_id,
            _ => return,
        };
        let Some(orig) = tab.document.assets.get(&asset_id) else {
            return;
        };
        match resample(orig, new_w.clamp(4, 8192), new_h.clamp(4, 8192)) {
            Ok(mut resized) => {
                resized.id = asset_id;
                tab.document.assets.insert(asset_id, resized);
                tab.mark_modified();
                self.checkpoint_wal_snapshot_active();
                self.push_toast(format!("Resampled bitmap to {new_w}x{new_h} px"), false);
            }
            Err(e) => self.push_toast(format!("Resample failed: {e}"), true),
        }
    }

    /// Builds a [`FindQuery`] from [`FindReplaceState`].
    #[must_use]
    pub fn build_find_query(&self) -> FindQuery {
        let fr = &self.find_replace_state;
        FindQuery {
            scope: fr.scope,
            object_kind: fr.object_kind,
            fill_color: None,
            outline_color: None,
            outline_width_min_mm: None,
            outline_width_max_mm: None,
            is_hairline: fr.filter_hairline,
            font_family: (!fr.font_query.trim().is_empty())
                .then(|| fr.font_query.trim().to_owned()),
            text_pattern: (!fr.text_query.is_empty()).then(|| {
                (
                    fr.text_query.clone(),
                    TextMatchMode::Contains {
                        case_sensitive: fr.case_sensitive,
                    },
                )
            }),
            lens_kind: fr.lens_filter,
            layer_id: None,
            layer_name: None,
        }
    }

    /// Finds matching objects and selects them on the active document.
    pub fn run_find_select(&mut self) -> Vec<ObjectId> {
        let query = self.build_find_query();
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return Vec::new();
        };
        let ids = find_objects(&tab.document, &query);
        tab.selection.select_many(&tab.document, &ids);
        tab.metrics_dirty = true;
        self.find_replace_state.last_report = Some(FindReplaceReport {
            matched_ids: ids.clone(),
            modified_ids: Vec::new(),
            replacements_applied: 0,
        });
        self.push_toast(format!("Found {} matching object(s)", ids.len()), false);
        ids
    }

    /// Runs `coree_prod::find_and_replace` with explicit `action`.
    pub fn run_find_and_replace_action(&mut self, action: ReplaceAction) -> FindReplaceReport {
        let query = self.build_find_query();
        let rep = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return FindReplaceReport::default();
            };
            let rep = find_and_replace(&mut tab.document, &query, &action);
            if !rep.matched_ids.is_empty() {
                tab.selection.select_many(&tab.document, &rep.matched_ids);
            }
            if !rep.modified_ids.is_empty() {
                tab.mark_modified();
            }
            rep
        };
        if !rep.modified_ids.is_empty() {
            self.checkpoint_wal_snapshot_active();
        }
        self.find_replace_state.last_report = Some(rep.clone());
        self.push_toast(
            format!(
                "Find & Replace: {} matched, {} updated",
                rep.matched_ids.len(),
                rep.replacements_applied
            ),
            false,
        );
        rep
    }

    /// Starts recording subsequent commands into a Rhai macro.
    pub fn start_macro_recording(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.history.start_macro_recording();
            self.push_toast("Recording Rhai Macro...", false);
        }
    }

    /// Stops macro recording and populates the Macro Docker editor with the generated Rhai script.
    pub fn stop_macro_recording(&mut self) -> String {
        let script = self
            .active_tab_mut()
            .map(|tab| tab.history.stop_macro_to_rhai())
            .unwrap_or_default();
        if !script.is_empty() {
            self.macro_state.script_source = script.clone();
            self.focus_docker(DockerTab::Macros);
            self.push_toast("Macro recorded to Rhai script", false);
        }
        script
    }

    /// Executes `script` inside `coree_prod::macros` strict sandbox against the active document.
    pub fn run_rhai_script(&mut self, script: &str) -> Result<usize, String> {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return Err("no active document".to_owned());
        };
        let cfg = MacroSandboxConfig::default();
        // Support both recorded-command scripts and production macro scripts cleanly
        let res = if script.contains("// coree recorded macro") {
            run_recorded_macro(&mut tab.document, &mut tab.history, script, &cfg)
        } else {
            run_macro(&mut tab.document, script, &BTreeMap::new(), &cfg)
        };
        match res {
            Ok(exec) => {
                tab.mark_modified();
                self.checkpoint_wal_snapshot_active();
                self.macro_state.output_logs = exec.logs;
                self.macro_state.last_error = None;
                self.push_toast(
                    format!("Macro ran ({} command(s))", exec.commands_executed),
                    false,
                );
                Ok(exec.commands_executed)
            }
            Err(e) => {
                let err_str = e.to_string();
                self.macro_state.last_error = Some(err_str.clone());
                self.push_toast(format!("Macro error: {err_str}"), true);
                Err(err_str)
            }
        }
    }

    /// Scans system fonts into SQLite `font_index` and refreshes the Font Manager list.
    pub fn refresh_font_manager(&mut self, rescan: bool) {
        let Some(ref db) = self.storage else {
            return;
        };
        if rescan || !self.font_manager_state.scanned_once {
            let _ = db.scan_system_fonts();
            self.font_manager_state.scanned_once = true;
        }
        let query = FontQuery {
            family_query: (!self.font_manager_state.search_query.trim().is_empty())
                .then(|| self.font_manager_state.search_query.trim().to_owned()),
            arabic_only: self.font_manager_state.arabic_only,
            kashida_only: self.font_manager_state.kashida_only,
            favorites_only: self.font_manager_state.favorites_only,
            collection_tag: None,
            limit: Some(200),
        };
        if let Ok(fonts) = db.search_fonts(&query) {
            self.font_manager_state.indexed_fonts = fonts;
        }
    }

    /// Imports a `.gpl`, `.ase`, or `.pal` palette file into `custom_palettes`.
    pub fn import_palette_from_path(&mut self, path: &Path) -> Result<String, String> {
        let pal = import_palette_file(path).map_err(|e| format!("palette import failed: {e}"))?;
        let name = pal.name.clone();
        self.custom_palettes.push(pal);
        self.active_palette_index = 3 + self.custom_palettes.len();
        self.push_toast(format!("Imported palette '{name}'"), false);
        Ok(name)
    }

    // -------------------------------------------------------------------------
    // Tool Switching, Dockers, Theme, Shortcuts & Command Palette
    // -------------------------------------------------------------------------

    /// Activates `tool` and updates its parent flyout group's remembered primary button.
    pub fn select_tool(&mut self, tool: ToolId) {
        if tool != ToolId::Pick {
            self.previous_non_pick_tool = tool;
        }
        let g = tool.group_index();
        self.group_active_tools[g] = tool;
        self.active_tool = tool;
        self.open_flyout_group = None;
        if let Some(tab) = self.active_tab_mut() {
            tab.poly_points_mm.clear();
            tab.live_sketch_strokes.clear();
        }
    }

    /// Classic Studio `Space` behavior: toggles cleanly between `Pick` and the previous active tool.
    pub fn toggle_pick_space(&mut self) {
        if self.active_tool == ToolId::Pick {
            let prev = self.previous_non_pick_tool;
            self.select_tool(prev);
        } else {
            self.select_tool(ToolId::Pick);
        }
    }

    /// Focuses `docker` in `egui_dock` (adding it to the main surface if it was closed).
    pub fn focus_docker(&mut self, docker: DockerTab) {
        self.show_dockers = true;
        if let Some(tab_path) = self.dock_state.find_tab(&docker) {
            let _ = self.dock_state.set_active_tab(tab_path);
        } else {
            self.dock_state.push_to_first_leaf(docker);
        }
    }

    /// Toggles `docker` open/closed inside `egui_dock`.
    pub fn toggle_docker(&mut self, docker: DockerTab) {
        if let Some(tab_path) = self.dock_state.find_tab(&docker) {
            self.dock_state.remove_tab(tab_path);
        } else {
            self.show_dockers = true;
            if self.dock_state.main_surface().is_empty() {
                self.dock_state = DockState::new(vec![docker]);
            } else {
                self.dock_state.set_focused_node_and_surface(NodePath {
                    surface: SurfaceIndex::main(),
                    node: NodeIndex::root(),
                });
                self.dock_state.push_to_focused_leaf(docker);
            }
        }
    }

    /// Switches the active studio theme and updates every open tab's pasteboard background.
    pub fn set_theme(&mut self, theme: StudioTheme) {
        self.theme = theme;
        self.theme_dirty = true;
        let bg = theme.tokens().canvas_well_core_color();
        for tab in &mut self.tabs {
            tab.render_options.canvas_bg = bg.clone();
            tab.needs_rerender = true;
            tab.tile_cache.mark_all_dirty();
        }
        if let Some(ref db) = self.storage {
            let _ = db.set_preference("ui.theme", "appearance", &theme);
        }
    }

    /// Switches the active keyboard shortcut profile (`ClassicStudio` / `StandardVector`).
    pub fn set_shortcut_profile(&mut self, profile: ShortcutProfile) {
        self.shortcut_profile = profile;
        if let Some(ref db) = self.storage {
            let _ = db.set_preference("ui.shortcut_profile", "shortcuts", &profile);
        }
        self.push_toast(format!("Shortcut profile: {}", profile.label()), false);
    }

    /// Executes a resolved [`ShortcutAction`] cleanly (used both by live egui input and headless tests).
    pub fn handle_shortcut(&mut self, action: ShortcutAction) {
        match action {
            ShortcutAction::TogglePickSpace => self.toggle_pick_space(),
            ShortcutAction::SelectPick => self.select_tool(ToolId::Pick),
            ShortcutAction::SelectShapeNode => self.select_tool(ToolId::ShapeNode),
            ShortcutAction::SelectZoom => self.select_tool(ToolId::Zoom),
            ShortcutAction::SelectPan => self.select_tool(ToolId::Pan),
            ShortcutAction::SelectFreehand => self.select_tool(ToolId::Freehand),
            ShortcutAction::SelectPen => self.select_tool(ToolId::Pen),
            ShortcutAction::SelectRectangle => self.select_tool(ToolId::Rectangle),
            ShortcutAction::SelectEllipse => self.select_tool(ToolId::Ellipse),
            ShortcutAction::SelectPolygon => self.select_tool(ToolId::Polygon),
            ShortcutAction::SelectSpiral => self.select_tool(ToolId::Spiral),
            ShortcutAction::SelectGraphPaper => self.select_tool(ToolId::GraphPaper),
            ShortcutAction::SelectArtisticText => self.select_tool(ToolId::ArtisticText),
            ShortcutAction::SelectInteractiveFill => self.select_tool(ToolId::InteractiveFill),
            ShortcutAction::SelectMeshFill => self.select_tool(ToolId::MeshFill),
            ShortcutAction::SelectEyedropper => self.select_tool(ToolId::ColorEyedropper),
            ShortcutAction::FocusOutlinePen => {
                self.properties_state.active_tab = PropertiesSubTab::Outline;
                self.focus_docker(DockerTab::Properties);
            }
            ShortcutAction::ZoomIn => self.zoom_step(1.25),
            ShortcutAction::ZoomOut => self.zoom_step(0.8),
            ShortcutAction::ZoomToAll => {
                if let Some(t) = self.active_tab_mut() {
                    t.fit_all_objects();
                }
            }
            ShortcutAction::ZoomToPage => {
                if let Some(t) = self.active_tab_mut() {
                    t.fit_active_page();
                }
            }
            ShortcutAction::ZoomActual => {
                if let Some(t) = self.active_tab_mut() {
                    t.viewport.zoom = 1.0;
                    t.needs_rerender = true;
                }
            }
            ShortcutAction::PrevPage => {
                if let Some(t) = self.active_tab_mut() {
                    if t.document.active_page_index > 0 {
                        t.document.active_page_index -= 1;
                        t.selection.clear();
                        t.sync_active_layer();
                        t.needs_rerender = true;
                    }
                }
            }
            ShortcutAction::NextPage => {
                if let Some(t) = self.active_tab_mut() {
                    if t.document.active_page_index + 1 < t.document.pages.len() {
                        t.document.active_page_index += 1;
                        t.selection.clear();
                        t.sync_active_layer();
                        t.needs_rerender = true;
                    }
                }
            }
            ShortcutAction::NewDocument => self.new_doc_dialog.open = true,
            ShortcutAction::OpenDocument => self.open_file_dialog(),
            ShortcutAction::SaveDocument => {
                if self
                    .active_tab()
                    .and_then(|t| t.source_path.as_ref())
                    .is_some()
                {
                    if let Err(e) = self.save_active(None) {
                        self.push_toast(e, true);
                    }
                } else {
                    self.save_as_file_dialog();
                }
            }
            ShortcutAction::SaveDocumentAs => self.save_as_file_dialog(),
            ShortcutAction::ImportFile => self.import_file_dialog(),
            ShortcutAction::ExportFile => self.export_dialog.open = true,
            ShortcutAction::ToggleCommandPalette => {
                self.command_palette.open = !self.command_palette.open;
                if self.command_palette.open {
                    self.command_palette.query.clear();
                    self.command_palette.selected_index = 0;
                    self.command_palette.just_opened = true;
                }
            }
            ShortcutAction::OpenFindReplace => self.focus_docker(DockerTab::FindReplace),
            ShortcutAction::ToggleDockerObjects => self.toggle_docker(DockerTab::Objects),
            ShortcutAction::ToggleDockerProperties => self.toggle_docker(DockerTab::Properties),
            ShortcutAction::ToggleDockerLens => self.toggle_docker(DockerTab::Lens),
            ShortcutAction::Undo => {
                let _ = self.undo();
            }
            ShortcutAction::Redo => {
                let _ = self.redo();
            }
            ShortcutAction::Duplicate => {
                let _ = self.duplicate_selection();
            }
            ShortcutAction::DeleteSelection => self.delete_selection(),
            ShortcutAction::SelectAll => self.select_all(),
            ShortcutAction::EscapeCancel => {
                self.open_flyout_group = None;
                if self.command_palette.open {
                    self.command_palette.open = false;
                } else if self.new_doc_dialog.open {
                    self.new_doc_dialog.open = false;
                } else if self.export_dialog.open {
                    self.export_dialog.open = false;
                } else if let Some(tab) = self.active_tab_mut() {
                    tab.drag_state = CanvasDragState::Idle;
                    tab.poly_points_mm.clear();
                    tab.live_sketch_strokes.clear();
                    tab.selection.clear();
                    tab.metrics_dirty = true;
                }
            }
            ShortcutAction::Group => self.group_selection(),
            ShortcutAction::Ungroup => self.ungroup_selection(),
            ShortcutAction::ConvertToCurves => self.convert_selection_to_curves(),
            ShortcutAction::BreakApart => self.apply_boolean_op(BooleanOp::BreakApart),
            ShortcutAction::Combine => self.apply_boolean_op(BooleanOp::Combine),
            ShortcutAction::ZOrder(op) => self.apply_z_order(op),
            ShortcutAction::Align(op) => self.apply_align(op),
            ShortcutAction::Nudge {
                dx_steps,
                dy_steps,
                mult,
            } => self.nudge_selection(dx_steps, dy_steps, mult),
        }
    }

    /// Zooms the active viewport around its center by `factor`.
    pub fn zoom_step(&mut self, factor: f64) {
        if let Some(tab) = self.active_tab_mut() {
            let center = Point2D::new(
                f64::from(tab.viewport.width_px) * 0.5,
                f64::from(tab.viewport.height_px) * 0.5,
            );
            tab.viewport.zoom_at_screen_point(center, factor);
            tab.needs_rerender = true;
        }
    }

    // -------------------------------------------------------------------------
    // Native `rfd` xdg-portal File Dialog Triggers
    // -------------------------------------------------------------------------

    /// Opens a native OS file picker and opens the chosen document.
    pub fn open_file_dialog(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Open Document")
            .add_filter(
                "All Supported Studio Formats",
                &[
                    "coree", "cdr", "cdt", "cmx", "cpt", "svg", "svgz", "png", "jpg", "jpeg",
                    "webp", "tiff", "tif", "bmp",
                ],
            )
            .add_filter("Native coree", &["coree"])
            .add_filter("CDR / CDT / CMX / CPT", &["cdr", "cdt", "cmx", "cpt"])
            .add_filter("SVG Vector", &["svg", "svgz"])
            .add_filter(
                "Raster Images",
                &["png", "jpg", "jpeg", "webp", "tiff", "bmp"],
            )
            .pick_file();
        if let Some(path) = picked {
            if let Err(err) = self.open_file_path(&path) {
                self.push_toast(err, true);
            }
        }
    }

    /// Opens a native OS file picker and imports the chosen file into the active page.
    pub fn import_file_dialog(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Import into Active Document")
            .add_filter(
                "Importable Files",
                &[
                    "cdr", "cdt", "cmx", "cpt", "svg", "svgz", "coree", "png", "jpg", "jpeg",
                    "webp", "tiff", "bmp",
                ],
            )
            .pick_file();
        if let Some(path) = picked {
            if let Err(err) = self.import_file_path(&path) {
                self.push_toast(err, true);
            }
        }
    }

    /// Opens a native OS Save As dialog for `.coree` / `.cdr` / `.svg`.
    pub fn save_as_file_dialog(&mut self) {
        let default_name = self
            .active_tab()
            .map(|t| format!("{}.coree", t.document.title))
            .unwrap_or_else(|| "Untitled.coree".to_owned());
        let picked = rfd::FileDialog::new()
            .set_title("Save Document As")
            .set_file_name(&default_name)
            .add_filter("Native coree", &["coree"])
            .add_filter("CDR X6+ Archive", &["cdr"])
            .add_filter("CDT Template", &["cdt"])
            .add_filter("SVG Vector", &["svg"])
            .save_file();
        if let Some(path) = picked {
            if let Err(err) = self.save_active_as(&path) {
                self.push_toast(err, true);
            }
        }
    }

    // -------------------------------------------------------------------------
    // Command Palette Registry & Fuzzy Search
    // -------------------------------------------------------------------------

    /// Builds the complete list of searchable Command Palette actions.
    #[must_use]
    pub fn all_palette_commands(&self) -> Vec<PaletteCommand> {
        let p = self.shortcut_profile;
        let mut list = vec![
            PaletteCommand {
                id: "file.new".into(),
                title: "New Document...".into(),
                category: "File".into(),
                shortcut: shortcut_label(p, ShortcutAction::NewDocument).into(),
            },
            PaletteCommand {
                id: "file.sample".into(),
                title: "Open Showcase Sample Document".into(),
                category: "File".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "file.open".into(),
                title: "Open File...".into(),
                category: "File".into(),
                shortcut: shortcut_label(p, ShortcutAction::OpenDocument).into(),
            },
            PaletteCommand {
                id: "file.save".into(),
                title: "Save Document".into(),
                category: "File".into(),
                shortcut: shortcut_label(p, ShortcutAction::SaveDocument).into(),
            },
            PaletteCommand {
                id: "file.export".into(),
                title: "Export Document...".into(),
                category: "File".into(),
                shortcut: shortcut_label(p, ShortcutAction::ExportFile).into(),
            },
            PaletteCommand {
                id: "edit.undo".into(),
                title: "Undo".into(),
                category: "Edit".into(),
                shortcut: shortcut_label(p, ShortcutAction::Undo).into(),
            },
            PaletteCommand {
                id: "edit.redo".into(),
                title: "Redo".into(),
                category: "Edit".into(),
                shortcut: shortcut_label(p, ShortcutAction::Redo).into(),
            },
            PaletteCommand {
                id: "edit.duplicate".into(),
                title: "Duplicate Selection".into(),
                category: "Edit".into(),
                shortcut: shortcut_label(p, ShortcutAction::Duplicate).into(),
            },
            PaletteCommand {
                id: "object.group".into(),
                title: "Group Selection".into(),
                category: "Object".into(),
                shortcut: shortcut_label(p, ShortcutAction::Group).into(),
            },
            PaletteCommand {
                id: "object.ungroup".into(),
                title: "Ungroup Selection".into(),
                category: "Object".into(),
                shortcut: shortcut_label(p, ShortcutAction::Ungroup).into(),
            },
            PaletteCommand {
                id: "object.curves".into(),
                title: "Convert to Curves".into(),
                category: "Object".into(),
                shortcut: shortcut_label(p, ShortcutAction::ConvertToCurves).into(),
            },
            PaletteCommand {
                id: "object.powerclip.create".into(),
                title: "PowerClip: Place Inside Frame".into(),
                category: "Object".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "object.powerclip.extract".into(),
                title: "PowerClip: Extract Contents".into(),
                category: "Object".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "prod.cutcontour".into(),
                title: "One-Click CutContour Spot Hairline".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "prod.preflight".into(),
                title: "Run Print Preflight Audit".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "prod.separations".into(),
                title: "Compute CMYK & Spot Separations".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "prod.printmarks".into(),
                title: "Generate Crop & Registration Marks".into(),
                category: "Production".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.multipage".into(),
                title: "Toggle Multipage View".into(),
                category: "View".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.wireframe".into(),
                title: "View Mode: Wireframe".into(),
                category: "View".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.enhanced".into(),
                title: "View Mode: Enhanced".into(),
                category: "View".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "view.zoom_all".into(),
                title: "Zoom to All Objects".into(),
                category: "View".into(),
                shortcut: shortcut_label(p, ShortcutAction::ZoomToAll).into(),
            },
            PaletteCommand {
                id: "view.zoom_page".into(),
                title: "Zoom to Active Page".into(),
                category: "View".into(),
                shortcut: shortcut_label(p, ShortcutAction::ZoomToPage).into(),
            },
            PaletteCommand {
                id: "text.fit_path".into(),
                title: "Fit Text to Path".into(),
                category: "Text".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "table.insert".into(),
                title: "Insert Vector Table (3x3)".into(),
                category: "Table".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "bitmap.trace".into(),
                title: "Vector Trace Selected Bitmap".into(),
                category: "Bitmaps".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "bitmap.convert".into(),
                title: "Convert Selection to Bitmap".into(),
                category: "Bitmaps".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "theme.dark".into(),
                title: "Theme: Dark Studio".into(),
                category: "Appearance".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "theme.light".into(),
                title: "Theme: Classic Light".into(),
                category: "Appearance".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "theme.contrast".into(),
                title: "Theme: High Contrast".into(),
                category: "Appearance".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "profile.classic".into(),
                title: "Shortcuts: Classic Studio".into(),
                category: "Shortcuts".into(),
                shortcut: String::new(),
            },
            PaletteCommand {
                id: "profile.standard".into(),
                title: "Shortcuts: Standard Vector".into(),
                category: "Shortcuts".into(),
                shortcut: String::new(),
            },
        ];

        // All 9 Boolean Shaping operations
        for (id_suffix, title) in [
            ("weld", "Shaping: Weld"),
            ("trim", "Shaping: Trim"),
            ("intersect", "Shaping: Intersect"),
            ("simplify", "Shaping: Simplify"),
            ("front_minus_back", "Shaping: Front Minus Back"),
            ("back_minus_front", "Shaping: Back Minus Front"),
            ("boundary", "Shaping: Create Boundary"),
            ("combine", "Shaping: Combine"),
            ("break_apart", "Shaping: Break Apart"),
        ] {
            list.push(PaletteCommand {
                id: format!("bool.{id_suffix}"),
                title: title.into(),
                category: "Shaping".into(),
                shortcut: String::new(),
            });
        }

        // All 11 Lenses
        for (idx, name) in LensDockerState::NAMES.iter().enumerate() {
            list.push(PaletteCommand {
                id: format!("lens.{idx}"),
                title: format!("Apply Lens: {name}"),
                category: "Lens".into(),
                shortcut: String::new(),
            });
        }

        // All 14 Dockers
        for docker in DockerTab::ALL {
            list.push(PaletteCommand {
                id: format!("docker.{}", docker.title()),
                title: format!("Show Docker: {}", docker.title()),
                category: "Window".into(),
                shortcut: String::new(),
            });
        }

        // All Tools across all 15 Toolbox Flyout groups
        for group in TOOL_GROUPS {
            for &tool in group {
                let sc = tool
                    .shortcut_action()
                    .map(|a| shortcut_label(p, a))
                    .unwrap_or("");
                list.push(PaletteCommand {
                    id: format!("tool.{tool:?}"),
                    title: format!("Tool: {}", tool.label()),
                    category: "Toolbox".into(),
                    shortcut: sc.into(),
                });
            }
        }

        // Built-in Rhai Macros
        for m in builtin_macros() {
            list.push(PaletteCommand {
                id: format!("macro.{}", m.id),
                title: format!("Run Macro: {}", m.name),
                category: "Macros".into(),
                shortcut: String::new(),
            });
        }

        list
    }

    /// Fuzzy-filters Command Palette commands ordered by match quality.
    #[must_use]
    pub fn search_command_palette(&self, query: &str) -> Vec<PaletteCommand> {
        let q = query.trim().to_ascii_lowercase();
        let all = self.all_palette_commands();
        if q.is_empty() {
            return all;
        }
        let mut scored: Vec<(i32, PaletteCommand)> = all
            .into_iter()
            .filter_map(|cmd| {
                let hay = format!("{} {}", cmd.title, cmd.category).to_ascii_lowercase();
                fuzzy_score(&hay, &q).map(|s| (s, cmd))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
        scored.into_iter().map(|(_, c)| c).collect()
    }

    /// Executes a Command Palette action by its `id`.
    pub fn execute_palette_command(&mut self, id: &str) -> bool {
        match id {
            "file.new" => self.new_doc_dialog.open = true,
            "file.sample" => self.new_sample_document(),
            "file.open" => self.open_file_dialog(),
            "file.save" => self.handle_shortcut(ShortcutAction::SaveDocument),
            "file.export" => self.export_dialog.open = true,
            "edit.undo" => {
                let _ = self.undo();
            }
            "edit.redo" => {
                let _ = self.redo();
            }
            "edit.duplicate" => {
                let _ = self.duplicate_selection();
            }
            "object.group" => self.group_selection(),
            "object.ungroup" => self.ungroup_selection(),
            "object.curves" => self.convert_selection_to_curves(),
            "object.powerclip.create" => self.create_powerclip_from_selection(),
            "object.powerclip.extract" => self.extract_selected_powerclip(),
            "prod.cutcontour" => self.generate_cut_contour_active(),
            "prod.preflight" => {
                self.run_preflight_active();
                self.production_state.sub_section = 0;
                self.focus_docker(DockerTab::Production);
            }
            "prod.separations" => {
                self.compute_separations_active();
                self.production_state.sub_section = 1;
                self.focus_docker(DockerTab::Production);
            }
            "prod.printmarks" => self.add_print_marks_active(),
            "view.multipage" => {
                if let Some(t) = self.active_tab_mut() {
                    t.document.multipage_view = !t.document.multipage_view;
                    t.viewport.multipage_view = t.document.multipage_view;
                    t.needs_rerender = true;
                }
            }
            "view.wireframe" => {
                if let Some(t) = self.active_tab_mut() {
                    t.view_mode = ViewMode::Wireframe;
                    t.needs_rerender = true;
                }
            }
            "view.enhanced" => {
                if let Some(t) = self.active_tab_mut() {
                    t.view_mode = ViewMode::Enhanced;
                    t.needs_rerender = true;
                }
            }
            "view.zoom_all" => self.handle_shortcut(ShortcutAction::ZoomToAll),
            "view.zoom_page" => self.handle_shortcut(ShortcutAction::ZoomToPage),
            "text.fit_path" => self.fit_selected_text_to_path(),
            "table.insert" => self.insert_table(3, 3),
            "bitmap.trace" => {
                if let Err(e) = self.trace_selected_raster() {
                    self.push_toast(e, true);
                }
            }
            "bitmap.convert" => self.convert_selection_to_bitmap(),
            "theme.dark" => self.set_theme(StudioTheme::DarkStudio),
            "theme.light" => self.set_theme(StudioTheme::ClassicLight),
            "theme.contrast" => self.set_theme(StudioTheme::HighContrast),
            "profile.classic" => self.set_shortcut_profile(ShortcutProfile::ClassicStudio),
            "profile.standard" => self.set_shortcut_profile(ShortcutProfile::StandardVector),
            "bool.weld" => self.apply_boolean_op(BooleanOp::Weld),
            "bool.trim" => self.apply_boolean_op(BooleanOp::Trim),
            "bool.intersect" => self.apply_boolean_op(BooleanOp::Intersect),
            "bool.simplify" => self.apply_boolean_op(BooleanOp::Simplify),
            "bool.front_minus_back" => self.apply_boolean_op(BooleanOp::FrontMinusBack),
            "bool.back_minus_front" => self.apply_boolean_op(BooleanOp::BackMinusFront),
            "bool.boundary" => self.apply_boolean_op(BooleanOp::CreateBoundary),
            "bool.combine" => self.apply_boolean_op(BooleanOp::Combine),
            "bool.break_apart" => self.apply_boolean_op(BooleanOp::BreakApart),
            other => {
                if let Some(idx_str) = other.strip_prefix("lens.") {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if idx < 11 {
                            self.lens_state.selected_index = idx;
                            let cfg = self.lens_state.build_config();
                            self.apply_lens_to_selection(Some(cfg));
                            self.focus_docker(DockerTab::Lens);
                            return true;
                        }
                    }
                }
                if let Some(title) = other.strip_prefix("docker.") {
                    if let Some(d) = DockerTab::ALL.into_iter().find(|d| d.title() == title) {
                        self.focus_docker(d);
                        return true;
                    }
                }
                if let Some(tool_dbg) = other.strip_prefix("tool.") {
                    for group in TOOL_GROUPS {
                        for &tool in group {
                            if format!("{tool:?}") == tool_dbg {
                                self.select_tool(tool);
                                return true;
                            }
                        }
                    }
                }
                if let Some(macro_id) = other.strip_prefix("macro.") {
                    if let Some(m) = coree_prod::find_builtin_macro(macro_id) {
                        let _ = self.run_rhai_script(m.script);
                        return true;
                    }
                }
                return false;
            }
        }
        true
    }

    /// Creates a quick vector object from a canvas drag rectangle for the current drawing tool.
    pub fn commit_drawn_box(&mut self, rect: Rect2D) {
        if rect.width() < 0.5 && rect.height() < 0.5 {
            return;
        }
        let Some(tab) = self.active_tab() else {
            return;
        };
        let layer_id = tab.active_layer_id;
        let fill = self.default_fill.clone();
        let outline = self.default_outline.clone();
        let ts = &self.tool_settings;

        let (name, payload, xf) = match self.active_tool {
            ToolId::Rectangle | ToolId::ThreePointRectangle => (
                "Rectangle".to_owned(),
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: rect.width(),
                    height: rect.height(),
                    corner_radii: [ts.rect_corner_radius_mm; 4],
                    corner_style: ts.rect_corner_style,
                    lock_corners: true,
                }),
                Affine2D::translate(rect.min_x, rect.min_y),
            ),
            ToolId::Ellipse | ToolId::ThreePointEllipse => (
                "Ellipse".to_owned(),
                NodePayload::Shape(ParametricShape::Ellipse {
                    rx: rect.width() * 0.5,
                    ry: rect.height() * 0.5,
                    mode: ts.ellipse_mode,
                    start_deg: 0.0,
                    end_deg: 360.0,
                }),
                Affine2D::translate(rect.center().x, rect.center().y),
            ),
            ToolId::Polygon => (
                "Polygon".to_owned(),
                NodePayload::Shape(ParametricShape::Polygon {
                    sides: ts.polygon_sides.clamp(3, 64),
                    radius_x: rect.width() * 0.5,
                    radius_y: rect.height() * 0.5,
                }),
                Affine2D::translate(rect.center().x, rect.center().y),
            ),
            ToolId::Star | ToolId::ComplexStar => (
                "Star".to_owned(),
                NodePayload::Shape(ParametricShape::Star {
                    points: ts.star_points.clamp(3, 64),
                    outer_radius: (rect.width().max(rect.height())) * 0.5,
                    inner_ratio: ts.star_inner_ratio.clamp(0.1, 0.9),
                    complex: self.active_tool == ToolId::ComplexStar,
                }),
                Affine2D::translate(rect.center().x, rect.center().y),
            ),
            ToolId::Spiral => (
                "Spiral".to_owned(),
                NodePayload::Shape(ParametricShape::Spiral {
                    revolutions: ts.spiral_revolutions.clamp(1.0, 30.0),
                    radius: (rect.width().max(rect.height())) * 0.5,
                    logarithmic: ts.spiral_logarithmic,
                    expansion: 1.5,
                }),
                Affine2D::translate(rect.center().x, rect.center().y),
            ),
            ToolId::GraphPaper => (
                "Graph Paper".to_owned(),
                NodePayload::Shape(ParametricShape::GraphPaper {
                    cols: ts.graph_cols.clamp(1, 50),
                    rows: ts.graph_rows.clamp(1, 50),
                    width: rect.width(),
                    height: rect.height(),
                }),
                Affine2D::translate(rect.min_x, rect.min_y),
            ),
            ToolId::Impact => (
                "Impact Rays".to_owned(),
                NodePayload::Shape(ParametricShape::Impact {
                    rays: ts.impact_rays.clamp(4, 128),
                    inner_radius: (rect.width().min(rect.height())) * 0.15,
                    outer_radius: (rect.width().max(rect.height())) * 0.5,
                    style: ts.impact_style,
                }),
                Affine2D::translate(rect.center().x, rect.center().y),
            ),
            ToolId::BasicShapes => (
                format!("{:?}", ts.basic_shape_kind),
                NodePayload::Shape(ParametricShape::BasicShape {
                    kind: ts.basic_shape_kind,
                    width: rect.width(),
                    height: rect.height(),
                }),
                Affine2D::translate(rect.min_x, rect.min_y),
            ),
            ToolId::ParagraphText => (
                "Paragraph Text".to_owned(),
                NodePayload::ParagraphText(coree_core::ParagraphText::new(
                    ts.text_insert_draft.clone(),
                    Vec2D::new(rect.width().max(20.0), rect.height().max(12.0)),
                    self.default_text_style.clone(),
                )),
                Affine2D::translate(rect.min_x, rect.min_y),
            ),
            ToolId::Table => {
                let rows = ts.table_rows.clamp(1, 30);
                let cols = ts.table_cols.clamp(1, 30);
                (
                    format!("Table {rows}x{cols}"),
                    NodePayload::Table(VectorTable::new(
                        rows,
                        cols,
                        (rect.width() / cols as f64).max(4.0),
                        (rect.height() / rows as f64).max(4.0),
                    )),
                    Affine2D::translate(rect.min_x, rect.min_y),
                )
            }
            ToolId::LensTool => {
                let lens = self.lens_state.build_config();
                let mut obj = SceneObject::new(
                    ObjectId(0),
                    format!("{} Lens", lens.kind.name()),
                    NodePayload::Shape(ParametricShape::Ellipse {
                        rx: rect.width() * 0.5,
                        ry: rect.height() * 0.5,
                        mode: EllipseMode::Full,
                        start_deg: 0.0,
                        end_deg: 360.0,
                    }),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::solid(Color::KEY_BLACK, 0.25))
                .with_transform(Affine2D::translate(rect.center().x, rect.center().y));
                obj.lens = Some(lens);
                let _ = self.execute_command(CommandOp::create_object(obj, layer_id));
                return;
            }
            ToolId::ParallelDimension
            | ToolId::HvDimension
            | ToolId::AngularDimension
            | ToolId::SegmentDimension
            | ToolId::ThreePointCallout => {
                let kind = match self.active_tool {
                    ToolId::HvDimension => {
                        if rect.width() >= rect.height() {
                            DimensionKind::Horizontal
                        } else {
                            DimensionKind::Vertical
                        }
                    }
                    ToolId::AngularDimension => DimensionKind::Angular,
                    ToolId::SegmentDimension => DimensionKind::Segment,
                    ToolId::ThreePointCallout => DimensionKind::Callout3Pt,
                    _ => DimensionKind::Parallel,
                };
                let dim = if kind == DimensionKind::Angular {
                    coree_core::DimensionObject::angular(
                        rect.min(),
                        Point2D::new(rect.max_x, rect.min_y),
                        rect.max(),
                        ts.dimension_offset_mm,
                    )
                } else {
                    coree_core::DimensionObject::linear(
                        kind,
                        rect.min(),
                        rect.max(),
                        ts.dimension_offset_mm,
                    )
                };
                (
                    format!("{kind:?} Dimension"),
                    NodePayload::Dimension(dim),
                    Affine2D::IDENTITY,
                )
            }
            ToolId::StraightConnector | ToolId::RightAngleConnector => {
                let style = if self.active_tool == ToolId::RightAngleConnector {
                    coree_core::ConnectorStyle::RightAngle
                } else {
                    coree_core::ConnectorStyle::Straight
                };
                let conn = coree_core::ConnectorObject {
                    from_obj: None,
                    from_anchor: coree_core::AnchorPoint::Center,
                    from_fallback: rect.min(),
                    to_obj: None,
                    to_anchor: coree_core::AnchorPoint::Center,
                    to_fallback: rect.max(),
                    style,
                    corner_radius_mm: 2.0,
                };
                (
                    "Connector".to_owned(),
                    NodePayload::Connector(conn),
                    Affine2D::IDENTITY,
                )
            }
            ToolId::Crop => {
                // Keep only objects intersecting crop rect on active page
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    if let Some(page) = tab.document.active_page_mut() {
                        page.width_mm = rect.width().max(10.0);
                        page.height_mm = rect.height().max(10.0);
                    }
                    tab.mark_modified();
                    self.push_toast(
                        format!(
                            "Cropped page to {:.1} x {:.1} mm",
                            rect.width(),
                            rect.height()
                        ),
                        false,
                    );
                }
                return;
            }
            _ => return,
        };

        let is_open_stroke = matches!(
            self.active_tool,
            ToolId::Spiral
                | ToolId::ParallelDimension
                | ToolId::HvDimension
                | ToolId::AngularDimension
                | ToolId::SegmentDimension
                | ToolId::ThreePointCallout
                | ToolId::StraightConnector
                | ToolId::RightAngleConnector
        );
        let obj = SceneObject::new(ObjectId(0), name, payload)
            .with_fill(if is_open_stroke { Fill::None } else { fill })
            .with_outline(outline)
            .with_transform(xf);
        let _ = self.execute_command(CommandOp::create_object(obj, layer_id));
        if let Some(tab) = self.active_tab_mut() {
            if let Some(&last_id) = tab.document.objects.keys().last() {
                tab.selection.select_single(&tab.document, last_id);
            }
        }
    }

    /// Creates an ArtisticText object at `pos_mm`.
    pub fn insert_artistic_text_at(&mut self, pos_mm: Point2D) {
        let layer_id = self.active_tab().and_then(|t| t.active_layer_id);
        let text = ArtisticText::from_plain(
            self.tool_settings.text_insert_draft.clone(),
            self.default_text_style.clone(),
        );
        let obj = SceneObject::new(
            ObjectId(0),
            "Artistic Text",
            NodePayload::ArtisticText(text),
        )
        .with_fill(Fill::Solid(self.default_text_style.fill.clone()))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(pos_mm.x, pos_mm.y));
        let _ = self.execute_command(CommandOp::create_object(obj, layer_id));
        if let Some(tab) = self.active_tab_mut() {
            if let Some(&last_id) = tab.document.objects.keys().last() {
                tab.selection.select_single(&tab.document, last_id);
            }
        }
    }

    /// Computes the object-local bounding box of `id` (before `obj.transform`), matching
    /// `coree-render`'s local coordinate space for `MeshFill`, `Distort`, `Envelope`, and `Perspective`.
    #[must_use]
    pub fn object_local_bounds(doc: &Document, id: ObjectId) -> Option<Rect2D> {
        let obj = doc.objects.get(&id)?;
        let direct = match &obj.payload {
            NodePayload::Path(p) => p.bounding_box(),
            NodePayload::Shape(s) => s.to_bezier_path().bounding_box(),
            NodePayload::ArtisticText(t) => Some(t.estimated_bounds()),
            NodePayload::ParagraphText(t) => Some(t.bounding_box()),
            NodePayload::TextOnPath(top) => Some(top.text.estimated_bounds()),
            NodePayload::Raster(r) => match r.crop_rect {
                Some(cr) if cr.is_finite() => Some(cr),
                None if r.width_mm.is_finite() && r.height_mm.is_finite() => {
                    Some(Rect2D::from_xywh(0.0, 0.0, r.width_mm, r.height_mm))
                }
                _ => None,
            },
            NodePayload::Table(t) => Some(t.to_bounding_box()),
            NodePayload::Dimension(d) => Some(d.bounding_box()),
            NodePayload::PowerClip { container, .. } => container.bounding_box(),
            NodePayload::Connector(_) | NodePayload::Group(_) | NodePayload::SymbolInstance(_) => {
                doc.object_bounds(id).and_then(|wb| {
                    let inv = obj.transform.inverse().unwrap_or(Affine2D::IDENTITY);
                    inv.try_transform_rect(wb)
                })
            }
        };
        direct.filter(|r| r.is_finite())
    }

    /// Object-local bounding box of the primary selected object on the active tab.
    #[must_use]
    pub fn selected_local_bounds(&self) -> Option<Rect2D> {
        let tab = self.active_tab()?;
        let id = *tab.selection.objects.first()?;
        Self::object_local_bounds(&tab.document, id)
    }

    /// Helper to build a default `MeshFill` grid in **object-local** coordinates matching
    /// `coree-render` (`rows` x `cols` node grid, clamped `2..=32`, emitting `rows * cols` nodes).
    #[must_use]
    pub fn build_mesh_fill_for_bounds(local_bounds: Rect2D, rows: usize, cols: usize) -> Fill {
        let rows = rows.clamp(2, 32);
        let cols = cols.clamp(2, 32);
        let mut nodes = Vec::with_capacity(rows * cols);
        for r in 0..rows {
            let v = r as f64 / (rows - 1) as f64;
            for c in 0..cols {
                let u = c as f64 / (cols - 1) as f64;
                let pos = Point2D::new(
                    local_bounds.min_x + local_bounds.width() * u,
                    local_bounds.min_y + local_bounds.height() * v,
                );
                let color = Color::cmyk(u as f32, v as f32, ((1.0 - u) * 0.4) as f32, 0.0);
                nodes.push(MeshNode {
                    row: r,
                    col: c,
                    pos,
                    color,
                });
            }
        }
        Fill::MeshFill { rows, cols, nodes }
    }

    /// Adds a document guide line via undoable [`CommandOp::AddGuide`].
    pub fn add_guide(&mut self, orientation: GuideOrientation) {
        let _ = self.execute_command(CommandOp::AddGuide {
            orientation,
            locked: false,
        });
    }

    /// Builds the current effect from [`EffectsDockerState`].
    /// Geometry-deforming effects (`Distort`, `Envelope`, `Perspective`) use **object-local**
    /// coordinates matching `coree_render::apply_geometry_effects`, and `Envelope` emits a full
    /// 8-point `[TL, T_mid, TR, R_mid, BR, B_mid, BL, L_mid]` Coons cage.
    #[must_use]
    pub fn build_selected_effect(&self) -> Effect {
        let es = &self.effects_state;
        let local_bb = self
            .selected_local_bounds()
            .unwrap_or_else(|| Rect2D::from_xywh(0.0, 0.0, 40.0, 40.0));
        match es.selected_effect_kind {
            0 => Effect::DropShadow {
                dx_mm: es.dx_mm,
                dy_mm: es.dy_mm,
                blur_mm: es.blur_mm.max(0.0),
                opacity: es.opacity.clamp(0.0, 1.0),
                color: es.color.clone(),
                feather_direction: FeatherDirection::Middle,
            },
            1 => Effect::BlockShadow {
                dx_mm: es.dx_mm,
                dy_mm: es.dy_mm,
                color: es.color.clone(),
                simplify: true,
                overprint: false,
            },
            2 => Effect::Contour {
                mode: es.contour_mode,
                steps: es.steps.clamp(1, 50),
                offset_mm: es.offset_mm.max(0.2),
                fill_color: es.color.clone(),
                outline_color: Color::BLACK,
            },
            3 => {
                let target_id = self
                    .active_tab()
                    .and_then(|t| t.selection.objects.get(1).copied())
                    .unwrap_or(ObjectId(1));
                Effect::Blend {
                    target_id,
                    steps: es.steps.clamp(1, 100),
                    rotation_deg: 0.0,
                    along_path: None,
                }
            }
            4 => Effect::Distort {
                kind: es.distort_kind,
                amplitude: es.amplitude,
                frequency: es.frequency.max(0.2),
                center: local_bb.center(),
            },
            5 => {
                let cx = local_bb.center().x;
                let cy = local_bb.center().y;
                let bulge_y = (local_bb.height() * 0.15).clamp(2.0, 12.0);
                // 8-point Coons cage: [TL, T_mid, TR, R_mid, BR, B_mid, BL, L_mid]
                Effect::Envelope {
                    mode: es.envelope_mode,
                    cage: vec![
                        Point2D::new(local_bb.min_x, local_bb.min_y),
                        Point2D::new(cx, local_bb.min_y - bulge_y),
                        Point2D::new(local_bb.max_x, local_bb.min_y),
                        Point2D::new(local_bb.max_x, cy),
                        Point2D::new(local_bb.max_x, local_bb.max_y),
                        Point2D::new(cx, local_bb.max_y + bulge_y),
                        Point2D::new(local_bb.min_x, local_bb.max_y),
                        Point2D::new(local_bb.min_x, cy),
                    ],
                }
            }
            6 => Effect::Extrude25D {
                vanishing_point: Point2D::new(140.0, 40.0),
                depth: es.extrude_depth.max(1.0),
                bevel_depth: 1.0,
                shade_color: es.color.clone(),
                light_dir: [0.4, -0.5, 0.75],
            },
            7 => Effect::Transparency {
                mode: TransparencyMode::Uniform(es.opacity.clamp(0.0, 1.0)),
                blend_mode: es.blend_mode,
            },
            8 => Effect::Perspective {
                quad: [
                    Point2D::new(local_bb.min_x + local_bb.width() * 0.12, local_bb.min_y),
                    Point2D::new(local_bb.max_x - local_bb.width() * 0.12, local_bb.min_y),
                    local_bb.max(),
                    Point2D::new(local_bb.min_x, local_bb.max_y),
                ],
            },
            _ => Effect::Symmetry {
                axes: es.symmetry_axes.clamp(2, 24),
                mirror: es.symmetry_mirror,
                origin: self
                    .active_tab()
                    .and_then(|t| t.selection.bounding_box(&t.document))
                    .map_or(Point2D::new(105.0, 148.0), |b| b.center()),
            },
        }
    }

    /// Applies an artistic media stroke expansion onto a freehand curve.
    #[must_use]
    pub fn expand_artistic_stroke(&self, centerline: &BezierPath) -> BezierPath {
        coree_core::ArtisticMedia::expand_stroke(
            centerline,
            &ArtisticMediaStyle::Preset {
                max_width_mm: self.tool_settings.artistic_media_width_mm,
                profile: self.tool_settings.artistic_media_profile.clone(),
            },
        )
    }

    /// Returns the production CutContour spot palette.
    #[must_use]
    pub fn cut_production_palette() -> ColorPalette {
        let mut pal = ColorPalette::new("CutContour & Production", ColorMode::Cmyk);
        pal.swatches = vec![
            ColorSwatch::new("CutContour", Color::cut_contour_spot()),
            ColorSwatch::new(
                "PerfCutContour",
                Color::spot("PerfCutContour", 1.0, [1.0, 0.0, 0.0, 0.0]),
            ),
            ColorSwatch::new("Crease", Color::spot("Crease", 1.0, [0.0, 0.7, 1.0, 0.0])),
            ColorSwatch::new(
                "Spot White",
                Color::spot("SpotWhite", 1.0, [0.1, 0.0, 0.0, 0.0]),
            ),
            ColorSwatch::new(
                "Spot Varnish",
                Color::spot("SpotVarnish", 1.0, [0.0, 0.0, 1.0, 0.0]),
            ),
            ColorSwatch::new(
                "Foil Gold",
                Color::spot("FoilGold", 1.0, [0.0, 0.25, 0.85, 0.1]),
            ),
            ColorSwatch::new(
                "Registration",
                Color::spot("Registration", 1.0, [1.0, 1.0, 1.0, 1.0]),
            ),
        ];
        pal
    }

    /// Inspects the active document into its `inspection_report` (for the CDR Inspector docker).
    /// When `force` is true, or when the report is missing or the document is dirty, recomputes
    /// fresh structural metadata (preserving on-disk binary RIFF/ZIP details when clean).
    pub fn refresh_inspector_for_active(&mut self, force: bool) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            if force || tab.inspection_report.is_none() || tab.dirty {
                if !tab.dirty {
                    if let Some(ref src_path) = tab.source_path {
                        if let Ok(rep) = inspect_any_file(src_path) {
                            tab.inspection_report = Some(rep);
                            return;
                        }
                    }
                }
                tab.inspection_report = Some(inspect_in_memory_document(&tab.document));
            }
        }
    }

    /// Adds a new layer on the active page.
    pub fn add_layer_active(&mut self, name: String, is_cut: bool, scope: MasterLayerScope) {
        let Some(page_id) = self
            .active_tab()
            .and_then(|t| t.document.active_page())
            .map(|p| p.id)
        else {
            return;
        };
        let _ = self.execute_command(CommandOp::AddLayer {
            page_id,
            name,
            is_cut_layer: is_cut,
            master_scope: scope,
        });
    }
}

/// Recursively builds deterministic, undoable [`CommandOp`]s to clone an object subtree
/// (including `Group` and `PowerClip` hierarchies) with cycle/depth guards.
#[allow(clippy::too_many_arguments)]
fn build_duplicate_tree_ops(
    doc: &Document,
    src_id: ObjectId,
    layer_id: Option<LayerId>,
    delta_xf: Affine2D,
    depth: usize,
    next_id: &mut u64,
    visiting: &mut HashSet<ObjectId>,
    id_map: &mut HashMap<ObjectId, ObjectId>,
    ops: &mut Vec<CommandOp>,
) -> Option<ObjectId> {
    if depth > 64 || !visiting.insert(src_id) {
        return None;
    }
    let orig = match doc.objects.get(&src_id) {
        Some(o) => o,
        None => {
            visiting.remove(&src_id);
            return None;
        }
    };

    let result_id = match &orig.payload {
        NodePayload::Group(children) => {
            let mut new_children = Vec::with_capacity(children.len());
            for &cid in children {
                if let Some(nid) = build_duplicate_tree_ops(
                    doc,
                    cid,
                    layer_id,
                    Affine2D::IDENTITY,
                    depth + 1,
                    next_id,
                    visiting,
                    id_map,
                    ops,
                ) {
                    new_children.push(nid);
                }
            }
            if new_children.is_empty() {
                None
            } else {
                let group_id = ObjectId(*next_id);
                *next_id = next_id.saturating_add(1);
                id_map.insert(src_id, group_id);
                ops.push(CommandOp::Group {
                    ids: new_children,
                    name: orig.name.clone(),
                });
                let grp_xf = delta_xf.compose(&orig.transform);
                if grp_xf != Affine2D::IDENTITY {
                    ops.push(CommandOp::TransformObjects {
                        ids: vec![group_id],
                        affine: grp_xf,
                    });
                }
                if orig.lens.is_some() {
                    ops.push(CommandOp::SetLens {
                        ids: vec![group_id],
                        lens: orig.lens.clone(),
                    });
                }
                for eff in &orig.effects {
                    ops.push(CommandOp::AddEffect {
                        id: group_id,
                        effect: eff.clone(),
                    });
                }
                Some(group_id)
            }
        }
        NodePayload::PowerClip {
            container,
            contents,
        } => {
            let container_id = ObjectId(*next_id);
            *next_id = next_id.saturating_add(1);
            id_map.insert(src_id, container_id);

            let mut container_obj = orig.clone();
            container_obj.id = container_id;
            container_obj.payload = NodePayload::Path((**container).clone());
            container_obj.transform = delta_xf.compose(&orig.transform);
            let container_xf = container_obj.transform;
            ops.push(CommandOp::create_object(container_obj, layer_id));

            let mut new_contents = Vec::with_capacity(contents.len());
            for &cid in contents {
                // `create_powerclip` converts child transforms by `inv(container_xf)`, so
                // pre-compose `container_xf` onto each cloned child to preserve exact local coords.
                if let Some(nid) = build_duplicate_tree_ops(
                    doc,
                    cid,
                    layer_id,
                    container_xf,
                    depth + 1,
                    next_id,
                    visiting,
                    id_map,
                    ops,
                ) {
                    new_contents.push(nid);
                }
            }
            ops.push(CommandOp::CreatePowerClip {
                container_id,
                content_ids: new_contents,
            });
            Some(container_id)
        }
        _ => {
            let new_id = ObjectId(*next_id);
            *next_id = next_id.saturating_add(1);
            id_map.insert(src_id, new_id);
            let mut cloned = orig.clone();
            cloned.id = new_id;
            cloned.transform = delta_xf.compose(&cloned.transform);
            ops.push(CommandOp::create_object(cloned, layer_id));
            Some(new_id)
        }
    };

    visiting.remove(&src_id);
    result_id
}

/// Remaps cross-object references (`TextOnPath`, `Connector`, `ParagraphText`, `Effect::Blend`)
/// inside queued duplicate [`CommandOp`]s according to `id_map`.
fn remap_duplicate_ops(ops: &mut [CommandOp], id_map: &HashMap<ObjectId, ObjectId>) {
    if id_map.is_empty() {
        return;
    }
    for op in ops {
        match op {
            CommandOp::CreateObject { object, .. } => {
                match &mut object.payload {
                    NodePayload::TextOnPath(top) => {
                        if let Some(&mapped) = id_map.get(&top.path_id) {
                            top.path_id = mapped;
                        }
                    }
                    NodePayload::Connector(conn) => {
                        if let Some(fid) = conn.from_obj {
                            if let Some(&mapped) = id_map.get(&fid) {
                                conn.from_obj = Some(mapped);
                            }
                        }
                        if let Some(tid) = conn.to_obj {
                            if let Some(&mapped) = id_map.get(&tid) {
                                conn.to_obj = Some(mapped);
                            }
                        }
                    }
                    NodePayload::ParagraphText(para) => {
                        if let Some(next_id) = para.linked_next_frame {
                            if let Some(&mapped) = id_map.get(&next_id) {
                                para.linked_next_frame = Some(mapped);
                            }
                        }
                    }
                    _ => {}
                }
                for eff in &mut object.effects {
                    if let Effect::Blend {
                        target_id,
                        along_path,
                        ..
                    } = eff
                    {
                        if let Some(&mapped) = id_map.get(target_id) {
                            *target_id = mapped;
                        }
                        if let Some(ap) = *along_path {
                            if let Some(&mapped) = id_map.get(&ap) {
                                *along_path = Some(mapped);
                            }
                        }
                    }
                }
            }
            CommandOp::AddEffect {
                effect:
                    Effect::Blend {
                        target_id,
                        along_path,
                        ..
                    },
                ..
            } => {
                if let Some(&mapped) = id_map.get(target_id) {
                    *target_id = mapped;
                }
                if let Some(ap) = *along_path {
                    if let Some(&mapped) = id_map.get(&ap) {
                        *along_path = Some(mapped);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Lightweight fuzzy subsequence scorer for Command Palette search.
fn fuzzy_score(haystack: &str, needle: &str) -> Option<i32> {
    if haystack.contains(needle) {
        let bonus = if haystack.starts_with(needle) {
            100
        } else {
            50
        };
        return Some(bonus - (haystack.len() as i32 - needle.len() as i32).min(40));
    }
    let mut score = 0i32;
    let mut h_chars = haystack.chars().peekable();
    let mut consecutive = 0i32;
    for nc in needle.chars() {
        let mut matched = false;
        for hc in h_chars.by_ref() {
            if hc == nc {
                matched = true;
                consecutive += 1;
                score += 5 + consecutive * 2;
                break;
            }
            consecutive = 0;
        }
        if !matched {
            return None;
        }
    }
    Some(score)
}
