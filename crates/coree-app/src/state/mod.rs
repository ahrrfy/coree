// SPDX-License-Identifier: MIT OR Apache-2.0

//! Complete headless-drivable studio application state, multi-document tab manager,
//! all 15 Toolbox Flyout groups, rearrangeable `egui_dock` layout state, SQLite
//! autosave/WAL crash recovery, `JobPool` background jobs, and studio actions.

use crate::cdr_bridge::{inspect_any_file, inspect_in_memory_document, FileInspectionReport};
use crate::io_bridge::{
    build_import_document_ops_on_layer, export_any_document, open_any_document,
    open_any_document_with_codepage, resolve_import_target_layer, UniversalExportOptions,
};
use crate::printing::{
    list_system_printers, print_document_via_os_spooler, PrintJobConfig, PrinterInfo,
};
use crate::shortcuts::{
    shortcut_label, shortcut_label_for, CustomShortcutMap, ShortcutAction, ShortcutProfile,
};
use crate::theme::{StudioLanguage, StudioTheme};
use coree_cdr::generate_sample_document;
use coree_core::{
    Affine2D, AlignOp, AnchorPoint, ArtisticMediaStyle, ArtisticText, ArtisticWidthProfile,
    BasicShapeKind, BezierPath, BlendMode, BooleanOp, BuiltinIccProfile, Color, ColorMapMode,
    ColorMode, ColorPalette, ColorSwatch, CommandHistory, CommandOp, ConnectorObject,
    ConnectorStyle, ContourMode, CornerStyle, DimensionKind, DistortKind, DistributeOp, Document,
    DocumentInvalidation, Effect, EllipseMode, EnvelopeMode, FeatherDirection, Fill,
    GuideOrientation, ImageAssetId, ImpactStyle, LayerId, LensConfig, LensKind, MasterLayerScope,
    NodeKind, NodePayload, NodeRef, ObjectId, OutlineStyle, PageId, ParametricShape, PathNode,
    Point2D, PressureCurve, RasterAsset, RasterFilter, RasterLayerData, RasterMaskMode,
    RasterMaskSelection, RasterMaskShape, Rect2D, RenderingIntent, SceneObject, SelectionState,
    SnapEngine, StylusSample, Subpath, SymbolId, TextOnPath, TextStyle, TransparencyMode, Unit,
    Vec2D, VectorTable, ZOrderOp,
};
use coree_io::import_palette_file;
use coree_prod::{
    add_barcode_to_document, add_qr_code_to_document, apply_print_merge, apply_trapping,
    builtin_macros, collect_for_output, compute_production_metrics,
    compute_separations_with_text_outliner, detect_executable, evaluate_cql_query,
    export_for_plugin, export_separated_plates_pdf, find_objects, frame_open_laser_protocol,
    generate_cut_contour, generate_dieline, generate_halftone_preview, generate_print_marks,
    impose_document, launch_external_bridge, plan_laser_toolpath, plan_macro_ops_with_selection,
    plan_recorded_macro_ops, run_macro_event_hook_with_selection, run_preflight, BarcodeOptions,
    BarcodeSymbology, Code128Mode, CollectOptions, DielineKind, FefcoCode, FindQuery,
    FindReplaceReport, FindScope, FluteType, HalftoneDotShape, HalftoneSettings, ImpositionMode,
    LaserJobConfig, LaserToolpathPlan, LensKindFilter, MacroEventHook, MacroSandboxConfig,
    ObjectKindFilter, OpenLaserProtocol, PluginExportOutcome, PluginKind, PluginRegistry,
    PreflightReport, PreflightRules, PrintMarksOptions, PrintMergeOptions, PrintMergeTable,
    ProductionMetrics, QrCodeOptions, QrErrorCorrection, ReplaceAction, SeparationOptions,
    SeparationResult, TextMatchMode, TrapPlacement, TrappingSettings,
};
use coree_render::{
    apply_dodge_burn_stroke_stylus_masked, apply_healing_stroke_stylus_masked,
    clone_stamp_stroke_stylus_masked, compute_page_layouts, convert_object_to_bitmap_with_bounds,
    create_smart_fill_object, cutout_lab, erase_stroke_stylus_masked, hit_test_point,
    lower_effects_to_primitives, paint_brush_stroke_stylus, raster_flood_fill_masked,
    smudge_stroke_stylus_masked, trace_raster_to_mm, DirtyTileCache, DodgeBurnMode,
    DynamicGuideLine, GlyphBlockFilter, OutlinePreset, RasterMask, RenderOptions, SceneRenderer,
    SnapOptions, StylusPoint, TextEngine, ToneRange, TraceConfig, TraceMode, ViewMode, Viewport,
};
use coree_storage::{
    DifferentialSnapshotPatch, FontQuery, FontRecord, FontSubstitutionRecord, JobHandle,
    JobOutcome, JobPool, PanoseClassification, RecentDocumentRecord, StorageDb, TemplateRecord,
    UncleanSession, WorkspaceLayoutRecord,
};
pub use coree_tools::{
    ActiveDragState, CursorHint, HandleShape, KeyEvent, ModifiersState, NodeDragPart,
    NodeSmoothness, OverlayHandle, OverlayPath, OverlayStrokeKind, ParametricHandleKind,
    PointerButton, PointerEvent, RasterBrushOp, ScaleHandle, ToolActionRequest, ToolCore, ToolId,
    ToolKey, ToolOutput, ToolReadContext, ToolSettingsSnapshot, TransientOverlay, TOOL_GROUPS,
};
use egui_dock::{DockState, NodeIndex, NodePath, SurfaceIndex};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

mod canvas_tools;
mod editing;
pub(crate) mod helpers;
mod production;
mod tab;
mod types;

pub(crate) use helpers::*;
pub use tab::*;
pub use types::*;

/// Deep-cloned subtree payload stored alongside `StudioApp::clipboard_objects`
/// so Cut/Copy + Paste preserves `Group` & `PowerClip` descendants, cross-object
/// references, and raster assets cleanly without ID aliasing or data loss.
#[derive(Debug, Clone)]
pub struct ClipboardSnapshot {
    pub source_doc_id: Uuid,
    pub roots: Vec<ObjectId>,
    pub document: Document,
    pub text_on_path_fallbacks: HashMap<ObjectId, Affine2D>,
}

/// Complete desktop studio application state (`eframe::App` + headless test API).
pub struct StudioApp {
    pub tabs: Vec<DocumentTab>,
    pub active_tab: usize,
    pub show_welcome: bool,
    pub theme: StudioTheme,
    pub ui_language: StudioLanguage,
    pub theme_dirty: bool,
    pub fonts_configured: bool,
    pub shortcut_profile: ShortcutProfile,
    pub custom_shortcuts: CustomShortcutMap,
    pub shortcut_editor_idx: usize,
    pub shortcut_editor_combo: String,

    // Native OS Print State
    pub print_job_config: PrintJobConfig,
    pub available_printers: Vec<PrinterInfo>,
    pub printers_status: Option<String>,
    pub(crate) printer_scan_rx: Option<std::sync::mpsc::Receiver<Result<Vec<PrinterInfo>, String>>>,
    pub scanning_printers: bool,

    // Toolbox & Dockers
    pub active_tool: ToolId,
    pub previous_non_pick_tool: ToolId,
    pub one_shot_zoom_return_tool: Option<ToolId>,
    pub group_active_tools: [ToolId; 15],
    pub open_flyout_group: Option<usize>,
    pub dock_state: DockState<DockerTab>,
    pub show_dockers: bool,
    pub toolbox_two_column: bool,
    pub active_workspace_preset: WorkspacePreset,
    pub toolbar_config: ToolbarConfig,
    pub templates: Vec<TemplateRecord>,
    pub template_category_filter: Option<String>,
    pub objects_filter: String,
    pub selected_cdr_chunk_idx: Option<usize>,
    pub show_navigator_popup: bool,
    pub swatch_hold_state: Option<(Instant, Color, egui::Pos2)>,
    pub tint_grid_popup: Option<(Color, egui::Pos2)>,
    pub print_preview_zoom: f32,
    pub print_preview_pan: [f32; 2],
    pub print_preview_plate: Option<usize>,
    pub print_preview_tex: std::cell::RefCell<Option<(u64, egui::TextureHandle)>>,

    // Studio Viewport & Options State
    pub show_options_dialog: bool,
    pub options_tab: usize,
    pub full_screen_preview: bool,
    pub treat_all_objects_as_filled: bool,
    pub object_origin_anchor: (u8, u8),

    // ICC Soft-Proofing, Gamut Warning & OS Display ICC Calibration
    pub soft_proof_cmyk: bool,
    pub gamut_warning: bool,
    pub proof_profile: BuiltinIccProfile,
    pub proof_intent: RenderingIntent,
    pub use_display_icc: bool,
    pub display_icc_info: Option<crate::display_icc::DisplayIccInfo>,
    pub display_icc_intent: RenderingIntent,

    // Hardware Stylus & Display ICC Providers
    pub(crate) stylus_provider: Box<dyn crate::stylus::StylusProvider>,
    pub last_stylus_sample: StylusSample,
    pub(crate) display_icc_provider: Box<dyn crate::display_icc::DisplayIccProvider>,

    // Laser / Engraving / Plotter & Plugin Bridge
    pub laser_config: LaserJobConfig,
    pub plugin_registry: PluginRegistry,
    pub plugin_exec_overrides: BTreeMap<String, String>,
    pub laser_plan_preview: Option<LaserToolpathPlan>,
    pub show_laser_toolpath_overlay: bool,
    pub last_plugin_export_path: Option<PathBuf>,

    // Glyphs Docker State
    pub glyph_block_filter: GlyphBlockFilter,
    pub glyph_search: String,

    // Repeat & Clipboard State
    pub last_executed_op: Option<CommandOp>,
    pub(crate) last_world_transform: Option<Affine2D>,
    pub clipboard_objects: Vec<SceneObject>,
    pub clipboard_snapshot: Option<ClipboardSnapshot>,
    pub(crate) clipboard_backend: Box<dyn crate::clipboard::ClipboardBackend>,
    pub(crate) last_clipboard_hash: Option<u64>,

