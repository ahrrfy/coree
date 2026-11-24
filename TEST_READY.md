# TEST_READY — `coree` Opaque-Box E2E Test Suite (`e2e_suite/`)

**Status:** `READY` (`M5` Integrated Verification Complete — `2/2` clean `/egm-review-loop` rounds across Milestones `M1`–`M4`)
**Target:** `coree v0.3.1` baseline through Waves `W1` → `W5` (`99.00%` clean-room CorelDRAW parity)
**Runtime:** Pure Python `3.14+` stdlib (`0` pip packages, `0` network calls; optional `PIL` fast-path strictly guarded with pure-stdlib fallback)
**Authoritative Spec:** `TEST_INFRA.md` (`e6a63e29-6f24-42d4-b5e4-d855aff395bf/.agents/teamwork/TEST_INFRA.md`)

### Milestone Gate Status
- **M1 (`tier1_cli_roundtrip.py`):** DONE (`2/2` consecutive clean rounds — Rounds 26 & 27)
- **M2 (`tier2_boundary_gui.py`):** DONE (`2/2` consecutive clean rounds — Rounds 4 & 5)
- **M3 (`tier3_color_visual.py`):** DONE (`2/2` consecutive clean rounds — Rounds 3 & 4)
- **M4 (`tier4_stress_perf.py`):** DONE (`2/2` consecutive clean rounds — Rounds 18 & 19)
- **M5 (`e2e_verify` & `TEST_READY.md`):** Integrated verification executed against isolated `$PROBE_DIR` snapshots of `target/debug/{coree-cli,coree}`

> **Zero `Cargo.lock` Contention Rule:** Always snapshot prebuilt `coree-cli` and `coree` binaries into a private `$PROBE_DIR` and pass `--cli-bin "$PROBE_DIR/coree-cli"` when running `e2e_suite/run_e2e.py` in parallel with Cargo builds or wave agents. Never run `cargo build` or `cargo test` inside E2E verification probes unless zero prebuilt binaries exist.

---

## Test Runner

### Standard Isolated Invocation (`$PROBE_DIR`)
```bash
PROBE_DIR=$(mktemp -d -- "${TMPDIR:-/tmp}/teamwork_probe.XXXXXX") && chmod 700 "$PROBE_DIR"
trap 'rm -rf "$PROBE_DIR"' EXIT
export PYTHONPYCACHEPREFIX="$PROBE_DIR/pycache"
cp target/debug/coree-cli target/debug/coree "$PROBE_DIR/"

# 1. Syntax & warning check across all 7 Python modules
python3 -B -W error -m py_compile \
  e2e_suite/__init__.py e2e_suite/harness.py e2e_suite/run_e2e.py \
  e2e_suite/tier1_cli_roundtrip.py e2e_suite/tier2_boundary_gui.py \
  e2e_suite/tier3_color_visual.py e2e_suite/tier4_stress_perf.py

# 2. Validate contracts, 51/51 roadmap coverage, 86/6/12 features.csv invariants, and offline fixtures
python3 -B -W error e2e_suite/run_e2e.py --validate --require-all-tiers --cli-bin "$PROBE_DIR/coree-cli"

# 3. List all 102 discovered tests
python3 -B -W error e2e_suite/run_e2e.py --list

# 4. Run baseline smoke gate (27 tests, exit 0 expected on v0.3.1+)
python3 -B -W error e2e_suite/run_e2e.py --smoke --cli-bin "$PROBE_DIR/coree-cli"

# 5. Run individual tiers (1..4) or the full 102-test suite with JSON telemetry
python3 -B -W error e2e_suite/run_e2e.py --tier 1 --cli-bin "$PROBE_DIR/coree-cli"
python3 -B -W error e2e_suite/run_e2e.py --all --cli-bin "$PROBE_DIR/coree-cli" --json-out "$PROBE_DIR/e2e_all.json"
```

### CLI Flags (`e2e_suite/run_e2e.py`, 10 Flags)
- `--tier {1,2,3,4}`: Repeatable tier selector (`1`, `2`, `3`, `4`).
- `--all`: Execute all discovered tiers (`1`–`4`).
- `--smoke`: Execute baseline `smoke=True` tests only (`27` tests; exit `0` iff all pass).
- `--validate`: Validate `TierModule` contracts, unique test IDs, `51/51` roadmap IDs, `features.csv` `86/6/12` invariants, Replica scripts, binary discovery (`allow_cargo_build=False`), and Tier 1 probe fixtures.
- `--require-all-tiers`: Require all 4 tier modules (`tier1..tier4`) and `86/86` `FEAT-*` coverage.
- `--list`: Print all selected tests with tier, target wave, smoke flag, roadmap IDs, and title without executing.
- `--json-out <path>`: Write structured JSON report (`cli_bin`, `gui_bin`, `cli_subcommands`, `counts`, `by_tier`, `by_wave`, `total_duration_ms`, `results`).
- `--bin-dir <path>`: Directory containing `coree-cli` and sibling `coree` binaries (overrides `COREE_BIN_DIR`).
- `--cli-bin <path>`: Explicit path to `coree-cli` binary (overrides `COREE_CLI_BIN` and workspace `target/` discovery).
- `--timeout-scale <float>`: Finite `> 0.0` multiplier applied to all `run_cmd(...)` and external oracle timeouts (`312/312` call sites wired).

### Expected Exit Codes & Binary Discovery
- `--validate --require-all-tiers`, `--list`, `--smoke`: Expected exit code **`0`** on `v0.3.1` baseline and all subsequent waves.
- `--all`: Expected exit code **`0`** once all Waves `W1`–`W5` are implemented. On the `v0.3.1` baseline (`W1.B` in progress), `--all` exits **`1`** honestly due to `72` expected future-wave `FAIL`s (`ERROR=0`, `SKIP=0` — zero skips or fake passes).
- **Binary Discovery (`harness.py:discover_binaries`):** (1) `--cli-bin` / `COREE_CLI_BIN` (+ `COREE_GUI_BIN`) → (2) `--bin-dir` / `COREE_BIN_DIR` → (3) newest `mtime` between `target/release` and `target/debug` (tie-break `release`) → (4) `cargo build -p coree-app --bin coree-cli` fallback **only** when `allow_cargo_build=True` and zero prebuilt binaries exist.

---

## Coverage Summary

### Suite Inventory (`24,626` Python Lines, `102` Tests, `54` Fixtures)
| Module / Directory | Lines | Bytes | Tier | Total Tests | Smoke (`baseline`) | Wave (`smoke=False`) | `FEAT-*` Covered | Fixtures |
|---|---:|---:|---|---:|---:|---:|---:|---|
| `e2e_suite/__init__.py` | `4` | `125` | — | — | — | — | — | — |
| `e2e_suite/run_e2e.py` | `397` | `16,333` | — | — | — | — | — | — |
| `e2e_suite/harness.py` | `4,351` | `175,027` | — | — | — | — | — | — |
| `e2e_suite/tier1_cli_roundtrip.py` | `7,923` | `376,786` | `T1` (`CLI, Codec Round-Trip & Hygiene`) | `50` | `16` | `34` | `58 / 86` | `fixtures/tier1/` (`25` files, `17,888` B) |
| `e2e_suite/tier2_boundary_gui.py` | `3,453` | `177,227` | `T2` (`Boundary, Negative & GUI/ToolCore`) | `22` | `5` | `17` | `67 / 86` | `fixtures/tier2/` (`20` files, `553,528` B) |
| `e2e_suite/tier3_color_visual.py` | `3,751` | `200,482` | `T3` (`Cross-Feature Pairwise, ICC & Visual Oracles`) | `20` | `6` | `14` | `67 / 86` | `fixtures/tier3/` (`6` files, `8,074` B) |
| `e2e_suite/tier4_stress_perf.py` | `4,747` | `235,511` | `T4` (`Real-World Workloads, Stress & Perf Gates`) | `10` | `0` | `10` | `29 / 86` | `fixtures/tier4/` (`3` files, `1,886` B) |
| **Total** | **`24,626`** | **`1,181,491`** | **T1–T4** | **`102`** | **`27`** | **`75`** | **`86 / 86` (`100%`)** | **`54` files (`581,376` B)** |

### Requirement & Kernel Coverage Totals
- **Roadmap Items:** **`51 / 51` (`100%`)** (`MND-01..05`, `REQ-01..12`, `ADD-01..12`, `IMP-01..12`, `REM-01..10`)
- **`replica/features.csv` Counted Features:** **`86 / 86` (`100%`)** (`68 must`, `17 should`, `1 could` — per-tier: `T1=58`, `T2=67`, `T3=67`, `T4=29`)
- **`replica/features.csv` Skipped Features:** **`6 / 6` (`100%`)** (`SKIP-01..06`, `T1=6`)
- **`replica/features.csv` Extra Features:** **`12 / 12` (`100%`)** (`EXT-F01..12` — per-tier: `T1=12`, `T2=7`, `T4=9`)
- **6-Layer CorelDRAW Atomic Kernel Mapping (`coreldraw_parity_standard.md` §5):**
  1. **`VGCore` / `CdrCore`** (`coree-core`): `CommandOp`/`DocumentDelta`, `ObjectId` cascade, `Symbol`/`Clone`/`PowerClip`, `10k+` relational tree & `<= 2ms` differential autosave (`T1-W1-COMMAND-OPS`, `T2-W1-COMMAND-DELTA-BVA`, `T3-W2-PERSP-CLIP-DIM-CDR`, `T4-W4-SCENE10K-BENCH`).
  2. **`CDRCRV`** (`coree-core`, `coree-render`): Analytical `erase_circle`, `stroke_to_closed_path`, `PathNode` continuity, `convert-to-curves`, `DCEL SmartFill`, `BooleanOp` metadata preservation (`T1-W2-CURVES-CONT`, `T1-W2-CONVERT-CURVES`, `T1-W2-SMARTFILL-META`, `T2-W2-CDRCRV-GEOMETRY-BVA`, `T3-W2-DCEL-CURVES-BOOL`).
  3. **`ToolCore`** (`coree-tools`, `coree-app`): `egui`-independent `ToolCore`/`TransientOverlay`, `0` `objects.get_mut` / drag `mark_all_dirty()`, `65/65` icons & Property Bars, zero `DrawingBox`, RTL shell mirroring (`T1-W1-COREE-TOOLS`, `T1-W5-UI-65-NO-BOX`, `T2-W1-TOOLCORE-INTERACTION`, `T2-W5-I18N-RTL-SHELL-MIRRORING`, `T3-W5-VIEW-SNAP-SHAPES-SCULPT`).
  4. **`Crl2D` + `CDROPENCL`** (`coree-render`): `WgpuBackend` vs `TinySkiaBackend` parity, `WorldTileKey` `DirtyTileCache`, `InnerShadow`/`Bevel`/`Extrude3D`/Coons `MeshFill`/`Envelope`/`Blend`/`Frozen` lens, `Potrace`, 16-bit CoW Photo Studio (`T1-W3-WGPU-TILES`, `T1-W3-EFFECTS-COONS-LENS`, `T3-W3-WGPU-CPU-GPU-PARITY`, `T3-W3-COONS-EFFECTS-TRANSP`, `T4-W4-PHOTO16-PSD`, `T4-W4-POTRACE-COONS-ICC`).
  5. **`CRLCLR` + `CRLFOM` + `CdrPDF` / `CdrPsi`** (`coree-core`, `coree-render`, `coree-io`, `coree-prod`): `ICC+BPC+chad` (`CIEDE2000 < 1.0`), `ExportDisplayList`, `PDF/X-1a/4` + `/CIDFontType2`+`/ToUnicode`, vector separations/`Trapping`/`Halftone`/`PerfectBound`, `EPS`/`SVG`/`DXF`/`PLT`/`.dwg`/`.emf`/`.acb`/`30+ FEFCO`/2D barcodes/PANOSE (`T1-W3-ICC-PIPELINE`, `T1-W4-PDFX-SUBSET`, `T1-W4-DISPLAYLIST-SEP`, `T3-W3-ICC-BPC-CHAD-DE2000`, `T3-W4-PDFX-GS-POPPLER-SEP`, `T4-W4-BROCHURE-PDFX`, `T4-W4-PACKAGING-FEFCO`).
  6. **`CrlCQL` + `CrlAutomation` + `RIFF/ZCF`** (`coree-prod`, `coree-cdr`, `coree-app`): `coree-cli cql`, `Rhai Scene DOM` + event hooks, JSON-RPC plugin host, UDS `0600` IPC, `20B` reversed `loda`, `CDRO` `16B` ZCF indirect pointers, `document.cdss`, `UnknownChunkMap`, `v3–v5`/`.wi`/`bmpt`, `CMX`/`CPT`, `10k`-row streaming Print Merge, offline `fuzz/` (`T1-W1-IPC-UDS`, `T1-W2-CDR-WRITER`, `T1-W2-UNKNOWN-CHUNKS`, `T1-W4-RHAI-DOM-CQL`, `T4-W2-CDR-MIGRATION`, `T4-W4-PRINT-MERGE-10K`, `T4-W3-FUZZ-OFFLINE`).

---

## Baseline Run Metrics

### Verified Snapshot Binaries (`2026-11-23`)
- **`target/debug/coree-cli`:** `574,642,032` bytes, mtime `2026-11-23 15:17:30.265848563 +0300` (snapshotted to `$PROBE_DIR/coree-cli`, `coree 0.3.1`, `12` subcommands: `collect`, `convert`, `generate-sample`, `help`, `inspect`, `laser`, `preflight`, `print-merge`, `render`, `run-macro`, `thumbnail`, `trace`).
- **`target/debug/coree`:** `574,570,120` bytes, mtime `2026-11-23 15:17:30.142845868 +0300` (snapshotted to `$PROBE_DIR/coree`).

