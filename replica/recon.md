# Recon map: CorelDRAW Graphics Suite (Desktop — Windows / macOS; rebuilt natively for Linux-first & Windows as `coree`)

Scope: Full offline desktop vector illustration, multi-page layout, 11 interactive Lenses, vector/bitmap Effects, integrated Raster Photo Studio, PowerTRACE bitmap-to-vector tracing, commercial Prepress/Imposition/Preflight/Barcode/Print Merge/DXF/PLT, CutContour/Parametric Dielines/Path Meter, Arabic RTL/Kashida & legacy CP1256 `.cdr` support, Unknown-Chunk Passthrough + CDR Inspector, and Serializable Command Journal / Rhai Macro Recorder.
For: Print shops, sign makers, packaging/laser/vinyl cutting workshops, Arabic/MENA & international studios, and independent designers on Linux and Windows who want a fast, 100% offline, zero-subscription `MIT OR Apache-2.0` pure-Rust production studio.
Date: 2026-11-21

## Sources

| # | source | URL | notes |
| --- | --- | --- | --- |
| 1 | official product overview & pricing | https://www.coreldraw.com/en/product/coreldraw/ | Verified HTTP 200; suite apps (CorelDRAW, PHOTO-PAINT, Font Manager), subscription vs perpetual structure, system specs |
| 2 | official edition comparison | https://www.coreldraw.com/en/product/coreldraw/compare/ | Verified HTTP 200; feature matrix across Graphics Suite, Standard, Essentials, Technical |
| 3 | official vector design guide | https://www.coreldraw.com/en/learn/guide-to-vector-design/ | Verified HTTP 200; curves, nodes, fills, typography, export workflows |
| 4 | official tutorials library | https://learn.corel.com/graphics-tutorials/coreldraw-tutorials/ | Verified HTTP 200; Lenses, PowerClip, PowerTRACE, Multipage View, Prepress, Tables |
| 5 | Mac App Store listing & reviews RSS | https://apps.apple.com/us/app/coreldraw/id1447605203 | Verified official Apple storefront + 216 customer reviews via Apple iTunes RSS feed (`reviews.csv`) |
| 6 | Document Liberation `libcdr` wiki | https://wiki.documentfoundation.org/DLP/Libraries/libcdr | Verified HTTP 200; clean-room public documentation of `.cdr`/`.cmx`/`.cdt` RIFF & ZIP container versions |
| 7 | `libcdr` public issue tracker | https://bugs.documentfoundation.org/buglist.cgi?quicksearch=libcdr | Verified official Bugzilla REST API; real-world `.cdr` compatibility, lens, and codepage edge cases |
| 8 | public community & developer trackers | https://github.com/LibreOffice/libcdr | Verified HTTP 200; plus HN Algolia API, GitHub Search API, and Inkscape GitLab API (`reviews.csv`) |

## Core loop

Create or open a multi-page vector/raster production document (including legacy and modern `.cdr` files), edit precision curves, typography, fills, PowerClips, effects, lenses, and dielines in a high-density studio workspace, and export press-ready CMYK PDF/X, plotter/laser cut files (`CutContour`/DXF/PLT), or round-tripped `.cdr` output — completely offline with zero data loss.

## Screens

