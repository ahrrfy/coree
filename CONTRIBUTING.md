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

## License (`MIT OR Apache-2.0`)

`coree` is dual-licensed under the **MIT License** ([LICENSE-MIT](LICENSE-MIT))
and the **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE)), at
your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in `coree` by you, as defined in Section 5 of the Apache-2.0
license, shall be dual-licensed as `MIT OR Apache-2.0`, without any additional
terms or conditions.

New source files should include the SPDX header where appropriate:

```rust
// SPDX-License-Identifier: MIT OR Apache-2.0
```

## Architecture & Coding Standards

- All 7 workspace crates (`coree-core`, `coree-cdr`, `coree-io`, `coree-storage`,
  `coree-render`, `coree-prod`, and `coree-app`) enforce `#![forbid(unsafe_code)]`.
- Keep `coree` **100% offline-first** with zero network calls and zero telemetry.
- Ensure Linux builds work with pure-Rust / dynamically loaded runtime
  dependencies (no required `-dev` C header packages like `libgtk-3-dev` or
  `libx11-dev`).
- Add unit and adversarial regression tests for geometry, binary codecs (`.cdr`,
  `.pdf`, `.eps`, `.dxf`, `.plt`, `.psd`, `.icc`), RTL/Arabic shaping, rendering,
  laser/engraving toolpaths, and prepress workflows.
- External laser/plotter plugins must execute target binaries via direct argv
  substitution (`std::process::Command::args`) — never via shell wrappers
  (`sh -c` / `cmd /c`).

## Development Workflow & Pre-PR Checks

Before opening a pull request, run the full workspace checks locally:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo test --workspace --doc
```

All CI checks (Linux and Windows) must pass with zero warnings.
