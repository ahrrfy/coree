# Security Policy

## Architectural Security Guarantees

`coree` is built as an offline-first, zero-telemetry desktop graphics and print
production suite designed to safely handle untrusted legacy and third-party
design documents (`.cdr`, `.cdt`, `.cmx`, `.cpt`, `.svg`, `.dxf`, `.plt`,
`.zip`-packaged archives, raster images, and macro scripts).

### Memory Safety (`#![forbid(unsafe_code)]`)

All core engine, file-format codec, I/O, storage, rendering, and print
production crates enforce `#![forbid(unsafe_code)]`:

- `crates/coree-core`
- `crates/coree-cdr`
- `crates/coree-io`
- `crates/coree-storage`
- `crates/coree-render`
- `crates/coree-prod`

Unsafe code is never permitted in parsers, decoders, layout engines, or
rendering pipelines.

### Parser & Archive Hardening

Because design files from print shops and archives may be malformed or
maliciously crafted, all decoders in `coree-cdr` and `coree-io` enforce strict
resource bounds:

- **Zip-Bomb protection:** Bounded decompression limits (maximum uncompressed
  entry size, total archive expansion cap, and compression-ratio guardrails) on
  ZIP-based `.cdr`/`.cdt`/`.cpt` containers and compressed RIFF sub-streams.
- **Zip-Slip protection:** Archive entry paths are strictly validated; path
  traversal components (`..`), absolute paths, and symlink/device entries are
  rejected before any access.
- **Parser recursion & allocation bounds:** Explicit limits on RIFF chunk
  nesting depth, SVG/DOM tree depth, Bezier path segment counts, bitmap
  dimensions, and memory allocations prior to allocation (`try_reserve` /
  explicit caps) to prevent stack overflows and OOM crashes.
- **Unknown-chunk preservation without execution:** Unrecognized binary chunks
  are preserved opaquely for round-trip fidelity and never interpreted as code.
- **Sandboxed scripting:** Automation macros run inside a bounded Rhai engine
  with operation limits and no arbitrary filesystem/network escape outside
  explicitly exposed document APIs.
- **Atomic local persistence:** Saves and autosave snapshots write to temporary
  files and atomically rename into place, preventing document corruption.
- **Zero network / zero telemetry:** `coree` operates 100% locally and makes no
  outbound network connections.

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
