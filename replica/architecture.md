# Architecture: `coree` (100% Pure-Rust, Offline-First Vector & Prepress Studio)

`coree` is a clean-room, MPL-2.0, pure-Rust desktop rebuild of CorelDRAW's production workflows for **Linux first (Wayland + X11)** and **Windows second** — with **zero cloud accounts, zero telemetry, zero subscriptions, and `#![forbid(unsafe_code)]` across every workspace crate**.

---

## 1. Stack

| layer | choice | why |
| --- | --- | --- |
| language & safety | Rust stable (`#![forbid(unsafe_code)]` workspace-wide) | Memory safety for untrusted binary `.cdr`/PDF/SVG parsers, predictable native speed, single-binary distribution |
| desktop UI shell | `eframe` (`glow`, `wayland`, `x11`, `default_fonts`, `persistence`) + `egui` + `egui_dock` | Zero C `-dev` header lock-in on Ubuntu/Linux; native Wayland & X11 + Windows; crisp rearrangeable dockers |
| native file dialogs | `rfd` (`default-features = false, features = ["xdg-portal"]`) | Pure-Rust D-Bus `xdg-desktop-portal` on Linux (no `libgtk-3-dev`), native Win32 dialogs on Windows |
| vector geometry & booleans | `kurbo` + `i_overlay` | Precision cubic Bézier/B-spline math (`f64`) + robust non-zero/even-odd Boolean Weld/Trim/Intersect/Simplify/Boundary |
| spatial indexing | `rstar` (R-Tree) | Sub-millisecond hit-testing, marquee selection, snapping, and viewport dirty-tile culling on 10,000+ objects |
| 2D vector & raster rendering | `tiny-skia` + `rayon` dirty-tile pipeline behind `CanvasBackend` trait | Deterministic CPU rendering of paths, mesh fills, PowerClips, all 11 Lenses, and Photo Studio pixels with multi-core tile parallelism |
| typography & Arabic RTL | `cosmic-text` (Rustybuzz/HarfBuzz shaping + Swash + Unicode Bidi) | Native Arabic/RTL shaping, Kashida justification, OpenType features, variable fonts, and paragraph frame flow without `libfontconfig-dev` |
| binary `.cdr` & codecs | `zip` (`deflate`), `flate2`, `byteorder`, `encoding_rs`, `usvg`, `resvg`, `image`, `qrcode` | Pure-Rust `.cdr`/`.cdt`/`.cmx`/`.cpt` RIFF & ZIP parsing with Unknown-Chunk Passthrough, CP1256 legacy decoding, CMYK PDF/X, SVG, EPS, DXF, HPGL `.plt` |
| local database | `rusqlite` (`features = ["bundled"]`) in WAL mode | Zero-config single-file SQLite database for recent files, crash-safe command journal, autosave snapshots, palettes, templates, fonts, and preferences |
| scripting & macros | `rhai` (sandboxed, op-capped) + `serde`/`serde_json` | Cross-platform Macro Recorder and script runner generated directly from serializable `DocumentCommand` logs (replaces Windows COM/VBA) |
| CLI & batch tooling | `clap` (`derive`) + `tempfile` + `dirs` | Headless `.cdr` inspection, preflight, and PDF/SVG/DXF/PLT batch export; atomic temp-file saves |
| network / cloud / auth | **None (explicitly prohibited)** | Zero HTTP clients, zero login flows, zero license checks, zero telemetry sockets |

---

## 2. 7-Crate Workspace Topology

Strict acyclic dependency graph (all 7 crates enforce `#![forbid(unsafe_code)]`):

