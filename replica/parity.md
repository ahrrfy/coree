## Parity: 100.0 / 100

features 100.0  (86 counted, must-haves 68 of 68 done)

## By area, weakest first
- workspace                    100.0  (6 features)
- pages                        100.0  (5 features)
- selection                    100.0  (3 features)
- drawing                      100.0  (13 features)
- technical                    100.0  (3 features)
- color-fill                   100.0  (7 features)
- typography                   100.0  (4 features)
- effects                      100.0  (10 features)
- lenses                       100.0  (12 features)
- photo-studio                 100.0  (6 features)
- powertrace                   100.0  (3 features)
- prepress                     100.0  (8 features)
- codecs                       100.0  (6 features)

## Missing, in build order
- nothing. Every counted feature is done.

## Left out on purpose (not scored)
- Corel Cloud storage online file sync and web review comments: offline-first design with zero cloud accounts or remote servers
- CorelDRAW Web browser editor and iPad companion app: desktop-first native Linux and Windows scope only
- User accounts license activation servers and subscription billing: zero accounts zero DRM MIT OR Apache-2.0 open-source dual license
- Licensed Pantone books and proprietary encrypted vendor color libraries: proprietary trademarked color libraries; users import custom spot palettes instead
- Proprietary Corel clipart stock photos licensed Monotype fonts and cloud AI credits: copyrighted commercial content; users bring local fonts and assets
- Windows-only COM/VBA/VSTA macro scripting runtime: proprietary Windows COM runtime; replaced by cross-platform Rhai scripting and JSON/TOML plugins

## Yours, not in the original (not scored)
- Native Linux-first desktop build (Wayland + X11 zero Wine pure Rust xdg-desktop-portal)
- 100% offline-first zero-account zero-telemetry MIT OR Apache-2.0 operation (never phones home)
- Unknown-Chunk Passthrough in .cdr/.cdt/.cmx round-trip saves (preserve unrecognized chunks verbatim)
- Live CDR Binary/Chunk Inspector docker (hex/tree view of RIFF/ZIP chunks version codepage warnings)
- Native Arabic RTL / HarfBuzz bidirectional text shaping Kashida justification and CP1256 legacy .cdr decoder
- First-class CutContour / PerfCutContour / Crease spot presets and Parametric Packaging Dielines (FEFCO)
- Live Path Meter HUD (total cut length in mm/m node count bounding area estimated laser/plotter time)
- Production Laser Cutting Engraving & Plotter Plugin System (LightBurn RDWorks EZCad LaserDRW LaserGRBL InkCut HPGL + custom JSON/TOML/Rhai plugins + kerf/hatch/dedupe/G-Code)
- Native Linux & Windows OS File Integration (canonical .cdr MIME sub-class-of zip + GNOME thumbnailer + Windows WiX v4 MSI + coree.ico + UTF-8 Arabic manifest + portable HKCU association script)
- Crash-safe SQLite WAL Serializable Command Journal Atomic File Saves and instant crash recovery
- Cross-platform Rhai Macro Recorder and Script Runner (record CommandOp log into editable .rhai scripts)
- R-Tree (rstar) spatial index and Rayon multi-threaded dirty-tile rendering for 10000+ object files

## Deterministic Visual Rendering Verification (`imgdiff.py`)

Two independent release-binary renders (`./target/release/coree render ... --dpi 150`) of `showcase_lenses_vectors.coree` (containing parametric shapes, multi-stop Fountain gradients, a 4×4 Bezier Mesh Fill, Drop Shadow, Stepped Contour, and 5 live optical Lenses: `FishEye`, `Magnify`, `Invert`, `HeatMap`, `Wireframe`) were compared using `imgdiff.py --mode pixel --tolerance 0`:

- **Resolution:** `1417 × 945` px (`150 DPI`, `ViewMode::Enhanced`)
- **Pixel Determinism Score (`imgdiff.py --mode pixel --tolerance 0`):** **`100.0 / 100`** (`verdict: matches`, `changed_pixels: 0`, `height_delta_pct: 0.0`, `regions: []`)
- **Byte-for-Byte SHA-256 (Run 1 == Run 2):** `a68acf8909f23a33119fcd6c28c0d1b6adbcf7adf2bce8390968c8d6ef54de9f`
- **Generated Showcase Assets:**
  1. `showcase_lenses_vectors.png` (`1417 × 945` @ 150 DPI) — Vector shapes, Fountain gradients, 4×4 Mesh Fill, Drop Shadow, Contour, and 5 live optical Lenses (`FishEye`, `Magnify 2.2×`, `Invert`, `HeatMap`, `Wireframe`)
  2. `showcase_arabic_packaging.png` (`1654 × 1181` @ 150 DPI) — Arabic RTL + Kashida typography, Parametric Tuck-End Box Dieline (`CutContour` + `Crease`), vector QR Code & EAN-13 Barcode, live Dimensions & Path Meter
  3. `showcase_multipage.png` (`2287 × 497` @ 120 DPI) — 3-page Multipage View side-by-side with shared `AllPages` Master Layer, `VectorTable`, CMYK/Spot swatches, Sticker Sheet dieline & QR Code

## Real-World v0.2 Codec & Color Management Verification (Wave A)

Independently verified read-only by `@senior-verifier` on real-world production files and system ICC profiles:

