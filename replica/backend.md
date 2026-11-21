# Local Offline Backend & Security Specification — `coree`

`coree` is **100% offline-first**. Unlike SaaS clones, `coree` has **zero remote servers, zero user accounts, zero subscription/Stripe billing, zero email/push processors, zero telemetry, and zero third-party OAuth keys**. This document specifies `coree`'s local persistence backend (`coree-storage`), atomic filesystem I/O (`coree-io`), sandboxed scripting (`coree-prod`), and the mandatory security hardening checklist.

---

## 1. Local Backend Architecture (Zero Cloud / Zero Auth)

| `replica-backend` area | `coree` local equivalent | implementation |
| --- | --- | --- |
| Auth & Accounts | **Explicitly absent (by design)** | Zero login screens, zero license activation checks, zero network sockets. App opens offline immediately on first launch. |
| Database & Migrations | Local SQLite 3 (`rusqlite` `bundled`, WAL mode) | Stored at `$XDG_DATA_HOME/coree/coree.db` (Linux) or `%APPDATA%\coree\coree.db` (Windows) with `0600` file permissions. Schema defined in [schema.sql](file:///run/media/ahmed/DATA/coreldraw%20me/replica/schema.sql), applied via idempotent versioned SQL migrations in `coree-storage::migrations`. |
| Payments & Billing | **Explicitly absent (MPL-2.0 Free & Open Source)** | Zero paywalls, zero trial expirations, zero billing logic. |
| Background Jobs | Local worker pools & timers | `autosave-flusher` (30 s idle / 25 cmds), `font-indexer` (incremental OS font scan), `tile-render-pool` (Rayon 256×256 tile rasterizer), `trace-worker` (PowerTRACE async job). |
| File Storage & Saves | Local filesystem via `rfd` (`xdg-portal`) + atomic saves | Every save writes to a `NamedTempFile` in the destination directory, calls `file.sync_all()`, and atomically renames (`persist`) over the target file — eliminating mid-crash `.cdr` corruption. |
| Third-Party APIs / Secrets | **None** | No `.env` secrets, no API keys, no outbound HTTP crates (`reqwest`/`ureq`/`hyper` are forbidden in `Cargo.toml`). |

---

## 2. SQLite WAL Persistence & Crash Recovery Protocol

1. **Initialization (`coree-storage`):**
   - Enable `PRAGMA foreign_keys = ON;`, `PRAGMA journal_mode = WAL;`, `PRAGMA synchronous = NORMAL;`, and `PRAGMA busy_timeout = 5000;`.
   - Run embedded schema migrations in a single transaction, recording `PRAGMA user_version`.
   - Seed open built-in palettes (`CMYK Default`, `RGB Studio`, `CutContour & Packaging Spot Presets`) and default workspace layouts (`Studio Default`, `Prepress & Packaging`, `Photo Studio`, `Arabic RTL Layout`) idempotently on first run.
2. **Command Journaling:**
   - Every user mutation emits a `DocumentCommand` serialized via `serde_json` and appended to `command_journal` with monotonically increasing `(document_id, session_id, seq_no)`.
   - On clean document close, `autosave_snapshots.clean_shutdown` is updated to `1` and stale journal rows older than the retention window (default 7 days) are pruned via `VACUUM` / `PRAGMA wal_checkpoint(TRUNCATE)`.
3. **Crash Recovery Replay (`F18`):**
   - On startup (`S01`), `coree-storage` queries `SELECT * FROM autosave_snapshots WHERE clean_shutdown = 0 ORDER BY updated_at DESC`.
   - If any exist, `S01` displays a non-blocking **Crash Recovery Banner** allowing one-click restore: loads `snapshot_blob_path` (verifying `snapshot_sha256` first), then replays all `command_journal` rows where `seq_no > last_command_seq` in exact order. Any incomplete trailing transaction is safely rolled back by SQLite WAL.

---

## 3. Binary & File Parser Hardening Limits (`coree-cdr`, `coree-io`)

Because commercial print shops routinely open untrusted client `.cdr`, `.cmx`, `.svg`, `.pdf`, `.dxf`, and image files, every parser enforces strict compile-time and runtime resource ceilings:

| threat vector | enforced hard limit | crate / enforcement point |
| --- | --- | --- |
| Memory safety / UB / buffer overflows | `#![forbid(unsafe_code)]` in all 7 crates | Root + every `crates/*/src/lib.rs` & `main.rs` |
| Zip-Bomb (`.cdr` X4–2025+ ZIP containers) | Max uncompressed total `512 MiB`, max single entry `128 MiB`, max entry count `4,096`, max compression ratio `100:1` (enforced via streaming bounded `Read::take` before allocation) | `coree-cdr::zip_container` |
| Zip-Slip path traversal | Reject any ZIP member containing `..`, leading `/`, `\`, drive letters (`C:`), or NUL bytes; never extract ZIP entries directly to disk — parse strictly in bounded memory | `coree-cdr::zip_container` |
| Malformed RIFF / integer overflow (`.cdr` v3–v13) | Checked arithmetic (`checked_add`, `checked_mul`) on all chunk sizes/offsets; chunk length verified `<= remaining_bytes` before slicing | `coree-cdr::riff` |
| Deep nesting / stack overflow (XML, RIFF lists, Groups, PowerClips, Lenses) | Hard maximum tree nesting depth of **`64`**; cyclic PowerClip/Lens/Symbol references rejected with `CodecError::NestingDepthExceeded` | `coree-cdr`, `coree-core`, `coree-io` |
| Decompression / raster pixel bomb | Max single decoded bitmap `16,384 × 16,384` px (`<= 1 GiB` RGBA8 cap, configurable down in Preferences) before allocating pixel buffer | `coree-io::raster`, `coree-render` |
| SVG XXE / external resource SSRF | External `<image href="http://...">`, `file://` URIs, and XML external entities are strictly disabled in `usvg::Options` (zero network or arbitrary filesystem reads) | `coree-io::svg` |
| Malicious Rhai macros (`S17`) | Sandboxed `rhai::Engine` with `set_max_operations(500_000)`, `set_max_call_levels(32)`, `set_max_string_size(1_048_576)`, `set_max_array_size(65_536)`, and **no filesystem, process, or network modules registered** | `coree-prod::macro_engine` |
| Mid-save file corruption | Write to `NamedTempFile` in target parent directory -> `flush()` -> `sync_all()` -> atomic `persist(target_path)` | `coree-io::atomic_write` |

---

## 4. Security Hardening Checklist

- [x] **`#![forbid(unsafe_code)]` enforced** as a workspace-wide requirement across all 7 crates (`coree-core`, `coree-cdr`, `coree-io`, `coree-storage`, `coree-render`, `coree-prod`, `coree-app`).
- [x] **Zero network / zero telemetry architecture** — no HTTP/TLS/analytics crates in workspace dependencies; app operates identically in an air-gapped network namespace (`unshare -n`).
- [x] **Zero hardcoded secrets or cloud credentials** — no `.env` secrets, no embedded OAuth client IDs, no license phone-home endpoints.
- [x] **Zip-Bomb bounds specified & enforced** — streaming `512 MiB` total / `128 MiB` per-entry / `100:1` ratio / `4,096` entries ceiling before decompression allocation.
- [x] **Zip-Slip traversal blocked** — in-memory bounded parsing only; reject `..`, absolute paths, Windows drive prefixes, and NUL bytes.
- [x] **Tree-depth & cycle caps enforced** — max nesting depth `64` across RIFF lists, XML/SVG nodes, nested Groups, PowerClip containers, and recursive Lenses.
- [x] **Integer overflow & bitmap allocation guards** — `checked_*` arithmetic on all binary header offsets/lengths and pixel dimensions (`<= 16,384 × 16,384`).
- [x] **SVG/PDF external entity & SSRF disabled** — no external URI resolution (`http://`, `https://`, `file://`) during document import.
- [x] **Rhai macro sandbox hardened** — operation count, call depth, and allocation limits enforced with zero ambient OS/filesystem/network capabilities.
- [x] **Atomic file saves & SQLite WAL crash recovery** — temp-file + `fsync` + atomic rename prevents truncating user `.cdr` files on crash or disk-full errors.
- [x] **Runtime implementation & CI verification** — verified across all 7 crates: **323/323** workspace tests passing (including adversarial Zip-Bomb, `LIST cmpr` Zlib-Bomb, Zip-Slip, forged `compressed_size`, PNG-in-ZIP RAM amplification, SVGZ bomb, truncated/deep-nested RIFF, malformed `RasterAsset`, Rhai infinite-loop abort, and SQLite WAL crash-recovery tests), `cargo clippy --workspace --all-targets -- -D warnings` (0 warnings), `cargo fmt --all -- --check` clean, and CI workflow configured in `.github/workflows/ci.yml` (`ubuntu-latest` & `windows-latest` both green).
- [x] **Local DB permissions check** — `StorageDb::open_with_data_dir`, `autosave_dir`, and `atomic_write_blob` enforce owner-only Unix permissions (`0700` on `<data_dir>` / `autosave/` and `0600` on `coree.db` and temporary/persisted snapshot blobs before atomic `persist`), verified by `unix_storage_directories_and_db_have_owner_only_permissions` in `coree-storage`.
