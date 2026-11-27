// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

// =============================================================================
// 15 Toolbox Flyout Groups & Complete Tool Registry (re-exported from coree-tools)
// =============================================================================

/// Associated shortcut action for any of the 65 [`ToolId`] tools (zero unmapped tools).
#[must_use]
pub const fn shortcut_action(tool: ToolId) -> Option<ShortcutAction> {
    match tool {
        ToolId::Pick => Some(ShortcutAction::SelectPick),
        ToolId::FreehandPick => Some(ShortcutAction::SelectTool(ToolId::FreehandPick)),
        ToolId::FreeTransform => Some(ShortcutAction::SelectTool(ToolId::FreeTransform)),
        ToolId::ShapeNode => Some(ShortcutAction::SelectShapeNode),
        ToolId::Smooth => Some(ShortcutAction::SelectTool(ToolId::Smooth)),
        ToolId::Smear => Some(ShortcutAction::SelectTool(ToolId::Smear)),
        ToolId::Twirl => Some(ShortcutAction::SelectTool(ToolId::Twirl)),
        ToolId::AttractRepel => Some(ShortcutAction::SelectTool(ToolId::AttractRepel)),
        ToolId::Roughen => Some(ShortcutAction::SelectTool(ToolId::Roughen)),
        ToolId::Crop => Some(ShortcutAction::SelectTool(ToolId::Crop)),
        ToolId::Knife => Some(ShortcutAction::SelectTool(ToolId::Knife)),
        ToolId::VirtualSegmentDelete => {
            Some(ShortcutAction::SelectTool(ToolId::VirtualSegmentDelete))
        }
        ToolId::Eraser => Some(ShortcutAction::SelectTool(ToolId::Eraser)),
        ToolId::Zoom => Some(ShortcutAction::SelectZoom),
        ToolId::Pan => Some(ShortcutAction::SelectPan),
        ToolId::Freehand => Some(ShortcutAction::SelectFreehand),
        ToolId::TwoPointLine => Some(ShortcutAction::SelectTool(ToolId::TwoPointLine)),
        ToolId::Bezier => Some(ShortcutAction::SelectTool(ToolId::Bezier)),
        ToolId::Pen => Some(ShortcutAction::SelectPen),
        ToolId::BSpline => Some(ShortcutAction::SelectTool(ToolId::BSpline)),
        ToolId::Polyline => Some(ShortcutAction::SelectTool(ToolId::Polyline)),
        ToolId::ThreePointCurve => Some(ShortcutAction::SelectTool(ToolId::ThreePointCurve)),
        ToolId::SmartDrawing => Some(ShortcutAction::SelectTool(ToolId::SmartDrawing)),
        ToolId::LiveSketch => Some(ShortcutAction::SelectTool(ToolId::LiveSketch)),
        ToolId::ArtisticMedia => Some(ShortcutAction::SelectTool(ToolId::ArtisticMedia)),
        ToolId::Rectangle => Some(ShortcutAction::SelectRectangle),
        ToolId::ThreePointRectangle => {
            Some(ShortcutAction::SelectTool(ToolId::ThreePointRectangle))
        }
        ToolId::Ellipse => Some(ShortcutAction::SelectEllipse),
        ToolId::ThreePointEllipse => Some(ShortcutAction::SelectTool(ToolId::ThreePointEllipse)),
        ToolId::Polygon => Some(ShortcutAction::SelectPolygon),
        ToolId::Star => Some(ShortcutAction::SelectTool(ToolId::Star)),
        ToolId::ComplexStar => Some(ShortcutAction::SelectTool(ToolId::ComplexStar)),
        ToolId::Spiral => Some(ShortcutAction::SelectSpiral),
        ToolId::GraphPaper => Some(ShortcutAction::SelectGraphPaper),
        ToolId::Impact => Some(ShortcutAction::SelectTool(ToolId::Impact)),
        ToolId::BasicShapes => Some(ShortcutAction::SelectTool(ToolId::BasicShapes)),
        ToolId::ArtisticText => Some(ShortcutAction::SelectArtisticText),
        ToolId::ParagraphText => Some(ShortcutAction::SelectTool(ToolId::ParagraphText)),
        ToolId::Table => Some(ShortcutAction::SelectTool(ToolId::Table)),
        ToolId::ParallelDimension => Some(ShortcutAction::SelectTool(ToolId::ParallelDimension)),
        ToolId::HvDimension => Some(ShortcutAction::SelectTool(ToolId::HvDimension)),
        ToolId::AngularDimension => Some(ShortcutAction::SelectTool(ToolId::AngularDimension)),
        ToolId::SegmentDimension => Some(ShortcutAction::SelectTool(ToolId::SegmentDimension)),
        ToolId::ThreePointCallout => Some(ShortcutAction::SelectTool(ToolId::ThreePointCallout)),
        ToolId::StraightConnector => Some(ShortcutAction::SelectTool(ToolId::StraightConnector)),
        ToolId::RightAngleConnector => {
            Some(ShortcutAction::SelectTool(ToolId::RightAngleConnector))
        }
        ToolId::DropShadow => Some(ShortcutAction::SelectTool(ToolId::DropShadow)),
        ToolId::BlockShadow => Some(ShortcutAction::SelectTool(ToolId::BlockShadow)),
        ToolId::Contour => Some(ShortcutAction::SelectTool(ToolId::Contour)),
        ToolId::Blend => Some(ShortcutAction::SelectTool(ToolId::Blend)),
        ToolId::Distort => Some(ShortcutAction::SelectTool(ToolId::Distort)),
        ToolId::Envelope => Some(ShortcutAction::SelectTool(ToolId::Envelope)),
        ToolId::Extrude25D => Some(ShortcutAction::SelectTool(ToolId::Extrude25D)),
        ToolId::Transparency => Some(ShortcutAction::SelectTool(ToolId::Transparency)),
        ToolId::ColorEyedropper => Some(ShortcutAction::SelectEyedropper),
        ToolId::AttributesEyedropper => {
            Some(ShortcutAction::SelectTool(ToolId::AttributesEyedropper))
        }
        ToolId::InteractiveFill => Some(ShortcutAction::SelectInteractiveFill),
        ToolId::MeshFill => Some(ShortcutAction::SelectMeshFill),
        ToolId::SmartFill => Some(ShortcutAction::SelectTool(ToolId::SmartFill)),
        ToolId::LensTool => Some(ShortcutAction::SelectTool(ToolId::LensTool)),
        ToolId::BitmapBrush => Some(ShortcutAction::SelectTool(ToolId::BitmapBrush)),
        ToolId::BitmapEraser => Some(ShortcutAction::SelectTool(ToolId::BitmapEraser)),
        ToolId::CloneStamp => Some(ShortcutAction::SelectTool(ToolId::CloneStamp)),
        ToolId::Smudge => Some(ShortcutAction::SelectTool(ToolId::Smudge)),
        ToolId::MaskSelect => Some(ShortcutAction::SelectTool(ToolId::MaskSelect)),
    }
}

