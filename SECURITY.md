# Security Policy

## Architectural Security Guarantees

`coree` is built as an offline-first, zero-telemetry desktop graphics, laser
production, and print-prepress suite (dual-licensed `MIT OR Apache-2.0`)
designed to safely handle untrusted legacy and third-party design documents
(`.cdr`, `.cdr.zip`, `.cdt`, `.cmx`, `.cpt`, `.pdf`, `.ai`, `.eps`, `.svg`,
`.svgz`, `.dxf`, `.plt`, `.psd`, `.psb`, `.icc`, `.zip`-packaged archives,
raster images, laser plugin manifests, and automation scripts).

### Memory Safety (`#![forbid(unsafe_code)]`)

All 7 workspace crates enforce `#![forbid(unsafe_code)]`:

- `crates/coree-core`
- `crates/coree-cdr`
- `crates/coree-io`
- `crates/coree-storage`
- `crates/coree-render`
- `crates/coree-prod`
- `crates/coree-app` (including `build.rs`)

Unsafe code is never permitted in parsers, decoders, layout engines, laser
planners, or rendering pipelines.

### Parser, Archive & Plugin Hardening

Because design files from print shops, signage studios, and client archives may
be malformed or maliciously crafted, all decoders in `coree-cdr`, `coree-io`,
`coree-core`, and `coree-prod` enforce strict resource bounds:

- **Zip-Bomb & Zlib-Bomb protection:** Bounded decompression limits (maximum
  uncompressed entry size, cumulative archive expansion caps, and incremental
  compression-ratio guardrails) on ZIP-based `.cdr`/`.cdt`/`.cpt`/`.coree`
  containers, compressed RIFF `LIST cmpr` sub-streams, `.svgz`, and PDF
  `/FlateDecode` / `/ASCII85Decode` / `/RunLengthDecode` filter chains.
- **Zip-Slip & Path Traversal protection:** Archive entry paths and custom
  plugin manifests (`plugin.json` / `plugin.toml`) are strictly validated;
  path traversal components (`..`), absolute paths, backslashes in relative
  paths, and symlink escapes are rejected before any filesystem access.
- **ZCF Indirect Pointer & Codec Amplification guards:** Strictly monotonic
  `loda` offset validation, cumulative ZCF dereference budgets, PDF indirect
  `/Length` cycle guards, PostScript stack-node/repeat caps, DXF block-DAG
  expansion limits, streaming bounded PSD/PSB layer RGBA downscaling, and
  bounded ICC v2/v4 `mluc`/`desc`/CLUT parsing prevent CPU hangs and OOM bombs.
- **Unknown-chunk preservation without execution:** Unrecognized binary chunks
  in `.cdr`/`.cdt`/`.cmx` files are preserved opaquely for round-trip fidelity
  and never interpreted as code.
- **Safe Laser & Plotter Plugin Execution:** External bridge plugins
  (`LightBurn`, `RDWorks`, `EZCad`, `LaserDRW`, `LaserGRBL`, `InkCut/VisiCut`,
  `HPGL Plotter`) invoke executables via direct `std::process::Command::args`
  with single-pass placeholder substitution (`{file}`, `{dir}`, `{stem}`) —
  never via `sh -c` or `cmd /c` (eliminating shell command injection). G-Code
  comments strip control characters (`\r`, `\n`).
- **Sandboxed scripting:** Automation macros run inside a bounded Rhai engine
  with operation caps (`max_operations`) and no arbitrary filesystem or network
  escape outside explicitly exposed document APIs.
- **Atomic local persistence:** Saves and autosave snapshots write to temporary
  files and atomically rename into place, preventing document corruption.
- **Zero network / zero telemetry & Flatpak isolation:** `coree` operates 100%
  locally with zero outbound network connections; the official Flatpak manifest
  omits `--share=network` to enforce OS-level offline sandboxing.

## Supported Versions

Security fixes are applied to the `main` branch and the latest tagged release.

## Reporting a Vulnerability

If you discover a security vulnerability (such as a crash, denial-of-service /
resource exhaustion on a malformed file, path traversal, or sandbox escape):

1. **Do not** open a public GitHub issue containing an unpatched exploit or
   weaponized sample file.
2. Report the issue privately via GitHub Security Advisories on the repository
   (`https://github.com/ahrrfy/coree/security/advisories/new`).
3. Include:
   - Affected crate(s) and commit hash or version
   - Minimal reproduction file or steps
   - Expected vs. actual behavior and crash/backtraces if applicable

We aim to acknowledge vulnerability reports within 72 hours and coordinate a
fix and disclosure timeline with the reporter.
