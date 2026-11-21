# Contributing to `coree`

Thank you for your interest in contributing to `coree`!

## Clean-Room Contribution Policy (Mandatory)

`coree` is a strict **clean-room** open-source project. By submitting a pull
request, issue attachment, or patch, you certify that your contribution:

1. **Contains ZERO proprietary code or decompiled material:** Do not submit
   code copied from, derived from, or decompiled/disassembled from CorelDRAW or
   any other proprietary graphics software.
2. **Contains ZERO proprietary assets:** Do not submit proprietary icons,
   splash screens, UI bitmaps, fonts, templates, clipart, brush textures, or
   stock files.
3. **Contains ZERO proprietary color books:** Do not submit Pantone®, Toyo,
   DIC, RAL, or other copyrighted/licensed commercial spot-color tables.
   Built-in palettes must use open standards, mathematical color spaces, or
   freely licensed/original definitions.
4. **Respects trademarks:** Refer to third-party software and formats only for
   factual interoperability notes (see [NOTICE](NOTICE)).

## License (MPL-2.0)

All contributions to `coree` are licensed under the **Mozilla Public License
Version 2.0 (MPL-2.0)**. See [LICENSE](LICENSE) for full terms. New source
files should include the SPDX header where appropriate:

```rust
// SPDX-License-Identifier: MPL-2.0
```

## Architecture & Coding Standards

- All library crates (`coree-core`, `coree-cdr`, `coree-io`, `coree-storage`,
  `coree-render`, `coree-prod`) enforce `#![forbid(unsafe_code)]`.
- Keep `coree` **offline-first** with zero network calls and zero telemetry.
- Ensure Linux builds work with pure-Rust / dynamically loaded runtime
  dependencies (no required `-dev` C header packages like `libgtk-3-dev` or
  `libx11-dev`).
- Add unit tests for geometry, parsers, RTL/Arabic shaping, rendering, and
  prepress workflows, including fuzz/malformed-input tests for codecs.

## Development Workflow & Pre-PR Checks

Before opening a pull request, run the full workspace checks locally:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All CI checks (Linux and Windows) must pass with zero warnings.