### Gate Check Results
- **`py_compile -B -W error` (all 7 modules):** `exit 0` (`0` errors, `0` warnings).
- **`--validate --require-all-tiers`:** `ok=True`, `tiers=[1, 2, 3, 4]`, `tests=102 (smoke=27)`, `roadmap=51/51`, `features_mapped=86/86`, `subcommands=12`, `errors=[]`, `warnings=[]`, `exit 0`.
- **`--list`:** `Total listed: 102` (`50` T1, `22` T2, `20` T3, `10` T4), `exit 0`.
- **`--smoke`:** **`PASS=27, FAIL=0, ERROR=0, SKIP=0`** in **`24,545.3 ms`**, `exit 0` (`16` T1 + `5` T2 + `6` T3 baseline checks).
- **`--all`:** **`PASS=30, FAIL=72, ERROR=0, SKIP=0`** across all **`102`** tests in **`95,822.66 ms`** (`95,822.7 ms` stdout total), `exit 1` (due strictly to `72` honest unimplemented `W1.B`–`W5` wave checks; `0` unhandled errors, `0` skips).

### `--all` Breakdown by Tier
| Tier | Total | PASS | FAIL | ERROR | SKIP | Sum Duration (ms) |
|---|---:|---:|---:|---:|---:|---:|
| Tier 1 (`T1`) | 50 | 19 | 31 | 0 | 0 | 19,666.11 |
| Tier 2 (`T2`) | 22 | 5 | 17 | 0 | 0 | 7,883.96 |
| Tier 3 (`T3`) | 20 | 6 | 14 | 0 | 0 | 8,513.89 |
| Tier 4 (`T4`) | 10 | 0 | 10 | 0 | 0 | 59,693.57 |
| **Total** | **102** | **30** | **72** | **0** | **0** | **95,822.66** *(harness wall)* |

### `--all` Breakdown by Target Wave
| Target Wave | Total | PASS | FAIL | ERROR | SKIP | Sum Duration (ms) | Notes |
|---|---:|---:|---:|---:|---:|---:|---|
| `baseline` | 27 | 27 | 0 | 0 | 0 | 24,613.07 | 100% baseline smoke gate green |
| `W1` | 15 | 3 | 12 | 0 | 0 | 5,735.39 | Passing `W1.A`: `T1-W1-SWEEP-CRATES`, `T1-W1-SWEEP-PACKAGING`, `T1-W1-VERSION-SYNC`; 12 pending `W1.B`/`W1.C`/`W1.D` |
| `W2` | 15 | 0 | 15 | 0 | 0 | 1,462.53 | Planned Wave `W2` |
| `W3` | 14 | 0 | 14 | 0 | 0 | 1,621.37 | Planned Wave `W3` |
| `W4` | 21 | 0 | 21 | 0 | 0 | 61,103.34 | Planned Wave `W4` (includes `35,047.1 ms` `T4-W4-PRINT-MERGE-10K` timeout & `15,676.3 ms` `T4-W4-PHOTO16-PSD`) |
| `W5` | 10 | 0 | 10 | 0 | 0 | 1,221.83 | Planned Wave `W5` |
| **Total** | **102** | **30** | **72** | **0** | **0** | **95,822.66** | `ERROR=0`, `SKIP=0` |

---

## External Verification Oracles & Offline Assets

| Oracle / Asset | Path / Helper | Verification Scope |
|---|---|---|
| Ghostscript | `/usr/bin/gs` (`run_ghostscript_render`, `tiffsep`) | PDF/X-1a & PDF/X-4 syntax, EPS `pngalpha` zero white-knockout halo (`opaque_white_pixels == 0`), CMYK/Spot `tiffsep` plates |
| Poppler Suite | `/usr/bin/pdfinfo`, `/usr/bin/pdffonts`, `/usr/bin/pdftotext`, `/usr/bin/pdftoppm` | `MediaBox`/`TrimBox`/`BleedBox`, `/Type0`+`CIDFontType2` (`emb=yes`, `sub=yes`, `uni=yes`), Arabic+Latin `/ToUnicode` text, TrimBox pixel parity *(host `/usr/bin/pdfimages` not wrapped in `harness.py`)* |
| System ICC + Colorimetry | `/usr/share/color/icc/` (`52` profiles: `colord/` `38`, `ghostscript/` `14`) + `IccProfileOracle` + `ciede2000` | Pure-Python ICC v2/v4 parser (`mAB`/`mBA` LUTs) & `CIEDE2000` (`Delta-E < 1.0`), calibrated against `/usr/bin/transicc` (`49/49` profiles) |
| File Magic & Packaging | `/usr/bin/file`, `/usr/bin/dpkg-deb` (`run_file_magic`, `run_dpkg_deb_inspect`) | MIME/header classification and `.deb` package metadata/contents inspection |
| Standalone Python Dissectors | `e2e_suite/harness.py` (`dissect_cdr_riff`, `dissect_cdr_zcf`, `dissect_psd`, `dissect_emf`, `dissect_wmf`, `dissect_acb`, `dissect_dwg`, `parse_png_info`, `compare_png_pixels`, `parse_pdf_structure`, `parse_svg_structure`, `parse_dxf_structure`, `parse_plt_commands`) | Independent byte-level verification of `20B` reversed `loda`, `CDRO` `16B` ZCF indirect ptrs, `cdss`/XML, `8BPS`, `EMR_HEADER`/`EMR_EOF`, `0x9AC6CDD7` WMF, `8BCB` ACB, `AC1012..AC1032` DWG, CRC32 PNG + `PIL.ImageChops`/stdlib pixel diffs |
| Replica Verification Scripts | `sweep.py`, `contrast.py`, `listing.py`, `parity.py` (`scripts/brand_sweep.py`, `scripts/verify_invariants.py`) | Brand hygiene (`0` hits, MIME allowlist), `35/35` WCAG AA contrast, store listing lint, `86/68/6/12` `features.csv` parity, `#![forbid(unsafe_code)]`, `0` network crates, semver sync |

---

## Feature Checklist

### 1. All 86 Counted Features (`FEAT-01..FEAT-86`, `T1=58, T2=67, T3=67, T4=29`, Combined `86/86`)

