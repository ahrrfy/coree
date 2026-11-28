# Changelog

All notable changes to **`coree`** are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1] - 2026-11-28

### Fixed & Added (Hardware Resource Governor, Ultra-Smooth Vector Trace, Tier-5 Hardening & Deep Parity — `BUG-106`–`BUG-116`)
- **Adaptive Hardware Resource Governor & Multi-Page Fluidity (`coree-core`, `coree-render`, `coree-app`):**
  - Added cross-platform `HostResourceProfile::probe()` and `ResourceBudget::compute()` (`Auto`, `EcoLowMemory`, `Balanced`, `MaxPerformance`) adapting worker threads, LRU world-tile budgets, GPU tile texture pools, Undo depth, and node-overlay budgets automatically to any host machine.
  - Added direct GPU `256×256` tile texture compositing (`paint_viewport_tiles_gpu`), multi-page `WorldTileKey.page_scope` LRU retention for zero-rerender page switching, `RwLock`-cached `SpatialIndex` hit-testing, viewport + `5×5 px` screen-cell LOD node decimation for `40k–200k+` node paths, scrollable multi-page jump bar (`1..500+` pages), and non-blocking async file Open/Import/Trace/Metrics with cancelable HUD.
- **Precision Vector Trace, UX Polish & Cross-Platform BiDi/IPC (`coree-render`, `coree-io`, `coree-app`, Packaging):**
  - Upgraded Vector Trace with Taubin curvature-preserving polyline fairing, Philip Schneider iterative Newton-Raphson cubic Bézier fitting, Otsu anti-halo foreground clustering, and recursive extraction of raster images inside PDF `PowerClip`/Group containers.
  - Replaced horizontal toolbar/vertical toolbox scrollbars with responsive wrapping and compact 2-column classic layout, fixed per-section UAX #9 Arabic RTL/BiDi live `TextEdit` shaping across both dedicated (`Noto Sans Arabic`) and combined Latin+Arabic (`Arial`/`Tahoma`/`Segoe UI`/`DejaVu Sans`) system fonts, reconciled SVG 2 `<tspan opacity>` on system-matched fonts, added `O(W × H)` 3-pass premultiplied box blur for large radii, replaced Windows PowerShell NamedPipe IPC with a zero-hang atomic spool maildrop, and shipped the Win32 `IThumbnailProvider` COM Shell Extension (`coree_thumb.dll`).
- **Geometry, Curves & Scene Graph (`coree-core`):**
  - Implemented Philip Schneider cubic Bézier curve re-fitting (`fit_cubic_bezier_subpath`) preserving sharp corners across `boolean.rs` and `erase_circle`, and fixed the `erase_circle` open-subpath tail drop/duplication bug (`BUG-106`).
  - Aligned `build_spatial_index` and `hit_test` with `MasterLayerScope::{OddPages,EvenPages}`, resolved composed ancestor `world_xf` on `ConnectorObject` endpoints and `convert_to_curves_with_outliner`, and added undo-safe `CommandHistory::gc_document_assets` garbage collection (`BUG-107`).
- **ICC v4 Color Management & Rendering Engine (`coree-core`, `coree-render`):**
  - Implemented arbitrary-dimension ICC v4 `mAB`/`mBA` CLUT parsing (`IccV4Lut`) with `RenderingIntent` and media white-point adaptation, and cached `SoftProofLut3D` inside `SceneRenderer` (`BUG-108`).
  - Added live text effects (`render_text_pieces_with_effects`), 999-step multi-subpath `Blend`, C¹ Catmull-Rom 10×10 `MeshFill`, `960px` curve-refit `SmartFill`, shaped `measure_run_advance_mm` Kashida + `TextAlign::ForceJustifyKashida`, and `Frozen` lens pre-warm enabling parallel Rayon tile rendering (`BUG-111`).
- **CDR Codec & Compressed Autosave (`coree-cdr`, `coree-storage`):**
  - Removed wrong-asset fallback on missing `bmp_ref` in `cdr_riff.rs`, preserved unrecognized nested `lgob`/`pwrp` `LIST` chunks in `unknown_chunks`, and rendered real `64×64` document previews in ZIP `thumbnail.bmp` (`BUG-110`).
  - Added streaming gzip compression (`.snapshot.json.gz`) in `autosave.rs` with backward-compatible magic sniffing and surfaced SQLite open errors with an explicit UI toast in `StudioApp::new_desktop` (`BUG-109`).
