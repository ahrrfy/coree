# Bug & Audit Remediation Register — `coree`

Every crate underwent independent adversarial code, security, geometry, and runtime auditing by `@verifier` prior to milestone sign-off. All **46 bugs** identified across Waves 1–3 were fixed, covered by targeted regression tests, and independently re-verified before landing on `main`.

- **Open S1 (Critical / Crash / Security) bugs:** **0**
- **Open S2 (High / Functional / Data Integrity) bugs:** **0**
- **Total Resolved Bugs Across Waves 1–3:** **46** (all covered by automated regression tests)

---

## Summary by Wave & Commit

| Wave | Crate(s) | Commit SHA | Bugs Fixed | Verification Status |
| --- | --- | --- | --- | --- |
| Wave 1 | `coree-core` | `c5edb05` | 9 | Verified (`44/44` tests pass) |
| Wave 2 | `coree-prod` | `8cb7354` | 9 | Verified (`36/36` tests pass) |
| Wave 2 | `coree-cdr` | `8cb7354` | 6 | Verified (`31/31` tests pass) |
| Wave 2 | `coree-io`, `coree-storage` | `8cb7354` | 7 (4 + 3) | Verified (`31/31` + `18/18` tests pass) |
| Wave 2 | `coree-render` | `8cb7354` | 7 | Verified (`19/19` tests pass) |
| Wave 3 | `coree-app` | `b44ba59` | 8 | Verified (`23/23` tests pass incl. Wave 4 E2E) |
| **Total** | **All 7 Crates** | `c5edb05`..`b44ba59` | **46** | **0 open S1/S2 — `202/202` workspace tests pass** |

---

## Wave 1 — `coree-core` (9 Fixes in `c5edb05`)

| ID | Severity | Location | Defect Summary | Resolution & Regression Coverage |
| --- | --- | --- | --- | --- |
| BUG-001 | S1 | `coree-core/src/document.rs` | Self-referencing or mutual cyclic `ConnectorObject` reset recursion depth to `0` in `object_bounds`, causing stack overflow, and double-transformed world endpoints. | Propagated recursion `depth + 1` via internal bounds helper and removed redundant world transform on routed endpoints. |
| BUG-002 | S1 | `coree-core/src/document.rs`, `math.rs` | Non-finite (`NaN`/`Inf`) bounds in `Affine2D::transform_rect` propagated into `rstar::RTree::bulk_load`, panicking spatial index construction. | Filtered non-finite bounds in `object_bounds_inner` and `build_spatial_index`. |
| BUG-003 | S2 | `coree-core/src/geometry.rs` | Even complex stars (`n = 6, 8, 10, ...`) computed step `n / 2`, producing degenerate 2-point chords and empty paths. | Updated step calculation to `((n - 1) / 2).max(2)` (producing valid `{6/2}`, `{8/3}` stars). |
| BUG-004 | S2 | `coree-core/src/geometry.rs` | Closed-loop `virtual_segment_delete` missed symmetric wrap-around edge `(n-2, 0)` (ghost seam cut) and shifted subpath indices when filtering degenerate subpaths. | Added symmetric seam check and preserved original subpath indices. |
| BUG-005 | S2 | `coree-core/src/geometry.rs` | `knife_cut` dropped cuts passing directly through interior vertices (`segment_t ≈ 0` / `1`). | Handled vertex boundary splits cleanly on open and closed paths. |
| BUG-006 | S2 | `coree-core/src/table.rs` | `VectorTable::insert_row`/`insert_col` left stale `merged_into` pointers outside truncated spans; zero `row_span`/`col_span` risked underflow; unbounded dimensions risked OOM. | Strengthened `repair_merge_invariants()`, guarded spans `>= 1`, and capped dimensions to `MAX_DIM = 200`. |
| BUG-007 | S2 | `coree-core/src/document.rs` | Nested `ungroup_object`/`extract_powerclip` orphaned children; `convert_to_curves` on PowerClip leaked contents; `create_powerclip` didn't deduplicate; base64 decoder accepted malformed padding. | Preserved parent hierarchy on ungroup/extract, detached prior parents, deduplicated PowerClip contents, and hardened base64 padding validation. |
| BUG-008 | S2 | `coree-core/src/command.rs`, `boolean.rs` | `DeleteObjects`/`BooleanOp`/`ConvertToCurves` bypassed `obj.locked`; `undo`/`redo` allowed calls during active transactions; Rhai export flattened CMYK/Spot fills to sRGB hex; `Simplify` overwrote per-object styles. | Enforced `obj.locked`, rejected `undo`/`redo` while transaction active, preserved CMYK/Spot via JSON fallback in Rhai export, returned explicit Rhai eval errors, and kept per-object styles in `Simplify`. |
| BUG-009 | S3 | `coree-core/src/dimension.rs`, `math.rs` | `Angular` dimension formatted `"NaN°"` on non-finite points; `Affine2D::decompose` returned inverted angle sign when `sx == 0`. | Added finiteness guard on angular legs and corrected `(-c).atan2(d)` branch. |