/// Associated shortcut action for a [`ToolId`] if one exists.
#[must_use]
pub const fn tool_shortcut_action(tool: ToolId) -> Option<ShortcutAction> {
    shortcut_action(tool)
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

    /// Localized short tab title displayed in `egui_dock` (respecting English / Arabic UAX #9 BiDi).
    #[must_use]
    pub fn title_for(self, lang: crate::theme::StudioLanguage) -> String {
        crate::i18n::docker_i18n_key(self).tr_ui(lang)
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
// Workspace Presets (RasterBrushOp & NodeSmoothness re-exported from coree-tools)
// =============================================================================

/// Switchable clean-room studio workspace presets (exportable/importable as `.coreews` JSON).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum WorkspacePreset {
    #[default]
    DefaultClassic,
    Lite,
    Illustration,
    Illustrator,
    PageLayout,
    LaserSignStudio,
    TouchCompact,
}

impl WorkspacePreset {
    /// All 7 built-in workspace presets.
    pub const ALL: [Self; 7] = [
        Self::DefaultClassic,
        Self::Lite,
        Self::Illustration,
        Self::Illustrator,
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
            Self::Illustrator => "Illustrator",
            Self::PageLayout => "Page Layout",
            Self::LaserSignStudio => "Laser & Sign Studio",
            Self::TouchCompact => "Touch Compact",
        }
    }

    /// Localized workspace name respecting English / Arabic UAX #9 BiDi.
    #[must_use]
    pub fn label_for(self, lang: crate::theme::StudioLanguage) -> String {
        crate::i18n::workspace_i18n_key(self).tr_ui(lang)
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
            Self::Illustrator => {
                "Universal vector workflow (`V`/`P`/`R`/`O`/`T`) with Properties, Symbols & Effects."
            }
            Self::PageLayout => "Multi-page publishing with Fonts, Find & Replace & Production.",
            Self::LaserSignStudio => {
                "Cut/engrave workflow with Production, Transform & CDR Inspector."
            }
            Self::TouchCompact => "Two-column toolbox and compact dockers for smaller displays.",
        }
    }
}