- **Prepress, CQL Query Engine & Font Collection (`coree-prod`, `coree-app`):**
  - Added `compute_separations_with_text_outliner` wired to `TextEngine` plus `SymbolInstance` spot discovery and text/table separation rasterization (`BUG-112`).
  - Built clean-room `coree-cql` query parser/evaluator (`parse_cql_query`, `evaluate_cql_query`) with unit conversions, boolean AST, and Find/Replace UI quick-filter bar, plus a pure-Rust bounded SFNT `name` table reader in `collect_for_output` (`BUG-113`).
- **Security, Nested Transforms & Studio UI (`coree-app`):**
  - Enforced Unix `0o700` parent directory and `0o600` atomic lockfile permissions in `ipc.rs` (`BUG-114`).
  - Unified composed ancestor `world_transform_of_or_local` / `world_inverse_transform_of` and `is_object_editable_in_doc` lock checks across 26+ canvas/tool/drag/node-editing paths (`BUG-115`).
  - Added live visual sheet/bleed/crop/imposition preview thumbnail in `PrintPreviewDialog`, distinct per-tool vector icons, Property Bar parameters for Ellipse/Impact/BasicShapes/ArtisticMedia/Curves/Dimensions/Connectors, and Arabic UAX #9 localized `DockerTab::title_for` titles (`336 / 336` workspace tests passing, `BUG-116`).

## [0.3.0] - 2026-11-22

### Added & Hardened (Wave D — Deep Parity, Performance, Print & OS Integration)
- **Color Management & Print Fidelity (`coree-core`, `coree-io`, `coree-render`, `coree-app`):**
  - Added clean-room ICC v2 sRGB (`generate_minimal_srgb_icc_v2`) and Output CMYK (`generate_minimal_cmyk_icc_v2` with `A2B0`/`B2A0` LUTs) generators + ISO 32000-1 `/Catalog /OutputIntents` embedding in PDF export (`coree-io/src/pdf.rs`).
  - Added per-document `custom_icc_profile` support across `Document`, live `SoftProofLut3D::build_from_profile` canvas soft-proofing, PDF `/OutputIntents`, and `Options -> Color Management` file picker (`.icc` / `.icm`).
  - Added **Native OS Print Studio (`Ctrl+P`)** in `coree-app/src/printing.rs` & Print dialog with CUPS (`lpstat`/`lp`/`lpr`) and Windows Spooler (`Get-Printer`/`Start-Process -Verb PrintTo`) support, page-range filtering, copies, fit-to-page, collate, and prepress sheet imposition.
- **CDR / ZCF Codec Depth (`coree-cdr`):**
  - Implemented legacy RIFF v7–v15 `arrw` arrowhead, `bmpf` bitmap pattern, `vpat` vector pattern, and `clrs` color style handlers plus legacy binary `fild`/`outl` fallbacks.
  - Upgraded ZCF `styles/document.cdss` parser to extract both `character.latin` and `character.RTL` fonts (`bidi_font_family`) with numeric `font_weight` (`100..=900`), and preserved `ParametricShape` across `PowerClip` extract/roundtrip.
- **Scene Graph, Memory & Effects (`coree-core`, `coree-render`, `coree-app`):**
  - Added CorelDRAW's persistent cross-page **`Desktop` layer** (`Layer::is_desktop_layer`, `CommandOp::MoveToDesktopLayer`) indexed across pages and included in canvas, PDF, EPS, and SVG output.
  - Added `PowerClip` **`lock_contents`** behavior (`CommandOp::SetPowerClipLock`), UI checkbox & command-palette integration.
  - Replaced full 2× `BTreeMap` document cloning per Undo step with compact `DocumentDelta` / `ObjectDelta` diffing (`coree-core/src/command.rs`).
  - Implemented **`lower_effects_to_primitives`** (`coree-render`) and wired `Ctrl+K` (*Break Apart*) to decompose live Shadows, Contours, 2.5D Extrudes, Blends, and Symmetry into editable vector primitives atomically.
  - Added process-wide `OnceLock<fontdb::Database>` caching (`TextEngine`), OpenType `liga`/`calt`/`kern`/`dlig`/`smcp` + numeric `font_weight` + `bidi_font_family` controls, and **Rayon parallel dirty-tile rendering** (`DirtyTileCache`).
