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
mod session_io;
mod tab;
mod templates_autosave;
mod tool_dispatch;
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
    pub show_rulers: bool,
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
        let mut template_errors = Vec::new();
        let defaults = templates_autosave::builtin_default_templates();
        if let Some(ref db) = storage {
            let mut repaired = false;
            for tpl in &defaults {
                if !templates.iter().any(|existing| existing.id == tpl.id) {
                    match db.upsert_template(tpl) {
                        Ok(()) => repaired = true,
                        Err(err) => template_errors.push(format!(
                            "Failed to store built-in template '{}': {err}",
                            tpl.id
                        )),
                    }
                }
            }
            if repaired || templates.is_empty() {
                templates = db.list_templates(None).unwrap_or(defaults);
            }
        } else if templates.is_empty() {
            templates = defaults;
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
            show_rulers: true,
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

        for err in template_errors {
            app.push_toast(err, true);
        }

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

    /// Clears unpinned recent documents from SQLite and refreshes `recent_docs`.
    pub fn clear_recent_docs(&mut self) {
        if let Some(ref db) = self.storage {
            let _ = db.clear_unpinned_recent_documents();
        }
        self.refresh_storage_lists();
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
}