/// Customizable visibility & group ordering for the top Standard Toolbar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolbarConfig {
    #[serde(default = "default_true_persist")]
    pub show_file_ops: bool,
    #[serde(default = "default_true_persist")]
    pub show_undo_redo: bool,
    #[serde(default = "default_true_persist")]
    pub show_clipboard: bool,
    #[serde(default = "default_true_persist")]
    pub show_zoom: bool,
    #[serde(default = "default_true_persist")]
    pub show_view_mode: bool,
    #[serde(default = "default_true_persist")]
    pub show_snap_toggles: bool,
    #[serde(default = "default_true_persist")]
    pub show_layout_toggles: bool,
    #[serde(default = "default_true_persist")]
    pub show_drawing_scale: bool,
    #[serde(default = "default_toolbar_group_order")]
    pub group_order: Vec<usize>,
}

fn default_toolbar_group_order() -> Vec<usize> {
    vec![0, 1, 2, 3, 4, 5, 6, 7]
}

impl Default for ToolbarConfig {
    fn default() -> Self {
        Self {
            show_file_ops: true,
            show_undo_redo: true,
            show_clipboard: true,
            show_zoom: true,
            show_view_mode: true,
            show_snap_toggles: true,
            show_layout_toggles: true,
            show_drawing_scale: true,
            group_order: default_toolbar_group_order(),
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
    #[serde(default)]
    pub(crate) toolbar_config: ToolbarConfig,
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
    #[serde(default)]
    pub(crate) toolbar_config: ToolbarConfig,
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

// Note: `ParametricHandleKind`, `ScaleHandle`, `NodeDragPart`, and `ActiveDragState`
// live in `coree_tools` (`crate::state` re-exports `CanvasDragState = ActiveDragState` if needed).

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
    pub sculpt_sub_mode: coree_tools::SculptSubMode,
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
    pub mask_mode: RasterMaskMode,
    pub mask_feather_px: f32,
    pub magic_wand_tolerance: f32,
    pub magic_wand_contiguous: bool,
    pub mask_inverted: bool,
    pub eyedropper_apply_mode: bool,
    pub sampled_effects: Vec<Effect>,
    pub shape_reduce_tolerance_mm: f64,
    pub text_insert_draft: String,
    pub dimension_offset_mm: f64,
    pub symmetry_enabled: bool,
    pub symmetry_folds: u32,
    pub symmetry_mirror: bool,
    pub draw_in_perspective: bool,
    pub pressure_curve_json: String,
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
            sculpt_sub_mode: coree_tools::SculptSubMode::Attract,
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
            mask_mode: RasterMaskMode::Rectangle,
            mask_feather_px: 0.0,
            magic_wand_tolerance: 24.0,
            magic_wand_contiguous: true,
            mask_inverted: false,
            eyedropper_apply_mode: false,
            sampled_effects: Vec::new(),
            shape_reduce_tolerance_mm: 0.5,
            text_insert_draft: "coree Studio".to_owned(),
            dimension_offset_mm: 8.0,
            symmetry_enabled: false,
            symmetry_folds: 4,
            symmetry_mirror: true,
            draw_in_perspective: false,
            pressure_curve_json: "[[0.0,0.0],[1.0,1.0]]".to_owned(),
        }
    }
}