- **Studio UX, Single-Instance IPC & Windows Dual Binary (`coree-app`, CI/Packaging):**
  - Modularized monolithic `state.rs` into cohesive `crates/coree-app/src/state/{mod,types,tab,editing,production,canvas_tools,helpers}.rs`.
  - Added pure-Rust single-instance TCP loopback IPC + lockfile (`coree-app/src/ipc.rs`, `--new-window`) so Explorer/Desktop file opens focus and open tabs in the existing window cleanly.
  - Added full v2 `.coreews` `WorkspaceBundle` serialization (including `egui_dock` tree with finite-rect sanitization, toolbox active tools, UI language, and `CustomShortcutMap`) + SQLite persistence and live Custom Shortcut Editor (`Ctrl+J`).
  - Extended `InteractiveFill` canvas tool with Radial Focal point handle, intermediate stop handles across Linear/Radial/Conical/Square gradients, and double-click stop insertion.
  - Fixed multi-word Arabic UI run splitting (`configure_arabic_capable_fonts_for_language` prioritizing Arabic face in Arabic mode) and added `unicode-bidi` visual ordering helper (`shape_mixed_ui_bidi`).
  - Fixed Windows Release CLI (`#136994479287`): added Console-subsystem `coree-cli` shipped beside `coree.exe` as `coree.com` in WiX MSI & portable ZIP + synchronous `Start-Process` GUI smoke checks.
- **Community-Driven Precision, Ergonomics & Productivity Polish (`coree-core`, `coree-storage`, `coree-io`, `coree-prod`, `coree-render`, `coree-app`):**
  - Implemented `stroke_to_closed_path` (`coree-core::boolean`), `Document::convert_outline_to_object`, `CommandOp::ConvertOutlineToObject`, and `Ctrl+Shift+Q` shortcut/menu/context-menu workflow for converting outlines into fillable vector contours with z-order & Undo/Redo integrity.
  - Added `OutlineStyle::scaled_width_mm_for_transform` across PDF, EPS, Preflight hairline checks, and canvas hit-testing so `scale_with_object` matches screen preview consistently.
  - Upgraded DXF exporter (`coree-io/src/cad_plot.rs`) to emit native `0\nCIRCLE` and `0\nARC` entities for circular/arc shapes beside polyline curves.
  - Made OS print queue discovery non-blocking via background thread + polling (`refresh_system_printers_async` / `poll_system_printers_async`) so Print Preview never stalls the UI thread.
  - Added corrupted font file quarantine tracking (`FontScanReport::quarantined_files`) with a Font Manager warning badge, `PowerClip` internal content edit mode (`Ctrl+Click`, banner pill, `hit_test_powerclip_contents`, `Esc` exit), smart `Ctrl+D` step-and-repeat delta tracking, Left-Drag + `Spacebar` / Right-Click stamp duplicate guard, Toolbox Pick/Rect/`Shift+DblClick` quick frames, `Save Incremental Copy (_vN)`, Status Bar live dimensions/hints/right-click swatch clear, and theme-tokenized Toolbox icons (`326 / 326` workspace tests passing).

## [0.2.1] - 2026-11-22

### Fixed & Added (Wave C Deep Atomic Audit — `BUG-089`–`BUG-097`)
- **Vector Print & Web Export Fidelity (`coree-io`, `coree-render`):**
  - Replaced non-WinAnsi / Arabic RTL solid-rectangle fallback "Tofu" boxes in **PDF** and **EPS** exporters with real cubic Bézier glyph outlines via `coree_render::TextEngine` (`artistic_text_to_curves` / `paragraph_text_to_curves` / `text_on_path_to_curves`).
  - Added ISO 32000-1 `/Separation /<Spot> /DeviceCMYK` Spot color spaces (preserving cut-layer vector CutContour strokes over artwork), native `/ShadingType 2/3` axial/radial gradients, arrowheads, `Symmetry` clones, geometry deformations, and isolated 300-DPI `/SMask` RGBA XObjects for complex fills, live Lenses, and stacked effects in PDF.
  - Fixed EPS `NodePayload::Raster` (`ASCIIHexDecode` Level 2/3 `/ImageType 1` export + importer stream reader so inline hex streams re-import cleanly), `shfill` gradients, arrowheads, and effects.
  - Added SVG `<pattern>` emitters for `TwoColorPattern`, `BitmapPattern`, and `VectorPattern`, SVG arrowheads, and 150-DPI embedded PNG `<image>` fallback for lenses and non-SVG gradients/effects.