| ID | Area | Prio | Feature Summary | T1 | T2 | T3 | T4 | Roadmap ID(s) | Target Wave |
|---|---|---|---|---|---|---|---|---|---|
| `FEAT-01` | workspace | must | Multi-document tabbed workspace with Welcome screen and recent files | — | Yes (4) | — | — | `REQ-12`, `IMP-10`, `IMP-12` | `baseline` / `W5` |
| `FEAT-02` | workspace | must | Customizable Toolbox flyouts context-sensitive Property Bar and Status Bar | Yes (2) | Yes (3) | Yes (1) | — | `MND-02`, `IMP-08`, `IMP-09`, `IMP-10` | `W1` / `W5` |
| `FEAT-03` | workspace | must | Rearrangeable Dockers (Objects Properties Color Styles Transform Align Shaping Lens Symbols Scripts) | — | Yes (2) | — | — | `IMP-10`, `IMP-11` | `baseline` / `W5` |
| `FEAT-04` | workspace | must | Interactive Rulers with movable origin Guidelines Dynamic Guides and Document Grids | — | Yes (1) | Yes (1) | — | `ADD-08`, `IMP-08`, `MND-02` | `W5` |
| `FEAT-05` | workspace | must | Canvas View Modes (Enhanced Normal Wireframe Simple Wireframe Pixels Simulate Overprints) | Yes (3) | Yes (1) | Yes (3) | — | `ADD-08`, `IMP-01` | `baseline` / `W3` / `W5` |
| `FEAT-06` | workspace | should | Workspace layout presets and customizable keyboard shortcuts | Yes (1) | Yes (2) | — | — | `ADD-09`, `IMP-09`, `IMP-10` | `W5` |
| `FEAT-07` | pages | must | Multi-page document engine with independent page sizes orientations and bleed | Yes (1) | Yes (3) | — | Yes (2) | `REQ-01`, `IMP-11` | `baseline` |
| `FEAT-08` | pages | must | Multipage View (Grid Vertical Horizontal Freeform page arrangement and drag reorder) | Yes (2) | Yes (2) | — | — | `IMP-09`, `REQ-01` | `baseline` / `W1` / `W5` |
| `FEAT-09` | pages | must | Master Layers (All Pages Odd Pages Even Pages) and automatic Page Numbering | — | Yes (1) | Yes (1) | Yes (1) | `ADD-07`, `REM-03` | `W2` / `W5` |
| `FEAT-10` | pages | should | Facing pages booklet layout and Autofit Page to content | — | Yes (1) | Yes (1) | Yes (1) | `ADD-07`, `REQ-05` | `W5` |
| `FEAT-11` | pages | should | Document Templates (.cdt) and printable Page Background settings | Yes (1) | Yes (2) | Yes (1) | — | `REQ-12`, `MND-03` | `W2` / `W5` |
| `FEAT-12` | selection | must | Pick Freehand Pick and Free Transform tools (move scale rotate skew mirror pivot) | — | Yes (1) | — | — | `MND-02`, `IMP-11` | `baseline` / `W1` |
| `FEAT-13` | selection | must | Zoom and Pan tool (marquee zoom fit page selection 1:1 Navigator popup) | Yes (1) | Yes (1) | — | Yes (1) | `ADD-08`, `IMP-02`, `IMP-01` | `W3` / `W5` |
| `FEAT-14` | selection | should | Find and Replace Objects (by fill stroke type font color model) | Yes (2) | Yes (2) | — | Yes (1) | `REQ-01`, `IMP-08`, `ADD-11` | `W1` / `W4` / `W5` |
| `FEAT-15` | drawing | must | Shape Tool node editing (cusp smooth symmetrical join break reverse auto-close reduce nodes) | Yes (1) | Yes (1) | Yes (1) | — | `REM-10`, `MND-02` | `W2` |
| `FEAT-16` | drawing | should | Sculpting brushes (Smooth Smear Twirl Attract Repel Smudge Roughen) | — | Yes (1) | Yes (1) | — | `MND-02`, `IMP-03`, `IMP-08` | `W1` / `W3` / `W5` |
| `FEAT-17` | drawing | must | Crop Knife (straight freehand Bezier slice) Virtual Segment Delete and Eraser tools | Yes (1) | Yes (1) | Yes (1) | — | `REM-01`, `MND-02` | `W2` |
| `FEAT-18` | drawing | must | Freehand 2-Point Line Bezier Pen B-Spline Polyline and 3-Point Curve tools | Yes (1) | Yes (1) | Yes (1) | — | `ADD-02`, `IMP-08`, `MND-02` | `W2` / `W5` |
| `FEAT-19` | drawing | could | LiveSketch and Smart Drawing freehand-to-curve recognition | — | Yes (1) | Yes (1) | — | `MND-02`, `ADD-02` | `W1` / `W2` |
| `FEAT-20` | drawing | should | Artistic Media (Preset Brush Sprayer Calligraphic pressure-sensitive strokes) | — | Yes (2) | Yes (1) | — | `ADD-02`, `IMP-03` | `W2` / `W3` |
| `FEAT-21` | drawing | must | Rectangle and 3-Point Rectangle with independent corner radius scallop and chamfer | Yes (1) | Yes (2) | Yes (1) | — | `MND-02`, `IMP-11` | `baseline` |
| `FEAT-22` | drawing | must | Ellipse and 3-Point Ellipse (full pie and arc modes) | Yes (1) | Yes (2) | Yes (1) | — | `MND-02`, `IMP-11` | `baseline` |
| `FEAT-23` | drawing | must | Polygon Star Spiral Common Shapes Impact and Graph Paper tools | — | Yes (1) | Yes (1) | — | `IMP-08`, `MND-02` | `W1` / `W5` |
| `FEAT-24` | drawing | must | Boolean Shaping engine (Weld Trim Intersect Simplify Front/Back Minus Boundary Combine/Break) | Yes (1) | Yes (2) | Yes (1) | — | `REM-03`, `REM-01` | `baseline` / `W1` |
| `FEAT-25` | drawing | must | Smart Fill tool (flood-fill bounded regions into closed vector shapes) | Yes (1) | Yes (1) | Yes (1) | — | `IMP-05`, `MND-02` | `W2` |
| `FEAT-26` | drawing | should | Symmetry Drawing Mode (live multi-axis mirror and radial symmetry) | — | Yes (1) | Yes (1) | — | `ADD-01`, `MND-02`, `IMP-08` | `W2` |
| `FEAT-27` | drawing | should | Perspective Drawing Mode (1-point 2-point and 3-point perspective planes) | Yes (1) | Yes (1) | Yes (1) | — | `ADD-04`, `REQ-01` | `W2` |
| `FEAT-28` | technical | must | Dimension tools (Parallel Horizontal/Vertical Angular Segment 3-Point Callout with live units) | Yes (2) | Yes (1) | Yes (1) | — | `IMP-08`, `ADD-07`, `REM-08`, `MND-03` | `W2` / `W4` / `W5` |
| `FEAT-29` | technical | should | Connector tools (Straight-Line Right-Angle Rounded Right-Angle with object anchor snapping) | Yes (1) | Yes (1) | — | — | `REM-02`, `IMP-08`, `MND-03` | `W1` / `W5` |
| `FEAT-30` | technical | must | Interactive Table tool (grid merge/split cells borders fills cell text CSV/TSV import) | Yes (1) | Yes (1) | — | — | `IMP-08`, `MND-03`, `REQ-01` | `W2` / `W5` |
| `FEAT-31` | color-fill | must | Uniform Fill and Fountain Fill (Linear Elliptical Conical Rectangular multi-stop gradients) | Yes (1) | Yes (2) | Yes (2) | — | `REQ-02`, `IMP-08` | `baseline` / `W3` |
| `FEAT-32` | color-fill | should | Vector Pattern Bitmap Pattern Two-Color Pattern Texture and PostScript Fills | Yes (1) | Yes (1) | Yes (1) | — | `ADD-12`, `MND-03`, `REM-08` | `W2` / `W4` / `W5` |
| `FEAT-33` | color-fill | must | Interactive Mesh Fill (NxM Bezier color mesh grid with node color and opacity) | Yes (1) | Yes (1) | Yes (1) | Yes (1) | `IMP-04`, `REQ-03` | `W3` / `W4` |
| `FEAT-34` | color-fill | must | Interactive Fill tool and Color/Attributes Eyedropper tools | — | Yes (1) | — | — | `MND-02`, `IMP-08` | `baseline` / `W1` |
| `FEAT-35` | color-fill | must | Outline Pen engine (width hairline joins caps dashes arrowheads calligraphic nib behind fill scale with object) | Yes (2) | Yes (2) | Yes (1) | — | `REM-01` | `baseline` / `W2` |
| `FEAT-36` | color-fill | must | Color Models (CMYK RGB Lab HSB Grayscale Spot channels) and ICC Color Management | Yes (1) | — | Yes (1) | Yes (2) | `REQ-02`, `IMP-03`, `ADD-06` | `W3` |
| `FEAT-37` | color-fill | must | Color Palette Bar (docked swatch strips Show Color Names Document Palette Custom Palettes Harmonies) | Yes (3) | Yes (3) | — | — | `ADD-09`, `ADD-06`, `REQ-01` | `baseline` / `W4` / `W5` |
| `FEAT-38` | typography | must | Artistic Text and Paragraph Text frames (linked text flow columns drop caps bullets tabs wrap) | Yes (2) | Yes (3) | Yes (1) | Yes (1) | `ADD-10`, `IMP-10`, `REQ-03` | `W3` / `W4` |
| `FEAT-39` | typography | must | Fit Text to Path (interactive offset distance orientation mirror) | Yes (1) | Yes (1) | Yes (1) | — | `ADD-10`, `MND-03` | `baseline` / `W2` / `W3` |
| `FEAT-40` | typography | must | OpenType features (ligatures alternates fractions swashes variable axes) and Convert to Curves | Yes (2) | Yes (1) | Yes (1) | — | `ADD-10`, `REM-02` | `W2` / `W3` |
| `FEAT-41` | typography | should | Glyphs docker and local Font Manager (collections preview search missing-font substitution) | — | Yes (1) | — | — | `REQ-07`, `IMP-10` | `W3`–`W5` |
| `FEAT-42` | effects | must | PowerClip (Place Inside Edit Contents Extract Fit/Center/Stretch Lock Contents) | Yes (1) | Yes (3) | Yes (1) | Yes (1) | `ADD-03`, `IMP-08`, `REQ-01` | `W2` / `W5` |
| `FEAT-43` | effects | must | Interactive Drop Shadow and Inner Shadow (feather direction color opacity perspective) | Yes (1) | Yes (1) | Yes (1) | Yes (1) | `ADD-01`, `MND-03` | `W2`–`W3` |
| `FEAT-44` | effects | must | Block Shadow (solid vector extruded shadow for signage and screenprinting) | — | — | Yes (1) | — | `ADD-01`, `MND-03` | `W2`–`W3` |
| `FEAT-45` | effects | must | Interactive Contour (To Center Inside Outside steps offset color acceleration) | — | Yes (1) | Yes (1) | — | `ADD-01`, `REQ-02`, `IMP-08` | `W2`–`W3` |
| `FEAT-46` | effects | must | Interactive Blend (linear path-bound multi-step color and shape morphing) | — | Yes (1) | Yes (1) | — | `IMP-04`, `REM-02` | `W3` |
| `FEAT-47` | effects | should | Interactive Distort (Push and Pull Zipper Twister) | — | — | Yes (1) | — | `ADD-01`, `IMP-08` | `W2`–`W3` |
| `FEAT-48` | effects | must | Interactive Envelope (Straight Single Arc Double Arc Unconstrained warp and Copy Envelope) | — | Yes (1) | Yes (1) | Yes (1) | `IMP-04`, `ADD-01` | `W3` |
| `FEAT-49` | effects | should | Interactive Extrude (3D vector extrusion vanishing point bevels rotation lighting) | — | Yes (1) | Yes (1) | — | `ADD-01`, `IMP-08` | `W2`–`W3` |
| `FEAT-50` | effects | must | Interactive Transparency (Uniform Fountain Pattern Texture and Blend/Merge modes) | — | — | Yes (1) | — | `ADD-01`, `REQ-04` | `W2`–`W3` |
| `FEAT-51` | effects | should | Non-destructive Bitmap Effects stack (Blur Sharpen Color Balance Curves HSL Levels Art Strokes Noise) | — | Yes (1) | Yes (1) | — | `IMP-06`, `ADD-12` | `W3`–`W5` |
| `FEAT-52` | lenses | must | Lens 1: Brighten Lens | Yes (1) | — | Yes (1) | — | `REM-04`, `REQ-02` | `baseline` |
| `FEAT-53` | lenses | must | Lens 2: Color Add Lens | Yes (1) | — | Yes (1) | — | `REM-04`, `REQ-02` | `baseline` |
| `FEAT-54` | lenses | must | Lens 3: Color Limit Lens | Yes (1) | — | Yes (1) | — | `REM-04`, `REQ-02` | `baseline` |
| `FEAT-55` | lenses | must | Lens 4: Custom Color Map Lens | Yes (1) | — | Yes (1) | — | `REM-04`, `REQ-02` | `baseline` |
| `FEAT-56` | lenses | must | Lens 5: Fish Eye Lens | Yes (1) | — | Yes (1) | — | `REM-04`, `REQ-02` | `baseline` |
| `FEAT-57` | lenses | must | Lens 6: Heat Map Lens | Yes (1) | — | Yes (1) | — | `REM-04`, `REQ-02` | `baseline` |
| `FEAT-58` | lenses | must | Lens 7: Invert Lens | Yes (1) | — | Yes (1) | — | `REM-04`, `REQ-02` | `baseline` |
| `FEAT-59` | lenses | must | Lens 8: Magnify Lens | Yes (1) | — | Yes (1) | — | `REM-04`, `REQ-02` | `baseline` |
| `FEAT-60` | lenses | must | Lens 9: Tinted Grayscale Lens | Yes (1) | — | Yes (1) | — | `REM-04`, `REQ-02` | `baseline` |
| `FEAT-61` | lenses | must | Lens 10: Transparency Lens | Yes (1) | — | Yes (1) | — | `REM-04`, `REQ-02` | `baseline` |
| `FEAT-62` | lenses | must | Lens 11: Wireframe Lens | Yes (1) | — | Yes (1) | — | `REM-04`, `REQ-02` | `baseline` |
| `FEAT-63` | lenses | must | Lens Controls: Frozen state Viewpoint X/Y offset and Remove Face toggle | Yes (1) | Yes (1) | Yes (1) | — | `REM-04` | `W3` |
| `FEAT-64` | photo-studio | must | Integrated Raster Photo Studio workspace | — | Yes (1) | — | Yes (1) | `IMP-06`, `ADD-12` | `W3`–`W5` |
| `FEAT-65` | photo-studio | must | Pixel Layers with blend modes opacity alpha channels and Clip Masks | — | Yes (1) | Yes (1) | Yes (1) | `IMP-06` | `W3`–`W5` |
| `FEAT-66` | photo-studio | must | Raster Selection and Masking tools (Rect Ellipse Lasso Magic Wand Smart Selection Feather/Invert) | — | Yes (1) | Yes (1) | — | `IMP-06` | `W3`–`W5` |
| `FEAT-67` | photo-studio | must | Pixel Paint Eraser Clone Stamp Healing Smudge Dodge/Burn and Flood Fill brushes | — | Yes (1) | Yes (1) | — | `IMP-06`, `IMP-03` | `W3` |
| `FEAT-68` | photo-studio | must | Photo Tone and Color Adjustments (Levels Curves HSL Color Balance Desaturate Channel Mixer Threshold) | Yes (1) | Yes (1) | Yes (1) | — | `ADD-12`, `IMP-06` | `W3`–`W5` |
| `FEAT-69` | photo-studio | should | Image Resample (Bicubic/Lanczos) Crop/Straighten Cutout Lab and Color Mode conversion | — | Yes (1) | Yes (1) | Yes (1) | `IMP-06`, `REQ-02` | `W3`–`W5` |
| `FEAT-70` | powertrace | must | PowerTRACE Quick Trace and Centerline Trace | Yes (2) | — | Yes (2) | Yes (1) | `IMP-05` | `baseline` / `W4` |
| `FEAT-71` | powertrace | must | PowerTRACE Outline Trace presets | Yes (2) | — | Yes (1) | Yes (1) | `IMP-05` | `W4` |
| `FEAT-72` | powertrace | must | PowerTRACE Interactive Controls (Detail Smoothing Corners Color Count Remove Background Split Preview) | — | Yes (1) | — | — | `IMP-05` | `W5` |
| `FEAT-73` | prepress | must | Print Dialog and Live Print Preview with Tiling Scaling and Layout | — | Yes (2) | Yes (1) | — | `IMP-09`, `REQ-10` | `W4` / `W5` |
| `FEAT-74` | prepress | must | Imposition Layout Engine (N-Up Saddle-Stitch Booklet Perfect Bound Step and Repeat Gutters) | Yes (1) | Yes (1) | Yes (1) | Yes (1) | `REQ-05` | `W4` |
| `FEAT-75` | prepress | must | Printers Marks (Crop/Fold Registration Densitometer Calibration Bars File Info Bleed Limit) | Yes (1) | — | Yes (1) | Yes (1) | `REQ-03`, `REQ-05` | `baseline` |
| `FEAT-76` | prepress | must | Color Separations (CMYK and Spot plates preview Overprint Black/Fill/Stroke Trapping Mirror/Negative) | Yes (1) | — | Yes (1) | Yes (1) | `REQ-05`, `IMP-09` | `W4` / `W5` |
| `FEAT-77` | prepress | must | Document Preflight Inspector (low-DPI bitmaps RGB-in-CMYK hairlines missing fonts overprint/bleed) | Yes (2) | Yes (2) | — | Yes (1) | `REQ-07`, `ADD-11` | `baseline` / `W4` / `W5` |
| `FEAT-78` | prepress | must | Print Merge (CSV/TSV data-driven variable text/numbering merge to pages or batch PDF) | Yes (1) | Yes (3) | — | Yes (1) | `IMP-07`, `ADD-05` | `baseline` / `W4` |
| `FEAT-79` | prepress | must | Built-in Barcode and QR Code Generator (EAN-13 UPC-A Code 128 Code 39 ITF QR vector paths) | Yes (1) | Yes (1) | Yes (1) | Yes (2) | `ADD-05` | `W4` |
| `FEAT-80` | prepress | should | Collect for Output (bundle document used fonts linked images and PDF/X report) | Yes (2) | Yes (1) | — | Yes (1) | `REQ-07` | `baseline` / `W4` / `W5` |
| `FEAT-81` | codecs | must | Native .cdr and .cdt RIFF (v3-v13) and ZIP-container (v14/X4-v25+) reader and writer | Yes (5) | Yes (4) | Yes (1) | Yes (2) | `MND-03`, `MND-04`, `REQ-06`, `REQ-08`, `REM-05`, `REM-06` | `W2` / `W5` |
| `FEAT-82` | codecs | should | Corel .cmx (Presentation Exchange) and .cpt (PHOTO-PAINT raster) import and export | Yes (2) | Yes (2) | — | Yes (2) | `REM-07`, `MND-03`, `MND-04` | `W2` |
| `FEAT-83` | codecs | must | PDF and PDF/X-1a / PDF/X-4 native CMYK and Spot Color export and PDF import | Yes (4) | Yes (3) | Yes (1) | Yes (3) | `REQ-03`, `REQ-04`, `MND-04` | `W4` |
| `FEAT-84` | codecs | must | SVG 1.1/2.0 and EPS/PostScript import and export | Yes (3) | Yes (4) | Yes (1) | Yes (2) | `REM-08`, `REQ-04`, `MND-04` | `W4` |
| `FEAT-85` | codecs | must | CAD and Plotter codecs: DXF (R12/R2000+) and HPGL .plt export/import | Yes (2) | Yes (3) | Yes (1) | Yes (1) | `REM-08`, `ADD-06` | `W4` |
| `FEAT-86` | codecs | must | Raster image codecs: PNG JPEG WebP TIFF (CMYK/RGB) BMP and PSD layer import | Yes (3) | Yes (4) | Yes (1) | Yes (2) | `IMP-06`, `ADD-06`, `MND-04` | `W4` |

### 2. All 6 Skipped Features (`SKIP-01..SKIP-06`) & 12 Extra Features (`EXT-F01..EXT-F12`)

