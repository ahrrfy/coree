# Verification & End-to-End Test Plan — `coree` (`v0.2.0`)

All **18 core user flows (`F01`–`F18`)** from [recon.md](recon.md) plus all **v0.2 expansion flows** (`V02-01`–`V02-08`: ZCF CDR X6–2025, PDF/AI/EPS/DXF/PLT/PSD importers, ICC v2/v4 soft-proofing, Raster Healing/Dodge/Burn/FloodFill, PostScript Fills, Autofit Page, Glyphs Docker, Laser & Engraving Plugins, 62-tool live audit, and Linux/Windows OS packaging) are verified by automated unit, integration, adversarial security, headless UI, real-file read-only benchmarks, and packaging validation checks across all 7 workspace crates.

## Automated Suite Summary (`v0.2.0` Final Verified Workspace)

| Crate | Unit Tests | Integration / E2E Tests | Total Passed | Failed | Ignored |
| --- | ---: | ---: | ---: | ---: | ---: |
| `coree-core` | 36 | 19 (`comprehensive_engine_tests.rs`, ICC v2/v4, PostScript fills, Autofit Page) | **55** | 0 | 0 |
| `coree-cdr` | 14 | 26 (`codec_integration.rs`, ZCF v16–26, preview extraction, audit regressions) | **40** | 0 | 0 |
| `coree-io` | 42 | 10 (`io_integration.rs`, PDF/AI, EPS, DXF, PLT, PSD, adversarial bombs) | **52** | 0 | 0 |
| `coree-storage` | 11 | 7 (`storage_integration.rs`) | **18** | 0 | 0 |
| `coree-render` | 1 | 23 (`render_suite.rs`, ICC soft-proof, Healing/Dodge/Burn/FloodFill, PostScript fills, Glyphs) | **24** | 0 | 0 |
| `coree-prod` | 31 | 30 (`integration_production.rs`: 16 + `laser_and_plugins_suite.rs`: 14) | **61** | 0 | 0 |
| `coree-app` | 9 | 39 (`studio_integration.rs`: 15 + `e2e_workflows.rs`: 8 + `tools_62_audit.rs`: 16) | **48** | 0 | 0 |
| **Workspace Total (`v0.2.0`)** | **144** | **154** | **298** | **0** | **0** |

Additional verification gates passing with exit code `0`:
- `cargo test --workspace --doc` — **0 failures**
- `cargo clippy --workspace --all-targets -- -D warnings` — **0 warnings**
- `cargo fmt --all -- --check` — **clean**
- `#![forbid(unsafe_code)]` enforced across all 7 crates (0 `unsafe` blocks)
- `contrast.py replica/design/tokens.json` — **35/35 WCAG AA/AAA pairs passing (0 failing)**
- `imgdiff.py --mode pixel --tolerance 0` across two independent release renders of `showcase_lenses_vectors.png` — **100.0 / 100 (`0` changed pixels, identical SHA-256)**
- `parity.py replica/features.csv` — **100.0 / 100 (`68/68` must-haves done, `0` missing)**
- Packaging & OS Integration validators:
  - `xml.etree.ElementTree.parse` on `packaging/linux/coree-mime.xml`, `packaging/linux/flatpak/io.github.ahrrfy.coree.metainfo.xml`, `packaging/windows/coree.exe.manifest`, `packaging/windows/wix/main.wxs` — **PASS**
  - `desktop-file-validate packaging/linux/coree.desktop` — **PASS**
  - `appstreamcli validate --no-net packaging/linux/flatpak/io.github.ahrrfy.coree.metainfo.xml` — **PASS**
  - Isolated `update-mime-database` + `gio info` verification proving X6–2025 `.cdr`, `.cdr.zip`, X4 `.cdr`, and RIFF `.cdr` classify as `application/vnd.corel-draw` with `coree` icon (never `application/zip`) while normal `.zip` stays `application/zip` — **PASS**
  - `packaging/linux/build-deb.sh` (`dpkg-deb --build --root-owner-group` + `dpkg-deb --info`) — **PASS**
  - `packaging/windows/coree.ico` binary header + 4 PNG-in-ICO frames (`16`, `32`, `48`, `256` RGBA + IHDR CRC32) — **PASS**
  - PyYAML syntax validation on `.github/workflows/ci.yml`, `.github/workflows/release.yml`, and `packaging/linux/flatpak/io.github.ahrrfy.coree.yml` — **PASS**

