# Tier 4 Fixtures (`e2e_suite/fixtures/tier4/`)

Compact deterministic offline seed assets (`< 30 KiB` total) for `e2e_suite/tier4_stress_perf.py`:

- `logo_seed.png`: Deterministic `128x128` RGBA geometric brand emblem with adjacent color blocks (for `Potrace` topological trace + Coons `MeshFill` + `InnerShadow` + ICC soft-proofing in `T4-W4-POTRACE-COONS-ICC`).
- `scene_dom_recolor.rhai`: Rhai Hierarchical Scene DOM macro (`doc.pages[0].layers[0].shapes`) for batch inspection and recoloring of the `10,000`-object scene in `T4-W4-SCENE10K-BENCH`.

Large stress payloads (`64`-page facing-pages booklet, `10,000`-row bilingual CSV, `10,000`-object scene, `4096x4096` 16-bit multi-layer `> 300 MiB` PSD) are generated deterministically via streaming writes inside each test's isolated scratch directory (`ctx.make_scratch_dir(...)`) at runtime to keep repository size minimal and Python memory well below `RLIMIT_AS=512 MiB`.