| ID | CSV Line | Kind | Area | Tiers | Mapped Test IDs | Verification Coverage | Roadmap ID(s) |
|---|---:|---|---|---|---|---|---|
| `SKIP-01` | 88 | skip | cloud | T1 | `T1-BASE-HYGIENE-PAL`, `T1-REPLICA-PARITY` | `parity.py` (`skipped==6`) + offline-first check (`0` network crates) | `MND-05` |
| `SKIP-02` | 89 | skip | cloud | T1 | `T1-BASE-HYGIENE-PAL`, `T1-REPLICA-PARITY` | `parity.py` (`skipped==6`) + native desktop binary check | `MND-05` |
| `SKIP-03` | 90 | skip | account | T1 | `T1-BASE-HYGIENE-PAL`, `T1-REPLICA-PARITY` | `parity.py` (`skipped==6`) + dual license `MIT OR Apache-2.0` check | `MND-05` |
| `SKIP-04` | 91 | skip | color-fill | T1 | `T1-REPLICA-PARITY`, `T1-W4-DWG-EMF-ACB-PSD` | `parity.py` (`skipped==6`) + user `.acb`/`.gpl`/`.ase`/`.pal` import | `ADD-06`, `MND-05` |
| `SKIP-05` | 92 | skip | assets | T1 | `T1-REPLICA-CONTRAST`, `T1-REPLICA-LISTING`, `T1-REPLICA-PARITY`, `T1-REPLICA-SWEEP-APP`, `T1-W1-SWEEP-CRATES` | `parity.py` (`skipped==6`) + `sweep.py` clean check | `MND-01`, `MND-05` |
| `SKIP-06` | 93 | skip | automation | T1 | `T1-BASE-RHAI-MACRO`, `T1-REPLICA-PARITY` | `parity.py` (`skipped==6`) + Rhai macro runner | `ADD-11`, `MND-05` |
| `EXT-F01` | 94 | extra | platforms | T1, T2 | `T1-BASE-CLI-CORE`, `T1-REPLICA-LISTING`, `T1-W1-IPC-UDS`, `T1-W5-I18N-BG-PKG`, `T2-W1-IPC-UDS-CLIPBOARD` | Native Linux build (`coree`/`coree-cli`), Flatpak `cargo-sources.json`, `xdg-desktop-portal` | `REQ-11`, `REQ-10`, `IMP-03` |
| `EXT-F02` | 95 | extra | offline | T1 | `T1-BASE-CLI-CORE`, `T1-BASE-EMPTY-PATH`, `T1-BASE-HYGIENE-PAL`, `T1-REPLICA-CONTRAST`, `T1-W1-SWEEP-CRATES`, `T1-W1-VERSION-SYNC` | Zero network crates/calls, `MIT OR Apache-2.0`, `--diagnostics` offline mode | `MND-05`, `MND-01` |
| `EXT-F03` | 96 | extra | codecs | T1, T2, T4 | `T1-W2-UNKNOWN-CHUNKS`, `T2-BASE-ADVERSARIAL-ARCHIVE-XML`, `T2-W2-CDR-CODEC-BOUNDARY`, `T4-W2-CDR-MIGRATION` | `UnknownChunkMap` byte-exact passthrough in `.cdr`/`.cdt`/`.cmx` round-trips | `REM-05`, `MND-03` |
| `EXT-F04` | 97 | extra | codecs | T1, T2, T4 | `T1-BASE-GEN-INSPECT`, `T1-W5-SWF-CP1256-UX`, `T2-BASE-CORRUPT-HEADERS`, `T2-W2-CDR-CODEC-BOUNDARY`, `T2-W5-VIEW-GUIDES-NAVIGATOR-PRINTUI`, `T4-W2-CDR-MIGRATION` | `coree-cli inspect --json` + `S16` Hex/ASCII chunk byte viewer | `REQ-08` |
| `EXT-F05` | 98 | extra | typography | T1, T2, T4 | `T1-BASE-PRINT-MERGE`, `T1-W4-PDFX-SUBSET`, `T1-W5-I18N-BG-PKG`, `T1-W5-SWF-CP1256-UX`, `T2-BASE-ARABIC-CP1256-EDGE`, `T2-W5-I18N-RTL-SHELL-MIRRORING`, `T4-W4-BROCHURE-PDFX`, `T4-W2-CDR-MIGRATION`, `T4-W4-PRINT-MERGE-10K` | Arabic RTL/HarfBuzz shaping, post-shaping Kashida, `CP1256` `.cdr` decoder | `IMP-10`, `REQ-08`, `REQ-03` |
| `EXT-F06` | 99 | extra | prepress | T1, T4 | `T1-BASE-LASER`, `T1-W4-FEFCO-2D-KERF`, `T4-W4-PACKAGING-FEFCO`, `T4-W4-LASER-NESTING` | `CutContour`/`PerfCutContour`/`Crease` + `30+` FEFCO parametric dielines | `ADD-05`, `REQ-05` |
| `EXT-F07` | 100 | extra | prepress | T1, T2, T4 | `T1-BASE-PREFLIGHT-COLL`, `T2-BASE-CLI-BVA-NUM`, `T4-W4-PACKAGING-FEFCO` | Live Path Meter HUD (`ProductionMetrics` cut length / utilization via `collect manifest.json` & HUD) | `ADD-05`, `IMP-08` |
| `EXT-F08` | 101 | extra | prepress | T1, T4 | `T1-BASE-LASER`, `T1-W4-FEFCO-2D-KERF`, `T4-W4-LASER-NESTING` | Production laser toolpath (`coree-cli laser`), 7 built-ins, Boolean `Kerf` cleanup | `REM-09` |
| `EXT-F09` | 102 | extra | platforms | T1 | `T1-W1-SWEEP-PACKAGING`, `T1-W5-I18N-BG-PKG` | Linux `.cdr` MIME + GNOME thumbnailer + WiX MSI + `IThumbnailProvider` + MIME allowlist | `REQ-11`, `MND-01` |
| `EXT-F10` | 103 | extra | storage | T1, T2, T4 | `T1-W1-AUTOSAVE-WAL`, `T2-W1-SQLITE-QUARANTINE-WAL`, `T4-W4-SCENE10K-BENCH` | SQLite WAL command journal, async differential autosave, corrupt DB quarantine (`T1-W1-AUTOSAVE-WAL`) | `IMP-07`, `REQ-01` |
| `EXT-F11` | 104 | extra | automation | T1, T2, T4 | `T1-BASE-RHAI-MACRO`, `T1-W4-RHAI-DOM-CQL`, `T2-W4-CQL-RHAI-PLUGIN-SANDBOX`, `T4-W4-SCENE10K-BENCH` | Rhai macro recorder/runner (`coree-cli run-macro`) + Hierarchical Scene DOM | `ADD-11` |
| `EXT-F12` | 105 | extra | performance | T1, T4 | `T1-W1-BENCHES-INIT`, `T1-W3-WGPU-TILES`, `T4-W4-SCENE10K-BENCH` | R-Tree spatial index + world-space `DirtyTileCache` + `wgpu` (`10k+` `>= 60 FPS`) | `IMP-01`, `IMP-02`, `IMP-12` |

### 3. All 51 Roadmap Items (`MND-01..05`, `REQ-01..12`, `ADD-01..12`, `IMP-01..12`, `REM-01..10`)

