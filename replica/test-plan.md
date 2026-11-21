# Verification & End-to-End Test Plan — `coree`

All **18 core user flows (`F01`–`F18`)** from [recon.md](recon.md) are verified by automated unit, integration, adversarial security, headless UI, and real CLI binary tests across all 7 workspace crates.

## Automated Suite Summary

| Crate | Unit Tests | Integration / E2E Tests | Total Passed | Failed | Ignored |
| --- | ---: | ---: | ---: | ---: | ---: |
| `coree-core` | 28 | 16 (`comprehensive_engine_tests.rs`) | **44** | 0 | 0 |
| `coree-cdr` | 9 | 22 (`codec_integration.rs`) | **31** | 0 | 0 |
| `coree-io` | 23 | 8 (`io_integration.rs`) | **31** | 0 | 0 |
| `coree-storage` | 11 | 7 (`storage_integration.rs`) | **18** | 0 | 0 |
| `coree-render` | 1 | 18 (`render_suite.rs`) | **19** | 0 | 0 |
| `coree-prod` | 20 | 16 (`integration_production.rs`) | **36** | 0 | 0 |
| `coree-app` | 0 | 23 (`studio_integration.rs`: 15 + `e2e_workflows.rs`: 8) | **23** | 0 | 0 |
| **Workspace Total** | **92** | **110** | **202** | **0** | **0** |

Additional verification gates passing with exit code `0`:
- `cargo test --workspace --doc` — **0 failures**
- `cargo clippy --workspace --all-targets -- -D warnings` — **0 warnings**
- `cargo fmt --all -- --check` — **clean**
- `cargo build --release` + `./packaging/linux/install-linux.sh` + `~/.local/bin/coree --diagnostics` — **verified**
- `contrast.py replica/design/tokens.json` — **35/35 WCAG AA/AAA pairs passing (0 failing)**
- `imgdiff.py --mode pixel --tolerance 0` across two independent `./target/release/coree render` runs of `showcase_lenses_vectors.png` — **100.0 / 100 (`0` changed pixels, identical SHA-256)**

---

## Flow-by-Flow Test Matrix (`F01`–`F18`)

