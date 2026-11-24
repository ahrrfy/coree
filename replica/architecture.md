# Architecture: `coree` (100% Pure-Rust, Offline-First Vector & Prepress Studio)

`coree` is a clean-room, `MIT OR Apache-2.0`, pure-Rust desktop vector graphics, multipage layout, raster studio, and prepress/laser production suite for **Linux first (Wayland + X11)** and **Windows second** — with **zero cloud accounts, zero telemetry, zero subscriptions, and `#![forbid(unsafe_code)]` across every workspace crate and integration test**.

---

## 1. Stack (Active `v0.3.1` Post-`W1.C` Architecture & Planned `W2`–`W5` Extensions)

| layer | active `v0.3.1` choice (`Cargo.toml`) | why |
| --- | --- | --- |
| language & safety | Rust stable (`#![forbid(unsafe_code)]` across all crates, bins, `build.rs`, & tests) | Memory safety for untrusted binary `.cdr`/PDF/SVG parsers, predictable native speed, single-binary distribution |
| scene, command & spatial kernel | `coree-core` (`kurbo` `0.13`, `i_overlay` `9.2`, `rstar` `0.14`, `rhai` `1.27`) | Encapsulated `Document` AST, `66`-variant `CommandOp` + `CommandHistory` (crate-private `DocumentDelta` & `pub DocumentInvalidation`), `Viewport`, `hit_test`/`SnapEngine`, and non-cloning `preview_find_and_replace` |
| headless interaction kernel | `coree-tools` (depends solely on `coree-core` + `serde`, zero `egui`/`coree-render`) | `ToolCore` & `ToolHandler` state machines across all `65` tools emitting `TransientOverlay` previews and atomic `CommandOp`s with zero mid-drag `Document` mutation |
| desktop UI shell | `eframe` (`0.37`, features: `glow`, `wayland`, `x11`, `default_fonts`, `persistence`) + `egui` (`0.37`) + `egui_dock` (`0.22`) | Zero C `-dev` header lock-in on Ubuntu/Linux; native Wayland & X11 + Windows; rearrangeable dockers |
| native file dialogs | `rfd` (`0.18`, `default-features = false, features = ["xdg-portal"]`) | Pure-Rust D-Bus `xdg-desktop-portal` on Linux (no `libgtk-3-dev`), native Win32 dialogs on Windows |
| vector geometry & booleans | `kurbo` (`0.13`) + `i_overlay` (`9.2`) | Precision cubic Bézier/B-spline math (`f64`) + robust non-zero/even-odd Boolean Weld/Trim/Intersect/Simplify/Boundary |
| spatial indexing | `rstar` (`0.14`) | Sub-millisecond hit-testing, marquee selection, snapping, and viewport dirty-tile culling on 10,000+ objects |
| 2D vector & raster rendering | `tiny-skia` (`0.12`) + `rayon` (`1.12`) `DirtyTileCache` behind `CanvasBackend` trait (`coree-render`) | Deterministic `TinySkiaBackend` CPU reference renderer with multi-core targeted dirty-tile invalidation today; `WgpuBackend` (`wgpu`) planned for W3 |
| typography & Arabic RTL | `cosmic-text` (`0.19`, in `coree-render`) + `unicode-bidi` (`0.3.18`, in `coree-app`) | HarfBuzz/Swash Arabic RTL shaping, pre-shaping Kashida (`apply_kashida_elongation`; post-shaping per-glyph Kashida planned `W3`), OpenType features, and mixed-script UI BiDi reordering without `libfontconfig-dev` |
| binary `.cdr` & codecs | `zip` (`9.8`), `flate2` (`1.1`), `byteorder` (`1.5`), `encoding_rs` (`0.8`), `usvg`/`resvg` (`0.50`), `image` (`0.25`), `qrcode` (`0.15`); `coree-io` depends on `coree-core` + `coree-render` | Pure-Rust `.cdr`/`.cdt`/`.cmx`/`.cpt` RIFF & ZIP parsing with Unknown-Chunk retention, CP1256 legacy decoding, print-ready `DeviceCMYK` PDF 1.7 (`coree-render` glyph outliner; strict `PDF/X-1a/4` + CID subsetting planned `W4`), SVG, EPS, DXF, HPGL `.plt`, PSD import (PSD export planned `W4`) |
| local database | `rusqlite` (`0.43`, `features = ["bundled"]`) in WAL mode (`coree-storage`) | Zero-config single-file SQLite database for recents, crash-safe command journal, gzipped autosave snapshots, palettes, templates, fonts, and preferences |
| scripting & macros | `rhai` (`1.27`, sandboxed) + `serde`/`serde_json` (`1.0`) | Cross-platform Macro Recorder and script runner generated from serializable `CommandOp` logs via `CommandHistory::stop_macro_to_rhai` / `run_rhai_macro` |
| CLI & batch tooling | `clap` (`4.6`, `derive`) + `tempfile` (`3.27`) + `dirs` (`7.0`) | Dual binaries (`coree` GUI+CLI + `coree-cli` / Windows `coree.com`) with 11 headless subcommands + `--diagnostics` / `--new-window` |
| network / cloud / auth | **None (explicitly prohibited)** | Zero HTTP/TLS/DNS/async-network crates (enforced by `scripts/verify_invariants.py`) |