| # | Roadmap ID | Wave | Tiers | Mapped Test ID(s) | Semantic Oracle Summary |
|---:|---|---|---|---|---|
| 1 | `MND-01` | `W1` | T1 | `T1-REPLICA-CONTRAST`, `T1-REPLICA-LISTING`, `T1-REPLICA-SWEEP-APP`, `T1-W1-SWEEP-CRATES`, `T1-W1-SWEEP-PACKAGING` | `scripts/brand_sweep.py --require-external-sweep` exit 0, 0 non-MIME hits on `crates/` & `packaging/` (validated via `scripts/brand_sweep.py --require-external-sweep`), `ci.yml` + `brand.md` trademark log |
| 2 | `MND-02` | `W1` | T1, T2, T3 | `T1-W1-COREE-TOOLS`, `T2-W1-TOOLCORE-INTERACTION`, `T3-BASE-PAIRWISE-FILLS`, `T3-W2-BSPLINE-MEDIA-CLONE`, `T3-W5-VIEW-SNAP-SHAPES-SCULPT` | `crates/coree-tools` exists without `egui`/`eframe` dep; `0` `objects.get_mut` & `0` drag `mark_all_dirty()` in `canvas.rs`; `cargo test -p coree-tools` |
| 3 | `MND-03` | `W2` | T1, T2, T3, T4 | `T1-W2-CDR-WRITER`, `T2-W2-DRAWING-MODES-MASTER`, `T2-W2-CDR-CODEC-BOUNDARY`, `T3-BASE-BLOCKSHADOW-DISTORT`, `T3-W2-PERSP-CLIP-DIM-CDR`, `T4-W2-CDR-MIGRATION` | `coree-cli convert` `.cdr` emits `20B` reversed `loda`, `CDRO` ZCF `16B` external ptrs dereferenced to `20B` `loda` (`0` unindirected / `0` 12B deref `loda`), non-empty `styles/document.cdss` (`cdss_key_count >= 1`) & `docPalette.xml` (`>= 2` `<color>`), plus non-flattening `Table`/`Dimension`/`Connector`/`SymbolInstance`/`DropShadow`/`TwoColorPattern` `.cdr` round-trip |
| 4 | `MND-04` | `W1–W3` | T1, T2, T4 | `T1-W1-FUZZ-PACK`, `T2-BASE-CORRUPT-HEADERS`, `T2-BASE-ADVERSARIAL-ARCHIVE-XML`, `T2-BASE-JSON-COREE-NAN-CYCLE`, `T2-W4-EXCHANGE-DWG-EMF-PSD-BVA`, `T4-W2-CDR-MIGRATION`, `T4-W3-FUZZ-OFFLINE` | `fuzz/` targets compile (`cargo check`) & `crates/coree-cdr/tests/fixtures_real_cdr/` real `.cdr` pack passes `coree-cli inspect --json` |
| 5 | `MND-05` | `W1` | T1 | `T1-BASE-CLI-CORE`, `T1-BASE-EMPTY-PATH`, `T1-BASE-HYGIENE-PAL`, `T1-REPLICA-PARITY`, `T1-W1-VERSION-SYNC` | `#![forbid(unsafe_code)]`, `0` network crates, `scripts/verify_invariants.py` exit 0, exact semver sync across `Cargo.toml`/`coree.rc`/`main.wxs`/`CHANGELOG.md`/`metainfo.xml` + `ci.yml` |
| 6 | `REQ-01` | `W1` | T1, T2 | `T1-W1-COMMAND-OPS`, `T2-BASE-JSON-COREE-NAN-CYCLE`, `T2-W1-COMMAND-DELTA-BVA`, `T2-W4-CQL-RHAI-PLUGIN-SANDBOX` | `CommandOp::{FindAndReplace, DuplicateObjects, ReorderPages, FitPowerClip}`, `DocumentDelta`, zero silent `build_find_query` stub, `cargo test -p coree-core -- command` |
| 7 | `REQ-02` | `W3` | T1, T2, T3, T4 | `T1-W3-ICC-PIPELINE`, `T2-W3-COLOR-MESH-EFFECTS-LENS`, `T2-W3-PHOTO-STUDIO-BOUNDARY`, `T3-BASE-ICC-PARSER-DE2000`, `T3-BASE-PAIRWISE-FILLS`, `T3-W3-ICC-BPC-CHAD-DE2000`, `T4-W4-BROCHURE-PDFX`, `T4-W4-POTRACE-COONS-ICC` | `Color::cmyk_to_rgb_icc`/`RenderingIntent` + `ICC + BPC + chad`, embedded system `.icc` CLI render, `cargo test -p coree-core -- icc`; Tier 3 `Delta-E < 1.0` |
| 8 | `REQ-03` | `W4` | T1, T2, T3, T4 | `T1-W4-PDFX-SUBSET`, `T2-W4-PREPRESS-IMPOSITION-BARCODE`, `T3-BASE-VISUAL-ORACLES`, `T3-W4-PDFX-GS-POPPLER-SEP`, `T4-W4-BROCHURE-PDFX` | Exported `.pdf` carries `/GTS_PDFXVersion`, `/OutputIntents`, `/Trapped`, `/Type0`+`/CIDFontType2`+`/ToUnicode`, verified via `gs`, `pdfinfo -box` (`MediaBox`/`TrimBox`/`BleedBox`), `pdffonts` (`emb/sub/uni=yes`), `pdftotext` |
| 9 | `REQ-04` | `W4` | T1, T2, T3 | `T1-BASE-ROUNDTRIP-VEC`, `T1-BASE-EMPTY-PATH`, `T1-W4-DISPLAYLIST-SEP`, `T2-BASE-CLI-BVA-NUM`, `T2-BASE-ADVERSARIAL-ARCHIVE-XML`, `T2-W4-PREPRESS-IMPOSITION-BARCODE`, `T3-BASE-VISUAL-ORACLES`, `T3-W3-COONS-EFFECTS-TRANSP`, `T3-W4-PDFX-GS-POPPLER-SEP` | Baseline PDF/SVG round-trip + TrimBox pixel oracle + W4 `ExportDisplayList` & `cargo test -p coree-io -- display_list` |
| 10 | `REQ-05` | `W4` | T1, T2, T3, T4 | `T1-W4-DISPLAYLIST-SEP`, `T2-W4-PREPRESS-IMPOSITION-BARCODE`, `T3-W4-PDFX-GS-POPPLER-SEP`, `T4-W4-PACKAGING-FEFCO`, `T4-W5-BOOKLET-64P` | Vector CMYK/Spot separations + `Trapping` + `Halftone` + `PerfectBound` imposition + `cargo test -p coree-prod -- prepress` |
| 11 | `REQ-06` | `W2` | T1, T2, T3, T4 | `T1-BASE-GEN-INSPECT`, `T1-W2-CDR-LEGACY-XML`, `T2-BASE-CORRUPT-HEADERS`, `T2-BASE-ADVERSARIAL-ARCHIVE-XML`, `T2-W2-CDR-CODEC-BOUNDARY`, `T3-W2-PERSP-CLIP-DIM-CDR`, `T4-W2-CDR-MIGRATION` | Baseline RIFF/X4/X6/CDT dissectors + W2 ZCF Spot color CLI test, `quick-xml`/`roxmltree` in `zcf.rs`, `cargo test -p coree-cdr -- zcf` |
| 12 | `REQ-07` | `W3–W5` | T1, T2, T4 | `T1-BASE-PREFLIGHT-COLL`, `T1-W4-PREFLIGHT-IPP`, `T2-BASE-CLI-BVA-NUM`, `T2-W3-TYPOGRAPHY-STYLUS-SCULPT`, `T2-W4-PREPRESS-IMPOSITION-BARCODE`, `T4-W4-PACKAGING-FEFCO`, `T4-W5-BOOKLET-64P` | Baseline preflight/collect + W4 live `coree-cli preflight --json` `MissingFont`, `OS/2 fsType` in `collect.rs` |
| 13 | `REQ-08` | `W5` | T1, T2 | `T1-W5-SWF-CP1256-UX`, `T2-BASE-ARABIC-CP1256-EDGE`, `T2-W5-VIEW-GUIDES-NAVIGATOR-PRINTUI` | `S15` interactive codepage selector (`CP1256`/`CP1251`/`UTF-8`) + `S16` Hex/ASCII chunk byte viewer |
| 14 | `REQ-09` | `W1` | T1, T2 | `T1-W1-VECTOR-CLIPBOARD`, `T2-W1-IPC-UDS-CLIPBOARD` | Pure-Rust OS Live Vector Clipboard codec (`SVG`/`PDF`/`EMF`) + `cargo test -p coree-io -- clipboard` |
| 15 | `REQ-10` | `W1, W4` | T1, T2 | `T1-W1-IPC-UDS`, `T1-W4-PREFLIGHT-IPP`, `T2-W1-IPC-UDS-CLIPBOARD`, `T2-W4-PREPRESS-IMPOSITION-BARCODE` | `W1`: UDS `0600`/NamedPipe IPC + `verify_invariants.py --strict-no-std-net` + `cargo test`; `W4`: native `CUPS IPP` (no `lp` shell-out) |
| 16 | `REQ-11` | `W5` | T1, T2 | `T1-W1-SWEEP-PACKAGING`, `T1-W5-I18N-BG-PKG`, `T2-W5-I18N-RTL-SHELL-MIRRORING` | `packaging/linux/cargo-sources.json` valid non-empty JSON via `json.loads()`, Windows packaging, `ci.yml` gates |
| 17 | `REQ-12` | `W5` | T1, T2, T3 | `T1-W5-I18N-BG-PKG`, `T2-W5-WELCOME-DOCKERS-PRESETS`, `T2-W5-VIEW-GUIDES-NAVIGATOR-PRINTUI`, `T3-W5-PAGEBG-SPREAD-PREVIEW` | `Page.printable_background` live `coree-cli render` non-white corner pixel verification |
| 18 | `ADD-01` | `W2–W3` | T1, T2, T3, T4 | `T1-W3-EFFECTS-COONS-LENS`, `T2-W2-DRAWING-MODES-MASTER`, `T2-W3-COLOR-MESH-EFFECTS-LENS`, `T3-BASE-BLOCKSHADOW-DISTORT`, `T3-W2-BSPLINE-MEDIA-CLONE`, `T3-W3-COONS-EFFECTS-TRANSP`, `T4-W4-POTRACE-COONS-ICC` | `Effect::InnerShadow` live `.coree` CLI render + `Bevel` / `Extrude` / `Transparency` |
| 19 | `ADD-02` | `W2` | T1, T2, T3 | `T1-W2-BSPLINE-SYM-PERSP`, `T2-W2-DRAWING-MODES-MASTER`, `T3-W2-BSPLINE-MEDIA-CLONE` | `NodePayload::BSpline` live `.coree` CLI render + `ArtisticMedia` + `Clone` |
| 20 | `ADD-03` | `W2` | T1, T2, T3, T4 | `T1-W2-BSPLINE-SYM-PERSP`, `T2-W2-DRAWING-MODES-MASTER`, `T3-W2-PERSP-CLIP-DIM-CDR`, `T4-W2-CDR-MIGRATION` | `Symbol` isolated studio + `PowerClip` `FitContents`/`CenterContents`/`StretchToFill` |
| 21 | `ADD-04` | `W2` | T1, T2, T3 | `T1-W2-BSPLINE-SYM-PERSP`, `T2-W2-DRAWING-MODES-MASTER`, `T3-W2-PERSP-CLIP-DIM-CDR` | `3×3` homography perspective transform in `coree-core/src/math.rs` |
| 22 | `ADD-05` | `W4` | T1, T2, T3, T4 | `T1-BASE-LASER`, `T1-W4-FEFCO-2D-KERF`, `T2-W4-PREPRESS-IMPOSITION-BARCODE`, `T3-W4-PDFX-GS-POPPLER-SEP`, `T4-W4-PACKAGING-FEFCO`, `T4-W4-LASER-NESTING`, `T4-W4-PRINT-MERGE-10K` | Baseline `CutContour` + W4 `30+` FEFCO, `DataMatrix`/`Pdf417`/`Aztec`/`Gs1128`, `cargo test -p coree-prod --lib` |
| 23 | `ADD-06` | `W4` | T1, T2, T3, T4 | `T1-BASE-ROUNDTRIP-VEC`, `T1-W4-DWG-EMF-ACB-PSD`, `T2-W4-EXCHANGE-DWG-EMF-PSD-BVA`, `T3-W4-EPS-ALPHA-SVG-CAD-PSD`, `T4-W4-PHOTO16-PSD` | Baseline DXF/PLT/PSD + W4 `.dwg`/`.emf`/`.acb` `inspect --json` `detected_format` + `.emf`/`.wmf` (`records >= 3`, `has_eof`)/`.dwg` (`dissect_dwg` + `inspect --json`)/`.psd` export dissectors |
| 24 | `ADD-07` | `W5` | T1, T2, T3, T4 | `T1-W5-SWF-CP1256-UX`, `T2-W5-VIEW-GUIDES-NAVIGATOR-PRINTUI`, `T3-W5-PAGEBG-SPREAD-PREVIEW`, `T4-W5-BOOKLET-64P` | `Facing Pages` spreads (live CLI `--multipage` diff), `AutoPageNumber` dynamic field, `DrawingScale` |
| 25 | `ADD-08` | `W5` | T1, T2, T3 | `T1-W5-SWF-CP1256-UX`, `T2-W5-VIEW-GUIDES-NAVIGATOR-PRINTUI`, `T3-W5-VIEW-SNAP-SHAPES-SCULPT` | Live `coree-cli render --view simple-wireframe` vs `enhanced` pixel diff + 5 geometric snaps + `Navigator` |
| 26 | `ADD-09` | `W5` | T1, T2 | `T1-W5-UI-65-NO-BOX`, `T2-W5-WELCOME-DOCKERS-PRESETS` | `WorkspacePreset::Illustrator`, `Long-press Tint Grid`, `Drag-and-drop swatch` |
| 27 | `ADD-10` | `W3` | T1, T2, T3, T4 | `T1-W3-TYPO-STYLUS`, `T2-W3-TYPOGRAPHY-STYLUS-SCULPT`, `T3-BASE-VISUAL-ORACLES`, `T3-W3-TYPO-WRAP-FVAR-CURVES`, `T4-W4-BROCHURE-PDFX` | `linked_next_frame`, `TextWrapMode`, `TextOnPath`, `DropCap`, `fvar`, `SubpixelMask`, `cargo test -p coree-render -- kashida` |
| 28 | `ADD-11` | `W4` | T1, T2, T4 | `T1-BASE-RHAI-MACRO`, `T1-W4-RHAI-DOM-CQL`, `T2-W4-CQL-RHAI-PLUGIN-SANDBOX`, `T4-W4-SCENE10K-BENCH` | Baseline Rhai macro (RGB->CMYK verified) + W4 `coree-cli cql` + `Rhai Scene DOM` + `on_before_export` + JSON-RPC plugin host |
| 29 | `ADD-12` | `W3–W5` | T1, T2, T3, T4 | `T1-W4-POTRACE-PHOTO16`, `T2-W3-PHOTO-STUDIO-BOUNDARY`, `T3-W3-PHOTO16-FILTERS`, `T4-W4-PHOTO16-PSD` | `ChannelMixer` / `ToneCurve` / `Histogram` + `VectorPattern`/`BitmapPattern` |
| 30 | `IMP-01` | `W3` | T1, T2, T3, T4 | `T1-BASE-RENDER-THUMB`, `T1-W3-WGPU-TILES`, `T2-W5-VIEW-GUIDES-NAVIGATOR-PRINTUI`, `T3-BASE-VISUAL-ORACLES`, `T3-W3-WGPU-CPU-GPU-PARITY`, `T4-W4-SCENE10K-BENCH` | Baseline `TinySkiaBackend` 4 views/multipage/thumb + W3 `struct WgpuBackend` + `impl CanvasBackend` |
| 31 | `IMP-02` | `W3` | T1, T2, T3, T4 | `T1-W3-WGPU-TILES`, `T2-W5-VIEW-GUIDES-NAVIGATOR-PRINTUI`, `T3-W3-WGPU-CPU-GPU-PARITY`, `T4-W4-SCENE10K-BENCH` | World-space `struct WorldTileKey` `DirtyTileCache` + `cargo test -p coree-render -- tile_cache` |
| 32 | `IMP-03` | `W3` | T1, T2, T3 | `T1-W3-TYPO-STYLUS`, `T2-W3-TYPOGRAPHY-STYLUS-SCULPT`, `T3-W3-ICC-BPC-CHAD-DE2000` | `StylusProvider` (`crates/coree-app/src/stylus.rs`) + `DisplayIccProvider` |
| 33 | `IMP-04` | `W3` | T1, T2, T3, T4 | `T1-W3-EFFECTS-COONS-LENS`, `T2-W3-COLOR-MESH-EFFECTS-LENS`, `T3-W3-COONS-EFFECTS-TRANSP`, `T4-W4-POTRACE-COONS-ICC` | Bicubic Coons `MeshFill` (`MeshNode` N/S/E/W tangent handles + node alpha, runtime node alpha diff) + 4-curve `Envelope` + `Blend` tangent rotation |
| 34 | `IMP-05` | `W2, W4, W5` | T1, T2, T3, T4 | `T1-BASE-TRACE`, `T1-W2-SMARTFILL-META`, `T1-W4-POTRACE-PHOTO16`, `T2-BASE-CLI-BVA-NUM`, `T2-W2-CDRCRV-GEOMETRY-BVA`, `T2-W5-POWERTRACE-INTERACTIVE`, `T3-BASE-VISUAL-ORACLES`, `T3-W2-DCEL-CURVES-BOOL`, `T3-W4-POTRACE-ZEROGAP`, `T4-W4-POTRACE-COONS-ICC` | Baseline trace + W2 planar `DCEL` `SmartFill` (`cargo test`) + W4 topological `Potrace` closed SVG paths |
| 35 | `IMP-06` | `W3–W5` | T1, T2, T3, T4 | `T1-W4-POTRACE-PHOTO16`, `T2-W3-PHOTO-STUDIO-BOUNDARY`, `T3-W3-PHOTO16-FILTERS`, `T3-W4-EPS-ALPHA-SVG-CAD-PSD`, `T4-W4-PHOTO16-PSD` | Tiled 16-bit (`u16`) CoW `RasterAsset` + `ChannelMixer` + `psd.rs` >2048px/16b/masks/Spot |
| 36 | `IMP-07` | `W1, W4` | T1, T2, T4 | `T1-BASE-PRINT-MERGE`, `T1-W1-AUTOSAVE-WAL`, `T2-BASE-CLI-BVA-NUM`, `T2-BASE-ARABIC-CP1256-EDGE`, `T2-W1-SQLITE-QUARANTINE-WAL`, `T4-W4-PRINT-MERGE-10K`, `T4-W4-SCENE10K-BENCH` | Baseline Arabic/Latin CSV print-merge (`2` pages verified) + W1 async differential autosave/corrupt SQLite quarantine + W4 streaming |
| 37 | `IMP-08` | `W5` | T1, T2, T3 | `T1-W5-UI-65-NO-BOX`, `T2-W5-WELCOME-DOCKERS-PRESETS`, `T3-W5-VIEW-SNAP-SHAPES-SCULPT` | `65/65` icons & Property Bars, zero `DrawingBox` in `canvas.rs`, `cargo test -p coree-app --lib` |
| 38 | `IMP-09` | `W5` | T1, T2, T3 | `T1-W5-SWF-CP1256-UX`, `T2-W5-WELCOME-DOCKERS-PRESETS`, `T2-W5-VIEW-GUIDES-NAVIGATOR-PRINTUI`, `T3-W5-PAGEBG-SPREAD-PREVIEW` | Interactive multi-plate `PrintPreviewDialog` (`draw_print_preview_thumbnail` + `separations_preview`), `S03` drag-reorder (`CommandOp::ReorderPages`), `65/65` shortcuts |
| 39 | `IMP-10` | `W3, W5` | T1, T2, T3, T4 | `T1-W5-I18N-BG-PKG`, `T2-BASE-ARABIC-CP1256-EDGE`, `T2-W3-TYPOGRAPHY-STYLUS-SCULPT`, `T2-W5-WELCOME-DOCKERS-PRESETS`, `T2-W5-I18N-RTL-SHELL-MIRRORING`, `T3-W3-TYPO-WRAP-FVAR-CURVES`, `T4-W4-BROCHURE-PDFX` | Compile-time `enum I18nKey` (`488+` EN/AR) & `ShellLayoutDirections` RTL mirroring |
| 40 | `IMP-11` | `W1, W2, W5` | T1, T2, T3 | `T1-W1-MODULARIZE-ARCH`, `T2-W1-TOOLCORE-INTERACTION`, `T2-W1-COMMAND-DELTA-BVA`, `T2-W5-WELCOME-DOCKERS-PRESETS`, `T3-BASE-PAIRWISE-FILLS` | Split bloated files (`<= 1800` lines each), reconcile `replica/architecture.md` with `crates/coree-tools` |
| 41 | `IMP-12` | `W1, W3` | T1, T2, T4 | `T1-W1-BENCHES-INIT`, `T2-W5-WELCOME-DOCKERS-PRESETS`, `T4-W5-BOOKLET-64P`, `T4-W4-PHOTO16-PSD`, `T4-W4-PRINT-MERGE-10K`, `T4-W4-SCENE10K-BENCH` | Offline `benches/` directory + `[[bench]]` in `Cargo.toml` + `cargo check --benches` |
| 42 | `REM-01` | `W2` | T1, T2, T3 | `T1-W2-CURVES-CONT`, `T2-W2-CDRCRV-GEOMETRY-BVA`, `T3-BASE-OUTLINE-PEN`, `T3-W2-DCEL-CURVES-BOOL` | Analytical `erase_circle` (zero 96-seg polygon) + `stroke_to_closed_path` + `cargo test -p coree-core -- geometry` |
| 43 | `REM-02` | `W1, W2` | T1, T2, T3 | `T1-W2-CONVERT-CURVES`, `T2-W3-COLOR-MESH-EFFECTS-LENS`, `T2-W3-TYPOGRAPHY-STYLUS-SCULPT`, `T3-W3-TYPO-WRAP-FVAR-CURVES` | `coree-cli convert-to-curves` real glyph Bézier curves (`max_nodes > 4`, zero AABB) |
| 44 | `REM-03` | `W1, W2` | T1, T2, T3 | `T1-W2-SMARTFILL-META`, `T2-W2-CDRCRV-GEOMETRY-BVA`, `T2-W2-DRAWING-MODES-MASTER`, `T3-W2-DCEL-CURVES-BOOL` | `BooleanOp` metadata preservation (`cargo test -p coree-core -- boolean`) + Master `hit_test` |
| 45 | `REM-04` | `W3` | T1, T2, T3 | `T1-BASE-LENSES`, `T1-W3-EFFECTS-COONS-LENS`, `T2-W3-COLOR-MESH-EFFECTS-LENS`, `T3-BASE-LENSES-SEMANTIC`, `T3-W3-FROZEN-LENS-REMFACE` | Baseline all 11 Lenses semantic CLI render + W3 `.coree` `LensConfig::frozen_snapshot` persistence (surviving underlying object removal) + glyph-exact `remove_face` mask |
| 46 | `REM-05` | `W2` | T1, T2, T3, T4 | `T1-W2-UNKNOWN-CHUNKS`, `T2-W2-CDR-CODEC-BOUNDARY`, `T3-W2-PERSP-CLIP-DIM-CDR`, `T4-W2-CDR-MIGRATION` | Real `XDOC` (`LIST:doc `) + `XOBJ` (`LIST:obj `) injected RIFF `.cdr` round-trip verified byte-exact via `parse_riff_chunks` |
| 47 | `REM-06` | `W2` | T1, T2, T3, T4 | `T1-W2-CDR-LEGACY-XML`, `T2-W2-CDR-CODEC-BOUNDARY`, `T3-W2-PERSP-CLIP-DIM-CDR`, `T4-W2-CDR-MIGRATION` | Remove `[0,0,0,1]` Spot fallback in `zcf.rs` + `quick-xml`/`roxmltree` + live ZCF Spot CLI test |
| 48 | `REM-07` | `W2` | T1, T2, T4 | `T1-BASE-GEN-INSPECT`, `T1-W2-CMX-CPT-FULL`, `T2-BASE-CORRUPT-HEADERS`, `T2-W2-CDR-CODEC-BOUNDARY`, `T4-W2-CDR-MIGRATION` | Baseline CMX/CPT + W2 `CMX` `ixlr`/`ixpg` CLI round-trip + `CPT` CLI render & `CPTObject` |
| 49 | `REM-08` | `W4` | T1, T2, T3, T4 | `T1-BASE-ROUNDTRIP-VEC`, `T1-W4-EPS-TRANSP-CAD`, `T2-W4-EXCHANGE-DWG-EMF-PSD-BVA`, `T3-W4-EPS-ALPHA-SVG-CAD-PSD`, `T4-W4-LASER-NESTING` | `gs -dEPSCrop pngalpha` (`opaque_white_pixels == 0`), dynamic SVG `TwoColorPattern`, DXF `TEXT`/`DIMENSION` & PLT `CI`/`AA` |
| 50 | `REM-09` | `W4` | T1, T2, T4 | `T1-BASE-LASER`, `T1-W4-FEFCO-2D-KERF`, `T2-BASE-CLI-BVA-NUM`, `T4-W4-LASER-NESTING` | `coree-cli laser --kerf` acute star closed SVG + Boolean self-intersection cleanup |
| 51 | `REM-10` | `W1, W2` | T1, T2, T3 | `T1-W2-CURVES-CONT`, `T2-W2-CDRCRV-GEOMETRY-BVA`, `T3-W2-DCEL-CURVES-BOOL` | `PathNode::enforce_continuity` (`Smooth`/`Symmetrical`) + `Document` encapsulation (`T1-W1-COMMAND-OPS`) |