---

## Wave 2 — `coree-prod` (9 Fixes in `8cb7354`)

| ID | Severity | Location | Defect Summary | Resolution & Regression Coverage |
| --- | --- | --- | --- | --- |
| BUG-010 | S1 | `coree-prod/src/print_merge.rs` | `strip_prefix_ignore_ascii_case` sliced by byte index across multibyte UTF-8 Arabic object names, causing a panic. | Replaced with byte-slice prefix comparison (`regression_1`). |
| BUG-011 | S1 | `coree-prod/src/barcode.rs` | `encode_ean13`/`encode_ean8`/`encode_upca` sliced strings before checking `is_ascii()`, panicking on multibyte UTF-8 inputs of matching byte length. | Rejected non-ASCII payloads upfront before slicing (`regression_2`). |
| BUG-012 | S1 | `coree-prod/src/find_replace.rs`, `barcode.rs`, `cut_packaging.rs`, `prepress.rs` | Unbounded OOM/overflow vectors in regex class expansion, QR `quiet_zone_modules`/`size_mm` weld scaling, `StickerSheet` cell counts, and N-Up/Tiling imposition counts. | Switched regex classes to compact `(char, char)` ranges and enforced explicit checked caps (`MAX_QUIET_ZONE_MODULES = 64`, `MAX_IMPOSITION_SLOTS_PER_SHEET = 10_000`, `StickerSheet <= 10_000`) (`regression_3`). |
| BUG-013 | S2 | `coree-prod/src/prepress.rs` | Gradient separations evaluated local gradient coordinates against world coordinates without `inv_world_xf`. | Applied inverse world transform when sampling gradients (`regression_4`). |
| BUG-014 | S2 | `coree-prod/src/preflight.rs`, `cut_packaging.rs` | Grouped and PowerClipped objects used local bounds instead of world/clipped bounds for bleed checks and metrics. | Propagated world transforms and PowerClip clip intersections (`regression_5`). |
| BUG-015 | S2 | `coree-prod/src/find_replace.rs` | Exact `ColorMatch` cross-matched `Color::Spot` (e.g. `CutContour`) against process CMYK colors sharing the same fallback. | Separated Spot vs process inks under exact tolerance (`<= 1e-4`) (`regression_6`). |
| BUG-016 | S2 | `coree-prod/src/collect.rs` | Font destination filename collisions overwrote files; `VectorTable` cells and `DimensionObject`s were missed in font/spot collection. | Added collision-safe font filenames and traversed tables, dimensions, and text runs (`regression_7`). |
| BUG-017 | S2 | `coree-prod/src/macros.rs` | `align_grid` and `set_outline_mm` accepted non-finite/negative arguments; `clean_empty_layers` could remove locked layers. | Added finite/non-negative validation and skipped locked layers (`regression_8`). |
| BUG-018 | S2 | `coree-prod/src/print_merge.rs` | Grouped dynamic barcode/QR placeholders were skipped and one branch lacked an empty-resolved guard. | Recursed into groups during merge substitution and guarded empty payloads (`regression_9`). |

---

## Wave 2 — `coree-cdr` (6 Fixes in `8cb7354`)