    // Offline local storage & async job pool
    pub storage: Option<StorageDb>,
    pub job_pool: Option<JobPool>,
    pub(crate) ipc_server: Option<crate::ipc::IpcServer>,
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
    pub macro_hooks: Vec<(MacroEventHook, String)>,
    pub macro_hook_in_flight: bool,
    pub new_doc_dialog: NewDocDialogState,
    pub export_dialog: ExportDialogState,
    pub command_palette: CommandPaletteState,
    pub show_about_dialog: bool,
    pub show_preferences_dialog: bool,
    pub show_print_preview_dialog: bool,
    pub show_edit_fill_dialog: bool,
    pub edit_fill_draft: Fill,
    pub edit_fill_selected_stop: usize,
    pub show_outline_pen_dialog: bool,
    pub outline_pen_draft: OutlineStyle,
    pub missing_font_dialog: MissingFontPromptState,
    pub cdr_import_dialog: CdrImportPromptState,

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
        Self::from_storage(
            storage,
            Vec::new(),
            false,
            Box::new(crate::clipboard::MemoryClipboardBackend::new()),
            Box::new(crate::stylus::NullStylusProvider),
            Box::new(crate::display_icc::NullDisplayIccProvider),
        )
    }

    /// Creates a `StudioApp` attached to an explicit [`StorageDb`] instance.
    #[must_use]
    pub fn new_with_storage(storage: StorageDb) -> Self {
        Self::from_storage(
            Some(storage),
            Vec::new(),
            false,
            Box::new(crate::clipboard::MemoryClipboardBackend::new()),
            Box::new(crate::stylus::NullStylusProvider),
            Box::new(crate::display_icc::NullDisplayIccProvider),
        )
    }

    /// Creates the desktop `StudioApp` opening local SQLite persistence, scanning for
    /// crash-recovery sessions, loading preferences, and opening any CLI-supplied files.
    #[must_use]
    pub fn new_desktop(initial_files: Vec<PathBuf>) -> Self {
        let (storage, storage_err) = match StorageDb::open_default() {
            Ok(db) => (Some(db), None),
            Err(err) => (None, Some(err.to_string())),
        };
        let quarantined_backup = storage
            .as_ref()
            .and_then(|db| db.last_quarantined_backup().map(Path::to_path_buf));
        let mut app = Self::from_storage(
            storage,
            initial_files,
            true,
            Box::new(crate::clipboard::OsClipboardBackend::new()),
            crate::stylus::platform_stylus_provider(),
            Box::new(crate::display_icc::OsDisplayIccProvider),
        );
        if let Some(bak) = quarantined_backup {
            let msg = format!(
                "Recovered from corrupt local database (quarantined to {})",
                bak.display()
            );
            app.status_message = msg.clone();
            app.push_toast(msg, true);
        }
        if let Some(err) = storage_err {
            let msg = format!("Local database/autosave unavailable (running in-memory): {err}");
            app.status_message = msg.clone();
            app.push_toast(msg, true);
        }
        app
    }

    /// Replaces the active clipboard backend (useful for shared cross-instance tests).
    pub fn set_clipboard_backend(&mut self, backend: Box<dyn crate::clipboard::ClipboardBackend>) {
        self.clipboard_backend = backend;
    }

    /// Replaces the active stylus provider (useful for hardware/mock stylus tests).
    pub fn set_stylus_provider(&mut self, provider: Box<dyn crate::stylus::StylusProvider>) {
        self.stylus_provider = provider;
    }

    /// Replaces the active display ICC provider and refreshes the cached display profile.
    pub fn set_display_icc_provider(
        &mut self,
        provider: Box<dyn crate::display_icc::DisplayIccProvider>,
    ) {
        self.display_icc_provider = provider;
        self.detect_os_display_icc();
    }

    /// Queries [`DisplayIccProvider::load_display_icc`] and activates it if valid.
    pub fn detect_os_display_icc(&mut self) -> bool {
        if let Some(info) = self.display_icc_provider.load_display_icc() {
            self.display_icc_info = Some(info);
            self.use_display_icc = true;
            if let Some(tab) = self.active_tab_mut() {
                tab.needs_rerender = true;
            }
            true
        } else {
            false
        }
    }

    /// Returns `true` if either the internal studio clipboard or the OS clipboard holds supported content.
    #[must_use]
    pub fn has_clipboard_content(&self) -> bool {
        !self.clipboard_objects.is_empty()
            || self.clipboard_snapshot.is_some()
            || self.clipboard_backend.has_supported_content()
    }

    fn from_storage(
        storage: Option<StorageDb>,
        initial_files: Vec<PathBuf>,
        open_welcome_when_empty: bool,
        clipboard_backend: Box<dyn crate::clipboard::ClipboardBackend>,
        stylus_provider: Box<dyn crate::stylus::StylusProvider>,
        display_icc_provider: Box<dyn crate::display_icc::DisplayIccProvider>,
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
        let saved_language = storage
            .as_ref()
            .and_then(|db| {
                db.get_preference::<StudioLanguage>("ui.language")
                    .ok()
                    .flatten()
            })
            .unwrap_or_default();
        let saved_profile = storage
            .as_ref()
            .and_then(|db| {
                db.get_preference::<ShortcutProfile>("ui.shortcut_profile")
                    .ok()
                    .flatten()
            })
            .unwrap_or_default();
        let active_layout = storage
            .as_ref()
            .and_then(|db| db.get_active_workspace_layout().ok().flatten());
        let saved_dock = active_layout
            .as_ref()
            .and_then(|rec| serde_json::from_str::<DockState<DockerTab>>(&rec.dock_tree_json).ok())
            .unwrap_or_else(default_dock_state);
        let saved_toolbox = active_layout.as_ref().and_then(|rec| {
            serde_json::from_str::<ToolboxPersistConfig>(&rec.toolbox_config_json).ok()
        });
        let saved_custom_shortcuts = active_layout
            .as_ref()
            .and_then(|rec| serde_json::from_str::<CustomShortcutMap>(&rec.shortcuts_json).ok())
            .unwrap_or_default();

        if let Some(ref tb) = saved_toolbox {
            if let Some(tools) = tb.group_active_tools {
                group_active_tools = tools;
            }
        }

        let recent_docs = storage
            .as_ref()
            .and_then(|db| db.list_recent_documents(20).ok())
            .unwrap_or_default();
        let unclean_sessions = storage
            .as_ref()
            .and_then(|db| db.list_unclean_sessions().ok())
            .unwrap_or_default();
        let mut templates = storage
            .as_ref()
            .and_then(|db| db.list_templates(None).ok())
            .unwrap_or_default();
        if templates.is_empty() {
            let defaults = builtin_default_templates();
            if let Some(ref db) = storage {
                for tpl in &defaults {
                    let _ = db.upsert_template(tpl);
                }
                templates = db.list_templates(None).unwrap_or(defaults);
            } else {
                templates = defaults;
            }
        }
        let saved_toolbar_config = saved_toolbox
            .as_ref()
            .map(|t| t.toolbar_config.clone())
            .unwrap_or_default();

        let job_pool = JobPool::default_pool().ok();

        let initial_display_icc = display_icc_provider.load_display_icc();
        let initial_use_display_icc = initial_display_icc.is_some();

        let mut app = Self {
            tabs: Vec::new(),
            active_tab: 0,
            show_welcome: open_welcome_when_empty && initial_files.is_empty(),
            theme: saved_theme,
            ui_language: saved_language,
            theme_dirty: true,
            fonts_configured: false,
            shortcut_profile: saved_profile,
            custom_shortcuts: saved_custom_shortcuts,
            shortcut_editor_idx: 0,
            shortcut_editor_combo: String::new(),
            print_job_config: PrintJobConfig::default(),
            available_printers: Vec::new(),
            printers_status: None,
            printer_scan_rx: None,
            scanning_printers: false,
            active_tool: ToolId::Pick,
            previous_non_pick_tool: ToolId::Rectangle,
            one_shot_zoom_return_tool: None,
            group_active_tools,
            open_flyout_group: None,
            dock_state: saved_dock,
            show_dockers: saved_toolbox.as_ref().is_none_or(|t| t.show_dockers),
            toolbox_two_column: saved_toolbox.as_ref().is_some_and(|t| t.toolbox_two_column),
            active_workspace_preset: saved_toolbox
                .as_ref()
                .map_or(WorkspacePreset::DefaultClassic, |t| t.preset),
            toolbar_config: saved_toolbar_config,
            templates,
            template_category_filter: None,
            objects_filter: String::new(),
            selected_cdr_chunk_idx: None,
            show_navigator_popup: false,
            swatch_hold_state: None,
            tint_grid_popup: None,
            print_preview_zoom: 1.0,
            print_preview_pan: [0.0, 0.0],
            print_preview_plate: None,
            print_preview_tex: std::cell::RefCell::new(None),
            show_options_dialog: false,
            options_tab: 0,
            full_screen_preview: false,
            treat_all_objects_as_filled: saved_toolbox
                .as_ref()
                .is_some_and(|t| t.treat_all_objects_as_filled),
            object_origin_anchor: saved_toolbox.as_ref().map_or((1, 1), |t| {
                (
                    t.object_origin_anchor.0.min(2),
                    t.object_origin_anchor.1.min(2),
                )
            }),
            soft_proof_cmyk: false,
            gamut_warning: false,
            proof_profile: BuiltinIccProfile::default(),
            proof_intent: RenderingIntent::default(),
            use_display_icc: initial_use_display_icc,
            display_icc_info: initial_display_icc,
            display_icc_intent: RenderingIntent::RelativeColorimetric,
            stylus_provider,
            last_stylus_sample: StylusSample::DEFAULT,
            display_icc_provider,
            laser_config: LaserJobConfig::default(),
            plugin_registry: PluginRegistry::with_builtins(),
            plugin_exec_overrides: BTreeMap::new(),
            laser_plan_preview: None,
            show_laser_toolpath_overlay: false,
            last_plugin_export_path: None,
            glyph_block_filter: GlyphBlockFilter::default(),
            glyph_search: String::new(),
            last_executed_op: None,
            last_world_transform: None,
            clipboard_objects: Vec::new(),
            clipboard_snapshot: None,
            clipboard_backend,
            last_clipboard_hash: None,
            storage,
            job_pool,
            ipc_server: None,
            recent_docs,
            unclean_sessions,
            active_jobs: Vec::new(),
            autosave_interval: Duration::from_secs(30),
            default_fill: Fill::Solid(Color::cmyk(0.75, 0.15, 0.0, 0.0)),
            default_outline: OutlineStyle::solid(Color::KEY_BLACK, 0.3),
            default_text_style: TextStyle::default(),
            active_palette_index: 0,
            custom_palettes: Vec::new(),
            palette_show_names: saved_toolbox.as_ref().is_some_and(|t| t.palette_show_names),
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
            macro_hooks: Vec::new(),
            macro_hook_in_flight: false,
            new_doc_dialog: NewDocDialogState::default(),
            export_dialog: ExportDialogState::default(),
            command_palette: CommandPaletteState::default(),
            show_about_dialog: false,
            show_preferences_dialog: false,
            show_print_preview_dialog: false,
            show_edit_fill_dialog: false,
            edit_fill_draft: Fill::Solid(Color::cmyk(0.75, 0.15, 0.0, 0.0)),
            edit_fill_selected_stop: 0,
            show_outline_pen_dialog: false,
            outline_pen_draft: OutlineStyle::solid(Color::KEY_BLACK, 0.3),
            missing_font_dialog: MissingFontPromptState::default(),
            cdr_import_dialog: CdrImportPromptState::default(),
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
        self.open_file_path_with_codepage(path, None)
    }

    /// Opens any supported document file with an explicit `.cdr` fallback codepage (`CP1256`/`CP1251`/`UTF-8`).
    pub fn open_file_path_with_codepage(
        &mut self,
        path: &Path,
        default_codepage: Option<u16>,
    ) -> Result<(), String> {
        let opened = if default_codepage.is_some() {
            open_any_document_with_codepage(path, default_codepage)?
        } else {
            open_any_document(path)?
        };
        let title = opened.document.title().to_owned();
        let page_count = opened.document.pages().len() as u32;
        let color_mode = match opened.document.color_mode() {
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

        self.check_missing_fonts_on_active_tab(&title);

        if opened.warnings.is_empty() {
            self.push_toast(format!("Opened '{title}'"), false);
        } else {
            self.push_toast(
                format!("Opened '{title}' with {} notice(s)", opened.warnings.len()),
                false,
            );
        }
        let _ = self.fire_macro_hook(MacroEventHook::OnOpen);
        Ok(())
    }

    /// Imports an external vector/raster file into the active document page.
    pub fn import_file_path(&mut self, path: &Path) -> Result<usize, String> {
        let opened = open_any_document(path)?;
        enum ImportPlan {
            NoTab,
            NoUnlockedLayer,
            Ready(Vec<CommandOp>, Vec<ObjectId>),
        }
        let plan = match self.tabs.get(self.active_tab) {
            None => ImportPlan::NoTab,
            Some(tab) => {
                if resolve_import_target_layer(&tab.document, tab.active_layer_id).is_none() {
                    ImportPlan::NoUnlockedLayer
                } else {
                    let (ops, created_ids) = build_import_document_ops_on_layer(
                        &tab.document,
                        &opened.document,
                        tab.active_layer_id,
                    );
                    ImportPlan::Ready(ops, created_ids)
                }
            }
        };
        let (ops, created_ids) = match plan {
            ImportPlan::NoTab => return Err("no active document".to_owned()),
            ImportPlan::NoUnlockedLayer => {
                let msg = "Cannot import: active page has no visible unlocked layer".to_owned();
                self.push_toast(msg.clone(), true);
                return Err(msg);
            }
            ImportPlan::Ready(ops, created_ids) => (ops, created_ids),
        };
        let count = created_ids.len();
        if !ops.is_empty() && count > 0 {
            let cmd = if ops.len() == 1 {
                ops.into_iter().next().expect("len == 1")
            } else {
                CommandOp::Batch(ops)
            };
            self.execute_command(cmd)?;
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                tab.selection.select_many(&tab.document, &created_ids);
            }
            self.checkpoint_wal_snapshot_active();
        }
        self.check_missing_fonts_on_active_tab(opened.document.title());
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
                tab.document.set_title(stem.to_owned());
            }
            (
                tab.document.id().to_string(),
                tab.document.title().to_owned(),
                tab.document.pages().len() as u32,
                match tab.document.color_mode() {
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
            "cdr" | "cdt" | "cmx" | "cpt" | "svg" | "pdf" | "eps" | "dxf" | "dwg" | "wmf"
            | "emf" | "plt" | "png" | "jpg" | "tiff" | "psd" | "coree" => ext,
            _ => "coree".to_owned(),
        };

        if let Some(ref db) = self.storage {
            let _ = db.flush_autosave(Duration::from_secs(5));
            let _ = db.mark_clean_close(&doc_id_str);
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                tab.journal_seq = 0;
                tab.wal_session_open = false;
                tab.wal_session_id = None;
                tab.wal_base_snapshot_id = None;
                tab.wal_delta_count = 0;
                clear_tab_autosave_dirty(tab);
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

    /// Registers a Rhai lifecycle hook (`OnOpen`, `OnBeforeExport`, `OnSelectionChange`).
    pub fn register_macro_hook(&mut self, hook: MacroEventHook, script: impl Into<String>) {
        self.macro_hooks.push((hook, script.into()));
    }

    /// Executes all registered Rhai scripts matching `hook` on the active document via `CommandHistory`.
    pub fn fire_macro_hook(&mut self, hook: MacroEventHook) -> Result<(), String> {
        if self.macro_hook_in_flight {
            return Ok(());
        }
        let scripts: Vec<String> = self
            .macro_hooks
            .iter()
            .filter(|(h, _)| *h == hook)
            .map(|(_, s)| s.clone())
            .collect();
        if scripts.is_empty() {
            return Ok(());
        }
        self.macro_hook_in_flight = true;
        let result = (|| -> Result<(), String> {
            let mut ops_to_apply: Vec<CommandOp> = Vec::new();
            {
                let Some(tab) = self.tabs.get(self.active_tab) else {
                    return Ok(());
                };
                let mut preview_doc = tab.document.clone();
                let active_sel = tab.selection.objects.clone();
                let sandbox = MacroSandboxConfig::default();
                for script in &scripts {
                    let (_res, ops) = run_macro_event_hook_with_selection(
                        &mut preview_doc,
                        hook,
                        script,
                        &BTreeMap::new(),
                        &sandbox,
                        &active_sel,
                    )
                    .map_err(|e| e.to_string())?;
                    ops_to_apply.extend(ops);
                }
            }
            if !ops_to_apply.is_empty() {
                self.execute_command(CommandOp::Batch(ops_to_apply))?;
            }
            Ok(())
        })();
        self.macro_hook_in_flight = false;
        result
    }

    /// Fires [`MacroEventHook::OnSelectionChange`] if any hooks are registered and active selection changed.
    pub(crate) fn maybe_fire_selection_change(&mut self, before_ids: &[ObjectId]) {
        if self.macro_hooks.is_empty() || self.macro_hook_in_flight {
            return;
        }
        let changed = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|t| t.selection.objects.as_slice() != before_ids);
        if changed {
            let _ = self.fire_macro_hook(MacroEventHook::OnSelectionChange);
        }
    }

    /// Exports the active document using explicit [`UniversalExportOptions`].
    pub fn export_active(
        &mut self,
        output_path: &Path,
        opts: &UniversalExportOptions,
    ) -> Result<String, String> {
        self.fire_macro_hook(MacroEventHook::OnBeforeExport)?;
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
        if let Err(err) = self.fire_macro_hook(MacroEventHook::OnBeforeExport) {
            self.push_toast(err, true);
            return;
        }
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
        let doc_id = self.tabs[idx].document.id().to_string();
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
        let _ = self.ipc_server.take();
        let dock_sanitized = sanitize_dock_state_for_serde(&self.dock_state);
        let dock_json = serde_json::to_string(&dock_sanitized).ok();
        let toolbox_cfg = ToolboxPersistConfig {
            preset: self.active_workspace_preset,
            toolbox_two_column: self.toolbox_two_column,
            show_dockers: self.show_dockers,
            palette_show_names: self.palette_show_names,
            treat_all_objects_as_filled: self.treat_all_objects_as_filled,
            object_origin_anchor: self.object_origin_anchor,
            group_active_tools: Some(self.group_active_tools),
            toolbar_config: self.toolbar_config.clone(),
        };
        let toolbox_json = serde_json::to_string(&toolbox_cfg).unwrap_or_else(|_| "{}".to_owned());
        let shortcuts_json =
            serde_json::to_string(&self.custom_shortcuts).unwrap_or_else(|_| "{}".to_owned());
        if let Some(ref mut db) = self.storage {
            for tab in &self.tabs {
                let _ = db.mark_clean_close(&tab.document.id().to_string());
            }
            let _ = db.set_preference("ui.theme", "ui", &self.theme);
            let _ = db.set_preference("ui.language", "ui", &self.ui_language);
            let _ = db.set_preference("ui.shortcut_profile", "ui", &self.shortcut_profile);
            if let Some(json) = dock_json {
                let rec = WorkspaceLayoutRecord {
                    id: "coree-last-session".to_owned(),
                    name: "Last Session".to_owned(),
                    description: "Automatically saved workspace layout".to_owned(),
                    dock_tree_json: json,
                    toolbox_config_json: toolbox_json,
                    shortcuts_json,
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

    /// Saves an incremental differential (or baseline) crash-recovery snapshot of the active tab
    /// asynchronously on the background autosave worker thread (`<= 2 ms` UI-thread enqueue).
    pub fn perform_autosave_active(&mut self) -> Result<(), String> {
        let Some(ref db) = self.storage else {
            return Ok(());
        };
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return Ok(());
        };
        let doc_id = tab.document.id().to_string();
        if tab.wal_session_open && !tab.autosave_dirty_all {
            if let (Some(session_id), Some(base_id)) =
                (tab.wal_session_id.clone(), tab.wal_base_snapshot_id.clone())
            {
                let patch =
                    build_differential_patch_from_tab(tab, &doc_id, session_id.clone(), base_id);
                let delta_id = db
                    .enqueue_differential_autosave(patch)
                    .map_err(|e| format!("autosave failed: {e}"))?;
                tab.wal_base_snapshot_id = Some(delta_id.clone());
                clear_tab_autosave_dirty(tab);
                if tab.wal_delta_count >= coree_storage::WAL_COMPACTION_INTERVAL {
                    if let Ok(new_base) = db.enqueue_compact_baseline(
                        &doc_id,
                        &session_id,
                        tab.document.title(),
                        tab.source_path.as_deref(),
                        &delta_id,
                        tab.journal_seq,
                    ) {
                        tab.wal_base_snapshot_id = Some(new_base);
                        tab.wal_delta_count = 0;
                    }
                }
                return Ok(());
            }
        }

        let session_id = match tab.wal_session_id.clone() {
            Some(s) if tab.wal_session_open => s,
            _ => {
                db.resolve_or_open_session(&doc_id)
                    .map_err(|e| format!("autosave session error: {e}"))?
                    .0
            }
        };
        let snap_id = db
            .enqueue_baseline_snapshot(
                &doc_id,
                &session_id,
                tab.document.title(),
                tab.source_path.as_deref(),
                tab.document.clone(),
                tab.journal_seq,
            )
            .map_err(|e| format!("autosave failed: {e}"))?;
        tab.wal_session_open = true;
        tab.wal_session_id = Some(session_id);
        tab.wal_base_snapshot_id = Some(snap_id);
        tab.wal_delta_count = 0;
        clear_tab_autosave_dirty(tab);
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

        let title = doc.title().to_owned();
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

    /// Drains pending IPC file-open batches and bidirectional JSON-RPC `2.0` Scene-DOM /
    /// `CommandHistory` automation calls, executing mutations through `CommandHistory` + WAL.
    pub fn process_pending_ipc(&mut self, ctx: Option<&egui::Context>) -> usize {
        use crate::ipc::{
            dispatch_jsonrpc_on_session, JsonRpcResponse, JSONRPC_INVALID_PARAMS,
            JSONRPC_INVALID_REQUEST, JSONRPC_SERVER_ERROR,
        };

        let mut handled = 0usize;
        let ipc_batches = self
            .ipc_server
            .as_ref()
            .map(crate::ipc::IpcServer::try_recv_batches)
            .unwrap_or_default();
        if !ipc_batches.is_empty() {
            if let Some(c) = ctx {
                c.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            for batch in ipc_batches {
                handled += 1;
                for path in batch {
                    if let Err(err) = self.open_file_path(&path) {
                        self.push_toast(err, true);
                    }
                }
            }
        }

        let rpc_calls = self
            .ipc_server
            .as_ref()
            .map(crate::ipc::IpcServer::try_recv_rpc)
            .unwrap_or_default();
        for call in rpc_calls {
            handled += 1;
            let req = call.request;
            let id = req.id.clone().unwrap_or(serde_json::Value::Null);
            let resp = if req.jsonrpc != "2.0" {
                JsonRpcResponse::err(id, JSONRPC_INVALID_REQUEST, "jsonrpc must be '2.0'")
            } else {
                match req.method.as_str() {
                    "scene.execute_command" => {
                        match req
                            .params
                            .get("op")
                            .cloned()
                            .ok_or_else(|| "missing params.op".to_owned())
                            .and_then(|v| {
                                serde_json::from_value::<CommandOp>(v)
                                    .map_err(|e| format!("invalid CommandOp: {e}"))
                            }) {
                            Ok(op) => match self.execute_command(op) {
                                Ok(()) => {
                                    let (next_id, obj_count, undo_depth) =
                                        self.active_tab().map_or((0, 0, 0), |t| {
                                            (
                                                t.document.next_id(),
                                                t.document.objects().len(),
                                                t.history.undo_labels().len(),
                                            )
                                        });
                                    JsonRpcResponse::ok(
                                        id,
                                        serde_json::json!({
                                            "applied": true,
                                            "next_id": next_id,
                                            "object_count": obj_count,
                                            "undo_depth": undo_depth,
                                        }),
                                    )
                                }
                                Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e),
                            },
                            Err(e) => JsonRpcResponse::err(id, JSONRPC_INVALID_PARAMS, e),
                        }
                    }
                    "scene.execute_batch" => {
                        match req
                            .params
                            .get("ops")
                            .cloned()
                            .ok_or_else(|| "missing params.ops".to_owned())
                            .and_then(|v| {
                                serde_json::from_value::<Vec<CommandOp>>(v)
                                    .map_err(|e| format!("invalid CommandOp array: {e}"))
                            }) {
                            Ok(ops) => match self.execute_command(CommandOp::Batch(ops)) {
                                Ok(()) => {
                                    let (next_id, obj_count, undo_depth) =
                                        self.active_tab().map_or((0, 0, 0), |t| {
                                            (
                                                t.document.next_id(),
                                                t.document.objects().len(),
                                                t.history.undo_labels().len(),
                                            )
                                        });
                                    JsonRpcResponse::ok(
                                        id,
                                        serde_json::json!({
                                            "applied": true,
                                            "next_id": next_id,
                                            "object_count": obj_count,
                                            "undo_depth": undo_depth,
                                        }),
                                    )
                                }
                                Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e),
                            },
                            Err(e) => JsonRpcResponse::err(id, JSONRPC_INVALID_PARAMS, e),
                        }
                    }
                    "scene.undo" => match self.undo() {
                        Ok(label) => {
                            let (obj_count, undo_depth, redo_depth) =
                                self.active_tab().map_or((0, 0, 0), |t| {
                                    (
                                        t.document.objects().len(),
                                        t.history.undo_labels().len(),
                                        t.history.redo_labels().len(),
                                    )
                                });
                            JsonRpcResponse::ok(
                                id,
                                serde_json::json!({
                                    "undone": true,
                                    "label": label,
                                    "object_count": obj_count,
                                    "undo_depth": undo_depth,
                                    "redo_depth": redo_depth,
                                }),
                            )
                        }
                        Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e),
                    },
                    "scene.redo" => match self.redo() {
                        Ok(label) => {
                            let (obj_count, undo_depth, redo_depth) =
                                self.active_tab().map_or((0, 0, 0), |t| {
                                    (
                                        t.document.objects().len(),
                                        t.history.undo_labels().len(),
                                        t.history.redo_labels().len(),
                                    )
                                });
                            JsonRpcResponse::ok(
                                id,
                                serde_json::json!({
                                    "redone": true,
                                    "label": label,
                                    "object_count": obj_count,
                                    "undo_depth": undo_depth,
                                    "redo_depth": redo_depth,
                                }),
                            )
                        }
                        Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e),
                    },
                    "app.get_selection" => {
                        let ids: Vec<u64> = self
                            .active_tab()
                            .map(|t| t.selection.objects.iter().map(|oid| oid.0).collect())
                            .unwrap_or_default();
                        JsonRpcResponse::ok(id, serde_json::json!({ "selected_ids": ids }))
                    }
                    "app.set_selection" => {
                        let raw_ids = req
                            .params
                            .get("ids")
                            .and_then(serde_json::Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        let ids: Vec<ObjectId> = raw_ids
                            .iter()
                            .filter_map(serde_json::Value::as_u64)
                            .map(ObjectId)
                            .collect();
                        let applied_ids: Vec<u64> =
                            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                                tab.selection.select_many(&tab.document, &ids);
                                tab.selection.objects.iter().map(|oid| oid.0).collect()
                            } else {
                                Vec::new()
                            };
                        let _ = self.fire_macro_hook(MacroEventHook::OnSelectionChange);
                        JsonRpcResponse::ok(id, serde_json::json!({ "selected_ids": applied_ids }))
                    }
                    "scene.run_macro" => {
                        let script = req
                            .params
                            .get("script")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("");
                        if script.trim().is_empty() {
                            JsonRpcResponse::err(
                                id,
                                JSONRPC_INVALID_PARAMS,
                                "missing params.script",
                            )
                        } else if let Some(tab) = self.tabs.get(self.active_tab) {
                            let sandbox = MacroSandboxConfig::default();
                            match plan_macro_ops_with_selection(
                                &tab.document,
                                script,
                                &BTreeMap::new(),
                                &sandbox,
                                &tab.selection.objects,
                            ) {
                                Ok((res, ops)) => {
                                    let apply_res = if ops.is_empty() {
                                        Ok(())
                                    } else {
                                        self.execute_command(CommandOp::Batch(ops))
                                    };
                                    match apply_res {
                                        Ok(()) => {
                                            let obj_count = self
                                                .active_tab()
                                                .map_or(0, |t| t.document.objects().len());
                                            JsonRpcResponse::ok(
                                                id,
                                                serde_json::json!({
                                                    "commands_executed": res.commands_executed,
                                                    "logs": res.logs,
                                                    "object_count": obj_count,
                                                }),
                                            )
                                        }
                                        Err(e) => JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e),
                                    }
                                }
                                Err(e) => {
                                    JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, e.to_string())
                                }
                            }
                        } else {
                            JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, "no active document tab")
                        }
                    }
                    _ => match self.tabs.get_mut(self.active_tab) {
                        Some(tab) => {
                            dispatch_jsonrpc_on_session(&req, &mut tab.document, &mut tab.history)
                        }
                        None => {
                            JsonRpcResponse::err(id, JSONRPC_SERVER_ERROR, "no active document tab")
                        }
                    },
                }
            };
            let _ = call.reply_tx.send(resp);
        }

        handled
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

    /// Checks whether the active creation target layer is writable (visible and unlocked),
    /// returning `Ok(Option<LayerId>)` for `CommandOp::create_object` or an error message.
    pub(crate) fn active_creation_layer_or_err(&self) -> Result<Option<LayerId>, String> {
        let tab = self
            .tabs
            .get(self.active_tab)
            .ok_or_else(|| "no active document".to_owned())?;
        if let Some(lid) = tab.active_layer_id {
            match tab.document.find_layer(lid) {
                Some(l) if l.locked => Err(format!("Layer '{}' is locked", l.name)),
                Some(l) if !l.visible => Err(format!("Layer '{}' is hidden", l.name)),
                Some(_) => Ok(Some(lid)),
                None => Err("Active layer no longer exists".to_owned()),
            }
        } else if let Some(page) = tab.document.active_page() {
            if page.layers.iter().any(|l| l.visible && !l.locked) {
                Ok(None)
            } else if page.layers.iter().all(|l| l.locked) {
                Err("All layers on active page are locked".to_owned())
            } else {
                Err("Active page has no visible unlocked layer".to_owned())
            }
        } else {
            Err("No active page".to_owned())
        }
    }

    /// Validates that the active creation target layer is visible and unlocked, pushing an
    /// error toast and returning `None` if creation should be refused.
    pub(crate) fn ensure_active_creation_layer(&mut self) -> Option<Option<LayerId>> {
        match self.active_creation_layer_or_err() {
            Ok(lid) => Some(lid),
            Err(e) => {
                self.push_toast(e, true);
                None
            }
        }
    }

    /// Creates `obj` on the active writable layer via `CommandOp::create_object`, propagating
    /// errors as error toasts and selecting the newly created object only on success.
    pub(crate) fn create_object_on_active_layer(&mut self, obj: SceneObject) -> Option<ObjectId> {
        let layer_id = self.ensure_active_creation_layer()?;
        match self.execute_command(CommandOp::create_object(obj, layer_id)) {
            Ok(()) => {
                let created_id = self
                    .active_tab()
                    .and_then(|t| t.document.objects().keys().last().copied());
                if let (Some(tab), Some(id)) = (self.active_tab_mut(), created_id) {
                    tab.selection.select_single(&tab.document, id);
                    tab.metrics_dirty = true;
                    tab.needs_rerender = true;
                }
                created_id
            }
            Err(e) => {
                self.push_toast(e, true);
                None
            }
        }
    }

    /// Executes `op` through the active tab's [`CommandHistory`] and appends it to SQLite WAL journal.
    pub fn execute_command(&mut self, op: CommandOp) -> Result<(), String> {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return Err("no active document".to_owned());
        };
        let doc_id = tab.document.id().to_string();

        // Ensure an active unclean baseline snapshot exists before journaling commands
        // (including after `save_active_as` marks the previous session cleanly closed).
        if !tab.wal_session_open {
            if let Some(ref db) = self.storage {
                if let Ok((session_id, _)) = db.resolve_or_open_session(&doc_id) {
                    if let Ok(base_id) = db.enqueue_baseline_snapshot(
                        &doc_id,
                        &session_id,
                        tab.document.title(),
                        tab.source_path.as_deref(),
                        tab.document.clone(),
                        tab.journal_seq,
                    ) {
                        tab.wal_session_open = true;
                        tab.wal_session_id = Some(session_id);
                        tab.wal_base_snapshot_id = Some(base_id);
                        tab.wal_delta_count = 0;
                        clear_tab_autosave_dirty(tab);
                    }
                }
            }
        }

        tab.history
            .execute(&mut tab.document, op.clone())
            .map_err(|e| e.to_string())?;
        let pruned_assets = tab.history.take_last_gc_pruned();
        let inv = tab.history.last_invalidation().clone();
        let wal_hints = tab.history.last_delta_wal_hints().clone();
        tab.journal_seq = tab.journal_seq.saturating_add(1);
        let seq = tab.journal_seq;
        tab.mark_modified_with_invalidation(&inv);
        if wal_hints.touches_structure || command_op_touches_structure(&op) {
            tab.autosave_dirty_structure = true;
        }
        tab.autosave_dirty_palette |= wal_hints.touches_palette;
        for &aid in &wal_hints.upserted_assets {
            tab.autosave_deleted_assets.remove(&aid);
            tab.autosave_upserted_assets.insert(aid);
        }
        for &aid in &wal_hints.deleted_assets {
            tab.autosave_upserted_assets.remove(&aid);
            tab.autosave_deleted_assets.insert(aid);
        }
        if !pruned_assets.is_empty() {
            tab.autosave_dirty_structure = true;
            for &aid in &pruned_assets {
                tab.autosave_upserted_assets.remove(&aid);
                tab.autosave_deleted_assets.insert(aid);
            }
        }
        tab.wal_delta_count = tab.wal_delta_count.saturating_add(1);

        match &op {
            CommandOp::TransformObjects { ids, affine } if !ids.is_empty() => {
                if let Some((ref batch_ids, ref mut prev_delta)) = tab.last_duplicate_batch {
                    if *batch_ids == *ids {
                        *prev_delta = affine.compose(prev_delta);
                    } else {
                        tab.last_duplicate_batch = None;
                    }
                }
            }
            _ => {
                tab.last_duplicate_batch = None;
            }
        }

        if let Some(ref db) = self.storage {
            if let Some(sess_id) = tab.wal_session_id.clone() {
                let base_id = tab.wal_base_snapshot_id.as_deref();
                let _ = db.enqueue_command_async(&doc_id, &sess_id, base_id, seq, op.clone());
                if !pruned_assets.is_empty() {
                    tab.journal_seq = tab.journal_seq.saturating_add(1);
                    let prune_seq = tab.journal_seq;
                    let _ = db.enqueue_command_async(
                        &doc_id,
                        &sess_id,
                        base_id,
                        prune_seq,
                        CommandOp::PruneAssets { ids: pruned_assets },
                    );
                }
                if tab.wal_delta_count >= coree_storage::WAL_COMPACTION_INTERVAL {
                    if tab.autosave_dirty_all
                        || !tab.wal_session_open
                        || tab.wal_base_snapshot_id.is_none()
                    {
                        if let Ok(snap_id) = db.enqueue_baseline_snapshot(
                            &doc_id,
                            &sess_id,
                            tab.document.title(),
                            tab.source_path.as_deref(),
                            tab.document.clone(),
                            tab.journal_seq,
                        ) {
                            tab.wal_session_open = true;
                            tab.wal_base_snapshot_id = Some(snap_id);
                            tab.wal_delta_count = 0;
                            clear_tab_autosave_dirty(tab);
                        }
                    } else if let Some(latest_id) = tab.wal_base_snapshot_id.clone() {
                        // Flush accumulated dirty objects/structure/assets/palette into a
                        // differential delta at `tab.journal_seq` before compacting, so the
                        // compacted baseline never skips un-materialized `command_journal` edits.
                        let patch = build_differential_patch_from_tab(
                            tab,
                            &doc_id,
                            sess_id.clone(),
                            latest_id,
                        );
                        if let Ok(delta_id) = db.enqueue_differential_autosave(patch) {
                            tab.wal_base_snapshot_id = Some(delta_id.clone());
                            clear_tab_autosave_dirty(tab);
                            if let Ok(new_base) = db.enqueue_compact_baseline(
                                &doc_id,
                                &sess_id,
                                tab.document.title(),
                                tab.source_path.as_deref(),
                                &delta_id,
                                tab.journal_seq,
                            ) {
                                tab.wal_base_snapshot_id = Some(new_base);
                                tab.wal_delta_count = 0;
                            }
                        }
                    }
                }
            } else {
                let _ = db.append_command(&doc_id, seq, &op);
                if !pruned_assets.is_empty() {
                    tab.journal_seq = tab.journal_seq.saturating_add(1);
                    let prune_seq = tab.journal_seq;
                    let _ = db.append_command(
                        &doc_id,
                        prune_seq,
                        &CommandOp::PruneAssets { ids: pruned_assets },
                    );
                }
            }
        }
        self.last_world_transform = match &op {
            CommandOp::TransformObjects { ids, affine } if !ids.is_empty() => {
                let doc = &self.tabs[self.active_tab].document;
                Some(helpers::unconjugate_local_affine_to_world(
                    doc, ids[0], *affine,
                ))
            }
            CommandOp::Batch(ops)
                if !ops.is_empty()
                    && ops.iter().all(|inner| {
                        matches!(inner, CommandOp::TransformObjects { ids, .. } if !ids.is_empty())
                    }) =>
            {
                if let Some(CommandOp::TransformObjects { ids, affine }) = ops.first() {
                    let doc = &self.tabs[self.active_tab].document;
                    Some(helpers::unconjugate_local_affine_to_world(
                        doc, ids[0], *affine,
                    ))
                } else {
                    None
                }
            }
            _ => None,
        };
        self.last_executed_op = Some(op);
        Ok(())
    }

    /// Applies `world_affine` in world coordinates to `ids` via `build_world_transform_command`.
    pub(crate) fn apply_world_transform_to_selection(
        &mut self,
        ids: &[ObjectId],
        world_affine: Affine2D,
    ) -> bool {
        let Some(cmd) = self
            .active_tab()
            .and_then(|t| helpers::build_world_transform_command(&t.document, ids, world_affine))
        else {
            return false;
        };
        self.execute_command(cmd).is_ok()
    }

    /// Undoes the last command on the active tab and streams a targeted zero-clone WAL delta
    /// so a subsequent crash cannot resurrect the undone edit.
    pub fn undo(&mut self) -> Result<String, String> {
        let (label, inv, wal_hints) = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let label = tab
                .history
                .undo(&mut tab.document)
                .map_err(|e| e.to_string())?;
            let inv = tab.history.last_invalidation().clone();
            let wal_hints = tab.history.last_delta_wal_hints().clone();
            tab.last_duplicate_batch = None;
            tab.viewport.multipage_view = tab.document.multipage_view();
            tab.snap_options.grid = tab.document.grid().snap;
            tab.sync_active_layer();
            tab.mark_modified_with_invalidation(&inv);
            tab.autosave_dirty_structure |= wal_hints.touches_structure;
            tab.autosave_dirty_palette |= wal_hints.touches_palette;
            for &aid in &wal_hints.upserted_assets {
                tab.autosave_deleted_assets.remove(&aid);
                tab.autosave_upserted_assets.insert(aid);
            }
            for &aid in &wal_hints.deleted_assets {
                tab.autosave_upserted_assets.remove(&aid);
                tab.autosave_deleted_assets.insert(aid);
            }
            tab.journal_seq = tab.journal_seq.saturating_add(1);
            tab.wal_delta_count = tab.wal_delta_count.saturating_add(1);
            (label, inv, wal_hints)
        };
        self.stream_wal_undo_redo_delta(&inv, &wal_hints);
        self.push_toast(format!("Undo: {label}"), false);
        Ok(label)
    }

    /// Redoes the last undone command on the active tab and streams a targeted zero-clone WAL delta.
    pub fn redo(&mut self) -> Result<String, String> {
        let (label, inv, wal_hints) = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return Err("no active document".to_owned());
            };
            let label = tab
                .history
                .redo(&mut tab.document)
                .map_err(|e| e.to_string())?;
            let inv = tab.history.last_invalidation().clone();
            let wal_hints = tab.history.last_delta_wal_hints().clone();
            tab.last_duplicate_batch = None;
            tab.viewport.multipage_view = tab.document.multipage_view();
            tab.snap_options.grid = tab.document.grid().snap;
            tab.sync_active_layer();
            tab.mark_modified_with_invalidation(&inv);
            tab.autosave_dirty_structure |= wal_hints.touches_structure;
            tab.autosave_dirty_palette |= wal_hints.touches_palette;
            for &aid in &wal_hints.upserted_assets {
                tab.autosave_deleted_assets.remove(&aid);
                tab.autosave_upserted_assets.insert(aid);
            }
            for &aid in &wal_hints.deleted_assets {
                tab.autosave_upserted_assets.remove(&aid);
                tab.autosave_deleted_assets.insert(aid);
            }
            tab.journal_seq = tab.journal_seq.saturating_add(1);
            tab.wal_delta_count = tab.wal_delta_count.saturating_add(1);
            (label, inv, wal_hints)
        };
        self.stream_wal_undo_redo_delta(&inv, &wal_hints);
        self.push_toast(format!("Redo: {label}"), false);
        Ok(label)
    }

    fn stream_wal_undo_redo_delta(
        &mut self,
        inv: &coree_core::DocumentInvalidation,
        wal_hints: &coree_core::DeltaWalHints,
    ) {
        let full_all = matches!(inv, coree_core::DocumentInvalidation::All);
        let needs_baseline = self.tabs.get(self.active_tab).is_some_and(|t| {
            !t.wal_session_open
                || t.wal_session_id.is_none()
                || t.wal_base_snapshot_id.is_none()
                || t.autosave_dirty_all
                || full_all
        });
        if needs_baseline {
            self.checkpoint_wal_snapshot_active();
            return;
        }
        let Some(ref db) = self.storage else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let (Some(session_id), Some(base_id)) =
            (tab.wal_session_id.clone(), tab.wal_base_snapshot_id.clone())
        else {
            return;
        };
        let doc_id = tab.document.id().to_string();
        let struct_dirty = tab.autosave_dirty_structure || wal_hints.touches_structure;
        let mut upserted_objects = Vec::new();
        let mut deleted_object_ids = Vec::new();
        for &oid in &tab.autosave_dirty_objects {
            if let Some(obj) = tab.document.object(oid) {
                upserted_objects.push((oid, obj.clone()));
            } else {
                deleted_object_ids.push(oid);
            }
        }
        let mut merged_upserted: HashSet<ImageAssetId> = tab.autosave_upserted_assets.clone();
        merged_upserted.extend(wal_hints.upserted_assets.iter().copied());
        let mut merged_deleted: HashSet<ImageAssetId> = tab.autosave_deleted_assets.clone();
        merged_deleted.extend(wal_hints.deleted_assets.iter().copied());
        for aid in &merged_deleted {
            merged_upserted.remove(aid);
        }
        let mut upserted_assets = Vec::new();
        for aid in merged_upserted {
            if let Some(asset) = tab.document.asset(aid) {
                upserted_assets.push((aid, asset.clone()));
            }
        }
        let deleted_asset_ids: Vec<ImageAssetId> = merged_deleted.into_iter().collect();
        let pal_dirty = struct_dirty || tab.autosave_dirty_palette || wal_hints.touches_palette;
        let patch = DifferentialSnapshotPatch {
            doc_id: doc_id.clone(),
            session_id: session_id.clone(),
            title: tab.document.title().to_owned(),
            source_path: tab
                .source_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            base_snapshot_id: base_id,
            last_command_seq: tab.journal_seq,
            upserted_objects,
            deleted_object_ids,
            upserted_assets,
            deleted_asset_ids,
            full_assets: None,
            pages: struct_dirty.then(|| tab.document.pages().to_vec()),
            active_page_index: struct_dirty.then(|| tab.document.active_page_index()),
            next_id: Some(tab.document.next_id()),
            color_mode: struct_dirty.then(|| tab.document.color_mode()),
            default_unit: struct_dirty.then(|| tab.document.default_unit()),
            dpi: struct_dirty.then(|| tab.document.dpi()),
            multipage_view: struct_dirty.then(|| tab.document.multipage_view()),
            palettes: pal_dirty.then(|| tab.document.palettes().to_vec()),
            document_palette: pal_dirty.then(|| tab.document.document_palette().clone()),
            guides: struct_dirty.then(|| tab.document.guides().to_vec()),
            grid: struct_dirty.then(|| tab.document.grid()),
            perspective_grid: struct_dirty.then(|| tab.document.perspective_grid().clone()),
            color_styles: struct_dirty.then(|| tab.document.color_styles().to_vec()),
            object_styles: struct_dirty.then(|| tab.document.object_styles().to_vec()),
            symbols: struct_dirty.then(|| tab.document.symbols().clone()),
            metadata: struct_dirty.then(|| tab.document.metadata().clone()),
        };
        if let Ok(delta_id) = db.enqueue_differential_autosave(patch) {
            tab.wal_base_snapshot_id = Some(delta_id.clone());
            clear_tab_autosave_dirty(tab);
            if tab.wal_delta_count >= coree_storage::WAL_COMPACTION_INTERVAL {
                if let Ok(new_base) = db.enqueue_compact_baseline(
                    &doc_id,
                    &session_id,
                    tab.document.title(),
                    tab.source_path.as_deref(),
                    &delta_id,
                    tab.journal_seq,
                ) {
                    tab.wal_base_snapshot_id = Some(new_base);
                    tab.wal_delta_count = 0;
                }
            }
        }
    }

    /// Builds an immutable [`ToolSettingsSnapshot`] + active lens config for `ToolCore`.
    #[must_use]
    pub fn current_tool_snapshot(&self) -> (ToolSettingsSnapshot, LensConfig) {
        (
            self.tool_settings
                .to_snapshot(&self.properties_state, &self.effects_state),
            self.lens_state.build_config(),
        )
    }

    /// Dispatches a pointer-down event to the active tab's [`ToolCore`] and applies its output.
    pub fn run_tool_pointer_down(&mut self, event: &PointerEvent) {
        let creation_layer_id = self.ensure_active_creation_layer().flatten();
        let (settings, default_lens) = self.current_tool_snapshot();
        let treat_all = self.treat_all_objects_as_filled;
        let default_fill = self.default_fill.clone();
        let default_outline = self.default_outline.clone();
        let default_text_style = self.default_text_style.clone();
        let active_tool = self.active_tool;

        let out = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return;
            };
            if tab.tool_core.active_tool() != active_tool {
                tab.tool_core.set_active_tool(active_tool);
            }
            tab.cursor_world_mm = event.world_pt_mm;
            // Keep ToolCore's mirror of pending crop/raster/guide/mesh state synced if modified from toolbar.
            tab.tool_core.pending_crop_rect = tab.pending_crop_rect;
            tab.tool_core.raster_mask_rect_mm = tab.raster_mask_rect_mm;
            tab.tool_core.selected_guide_id = tab.selected_guide_id;
            tab.tool_core.selected_mesh_node = tab.selected_mesh_node;
            let mut tool_core = std::mem::replace(&mut tab.tool_core, ToolCore::new(active_tool));
            let ctx = ToolReadContext {
                document: &tab.document,
                selection: &tab.selection,
                snap_engine: &SnapEngine,
                viewport: &tab.viewport,
                snap_options: &tab.snap_options,
                active_layer_id: tab.active_layer_id,
                creation_layer_id,
                powerclip_edit_id: tab.powerclip_edit_id,
                treat_all_objects_as_filled: treat_all,
                default_fill: &default_fill,
                default_outline: &default_outline,
                default_text_style: &default_text_style,
                default_lens: &default_lens,
                settings: &settings,
            };
            let out = tool_core.on_pointer_down(&ctx, event);
            tab.tool_core = tool_core;
            out
        };
        self.apply_tool_output(out);
    }

    /// Dispatches a pointer-move/drag event to the active tab's [`ToolCore`] and applies its output.
    pub fn run_tool_pointer_move(&mut self, event: &PointerEvent) {
        let creation_layer_id = self
            .tabs
            .get(self.active_tab)
            .and_then(|t| t.active_layer_id);
        let (settings, default_lens) = self.current_tool_snapshot();
        let treat_all = self.treat_all_objects_as_filled;
        let default_fill = self.default_fill.clone();
        let default_outline = self.default_outline.clone();
        let default_text_style = self.default_text_style.clone();
        let active_tool = self.active_tool;

        let out = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return;
            };
            if tab.tool_core.active_tool() != active_tool {
                tab.tool_core.set_active_tool(active_tool);
            }
            tab.cursor_world_mm = event.world_pt_mm;
            tab.tool_core.pending_crop_rect = tab.pending_crop_rect;
            tab.tool_core.raster_mask_rect_mm = tab.raster_mask_rect_mm;
            tab.tool_core.selected_guide_id = tab.selected_guide_id;
            tab.tool_core.selected_mesh_node = tab.selected_mesh_node;
            let mut tool_core = std::mem::replace(&mut tab.tool_core, ToolCore::new(active_tool));
            let ctx = ToolReadContext {
                document: &tab.document,
                selection: &tab.selection,
                snap_engine: &SnapEngine,
                viewport: &tab.viewport,
                snap_options: &tab.snap_options,
                active_layer_id: tab.active_layer_id,
                creation_layer_id,
                powerclip_edit_id: tab.powerclip_edit_id,
                treat_all_objects_as_filled: treat_all,
                default_fill: &default_fill,
                default_outline: &default_outline,
                default_text_style: &default_text_style,
                default_lens: &default_lens,
                settings: &settings,
            };
            let out = tool_core.on_pointer_move(&ctx, event);
            tab.tool_core = tool_core;
            out
        };
        self.apply_tool_output(out);
    }

    /// Dispatches a pointer-up event to the active tab's [`ToolCore`] and applies its output.
    pub fn run_tool_pointer_up(&mut self, event: &PointerEvent) {
        let creation_layer_id = self.ensure_active_creation_layer().flatten();
        let (settings, default_lens) = self.current_tool_snapshot();
        let treat_all = self.treat_all_objects_as_filled;
        let default_fill = self.default_fill.clone();
        let default_outline = self.default_outline.clone();
        let default_text_style = self.default_text_style.clone();
        let active_tool = self.active_tool;

        let out = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return;
            };
            if tab.tool_core.active_tool() != active_tool {
                tab.tool_core.set_active_tool(active_tool);
            }
            tab.cursor_world_mm = event.world_pt_mm;
            tab.tool_core.pending_crop_rect = tab.pending_crop_rect;
            tab.tool_core.raster_mask_rect_mm = tab.raster_mask_rect_mm;
            tab.tool_core.selected_guide_id = tab.selected_guide_id;
            tab.tool_core.selected_mesh_node = tab.selected_mesh_node;
            let mut tool_core = std::mem::replace(&mut tab.tool_core, ToolCore::new(active_tool));
            let ctx = ToolReadContext {
                document: &tab.document,
                selection: &tab.selection,
                snap_engine: &SnapEngine,
                viewport: &tab.viewport,
                snap_options: &tab.snap_options,
                active_layer_id: tab.active_layer_id,
                creation_layer_id,
                powerclip_edit_id: tab.powerclip_edit_id,
                treat_all_objects_as_filled: treat_all,
                default_fill: &default_fill,
                default_outline: &default_outline,
                default_text_style: &default_text_style,
                default_lens: &default_lens,
                settings: &settings,
            };
            let out = tool_core.on_pointer_up(&ctx, event);
            tab.tool_core = tool_core;
            out
        };
        self.apply_tool_output(out);
    }

    /// Dispatches a keyboard event to the active tab's [`ToolCore`] and applies its output.
    pub fn run_tool_key_event(&mut self, event: &KeyEvent) {
        let creation_layer_id = self
            .tabs
            .get(self.active_tab)
            .and_then(|t| t.active_layer_id);
        let (settings, default_lens) = self.current_tool_snapshot();
        let treat_all = self.treat_all_objects_as_filled;
        let default_fill = self.default_fill.clone();
        let default_outline = self.default_outline.clone();
        let default_text_style = self.default_text_style.clone();
        let active_tool = self.active_tool;

        let out = {
            let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                return;
            };
            if tab.tool_core.active_tool() != active_tool {
                tab.tool_core.set_active_tool(active_tool);
            }
            tab.tool_core.pending_crop_rect = tab.pending_crop_rect;
            tab.tool_core.raster_mask_rect_mm = tab.raster_mask_rect_mm;
            tab.tool_core.selected_guide_id = tab.selected_guide_id;
            tab.tool_core.selected_mesh_node = tab.selected_mesh_node;
            let mut tool_core = std::mem::replace(&mut tab.tool_core, ToolCore::new(active_tool));
            let ctx = ToolReadContext {
                document: &tab.document,
                selection: &tab.selection,
                snap_engine: &SnapEngine,
                viewport: &tab.viewport,
                snap_options: &tab.snap_options,
                active_layer_id: tab.active_layer_id,
                creation_layer_id,
                powerclip_edit_id: tab.powerclip_edit_id,
                treat_all_objects_as_filled: treat_all,
                default_fill: &default_fill,
                default_outline: &default_outline,
                default_text_style: &default_text_style,
                default_lens: &default_lens,
                settings: &settings,
            };
            let out = tool_core.on_key(&ctx, event);
            tab.tool_core = tool_core;
            out
        };
        self.apply_tool_output(out);
    }

    /// Applies a [`ToolOutput`] returned by [`ToolCore`] to the active tab and studio shell.
    pub fn apply_tool_output(&mut self, output: ToolOutput) {
        match output {
            ToolOutput::Continue { overlay } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.active_guides = overlay.active_guides.clone();
                    tab.transient_overlay = overlay;
                    tab.viewport.lod_dragging = tab.tool_core.is_transform_dragging();
                }
            }
            ToolOutput::Cancel => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.transient_overlay = TransientOverlay::default();
                    tab.active_guides.clear();
                    tab.viewport.lod_dragging = false;
                    tab.pending_crop_rect = tab.tool_core.pending_crop_rect;
                    tab.raster_mask_rect_mm = tab.tool_core.raster_mask_rect_mm;
                }
            }
            ToolOutput::Commit {
                mut ops,
                clear_overlay,
                post_actions,
            } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.lod_dragging = false;
                    if clear_overlay {
                        tab.transient_overlay = TransientOverlay::default();
                        tab.active_guides.clear();
                    }
                    tab.pending_crop_rect = tab.tool_core.pending_crop_rect;
                    tab.raster_mask_rect_mm = tab.tool_core.raster_mask_rect_mm;
                    tab.selected_guide_id = tab.tool_core.selected_guide_id;
                    tab.selected_mesh_node = tab.tool_core.selected_mesh_node;
                }
                if !ops.is_empty() {
                    let has_create = ops
                        .iter()
                        .any(|op| matches!(op, CommandOp::CreateObject { .. }));
                    let has_explicit_select = post_actions
                        .iter()
                        .any(|a| matches!(a, ToolActionRequest::SelectObjects { .. }));
                    let cmd = if ops.len() == 1 {
                        ops.remove(0)
                    } else {
                        CommandOp::Batch(ops)
                    };
                    if let Err(err) = self.execute_command(cmd) {
                        self.push_toast(err, true);
                        return;
                    }
                    if has_create && !has_explicit_select {
                        let before_sel = self
                            .active_tab()
                            .map(|t| t.selection.objects.clone())
                            .unwrap_or_default();
                        let mut updated = false;
                        if let Some(tab) = self.active_tab_mut() {
                            if let Some(created_id) = tab.document.objects().keys().last().copied()
                            {
                                tab.selection.select_single(&tab.document, created_id);
                                tab.metrics_dirty = true;
                                updated = true;
                            }
                        }
                        if updated {
                            self.maybe_fire_selection_change(&before_sel);
                        }
                    }
                }
                for act in post_actions {
                    self.handle_tool_action(act);
                }
            }
            ToolOutput::Consumed { overlay, actions } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.lod_dragging = tab.tool_core.is_transform_dragging();
                    if let Some(ov) = overlay {
                        tab.active_guides = ov.active_guides.clone();
                        tab.transient_overlay = ov;
                    }
                    tab.pending_crop_rect = tab.tool_core.pending_crop_rect;
                    tab.raster_mask_rect_mm = tab.tool_core.raster_mask_rect_mm;
                    tab.selected_guide_id = tab.tool_core.selected_guide_id;
                    tab.selected_mesh_node = tab.tool_core.selected_mesh_node;
                }
                for act in actions {
                    self.handle_tool_action(act);
                }
            }
        }
    }

    /// Executes a single host-delegated [`ToolActionRequest`] emitted by [`ToolCore`].
    pub fn handle_tool_action(&mut self, action: ToolActionRequest) {
        match action {
            ToolActionRequest::SelectObjects {
                ids,
                additive,
                toggle,
            } => {
                let active_tool = self.active_tool;
                let before_sel = self
                    .active_tab()
                    .map(|t| t.selection.objects.clone())
                    .unwrap_or_default();
                if let Some(tab) = self.active_tab_mut() {
                    if ids.is_empty() {
                        if !additive && !toggle {
                            tab.selection.clear();
                        }
                    } else if additive {
                        for id in ids {
                            if !tab.selection.contains(id) {
                                tab.selection.toggle_object(&tab.document, id);
                            }
                        }
                    } else if toggle {
                        for id in ids {
                            tab.selection.click_object(&tab.document, id, true);
                        }
                    } else if ids.len() == 1 {
                        if active_tool == ToolId::Pick {
                            tab.selection.click_object(&tab.document, ids[0], false);
                        } else {
                            tab.selection.select_single(&tab.document, ids[0]);
                        }
                    } else {
                        tab.selection.select_many(&tab.document, &ids);
                    }
                    tab.metrics_dirty = true;
                    tab.last_duplicate_batch = None;
                }
                self.maybe_fire_selection_change(&before_sel);
            }
            ToolActionRequest::ClearSelection => {
                let before_sel = self
                    .active_tab()
                    .map(|t| t.selection.objects.clone())
                    .unwrap_or_default();
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.clear();
                    tab.metrics_dirty = true;
                    tab.last_duplicate_batch = None;
                }
                self.maybe_fire_selection_change(&before_sel);
            }
            ToolActionRequest::ToggleTransformMode => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.toggle_transform_mode();
                    tab.metrics_dirty = true;
                }
            }
            ToolActionRequest::SelectNodes { nodes, additive } => {
                if let Some(tab) = self.active_tab_mut() {
                    if !additive {
                        tab.selection.node_selection.clear();
                    }
                    for n in nodes {
                        if additive {
                            tab.selection.node_selection.toggle(n);
                        } else if !tab.selection.node_selection.contains(n) {
                            tab.selection.node_selection.nodes.push(n);
                        }
                    }
                }
            }
            ToolActionRequest::ClearNodeSelection => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.node_selection.clear();
                }
            }
            ToolActionRequest::SetPivot { pivot_mm } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.set_pivot(pivot_mm);
                }
            }
            ToolActionRequest::PanTo {
                pan_mm,
                lod_dragging,
            } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.pan_mm = pan_mm;
                    tab.viewport.lod_dragging = lod_dragging;
                    tab.needs_rerender = true;
                }
            }
            ToolActionRequest::ZoomFitRect {
                rect_mm,
                margin_px,
                one_shot_complete,
            } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.fit_rect(rect_mm, margin_px);
                    tab.needs_rerender = true;
                }
                if one_shot_complete {
                    self.complete_one_shot_zoom_if_active();
                }
            }
            ToolActionRequest::ZoomAtScreen {
                screen_px,
                factor,
                one_shot_complete,
            } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.viewport.zoom_at_screen_point(screen_px, factor);
                    tab.needs_rerender = true;
                }
                if one_shot_complete {
                    self.complete_one_shot_zoom_if_active();
                }
            }
            ToolActionRequest::SetPendingCropRect(rect) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.pending_crop_rect = rect;
                    tab.tool_core.pending_crop_rect = rect;
                }
            }
            ToolActionRequest::ApplyCropRect(rect) => {
                self.apply_crop_rect(rect);
                if let Some(tab) = self.active_tab_mut() {
                    tab.pending_crop_rect = None;
                    tab.tool_core.pending_crop_rect = None;
                    tab.transient_overlay = TransientOverlay::default();
                }
            }
            ToolActionRequest::SetRasterMaskRect(rect) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.raster_mask_rect_mm = rect;
                    tab.tool_core.raster_mask_rect_mm = rect;
                }
            }
            ToolActionRequest::SetRasterMask(sel) => {
                if let Some(tab) = self.active_tab_mut() {
                    if let Some(ref s) = sel {
                        if let Some(bb) = s.world_bounds() {
                            tab.raster_mask_rect_mm = Some(bb);
                            tab.tool_core.raster_mask_rect_mm = Some(bb);
                        }
                    }
                    tab.raster_mask = sel;
                    tab.needs_rerender = true;
                }
            }
            ToolActionRequest::ClearRasterMask => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.raster_mask = None;
                    tab.raster_mask_rect_mm = None;
                    tab.tool_core.raster_mask_rect_mm = None;
                    tab.needs_rerender = true;
                }
            }
            ToolActionRequest::SetSelectedGuide(gid) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selected_guide_id = gid;
                    tab.tool_core.selected_guide_id = gid;
                }
            }
            ToolActionRequest::SetSelectedMeshNode(node) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selected_mesh_node = node;
                    tab.tool_core.selected_mesh_node = node;
                }
            }
            ToolActionRequest::EnterPowerClipEdit(oid) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.powerclip_edit_id = Some(oid);
                }
            }
            ToolActionRequest::ExitPowerClipEdit => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.powerclip_edit_id = None;
                }
            }
            ToolActionRequest::OpenInlineTextEdit {
                object_id,
                world_pos_mm,
                is_paragraph,
                box_size_mm,
            } => {
                self.begin_inline_text_edit(object_id, world_pos_mm, is_paragraph, box_size_mm);
            }
            ToolActionRequest::CommitInlineTextEdit => {
                let _ = self.commit_inline_text_edit();
            }
            ToolActionRequest::SmartFillAt { pt_mm } => {
                if let Some(lid) = self.ensure_active_creation_layer() {
                    let created = self.active_tab().and_then(|tab| {
                        create_smart_fill_object(
                            &tab.document,
                            tab.document.active_page_index(),
                            pt_mm,
                            self.default_fill.clone(),
                            self.default_outline.clone(),
                        )
                    });
                    if let Some(obj) = created {
                        match self.execute_command(CommandOp::create_object(obj, lid)) {
                            Ok(()) => {
                                if let Some(tab) = self.active_tab_mut() {
                                    if let Some(new_id) =
                                        tab.document.objects().keys().last().copied()
                                    {
                                        tab.selection.select_single(&tab.document, new_id);
                                        tab.metrics_dirty = true;
                                    }
                                }
                                self.push_toast("Created Smart Fill region", false);
                            }
                            Err(e) => self.push_toast(e, true),
                        }
                    } else {
                        self.push_toast("Click inside an enclosed boundary for Smart Fill", false);
                    }
                }
            }
            ToolActionRequest::ApplyEyedropper {
                pt_mm,
                attributes_mode,
                force_sample,
            } => {
                self.eyedropper_click(pt_mm, attributes_mode, force_sample);
            }
            ToolActionRequest::ApplyRasterStroke {
                tool,
                points_mm,
                samples,
            } => {
                let _ = self.apply_raster_tool_stroke_with_stylus(tool, &points_mm, &samples);
            }
            ToolActionRequest::SetCloneSource(pt) => {
                self.tool_settings.clone_source_mm = Some(pt);
                self.push_toast(
                    format!("Set sample source at ({:.1}, {:.1}) mm", pt.x, pt.y),
                    false,
                );
            }
            ToolActionRequest::StampDuplicateDuringDrag { live_world_affine } => {
                let _ = self.stamp_duplicate_with_affine(live_world_affine);
            }
            ToolActionRequest::ApplyLensToObject { object_id } => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.selection.select_single(&tab.document, object_id);
                }
                let cfg = self.lens_state.build_config();
                self.apply_lens_to_selection(Some(cfg));
            }
            ToolActionRequest::EnsureEnvelopeOnObject { object_id } => {
                let _ = self.ensure_envelope_on_object(object_id);
            }
            ToolActionRequest::MeshFillInteract {
                start_mm,
                end_mm,
                shift_recolor,
            } => {
                let recolor = if shift_recolor {
                    self.default_fill.referenced_colors().into_iter().next()
                } else {
                    None
                };
                self.mesh_fill_interact(start_mm, end_mm, recolor);
            }
            ToolActionRequest::Toast { message, is_error } => {
                self.push_toast(message, is_error);
            }
        }
    }

    /// Audits the active tab for missing font families (`CRLFOM` modal trigger on open/import)
    /// and prepares `PANOSE` perceptual fallback suggestions (`REQ-07`).
    pub fn check_missing_fonts_on_active_tab(&mut self, doc_title: &str) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let rules = PreflightRules {
            check_rgb_in_cmyk: false,
            warn_live_text: false,
            check_missing_fonts: true,
            check_bleed: false,
            check_layer_states: false,
            ..PreflightRules::default()
        };
        let report = run_preflight(&tab.document, &rules);
        let mut missing_families: Vec<String> = Vec::new();
        for issue in report.issues {
            if issue.kind == coree_prod::PreflightIssueKind::MissingFont {
                if let Some(rest) = issue.message.split("references font family '").nth(1) {
                    if let Some((fam, _)) = rest.split_once('\'') {
                        let fam = fam.trim();
                        if !fam.is_empty()
                            && !missing_families
                                .iter()
                                .any(|existing| existing.eq_ignore_ascii_case(fam))
                        {
                            missing_families.push(fam.to_owned());
                        }
                    }
                }
            }
        }
        if missing_families.is_empty() {
            return;
        }
        if self.font_manager_state.indexed_fonts.is_empty() && self.storage.is_some() {
            self.refresh_font_manager(true);
        }
        let mut entries = Vec::with_capacity(missing_families.len());
        for fam in missing_families {
            let stored = self
                .storage
                .as_ref()
                .and_then(|db| db.get_font_substitution(&fam).ok().flatten());
            let is_arabic = fam.to_ascii_lowercase().contains("arab")
                || fam.to_ascii_lowercase().contains("naskh")
                || fam.to_ascii_lowercase().contains("kufi");
            let panose_match = self.find_best_panose_substitute(
                &PanoseClassification::default(),
                400,
                false,
                is_arabic,
            );
            let fallback = if is_arabic {
                "Noto Sans Arabic"
            } else {
                "Noto Sans"
            };
            let suggested = panose_match
                .as_ref()
                .map(|(rec, _)| rec.family_name.clone())
                .unwrap_or_else(|| fallback.to_owned());
            let selected = stored
                .map(|s| s.substitute_family)
                .unwrap_or_else(|| suggested.clone());
            entries.push(MissingFontPromptEntry {
                missing_family: fam,
                suggested_family: suggested,
                selected_family: selected,
                panose_distance: panose_match.map(|(_, d)| d),
            });
        }
        self.missing_font_dialog = MissingFontPromptState {
            open: true,
            document_title: doc_title.to_owned(),
            entries,
            remember_rules: true,
        };
    }

    /// Applies the user's `CRLFOM` modal font substitutions to the active document via `CommandOp`
    /// and optionally persists each rule into SQLite `font_substitutions`.
    pub fn apply_missing_font_dialog_substitutions(&mut self) -> usize {
        let entries = self.missing_font_dialog.entries.clone();
        let remember = self.missing_font_dialog.remember_rules;
        let mut replaced_total = 0usize;
        for entry in &entries {
            let target = entry.selected_family.trim();
            if target.is_empty() {
                continue;
            }
            if remember && self.storage.is_some() {
                let _ = self.set_font_substitution(&entry.missing_family, target, None);
            }
            let query = FindQuery {
                scope: FindScope::WholeDocument,
                object_kind: None,
                fill_color: None,
                outline_color: None,
                outline_width_min_mm: None,
                outline_width_max_mm: None,
                is_hairline: None,
                font_family: Some(entry.missing_family.clone()),
                text_pattern: None,
                lens_kind: None,
                layer_id: None,
                layer_name: None,
            };
            let cmd = CommandOp::FindAndReplace {
                query,
                action: ReplaceAction::ReplaceFontFamily {
                    font_family: target.to_owned(),
                },
            };
            if self.execute_command(cmd).is_ok() {
                replaced_total += 1;
            }
        }
        self.missing_font_dialog.open = false;
        if replaced_total > 0 {
            self.push_toast(
                format!("Substituted fonts in {replaced_total} missing family rule(s)"),
                false,
            );
        }
        replaced_total
    }

    /// Creates a fresh document from a SQLite/built-in template (opening `.cdt` if present on disk,
    /// otherwise creating a configured `Document` with matching dimensions, bleed, color mode & pages).
    pub fn create_document_from_template(&mut self, tpl: &TemplateRecord) {
        if !tpl.template_path.trim().is_empty() {
            let path = Path::new(&tpl.template_path);
            if path.is_file() && self.open_file_path(path).is_ok() {
                return;
            }
        }
        let color_mode = if tpl.color_mode.eq_ignore_ascii_case("rgb") {
            ColorMode::Rgb
        } else {
            ColorMode::Cmyk
        };
        let mut doc = Document::new(
            &tpl.name,
            tpl.width_mm.max(10.0),
            tpl.height_mm.max(10.0),
            color_mode,
            300.0,
        );
        if let Some(p0) = doc.page_mut(0) {
            p0.bleed_mm = tpl.bleed_mm.max(0.0);
        }
        for p_idx in 1..tpl.page_count.max(1) {
            doc.add_page(
                format!("Page {}", p_idx + 1),
                tpl.width_mm.max(10.0),
                tpl.height_mm.max(10.0),
                tpl.bleed_mm.max(0.0),
            );
        }
        self.new_document(doc);
    }

    /// Saves the active document as a `.cdt` template file and registers it in SQLite `templates`.
    pub fn save_active_as_cdt_template(
        &mut self,
        path: &Path,
        category: &str,
        description: &str,
    ) -> Result<(), String> {
        let tpl_rec = {
            let tab = self
                .tabs
                .get(self.active_tab)
                .ok_or_else(|| "no active document".to_owned())?;
            let page0 = tab.document.pages().first();
            coree_io::save_coree(&tab.document, path).map_err(|e| e.to_string())?;
            TemplateRecord {
                id: format!("tpl-{}", Uuid::new_v4()),
                name: tab.document.title().to_owned(),
                category: if category.trim().is_empty() {
                    "custom".to_owned()
                } else {
                    category.trim().to_owned()
                },
                description: description.trim().to_owned(),
                template_path: path.to_string_lossy().into_owned(),
                width_mm: page0.map_or(210.0, |p| p.width_mm),
                height_mm: page0.map_or(297.0, |p| p.height_mm),
                bleed_mm: page0.map_or(3.0, |p| p.bleed_mm),
                page_count: tab.document.pages().len() as u32,
                color_mode: match tab.document.color_mode() {
                    ColorMode::Cmyk => "cmyk",
                    ColorMode::Rgb => "rgb",
                }
                .to_owned(),
                thumbnail_png: None,
                is_builtin: false,
                favorite: true,
            }
        };
        if let Some(ref db) = self.storage {
            db.upsert_template(&tpl_rec).map_err(|e| e.to_string())?;
            if let Ok(list) = db.list_templates(None) {
                self.templates = list;
            }
        } else {
            self.templates.push(tpl_rec);
        }
        self.push_toast(format!("Saved template '{}'", path.display()), false);
        Ok(())
    }
}