- **ZCF & `.CDR` Fidelity (`coree-cdr`):**
  - Fixed `[0..1]` normalized vs local-millimeter gradient mismatch (`normalized_fills` + `rescale_normalized_fill_to_bounds`), added cycle-guarded `CdssStyleSheet::resolve()` `"parent"` inheritance, expanded `parse_real_fild_v16` (Radial/Conical/Square/angle/patterns), and bounds-hardened `parse_real_outl_v16`.
- **Interactive Studio, HiDPI & Persistence (`coree-app`, `coree-core`):**
  - Added classic modal **Edit Fill (`F11`)** & **Outline Pen (`F12`)** dialogs with live gradient stop strip, calligraphy nib preview, arrowhead swap, and pre-press Overprint toggles.
  - Added interactive canvas manipulation for Radial/Conical/Square/Mid-Stop gradient handles, live Effect control handles, and selectable/draggable/deletable canvas `Guidelines` (`CommandOp::{MoveGuide, RemoveGuide, ToggleGuideLock}`).
  - Added live bilingual **English / العربية** (`EN | ع`) studio switcher and fixed silent SQLite `CHECK (category = 'ui')` failure so theme, shortcut profile, and language persist across sessions.
  - Fixed HiDPI (`pixels_per_point()`) canvas rendering, Frame-0 `CentralPanel` bounds clamping, `physical_key` shortcut lookup on Arabic keyboard layouts, and avoided R-Tree rebuilds on pure Pan/Zoom frames (`DirtyTileCache`).
- **Release Packaging & Native Binaries:**
  - Enabled stripped thin-LTO `[profile.release]` in `Cargo.toml`, added `windows_subsystem = "windows"` for release GUI builds on Windows (`main.rs`), `%WINDIR%\Fonts` discovery (`theme.rs`), and `.cdr` `Content Type` registration in `wix/main.wxs`.

## [0.2.0] - 2026-11-21

### Changed
- **Open-Source Community Dual License (`MIT OR Apache-2.0`):** Transitioned project licensing from `MPL-2.0` to the Rust ecosystem gold-standard `MIT OR Apache-2.0` dual license ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), [NOTICE](NOTICE)) — combining MIT's maximum global adoption with Apache-2.0's explicit patent grant and trademark non-grant.

### Added
- **Authentic ZCF `.cdr` X6–2025 (v16–26) Codec & Preview Extractor (`coree-cdr`):**
  - Complete ZCF 16-byte indirect pointer detection and bounded dereferencing (`zcf.rs`) across `content/dataFileList.dat` pools with anti-aliasing cumulative budgets.
  - Native `styles/document.cdss` JSON style sheet, `loda 0x00C9` style override, `color/docPalette.xml`, `color/color.xml`, `META-INF/textinfo.xml`, and `META-INF/metadata.xml` XMP parsers.
  - Accurate v16–26 geometry & matrix conversion (center-origin Y-up `0.1 µm` to top-left Y-down `mm`, `0xC0` cubic Bézier control handles, chamfered/rounded/scalloped rectangles, ellipses, and authentic UTF-16LE / CP1256 Arabic text).
  - Pure-Rust `extract_cdr_preview_png` extracting embedded ZIP thumbnails (`previews/thumbnail.png`, `previews/page1.png`, `preview/thumbnail.bmp`) and RIFF `DISP` DIB previews into valid RGBA8 PNGs.
  - Magic-byte file detection and transparent `.cdr.zip` / `.cdt.zip` opening.