```
                        +-----------------+
                        |   coree-app     |  (binary: `coree`, CLI + eframe/egui/egui_dock UI S01-S18)
                        +---+----+----+---+
                            |    |    |
            +---------------+    |    +------------------+
            |                    |                       |
            v                    v                       v
   +----------------+   +----------------+      +----------------+
   |  coree-render  |   |   coree-prod   |      | coree-storage  |
   | (CanvasBackend,|   | (Imposition,   |      | (SQLite WAL,   |
   |  11 Lenses,    |   |  Preflight,    |      |  Autosave,     |
   |  Photo Studio, |   |  CutContour,   |      |  Journal,      |
   |  PowerTRACE)   |   |  Dielines,     |      |  Palettes,     |
   +-------+--------+   |  Barcodes,     |      |  Font Index)   |
           |            |  PrintMerge,   |      +-------+--------+
           |            |  PathMeter,    |              |
           |            |  Rhai Macros)  |              |
           |            +----+-----+-----+              |
           |                 |     |                    |
           |        +--------+     +---------+          |
           |        v                        v          |
           |  +-------------+         +-------------+   |
           |  |  coree-io   |         |  coree-cdr  |   |
           |  | (PDF/X, SVG,|         | (.cdr/.cdt/ |   |
           |  |  EPS, DXF,  |         |  .cmx/.cpt, |   |
           |  |  PLT, PNG/  |         |  Unknown-   |   |
           |  |  TIFF/PSD)  |         |  Chunk Pass,|   |
           |  +------+------+         |  Inspector) |   |
           |         |                +------+------+   |
           |         |                       |          |
           +---------+-----------+-----------+----------+
                                 |
                                 v
                        +-----------------+
                        |   coree-core    |  (Document, SceneGraph, kurbo, i_overlay, rstar,
                        |                 |   cosmic-text RTL/Kashida, Color/CMYK/Spot,
                        |                 |   11 Lenses/Effects AST, DocumentCommand)
                        +-----------------+
```

### Crate Ownership & Boundaries

| crate | owns | must not depend on |
| --- | --- | --- |
| `coree-core` | Pure domain model: `Document`, `Page`, `Layer`, `SceneNode`, `BezPath` (`kurbo`), Boolean ops (`i_overlay`), `SpatialIndex` (`rstar`), `ColorSpec` (CMYK/RGB/Lab/Spot + ICC metadata), `TextModel` (`cosmic-text` RTL/Kashida), `EffectSpec`, `LensSpec` (all 11 Lenses), `TableModel`, `DimensionModel`, `UnknownChunkMap`, and `DocumentCommand` + `CommandHistory`. | Any UI, SQLite, filesystem I/O, or rendering backend |
| `coree-cdr` | Clean-room `.cdr`/`.cdt`/`.cmx`/`.cpt` reader & writer: RIFF container (v3–v13) & ZIP XML/binary container (v14/X4–v25+), `encoding_rs` legacy codepage decoder (CP1256, CP1251, CP1250, CP1252), **Unknown-Chunk Passthrough** (`raw_chunks` byte-exact retention keyed by node/document ID), `CdrInspectTree` for `S16`, strict Zip-Bomb/Zip-Slip/depth guards. | `egui`, `eframe`, `rusqlite`, `tiny-skia` |
| `coree-io` | Production import/export codecs: native CMYK & Spot `PDF/X-1a` / `PDF/X-4` writer + PDF importer, SVG (`usvg`/`resvg`), EPS/PostScript, CAD DXF (R12/R2000+), cutting-plotter HPGL `.plt`, raster formats (`PNG`, `JPEG`, `WebP`, `TIFF`, `BMP`, `PSD` layers), plus atomic temp-file save helper (`atomic_write`). | `egui`, `eframe`, `rusqlite` |
| `coree-storage` | Offline SQLite persistence (`rusqlite` `bundled`, WAL mode), schema bootstrap from `replica/schema.sql`, `CommandJournalStore`, `AutosaveManager` & crash recovery replay, `PaletteStore`, `TemplateStore`, `WorkspaceLayoutStore`, `FontIndexStore`, `BrushPresetStore`, `PreferencesStore`. | `egui`, `eframe`, `tiny-skia` |
| `coree-render` | `CanvasBackend` trait & `TileSkiaBackend` (Rayon multi-threaded 256×256 dirty-tile rasterizer via `tiny-skia`), R-Tree viewport culling, all 11 Lens shaders (with `Frozen`, `Viewpoint`, `Remove Face`), Mesh Fill tessellation, PowerClip clipping, Overprint/Wireframe/Pixels view modes, integrated Photo Studio pixel layer/mask/brush/tone engine, and PowerTRACE bitmap-to-vector tracer. | `egui`, `eframe`, `rusqlite` |
| `coree-prod` | Prepress & production pipelines: Imposition engine (N-Up, Booklet, Perfect Bound, Step & Repeat), Printer's Marks generator, CMYK+Spot Color Separations, Document Preflight rules engine, Collect for Output bundler, CSV/TSV Print Merge, vector Barcode/QR generator, `CutContour`/`PerfCutContour`/`Crease` presets, Parametric FEFCO Packaging Dielines, Live Path Meter calculator, and sandboxed Rhai Macro Recorder / Runner. | `egui`, `eframe` |
| `coree-app` | Native Linux (Wayland/X11) & Windows `eframe`/`egui`/`egui_dock` application shell implementing `S01`–`S18`, design tokens from `replica/design/tokens.json`, Toolbox + Flyouts, Property Bar, Rulers/Guides, Page Tabs, Dockers, Color Palette Bar, Command Palette (`Ctrl+K`), CDR Inspector, `rfd` (`xdg-portal`) dialogs, and `clap` headless CLI mode. | Zero direct `unsafe` code or network crates |