/// Built-in offline production templates seeded into SQLite `templates` on first run (`REQ-12`).
fn builtin_default_templates() -> Vec<TemplateRecord> {
    vec![
        TemplateRecord {
            id: "builtin-a4-flyer".to_owned(),
            name: "A4 Print Flyer (CMYK 3mm Bleed)".to_owned(),
            category: "print".to_owned(),
            description: "ISO A4 210x297 mm CMYK press-ready layout with 3 mm bleed.".to_owned(),
            template_path: String::new(),
            width_mm: 210.0,
            height_mm: 297.0,
            bleed_mm: 3.0,
            page_count: 1,
            color_mode: "cmyk".to_owned(),
            thumbnail_png: None,
            is_builtin: true,
            favorite: true,
        },
        TemplateRecord {
            id: "builtin-bizcard".to_owned(),
            name: "Business Card (90x50 mm)".to_owned(),
            category: "print".to_owned(),
            description: "Standard 90x50 mm front & back double-sided business card.".to_owned(),
            template_path: String::new(),
            width_mm: 90.0,
            height_mm: 50.0,
            bleed_mm: 3.0,
            page_count: 2,
            color_mode: "cmyk".to_owned(),
            thumbnail_png: None,
            is_builtin: true,
            favorite: false,
        },
        TemplateRecord {
            id: "builtin-fefco-pkg".to_owned(),
            name: "Packaging Dieline Sheet (450x320 mm)".to_owned(),
            category: "packaging".to_owned(),
            description: "SRA3 450x320 mm packaging & CutContour dieline sheet.".to_owned(),
            template_path: String::new(),
            width_mm: 450.0,
            height_mm: 320.0,
            bleed_mm: 5.0,
            page_count: 1,
            color_mode: "cmyk".to_owned(),
            thumbnail_png: None,
            is_builtin: true,
            favorite: false,
        },
        TemplateRecord {
            id: "builtin-social-rgb".to_owned(),
            name: "Social Square Post (1080px / RGB)".to_owned(),
            category: "digital".to_owned(),
            description: "Square sRGB social media graphic preset.".to_owned(),
            template_path: String::new(),
            width_mm: 91.44,
            height_mm: 91.44,
            bleed_mm: 0.0,
            page_count: 1,
            color_mode: "rgb".to_owned(),
            thumbnail_png: None,
            is_builtin: true,
            favorite: false,
        },
    ]
}