| ID | screen | route / how to reach | purpose | key components | states seen |
| --- | --- | --- | --- | --- | --- |
| S01 | Welcome & Recovery Hub | App launch or `File > New / Welcome` | Create blank/template documents, reopen recent files, inspect crash-recovery autosave snapshots | Tab strip, Template gallery cards, Recent file list, Recovery banner, Primary CTA buttons | empty (first run), filled (recent files), recovery-prompt, error (missing file path) |
| S02 | Main Vector Studio Canvas | Open/create document (default primary view) | Primary vector illustration & layout workspace | Menu Bar, Standard Bar, Context Property Bar, Toolbox + Flyouts, Rulers/Guides/Grid, Interactive Viewport, Page Tabs, Docked Color Palette Bar, Status Bar + Path Meter HUD | empty page, populated canvas, tool-active, PowerClip-edit isolation, wireframe/overprint preview, busy-render |
| S03 | Multipage View & Page Manager | `View > Multipage View` or Page Tab bar toggle | Arrange, resize, reorder, duplicate, and manage multi-page layouts and Master Layers | Multipage layout toolbar (Grid/Vertical/Horizontal/Freeform), Page thumbnails, Page Properties inspector, Bleed/Facing controls | single-page, multi-page grid, dragging-page, autofit-pending |
| S04 | Curve, Node & Boolean Shaping Workspace | Activate Shape (`F10`), Curve/Pen/B-Spline, Sculpting, or Shaping Docker | Precision Bézier/B-spline node editing, knife/virtual segment cutting, Boolean weld/trim/intersect, Symmetry & Perspective modes | Node handles overlay, Curve Property Bar, Shaping Docker (Weld/Trim/Intersect/Simplify/Boundary), Symmetry axis overlay, Perspective vanishing grid | no-selection, path-selected, nodes-selected, live-drag-preview, boolean-error (open/degenerate path) |
| S05 | Objects, Layers & PowerClip Docker | Right Docker tab `Objects` (`Alt+Shift+O`) | Manage page/master layer hierarchy, z-order, lock/hide/print/cut flags, and PowerClip containers | TreeView with drag-reorder, Eye/Lock/Print/Cut toggle icons, Master Layer badges, PowerClip mini-toolbar, Search/Filter input | empty layer, nested groups, filtered search, PowerClip-inside-edit mode |
| S06 | Color, Fill, Mesh & Outline Properties Docker | Right Docker tab `Properties` (`Alt+Enter`) or `F11` / `F12` | Configure Uniform/Fountain/Pattern/Mesh fills, Outline Pen settings, CMYK/RGB/Lab/Spot channels, and Custom Color Palettes | Fill mode segmented tabs, Color wheel/CMYK-RGB sliders, Gradient stop editor, Mesh NxM grid controls, Outline pen width/join/dash/arrow controls, Swatch Palette Manager | no-selection (document defaults), mixed-selection, spot-color active, out-of-gamut warning |
| S07 | Typography, Arabic RTL/Kashida & Font Manager Docker | Text tool (`F8`), `Text > Text Properties` (`Ctrl+T`), or Font Manager tab | Author Artistic & Paragraph text, Fit Text to Path, control Arabic RTL/Kashida/numerals, OpenType axes, Glyphs, and local font collections | Font picker with live preview, Size/Leading/Kerning numeric spinboxes, RTL/LTR toggle, Kashida slider, OpenType toggle chips, Glyph grid, Missing-Font substitution modal | empty text frame, overflow-indicator (`+`), RTL-mixed bidi active, missing-font warning |
| S08 | Interactive Vector & Bitmap Effects Docker | Toolbox Interactive Effects flyout or `Effects` Docker | Apply and edit non-destructive Drop/Inner/Block Shadows, Contours, Blends, Distortions, Envelopes, Extrusions, Transparency, and Bitmap Filter stack | On-canvas interactive control handles, Effect stack list (reorder/toggle/delete), Numeric parameter sliders, Blend-mode dropdown | no-effect, live-previewing, multi-effect stack, raster-cache-updating |
| S09 | 11-Lens Inspector Docker | `Effects > Lens` (`Alt+F3`) | Apply any of the 11 non-destructive Lenses (`Brighten`, `Color Add`, `Color Limit`, `Custom Color Map`, `Fish Eye`, `Heat Map`, `Invert`, `Magnify`, `Tinted Grayscale`, `Transparency`, `Wireframe`) with Frozen/Viewpoint/Remove Face | Lens type dropdown, Rate/Magnification/Color controls, Frozen/Viewpoint/Remove Face checkboxes, Viewpoint X/Y picker | no-closed-shape selected (disabled), lens-applied, viewpoint-picking, frozen-locked |
| S10 | Integrated Photo Studio Workspace | Switch mode `Studio > Photo` or `Edit Bitmap` on selected raster | In-app raster pixel editing (replacing external PHOTO-PAINT): layers, masks, paint/clone/heal brushes, tone adjustments, Cutout Lab | Raster Toolbox, Brush Property Bar, Pixel Layers & Mask Docker, Live Histogram + Curves/Levels/HSL sliders, Selection marching-ants overlay | no-bitmap-active, pixel-editing, mask-overlay active, adjustment-previewing, resample-processing |
| S11 | PowerTRACE Bitmap-to-Vector Dialog | Select bitmap -> Property Bar `Trace Bitmap` | Convert raster artwork into editable vector curves via Quick, Centerline, or Outline Trace presets | Split Before/After interactive zoomable canvas, Preset dropdown, Detail/Smoothing/Corner sliders, Color Palette reducer table, Background removal picker | computing-preview, preview-ready, high-color-warning, trace-empty |
| S12 | Prepress, Print Preview, Imposition & Separations Studio | `File > Print Preview` (`Ctrl+Shift+P`) or `File > Print` | Configure N-Up/Booklet/Step-and-Repeat imposition, Printer's Marks, CMYK+Spot separations, overprint/trapping, and emulsion mirror/negative | Interactive sheet preview with fold/crop/registration marks, Imposition preset selector, Plate separation channel checkboxes (C/M/Y/K/Spots), Tiling grid | default sheet, booklet-signature view, single-plate-separation inspection, tiling-overflow warning |
| S13 | Document Preflight & Collect for Output Dialog | `File > Preflight` or `File > Collect for Output` | Audit press readiness (low-DPI, RGB in CMYK, hairlines, missing fonts, bleed/overprint) and bundle assets for a print shop | Severity filter tabs (Errors/Warnings/Info), Issue list with click-to-select-object, Rule checklist, Output bundle directory picker & manifest preview | scanning, clean-pass, issues-found, bundling-progress, bundle-complete |
| S14 | Packaging, CutContour, Barcode/QR & Print Merge Studio | `Production` menu / Docker (`CutContour`, `Dielines`, `Barcode`, `Print Merge`) | Generate parametric FEFCO dielines, assign `CutContour`/`PerfCutContour`/`Crease` spot layers, inspect live Path Meter metrics, insert vector Barcodes/QR, run CSV Print Merge | Parametric dimension inputs (L/W/H/flute), Live dieline wireframe preview, CutContour spot badge buttons, Path Meter breakdown table, Barcode type/payload inputs, CSV column mapper | empty, valid-dieline-preview, invalid-barcode-checksum error, csv-loaded-ready, merge-running |
| S15 | Universal Import / Export Dialog | `File > Open/Import` (`Ctrl+I`) or `File > Export` (`Ctrl+E`) | Read/write `.cdr`/`.cdt`/`.cmx`/`.cpt`, PDF/X-1a/4, SVG, EPS, DXF, HPGL `.plt`, PNG/JPEG/WebP/TIFF/BMP/PSD with safety & encoding options | Native `xdg-desktop-portal` file picker + Format Options panel (PDF/X preset, CMYK ICC, DPI, DXF units, PLT pen mapping, Legacy `.cdr` codepage selector CP1256/CP1251/UTF-8) | format-selected, exporting-progress, safety-limit-error (Zip-Bomb/corrupt archive), unknown-chunks-preserved notice |
| S16 | CDR Binary & Chunk Inspector Docker | `Tools > CDR Inspector` | Inspect raw RIFF/ZIP chunk hierarchy, CDR version, text encodings, embedded ICC/bitmaps, and Unknown-Chunk Passthrough retention | Collapsible Chunk TreeView, Hex/ASCII byte viewer, Tag/Length/Offset metadata table, Passthrough badge (`Parsed` vs `Preserved Raw`) | no-cdr-loaded, tree-populated, chunk-selected-hex, parse-warning highlighted |
| S17 | Command Journal, Crash Recovery & Rhai Macro Studio | `Tools > Scripts & Command Log` | Browse serializable `DocumentCommand` undo/redo timeline, inspect autosave snapshots, record/edit/run `.rhai` automation scripts | Command timeline list with JSON payload inspector, Record/Stop/Play macro buttons, Rhai script editor with console output, Snapshot restore list | idle, recording-macro, script-running, script-error (line highlighted), recovery-replaying |
| S18 | Preferences, Color Management & Command Palette | `Ctrl+J` (Preferences) / `Ctrl+K` (Command Palette) | Configure ICC color profiles, default units/nudges, canvas tile cache threads, keyboard shortcuts, workspace layout presets, and search any command instantly | Modal Search Command Palette (`Ctrl+K`), Categorized Preferences sidebar, ICC profile pickers, Shortcut capture input, Local-Only Offline badge | closed, searching-palette, shortcut-conflict warning, unsaved-changes |