impl ToolSettings {
    /// Captures an immutable [`coree_tools::ToolSettingsSnapshot`] for `ToolCore`.
    #[must_use]
    pub fn to_snapshot(
        &self,
        props: &PropertiesDockerState,
        fx: &EffectsDockerState,
    ) -> coree_tools::ToolSettingsSnapshot {
        coree_tools::ToolSettingsSnapshot {
            rect_corner_radius_mm: self.rect_corner_radius_mm,
            rect_corner_style: self.rect_corner_style,
            ellipse_mode: self.ellipse_mode,
            polygon_sides: self.polygon_sides,
            star_points: self.star_points,
            star_inner_ratio: self.star_inner_ratio,
            spiral_revolutions: self.spiral_revolutions,
            spiral_logarithmic: self.spiral_logarithmic,
            graph_cols: self.graph_cols,
            graph_rows: self.graph_rows,
            impact_rays: self.impact_rays,
            impact_style: self.impact_style,
            basic_shape_kind: self.basic_shape_kind,
            table_rows: self.table_rows,
            table_cols: self.table_cols,
            artistic_media_width_mm: self.artistic_media_width_mm,
            artistic_media_profile: self.artistic_media_profile.clone(),
            sculpt_radius_mm: self.sculpt_radius_mm,
            sculpt_strength: self.sculpt_strength,
            sculpt_sub_mode: self.sculpt_sub_mode,
            eraser_radius_mm: self.eraser_radius_mm,
            brush_radius_px: self.brush_radius_px,
            raster_op: self.raster_op,
            clone_source_mm: self.clone_source_mm,
            mask_mode: self.mask_mode,
            mask_feather_px: self.mask_feather_px,
            magic_wand_tolerance: self.magic_wand_tolerance,
            magic_wand_contiguous: self.magic_wand_contiguous,
            mask_inverted: self.mask_inverted,
            eyedropper_apply_mode: self.eyedropper_apply_mode,
            sampled_effects: self.sampled_effects.clone(),
            text_insert_draft: self.text_insert_draft.clone(),
            dimension_offset_mm: self.dimension_offset_mm,
            grad_start_color: props.grad_start_color.clone(),
            grad_end_color: props.grad_end_color.clone(),
            mesh_rows: props.mesh_rows,
            mesh_cols: props.mesh_cols,
            effect_dx_mm: fx.dx_mm,
            effect_dy_mm: fx.dy_mm,
            effect_blur_mm: fx.blur_mm,
            effect_opacity: fx.opacity,
            effect_color: fx.color.clone(),
            effect_contour_mode: fx.contour_mode,
            effect_steps: fx.steps,
            effect_distort_kind: fx.distort_kind,
            effect_frequency: fx.frequency,
            effect_envelope_mode: fx.envelope_mode,
            effect_blend_mode: fx.blend_mode,
            symmetry_enabled: self.symmetry_enabled,
            symmetry_folds: self.symmetry_folds.max(2),
            symmetry_mirror: self.symmetry_mirror,
            symmetry_center_mm: None,
            draw_in_perspective: self.draw_in_perspective,
            pressure_curve_json: self.pressure_curve_json.clone(),
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
    pub new_tab_stop_mm: f64,
    pub new_tab_stop_align: coree_core::ParagraphTabAlign,
    pub new_fvar_tag: String,
    pub new_fvar_val: f32,
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
            new_tab_stop_mm: 20.0,
            new_tab_stop_align: coree_core::ParagraphTabAlign::Left,
            new_fvar_tag: "wght".to_owned(),
            new_fvar_val: 500.0,
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
            frozen_snapshot: None,
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
    pub inner_shadow_feather: FeatherDirection,
    pub bevel_style: coree_core::BevelStyle,
    pub bevel_distance_mm: f64,
    pub bevel_softness_mm: f64,
    pub bevel_light_angle_deg: f64,
    pub bevel_altitude_deg: f64,
    pub bevel_highlight_color: Color,
    pub bevel_shadow_color: Color,
    pub bevel_intensity: f32,
    pub extrude3d_rotation: [f64; 3],
    pub extrude3d_light_dir: [f32; 3],
    pub extrude3d_light_intensity: f32,
    pub rotate_along_path: bool,
    pub block_shadow_simplify: bool,
    pub transp_kind: usize,
    pub copy_envelope_source: Option<ObjectId>,
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
            inner_shadow_feather: FeatherDirection::Inside,
            bevel_style: coree_core::BevelStyle::SoftEdge,
            bevel_distance_mm: 3.0,
            bevel_softness_mm: 1.0,
            bevel_light_angle_deg: 135.0,
            bevel_altitude_deg: 45.0,
            bevel_highlight_color: Color::WHITE,
            bevel_shadow_color: Color::BLACK,
            bevel_intensity: 0.85,
            extrude3d_rotation: [12.0, -18.0, 0.0],
            extrude3d_light_dir: [0.45, -0.6, 0.65],
            extrude3d_light_intensity: 0.9,
            rotate_along_path: true,
            block_shadow_simplify: true,
            transp_kind: 0,
            copy_envelope_source: None,
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

/// Cached high-res RGBA buffers for `SplitPreview`:
/// `(config_hash, contour_count, preview_w, preview_h, before_rgba, after_rgba)`.
pub type SplitPreviewCache = std::cell::RefCell<Option<(u64, usize, u32, u32, Vec<u8>, Vec<u8>)>>;

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
    pub speckle_size: usize,
    pub keep_original: bool,
    pub place_side_by_side: bool,
    pub split_preview: bool,
    pub split_divider: f32,
    pub split_preview_cache: SplitPreviewCache,
}

impl Default for TraceDockerState {
    fn default() -> Self {
        Self {
            mode_idx: 1,
            preset: OutlinePreset::Logo,
            max_colors: 12,
            smoothing: 0.50,
            detail: 0.82,
            remove_background: true,
            corner_threshold: 60.0,
            speckle_size: 2,
            keep_original: true,
            place_side_by_side: false,
            split_preview: true,
            split_divider: 0.5,
            split_preview_cache: std::cell::RefCell::new(None),
        }
    }
}

impl TraceDockerState {
    /// Populates sliders with tuned defaults when the user switches `OutlinePreset` in the UI.
    pub fn apply_preset_defaults(&mut self) {
        let (colors, detail, smooth, corners, speckle) = match self.preset {
            OutlinePreset::LineArt => (2, 0.90, 0.35, 65.0, 2),
            OutlinePreset::Logo => (12, 0.82, 0.50, 60.0, 2),
            OutlinePreset::DetailedLogo => (24, 0.92, 0.42, 55.0, 1),
            OutlinePreset::Clipart => (16, 0.80, 0.55, 60.0, 3),
            OutlinePreset::LowQualityImage => (12, 0.60, 0.72, 70.0, 6),
            OutlinePreset::HighQualityImage => (48, 0.92, 0.48, 55.0, 1),
        };
        self.max_colors = colors;
        self.detail = detail;
        self.smoothing = smooth;
        self.corner_threshold = corners;
        self.speckle_size = speckle;
    }

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
            speckle_size: self.speckle_size.min(4096),
        }
    }
}