### 4. All 102 TestCases Indexed by Tier & Wave (`T1`–`T4`)

| # | Test ID | Tier | Target Wave | Smoke | Roadmap IDs | Feature Refs | Title |
|---:|---|---|---|---|---|---|---|
| 1 | `T1-BASE-CLI-CORE` | `T1` | `baseline` | `True` | `MND-05` | `EXT-F01`, `EXT-F02` | coree-cli --version, --diagnostics, --help subcommands & sibling GUI binary |
| 2 | `T1-BASE-GEN-INSPECT` | `T1` | `baseline` | `True` | `REQ-06`, `REM-07` | `FEAT-07`, `FEAT-81`, `FEAT-82`, `EXT-F04` | generate-sample + inspect --json + independent Python/file dissectors |
| 3 | `T1-BASE-ROUNDTRIP-VEC` | `T1` | `baseline` | `True` | `REQ-04`, `ADD-06`, `REM-08` | `FEAT-21`, `FEAT-22`, `FEAT-35`, `FEAT-75`, `FEAT-83`, `FEAT-84`, `FEAT-85`, `FEAT-86` | Vector round-trips + Ghostscript + Poppler + TrimBox pixel comparison |
| 4 | `T1-BASE-EMPTY-PATH` | `T1` | `baseline` | `True` | `MND-05`, `REQ-04` | `EXT-F02`, `FEAT-83`, `FEAT-84` | Pure-Rust self-contained PDF/EPS/render fallback with PATH='' |
| 5 | `T1-BASE-RENDER-THUMB` | `T1` | `baseline` | `True` | `IMP-01` | `FEAT-05`, `FEAT-08` | Render 4 view modes, multipage & thumbnail --size 64 PNG pixel invariants |
| 6 | `T1-BASE-LASER` | `T1` | `baseline` | `True` | `ADD-05`, `REM-09` | `EXT-F06`, `EXT-F08` | Laser G-Code (GRBL/Marlin/Ruida), DXF, PLT, kerf & lightburn-bridge |
| 7 | `T1-BASE-PREFLIGHT-COLL` | `T1` | `baseline` | `True` | `REQ-07` | `FEAT-77`, `FEAT-80`, `EXT-F07` | Preflight --json, Collect for Output bundle & ProductionMetrics Path Meter |
| 8 | `T1-BASE-PRINT-MERGE` | `T1` | `baseline` | `True` | `IMP-07` | `FEAT-78`, `EXT-F05` | Print Merge bilingual Latin+Arabic UTF-8 CSV -> .coree & PDF |
| 9 | `T1-BASE-TRACE` | `T1` | `baseline` | `True` | `IMP-05` | `FEAT-70`, `FEAT-71` | PowerTRACE PNG/BMP -> SVG <path> + re-render verification |
| 10 | `T1-BASE-RHAI-MACRO` | `T1` | `baseline` | `True` | `ADD-11` | `EXT-F11`, `SKIP-06` | Rhai file macro & built-in recolor-cmyk semantic fill check |
| 11 | `T1-BASE-HYGIENE-PAL` | `T1` | `baseline` | `True` | `MND-05` | `FEAT-37`, `SKIP-01`, `SKIP-02`, `SKIP-03`, `EXT-F02` | Workspace #![forbid(unsafe_code)], 0 network crates, no #009c4a, valid .gpl/.ase/.pal |
| 12 | `T1-BASE-LENSES` | `T1` | `baseline` | `True` | `REM-04` | `FEAT-52`, `FEAT-53`, `FEAT-54`, `FEAT-55`, `FEAT-56`, `FEAT-57`, `FEAT-58`, `FEAT-59`, `FEAT-60`, `FEAT-61`, `FEAT-62` | All 11 non-destructive Lenses (FEAT-52..62) semantic CLI render vs control |
| 13 | `T1-REPLICA-CONTRAST` | `T1` | `baseline` | `True` | `MND-01` | `SKIP-05`, `EXT-F02` | Replica contrast.py 35/35 AA pass + negative contrast rejection |
| 14 | `T1-REPLICA-LISTING` | `T1` | `baseline` | `True` | `MND-01` | `SKIP-05`, `EXT-F01` | Replica listing.py store compliance + negative avoid-term rejection |
| 15 | `T1-REPLICA-PARITY` | `T1` | `baseline` | `True` | `MND-05` | `SKIP-01`, `SKIP-02`, `SKIP-03`, `SKIP-04`, `SKIP-05`, `SKIP-06` | Replica parity.py features.csv 86/68/6/12 integrity |
| 16 | `T1-REPLICA-SWEEP-APP` | `T1` | `baseline` | `True` | `MND-01` | `SKIP-05` | Replica sweep.py crates/coree-app clean (0 hits) |
| 17 | `T1-W1-SWEEP-CRATES` | `T1` | `W1` | `False` | `MND-01` | `SKIP-05`, `EXT-F02` | W1 MND-01: sweep.py crates/ returns 0 hits |
| 18 | `T1-W1-SWEEP-PACKAGING` | `T1` | `W1` | `False` | `MND-01`, `REQ-11` | `EXT-F09` | W1 MND-01: scripts/brand_sweep.py + packaging/ MIME allowlist CI gate & brand.md log |
| 19 | `T1-W1-COREE-TOOLS` | `T1` | `W1` | `False` | `MND-02` | `FEAT-02` | W1 MND-02: coree-tools crate without egui, 0 objects.get_mut & 0 drag mark_all_dirty() in canvas.rs |
| 20 | `T1-W1-FUZZ-PACK` | `T1` | `W1` | `False` | `MND-04` | `FEAT-81` | W1-W3 MND-04: fuzz/ targets & isolated real .cdr test pack compiled & inspected |
| 21 | `T1-W1-VERSION-SYNC` | `T1` | `W1` | `False` | `MND-05` | `EXT-F02` | W1 MND-05: scripts/verify_invariants.py + automated version sync across manifests & ci.yml |
| 22 | `T1-W1-COMMAND-OPS` | `T1` | `W1` | `False` | `REQ-01` | `FEAT-08`, `FEAT-14` | W1 REQ-01: CommandOp completeness, DocumentDelta, encapsulation & cargo test |
| 23 | `T1-W1-VECTOR-CLIPBOARD` | `T1` | `W1` | `False` | `REQ-09` | — | W1 REQ-09: OS Live Vector Clipboard codec (SVG/PDF/EMF) + cargo test |
| 24 | `T1-W1-IPC-UDS` | `T1` | `W1` | `False` | `REQ-10` | `EXT-F01` | W1 REQ-10: Bidirectional UDS (0600) / NamedPipe IPC + strict-no-std-net |
| 25 | `T1-W1-AUTOSAVE-WAL` | `T1` | `W1` | `False` | `IMP-07` | `EXT-F10` | W1 IMP-07: SQLite WAL + async differential autosave + corrupt DB quarantine |
| 26 | `T1-W1-MODULARIZE-ARCH` | `T1` | `W1` | `False` | `IMP-11` | — | W1 IMP-11: Reconcile replica/architecture.md & modularize bloated files |
| 27 | `T1-W1-BENCHES-INIT` | `T1` | `W1` | `False` | `IMP-12` | `EXT-F12` | W1 IMP-12: Quantitative offline benchmarks directory & cargo check --benches |
| 28 | `T1-W2-CDR-WRITER` | `T1` | `W2` | `False` | `MND-03` | `FEAT-81`, `FEAT-28`, `FEAT-29`, `FEAT-30` | W2 MND-03: Authentic 20B reversed loda + CDRO 16B ZCF + docPalette.xml + non-flattening Table/Dim/Conn |
| 29 | `T1-W2-CDR-LEGACY-XML` | `T1` | `W2` | `False` | `REQ-06`, `REM-06` | `FEAT-81` | W2 REQ-06/REM-06: ZCF Spot color + quick-xml/roxmltree + zcf tests |
| 30 | `T1-W2-UNKNOWN-CHUNKS` | `T1` | `W2` | `False` | `REM-05` | `FEAT-81`, `EXT-F03` | W2 REM-05: Byte-exact XDOC/XOBJ UnknownChunkMap passthrough in .cdr round-trip |
| 31 | `T1-W2-CMX-CPT-FULL` | `T1` | `W2` | `False` | `REM-07` | `FEAT-82` | W2 REM-07: Complete CMX (disp/ixlr/ixpg) round-trip & CPT render/CPTObject |
| 32 | `T1-W2-CURVES-CONT` | `T1` | `W2` | `False` | `REM-01`, `REM-10` | `FEAT-15`, `FEAT-17`, `FEAT-35` | W2 REM-01/REM-10: Analytical erase_circle, PathNode continuity & cargo test |
| 33 | `T1-W2-CONVERT-CURVES` | `T1` | `W2` | `False` | `REM-02` | `FEAT-40` | W2 REM-02: coree-cli convert-to-curves real glyph outline conversion |
| 34 | `T1-W2-BSPLINE-SYM-PERSP` | `T1` | `W2` | `False` | `ADD-02`, `ADD-03`, `ADD-04` | `FEAT-18`, `FEAT-27`, `FEAT-42` | W2 ADD-02/03/04: BSpline CLI render, PowerClip Fit & 3x3 Perspective |
| 35 | `T1-W2-SMARTFILL-META` | `T1` | `W2` | `False` | `IMP-05`, `REM-03` | `FEAT-24`, `FEAT-25` | W2 IMP-05/REM-03: Planar DCEL SmartFill & BooleanOp cargo tests |
| 36 | `T1-W3-ICC-PIPELINE` | `T1` | `W3` | `False` | `REQ-02` | `FEAT-31`, `FEAT-36` | W3 REQ-02: Color/lerp routed via ICC + BPC + chad + CLI ICC render |
| 37 | `T1-W3-WGPU-TILES` | `T1` | `W3` | `False` | `IMP-01`, `IMP-02` | `FEAT-05`, `FEAT-13`, `EXT-F12` | W3 IMP-01/IMP-02: WgpuBackend + WorldTileKey DirtyTileCache + cargo test |
| 38 | `T1-W3-EFFECTS-COONS-LENS` | `T1` | `W3` | `False` | `ADD-01`, `IMP-04`, `REM-04` | `FEAT-33`, `FEAT-43`, `FEAT-63` | W3 ADD-01/IMP-04/REM-04: InnerShadow CLI render, Coons MeshFill & Frozen Lens |
| 39 | `T1-W3-TYPO-STYLUS` | `T1` | `W3` | `False` | `ADD-10`, `IMP-03` | `FEAT-38`, `FEAT-39`, `FEAT-40` | W3 ADD-10/IMP-03: Linked text frames, SubpixelMask, Stylus/DisplayICC & cargo test |
| 40 | `T1-W4-PDFX-SUBSET` | `T1` | `W4` | `False` | `REQ-03` | `FEAT-83`, `FEAT-38`, `EXT-F05` | W4 REQ-03: Strict PDF/X-1a/4 + Ghostscript + pdfinfo + CIDFontType2 pdffonts/pdftotext |
| 41 | `T1-W4-DISPLAYLIST-SEP` | `T1` | `W4` | `False` | `REQ-04`, `REQ-05` | `FEAT-74`, `FEAT-76`, `FEAT-83` | W4 REQ-04/REQ-05: ExportDisplayList + Vector Separations + Trapping + Halftone + PerfectBound |
| 42 | `T1-W4-DWG-EMF-ACB-PSD` | `T1` | `W4` | `False` | `ADD-06` | `FEAT-37`, `FEAT-86`, `SKIP-04` | W4 ADD-06: .dwg/.emf/.acb inspect format & .emf/.wmf/.dwg/.psd export dissectors |
| 43 | `T1-W4-FEFCO-2D-KERF` | `T1` | `W4` | `False` | `ADD-05`, `REM-09` | `FEAT-79`, `EXT-F06`, `EXT-F08` | W4 ADD-05/REM-09: 30+ FEFCO + 2D barcodes + Boolean Kerf CLI laser test |
| 44 | `T1-W4-RHAI-DOM-CQL` | `T1` | `W4` | `False` | `ADD-11` | `FEAT-14`, `EXT-F11` | W4 ADD-11: coree-cli cql + Rhai Scene DOM + event hooks + JSON-RPC plugin host |
| 45 | `T1-W4-EPS-TRANSP-CAD` | `T1` | `W4` | `False` | `REM-08` | `FEAT-28`, `FEAT-32`, `FEAT-84`, `FEAT-85` | W4 REM-08: EPS transparent alpha (gs -dEPSCrop), dynamic SVG TwoColorPattern & DXF TEXT/DIMENSION + PLT CI/AA |
| 46 | `T1-W4-PREFLIGHT-IPP` | `T1` | `W4` | `False` | `REQ-07`, `REQ-10` | `FEAT-77`, `FEAT-80` | W4 REQ-07/REQ-10: Live MissingFont preflight, OS/2 fsType collect & native CUPS IPP |
| 47 | `T1-W4-POTRACE-PHOTO16` | `T1` | `W4` | `False` | `IMP-05`, `IMP-06`, `ADD-12` | `FEAT-68`, `FEAT-70`, `FEAT-71`, `FEAT-86` | W4 IMP-05/06/ADD-12: Topological Potrace closed SVG + 16-bit CoW RasterAsset & ChannelMixer |
| 48 | `T1-W5-UI-65-NO-BOX` | `T1` | `W5` | `False` | `IMP-08`, `ADD-09` | `FEAT-02`, `FEAT-06`, `FEAT-37` | W5 IMP-08/ADD-09: 65/65 icons & prop bars, zero DrawingBox, Illustrator preset & coree-app tests |
| 49 | `T1-W5-I18N-BG-PKG` | `T1` | `W5` | `False` | `REQ-11`, `REQ-12`, `IMP-10` | `FEAT-11`, `EXT-F01`, `EXT-F05`, `EXT-F09` | W5 REQ-11/12/IMP-10: Valid cargo-sources.json, live printable Page bg render, I18nKey & RTL Mirroring |
| 50 | `T1-W5-SWF-CP1256-UX` | `T1` | `W5` | `False` | `REQ-08`, `ADD-07`, `ADD-08`, `IMP-09` | `FEAT-05`, `EXT-F04`, `EXT-F05` | W5 REQ-08/ADD-07/08/IMP-09: Live simple-wireframe render, S15 CP1256 & S16 Hex viewer |
| 51 | `T2-BASE-CLI-BVA-NUM` | `T2` | `baseline` | `True` | `REQ-04`, `REQ-07`, `IMP-05`, `IMP-07`, `REM-09` | `FEAT-07`, `FEAT-77`, `FEAT-83`, `FEAT-84`, `FEAT-85`, `FEAT-86`, `EXT-F07` | CLI numeric/flag BVA (0, min, max, max+1, neg, NaN, Inf) with 0 panics |
| 52 | `T2-BASE-CORRUPT-HEADERS` | `T2` | `baseline` | `True` | `MND-04`, `REQ-06`, `REM-07` | `FEAT-81`, `FEAT-82`, `FEAT-83`, `FEAT-84`, `FEAT-85`, `FEAT-86`, `EXT-F04` | 0-byte (17 exts), truncated RIFF/CMX/BMP, encrypted PDF, bad PSD/PNG/CPT rejection |
| 53 | `T2-BASE-ADVERSARIAL-ARCHIVE-XML` | `T2` | `baseline` | `True` | `MND-04`, `REQ-06`, `REQ-04` | `FEAT-81`, `FEAT-84`, `FEAT-86`, `EXT-F03` | Zip-Slip, Zip-Bomb (ratio & 528M), Zlib-Bomb, depth-200, 65k DIB & XML DTD/depth bombs |
| 54 | `T2-BASE-ARABIC-CP1256-EDGE` | `T2` | `baseline` | `True` | `REQ-08`, `IMP-07`, `IMP-10` | `FEAT-38`, `FEAT-78`, `FEAT-81`, `EXT-F05` | CP1256 Arabic (explicit+heuristic+0x81/0x8D/0xFF), RTL Kashida/ZW & Print Merge CSV BVA |
| 55 | `T2-BASE-JSON-COREE-NAN-CYCLE` | `T2` | `baseline` | `True` | `REQ-01`, `MND-04` | `FEAT-07`, `FEAT-21`, `FEAT-22`, `FEAT-24`, `FEAT-31`, `FEAT-35`, `FEAT-42` | 0x0/1-pt path, PowerClip/Group cycle, 0-page/NaN JSON & Arabic UTF-8 file paths |
| 56 | `T2-W1-TOOLCORE-INTERACTION` | `T2` | `W1` | `False` | `MND-02`, `IMP-11` | `FEAT-12`, `FEAT-34`, `FEAT-02`, `FEAT-21`, `FEAT-22`, `FEAT-23`, `FEAT-29` | W1 MND-02: ToolCore headless pointer/snap/pivot/InteractiveFill/Eyedropper/Connector |
| 57 | `T2-W1-COMMAND-DELTA-BVA` | `T2` | `W1` | `False` | `REQ-01`, `IMP-11` | `FEAT-07`, `FEAT-08`, `FEAT-14`, `FEAT-37`, `FEAT-42` | W1 REQ-01: CommandOp/DocumentDelta BVA, ReorderPages OOB, FitPowerClip & FindAndReplace |
| 58 | `T2-W1-IPC-UDS-CLIPBOARD` | `T2` | `W1` | `False` | `REQ-09`, `REQ-10` | `FEAT-01`, `FEAT-06`, `EXT-F01` | W1 REQ-09/10: UDS 0600 JSON-RPC IPC boundary & Live Vector Clipboard (SVG/PDF/EMF) |
| 59 | `T2-W1-SQLITE-QUARANTINE-WAL` | `T2` | `W1` | `False` | `IMP-07` | `FEAT-01`, `FEAT-78`, `EXT-F10` | W1 IMP-07: Corrupt SQLite DB quarantine, SHA-256 snapshot & differential WAL recovery |
| 60 | `T2-W2-CDRCRV-GEOMETRY-BVA` | `T2` | `W2` | `False` | `REM-01`, `REM-03`, `REM-10`, `IMP-05` | `FEAT-15`, `FEAT-17`, `FEAT-24`, `FEAT-25`, `FEAT-35` | W2 REM-01/03/10/IMP-05: Analytical erase_circle, knife, continuity, Boolean & DCEL SmartFill |
| 61 | `T2-W2-DRAWING-MODES-MASTER` | `T2` | `W2` | `False` | `ADD-01`, `ADD-02`, `ADD-03`, `ADD-04`, `REM-03`, `MND-03` | `FEAT-09`, `FEAT-18`, `FEAT-19`, `FEAT-20`, `FEAT-26`, `FEAT-27`, `FEAT-28`, `FEAT-30`, `FEAT-42` | W2 ADD-01..04/MND-03: BSpline, ArtisticMedia, Clone, MasterLayer, Symmetry, Perspective, Table, Dim |
| 62 | `T2-W2-CDR-CODEC-BOUNDARY` | `T2` | `W2` | `False` | `MND-03`, `REQ-06`, `REM-05`, `REM-06`, `REM-07` | `FEAT-11`, `FEAT-81`, `FEAT-82`, `EXT-F03`, `EXT-F04` | W2 MND-03/REQ-06/REM-05..07: ZCF OOB ptr/XML entity, cyclic cdss, XDOC/XOBJ, CMX, CPT, CDT |
| 63 | `T2-W3-COLOR-MESH-EFFECTS-LENS` | `T2` | `W3` | `False` | `REQ-02`, `ADD-01`, `IMP-04`, `REM-02`, `REM-04` | `FEAT-31`, `FEAT-32`, `FEAT-33`, `FEAT-43`, `FEAT-45`, `FEAT-46`, `FEAT-48`, `FEAT-49`, `FEAT-63` | W3 REQ-02/ADD-01/IMP-04/REM-04: ICC limits, MeshFill, InnerShadow/Contour/Blend/Envelope/Extrude, Frozen Lens |
| 64 | `T2-W3-TYPOGRAPHY-STYLUS-SCULPT` | `T2` | `W3` | `False` | `ADD-10`, `IMP-03`, `REQ-07`, `IMP-10`, `REM-02` | `FEAT-41`, `FEAT-16`, `FEAT-20`, `FEAT-38`, `FEAT-39`, `FEAT-40` | W3 ADD-10/IMP-03/REQ-07: Font quarantine & Glyphs (FEAT-41), linked frames, TextOnPath, Sculpt, Stylus |
| 65 | `T2-W3-PHOTO-STUDIO-BOUNDARY` | `T2` | `W3` | `False` | `IMP-06`, `ADD-12`, `REQ-02` | `FEAT-51`, `FEAT-64`, `FEAT-65`, `FEAT-66`, `FEAT-67`, `FEAT-68`, `FEAT-69` | W3 IMP-06/ADD-12: 16-bit CoW Photo Studio, Pixel Layers, Masks, 8 Brushes, ChannelMixer & Resample |
| 66 | `T2-W4-PREPRESS-IMPOSITION-BARCODE` | `T2` | `W4` | `False` | `REQ-03`, `REQ-04`, `REQ-05`, `REQ-07`, `REQ-10`, `ADD-05` | `FEAT-73`, `FEAT-74`, `FEAT-77`, `FEAT-78`, `FEAT-79`, `FEAT-80`, `FEAT-83` | W4 REQ-03..07/10/ADD-05: Imposition, 2D Barcodes, FEFCO, Preflight/Collect, CUPS IPP & PDF |
| 67 | `T2-W4-EXCHANGE-DWG-EMF-PSD-BVA` | `T2` | `W4` | `False` | `ADD-06`, `REM-08`, `MND-04` | `FEAT-37`, `FEAT-84`, `FEAT-85`, `FEAT-86` | W4 ADD-06/REM-08/MND-04: DWG CRC, EMF/WMF/ACB bounds, >2048px/RLE PSD, DXF/EPS/PLT |
| 68 | `T2-W4-CQL-RHAI-PLUGIN-SANDBOX` | `T2` | `W4` | `False` | `ADD-11`, `REQ-01` | `FEAT-14`, `EXT-F11` | W4 ADD-11/REQ-01: CQL syntax/OOB, Rhai max_operations sandbox & JSON-RPC plugin limits |
| 69 | `T2-W5-WELCOME-DOCKERS-PRESETS` | `T2` | `W5` | `False` | `REQ-12`, `ADD-09`, `IMP-08`, `IMP-09`, `IMP-10`, `IMP-11`, `IMP-12` | `FEAT-01`, `FEAT-03`, `FEAT-02`, `FEAT-06`, `FEAT-37` | W5 REQ-12/ADD-09/IMP-08..12: S01 Welcome/Tabs (FEAT-01), 14 Dockers (FEAT-03), 65/65, Tint/Drag, Illustrator |
| 70 | `T2-W5-POWERTRACE-INTERACTIVE` | `T2` | `W5` | `False` | `IMP-05` | `FEAT-72` | W5 IMP-05: PowerTRACE S11 interactive sliders, SplitPreview & >16MP clamp (FEAT-72) |
| 71 | `T2-W5-VIEW-GUIDES-NAVIGATOR-PRINTUI` | `T2` | `W5` | `False` | `ADD-07`, `ADD-08`, `IMP-01`, `IMP-02`, `IMP-09`, `REQ-08`, `REQ-12` | `FEAT-04`, `FEAT-05`, `FEAT-08`, `FEAT-10`, `FEAT-11`, `FEAT-13`, `FEAT-73`, `EXT-F04` | W5 ADD-07/08/IMP-01/02/09/REQ-08/12: Rulers/Guides/6-Views/Navigator/Facing/PrintPreview/S15/S16 |
| 72 | `T2-W5-I18N-RTL-SHELL-MIRRORING` | `T2` | `W5` | `False` | `IMP-10`, `REQ-11` | `FEAT-01`, `FEAT-02`, `FEAT-03`, `FEAT-38`, `EXT-F05` | W5 IMP-10/REQ-11: 488+ I18nKey, ShellLayoutDirections RTL mirroring, text_focus & packaging |
| 73 | `T3-BASE-ICC-PARSER-DE2000` | `T3` | `baseline` | `True` | `REQ-02` | — | Pure-Python stdlib ICC v2/v4 parser (colord+ghostscript) & CIEDE2000 math oracle |
| 74 | `T3-BASE-PAIRWISE-FILLS` | `T3` | `baseline` | `True` | `MND-02`, `REQ-02`, `IMP-11` | `FEAT-21`, `FEAT-22`, `FEAT-31` | Pairwise ColorModels x ParametricShapes x Fills CLI render verification |
| 75 | `T3-BASE-OUTLINE-PEN` | `T3` | `baseline` | `True` | `REM-01` | `FEAT-35` | Pairwise OutlineStyle (dashes, arrowheads, calligraphy, hairline, behind_fill) visual diffs |
| 76 | `T3-BASE-BLOCKSHADOW-DISTORT` | `T3` | `baseline` | `True` | `ADD-01`, `MND-03` | `FEAT-44`, `FEAT-47` | Baseline BlockShadow (FEAT-44) & Distort PushPull/Zipper/Twister (FEAT-47) visual oracles |
| 77 | `T3-BASE-LENSES-SEMANTIC` | `T3` | `baseline` | `True` | `REM-04` | `FEAT-52`, `FEAT-53`, `FEAT-54`, `FEAT-55`, `FEAT-56`, `FEAT-57`, `FEAT-58`, `FEAT-59`, `FEAT-60`, `FEAT-61`, `FEAT-62` | All 11 Lenses (FEAT-52..62) semantic pixel/channel checks + viewpoint X/Y offset |
| 78 | `T3-BASE-VISUAL-ORACLES` | `T3` | `baseline` | `True` | `REQ-03`, `REQ-04`, `IMP-01`, `IMP-05`, `ADD-10` | `FEAT-05`, `FEAT-39`, `FEAT-70`, `FEAT-75` | ViewModes, TextOnPath, PrintersMarks, Trace & GS/Poppler TrimBox pixel parity |
| 79 | `T3-W2-BSPLINE-MEDIA-CLONE` | `T3` | `W2` | `False` | `ADD-02`, `ADD-01`, `MND-02` | `FEAT-18`, `FEAT-19`, `FEAT-20`, `FEAT-26` | W2 ADD-02/FEAT-19/20/26: BSpline, ArtisticMedia, Clone, LiveSketch & Symmetry visual checks |
| 80 | `T3-W2-DCEL-CURVES-BOOL` | `T3` | `W2` | `False` | `IMP-05`, `REM-01`, `REM-03`, `REM-10` | `FEAT-15`, `FEAT-17`, `FEAT-24`, `FEAT-25` | W2 IMP-05/REM-01/03/10: Planar DCEL SmartFill, analytical erase_circle, continuity & BooleanOp |
| 81 | `T3-W2-PERSP-CLIP-DIM-CDR` | `T3` | `W2` | `False` | `MND-03`, `ADD-03`, `ADD-04`, `REQ-06`, `REM-05`, `REM-06` | `FEAT-27`, `FEAT-28`, `FEAT-42`, `FEAT-81` | W2 MND-03/ADD-03/04: 3x3 Perspective, PowerClip, Dimension & ZCF .cdr visual round-trip |
| 82 | `T3-W3-ICC-BPC-CHAD-DE2000` | `T3` | `W3` | `False` | `REQ-02`, `IMP-03` | `FEAT-31`, `FEAT-36` | W3 REQ-02: FOGRA39/SWOP ICC + BPC + chad CLI render vs pure-Python CIEDE2000 < 1.0 |
| 83 | `T3-W3-WGPU-CPU-GPU-PARITY` | `T3` | `W3` | `False` | `IMP-01`, `IMP-02` | `FEAT-05` | W3 IMP-01/02: WgpuBackend vs TinySkiaBackend CPU/GPU pixel parity & WorldTileKey |
| 84 | `T3-W3-COONS-EFFECTS-TRANSP` | `T3` | `W3` | `False` | `ADD-01`, `IMP-04`, `REQ-04` | `FEAT-33`, `FEAT-43`, `FEAT-45`, `FEAT-46`, `FEAT-48`, `FEAT-49`, `FEAT-50` | W3 ADD-01/IMP-04: Coons MeshFill, InnerShadow, Bevel, Extrude3D, 4 TransparencyModes, Contour, Blend & Envelope |
| 85 | `T3-W3-FROZEN-LENS-REMFACE` | `T3` | `W3` | `False` | `REM-04` | `FEAT-63` | W3 REM-04: Persistent frozen_snapshot lens & glyph-exact remove_face over 'O' |
| 86 | `T3-W3-TYPO-WRAP-FVAR-CURVES` | `T3` | `W3` | `False` | `ADD-10`, `REM-02`, `IMP-10` | `FEAT-38`, `FEAT-40` | W3 ADD-10/REM-02: Linked frames, TextWrapMode, DropCap, OpenTypeFeatures, fvar, SubpixelMask & convert-to-curves |
| 87 | `T3-W3-PHOTO16-FILTERS` | `T3` | `W3` | `False` | `IMP-06`, `ADD-12` | `FEAT-51`, `FEAT-65`, `FEAT-66`, `FEAT-67`, `FEAT-68`, `FEAT-69` | W3 IMP-06/ADD-12: Bitmap Effects stack, 16-bit CoW Photo Studio, ChannelMixer & Resample |
| 88 | `T3-W4-PDFX-GS-POPPLER-SEP` | `T3` | `W4` | `False` | `REQ-03`, `REQ-04`, `REQ-05`, `ADD-05` | `FEAT-74`, `FEAT-76`, `FEAT-79`, `FEAT-83` | W4 REQ-03/05/ADD-05: PDF/X-1a/4 + CIDFontType2 + PerfectBound + Separations + 2D Barcodes |
| 89 | `T3-W4-EPS-ALPHA-SVG-CAD-PSD` | `T3` | `W4` | `False` | `REM-08`, `ADD-06`, `IMP-06` | `FEAT-32`, `FEAT-84`, `FEAT-85`, `FEAT-86` | W4 REM-08/ADD-06: GS pngalpha EPS zero-halo, dynamic SVG TwoColorPattern, DXF/PLT & PSD |
| 90 | `T3-W4-POTRACE-ZEROGAP` | `T3` | `W4` | `False` | `IMP-05` | `FEAT-70`, `FEAT-71` | W4 IMP-05: Topological zero-gap Potrace OutlineTrace over adjacent color regions |
| 91 | `T3-W5-PAGEBG-SPREAD-PREVIEW` | `T3` | `W5` | `False` | `REQ-12`, `ADD-07`, `IMP-09` | `FEAT-09`, `FEAT-10`, `FEAT-11`, `FEAT-73` | W5 REQ-12/ADD-07/IMP-09: Printable Page Bg, FacingPages spreads, AutoPageNumber & PrintPreviewDialog |
| 92 | `T3-W5-VIEW-SNAP-SHAPES-SCULPT` | `T3` | `W5` | `False` | `ADD-08`, `IMP-08`, `MND-02` | `FEAT-02`, `FEAT-04`, `FEAT-05`, `FEAT-16`, `FEAT-23` | W5 ADD-08/IMP-08: simple-wireframe, 5 geometric snaps, 5 ParametricShapes, Sculpting & 65/65 Toolbox/PropertyBars/StatusBar |
| 93 | `T4-W4-BROCHURE-PDFX` | `T4` | `W4` | `False` | `REQ-03`, `REQ-02`, `ADD-10`, `IMP-10` | `FEAT-07`, `FEAT-36`, `FEAT-38`, `FEAT-75`, `FEAT-83`, `EXT-F05` | Commercial CMYK Brochure + Bilingual Arabic/English PDF/X-1a + CID Subset |
| 94 | `T4-W4-PACKAGING-FEFCO` | `T4` | `W4` | `False` | `ADD-05`, `REQ-05`, `REQ-07` | `FEAT-76`, `FEAT-77`, `FEAT-79`, `EXT-F06`, `EXT-F07` | Die-Cut Packaging Box (30+ FEFCO, 2D/GS1 Barcodes, Spot Separations, DXF) |
| 95 | `T4-W5-BOOKLET-64P` | `T4` | `W5` | `False` | `REQ-05`, `ADD-07`, `REQ-07`, `IMP-12` | `FEAT-07`, `FEAT-09`, `FEAT-10`, `FEAT-74`, `FEAT-80`, `FEAT-83` | 64-Page Facing-Pages Saddle-Stitch / PerfectBound Booklet + Collect RSS Gate |
| 96 | `T4-W4-LASER-NESTING` | `T4` | `W4` | `False` | `REM-09`, `REM-08`, `ADD-05` | `FEAT-84`, `FEAT-85`, `EXT-F06`, `EXT-F08` | Industrial Laser / Plotter Nesting (--kerf Cleanup, GRBL/Ruida, DXF/PLT/EPS) |
| 97 | `T4-W2-CDR-MIGRATION` | `T4` | `W2` | `False` | `MND-03`, `MND-04`, `REQ-06`, `REM-05`, `REM-06`, `REM-07`, `ADD-03` | `FEAT-42`, `FEAT-81`, `FEAT-82`, `EXT-F03`, `EXT-F04`, `EXT-F05` | Legacy .cdr/.cmx/.cpt Migration (Real Pack, 20B loda, CDRO ZCF, CP1256, PowerClip, UnknownChunks) |
| 98 | `T4-W4-PHOTO16-PSD` | `T4` | `W4` | `False` | `IMP-06`, `ADD-06`, `ADD-12`, `IMP-12` | `FEAT-64`, `FEAT-65`, `FEAT-69`, `FEAT-86` | High-Res 300+ MB / 4096x4096 16-bit Multi-Layer+Mask+Spot PSD Pipeline & RSS Gate |
| 99 | `T4-W4-PRINT-MERGE-10K` | `T4` | `W4` | `False` | `IMP-07`, `ADD-05`, `IMP-12` | `FEAT-78`, `FEAT-79`, `EXT-F05` | Variable-Data 10,000-Row Bilingual Arabic/Latin Streaming Print Merge |
| 100 | `T4-W4-SCENE10K-BENCH` | `T4` | `W4` | `False` | `IMP-01`, `IMP-02`, `IMP-07`, `IMP-12`, `ADD-11` | `FEAT-13`, `FEAT-14`, `EXT-F10`, `EXT-F11`, `EXT-F12` | 10,000+ Object Scene + Rhai DOM/CQL + >=60 FPS Wgpu/Tiles + <=2ms Autosave |
| 101 | `T4-W4-POTRACE-COONS-ICC` | `T4` | `W4` | `False` | `IMP-05`, `IMP-04`, `ADD-01`, `REQ-02` | `FEAT-33`, `FEAT-36`, `FEAT-43`, `FEAT-48`, `FEAT-70`, `FEAT-71` | Potrace Brand Logo + Coons MeshFill + InnerShadow + Envelope + ICC Proofing |
| 102 | `T4-W3-FUZZ-OFFLINE` | `T4` | `W3` | `False` | `MND-04` | `FEAT-81`, `FEAT-82`, `FEAT-83`, `FEAT-84`, `FEAT-86` | Deterministic Offline fuzz/ Targets & CLI Mutation Stress (0 Panics / 0 OOM) |