## Flows

```
F01 Create a new CMYK print document from Welcome Hub
    S01 -> S02
    happy path clicks: 3 (New Document -> pick preset/size -> Create)
    edge: invalid custom dimensions (<= 0 or > 45 m), missing custom template file, unsaved crash recovery prompt appears first

F02 Draw, shape, and Boolean-weld vector artwork
    S02 -> S04 -> S02
    happy path clicks: 5 (pick Rectangle/Ellipse -> drag shape 1 -> drag shape 2 -> Select both -> Weld in Shaping Docker)
    edge: open subpaths passed to Boolean op, self-intersecting curves, zero-area degenerate result

F03 Multi-page brochure layout with Master Layers
    S02 -> S03 -> S05 -> S02
    happy path clicks: 5 (Add Page -> open Multipage View -> drag reorder -> select Master Layer -> place running header)
    edge: mixed page orientations/sizes, deleting last remaining page (blocked), autofit on empty page

F04 Apply interactive Fountain Fill, Mesh Fill, and Outline Pen with Spot Color
    S02 -> S06 -> S02
    happy path clicks: 4 (select object -> choose Fountain/Mesh in Properties -> click swatch in Palette Bar -> set Outline width/overprint)
    edge: mixed multi-object selection, out-of-gamut RGB-to-CMYK conversion warning, custom spot name collision

F05 Author Arabic RTL / Kashida paragraph text & fit artistic text to a curve
    S02 -> S07 -> S02
    happy path clicks: 5 (select Text tool -> toggle RTL + Kashida -> type Arabic/Latin bidi run -> select curve -> Fit Text to Path)
    edge: missing Arabic font triggers fallback substitution modal, paragraph frame text overflow, mixed LTR numbers inside RTL runs

F06 Place artwork inside a PowerClip container and edit contents in isolation
    S02 -> S05 -> S02
    happy path clicks: 4 (select content -> Place Inside Frame -> click container -> Edit PowerClip -> Finish)
    edge: circular PowerClip nesting (blocked), empty container frame, extracting contents onto locked layer

F07 Apply and customize a non-destructive Vector Effect (Drop Shadow / Contour / Blend / Envelope)
    S02 -> S08 -> S02
    happy path clicks: 4 (select shape -> choose Effect tool -> drag interactive handle on canvas -> tune steps/feather in Properties)
    edge: blending incompatible group depths, breaking apart effect into raw paths, extreme step counts (> 999 capped)

F08 Apply any of the 11 Interactive Lenses with Viewpoint & Frozen state
    S02 -> S09 -> S02
    happy path clicks: 4 (select closed lens shape -> open Lens Docker -> choose Lens type e.g. Fish Eye/Magnify -> set rate & optional Frozen/Viewpoint)
    edge: open path cannot host Lens (prompt to auto-close), recursive lens-over-lens cycle depth limit, printing/flattening lens to CMYK PDF

F09 Retouch an embedded bitmap in integrated Photo Studio
    S02 -> S10 -> S02
    happy path clicks: 4 (select bitmap -> Edit Bitmap -> apply mask/Levels/Clone Stamp -> Commit back to vector page)
    edge: huge high-DPI bitmap exceeds tile memory budget (streamed tiles), CMYK vs RGB filter availability, cancelling discards raster session cleanly

F10 Vectorize a raster logo with PowerTRACE
    S02 -> S11 -> S02
    happy path clicks: 4 (select bitmap -> Trace Bitmap > Outline Trace Logo -> adjust Detail/Colors & Remove Background -> OK)
    edge: massive > 16 MP bitmap prompts downsample before trace, noisy photo produces excessive curves (node budget warning), user cancels mid-trace

F11 Configure Imposition, Printer's Marks, and CMYK+Spot Color Separations
    S02 -> S12 -> S15
    happy path clicks: 5 (open Print Preview -> pick Booklet/N-Up imposition -> enable Crop/Registration marks -> inspect Spot/CMYK plates -> Export PDF/X-1a)
    edge: odd page count in saddle-stitch booklet (auto-pad blank pages), spot plate count > 8, bleed larger than gutter

F12 Run Document Preflight and Collect for Output
    S02 -> S13 -> S02
    happy path clicks: 3 (open Preflight -> click warning to jump/fix offending object -> Collect for Output)
    edge: read-only destination folder, missing linked image on disk, embedded font license flag warning

F13 Generate Parametric Packaging Dieline with CutContour & live Path Meter check
    S02 -> S14 -> S15
    happy path clicks: 5 (open Packaging Studio -> select FEFCO box & L/W/H -> Generate -> verify CutContour/Crease layers & Path Meter total mm -> Export DXF/PLT/PDF)
    edge: flute thickness > wall dimension (validation error), non-hairline CutContour stroke auto-flagged, open path on cut layer

F14 Insert vector Barcode/QR and run CSV Print Merge
    S02 -> S14 -> S12
    happy path clicks: 6 (insert Barcode/QR placeholder -> load CSV -> map fields -> Preview Merge -> Generate merged pages/PDF)
    edge: invalid EAN-13/UPC check digit, malformed UTF-8/CP1256 CSV delimiter, 10,000-row batch streams directly to disk PDF without RAM blowup

F15 Open legacy `.cdr` (v3–v25+) with Unknown-Chunk Passthrough & inspect raw chunks
    S01 -> S15 -> S02 -> S16
    happy path clicks: 4 (Open `.cdr` -> select CP1256/codepage if pre-X4 legacy -> view rendered canvas -> open CDR Inspector to verify preserved unknown chunks)
    edge: Zip-Bomb (> 512 MB uncompressed) or Zip-Slip path traversal rejected safely before extraction, truncated RIFF chunk gracefully skips with warning, round-trip save preserves unknown chunks byte-for-byte

F16 Export production files to PDF/X, SVG, DXF, HPGL `.plt`, or round-trip `.cdr`
    S02 -> S15
    happy path clicks: 3 (File > Export -> choose format & preset -> Save via xdg-desktop-portal)
    edge: disk full (atomic temp-file write prevents corrupting existing target), unsupported SVG blend mode warns before rasterizing fallback

F17 Record a Rhai Macro from Command Journal and replay it on another document
    S02 -> S17 -> S02
    happy path clicks: 4 (open Scripts Docker -> click Record -> perform canvas actions -> Stop & Run `.rhai` script)
    edge: Rhai script hits execution op-count sandbox limit (infinite loop aborted safely), script references missing selection

F18 Crash recovery & instant Command Palette action launch
    S01 (after crash) -> S17 -> S02 -> S18
    happy path clicks: 2 (click Restore Autosave on Welcome Recovery banner -> press `Ctrl+K` to jump to any tool/preference)
    edge: truncated trailing WAL command discarded cleanly back to last committed transaction, multiple unsaved tabs recovered independently
```