*(Planned offline permissive additions across `W2`–`W5`: `quick-xml`/`roxmltree` for ZCF XML, `wgpu` for GPU tile/canvas backend, `arboard` for OS vector clipboard, `ttf-parser`/`subsetter` for PDF `/Type0` CID font subsetting, and `criterion`/`proptest` for offline benchmarks & fuzzing.)*

---

## 2. Workspace Topology & 6-Layer Clean-Room Kernel Mapping

**Active `v0.3.1` Topology (8 Workspace Crates in `Cargo.toml`):** Strict acyclic dependency graph where every crate, binary, `build.rs`, and integration test enforces `#![forbid(unsafe_code)]`. `coree-tools` is active in `Cargo.toml` (`MND-02`, Layer 3 `ToolCore`), depending **solely on `coree-core` + `serde`** with zero `egui`/`winit`/`coree-render` dependencies.

```
                        +--------------------+
                        |     coree-app      |  (binaries: `coree` + `coree-cli` [Win: `coree.com`];
                        |                    |   eframe/egui/egui_dock UI S01-S18, unicode-bidi)
                        +-+---+----+----+--+-+--+
                          |   |    |    |  |    |
         +----------------+   |    |    |  |    +-----------------------+
         |         +----------+    |    |  +--------------+             |
         v         v               v    v                 v             v
+--------------+ +------------+ +-----------+ +--------------+ +----------+ +------------+
|  coree-io    | |coree-render| |coree-prod | |coree-storage | |coree-cdr | |coree-tools |
| (PDF 1.7,    | |(Canvas-    | |(Imposition| | (SQLite WAL, | |(.cdr/.cdt| |(ToolCore,  |
|  SVG, EPS,   | | Backend,   | | Preflight,| |  StorageDb,  | | .cmx/.cpt| | ToolOutput,|
|  DXF, PLT,   | | TinySkia-  | | Collect,  | |  SCHEMA_SQL, | | Unknown- | | Transient- |
|  PSD import, | | Backend,   | | Dielines, | |  JobPool,    | | Chunk,   | | Overlay,   |
|  atomic I/O) | | DirtyTile- | | Barcodes, | |  font_index) | | Inspector| | 65 tools)  |
|              | | Cache,     | | PrintMerge| |              | |          | |            |
|              | | TextEngine | | coree-cql,| |              | |          | |            |
|              | | cosmic-text| | Rhai/Laser| |              | |          | |            |
+------+-------+ +-----+------+ +-----+-----+ +------+-------+ +----+-----+ +-----+------+
       |               ^              |              |              |             |
       +---------------+              |              |              |             |
       |  (coree-io -> coree-render)  |              |              |             |
       v                              v              v              v             v
     +----------------------------------------------------------------------------------+
     |                                    coree-core                                    |
     | (Encapsulated Document, Page, Layer, SceneObject, kurbo, i_overlay, rstar,       |
     |  Color/CMYK/Spot/ICC, Text AST, Effect/Lens AST, VectorTable, Dimension/Connector|
     |  Viewport, hit_test/SnapEngine, CommandOp [66 variants] + CommandHistory +       |
     |  crate-private DocumentDelta + DocumentInvalidation + preview_find_and_replace)  |
     +----------------------------------------------------------------------------------+
```

### 6-Layer Clean-Room Atomic Kernel Mapping (`W1`–`W5`)