- **Real CorelDRAW 2021 (v23 ZCF `.cdr.zip`):** `CdrZipX6` (`v2300`), **3 pages** (`210×297 mm` A4), **99 objects**, **4,746 cubic Bézier handles** (`max_err = 0.000045 mm` vs CDR `bbox`), 5 CMYK swatches, `0` warnings, `256×452` extracted preview PNG (`56,592` bytes), all 3 pages rendered cleanly (`595×842` px).
- **Real 44 MB Multipage PDF:** **60 pages**, **185 objects**, **64 raster assets**, `210.00×297.00 mm`, CMYK, imported in **784 ms** (release), zero panics/OOM.
- **Real 295 MB High-DPI PSD (`12600×17820`):** **9 layers**, **9 objects**, **9 assets**, `210.00×297.00 mm @ 1524 DPI`, **117.4 MiB** bounded streaming RGBA, imported in **2.04 s** (release), zero panics/OOM.
- **Real System ICC Profiles (`/usr/share/color/icc/`):** **52 / 52** ICC v2/v4 profiles (including `FOGRA39L`, `SWOP`, `GRACoL`, `AdobeRGB1998`, `sRGB`, `default_cmyk`) parsed with `0` errors and `0` `NaN`s.

## Deep Audit, Wave D & v0.3.1 Hardening (`336 / 336` Automated Workspace Tests — `v0.3.1`)

- **Print Export & ICC Color Fidelity (`coree-core`, `coree-io`, `coree-render`, `coree-app`):** Added clean-room ICC v2 sRGB/CMYK profile synthesis, ISO 32000-1 PDF `/Catalog /OutputIntents` embedding, arbitrary `.icc`/`.icm` custom profile attachment & live soft-proofing, Native OS Print Studio (`Ctrl+P` CUPS/Windows Spooler), shaped `TextEngine` cubic glyph curves (zero WinAnsi Tofu boxes), `/Separation` Spot plates, `/ShadingType 2/3` gradients, `/SMask` RGBA composites, EPS `ASCIIHexDecode` raster export+import & `shfill`, and SVG bitmap/vector `<pattern>` + Effects/Arrowheads.
- **ZCF & `.CDR` Codec Coverage (`coree-cdr`):** Added RIFF `arrw`/`bmpf`/`vpat`/`clrs` handlers + legacy v7–v15 `fild`/`outl` fallbacks, `[0..1]` normalized vs local-mm gradient rescaling (`rescale_normalized_fill_to_bounds`), cycle-guarded `styles/document.cdss` `"parent"` style inheritance, numeric `font_weight` (`100..=900`), `character.RTL` `bidi_font_family`, and `PowerClip` parametric shape restoration.
- **Studio Architecture, IPC & Bilingual Arabic UI (`coree-core`, `coree-render`, `coree-app`):** Modularized `state.rs` into `state/`, added persistent cross-page `Desktop` layer, `PowerClip` `lock_contents` & `Ctrl+Click` internal editing mode, `Ctrl+Shift+Q` Convert Outline to Object (`scale_with_object` PDF/EPS/Preflight parity + native DXF `CIRCLE`/`ARC`), sparse `DocumentDelta` undo/redo, `Ctrl+K` effect lowering (`lower_effects_to_primitives`), async non-blocking OS printer discovery, font quarantine badge, smart `Ctrl+D` step-and-repeat, Left-Drag + `Spacebar`/Right-Click stamp duplicate, Toolbox Page/Selection Frame double-click, `_vN` incremental save, theme-unified Toolbox icons, process-wide `OnceLock<fontdb::Database>` caching, Rayon parallel dirty tiles, single-instance TCP IPC (`--new-window`), full v2 `.coreews` & Custom Shortcut persistence, `InteractiveFill` 4-type stop insertion & focal drag, Arabic UI `unicode-bidi` / font-order shaping, and Windows Console companion `coree.com` beside `coree.exe`.
- **v0.3.1 Atomic Infrastructure, Engine, Codec & UI Polish (`BUG-106`–`BUG-116`):** Added ICC v2/v4 `mAB`/`mBA` CLUT & rendering intent adaptation, cached `SoftProofLut3D`, Philip Schneider cubic Bézier refitting (`boolean.rs`, `erase_circle`), multi-subpath 999-step Blend, C¹ Catmull-Rom Mesh Fill, `960px` SmartFill, shaped Kashida `ForceJustifyKashida`, outliner-backed `compute_separations_with_text_outliner`, clean-room `coree-cql` parser & Find/Replace UI quick filter, pure-Rust SFNT font `name` collector, gzip-compressed snapshots, `0o600`/`0o700` IPC lockfile permissions, nested `Group`/`PowerClip` world-transform & lock fixes across all canvas/tool handlers, live Print Preview sheet/imposition thumbnail, per-tool vector icons, and Arabic Docker localization.
- **Honest Clean-Room Architectural Scope Notes:** Native binary size (`coree` is ~31 MB unstripped / ~9.6 MB `.deb` package) is intentionally much smaller than >2 GB commercial suites because it links zero `.NET`/CLR, zero WebView2/CEF, zero Python/Qt runtimes, zero bundled gigabytes of royalty-free clipart/stock photos/commercial font packs/localization audio, and zero ONNX weights; rendering uses CPU multi-threaded `tiny-skia` tiles + Rayon rather than a custom `wgpu` GPU shader compositor, stylus input uses standard OS pointer events rather than direct Wintab/HID tilt hardware drivers, macros use sandboxed cross-platform Rhai instead of Windows COM/VBA, and undocumented proprietary vendor `.cdr` chunks beyond ZCF/RIFF public structures preserve verbatim via Unknown-Chunk Passthrough while expanding iteratively from real files.