---

## 3. Core Trait Boundaries

### 3.1 `CanvasBackend` (`coree-render`)

Decouples `coree-app` and `coree-prod` from the underlying rasterizer so CPU multi-threaded `tiny-skia` tiles run everywhere today (including headless CI & software OpenGL/Wayland) while permitting an optional GPU backend later without touching scene or UI code:

```rust
pub trait CanvasBackend: Send + Sync {
    /// Synchronize mutated scene nodes and invalidate only intersecting 256x256 tiles.
    fn sync_document(&mut self, doc: &Document, dirty_regions: &[RectMm]);

    /// Render the visible viewport (or a print/export tile) into an RGBA8 pixel buffer.
    fn render_viewport(
        &mut self,
        doc: &Document,
        request: &ViewportRenderRequest, // camera transform, dpi, view_mode, plate_filter, overprint_sim
    ) -> Result<RenderedFrame, RenderError>;

    /// Render a single CMYK or Spot separation plate as 8-bit halftone/density channel.
    fn render_separation_plate(
        &mut self,
        doc: &Document,
        page_id: PageId,
        plate: &SeparationPlate, // Cyan | Magenta | Yellow | Black | Spot(String)
        dpi: f32,
    ) -> Result<GrayPlateBuffer, RenderError>;

    /// Evaluate an interactive Lens (any of the 11 LensKinds) over underlying layers.
    fn evaluate_lens(
        &self,
        doc: &Document,
        lens_node_id: NodeId,
        viewport_dpi: f32,
    ) -> Result<TileRgba, RenderError>;

    /// Accelerated hit-test against the backend's synchronized R-Tree + exact path bounds.
    fn hit_test(&self, doc: &Document, page_id: PageId, pt_mm: PointMm, tolerance_mm: f64) -> Vec<NodeId>;

    /// Clear cached tiles (e.g., on ICC profile change or memory pressure).
    fn purge_tile_cache(&mut self);
}
```

### 3.2 `DocumentCommand` (`coree-core`)

Every document edit — from mouse drags to Boolean Weld, PowerClip edits, Lens parameter tweaks, RTL Kashida changes, and Rhai scripts — executes exclusively as a serializable `DocumentCommand`. This guarantees deterministic undo/redo, SQLite WAL crash replay, and automatic Rhai macro recording from a single source of truth:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "args")]
pub enum DocumentCommand {
    Batch { label: String, commands: Vec<DocumentCommand> },
    CreateNode { layer_id: LayerId, node: SceneNode, z_index: usize },
    DeleteNodes { node_ids: Vec<NodeId> },
    TransformNodes { node_ids: Vec<NodeId>, affine: [f64; 6] },
    EditPathNodes { node_id: NodeId, new_path: BezPathData },
    BooleanOp { op: BooleanOpKind, target_ids: Vec<NodeId> },
    SetFill { node_ids: Vec<NodeId>, fill: FillSpec },
    SetOutline { node_ids: Vec<NodeId>, outline: OutlineSpec },
    SetTextRun { node_id: NodeId, text: TextModel },
    FitTextToPath { text_node_id: NodeId, path_node_id: NodeId, params: FitPathParams },
    PlaceInPowerClip { content_ids: Vec<NodeId>, container_id: NodeId },
    SetEffectStack { node_id: NodeId, effects: Vec<EffectSpec> },
    SetLens { node_id: NodeId, lens: Option<LensSpec> },
    AddPage { index: usize, spec: PageSpec },
    ReorderPages { new_order: Vec<PageId> },
    UpdateLayer { layer_id: LayerId, patch: LayerPatch },
    ApplyRasterStroke { node_id: NodeId, stroke_patch: RasterTileDiff },
    InsertDieline { page_id: PageId, template: FefcoSpec },
    InsertBarcode { layer_id: LayerId, spec: BarcodeSpec },
}