- **Universal Pro Vector & Raster Importers (`coree-io`):**
  - **PDF 1.0–2.0 & PDF-compatible `.ai` Importer:** Traditional `xref` + incremental `/Prev` + PDF 1.5+ `/XRef` & `/ObjStm` streams, bounded `/FlateDecode`/`/ASCII85Decode`/`/ASCIIHexDecode`/`/RunLengthDecode` + TIFF/PNG `/Predictor`, full multipage `/Pages` & content stream interpreter (RGB/CMYK/Gray/ICCBased/Separation, Form & Image XObjects, `/ToUnicode` CMap & UTF-16BE text). Verified on real 44 MB 60-page PDFs.
  - **EPS / PostScript Importer:** DOS binary EPS (`C5 D0 D3 C6`) unwrapping + bounded PostScript tokenizer/interpreter with step and stack-node guards.
  - **DXF (R12/R2000+) & HPGL (`.plt`) Importers:** Full DXF `$INSUNITS`, `TABLES`/`LAYER` (`CutContour`, ACI & 24-bit color), cycle-guarded `BLOCKS`/`INSERT`, `LINE`/`LWPOLYLINE` (bulge arcs)/`POLYLINE`/`CIRCLE`/`ARC`/`ELLIPSE`/`SPLINE`/`SOLID`/`TEXT`/`MTEXT`; HPGL 40 units/mm pen commands & arcs.
  - **Multi-Layer Adobe Photoshop `.psd` / `.psb` Importer:** Streaming `Read + Seek` parser for 1/8/16-bit Bitmap/Grayscale/Indexed/RGB/CMYK/Lab with DPI extraction, all 16 blend modes, Unicode layer names, RAW/PackBits/ZIP decoding, and 256 MiB bounded streaming RGBA downscaling. Verified on real 295 MB 9-layer PSDs.
- **Pure-Rust ICC v2/v4 Color Management & Live CMYK Soft-Proofing (`coree-core`, `coree-render`):**
  - Bounded ICC v2/v4 parser (`desc`/`mluc`, `wtpt`, `rXYZ`/`gXYZ`/`bXYZ`, `curv`/`para` TRCs, `mft1`/`mft2` 3D Sakamoto tetrahedral & 4D quadrilinear LUTs, v4 `mAB `/`mBA ` compatibility) verified across 52/52 real system ICC profiles.
  - Built-in Bradford D50 + Yule-Nielsen Neugebauer offset-press profiles (`sRGB`, `Display P3`, `ISO Coated v2 CMYK`, `US Web Coated SWOP CMYK`), all 4 rendering intents, cached `17×17×17` soft-proof LUT, and out-of-gamut overlay warning.
- **Raster Studio Brushes, PostScript Fills, Autofit Page & Glyphs API (`coree-core`, `coree-render`):**
  - BT.709 YCbCr **Healing Brush**, Shadows/Midtones/Highlights **Dodge & Burn**, and iterative scanline BFS **Raster Flood Fill**.
  - 10 resolution-independent analytic SDF **PostScript Procedural Fills** (`Hatch`, `CrossHatch`, `Checker`, `PolkaDots`, `Waves`, `Bricks`, `RadialBurst`, `HexGrid`, `Woodgrain`, `HalftoneDots`).
  - One-click **Autofit Page to Content** (`Document::autofit_page_to_content`).
  - Live OpenType/TrueType **Glyphs Catalog API** (`GlyphBlockFilter` incl. Arabic, Presentation Forms, Latin, Symbols, Arrows, Geometric, Math, Punctuation) with cmap coverage detection and click-to-insert Glyphs Docker.
- **Production Laser Cutting, Engraving & Plotter Plugin System (`coree-prod`):**
  - Full laser toolpath engine: duplicate-segment removal, open-curve joining, winding-invariant miter-limited kerf offset, rotated/cross scanline hatch fill with hole nesting, inner-loops-first cut ordering, nearest-neighbor travel optimization, optional weeding box, and job time/distance estimation.
  - Direct exporters for GRBL 1.1 / Marlin / Ruida G-Code (`.gcode`), Laser DXF R12 (`AC1009`), HPGL `.plt`, color-coded SVG, and EPS.
  - Extensible Plugin Registry (`plugin.json` / `plugin.toml` + Rhai scripts) with 7 built-in ready-to-use bridge presets: **LightBurn**, **RDWorks / Ruida CO2**, **EZCad Fiber**, **LaserDRW / M2 Nano**, **LaserGRBL**, **InkCut / VisiCut**, and **Vinyl Cutter / Plotter (HPGL)** — using safe direct `Command::args` execution (zero shell injection).