| kernel layer | crate(s) | active `v0.3.1` implementation & planned `W2`–`W5` extensions |
| --- | --- | --- |
| 1. Scene, Relational & Command Kernel | `coree-core` | Encapsulated `Document -> Page -> Layer -> SceneObject -> BezierPath -> Subpath -> PathNode`, `CommandOp` (**66 variants**) + `CommandHistory` (crate-private `DocumentDelta`, `pub DocumentInvalidation::All \| Targeted`, `run_rhai_macro`), `Viewport`, `hit_test`/`SnapEngine`, `preview_find_and_replace`, `ICC v2/v4`, `boolean::BooleanOp`, `geometry::fit_cubic_bezier_subpath`; *Planned `W2`*: relational dependency cleanup, `BSpline`/`Clone`/3×3 perspective |
| 2. Headless Interaction Kernel | `coree-tools` *(active)* | `ToolCore` & `ToolHandler` across all `65` `ToolId`s, `ToolReadContext`, `TransientOverlay`, `ToolOutput`, `ToolActionRequest`, 5-mode geometric snap, zero mid-drag `Document` mutation, zero `egui`/`coree-render` dependencies |
| 3. 2D/GPU Rendering & Compute Kernel | `coree-render` | `CanvasBackend` (`TinySkiaBackend`), Rayon `DirtyTileCache` (`TILE_SIZE_PX = 256`, targeted dirty-rect invalidation), `TextEngine` (`cosmic-text` + `apply_kashida_elongation`), 11 Lenses, Effects, `compute_smart_fill`, `trace_raster`/`trace_raster_to_mm`, `raster_engine` (re-exporting `coree_core::{viewport, hit_test}`); *Planned `W3`–`W4`*: `WgpuBackend`, world-space `DirtyTileCache`, Coons `MeshFill`, 16-bit CoW Photo Studio, topological `Potrace`, post-shaping Kashida |
| 4. Universal Vector & Print I/O Kernel | `coree-io` | Native `.coree`, PDF/AI import + PDF 1.7 (`DeviceCMYK`/`/OutputIntents`/`/SMask`), EPS, SVG, DXF/PLT, PSD import, raster/palette codecs, `atomic_write_bytes`; *Planned `W4`*: unified `ExportDisplayList` (`CdrPsi`), `/Type0` CID font subsetting, strict `PDF/X-1a:2001`/`PDF/X-4`, `.dwg`/`.wmf`/`.emf`/`.acb`, PSD export |
| 5. Prepress, CQL & Automation Kernel | `coree-prod` | Separations (`compute_separations_with_text_outliner`), Imposition, Preflight, `collect_for_output` (SFNT `name`), Barcodes (`BarcodeSymbology`), Print Merge, Dielines, Laser/Plugins (`PluginRegistry`), `coree-cql` (`find_replace.rs`), Rhai macros (`run_macro`); *Planned `W4`*: `Trapping`, `Halftone`, `PerfectBound`, 30+ FEFCO, 2D barcodes, streaming Print Merge, Rhai Hierarchical Scene DOM |
| 6. Binary CDR/ZCF & Local Persistence Shell | `coree-cdr`, `coree-storage`, `coree-app` | RIFF/ZCF/CDT/CMX/CPT codecs + `CdrInspectionReport`, `StorageDb` (inline `SCHEMA_SQL` SQLite WAL + gzipped autosave) + `JobPool`, `eframe`/`egui` desktop shell (`coree` + `coree-cli`) wired through `ToolCore` + `TransientOverlay`, loopback TCP `ipc.rs`, `printing.rs`; *Planned `W2`–`W5`*: UDS/NamedPipes IPC, async differential autosave, live Vector Clipboard, `20B` reverse-`loda` & `CDRO` ZCF writer, `I18nKey` + RTL shell mirroring |

### Crate Ownership & Boundaries

| crate | owns | must not depend on |
| --- | --- | --- |
| `coree-core` | Leaf domain model: encapsulated `Document`, `Page`, `Layer`, `SceneObject`, `BezierPath` (`kurbo`), Boolean ops (`i_overlay`), `SpatialIndex` (`rstar`), `Viewport`, `hit_test`/`SnapEngine`, `Color` + `IccProfile`/`SoftProofLut3D`, text AST (`ArtisticText`, `ParagraphText`, `TextOnPath` — no `cosmic-text`), `Effect`, `LensConfig`/`LensKind`, `VectorTable`, `DimensionObject`/`ConnectorObject`, `Vec<UnknownCdrChunk>`, `brand`, `preview_find_and_replace`, and `CommandOp` (66 variants) + `CommandHistory` (crate-private `DocumentDelta`, `pub DocumentInvalidation`) + `run_rhai_macro` (`rhai`). | `egui`/`eframe`, `rusqlite`, `tiny-skia`, `cosmic-text`, or filesystem I/O |
| `coree-tools` | `egui`-independent interactive tool state machines (`ToolCore`, `ToolHandler`, `ToolReadContext`, `TransientOverlay`, `ToolOutput`, `ToolActionRequest`, pointer/key transitions across 65 `ToolId`s emitting atomic `CommandOp`s with zero mid-drag `Document` mutation). | `egui`/`eframe`, `winit`, `tiny-skia`, `rusqlite`, `coree-render`, `coree-io` |
| `coree-cdr` | Clean-room `.cdr`/`.cdt`/`.cmx`/`.cpt` reader & writer: RIFF (`v4–v13` / X3; `v3–v5` 16-bit planned `W2`) & ZIP/ZCF (`X4–2025`), `encoding_rs` legacy decoder (`CP1256`/`CP1251`/UTF-16LE), `UnknownCdrChunk` passthrough, `CdrInspectionReport` (`inspect_cdr_*`), `import_cdr_*`, `export_cdr*`, `detect_format*`, `CdrFormat`, strict Zip-Bomb/Zip-Slip/depth guards. | `egui`, `eframe`, `rusqlite`, `tiny-skia` |
| `coree-io` | Production import/export codecs: `.coree` (`native.rs`), `pdf.rs` (`DeviceCMYK`/Spot/`/OutputIntents`/`/SMask`; strict `PDF/X-1a/4` + CID subsetting planned `W4`), `pdf_import.rs`, `svg.rs` (`usvg`/`resvg`), `eps.rs`, `cad_plot.rs` (DXF/PLT), `psd.rs` (import today; export planned `W4`), `raster.rs`, `palette_io.rs`, plus `atomic.rs` (`atomic_write_bytes`). Depends on `coree-core` + `coree-render` (for glyph outlining & isolated object rasterization). | `egui`, `eframe`, `rusqlite`, `coree-cdr` |
| `coree-storage` | Offline SQLite persistence (`rusqlite` `bundled`, WAL mode), inline `SCHEMA_SQL` constant (`SCHEMA_VERSION = 1`) in `db.rs` mirroring `replica/schema.sql` (`10` tables + `schema_migrations`, Unix `0o700`/`0o600` permissions): `StorageDb`, `JobPool` (`JobHandle<T>`, `JobContext`, `JobState`, `JobOutcome<T>`), `AutosaveSnapshotInfo`, `UncleanSession`, `JournalEntry`, `RecentDocumentRecord`, `CustomPaletteRecord`, `PaletteSwatchRecord`, `TemplateRecord`, `WorkspaceLayoutRecord`, `FontRecord`, `ParsedFontFace`, `FontQuery`, `FontScanReport`, `BrushPresetRecord`, `AppPreferenceRecord`, `StorageError`/`StorageResult`. | `egui`, `eframe`, `tiny-skia`, `coree-cdr`, `coree-render`, `coree-io`, `coree-prod` |
| `coree-render` | `CanvasBackend` trait & `TinySkiaBackend` (`WgpuBackend` planned `W3`), `DirtyTileCache` (`TILE_SIZE_PX = 256`, Rayon parallel dirty tiles), `SceneRenderer`, `TextEngine` (`cosmic-text` HarfBuzz/Swash Arabic RTL shaping, BiDi, `apply_kashida_elongation`, glyph-to-curve outlining), all 11 Lenses, vector Effects, Mesh Fill, PowerClip clipping, `raster_engine`, `trace` (`trace_raster`, `trace_raster_to_mm`, `TraceConfig`, `TraceMode`, `OutlinePreset`), `smart_fill` (`compute_smart_fill`, `create_smart_fill_object`), and re-exports of `coree_core::{viewport, hit_test}`. | `egui`, `eframe`, `rusqlite`, `coree-cdr`, `coree-io`, `coree-prod` |
| `coree-prod` | Prepress & production pipelines: Imposition (`ImpositionMode`), Printer's Marks, Separations (`compute_separations_with_text_outliner` via injected closure from `coree-app` + direct `tiny-skia`/`image`), Preflight, `collect_for_output` (SFNT `name` table reader), Print Merge, Barcodes (`BarcodeSymbology`, `QrCodeOptions`), `CutContour`/`Crease`, FEFCO Dielines (`DielineKind`), Live Path Meter (`ProductionMetrics`), `coree-cql` (`find_replace.rs`), `PluginRegistry` (7 built-in Laser/Plotter bridge presets via `builtin_presets()`), and sandboxed Rhai macros (`run_macro`). | `egui`, `eframe`, `coree-render`, `coree-io`, `coree-cdr`, `coree-storage` |
| `coree-app` | Dual binaries (`coree` GUI+CLI & `coree-cli` / Windows `coree.com`), `eframe`/`egui`/`egui_dock` shell (`S01`–`S18`), `unicode-bidi` UI run reordering (`theme.rs`), `ui/`, `state/`, `cdr_bridge.rs`, `io_bridge.rs`, `ipc.rs`, `printing.rs`, `rfd` (`xdg-portal`) dialogs, `ToolCore` pointer/key routing + `TransientOverlay` painting, and `clap` headless CLI. | Zero `unsafe` code, zero direct mid-drag `Document` mutations, or network crates |