fn build_differential_patch_from_tab(
    tab: &DocumentTab,
    doc_id: &str,
    session_id: String,
    base_snapshot_id: String,
) -> DifferentialSnapshotPatch {
    let mut upserted_objects = Vec::with_capacity(tab.autosave_dirty_objects.len());
    let mut deleted_object_ids = Vec::new();
    for &oid in &tab.autosave_dirty_objects {
        if let Some(obj) = tab.document.object(oid) {
            upserted_objects.push((oid, obj.clone()));
        } else {
            deleted_object_ids.push(oid);
        }
    }
    let struct_dirty = tab.autosave_dirty_structure;
    let pal_dirty = struct_dirty || tab.autosave_dirty_palette;
    let mut upserted_assets = Vec::new();
    let mut deleted_asset_ids = Vec::new();
    if !struct_dirty {
        for &aid in &tab.autosave_upserted_assets {
            if let Some(asset) = tab.document.asset(aid) {
                upserted_assets.push((aid, asset.clone()));
            }
        }
        deleted_asset_ids.extend(tab.autosave_deleted_assets.iter().copied());
    }
    DifferentialSnapshotPatch {
        doc_id: doc_id.to_owned(),
        session_id,
        title: tab.document.title().to_owned(),
        source_path: tab
            .source_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        base_snapshot_id,
        last_command_seq: tab.journal_seq,
        upserted_objects,
        deleted_object_ids,
        upserted_assets,
        deleted_asset_ids,
        full_assets: struct_dirty.then(|| tab.document.assets().clone()),
        pages: struct_dirty.then(|| tab.document.pages().to_vec()),
        active_page_index: struct_dirty.then(|| tab.document.active_page_index()),
        next_id: Some(tab.document.next_id()),
        color_mode: struct_dirty.then(|| tab.document.color_mode()),
        default_unit: struct_dirty.then(|| tab.document.default_unit()),
        dpi: struct_dirty.then(|| tab.document.dpi()),
        multipage_view: struct_dirty.then(|| tab.document.multipage_view()),
        palettes: pal_dirty.then(|| tab.document.palettes().to_vec()),
        document_palette: pal_dirty.then(|| tab.document.document_palette().clone()),
        guides: struct_dirty.then(|| tab.document.guides().to_vec()),
        grid: struct_dirty.then(|| tab.document.grid()),
        perspective_grid: struct_dirty.then(|| tab.document.perspective_grid().clone()),
        color_styles: struct_dirty.then(|| tab.document.color_styles().to_vec()),
        object_styles: struct_dirty.then(|| tab.document.object_styles().to_vec()),
        symbols: struct_dirty.then(|| tab.document.symbols().clone()),
        metadata: struct_dirty.then(|| tab.document.metadata().clone()),
    }
}