| ID | Severity | Location | Defect Summary | Resolution & Regression Coverage |
| --- | --- | --- | --- | --- |
| BUG-019 | S1 | `coree-cdr/src/lib.rs`, `inspector.rs`, `cdr_zip.rs`, `riff.rs`, `cdr_riff.rs`, `cmx.rs`, `cpt.rs` | Unbounded `std::fs::read` on disk imports/inspections; nested `LIST cmpr` reset `max_uncompressed_bytes` budget; multi-DIB/CPT allocations weren't cumulatively deducted. | Enforced `read_file_bounded` and shared a single cumulative document budget across ZIP, RIFF `LIST cmpr`, DIBs, and CPT layers (`regression_1`). |
| BUG-020 | S1 | `coree-cdr/src/cdr_zip.rs` | Duplicate `dataFileList.dat` references amplified CPU/RAM; duplicate case-insensitive ZIP paths silently overwrote; forged central-directory `compressed_size` weakened ratio checks. | Deduplicated `dataFileList.dat`, rejected duplicate ZIP entries, and clamped/validated `compressed_size` against archive length (`regression_2`). |
| BUG-021 | S1 | `coree-cdr/src/cmx.rs` | `RIFX` big-endian CMX files were read with hardcoded little-endian helpers; page/layer transitions leaked unclosed groups and orphaned groups outside layers. | Honored `RiffEndian` throughout CMX readers/writers and flushed unclosed groups cleanly (`regression_3`). |
| BUG-022 | S2 | `coree-cdr/src/cdr_riff.rs` | `LIST cmpr` parsed before sibling resource lists resolved IDs; unknown `LIST`s inside `doc` were dropped or flattened without child FourCC framing. | Unwrapped `cmpr` prior to Pass 1 (resources) / Pass 2 (pages) and preserved nested unknown `LIST`s verbatim (`regression_4`). |
| BUG-023 | S2 | `coree-cdr/src/cdr_riff.rs` | `decode_dib_to_rgba` mishandled 32-bit `BI_BITFIELDS` masks and accepted non-32-bit `BI_BITFIELDS`. | Properly parsed and applied 32-bit RGB channel masks and rejected unsupported `bpp` (`regression_5`). |
| BUG-024 | S2 | `coree-cdr/src/lib.rs`, `cmx.rs`, `inspector.rs` | `detect_format` disagreed with `import`/`inspect` on truncated 12-byte headers; CMX tables could emit `u16::MAX` overflow index; runtime reports contained trademarked strings. | Unified `detect_format` validation, bounded CMX table indices, and scrubbed runtime report labels (`regression_6`). |

---

## Wave 2 — `coree-io` & `coree-storage` (7 Fixes in `8cb7354`)

| ID | Severity | Location | Defect Summary | Resolution & Regression Coverage |
| --- | --- | --- | --- | --- |
| BUG-025 | S1 | `coree-io/src/native.rs` | `.coree` PNG-in-ZIP assets lacked path prefix validation, deduplication, and cumulative RGBA RAM budget; ratio check ran only after reading full buffer. | Restricted asset paths to `assets/`, deduplicated manifest entries, enforced cumulative RGBA budget, and checked compression ratio incrementally in 16 KiB chunks (`regression_1`). |
| BUG-026 | S1 | `coree-io/src/svg.rs` | `.svgz` lacked gzip ratio bomb guard; `<text>` double-scaled; cut-layer check matched `"Shortcut"`/`"Woodcut"`; grouped `TextOnPath`/`Connector` double-transformed. | Added `.svgz` ratio guard, fixed single-scale `<text>` & fallback ancestor transforms, matched exact `data-cut-layer="true"`/`cutcontour`, and fixed grouped export transforms (`regression_2`). |
| BUG-027 | S2 | `coree-io/src/pdf.rs`, `eps.rs`, `svg.rs`, `cad_plot.rs`, `raster.rs` | Master-layer z-order painted other pages' master layers above Page 1; `QuadTo` used approximate cubic controls; PDF/EPS text ignored rotation/scale & EPS dropped Arabic; unchecked `ihdr_end` add. | Unified master-layer z-order, upgraded `QuadTo` to exact `QuadBez::raise()`, applied full text transforms + Arabic curve fallback in EPS, guarded non-finite coords, and used `checked_add` (`regression_3`). |
| BUG-028 | S2 | `coree-io/src/palette_io.rs` | Brand sweep hit on runtime palette name; `import_ase`/non-JASC `import_pal` allowed `NaN`/`Inf` floats and uncapped swatches. | Renamed runtime label cleanly, sanitized non-finite floats, and enforced `MAX_PALETTE_SWATCHES` (`regression_4`). |
| BUG-029 | S1 | `coree-storage/src/autosave.rs` | Reopening a document after clean close collided on `UNIQUE (document_id, session_id, seq_no)`; failed SQLite inserts leaked blob files; `recover_document` lacked read cap. | Added session rotation + session-scoped recovery/close APIs, cleaned orphan blobs on DB error, and enforced bounded snapshot reads (`regression_5`). |
| BUG-030 | S2 | `coree-storage/src/assets.rs` | `NaN.clamp(...)` bound SQL `NULL` bypassing CHECK; `set_active_workspace_layout` cleared all active flags even when target ID did not exist. | Sanitized non-finite swatch values before clamping and made active-layout updates transactional (`regression_6`). |
| BUG-031 | S2 | `coree-storage/src/fonts.rs`, `jobs.rs` | Recursive font scan skipped symlinked Linux `.ttf`/`.otf` files; OpenType `cmap` fmt4 missed `idDelta` zero check; unbounded reads; brand sweep hit in `jobs.rs`. | Followed symlinks with canonical cycle guard, fixed `cmap` fmt4 delta check, bounded font reads transactionally, and scrubbed `jobs.rs` label (`regression_7`). |