---

## 3. Core Trait & API Boundaries

### 3.1 `CanvasBackend` (`crates/coree-render/src/backend.rs`)

Decouples `SceneRenderer` and `DirtyTileCache` from the underlying rasterizer so deterministic `TinySkiaBackend` runs everywhere today (including headless CI & software OpenGL/Wayland) while permitting `WgpuBackend` in W3 without changing scene or UI code:

```rust
pub trait CanvasBackend {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn clear(&mut self, color: Color);
    fn set_flatten_tolerance(&mut self, tolerance: Option<f64>);
    fn fill_path(&mut self, path: &BezierPath, paint: &BackendPaint, rule: FillRule, transform: Affine2D);
    fn stroke_path(&mut self, path: &BezierPath, stroke: &StrokeParams, transform: Affine2D);
    fn draw_pixmap(&mut self, image: &RgbaBuffer, transform: Affine2D, opacity: f32, blend_mode: BlendMode, smooth: bool);
    fn push_clip(&mut self, path: &BezierPath, rule: FillRule, transform: Affine2D);
    fn pop_clip(&mut self);
    fn read_region(&self, rect: PixelRect) -> RgbaBuffer;
    fn composite_region(&mut self, rect: PixelRect, src: &RgbaBuffer, opacity: f32, blend_mode: BlendMode);
    fn snapshot_rgba(&self) -> RgbaBuffer;
    fn into_rgba(self) -> RgbaBuffer;
}
```

Higher-level document rendering, `SoftProofLut3D` caching, and optical lenses are implemented in `SceneRenderer` (`renderer.rs`) and `DirtyTileCache` (`tile_cache.rs`) on top of `CanvasBackend`, while `Viewport` and spatial `hit_test`/`SnapEngine` reside in `coree-core` (re-exported in `coree-render`). `DirtyTileCache` receives targeted world dirty-rect invalidations via `DocumentInvalidation` on commit/undo/redo, while live drag previews render in `coree-tools::TransientOverlay` above cached tiles with zero mid-drag cache invalidation.

### 3.2 `CommandOp`, `CommandHistory` & `coree-tools::ToolCore` (`crates/coree-core/src/command.rs`, `crates/coree-tools/`)