## Components

| component | variants | states | used on |
| --- | --- | --- | --- |
| MenuBar & StandardToolbar | compact, dense-studio | default, hover, open-menu, disabled | S02, S03, S10 |
| Toolbox & ToolFlyout | single-column, two-column, flyout-drawer | default, hover, active-tool, flyout-expanded, disabled | S02, S04, S10 |
| ContextPropertyBar | document, pick, shape, text, fill, effect, table, dimension, bitmap | default, mixed-values (`—`), editing, invalid-input | S02, S03, S04, S07, S08, S10 |
| CanvasViewport & Rulers | vector-enhanced, wireframe, overprint, multipage, raster-pixel | idle, panning, zooming, marquee-selecting, dragging-handles, rendering-tiles | S02, S03, S04, S10, S11, S12 |
| PageTabBar | bottom-strip | active-page, inactive-page, dragging-reorder, master-page-indicator | S02, S03 |
| DockerPane & TabContainer | docked-right, docked-left, floating-window, collapsed-rail | expanded, collapsed, active-tab, drag-docking-preview | S02, S04, S05, S06, S07, S08, S09, S14, S16, S17 |
| ColorPaletteSwatchBar | cmyk-default, document-palette, spot-cutcontour, custom | default, hover-tooltip, active-fill, active-stroke, show-names-expanded | S02, S06, S14 |
| NumericSpinbox & UnitInput | mm, cm, in, pt, px, deg, pct | default, focus, scrub-dragging, mixed (`—`), invalid-range | S02, S03, S06, S07, S08, S09, S12, S14, S18 |
| TreeView (Objects / Chunks) | layer-tree, chunk-hex-tree | expanded, collapsed, selected, multi-selected, drag-target, hidden/locked | S05, S16 |
| SplitPreviewCanvas | side-by-side, swipe-divider, wireframe-overlay | loading, interactive-pan-zoom, synced | S11, S12 |
| DialogModal & Sheet | standard, wizard, preflight-drawer, export-options | open, busy-progress, validation-error, confirm-destructive | S01, S11, S12, S13, S14, S15, S18 |
| CommandPalette (`Ctrl+K`) | quick-action, tool-jump, docker-toggle | closed, typing-filtered, keyboard-navigating, no-results | all (`S01`–`S18`) |
| StatusBar & PathMeterHUD | compact, expanded-metrics | default, selection-summary, cut-length-active, autosave-saved, macro-recording | S02, S03, S04, S10, S14 |
| Button | primary, secondary, ghost, toggle-icon, danger | default, hover, active, focus-visible, disabled, loading | all |
| Toast / Non-Blocking Banner | info, success, warning, error, crash-recovery | visible, actionable, dismissed | all |

