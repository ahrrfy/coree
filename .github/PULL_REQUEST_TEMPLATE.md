## Summary

<!-- Describe what this pull request changes and why. -->

## Verification Checklist

- [ ] Clean-room compliance: 100% original code & assets (no proprietary binaries, logos, fonts, or copyrighted color books)
- [ ] Licensed under `MPL-2.0`
- [ ] `#![forbid(unsafe_code)]` preserved across all crates
- [ ] `cargo test --workspace --all-targets` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes with 0 warnings
- [ ] `cargo fmt --all -- --check` passes
- [ ] Security boundaries preserved (bounded reads, Zip-Bomb/Zip-Slip/depth guards, checked arithmetic)