Every document edit executes through `CommandOp` (**66 variants**) and `CommandHistory` (with transactional grouping, crate-private `DocumentDelta`/`ObjectDelta`, `pub DocumentInvalidation` (`All` vs `Targeted { object_ids, dirty_rects_mm }`), JSON command logs, WAL journal persistence, asset GC, and Rhai macro generation). All interactive pointer and keyboard gestures flow through `coree-tools::ToolCore` + `ToolReadContext<'a>` -> `ToolOutput` (`Continue { overlay }` / `Commit { ops, .. }` / `Cancel` / `Consumed`):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CommandOp {
    CreateObject { object: Box<SceneObject>, layer_id: Option<LayerId> },
    DeleteObjects { ids: Vec<ObjectId> },
    TransformObjects { ids: Vec<ObjectId>, affine: Affine2D },
    SetFill { ids: Vec<ObjectId>, fill: Fill },
    SetOutline { ids: Vec<ObjectId>, outline: OutlineStyle },
    SetLens { ids: Vec<ObjectId>, lens: Option<LensConfig> },
    AddEffect { id: ObjectId, effect: Effect },
    RemoveEffect { id: ObjectId, effect_index: usize },
    EditPath { id: ObjectId, path: BezierPath },
    BooleanOp { op: BooleanOp, ids: Vec<ObjectId> },
    Group { ids: Vec<ObjectId>, name: String },
    Ungroup { group_id: ObjectId },
    SetZOrder { id: ObjectId, op: ZOrderOp },
    AlignObjects { ids: Vec<ObjectId>, op: AlignOp },
    DistributeObjects { ids: Vec<ObjectId>, op: DistributeOp },
    AddPage { name: String, width_mm: f64, height_mm: f64, bleed_mm: f64 },
    DeletePage { page_id: PageId },
    RenamePage { page_id: PageId, new_name: String },
    AddLayer { page_id: PageId, name: String, is_cut_layer: bool, master_scope: MasterLayerScope },
    SetLayerFlags { layer_id: LayerId, visible: Option<bool>, locked: Option<bool>, printable: Option<bool>, is_cut_layer: Option<bool> },
    AddGuide { orientation: GuideOrientation, locked: bool },
    MoveGuide { id: u64, orientation: GuideOrientation },
    RemoveGuide { id: u64 },
    ToggleGuideLock { id: u64 },
    EditText { id: ObjectId, payload: NodePayload },
    EditTable { id: ObjectId, table: VectorTable },
    CreatePowerClip { container_id: ObjectId, content_ids: Vec<ObjectId> },
    ExtractPowerClip { powerclip_id: ObjectId },
    SetPowerClipLock { id: ObjectId, lock_contents: bool },
    MoveToDesktopLayer { id: ObjectId },
    ConvertToCurves { id: ObjectId },
    ConvertOutlineToObject { id: ObjectId },
    UpdateRaster { id: ObjectId, data: RasterLayerData },
    // Extended W1.B–W1.C variants (66 total):
    DuplicateObjects { .. }, SetObjectVisibility { .. }, SetObjectLocked { .. },
    SetObjectPrintable { .. }, RenameObject { .. }, SetOpacity { .. }, SetBlendMode { .. },
    SetDocumentProperties { .. }, SetPageProperties { .. }, ReorderPages { .. },
    RenameLayer { .. }, ReorderLayerObjects { .. }, SetGridConfig { .. },
    SetPerspectiveGrid { .. }, UpsertColorStyle { .. }, DeleteColorStyle { .. },
    UpsertObjectStyle { .. }, DeleteObjectStyle { .. }, SetObjectStyleLink { .. },
    UpsertSymbol { .. }, DeleteSymbol { .. }, InstantiateSymbol { .. },
    UnlinkSymbolInstance { .. }, FitPowerClip { .. }, FindAndReplace { .. },
    EditShape { .. }, DeleteLayer { .. }, RegisterAsset { .. },
    CreateDetachedObject { .. }, EditDimension { .. }, PruneAssets { .. },
    ReplaceObjectInParent { .. },
    Batch(Vec<CommandOp>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum DocumentInvalidation {
    All,
    Targeted { object_ids: Vec<ObjectId>, dirty_rects_mm: Vec<Rect2D> },
}

impl CommandHistory {
    pub fn execute(&mut self, doc: &mut Document, op: CommandOp) -> CoreResult<()>;
    pub fn undo(&mut self, doc: &mut Document) -> CoreResult<String>;
    pub fn redo(&mut self, doc: &mut Document) -> CoreResult<String>;
    pub fn last_invalidation(&self) -> &DocumentInvalidation;
    // + transactions, asset GC, JSON replay, and Rhai macro recording
}
```

### 3.3 Codec Entrypoints (`coree-cdr` & `coree-io`)

Rather than a trait object, `coree-cdr` and `coree-io` expose strongly typed pure-Rust free functions with explicit security/resource limits (`CodecLimits`) and atomic filesystem writes:

- **`coree-cdr` (`crates/coree-cdr/src/lib.rs`):** `detect_format`, `detect_format_with_limits`, `import_cdr_bytes`, `import_cdr_bytes_with_limits`, `import_cdr_file`, `import_cdr_file_with_limits` (returning `CdrResult<ImportReport>` under `CodecLimits`), `export_cdr_bytes`, `export_cdr`, `export_cdr_riff_uncompressed`, `CdrWriteVersion`, `cdt::{export_cdt*, import_cdt_bytes}`, `cmx::{export_cmx*, import_cmx_bytes}`, `coree_cdr::cpt::import_cpt_bytes`, `inspector::{inspect_cdr_bytes*, inspect_cdr_file*, CdrInspectionReport, format_report}`, `extract_cdr_preview_png`.
- **`coree-io` (`crates/coree-io/src/lib.rs`):** `native::{load_coree*, save_coree*, read_coree_*}`, `pdf::{export_pdf*, PdfExportOptions}`, `pdf_import::{import_pdf*}`, `svg::{export_svg*, import_svg*}`, `eps::{export_eps*, import_eps*}`, `cad_plot::{export_dxf*, import_dxf*, export_plt*, import_plt*}`, `psd::{import_psd*}` (export planned `W4`), `raster::{export_raster*, import_raster*, decode_raster_bytes_with_limits}`, `palette_io::{import_*, export_*}`, `atomic::{atomic_write_bytes, atomic_write_with}`.

---

## 4. Schema

Tables: **10** (plus `schema_migrations` with `SCHEMA_VERSION = 1`). Access rules: **100% single-user local offline SQLite** (`$XDG_DATA_HOME/coree/coree.db` on Linux, `%LOCALAPPDATA%\coree\coree.db` on Windows via `dirs::data_local_dir()/coree/coree.db`) with OS filesystem `0o700` directory / `0o600` file permissions, WAL journaling (`PRAGMA journal_mode = WAL`, `PRAGMA busy_timeout = 5000`, `PRAGMA synchronous = NORMAL`), strict `PRAGMA foreign_keys = ON`, and `CHECK` constraints on every enum/range column.

`crates/coree-storage/src/db.rs` embeds the complete 10-table DDL verbatim as an inline `pub const SCHEMA_SQL: &str` literal mirroring [schema.sql](schema.sql):
`recent_documents`, `autosave_snapshots`, `command_journal`, `custom_palettes`, `palette_swatches`, `templates`, `workspace_layouts`, `font_index`, `brush_presets`, `app_preferences`.

---

## 5. Internal Command & Action API (Offline Local Dispatch)

Because `coree` has **zero HTTP servers and zero remote APIs**, all actions execute locally via `StudioApp` / `CommandHistory::execute(doc, CommandOp)` and the headless CLI (`coree <subcommand>` / `coree-cli <subcommand>`):

| action / CLI command | does | crate | input | output | flow |
| --- | --- | --- | --- | --- | --- |
| `doc::create` | Creates blank or template document & registers WAL session | `coree-core`, `coree-storage` | `Document` / template, `ColorMode` | `Document`, initial gzipped `AutosaveSnapshotInfo` | F01 |
| `doc::dispatch` | Applies `CommandOp` via `CommandHistory::execute`, updates R-Tree & `DocumentInvalidation`, appends SQLite WAL journal | `coree-core`, `coree-storage` | `CommandOp` | `CoreResult<()>` + `pub CommandHistory::last_invalidation(&self) -> &DocumentInvalidation` | F02–F08 |
| `tool::dispatch` | Processes pointer/key events through `ToolCore`, emits `TransientOverlay` previews mid-drag and atomic `CommandOp`s on commit | `coree-tools`, `coree-core` | `ToolReadContext`, `PointerEvent` / `KeyEvent` | `ToolOutput` (`Continue` / `Commit` / `Cancel` / `Consumed`) | F01–F10 |
| `page::manage` | Adds, deletes, renames, or updates pages and Master Layers in Multipage View | `coree-core` | `CommandOp::{AddPage, DeletePage, RenamePage, SetLayerFlags}` | Updated `Page` list + dirty rects | F03 |
| `shape::boolean` | Runs `i_overlay` Weld / Trim / Intersect / Simplify / Boundary + curve refit | `coree-core` | `BooleanOp`, `Vec<ObjectId>` | Result `SceneObject` + delta | F02 |
| `text::shape_bidi` | Shapes UTF-8 / Arabic RTL / Kashida text runs, glyph outliner & Fit Text to Path | `coree-render` (`TextEngine`) | `ArtisticText` / `ParagraphText` / `TextOnPath` + `fontdb::Database` | Shaped glyph runs + outline curves | F05 |
| `lens::evaluate` | Evaluates any of 11 Lenses with `LensConfig` (`frozen`, `viewpoint`, `remove_face`) | `coree-render` | `ObjectId`, `LensConfig` | Cached RGBA lens buffer | F08 |
| `photo::commit_session` | Commits Photo Studio pixel layers/masks/tone adjustments to bitmap object | `coree-render`, `coree-core` | `CommandOp::UpdateRaster` | Updated `RasterAsset` / `RasterLayerData` + delta | F09 |
| `trace::vectorize` | Runs Quick/Outline/Centerline bitmap vectorization (`trace_raster` / `trace_raster_to_mm`; topological `Potrace` planned `W4`) | `coree-render` | `TraceConfig` | `Vec<(BezierPath, Color)>` + commit `CommandOp::CreateObject` | F10 |
| `prod::impose_preview` | Computes N-Up / Booklet / Step-Repeat sheets, Marks, Separations (`compute_separations_with_text_outliner` with `coree-app`-injected `TextEngine` outliner) | `coree-prod` | `ImpositionMode` / `SeparationOptions` | Sheet preview frames + plate buffers | F11 |
| `prod::preflight` | Runs commercial press rules (DPI, RGB-in-CMYK, hairlines, fonts, bleed) | `coree-prod` | `PreflightRules` | `PreflightReport` with offending `ObjectId`s | F12 |
| `prod::collect_output` | Bundles `.coree`, images, SFNT `name`-matched fonts & preflight report | `coree-prod` (`collect_for_output`) | `Document`, target dir | `CollectReport` + bundled files | F12 |
| `prod::dieline_and_meter` | Generates parametric FEFCO dielines (`CutContour`/`Crease`) & Path Meter | `coree-prod` | `DielineKind` / `Selection` | Dieline objects + `ProductionMetrics` | F13 |
| `prod::barcode_and_merge` | Generates vector EAN/UPC/Code128/Code39/QR & executes CSV Print Merge (`DataMatrix`/`PDF417`/`GS1-128`/`Aztec` & streaming 10k-row Print Merge planned `W4`) | `coree-prod` | `BarcodeSymbology` / `QrCodeOptions` / CSV rows | Vector barcode group or merged documents | F14 |
| `cdr::open_inspect` | Parses `.cdr`/`.cdt`/`.cmx`/`.cpt` with safety limits, CP1256, `UnknownCdrChunk` passthrough | `coree-cdr` | File path, `CodecLimits` | `ImportReport` + `CdrInspectionReport` + warnings | F15 |
| `io::export_atomic` | Exports PDF 1.7 (strict `PDF/X-1a/4` planned `W4`), SVG, EPS, DXF, PLT, PNG/TIFF, or round-trip `.cdr` atomically | `coree-io`, `coree-cdr` | `Document`, export options, path | Atomic file write (`atomic_write_bytes`) + byte count | F16 |
| `macro::record_run` | Exports recorded `CommandOp` sequence to `.rhai` and runs sandboxed | `coree-core`, `coree-prod` | `.rhai` source, `Document` | `CoreResult<usize>` (`coree-core::run_rhai_macro`) / `ProdResult<MacroExecutionResult>` (`coree-prod::run_macro`) | F17 |
| `storage::recover_crash` | Replays SQLite WAL `command_journal` over last gzipped `autosave_snapshots` blob | `coree-storage`, `coree-core` | Session / snapshot ID | Restored `Document` state | F18 |
| `cli: coree / coree-cli` | 11 subcommands: `generate-sample`, `inspect`, `convert`, `render`, `thumbnail`, `laser`, `preflight`, `print-merge`, `collect`, `trace`, `run-macro` + `--diagnostics`, `--new-window` | `coree-app` | CLI flags (`clap`) | Exit code `0` + JSON/file output | F12, F15, F16 |

**Webhooks in / out:** None (strictly offline desktop application).
**Background local execution (`v0.3.1` & Planned `W1`–`W4`):**
- `tile-render-pool` (Rayon `par_iter` in `DirtyTileCache::render_dirty_tiles`): parallel `256×256` (`TILE_SIZE_PX`) dirty-tile rasterization.
- `JobPool` (`coree-job-{idx}` Rayon pool in `coree-storage/src/jobs.rs`): non-blocking background jobs with cooperative cancellation & progress (`export_active_async`).
- `autosave` (`StudioApp::tick_autosave` every `30 s` idle on UI tick calling `StorageDb::save_snapshot` atomically to `.snapshot.json.gz`; async background differential autosave planned `W1`).
- `font-indexer` (`StorageDb::scan_system_fonts` / `scan_font_directories` on startup + manual refresh): scans OS font dirs incrementally using `file_mtime_epoch` to populate `font_index` with Arabic RTL/Kashida coverage flags; `TextEngine` shares `shared_base_font_db()` via `OnceLock<(String, cosmic_text::fontdb::Database)>`.
- `trace` (`trace_selected_raster` executes `trace_raster_to_mm` synchronously today; offloading trace preview onto `JobPool` + topological `Potrace` planned `W4`).

---

## 6. The Parts That Bite

- **Binary `.cdr` version sprawl & undocumented chunks:** `.cdr` spans RIFF binary formats (`v4–v13` / X3; `v3–v5` 16-bit planned `W2`) and ZIP-packaged XML/binary containers (v14/X4–v25+). Solution: parse known geometry/fill/text/lens chunks cleanly and store every unrecognized chunk verbatim in `Vec<UnknownCdrChunk>` (`SceneObject::unknown_chunks` & `Document::unknown_doc_chunks`) so round-trip saving preserves commercial shop files byte-for-byte, with full visibility in `S16` CDR Inspector.
- **Hostile file parser safety (Zip-Bomb, Zip-Slip, recursion):** Untrusted `.cdr`/`.svg`/`.pdf` files from clients can contain 10 GB zip bombs, `../../` entry paths, or 10,000-deep nested groups/PowerClips. Solution: `#![forbid(unsafe_code)]`, hard decompression cap (`512 MB` total / `100:1` ratio), strict canonical member name validation, and max AST/group/PowerClip nesting depth (`64`).
- **Floating-point robustness in Boolean & Offset operations:** Self-intersecting curves and near-collinear segments break naive polygon clippers. Solution: convert `kurbo` curves at flatness tolerance (`DEFAULT_FLATTEN_TOLERANCE_MM = 0.02 mm`) into integer-grid fixed-point paths for `i_overlay`, then refit smooth cubics cleanly via Philip Schneider curve re-fitting (`geometry::fit_cubic_bezier_subpath`, `0.025 mm` tolerance) preserving sharp corners.
- **Arabic RTL, Kashida & legacy CP1256 decoding:** Legacy pre-X4 `.cdr` files store single-byte CP1256/CP1251 strings rather than UTF-8, and naive font rendering disconnects Arabic glyphs. Solution: explicit codepage selector (`encoding_rs`) on import + `cosmic-text` HarfBuzz-grade shaping, Unicode Bidi (`UAX #9` + `unicode-bidi` in UI), and Kashida (`U+0640`) elongation (`apply_kashida_elongation` pre-shaping today; post-shaping per-glyph Kashida planned `W3`).
- **CMYK & Spot color fidelity on RGB monitors:** Displays are RGB while print shops require exact CMYK values (`0–100%`) and named Spot channels (`CutContour`). Solution: store canonical source color values untouched in `Color` (never destructive RGB round-trips!), soft-proof on screen via cached `SoftProofLut3D` & ICC v2/v4 `mAB`/`mBA` CLUTs, and emit exact `DeviceCMYK` + `Separation` color spaces in PDF 1.7 (strict `PDF/X-1a/4` planned `W4`).
- **60 FPS responsiveness on 10,000+ object layouts:** Re-rasterizing full pages per pointer drag causes severe lag. Solution: `rstar` R-Tree spatial culling + 256×256 `DirtyTileCache` invalidated selectively via `DocumentInvalidation::Targeted` (only tiles intersecting mutated `before`/`after` world bounds re-render via Rayon), while live drag previews render in `coree-tools::TransientOverlay` above cached tiles with zero mid-drag `Document` mutation or tile cache invalidation.
- **Crash safety & atomic disk writes:** Crashes mid-save truncate original `.cdr` files (a top complaint in `reviews.csv`). Solution: SQLite WAL journals every `CommandOp`, and every file save writes to a `NamedTempFile` in the same directory, calls `sync_all()`, and atomically renames over the target (`atomic_write_bytes`).
- **Linux zero-`-dev`-header buildability:** Target Ubuntu machines often lack `libgtk-3-dev`/`libfontconfig-dev`. Solution: pure-Rust `eframe` + `rfd` (`xdg-portal`) + `rusqlite` (`bundled`) + `cosmic-text` direct font file scanning.

---

## 7. Build Order

1. **Milestone 1 — Vertical Slice (`S01`, `S02`, `S15`, `S17`) + Wave `W1.A`–`W1.C` Kernel Hardening:**
   - Crates (`8`): `coree-core`, `coree-tools`, `coree-storage`, `coree-render`, `coree-io`, `coree-prod`, `coree-cdr`, `coree-app`.
   - End-to-end loop: launch `coree` offline -> create CMYK page -> draw Rect/Ellipse/Bézier via `ToolCore` + `TransientOverlay` -> apply Fill/Outline -> pan/zoom at 60 FPS via `TinySkiaBackend` & targeted `DocumentInvalidation` (`CommandHistory::last_invalidation`) -> Undo/Redo (`66 CommandOp` variants, encapsulated `Document`) logged to SQLite WAL -> atomic SVG/PDF/PNG export.
2. **Milestone 2 — Core Vector, Pages, Shaping, Typography & Codecs (`S03`, `S04`, `S05`, `S06`, `S07`, `S15`, `S16`):**
   - Complete Multipage View + Master Layers, Shape node editor, Boolean engine (`i_overlay`), PowerClip, Mesh/Fountain Fills, Color Palette Bar, Arabic RTL/Kashida + CP1256 decoder, `.cdr`/`.cdt` RIFF + ZIP codec with Unknown-Chunk Passthrough + CDR Inspector.
3. **Milestone 3 — Interactive Effects, All 11 Lenses, Photo Studio & Vector Trace (`S08`, `S09`, `S10`, `S11`):**
   - Drop/Inner/Block Shadow, Contour, Blend, Envelope, Distort, Extrude, Transparency; all 11 Lenses (`Brighten` through `Wireframe` + `Frozen`/`Viewpoint`/`Remove Face`); integrated Photo Studio pixel layers/masks/brushes/adjustments; Outline & Centerline bitmap-to-vector tracer.
4. **Milestone 4 — Prepress, Packaging, CutContour, Automation & Polish (`S12`, `S13`, `S14`, `S17`, `S18`):**
   - Imposition, Printer's Marks, CMYK+Spot Separations, Preflight + Collect for Output, Parametric FEFCO Dielines, `CutContour` + Live Path Meter, Barcode/QR, CSV Print Merge, DXF & HPGL `.plt`, Rhai Macro Recorder, Preferences/ICC & Command Palette (`Ctrl+K`), visual & parity QA.