/// Bitmap Studio Docker state (`S10`) covering all 20 non-destructive `RasterFilter` variants,
/// CoW 8/16-bit depth conversion, resampling kernel selection, and live histogram analysis.
#[derive(Debug, Clone)]
pub struct BitmapDockerState {
    pub filter_idx: usize,
    pub param_a: f32,
    pub param_b: f32,
    pub param_c: f32,
    pub tone_curve_points: Vec<(f32, f32)>,
    pub selected_curve_point: Option<usize>,
    pub mixer_red: [f32; 3],
    pub mixer_green: [f32; 3],
    pub mixer_blue: [f32; 3],
    pub mixer_mono: bool,
    pub selected_filter_stack_idx: Option<usize>,
    pub resample_w: u32,
    pub resample_h: u32,
    pub resample_algo: coree_render::ResampleAlgorithm,
    pub target_depth: coree_core::PixelDepth,
    pub target_color_mode: ColorMode,
    pub convert_dpi: f64,
    pub cutout_bg_threshold: f32,
    pub cutout_feather_px: f32,
    pub histogram_cache: std::cell::RefCell<Option<(u64, coree_render::RasterHistogram)>>,
}

impl Default for BitmapDockerState {
    fn default() -> Self {
        Self {
            filter_idx: 0,
            param_a: 0.15,
            param_b: 0.15,
            param_c: 1.0,
            tone_curve_points: vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)],
            selected_curve_point: None,
            mixer_red: [1.0, 0.0, 0.0],
            mixer_green: [0.0, 1.0, 0.0],
            mixer_blue: [0.0, 0.0, 1.0],
            mixer_mono: false,
            selected_filter_stack_idx: None,
            resample_w: 512,
            resample_h: 512,
            resample_algo: coree_render::ResampleAlgorithm::Bicubic,
            target_depth: coree_core::PixelDepth::Bit16,
            target_color_mode: ColorMode::Cmyk,
            convert_dpi: 300.0,
            cutout_bg_threshold: 24.0,
            cutout_feather_px: 1.5,
            histogram_cache: std::cell::RefCell::new(None),
        }
    }
}