fn clear_tab_autosave_dirty(tab: &mut DocumentTab) {
    tab.autosave_dirty_objects.clear();
    tab.autosave_dirty_structure = false;
    tab.autosave_dirty_palette = false;
    tab.autosave_upserted_assets.clear();
    tab.autosave_deleted_assets.clear();
    tab.autosave_dirty_all = false;
    tab.last_autosave_at = Instant::now();
}

fn command_op_touches_structure(op: &CommandOp) -> bool {
    match op {
        CommandOp::TransformObjects { .. }
        | CommandOp::SetFill { .. }
        | CommandOp::SetOutline { .. }
        | CommandOp::EditPath { .. }
        | CommandOp::EditShape { .. }
        | CommandOp::EditBSpline { .. }
        | CommandOp::EditArtisticMedia { .. }
        | CommandOp::EditText { .. }
        | CommandOp::EditTable { .. }
        | CommandOp::EditDimension { .. }
        | CommandOp::SetOpacity { .. }
        | CommandOp::SetBlendMode { .. }
        | CommandOp::SetLens { .. }
        | CommandOp::SetObjectPerspective { .. }
        | CommandOp::SetObjectColorStyleLink { .. }
        | CommandOp::AddEffect { .. }
        | CommandOp::RemoveEffect { .. }
        | CommandOp::SetObjectVisibility { .. }
        | CommandOp::SetObjectLocked { .. }
        | CommandOp::SetObjectPrintable { .. }
        | CommandOp::RenameObject { .. }
        | CommandOp::UpdateRaster { .. } => false,
        CommandOp::Batch(ops) => ops.iter().any(command_op_touches_structure),
        _ => true,
    }
}
