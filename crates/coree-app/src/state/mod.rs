// SPDX-License-Identifier: MIT OR Apache-2.0

//! Complete headless-drivable studio application state, multi-document tab manager,
//! all 15 Toolbox Flyout groups, rearrangeable `egui_dock` layout state, SQLite
//! autosave/WAL crash recovery, `JobPool` background jobs, and studio actions.

use crate::cdr_bridge::{inspect_any_file, inspect_in_memory_document, FileInspectionReport};
use crate::io_bridge::{
    export_any_document, import_into_document, open_any_document, UniversalExportOptions,
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
    Effect, EllipseMode, EnvelopeMode, FeatherDirection, Fill, GuideOrientation, ImageAssetId,
    ImpactStyle, LayerId, LensConfig, LensKind, MasterLayerScope, MeshNode, NodeKind, NodePayload,
    NodeRef, ObjectId, OutlineStyle, PageId, ParametricShape, PathNode, Point2D, RasterAsset,
    RasterFilter, RasterLayerData, Rect2D, RenderingIntent, SceneObject, SelectionState, Subpath,
    TextOnPath, TextStyle, TransparencyMode, Unit, Vec2D, VectorTable, ZOrderOp,
};
use coree_io::import_palette_file;
use coree_prod::{
    add_barcode_to_document, add_qr_code_to_document, apply_print_merge, builtin_macros,
    collect_for_output, compute_production_metrics, compute_separations, detect_executable,
    export_for_plugin, find_and_replace, find_objects, generate_cut_contour, generate_dieline,
    generate_print_marks, impose_document, launch_external_bridge, plan_laser_toolpath, run_macro,
    run_preflight, run_recorded_macro, BarcodeOptions, BarcodeSymbology, CollectOptions,
    DielineKind, FindQuery, FindReplaceReport, FindScope, ImpositionMode, LaserJobConfig,
    LaserToolpathPlan, LensKindFilter, MacroSandboxConfig, ObjectKindFilter, PluginExportOutcome,
    PluginKind, PluginRegistry, PreflightReport, PreflightRules, PrintMarksOptions,
    PrintMergeOptions, PrintMergeTable, ProductionMetrics, QrCodeOptions, QrErrorCorrection,
    ReplaceAction, SeparationOptions, SeparationResult, TextMatchMode,
};
use coree_render::{
    apply_dodge_burn_stroke, apply_healing_stroke, clone_stamp_stroke, compute_page_layouts,
    convert_object_to_bitmap, erase_stroke, hit_test_point, hit_test_powerclip_contents,
    lower_effects_to_primitives, paint_brush_stroke, paint_brush_stroke_masked,
    paragraph_to_curves, raster_flood_fill, resample, smudge_stroke, text_on_path_to_curves,
    text_to_curves, trace_raster_to_mm, DirtyTileCache, DodgeBurnMode, DynamicGuideLine,
    GlyphBlockFilter, OutlinePreset, RasterMask, RenderOptions, SceneRenderer, SnapOptions,
    ToneRange, TraceConfig, TraceMode, ViewMode, Viewport,
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

mod canvas_tools;
mod editing;
mod helpers;
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
    pub roots: Vec<ObjectId>,
    pub document: Document,
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

    // Studio Viewport & Options State
    pub show_options_dialog: bool,
    pub options_tab: usize,
    pub full_screen_preview: bool,
    pub treat_all_objects_as_filled: bool,
    pub object_origin_anchor: (u8, u8),

    // ICC Soft-Proofing & Gamut Warning
    pub soft_proof_cmyk: bool,
    pub gamut_warning: bool,
    pub proof_profile: BuiltinIccProfile,
    pub proof_intent: RenderingIntent,

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
    pub clipboard_objects: Vec<SceneObject>,
    pub clipboard_snapshot: Option<ClipboardSnapshot>,

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

        let job_pool = JobPool::default_pool().ok();

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
            laser_config: LaserJobConfig::default(),
            plugin_registry: PluginRegistry::with_builtins(),
            plugin_exec_overrides: BTreeMap::new(),
            laser_plan_preview: None,
            show_laser_toolpath_overlay: false,
            last_plugin_export_path: None,
            glyph_block_filter: GlyphBlockFilter::default(),
            glyph_search: String::new(),
            last_executed_op: None,
            clipboard_objects: Vec::new(),
            clipboard_snapshot: None,
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
        };
        let toolbox_json = serde_json::to_string(&toolbox_cfg).unwrap_or_else(|_| "{}".to_owned());
        let shortcuts_json =
            serde_json::to_string(&self.custom_shortcuts).unwrap_or_else(|_| "{}".to_owned());
        if let Some(ref mut db) = self.storage {
            for tab in &self.tabs {
                let _ = db.mark_clean_close(&tab.document.id.to_string());
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
            CommandOp::TransformObjects { ids, .. } if ids.is_empty() => {}
            _ => {
                tab.last_duplicate_batch = None;
            }
        }

        if let Some(ref db) = self.storage {
            let _ = db.append_command(&doc_id, seq, &op);
        }
        self.last_executed_op = Some(op);
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
            tab.last_duplicate_batch = None;
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
            tab.last_duplicate_batch = None;
            tab.mark_modified();
            label
        };
        self.checkpoint_wal_snapshot_active();
        self.push_toast(format!("Redo: {label}"), false);
        Ok(label)
    }
}