impl BitmapDockerState {
    /// All 20 non-destructive raster filters.
    pub const FILTER_NAMES: [&'static str; 20] = [
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
        "Channel Mixer",
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
                points: if self.tone_curve_points.len() >= 2 {
                    self.tone_curve_points.clone()
                } else {
                    vec![
                        (0.0, 0.0),
                        (0.5, (0.5 + self.param_a * 0.4).clamp(0.05, 0.95)),
                        (1.0, 1.0),
                    ]
                },
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
            18 => RasterFilter::EdgeDetect {
                sensitivity: self.param_c.clamp(0.2, 4.0),
            },
            _ => RasterFilter::ChannelMixer {
                red: self.mixer_red,
                green: self.mixer_green,
                blue: self.mixer_blue,
                monochrome: self.mixer_mono,
            },
        }
    }
}

/// Severity filter tabs for Preflight Audit (`S13`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreflightSeverityFilter {
    #[default]
    All,
    Errors,
    Warnings,
    Info,
}

/// Production & Prepress Docker state (`S12`, `S13`, `S14`).
#[derive(Debug, Clone)]
pub struct ProductionDockerState {
    pub sub_section: usize, // 0=Preflight, 1=Separations/Imposition, 2=Cut/Dielines, 3=Barcode/QR, 4=Merge/Collect
    pub preflight_min_dpi: f64,
    pub preflight_severity: PreflightSeverityFilter,
    pub preflight_rules: PreflightRules,
    pub cut_offset_mm: f64,
    pub cut_spot_name: String,
    pub dieline_type: usize, // 0=TuckEndBox, 1=Envelope, 2=ShoppingBag, 3=StickerSheet, 4=Fefco
    pub fefco_code: FefcoCode,
    pub flute_type: FluteType,
    pub box_l_mm: f64,
    pub box_w_mm: f64,
    pub box_h_mm: f64,
    pub sticker_cols: u32,
    pub sticker_rows: u32,
    pub barcode_symbology: BarcodeSymbology,
    pub barcode_payload: String,
    pub barcode_bwr_mm: f64,
    pub code128_mode: Code128Mode,
    pub qr_payload: String,
    pub qr_size_mm: f64,
    pub qr_ec: QrErrorCorrection,
    pub imposition_kind: usize, // 0=NUp, 1=Booklet, 2=Tiling, 3=PerfectBound
    pub imposition_signature_pages: u32,
    pub spine_thickness_mm: f64,
    pub creep_mm: f64,
    pub milling_allowance_mm: f64,
    pub trap_width_mm: f64,
    pub trap_placement: TrapPlacement,
    pub halftone_lpi: f64,
    pub halftone_shape: HalftoneDotShape,
    pub laser_protocol: OpenLaserProtocol,
    pub nup_cols: u32,
    pub nup_rows: u32,
    pub sheet_w_mm: f64,
    pub sheet_h_mm: f64,
    pub print_scale_pct: f64,
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
            preflight_severity: PreflightSeverityFilter::All,
            preflight_rules: PreflightRules::default(),
            cut_offset_mm: 2.0,
            cut_spot_name: "CutContour".to_owned(),
            dieline_type: 0,
            fefco_code: FefcoCode::Fefco0201,
            flute_type: FluteType::B,
            box_l_mm: 80.0,
            box_w_mm: 40.0,
            box_h_mm: 100.0,
            sticker_cols: 3,
            sticker_rows: 4,
            barcode_symbology: BarcodeSymbology::Ean13,
            barcode_payload: "590123412345".to_owned(),
            barcode_bwr_mm: 0.0,
            code128_mode: Code128Mode::Auto,
            qr_payload: coree_core::brand::REPO_URL.to_owned(),
            qr_size_mm: 28.0,
            qr_ec: QrErrorCorrection::M,
            imposition_kind: 0,
            imposition_signature_pages: 4,
            spine_thickness_mm: 4.0,
            creep_mm: 0.5,
            milling_allowance_mm: 1.0,
            trap_width_mm: 0.15,
            trap_placement: TrapPlacement::Centerline,
            halftone_lpi: 133.0,
            halftone_shape: HalftoneDotShape::Round,
            laser_protocol: OpenLaserProtocol::GrblStream,
            nup_cols: 2,
            nup_rows: 2,
            sheet_w_mm: 320.0,
            sheet_h_mm: 450.0,
            print_scale_pct: 100.0,
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
    pub use_fill_color: bool,
    pub find_fill_color: Color,
    pub use_outline_color: bool,
    pub find_outline_color: Color,
    pub use_outline_width: bool,
    pub outline_width_min_mm: f64,
    pub outline_width_max_mm: f64,
    pub filter_hairline: Option<bool>,
    pub font_query: String,
    pub replace_font: String,
    pub replace_fill_color: Color,
    pub replace_outline_color: Color,
    pub replace_outline_width_mm: f64,
    pub layer_filter: String,
    pub cql_query: String,
    pub last_cql_error: Option<String>,
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
            use_fill_color: false,
            find_fill_color: Color::BLACK,
            use_outline_color: false,
            find_outline_color: Color::BLACK,
            use_outline_width: false,
            outline_width_min_mm: 0.0,
            outline_width_max_mm: 5.0,
            filter_hairline: None,
            font_query: String::new(),
            replace_font: "Noto Sans".to_owned(),
            replace_fill_color: Color::CYAN,
            replace_outline_color: Color::MAGENTA,
            replace_outline_width_mm: 0.5,
            layer_filter: String::new(),
            cql_query: String::new(),
            last_cql_error: None,
            last_report: None,
        }
    }
}