---

## Wave 2 — `coree-render` (7 Fixes in `8cb7354`)

| ID | Severity | Location | Defect Summary | Resolution & Regression Coverage |
| --- | --- | --- | --- | --- |
| BUG-032 | S1 | `coree-render/src/*`, `tests/*` | Brand sweep flagged 25 legacy trademark strings in comments and test names. | Scrubbed all 25 occurrences so `sweep.py crates/coree-render` exits 0 (`0 hits`). |
| BUG-033 | S1 | `coree-render/src/backend.rs` | `PixelRect::right()/bottom()` overflowed `i32` at extreme zoom, dropping mesh fills, lenses, and shadows. | Computed clamping in `i64` with Sutherland-Hodgman viewport clipping (`regression_2`). |
| BUG-034 | S1 | `coree-render/src/tile_cache.rs`, `lens_render.rs`, `effects_render.rs` | `ViewportKey` omitted page index / multipage / `RenderOptions`; cross-tile `Frozen`/`FishEye`/`Magnify`/`Viewpoint` lenses and blurred `DropShadow`s produced tile seams or stretched slices. | Expanded `ViewportKey` and implemented seamless padded offscreen evaluation matching direct render (`regression_3`). |
| BUG-035 | S1 | `coree-render/src/renderer.rs`, `lens_render.rs` | Grouped / `Symmetry` lenses passed `parent_to_screen` as `page_to_screen`, double-transforming underlying page geometry and viewpoint offsets. | Threaded `page_to_surface` separately from `parent_to_surface` (`regression_4`). |
| BUG-036 | S1 | `coree-render/src/raster_engine.rs`, `trace.rs`, `smart_fill.rs` | Adversarial coordinates/dimensions could OOM `sample_stroke_dabs`/`smudge_stroke`/`resample`, OOB panic on malformed `RasterAsset`, or hang `draw_supercover_line` on `NaN`. | Added brush radius/dab caps, `validate_surface_dimensions`, strict `rgba_bytes.len()` checks, and finite/clamped `i64` Bresenham guards (`regression_5`). |
| BUG-037 | S1 | `coree-render/src/hit_test.rs` | Hit-testing ignored `layer.locked`, tested grouped non-path objects in local coords, and allowed angled snap guides to override X/Y snaps when `dynamic_guides = false`. | Enforced `layer.locked`, inverted `parent_xf` on grouped objects, and fixed snap precedence (`regression_6`). |
| BUG-038 | S2 | `coree-render/src/renderer.rs`, `effects_render.rs`, `trace.rs`, `text_layout.rs` | `VectorPattern` re-scanned OS fonts per tile; `Extrude25D` bevel drew under fill; `LineArt` color cap was dead code & thinning skipped 1px borders; `ParagraphText` ignored `letter_spacing_mm`; Arabic Kashida misclassified `'ژ'` and `'لٱ'`; Angular/Callout dims ignored Wireframe. | Reused `&mut self` with recursion guard, drew bevel rim post-fill, fixed `LineArt` cap & 1px border padding, honored `letter_spacing_mm`, fixed Arabic joining rules, and supported Wireframe dims (`regression_7`). |

