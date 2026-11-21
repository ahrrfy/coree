// SPDX-License-Identifier: MPL-2.0

//! Embedded offline-first SQLite database (`rusqlite` bundled), WAL mode,
//! strict foreign keys, schema version tracking, and automatic execution of
//! the exact DDL from `replica/schema.sql`.

use crate::error::{StorageError, StorageResult};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Current local SQLite schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Exact offline-first SQLite DDL from `replica/schema.sql` (all 10 tables,
/// CHECK constraints, foreign keys, and indexes).
pub const SCHEMA_SQL: &str = r#"
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;

-- 1. Recent documents shown on Welcome Hub (S01) and File > Open Recent
CREATE TABLE IF NOT EXISTS recent_documents (
    id                  TEXT PRIMARY KEY NOT NULL,
    file_path           TEXT NOT NULL UNIQUE,
    display_name        TEXT NOT NULL,
    file_format         TEXT NOT NULL CHECK (file_format IN (
                            'cdr', 'cdt', 'cmx', 'cpt', 'svg', 'pdf', 'eps', 'dxf', 'plt', 'png', 'jpg', 'tiff', 'psd', 'coree'
                        )),
    cdr_version         TEXT,
    page_count          INTEGER NOT NULL DEFAULT 1 CHECK (page_count >= 1),
    color_mode          TEXT NOT NULL DEFAULT 'cmyk' CHECK (color_mode IN ('cmyk', 'rgb', 'grayscale')),
    thumbnail_png       BLOB,
    file_size_bytes     INTEGER NOT NULL DEFAULT 0 CHECK (file_size_bytes >= 0),
    pinned              INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    last_opened_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_recent_documents_opened
    ON recent_documents (pinned DESC, last_opened_at DESC);
CREATE INDEX IF NOT EXISTS idx_recent_documents_format
    ON recent_documents (file_format);

-- 2. Autosave snapshots for crash recovery (S01, S17)
CREATE TABLE IF NOT EXISTS autosave_snapshots (
    id                  TEXT PRIMARY KEY NOT NULL,
    document_id         TEXT NOT NULL,
    session_id          TEXT NOT NULL,
    original_file_path  TEXT,
    display_name        TEXT NOT NULL,
    snapshot_blob_path  TEXT NOT NULL UNIQUE,
    snapshot_sha256     TEXT NOT NULL,
    last_command_seq    INTEGER NOT NULL DEFAULT 0 CHECK (last_command_seq >= 0),
    byte_size           INTEGER NOT NULL DEFAULT 0 CHECK (byte_size >= 0),
    clean_shutdown      INTEGER NOT NULL DEFAULT 0 CHECK (clean_shutdown IN (0, 1)),
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_autosave_snapshots_doc
    ON autosave_snapshots (document_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_autosave_snapshots_recovery
    ON autosave_snapshots (clean_shutdown, updated_at DESC);

-- 3. Crash-safe serializable DocumentCommand journal for deterministic undo, crash replay & Rhai macro export (S17)
CREATE TABLE IF NOT EXISTS command_journal (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    document_id         TEXT NOT NULL,
    session_id          TEXT NOT NULL,
    snapshot_id         TEXT REFERENCES autosave_snapshots(id) ON DELETE SET NULL,
    seq_no              INTEGER NOT NULL CHECK (seq_no >= 1),
    command_type        TEXT NOT NULL,
    payload_json        TEXT NOT NULL,
    inverse_json        TEXT NOT NULL,
    rhai_snippet        TEXT,
    applied             INTEGER NOT NULL DEFAULT 1 CHECK (applied IN (0, 1)),
    committed_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE (document_id, session_id, seq_no)
);

CREATE INDEX IF NOT EXISTS idx_command_journal_doc_seq
    ON command_journal (document_id, session_id, seq_no ASC);
CREATE INDEX IF NOT EXISTS idx_command_journal_snapshot
    ON command_journal (snapshot_id);
CREATE INDEX IF NOT EXISTS idx_command_journal_type
    ON command_journal (command_type);

-- 4. Custom & built-in open color palettes (S02, S06, S14)
CREATE TABLE IF NOT EXISTS custom_palettes (
    id                  TEXT PRIMARY KEY NOT NULL,
    name                TEXT NOT NULL UNIQUE,
    description         TEXT NOT NULL DEFAULT '',
    palette_kind        TEXT NOT NULL CHECK (palette_kind IN ('cmyk', 'rgb', 'spot', 'cutcontour', 'document', 'custom')),
    is_builtin          INTEGER NOT NULL DEFAULT 0 CHECK (is_builtin IN (0, 1)),
    is_docked           INTEGER NOT NULL DEFAULT 0 CHECK (is_docked IN (0, 1)),
    show_color_names    INTEGER NOT NULL DEFAULT 0 CHECK (show_color_names IN (0, 1)),
    sort_order          INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_custom_palettes_docked
    ON custom_palettes (is_docked DESC, sort_order ASC);

-- 5. Individual color swatches belonging to palettes (supports CMYK, RGB, Lab, and Spot/CutContour)
CREATE TABLE IF NOT EXISTS palette_swatches (
    id                  TEXT PRIMARY KEY NOT NULL,
    palette_id          TEXT NOT NULL REFERENCES custom_palettes(id) ON DELETE CASCADE,
    name                TEXT NOT NULL,
    color_space         TEXT NOT NULL CHECK (color_space IN ('cmyk', 'rgb', 'lab', 'spot')),
    c                   REAL CHECK (c IS NULL OR (c >= 0.0 AND c <= 100.0)),
    m                   REAL CHECK (m IS NULL OR (m >= 0.0 AND m <= 100.0)),
    y                   REAL CHECK (y IS NULL OR (y >= 0.0 AND y <= 100.0)),
    k                   REAL CHECK (k IS NULL OR (k >= 0.0 AND k <= 100.0)),
    r                   INTEGER CHECK (r IS NULL OR (r >= 0 AND r <= 255)),
    g                   INTEGER CHECK (g IS NULL OR (g >= 0 AND g <= 255)),
    b                   INTEGER CHECK (b IS NULL OR (b >= 0 AND b <= 255)),
    lab_l               REAL CHECK (lab_l IS NULL OR (lab_l >= 0.0 AND lab_l <= 100.0)),
    lab_a               REAL CHECK (lab_a IS NULL OR (lab_a >= -128.0 AND lab_a <= 127.0)),
    lab_b               REAL CHECK (lab_b IS NULL OR (lab_b >= -128.0 AND lab_b <= 127.0)),
    spot_channel_name   TEXT,
    overprint_default   INTEGER NOT NULL DEFAULT 0 CHECK (overprint_default IN (0, 1)),
    sort_order          INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    CONSTRAINT chk_spot_name_required CHECK (
        color_space != 'spot' OR (spot_channel_name IS NOT NULL AND length(trim(spot_channel_name)) > 0)
    )
);

CREATE INDEX IF NOT EXISTS idx_palette_swatches_palette
    ON palette_swatches (palette_id, sort_order ASC);
CREATE INDEX IF NOT EXISTS idx_palette_swatches_spot
    ON palette_swatches (spot_channel_name) WHERE spot_channel_name IS NOT NULL;

-- 6. Local document & packaging templates (.cdt / FEFCO / print presets) (S01, S03, S14)
CREATE TABLE IF NOT EXISTS templates (
    id                  TEXT PRIMARY KEY NOT NULL,
    name                TEXT NOT NULL,
    category            TEXT NOT NULL CHECK (category IN (
                            'print', 'packaging', 'signage', 'laser-vinyl', 'web-ui', 'arabic-rtl', 'custom'
                        )),
    description         TEXT NOT NULL DEFAULT '',
    template_path       TEXT NOT NULL UNIQUE,
    width_mm            REAL NOT NULL CHECK (width_mm > 0.0),
    height_mm           REAL NOT NULL CHECK (height_mm > 0.0),
    bleed_mm            REAL NOT NULL DEFAULT 3.0 CHECK (bleed_mm >= 0.0),
    page_count          INTEGER NOT NULL DEFAULT 1 CHECK (page_count >= 1),
    color_mode          TEXT NOT NULL DEFAULT 'cmyk' CHECK (color_mode IN ('cmyk', 'rgb', 'grayscale')),
    thumbnail_png       BLOB,
    is_builtin          INTEGER NOT NULL DEFAULT 0 CHECK (is_builtin IN (0, 1)),
    favorite            INTEGER NOT NULL DEFAULT 0 CHECK (favorite IN (0, 1)),
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_templates_category
    ON templates (category, favorite DESC, name ASC);

-- 7. Docked workspace layouts & keyboard shortcut profiles (S02, S18)
CREATE TABLE IF NOT EXISTS workspace_layouts (
    id                  TEXT PRIMARY KEY NOT NULL,
    name                TEXT NOT NULL UNIQUE,
    description         TEXT NOT NULL DEFAULT '',
    dock_tree_json      TEXT NOT NULL,
    toolbox_config_json TEXT NOT NULL,
    shortcuts_json      TEXT NOT NULL DEFAULT '{}',
    is_builtin          INTEGER NOT NULL DEFAULT 0 CHECK (is_builtin IN (0, 1)),
    is_active           INTEGER NOT NULL DEFAULT 0 CHECK (is_active IN (0, 1)),
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_workspace_layouts_active
    ON workspace_layouts (is_active DESC, name ASC);

-- 8. Local system & user font index for instant Font Manager search & Arabic/RTL capability filtering (S07)
CREATE TABLE IF NOT EXISTS font_index (
    id                  TEXT PRIMARY KEY NOT NULL,
    file_path           TEXT NOT NULL,
    face_index          INTEGER NOT NULL DEFAULT 0 CHECK (face_index >= 0),
    family_name         TEXT NOT NULL,
    subfamily_name      TEXT NOT NULL DEFAULT 'Regular',
    postscript_name     TEXT,
    weight              INTEGER NOT NULL DEFAULT 400 CHECK (weight BETWEEN 100 AND 1000),
    italic              INTEGER NOT NULL DEFAULT 0 CHECK (italic IN (0, 1)),
    is_variable         INTEGER NOT NULL DEFAULT 0 CHECK (is_variable IN (0, 1)),
    supports_arabic_rtl INTEGER NOT NULL DEFAULT 0 CHECK (supports_arabic_rtl IN (0, 1)),
    supports_kashida    INTEGER NOT NULL DEFAULT 0 CHECK (supports_kashida IN (0, 1)),
    collection_tag      TEXT NOT NULL DEFAULT 'system',
    favorite            INTEGER NOT NULL DEFAULT 0 CHECK (favorite IN (0, 1)),
    file_mtime_epoch    INTEGER NOT NULL DEFAULT 0,
    indexed_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE (file_path, face_index)
);

CREATE INDEX IF NOT EXISTS idx_font_index_family
    ON font_index (family_name COLLATE NOCASE ASC, weight ASC);
CREATE INDEX IF NOT EXISTS idx_font_index_rtl
    ON font_index (supports_arabic_rtl DESC, favorite DESC);
CREATE INDEX IF NOT EXISTS idx_font_index_collection
    ON font_index (collection_tag);

-- 9. Vector Artistic Media & Photo Studio raster brush presets (S04, S10)
CREATE TABLE IF NOT EXISTS brush_presets (
    id                  TEXT PRIMARY KEY NOT NULL,
    name                TEXT NOT NULL UNIQUE,
    engine_kind         TEXT NOT NULL CHECK (engine_kind IN ('artistic_media', 'raster_paint', 'eraser', 'clone', 'smudge')),
    category            TEXT NOT NULL DEFAULT 'default',
    size_px             REAL NOT NULL CHECK (size_px > 0.0 AND size_px <= 4096.0),
    hardness            REAL NOT NULL DEFAULT 0.8 CHECK (hardness >= 0.0 AND hardness <= 1.0),
    spacing             REAL NOT NULL DEFAULT 0.15 CHECK (spacing > 0.0 AND spacing <= 5.0),
    opacity             REAL NOT NULL DEFAULT 1.0 CHECK (opacity >= 0.0 AND opacity <= 1.0),
    pressure_curve_json TEXT NOT NULL DEFAULT '[[0.0,0.0],[1.0,1.0]]',
    params_json         TEXT NOT NULL DEFAULT '{}',
    is_builtin          INTEGER NOT NULL DEFAULT 0 CHECK (is_builtin IN (0, 1)),
    sort_order          INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_brush_presets_engine
    ON brush_presets (engine_kind, category, sort_order ASC);

-- 10. Key-value local application preferences (ICC profiles, units, nudges, tile cache threads, UI scale) (S18)
CREATE TABLE IF NOT EXISTS app_preferences (
    key                 TEXT PRIMARY KEY NOT NULL,
    value_json          TEXT NOT NULL,
    category            TEXT NOT NULL CHECK (category IN (
                            'general', 'units', 'color_management', 'performance', 'autosave', 'export', 'accessibility', 'ui'
                        )),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_app_preferences_category
    ON app_preferences (category);
"#;

/// Local offline SQLite storage handle for `coree`.
pub struct StorageDb {
    pub(crate) conn: Connection,
    data_dir: PathBuf,
    _temp_guard: Option<TempDir>,
}

impl StorageDb {
    /// Opens (or creates) the default local SQLite database at
    /// `dirs::data_local_dir()/coree/coree.db`.
    pub fn open_default() -> StorageResult<Self> {
        let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
        let data_dir = base.join("coree");
        std::fs::create_dir_all(&data_dir)?;
        let db_path = data_dir.join("coree.db");
        Self::open_with_data_dir(&db_path, data_dir)
    }

    /// Opens (or creates) a file-backed SQLite database at `db_path`, storing autosave
    /// snapshot blobs alongside it under `<parent>/autosave/`.
    pub fn open(db_path: &Path) -> StorageResult<Self> {
        let data_dir = match db_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        std::fs::create_dir_all(&data_dir)?;
        Self::open_with_data_dir(db_path, data_dir)
    }

    /// Opens a file-backed SQLite database at `db_path` with an explicit local `data_dir`.
    pub fn open_with_data_dir(db_path: &Path, data_dir: PathBuf) -> StorageResult<Self> {
        if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(&data_dir)?;
        let conn = Connection::open(db_path)?;
        configure_and_migrate(&conn, true)?;
        Ok(Self {
            conn,
            data_dir,
            _temp_guard: None,
        })
    }

    /// Opens a fresh in-memory SQLite database with an isolated temporary directory for
    /// autosave snapshot blobs (ideal for unit & integration tests).
    pub fn open_in_memory() -> StorageResult<Self> {
        let tmp = tempfile::tempdir()?;
        let data_dir = tmp.path().to_path_buf();
        let conn = Connection::open_in_memory()?;
        configure_and_migrate(&conn, false)?;
        Ok(Self {
            conn,
            data_dir,
            _temp_guard: Some(tmp),
        })
    }

    /// Returns the currently applied schema version from `schema_migrations`.
    pub fn schema_version(&self) -> StorageResult<u32> {
        let ver: u32 = self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        Ok(ver)
    }

    /// Returns the active SQLite journal mode (e.g. `"wal"` for file-backed DBs).
    pub fn journal_mode(&self) -> StorageResult<String> {
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        Ok(mode)
    }

    /// Returns `true` if `PRAGMA foreign_keys` is currently enabled (`1`).
    pub fn foreign_keys_enabled(&self) -> StorageResult<bool> {
        let enabled: i64 = self
            .conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        Ok(enabled == 1)
    }

    /// Underlying `rusqlite::Connection` reference for advanced read queries.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Directory where local application data and autosave snapshot blobs are stored.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Directory reserved for crash-recovery autosave blobs (`<data_dir>/autosave`).
    pub fn autosave_dir(&self) -> StorageResult<PathBuf> {
        let dir = self.data_dir.join("autosave");
        if !dir.exists() {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(dir)
    }
}

fn configure_and_migrate(conn: &Connection, enable_wal: bool) -> StorageResult<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    if enable_wal {
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StorageError::RecoveryFailed(format!(
                "expected SQLite WAL mode, got '{mode}'"
            )));
        }
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY NOT NULL,
             applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
         );",
    )?;

    let current: u32 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;

    if current < SCHEMA_VERSION {
        conn.execute_batch(SCHEMA_SQL)?;
        conn.execute(
            "INSERT OR REPLACE INTO schema_migrations (version) VALUES (?1)",
            [SCHEMA_VERSION],
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_create_all_ten_tables_and_wal_mode_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("coree.db");
        let db = StorageDb::open(&db_path).unwrap();

        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(db.journal_mode().unwrap().to_ascii_lowercase(), "wal");
        assert!(db.foreign_keys_enabled().unwrap());

        let expected_tables = [
            "recent_documents",
            "autosave_snapshots",
            "command_journal",
            "custom_palettes",
            "palette_swatches",
            "templates",
            "workspace_layouts",
            "font_index",
            "brush_presets",
            "app_preferences",
        ];
        for table in expected_tables {
            let exists: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "missing SQLite table '{table}'");
        }

        // Re-opening existing DB must be idempotent
        drop(db);
        let db2 = StorageDb::open(&db_path).unwrap();
        assert_eq!(db2.schema_version().unwrap(), SCHEMA_VERSION);
    }
}