## Inferred data model

```
Document          id, title, file_path, format_version, color_mode (cmyk|rgb), default_unit,
                  icc_profile_cmyk, icc_profile_rgb, created_at, updated_at, raw_unknown_chunks (blob_map)
                  evidence: S01, S02, S15, S16, libcdr format docs
                  confidence: high

Page              id, document_id, index, name, width_mm, height_mm, bleed_mm, orientation,
                  position_x_mm, position_y_mm (for Multipage View), background_fill, facing_side
                  evidence: S02, S03, Multipage View docs
                  confidence: high

Layer             id, page_id (nullable for Master Layers), master_scope (none|all|odd|even),
                  name, z_order, visible, locked, printable, is_cut_layer, guide_color
                  evidence: S03, S05, S14
                  confidence: high

SceneNode         id, layer_id, parent_id (groups / PowerClip containers), z_order, node_kind
                  (path|rect|ellipse|polygon|star|spiral|text_artistic|text_paragraph|bitmap|table|dimension|connector|group|symbol_instance),
                  transform_2d (6x f64), geometry_payload, fill_spec, outline_spec, opacity, blend_mode,
                  overprint_fill, overprint_stroke, powerclip_contents_ids, unknown_cdr_chunks
                  evidence: S02, S04, S05, S06, S07, S16
                  confidence: high

TextRun           node_id, content_utf8, source_codepage (utf8|cp1256|cp1251|cp1250|cp1252),
                  direction (ltr|rtl|auto), font_family, font_weight, font_size_pt, tracking,
                  leading, kashida_ratio, opentype_features, attached_path_id, linked_next_frame_id
                  evidence: S07, S15, Bugzilla codepage reports
                  confidence: high

EffectOrLens      id, target_node_id, stack_order, enabled, kind (drop_shadow|inner_shadow|block_shadow|
                  contour|blend|distort|envelope|extrude|transparency|bitmap_filter|lens_brighten|
                  lens_color_add|lens_color_limit|lens_custom_map|lens_fisheye|lens_heatmap|
                  lens_invert|lens_magnify|lens_tinted_gray|lens_transparency|lens_wireframe),
                  params_json, lens_frozen, lens_viewpoint_xy, lens_remove_face
                  evidence: S08, S09, CorelDRAW Lens & Effects documentation
                  confidence: high

ColorPalette      id, name, kind (cmyk|rgb|spot|document|custom), is_builtin, created_at
ColorSwatch       id, palette_id, name, color_space (cmyk|rgb|lab|spot), c, m, y, k, r, g, b,
                  spot_name (e.g. CutContour), sort_order
                  evidence: S02, S06, S14
                  confidence: high

CommandJournal    id, document_id, seq_no, command_type, payload_json, inverse_payload_json,
                  committed_at, session_id
                  evidence: S17, crash-safe undo & Rhai macro recording architecture
                  confidence: high

AutosaveSnapshot  id, document_id, file_path, snapshot_blob_path, last_command_seq,
                  clean_shutdown, created_at
                  evidence: S01, S17
                  confidence: high

LocalPreferences  RecentDocument, Template, WorkspaceLayout, FontIndexEntry, BrushPreset, AppPreference
                  evidence: S01, S07, S10, S18
                  confidence: high
```