- **62 Real Interactive Tools, Studio Workspaces & Muscle-Memory Shortcuts (`coree-app`):**
  - All 62 tools across 15 Toolbox flyout groups live and non-stubbed, plus live laser toolpath canvas preview overlay and right-click canvas context menu.
  - 6 Studio Workspace Presets (`DefaultClassic`, `Lite`, `Illustration`, `PageLayout`, `LaserSignStudio`, `TouchCompact`) with `.coreews` JSON export/import and full muscle-memory shortcuts (`P`, `L/R/T/B/C/E`, `F2`/`F3`/`F4`/`F9`, `Space`, `Ctrl+K`/`L`/`Q`/`D`/`R`/`J`, `Shift/Ctrl+PageUp/PageDown`, Nudge/Super-Nudge/Micro-Nudge).
  - New CLI subcommands: `coree thumbnail` (fast OS file-manager thumbnailer) and `coree laser` (toolpath planning, G-Code/DXF/PLT/SVG/EPS export & external plugin launch).
  - Embedded native window icon (Linux Wayland/X11 & Windows) and `build.rs` Win32 resource compilation.
- **Complete Linux & Windows Packaging & Multi-Format Release CI:**
  - **Linux OS Integration Fix (Zero More ZIP Icons for `.cdr`):** Canonical `application/vnd.corel-draw` MIME extension with `<sub-class-of type="application/zip"/>` and priority-90 magic rules for X6–2025 ZCF, X4–X5 ZIP, and RIFF/RIFX `.cdr`, MIME-type icons, GNOME/freedesktop `coree.thumbnailer`, upgraded `install-linux.sh` (`xdg-mime default`), offline-sandboxed **Flatpak** (`io.github.ahrrfy.coree.yml` + validated AppStream Metainfo), native zero-sudo **`.deb`** builder (`build-deb.sh`), and updated **`.AppImage`** builder (`build-appimage.sh`).
  - **Windows Native Packaging:** Multi-size `coree.ico` (`16`/`32`/`48`/`256` PNG-in-ICO), `coree.exe.manifest` (`PerMonitorV2` DPI + `<activeCodePage>UTF-8</activeCodePage>` for seamless Arabic filenames), `coree.rc`, upgraded WiX v4 `.msi` manifest (`wix/main.wxs` with Start Menu & Desktop shortcuts, PATH, `coree.Document` ProgID + `OpenWithProgids` for 11 formats), and zero-admin portable `register-file-associations.cmd`.
  - **Multi-Format GitHub Actions Release Pipeline:** Automated Linux (`.deb`, `.AppImage`, `.tar.gz`, SHA-256) and Windows (WiX v4 `.msi`, portable `.zip` + Arabic UTF-8 filename verification, SHA-256) builds and publishing.

### Fixed
- **Wave A Deep Adversarial Audit (28 S1/S2 Security, Codec & Geometry Bugs Fixed & Re-Verified):**
  - `coree-prod` (7 fixes): hatch scanline overflow, perforate dash hang/OOM, short-chord curve dedupe shredding, premature loop collapse, travel-optimizer `Inf`/`NaN` drop, G-Code newline injection & world-transformed connectors, and plugin `selection_only`/single-pass argv/`target_app` traversal/JSON spoof hardening.
  - `coree-core` & `coree-render` (4 fixes): ICC `mluc`/`desc` allocation-churn DoS, `profile_size` bounds slicing, missing-TRC `NaN` propagation, ICC v4 `mAB `/`mBA ` compatibility, plus lens `remove_face` warp, Arabic `TextOnPath` combining-mark anchoring, and nested PowerClip transforms.
  - `coree-io` (13 fixes): PSD mask/spot layer desync & composite RLE pre-alloc OOM; PDF delimiter hang, `/Length` cycle stack overflow, non-standard BPC underflow, filter/ASCII85/RLE/`/Contents` amplification; EPS unmatched `)` hang, extreme arc hang, empty `repeat` & exponential `dup` array bomb; HPGL UTF-8 boundary panic, DXF `inf` angle hang, block DAG bomb, and `i32::MIN.abs()` overflow.
  - `coree-cdr` (4 fixes): `trfd`/`convert_real_corel_rect`/`ellipse` `NaN`/`Inf` clamp panic, `loda` overlapping/decreasing offset 512x–1024x amplification + `0x00C9` JSON cap, off-by-one on 29-byte v16 `fild` color tail, and `extract_rdf_li_list` single-pass XML unescaping.

---

## [0.1.0] - 2026-11-21