---

## Core Flow-by-Flow Test Matrix (`F01`–`F18`)

| Case ID | Flow | Type | Scenario Verified | Automated Test Reference | Result |
| --- | --- | --- | --- | --- | --- |
| `F01-H1` | F01 | Happy | Create CMYK document from Welcome Hub (`S01 -> S02`), render Welcome & Studio frames headlessly (`egui 0.37` + `egui_dock 0.22`) | `coree-app::studio_integration::theme_tokens_match_design_spec_and_headless_egui_frames_render_cleanly` | **PASS** |
| `F01-H2` | F01 | Happy | Real `coree --diagnostics` & `generate-sample` across all 7 formats (`.coree`, `cdr-riff`, `cdr-x4`, `cdr-x6`, `.cdt`, `.cmx`, `.svg`) | `coree-app::e2e_workflows::e2e_f01_f15_cdr_cdt_cmx_cpt_passthrough_cp1256_inspector_and_adversarial` | **PASS** |
| `F01-N1` | F01 | Negative | Reject non-finite/zero/negative DPI (`NaN`, `inf`, `0`, `-72`) on CLI `render` | `coree-app::studio_integration::regression_cli_format_override_dpi_validation_and_bounded_read` | **PASS** |
| `F02-H1` | F02 | Happy | Draw parametric shapes, node editing, Knife slice, Virtual Segment Delete, Smart Fill, and ALL 9 `BooleanOp` variants (`Weld`..`BreakApart`) | `coree-app::e2e_workflows::e2e_f02_f04_f06_f07_effects_mesh_smartfill_knife_vsd_and_all_9_booleans` | **PASS** |
| `F02-E1` | F02 | Edge | Knife cut passing directly through interior vertex + closed-loop `virtual_segment_delete` seam + even complex stars (`{6/2}`, `{8/3}`) | `coree-core::comprehensive_engine_tests` (regressions 3, 4, 5) | **PASS** |
| `F02-N1` | F02 | Negative | Locked objects rejected by `DeleteObjects`, `BooleanOp`, and `ConvertToCurves`; non-finite bounds rejected before `rstar` R-Tree insert | `coree-core::comprehensive_engine_tests` (regressions 2, 8) | **PASS** |
| `F03-H1` | F03 | Happy | Multi-page layout (`3` pages), Master Layer (`AllPages`), `compute_page_layouts`, `autofit_page_to_content`, and real CLI `coree render --multipage` | `coree-app::e2e_workflows::e2e_f03_f05_*`, `coree-core::comprehensive_engine_tests` | **PASS** |
| `F03-E1` | F03 | Edge | `duplicate_page` clones full objects, `DeletePage`/`PrevPage`/`NextPage` syncs `active_layer_id`, oversized multipage auto-scales DPI <= 16384px | `coree-app::studio_integration::regression_page_switch_delete_layer_sync_and_duplicate_page` | **PASS** |
| `F03-N1` | F03 | Negative | Deleting last remaining page blocked cleanly; `autofit_page_to_content` handles empty/OOB pages & hidden objects cleanly | `coree-core::document::tests` | **PASS** |
| `F04-H1` | F04 | Happy | Fountain gradients, 3×3 & 4×4 `MeshFill`, 10 analytic SDF `PostScriptFillPreset`s, Spot `CutContour` outline, palette `.gpl`/`.ase`/`.pal` I/O | `coree-app::e2e_workflows::e2e_f02_f04_f06_f07_*`, `coree-render::render_suite`, `coree-io::io_integration` | **PASS** |
| `F04-E1` | F04 | Edge | Translated object local-space `MeshFill` (`rows * cols`) & `InteractiveFill` inverse-transform verification | `coree-app::studio_integration::regression_local_coords_mesh_envelope_and_bitmap_padding` | **PASS** |
| `F04-N1` | F04 | Negative | Non-finite swatch channels sanitized & `MAX_PALETTE_SWATCHES` enforced on `.ase`/`.pal` imports | `coree-io::io_integration::regression_4_*`, `coree-storage::storage_integration::regression_6_*` | **PASS** |
| `F05-H1` | F05 | Happy | Arabic RTL shaping, Kashida Tatweel (`U+0640`), mixed BiDi, Fit Text to Path, `ArtisticText`/`ParagraphText` -> real glyph curves (`Ctrl+Q`), Glyphs Catalog API, `VectorTable`, Dimensions & Connectors | `coree-app::e2e_workflows::e2e_f03_f05_*`, `coree-render::render_suite` | **PASS** |
| `F05-E1` | F05 | Edge | Arabic combining diacritics anchored on `TextOnPath`, Kashida `'ژ'`/`'لٱ'` rules, `VectorTable` merge invariant repair | `coree-render::render_suite`, `coree-core::comprehensive_engine_tests` | **PASS** |
| `F05-N1` | F05 | Negative | Editing shortcuts (`Ctrl+Z`/`Y`/`D`/`G`/`Q`) suppressed when `text_focus = true`; cyclic connectors bounded without stack overflow | `coree-app::studio_integration::regression_shortcuts_gated_when_text_focused`, `coree-core::comprehensive_engine_tests` | **PASS** |
| `F06-H1` | F06 | Happy | PowerClip container creation, clipped rendering, world-transform preservation, content extraction, and nested hierarchy preservation | `coree-app::e2e_workflows::e2e_f02_f04_f06_f07_*`, `coree-render::render_suite` | **PASS** |
| `F06-N1` | F06 | Negative | Circular Group/PowerClip nesting blocked cleanly during `create_powerclip` and `import_document_into` | `coree-app::studio_integration::regression_import_cycle_guard_symbols_and_id_remapping` | **PASS** |
| `F07-H1` | F07 | Happy | All 10 live Vector Effects (`DropShadow`, `BlockShadow`, `Contour`, `Blend`, `Distort`, `Envelope` 8-pt cage, `Extrude25D`, `Perspective`, `Symmetry`, `Transparency`) rendered cleanly | `coree-app::e2e_workflows::e2e_f02_f04_f06_f07_*` | **PASS** |
| `F07-E1` | F07 | Edge | Cross-tile `DropShadow` seamless parity in `DirtyTileCache`; post-fill `Extrude25D` bevel highlight; local-space 8-pt Coons Envelope cage | `coree-render::render_suite::regression_3_*`, `coree-app::studio_integration::regression_local_coords_mesh_envelope_and_bitmap_padding` | **PASS** |
| `F08-H1` | F08 | Happy | All 11 Lenses (`Brighten`, `ColorAdd`, `ColorLimit`, `CustomColorMap`, `FishEye`, `HeatMap`, `Invert`, `Magnify`, `TintedGrayscale`, `Transparency`, `Wireframe`) rendered headlessly & verified visually non-identical from baseline | `coree-app::e2e_workflows::e2e_f08_all_eleven_lenses_headless_render_and_controls` | **PASS** |
| `F08-E1` | F08 | Edge | `Frozen`, `Viewpoint` X/Y offset, warped `RemoveFace` mask on `Magnify`/`FishEye`, grouped/Symmetry lens `page_to_surface` projection, and cross-tile parity | `coree-app::e2e_workflows::e2e_f08_*`, `coree-render::render_suite` | **PASS** |
| `F09-H1` | F09 | Happy | Convert vector to bitmap (4mm padded), Paint, Eraser, Clone Stamp, Smudge, **Healing Brush**, **Dodge/Burn**, **Raster Flood Fill**, `RasterMask`, Cutout, all 19 `RasterFilter`s, Lanczos/Bicubic resample | `coree-app::e2e_workflows::e2e_f09_f10_*`, `coree-render::render_suite` | **PASS** |
| `F09-N1` | F09 | Negative | Malformed `RasterAsset` buffer mismatch, extreme brush coords/radii, and oversized resample dimensions rejected without OOM/panic | `coree-render::render_suite::regression_5_*` | **PASS** |
| `F10-H1` | F10 | Happy | Vector Trace (`QuickTrace`, `OutlineTrace`, `CenterlineTrace`) verified via Rust API & real CLI `coree trace --mode quick\|outline\|centerline` | `coree-app::e2e_workflows::e2e_f09_f10_bitmap_studio_and_vector_trace` | **PASS** |
| `F10-E1` | F10 | Edge | `LineArt` color quantization cap & 1px border-padded Zhang-Suen centerline thinning | `coree-render::render_suite::regression_7_*` | **PASS** |
| `F11-H1` | F11 | Happy | CMYK + Spot (`CutContour`) Separations, Overprint vs Knockout simulation, N-Up, Saddle-Stitch Booklet (with odd-page auto-pad), Tiling, and Print Marks | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_*` | **PASS** |
| `F11-N1` | F11 | Negative | Unbounded N-Up slot count & tiny Tiling tile size rejected with `ProdError::InvalidParameter` | `coree-prod::integration_production::regression_3_*` | **PASS** |
| `F12-H1` | F12 | Happy | Document Preflight (plain text + CLI `--json`) and `collect_for_output` bundling `manifest.json`, `preflight.txt`, fonts, and images | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_*` | **PASS** |
| `F12-E1` | F12 | Edge | Collision-safe font collection + `VectorTable`/`DimensionObject` font & spot ink discovery + PowerClip bleed clipping | `coree-prod::integration_production::regression_5_*`, `regression_7_*` | **PASS** |
| `F13-H1` | F13 | Happy | One-click `CutContour` spot hairline + ALL 4 Parametric Dielines (`TuckEndBox`, `Envelope`, `ShoppingBag`, `StickerSheet`) + Path Length & Area Meter (`ProductionMetrics`) | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_*` | **PASS** |
| `F13-N1` | F13 | Negative | Negative/zero/non-finite dieline dimensions & huge `StickerSheet` grids (>10,000 cells) rejected cleanly | `coree-prod::integration_production::regression_3_*` | **PASS** |
| `F14-H1` | F14 | Happy | Vector QR Code + all 5 1D Barcodes (`EAN-13`, `EAN-8`, `UPC-A`, `Code128`, `Code39`) + Bilingual Arabic/English UTF-8 CSV Print Merge (API + CLI) + Find & Replace | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_*` | **PASS** |
| `F14-N1` | F14 | Negative | Invalid EAN-13 checksum rejected; non-ASCII multibyte barcode strings & Arabic object names handled without UTF-8 boundary panics | `coree-prod::integration_production::regression_1_*`, `regression_2_*` | **PASS** |
| `F15-H1` | F15 | Happy | `.cdr` (RIFF v12, ZIP X4, authentic ZCF X6–2025 v16–26 + `document.cdss` + `docPalette.xml` + `extract_cdr_preview_png`) + `.cdt` + `.cmx` + `.cpt` round-trip + Unknown-Chunk Passthrough + CP1256 + CDR Inspector | `coree-cdr::codec_integration` (40 tests), `coree-app::e2e_workflows::e2e_f01_f15_*` | **PASS** |
| `F15-N1` | F15 | Negative | Zip-Bomb, `LIST cmpr` Zlib-Bomb, Zip-Slip, `loda` 512x offset amplification, `trfd` `NaN`/`Inf` clamp panics, truncated/deep-nesting/huge-bitmap inputs rejected cleanly | `coree-cdr::codec_integration::regression_1..7`, `coree-io::io_integration` | **PASS** |
| `F16-H1` | F16 | Happy | Bidirectional production I/O verified: `.coree`, `.cdr`, `.cdt`, `.cmx`, `.pdf`/`.ai`, `.eps`, `.svg`, `.dxf`, `.plt`, `.psd`/`.psb`, `.gcode`, `.png`/`.jpg`/`.webp` + atomic saves | `coree-io::io_integration` (52 tests), `coree-app::studio_integration` | **PASS** |
| `F16-E1` | F16 | Edge | Deterministic visual rendering check: two independent renders produce **100% pixel-identical RGBA & byte-for-byte identical PNG** | `coree-app::e2e_workflows::e2e_deterministic_rendering_byte_and_pixel_identical` | **PASS** |
| `F17-H1` | F17 | Happy | Record `CommandHistory` into `.rhai` script, execute sandboxed custom & built-in macros (`recolor-cmyk`) via API & real CLI `coree run-macro` | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_*` | **PASS** |
| `F17-N1` | F17 | Negative | Infinite loop Rhai macro (`while true`) aborted cleanly by `max_operations` sandbox limit; non-finite macro args rejected | `coree-app::e2e_workflows::e2e_f11_*`, `coree-prod::integration_production::regression_8_*` | **PASS** |
| `F18-H1` | F18 | Happy | SQLite WAL crash recovery replays snapshot + `command_journal` after simulated crash; Command Palette (`Ctrl+K`) fuzzy search & execution | `coree-app::e2e_workflows::e2e_f18_*`, `coree-app::studio_integration` | **PASS** |
| `F18-E1` | F18 | Edge | Post-save session rotation preserves post-save edits after crash; post-undo checkpoint prevents undone ops from resurrecting; clean shutdown clears unclean sessions | `coree-app::e2e_workflows::e2e_f18_sqlite_wal_crash_recovery_post_save_and_undo_checkpoint` | **PASS** |
| `F18-N1` | F18 | Negative | Corrupted snapshot SHA-256 checksum rejected during recovery; bounded snapshot read cap enforced | `coree-storage::autosave::tests::crash_recovery_detects_corrupted_snapshot_sha256` | **PASS** |

---

## v0.2 Expansion Verification Matrix (`V02-01`–`V02-08`)

| Case ID | Capability | Type | Scenario Verified | Automated / Real-File Reference | Result |
| --- | --- | --- | --- | --- | --- |
| `V02-01` | ZCF CDR X6–2025 & Preview | Happy / Real-File | Real v23 `.cdr.zip` (`درع الشكر والتقدير.cdr.zip`): 3 pages, 99 objects, 4,746 Bezier handles (`max_err = 0.000045 mm`), 5 CMYK swatches, `256×452` preview PNG, all 3 pages rendered cleanly | `coree-cdr::codec_integration` + `@senior-verifier` read-only real-file verification | **PASS** |
| `V02-02` | PDF/AI, EPS, DXF, PLT, PSD Importers | Happy / Real-File / Adversarial | Real 44 MB 60-page PDF (`784 ms`), real 295 MB 9-layer PSD (`2.04 s`, `117.4 MiB` bounded RGBA), EPS/DXF/PLT round-trips + 13 adversarial bomb/cycle/UTF-8/DAG regressions | `coree-io` (52/52 tests) + `@senior-verifier` read-only verification | **PASS** |
| `V02-03` | ICC v2/v4 & Soft-Proofing | Happy / Real-File / Adversarial | 52/52 real system ICC profiles under `/usr/share/color/icc/` parse cleanly with 0 `NaN`; live CMYK soft-proof LUT & gamut warning overlay; `mluc`/`profile_size` DoS regressions | `coree-core::icc` + `coree-render::render_suite` (79/79 tests) | **PASS** |
| `V02-04` | Raster Brushes, PostScript Fills, Autofit, Glyphs | Happy | BT.709 Healing Brush, Dodge/Burn, scanline Flood Fill, all 10 analytic SDF PostScript fills, `autofit_page_to_content`, and live cmap Glyphs catalog | `coree-core` + `coree-render` (79/79 tests) | **PASS** |
| `V02-05` | Laser Cutting, Engraving & Plugins | Happy / Adversarial | Dedupe, join, kerf offset, rotated/cross hatch, inner-loops-first, travel opt, weeding box, GRBL/Marlin/Ruida G-Code, DXF R12, PLT, 7 built-in plugins (`LightBurn`..`HPGL`) + 7 security/geometry regressions | `coree-prod::laser_and_plugins_suite` (61/61 tests) | **PASS** |
| `V02-06` | 62 Live Tools & Workspaces | Happy | All 62 tools across 15 Toolbox flyout groups, muscle-memory shortcuts (`P`, `L/R/T/B/C/E`, `F2-F4`, `F9`, `Space`, `Ctrl+K/L/Q/D/R/J`), 6 Workspace presets + `.coreews` JSON round-trip | `coree-app::tools_62_audit` (Stage B1) | **PASS** |
| `V02-07` | Linux OS Integration & Packages | Packaging Gate | `coree-mime.xml` + isolated `gio info` confirms zero ZIP-icon misclassification on X6/X4/RIFF `.cdr` & `.cdr.zip`; `coree.desktop`, `io.github.ahrrfy.coree.metainfo.xml`, `io.github.ahrrfy.coree.yml`, `build-deb.sh` (`dpkg-deb`) validated | `desktop-file-validate`, `appstreamcli validate --no-net`, `update-mime-database`, `dpkg-deb` | **PASS** |
| `V02-08` | Windows Packaging & Release CI | Packaging Gate | `coree.ico` (4 PNG frames `16/32/48/256`, CRC OK), `coree.exe.manifest` (`PerMonitorV2` + `UTF-8`), `coree.rc`, `wix/main.wxs` XML valid, `register-file-associations.cmd`, `.github/workflows/{ci,release}.yml` YAML valid & 0 brand hits | Python binary/XML/YAML validators + `sweep.py` | **PASS** |