| Case ID | Flow | Type | Scenario Verified | Automated Test Reference | Result |
| --- | --- | --- | --- | --- | --- |
| `F01-H1` | F01 | Happy | Create CMYK document from Welcome Hub (`S01 -> S02`), render Welcome & Studio frames headlessly (`egui 0.37` + `egui_dock 0.22`) | `coree-app::studio_integration::theme_tokens_match_design_spec_and_headless_egui_frames_render_cleanly` | **PASS** |
| `F01-H2` | F01 | Happy | Real `coree --diagnostics` & `generate-sample` across all 7 formats (`.coree`, `cdr-riff`, `cdr-x4`, `cdr-x6`, `.cdt`, `.cmx`, `.svg`) | `coree-app::e2e_workflows::e2e_f01_f15_cdr_cdt_cmx_cpt_passthrough_cp1256_inspector_and_adversarial` | **PASS** |
| `F01-N1` | F01 | Negative | Reject non-finite/zero/negative DPI (`NaN`, `inf`, `0`, `-72`) on CLI `render` | `coree-app::studio_integration::regression_cli_format_override_dpi_validation_and_bounded_read` | **PASS** |
| `F02-H1` | F02 | Happy | Draw parametric shapes, node editing, Knife slice, Virtual Segment Delete, Smart Fill, and ALL 9 `BooleanOp` variants (`Weld`..`BreakApart`) | `coree-app::e2e_workflows::e2e_f02_f04_f06_f07_effects_mesh_smartfill_knife_vsd_and_all_9_booleans` | **PASS** |
| `F02-E1` | F02 | Edge | Knife cut passing directly through interior vertex + closed-loop `virtual_segment_delete` seam + even complex stars (`{6/2}`, `{8/3}`) | `coree-core::comprehensive_engine_tests` (regressions 3, 4, 5) | **PASS** |
| `F02-N1` | F02 | Negative | Locked objects rejected by `DeleteObjects`, `BooleanOp`, and `ConvertToCurves`; non-finite bounds rejected before `rstar` R-Tree insert | `coree-core::comprehensive_engine_tests` (regressions 2, 8) | **PASS** |
| `F03-H1` | F03 | Happy | Multi-page layout (`3` pages), Master Layer (`AllPages`), `compute_page_layouts`, and real CLI `coree render --multipage` | `coree-app::e2e_workflows::e2e_f03_f05_arabic_rtl_kashida_text_on_path_curves_table_dims_connectors_multipage` | **PASS** |
| `F03-E1` | F03 | Edge | `duplicate_page` clones full objects, `DeletePage`/`PrevPage`/`NextPage` syncs `active_layer_id`, oversized multipage auto-scales DPI <= 16384px | `coree-app::studio_integration::regression_page_switch_delete_layer_sync_and_duplicate_page` | **PASS** |
| `F03-N1` | F03 | Negative | Deleting last remaining page blocked cleanly by `Document` engine | `coree-core::document::tests` | **PASS** |
| `F04-H1` | F04 | Happy | Fountain gradients (Linear/Radial/Conical/Square), 3×3 & 4×4 object-local `MeshFill`, Spot `CutContour` outline, palette `.gpl`/`.ase`/`.pal` I/O | `coree-app::e2e_workflows::e2e_f02_f04_f06_f07_effects_mesh_smartfill_knife_vsd_and_all_9_booleans`, `coree-io::io_integration` | **PASS** |
| `F04-E1` | F04 | Edge | Translated object local-space `MeshFill` (`rows * cols`) & `InteractiveFill` inverse-transform verification | `coree-app::studio_integration::regression_local_coords_mesh_envelope_and_bitmap_padding` | **PASS** |
| `F04-N1` | F04 | Negative | Non-finite swatch channels sanitized & `MAX_PALETTE_SWATCHES` enforced on `.ase`/`.pal` imports | `coree-io::io_integration::regression_4_*`, `coree-storage::storage_integration::regression_6_*` | **PASS** |
| `F05-H1` | F05 | Happy | Arabic RTL shaping, Kashida Tatweel (`U+0640`) elongation, mixed BiDi, Fit Text to Path, `ArtisticText`/`ParagraphText` -> real glyph curves (`Ctrl+Q`), `VectorTable`, Dimensions & Connectors | `coree-app::e2e_workflows::e2e_f03_f05_arabic_rtl_kashida_text_on_path_curves_table_dims_connectors_multipage` | **PASS** |
| `F05-E1` | F05 | Edge | Arabic Kashida right-joining `'ژ'` & Lam-Alef Wasla `'لٱ'` ligature rules + `VectorTable` row insertion across merged cells | `coree-render::render_suite::regression_7_*`, `coree-core::comprehensive_engine_tests` | **PASS** |
| `F05-N1` | F05 | Negative | Editing shortcuts (`Ctrl+Z`/`Y`/`D`/`G`/`Q`) suppressed when `text_focus = true`; cyclic connectors bounded without stack overflow | `coree-app::studio_integration::regression_shortcuts_gated_when_text_focused`, `coree-core::comprehensive_engine_tests` | **PASS** |
| `F06-H1` | F06 | Happy | PowerClip container creation, clipped rendering, content extraction, and nested hierarchy preservation | `coree-app::e2e_workflows::e2e_f02_f04_f06_f07_effects_mesh_smartfill_knife_vsd_and_all_9_booleans` | **PASS** |
| `F06-N1` | F06 | Negative | Circular Group/PowerClip nesting blocked cleanly during `create_powerclip` and `import_document_into` | `coree-app::studio_integration::regression_import_cycle_guard_symbols_and_id_remapping` | **PASS** |
| `F07-H1` | F07 | Happy | All 10 live Vector Effects (`DropShadow`, `BlockShadow`, `Contour`, `Blend`, `Distort`, `Envelope` 8-pt cage, `Extrude25D`, `Perspective`, `Symmetry`, `Transparency`) rendered cleanly | `coree-app::e2e_workflows::e2e_f02_f04_f06_f07_effects_mesh_smartfill_knife_vsd_and_all_9_booleans` | **PASS** |
| `F07-E1` | F07 | Edge | Cross-tile `DropShadow` seamless parity in `DirtyTileCache`; post-fill `Extrude25D` bevel highlight; local-space 8-pt Coons Envelope cage | `coree-render::render_suite::regression_3_*`, `coree-app::studio_integration::regression_local_coords_mesh_envelope_and_bitmap_padding` | **PASS** |
| `F08-H1` | F08 | Happy | All 11 Lenses (`Brighten`, `ColorAdd`, `ColorLimit`, `CustomColorMap`, `FishEye`, `HeatMap`, `Invert`, `Magnify`, `TintedGrayscale`, `Transparency`, `Wireframe`) rendered headlessly & verified visually non-identical from baseline | `coree-app::e2e_workflows::e2e_f08_all_eleven_lenses_headless_render_and_controls` | **PASS** |
| `F08-E1` | F08 | Edge | `Frozen`, `Viewpoint` X/Y offset, `RemoveFace` mask, grouped/Symmetry lens `page_to_surface` projection, and cross-tile lens parity | `coree-app::e2e_workflows::e2e_f08_all_eleven_lenses_headless_render_and_controls`, `coree-render::render_suite::regression_4_*` | **PASS** |
| `F09-H1` | F09 | Happy | Convert vector to bitmap (4mm padded), Paint Brush, Eraser, Clone Stamp, Smudge, `RasterMask`, Cutout background removal, all 19 `RasterFilter`s, and Lanczos/Bicubic resample | `coree-app::e2e_workflows::e2e_f09_f10_bitmap_studio_and_vector_trace`, `coree-app::studio_integration::studio_app_vector_effects_bitmap_studio_trace_macros_and_async_jobs` | **PASS** |
| `F09-N1` | F09 | Negative | Malformed `RasterAsset` buffer mismatch, extreme brush coordinates/radii, and oversized resample dimensions rejected without OOM/panic | `coree-render::render_suite::regression_5_*` | **PASS** |
| `F10-H1` | F10 | Happy | Vector Trace (`QuickTrace`, `OutlineTrace`, `CenterlineTrace`) verified via Rust API & real CLI `coree trace --mode quick\|outline\|centerline` | `coree-app::e2e_workflows::e2e_f09_f10_bitmap_studio_and_vector_trace` | **PASS** |
| `F10-E1` | F10 | Edge | `LineArt` color quantization cap & 1px border-padded Zhang-Suen centerline thinning | `coree-render::render_suite::regression_7_*` | **PASS** |
| `F11-H1` | F11 | Happy | CMYK + Spot (`CutContour`) Separations, Overprint vs Knockout simulation, N-Up, Saddle-Stitch Booklet (with odd-page auto-pad), Tiling, and Print Marks | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_full_production_pipeline_and_rhai_macros` | **PASS** |
| `F11-N1` | F11 | Negative | Unbounded N-Up slot count & tiny Tiling tile size rejected with `ProdError::InvalidParameter` | `coree-prod::integration_production::regression_3_*` | **PASS** |
| `F12-H1` | F12 | Happy | Document Preflight (plain text + CLI `--json`) and `collect_for_output` bundling `manifest.json`, `preflight.txt`, fonts, and images | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_full_production_pipeline_and_rhai_macros` | **PASS** |
| `F12-E1` | F12 | Edge | Collision-safe font collection + `VectorTable`/`DimensionObject` font & spot ink discovery + PowerClip bleed clipping | `coree-prod::integration_production::regression_5_*`, `regression_7_*` | **PASS** |
| `F13-H1` | F13 | Happy | One-click `CutContour` spot hairline + ALL 4 Parametric Dielines (`TuckEndBox`, `Envelope`, `ShoppingBag`, `StickerSheet`) + Path Length & Area Meter (`ProductionMetrics`) | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_full_production_pipeline_and_rhai_macros` | **PASS** |
| `F13-N1` | F13 | Negative | Negative/zero/non-finite dieline dimensions & huge `StickerSheet` grids (>10,000 cells) rejected cleanly | `coree-prod::integration_production::regression_3_*` | **PASS** |
| `F14-H1` | F14 | Happy | Vector QR Code + all 5 1D Barcodes (`EAN-13`, `EAN-8`, `UPC-A`, `Code128`, `Code39`) + Bilingual Arabic/English UTF-8 CSV Print Merge (API + CLI) + Find & Replace | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_full_production_pipeline_and_rhai_macros` | **PASS** |
| `F14-N1` | F14 | Negative | Invalid EAN-13 checksum rejected; non-ASCII multibyte barcode strings & Arabic object names handled without UTF-8 boundary panics | `coree-prod::integration_production::regression_1_*`, `regression_2_*` | **PASS** |
| `F15-H1` | F15 | Happy | `.cdr` (RIFF v12, ZIP X4, ZIP X6+) + `.cdt` + `.cmx` (RIFF & RIFX 16/32-bit) + `.cpt` (native & RIFF) round-trip + byte-exact `XDOC`/`XOBJ` Unknown-Chunk Passthrough + CP1256 Arabic decoding + CDR Inspector (`--json`) | `coree-app::e2e_workflows::e2e_f01_f15_cdr_cdt_cmx_cpt_passthrough_cp1256_inspector_and_adversarial` | **PASS** |
| `F15-N1` | F15 | Negative | Zip-Bomb, `LIST cmpr` Zlib-Bomb, Zip-Slip (`../../etc/passwd`), truncated header/chunk, garbage, deep nesting (200 > 64), huge 65536² bitmaps, forged `compressed_size`, and PNG-in-ZIP RAM bombs rejected cleanly (API + real CLI non-zero exit) | `coree-app::e2e_workflows::e2e_f01_f15_cdr_cdt_cmx_cpt_passthrough_cp1256_inspector_and_adversarial`, `coree-cdr::codec_integration`, `coree-io::io_integration` | **PASS** |
| `F16-H1` | F16 | Happy | Production exports verified: `.coree`, `.cdr`, `.cdt`, `.cmx`, `.pdf` (DeviceCMYK + Crop/Registration Marks), `.eps`, `.svg`, `.dxf`, `.plt` (HPGL), `.png`, `.jpg`, `.webp` + atomic temp-file writes + async background export job pool | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_full_production_pipeline_and_rhai_macros`, `coree-app::studio_integration::cli_generate_sample_inspect_and_universal_convert` | **PASS** |
| `F16-E1` | F16 | Edge | Deterministic visual rendering check: two independent renders (in-process + real CLI binary) produce **100% pixel-identical RGBA & byte-for-byte identical PNG** | `coree-app::e2e_workflows::e2e_deterministic_rendering_byte_and_pixel_identical` | **PASS** |
| `F17-H1` | F17 | Happy | Record `CommandHistory` into `.rhai` script, execute sandboxed custom & built-in macros (`recolor-cmyk`) via API & real CLI `coree run-macro` | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_full_production_pipeline_and_rhai_macros` | **PASS** |
| `F17-N1` | F17 | Negative | Infinite loop Rhai macro (`while true`) aborted cleanly by `max_operations` sandbox limit; non-finite macro args rejected | `coree-app::e2e_workflows::e2e_f11_f12_f13_f14_f16_f17_full_production_pipeline_and_rhai_macros`, `coree-prod::integration_production::regression_8_*` | **PASS** |
| `F18-H1` | F18 | Happy | SQLite WAL crash recovery replays snapshot + `command_journal` after simulated crash; Command Palette (`Ctrl+K`) fuzzy search & execution | `coree-app::e2e_workflows::e2e_f18_sqlite_wal_crash_recovery_post_save_and_undo_checkpoint`, `coree-app::studio_integration::studio_app_all_eleven_lenses_find_replace_and_command_palette` | **PASS** |
| `F18-E1` | F18 | Edge | Post-save session rotation preserves post-save edits after crash; post-undo checkpoint snapshot prevents undone operations from resurrecting; clean shutdown clears unclean sessions | `coree-app::e2e_workflows::e2e_f18_sqlite_wal_crash_recovery_post_save_and_undo_checkpoint` | **PASS** |
| `F18-N1` | F18 | Negative | Corrupted snapshot SHA-256 checksum rejected during recovery; bounded snapshot read cap enforced | `coree-storage::autosave::tests::crash_recovery_detects_corrupted_snapshot_sha256` | **PASS** |