---

## Wave 3 — `coree-app` (8 Fixes in `b44ba59`)

| ID | Severity | Location | Defect Summary | Resolution & Regression Coverage |
| --- | --- | --- | --- | --- |
| BUG-039 | S1 | `coree-app/src/state.rs`, `ui/*.rs` | Post-save edits did not rotate/reopen WAL session (lost on crash); `undo`/`redo` did not write checkpoint snapshot (undone ops resurrected on crash); several UI edits bypassed `CommandHistory`. | Reset `journal_seq`/`wal_session_open` on save, wrote checkpoint snapshots on `undo`/`redo`, and routed duplicate/gradient/layer/font/node edits through `execute_command` (`regression_wal_post_save_and_post_undo_crash_recovery`). |
| BUG-040 | S1 | `coree-app/src/state.rs`, `ui/canvas.rs`, `ui/dockers.rs` | `MeshFill` emitted `(rows+1)*(cols+1)` world coords instead of `rows*cols` local coords; `Distort`/`Envelope`/`Perspective` used world coords and 6-pt envelope collapsed shapes; `InteractiveFill` wrote world coords; `convert_selection_to_bitmap` omitted 4mm padding. | Switched to `object_local_bounds`, emitted `rows*cols` mesh & 8-pt Coons envelope cage, inverted transform in `InteractiveFill`, and applied 4mm bitmap padding (`regression_local_coords_mesh_envelope_and_bitmap_padding`). |
| BUG-041 | S1 | `coree-app/src/io_bridge.rs` | `import_document_into` lacked recursion/cycle guard, skipped `symbols`, and did not remap `TextOnPath`/`Connector`/`Blend` object IDs. | Added depth + `visiting` cycle guard, copied symbols, and remapped all cross-object references (`regression_import_cycle_guard_symbols_and_id_remapping`). |
| BUG-042 | S1 | `coree-app/src/state.rs`, `ui/*.rs` | Deleting or switching pages left stale `active_layer_id` causing `LayerNotFound` on next draw; `duplicate_page` created an empty page. | Added `sync_active_layer()` across all page transitions and cloned full page layers/objects in `duplicate_page` (`regression_page_switch_delete_layer_sync_and_duplicate_page`). |
| BUG-043 | S2 | `coree-app/src/state.rs` | `Ctrl+Q` Convert to Curves turned `ArtisticText` into a placeholder rectangle instead of real glyph curves. | Wired `coree_render::text_to_curves` / `paragraph_to_curves` into `convert_selection_to_curves` (`regression_ctrl_q_text_converts_to_real_glyph_curves`). |
| BUG-044 | S2 | `coree-app/src/cli.rs`, `io_bridge.rs`, `ui/dockers.rs` | `generate-sample --format coree` ignored explicit format if extension differed; `render --multipage` accepted `NaN`/negative DPI and cropped layouts > 16384px; CSV/Rhai reads were unbounded. | Honored explicit `--format coree`, validated finite positive DPI + auto-scaled oversized multipage renders, and enforced 64 MiB `read_text_file_bounded` (`regression_cli_format_override_dpi_validation_and_bounded_read`). |
| BUG-045 | S2 | `coree-app/src/ui/canvas.rs`, `dockers.rs`, `state.rs`, `menu_bar.rs` | Eraser only erased final point; Knife left deleted ID in selection; Transform "Apply to Duplicate" added +5mm offset before step; Text tab ignored `ParagraphText`/`TextOnPath`; CDR Inspector cached stale report. | Iterated full Eraser stroke, updated Knife selection, added `duplicate_selection_with_offset(Vec2D::ZERO)`, supported all text payloads, and refreshed CDR Inspector unconditionally. |
| BUG-046 | S2 | `coree-app/src/shortcuts.rs`, `ui/palette_bar.rs`, `dialogs.rs`, `command_palette.rs` | `Ctrl+Z/Y/D/G/U/Q/L` fired during text editing; compact palette right-click import wasn't wired; Export dialog dropped parent directory/page index/`.webp`; Command Palette arrows moved past 60 rendered rows. | Gated editing shortcuts by `!text_focus`, wired right-click palette import, fixed Export dialog path/page/WebP handling, and clamped Command Palette navigation (`regression_shortcuts_gated_when_text_focused`). |