pub trait CommandExecutor {
    /// Validates and executes the command, returning the inverse command + dirty bounds.
    fn execute(&mut self, cmd: &DocumentCommand) -> Result<CommandOutcome, CommandError>;

    /// Emits a clean, human-readable Rhai statement for the Macro Recorder (S17).
    fn to_rhai(&self, cmd: &DocumentCommand) -> Option<String>;
}
```

### 3.3 `DocumentCodec` (`coree-cdr` & `coree-io`)

```rust
pub trait DocumentCodec: Send + Sync {
    fn format_id(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    fn read(&self, bytes: &[u8], opts: &ImportOptions) -> Result<ImportReport, CodecError>;
    fn write(&self, doc: &Document, opts: &ExportOptions) -> Result<Vec<u8>, CodecError>;
}
```

---

## 4. Schema

Tables: **10**. Access rules: **100% single-user local offline SQLite** (`$XDG_DATA_HOME/coree/coree.db` on Linux, `%APPDATA%\coree\coree.db` on Windows) with OS filesystem `0600` permissions, WAL journaling, strict `PRAGMA foreign_keys = ON`, and `CHECK` constraints on every enum/range column.

See complete validated DDL in [schema.sql](file:///run/media/ahmed/DATA/coreldraw%20me/replica/schema.sql):
`recent_documents`, `autosave_snapshots`, `command_journal`, `custom_palettes`, `palette_swatches`, `templates`, `workspace_layouts`, `font_index`, `brush_presets`, `app_preferences`.

---

## 5. Internal Command & Action API (Offline Local Dispatch)

Because `coree` has **zero HTTP servers and zero remote APIs**, all actions execute locally via the `AppDispatcher` / `DocumentCommand` bus (and headless CLI `coree <subcommand>`):

| action / CLI command | does | crate | input | output | flow |
| --- | --- | --- | --- | --- | --- |
| `doc::create` | Creates blank or template document & registers WAL session | `coree-core`, `coree-storage` | `PageSpec`, `ColorMode`, optional `template_id` | `DocumentId`, initial `AutosaveSnapshot` | F01 |
| `doc::dispatch(cmd)` | Applies `DocumentCommand`, updates R-Tree, appends SQLite WAL journal | `coree-core`, `coree-storage` | `DocumentCommand` | `CommandOutcome` (inverse + `RectMm` dirty bounds) | F02–F08 |
| `page::manage` | Adds, resizes, reorders pages or edits Master Layers in Multipage View | `coree-core` | `PageOp` | Updated `Page` list + dirty rects | F03 |
| `shape::boolean` | Runs `i_overlay` Weld / Trim / Intersect / Simplify / Boundary | `coree-core` | `BooleanOpKind`, `Vec<NodeId>` | Result `SceneNode` + inverse snapshot | F02 |
| `text::shape_bidi` | Shapes UTF-8 / Arabic RTL / Kashida text runs & Fit Text to Path | `coree-core` | `TextModel`, `FontIndex` | Shaped glyph runs + outline curves | F05 |
| `lens::evaluate` | Evaluates any of 11 Lenses with `Frozen`, `Viewpoint`, `Remove Face` | `coree-render` | `NodeId`, `LensSpec` | Cached RGBA lens buffer | F08 |
| `photo::commit_session` | Commits Photo Studio pixel layers/masks/tone adjustments to bitmap node | `coree-render`, `coree-core` | `RasterSessionId` | `ApplyRasterStroke` command + tile diff | F09 |
| `trace::run_powertrace` | Runs Quick/Centerline/Outline bitmap vectorization on background thread | `coree-render` | `BitmapId`, `TraceSettings` | Preview paths + commit `CreateNode` batch | F10 |
| `prod::impose_preview` | Computes N-Up / Booklet / Step-Repeat sheets, Printer's Marks, Separations | `coree-prod`, `coree-render` | `Document`, `ImpositionSpec` | Sheet preview frames + plate buffers | F11 |
| `prod::preflight` | Runs commercial press rules (DPI, RGB-in-CMYK, hairlines, fonts, bleed) | `coree-prod` | `Document`, `PreflightProfile` | `Vec<PreflightIssue>` with offending `NodeId` | F12 |
| `prod::collect_output` | Bundles `.cdr`/`.coree`, PDF/X, linked images, used fonts & report | `coree-prod`, `coree-io` | `Document`, target dir | Atomic bundle manifest | F12 |
| `prod::dieline_and_meter` | Generates parametric FEFCO dielines (`CutContour`/`Crease`) & Path Meter | `coree-prod` | `FefcoSpec` / `Selection` | Dieline nodes + `PathMeterStats` (mm, nodes, est. time) | F13 |
| `prod::barcode_and_merge` | Generates vector EAN/UPC/Code128/QR & executes streaming CSV Print Merge | `coree-prod`, `coree-io` | `BarcodeSpec` / CSV path | Vector barcode group or merged PDF/pages | F14 |
| `cdr::open_inspect` | Parses `.cdr`/`.cdt`/`.cmx`/`.cpt` with safety limits, CP1256, Unknown-Chunk map | `coree-cdr` | File path, `ImportOptions` | `Document` + `CdrInspectTree` + warnings | F15 |
| `io::export_atomic` | Exports PDF/X, SVG, EPS, DXF, PLT, PNG/TIFF, or round-trip `.cdr` atomically | `coree-io`, `coree-cdr` | `Document`, `ExportOptions`, path | Atomic file write + byte count | F16 |
| `macro::record_run` | Exports recorded `DocumentCommand` sequence to `.rhai` and runs sandboxed | `coree-prod` | `.rhai` source, `Document` | `Vec<DocumentCommand>` batch or line error | F17 |
| `storage::recover_crash` | Replays SQLite WAL `command_journal` over last `autosave_snapshots` blob | `coree-storage`, `coree-core` | `AutosaveSnapshotId` | Restored `Document` state | F18 |
| `cli: coree convert / inspect / preflight` | Headless batch conversion, CDR chunk dump, and CI preflight checks | `coree-app` | CLI flags (`clap`) | Exit code `0` + JSON/file output | F12, F15, F16 |

**Webhooks in / out:** None (strictly offline desktop application).
**Background local threads:**
- `tile-render-pool` (Rayon): parallel 256×256 dirty-tile rasterization, cancellable on viewport pan/zoom.
- `autosave-flusher` (every 30 s idle or 25 commands): writes compact binary snapshot atomically and marks WAL checkpoint.
- `font-indexer` (on startup + manual refresh): scans OS font directories incrementally using `file_mtime_epoch` to populate `font_index` with Arabic RTL/Kashida coverage flags.
- `trace-worker` (on demand): asynchronous PowerTRACE preview computation without blocking UI thread.

---

## 6. The Parts That Bite

- **Binary `.cdr` version sprawl & undocumented chunks:** CorelDRAW spans RIFF binary formats (v3–v13) and ZIP-packaged XML/binary containers (v14/X4–v25+). Solution: parse known geometry/fill/text/lens chunks cleanly and store every unrecognized chunk verbatim in `UnknownChunkMap` keyed by parent ID so round-trip saving preserves commercial shop files byte-for-byte, with full visibility in `S16` CDR Inspector.
- **Hostile file parser safety (Zip-Bomb, Zip-Slip, recursion):** Untrusted `.cdr`/`.svg`/`.pdf` files from clients can contain 10 GB zip bombs, `../../` entry paths, or 10,000-deep nested groups/PowerClips. Solution: `#![forbid(unsafe_code)]`, hard decompression cap (`512 MB` total / `100:1` ratio), strict canonical member name validation, and max AST/group/PowerClip nesting depth (`64`).
- **Floating-point robustness in Boolean & Offset operations:** Self-intersecting curves and near-collinear segments break naive polygon clippers. Solution: convert `kurbo` curves at adaptive flatness (`0.005 mm`) into integer-grid fixed-point paths for `i_overlay`, then refit smooth cubics cleanly.
- **Arabic RTL, Kashida & legacy CP1256 decoding:** Legacy pre-X4 `.cdr` files store single-byte CP1256/CP1251 strings rather than UTF-8, and naive font rendering disconnects Arabic glyphs. Solution: explicit codepage selector (`encoding_rs`) on import + `cosmic-text` HarfBuzz-grade shaping, Unicode Bidi (`UAX #9`), and explicit Kashida (`U+0640`) distribution algorithm for paragraph justification.
- **CMYK & Spot color fidelity on RGB monitors:** Displays are RGB while print shops require exact CMYK values (`0–100%`) and named Spot channels (`CutContour`). Solution: store canonical source color values untouched in `ColorSpec` (never destructive RGB round-trips!), soft-proof on screen via ICC LUTs, and emit exact device CMYK + Separation color spaces in PDF/X-1a/4.
- **60 FPS responsiveness on 10,000+ object layouts:** Re-rasterizing full pages per pointer drag causes severe lag. Solution: `rstar` R-Tree spatial culling + 256×256 tile cache where only tiles intersecting mutated bounding boxes re-render via Rayon, plus immediate wireframe/transform preview while dragging.
- **Crash safety & atomic disk writes:** Crashes mid-save truncate original `.cdr` files (a top complaint in `reviews.csv`). Solution: SQLite WAL journals every `DocumentCommand`, and every file save writes to a `NamedTempFile` in the same directory, calls `sync_all()`, and atomically renames over the target.
- **Linux zero-`-dev`-header buildability:** Target Ubuntu machines often lack `libgtk-3-dev`/`libfontconfig-dev`. Solution: pure-Rust `eframe` + `rfd` (`xdg-portal`) + `rusqlite` (`bundled`) + `cosmic-text` direct font file scanning.

---

## 7. Build Order

1. **Milestone 1 — Vertical Slice (`S01`, `S02`, `S15`, `S17`):**
   - Crates: `coree-core`, `coree-storage`, `coree-render`, `coree-io`, `coree-app`.
   - End-to-end loop: launch `coree` offline -> create CMYK page -> draw Rect/Ellipse/Bézier -> apply Fill/Outline -> pan/zoom at 60 FPS via `TileSkiaBackend` -> Undo/Redo logged to SQLite WAL -> atomic SVG/PDF/PNG export.
2. **Milestone 2 — Core Vector, Pages, Shaping, Typography & Codecs (`S03`, `S04`, `S05`, `S06`, `S07`, `S15`, `S16`):**
   - Complete Multipage View + Master Layers, Shape node editor, Boolean engine (`i_overlay`), PowerClip, Mesh/Fountain Fills, Color Palette Bar, Arabic RTL/Kashida + CP1256 decoder, `.cdr`/`.cdt` RIFF + ZIP codec with Unknown-Chunk Passthrough + CDR Inspector.
3. **Milestone 3 — Interactive Effects, All 11 Lenses, Photo Studio & PowerTRACE (`S08`, `S09`, `S10`, `S11`):**
   - Drop/Inner/Block Shadow, Contour, Blend, Envelope, Distort, Extrude, Transparency; all 11 Lenses (`Brighten` through `Wireframe` + `Frozen`/`Viewpoint`/`Remove Face`); integrated Photo Studio pixel layers/masks/brushes/adjustments; PowerTRACE Outline & Centerline vectorizer.
4. **Milestone 4 — Prepress, Packaging, CutContour, Automation & Polish (`S12`, `S13`, `S14`, `S17`, `S18`):**
   - Imposition, Printer's Marks, CMYK+Spot Separations, Preflight + Collect for Output, Parametric FEFCO Dielines, `CutContour` + Live Path Meter, Barcode/QR, CSV Print Merge, DXF & HPGL `.plt`, Rhai Macro Recorder, Preferences/ICC & Command Palette (`Ctrl+K`), visual & parity QA.