### Added
- **7-Crate Pure-Rust Workspace (`MPL-2.0`, `#![forbid(unsafe_code)]`):**
  - `coree-core`: Scene graph, 8 physical units (`mm`..`CdrTenthMicron`), 6 color models (`RGB`/`CMYK`/`HSL`/`Lab`/`Gray`/`Spot`), procedural CMYK/RGB palettes, 8 live parametric shapes, Bezier node editing & sculpting tools, Knife/Virtual Segment Delete/Eraser, Smart Drawing, LiveSketch, Artistic Media, 9 `i_overlay` Boolean shaping operations, all 11 Lens models, 10 live vector effects, 19 raster filters, Arabic RTL/Kashida text model, `VectorTable`, live Dimensions & Connectors, R-Tree `SpatialIndex`, 28 serializable deterministic `CommandOp`s, transactional undo/redo, and sandboxed Rhai macro recording.
  - `coree-cdr`: Clean-room Pure-Rust `.cdr` (RIFF v4–v13, ZIP X4–X5, ZIP X6–2025), `.cdt`, `.cmx` (16/32-bit RIFF & RIFX), and `.cpt` codecs with byte-exact Unknown-Chunk Passthrough, legacy Arabic Windows-1256 (`CP1256`) text decoding, CDR Structural Inspector, and strict Zip-Bomb / Zip-Slip / depth / OOM guards.
  - `coree-io`: Atomic `.coree` native ZIP format, hardened `.svg`/`.svgz` import & export, print-ready DeviceCMYK/RGB PDF 1.7 export with Bleed/Crop/Registration/Color Bars, PostScript EPS Level 3, AutoCAD DXF & HPGL `.plt` plotter export, PNG/JPEG/WebP/TIFF/BMP with PNG `pHYs` DPI, and `.gpl`/`.ase`/`.pal` palette I/O.
  - `coree-storage`: Offline-first embedded SQLite 3 (WAL mode, 10-table schema), SHA-256 autosave snapshots, deterministic `command_journal` crash recovery, recents/palettes/templates/layouts/preferences, local SFNT system font scanner with Arabic coverage detection, and non-blocking Rayon background job pool.
  - `coree-render`: Backend-agnostic `CanvasBackend` (`tiny-skia`), Rayon 256×256 `DirtyTileCache` with Frustum Culling & drag LOD, all 11 live optical Lenses (with `Frozen`, `Viewpoint`, `RemoveFace`), Mesh Fill, 2.5D Extrude & vector effects, `cosmic-text` Arabic RTL/BiDi/Kashida shaping + `Fit Text to Path` + real glyph curves, integrated Raster Studio (brushes/masks/cutout/19 filters), 3-mode Vector Trace (`Quick`, `Outline`, `Centerline`), Smart Fill, and spatial hit-testing/snapping.
  - `coree-prod`: CMYK + Spot separations & Overprint simulation, N-Up / Saddle-Stitch Booklet / Tiling imposition, Print Marks, Preflight Inspector, Collect for Output, one-click `CutContour`, 4 Parametric Packaging Dielines (`TuckEndBox`, `Envelope`, `ShoppingBag`, `StickerSheet`), live Path Length & Area Meter, vector QR & 1D Barcodes (`EAN-13`/`EAN-8`/`UPC-A`/`Code128`/`Code39`), bilingual UTF-8 CSV Print Merge, CIE76 Find & Replace, and sandboxed Rhai automation macros.
  - `coree-app`: Native Linux/Windows desktop GUI (`eframe 0.37` / `egui 0.37` / `egui_dock 0.22` / pure-Rust `rfd` XDG Portal) with 15 Toolbox Flyout groups (62 tools), Context Property Bar, 20px drag-to-guide Rulers, Multipage View, dual-click Transform/Rotate handles with movable pivot, 14 dockable panels, right Color Palette Bar (L-click Fill / R-click Outline), Command Palette, WCAG 2.2 AA/AAA Dark/Light/High-Contrast themes, and 9-subcommand headless CLI (`coree`).
- **Linux & Windows Packaging:** Zero-sudo Linux installer (`packaging/linux/install-linux.sh`), `.desktop` entry, freedesktop MIME associations, SVG icon, AppImage staging script, Windows build guide, WiX v4 MSI manifest, and Linux + Windows GitHub Actions CI & Release workflows.
- **Verification:** 202/202 automated tests green (`0` failures), `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo fmt --all -- --check` clean, 35/35 WCAG AA/AAA contrast pairs passing, and 100.0% deterministic pixel rendering verified.