Relationships: `Document 1-n Page`, `Document 1-n Layer (Master)`, `Page 1-n Layer`, `Layer 1-n SceneNode`, `SceneNode 1-n SceneNode (Groups / PowerClip)`, `SceneNode 1-n EffectOrLens`, `ColorPalette 1-n ColorSwatch`, `Document 1-n CommandJournal`, `Document 1-n AutosaveSnapshot`.

## Feature matrix

See [features.csv](features.csv) (102 total rows, validated cleanly via `parity.py`):
- **Counted CorelDRAW parity rows (`original=yes`, active):** **86** (`must`: 68, `should`: 17, `could`: 1) across 13 areas (`workspace`, `pages`, `selection`, `drawing`, `technical`, `color-fill`, `typography`, `effects`, `lenses`, `photo-studio`, `powertrace`, `prepress`, `codecs`).
- **Out-of-scope (`skip`):** **6**.
- **`coree` differentiators from `replica-entrepreneur` (`original=no`):** **10** (`must`: 8, `should`: 2).

## Out of scope (cannot or should not be cloned)

- **Corel Cloud storage, online file sync & web collaboration comments** — `coree` is strictly 100% offline-first with zero cloud servers or network sockets.
- **CorelDRAW Web browser app & iPad companion app** — Desktop-first native Linux and Windows scope only.
- **User accounts, license activation servers & subscription billing** — Zero accounts, zero DRM, zero telemetry under `MIT OR Apache-2.0`.
- **Licensed Pantone® books & proprietary encrypted vendor color libraries** — Trademarked/licensed third-party data; `coree` provides open CMYK/RGB/Lab palettes plus user-defined Spot Color channels (including open `CutContour`/`PerfCutContour`/`Crease` presets) and palette import.
- **Proprietary Corel clipart, stock photos, licensed Monotype fonts & cloud AI credit packs** — Copyrighted third-party assets; users use local system fonts and their own assets.
- **Windows-only COM / VBA / VSTA macro runtime** — Platform-locked proprietary Windows automation; replaced cleanly by cross-platform Rhai scripting generated directly from the `DocumentCommand` log.

## Size

Screens: **18** (`S01`–`S18`), Flows: **18** (`F01`–`F18`), Entities: **10 core domain entities** (+ 6 local persistence tables).
Hard parts:
1. Clean-room binary `.cdr`/`.cdt`/`.cmx` (v3–v25+) codec with byte-exact Unknown-Chunk Passthrough, legacy codepage decoding (CP1256/CP1251), and Zip-Bomb/Zip-Slip parser hardening.
2. High-performance 2D vector/raster rendering (`tiny-skia` + `rstar` R-Tree + Rayon dirty tiles) covering all 11 interactive Lenses, Mesh Fills, PowerClips, Boolean shaping (`i_overlay`), and CMYK/Spot overprint simulation at 60 FPS on 10,000+ objects.
3. Native Arabic RTL/Kashida bidirectional text shaping (`cosmic-text`), PowerTRACE vectorization, and commercial prepress (CMYK PDF/X-1a/4, Imposition, Separations, DXF, HPGL `.plt`).

Size: **L–XL** (staged across vertical-slice milestones in [architecture.md](architecture.md)).