/// One missing font entry in the `CRLFOM` modal substitution dialog (`REQ-07`).
#[derive(Debug, Clone)]
pub struct MissingFontPromptEntry {
    pub missing_family: String,
    pub suggested_family: String,
    pub selected_family: String,
    pub panose_distance: Option<u32>,
}

/// Interactive `CRLFOM` missing-font `PANOSE` substitution dialog state.
#[derive(Debug, Clone)]
pub struct MissingFontPromptState {
    pub open: bool,
    pub document_title: String,
    pub entries: Vec<MissingFontPromptEntry>,
    pub remember_rules: bool,
}

impl Default for MissingFontPromptState {
    fn default() -> Self {
        Self {
            open: false,
            document_title: String::new(),
            entries: Vec::new(),
            remember_rules: true,
        }
    }
}

/// Interactive `S15` `.cdr` legacy codepage import dialog state (`CP1256` / `CP1251` / `UTF-8`).
#[derive(Debug, Clone, Default)]
pub struct CdrImportPromptState {
    pub open: bool,
    pub pending_path: Option<PathBuf>,
    pub selected_codepage: Option<u16>,
    pub preview_summary: String,
}

/// Font Manager Docker state (`S07`).
#[derive(Debug, Clone, Default)]
pub struct FontManagerState {
    pub search_query: String,
    pub arabic_only: bool,
    pub kashida_only: bool,
    pub favorites_only: bool,
    pub indexed_fonts: Vec<FontRecord>,
    pub quarantined_files: Vec<String>,
    pub scanned_once: bool,
    pub missing_family_input: String,
    pub substitute_family_input: String,
    pub panose_suggestion: Option<(FontRecord, u32)>,
    pub substitutions: Vec<FontSubstitutionRecord>,
    pub active_collection_tag: Option<String>,
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
