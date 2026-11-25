// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Offline-first local SQLite persistence, crash-safe atomic autosave snapshots,
//! deterministic command journal replay, custom palettes, templates, workspace
//! layouts, brush presets, app preferences, local SFNT font scanner, and
//! non-blocking rayon background job pool.

pub mod assets;
pub mod autosave;
pub mod db;
pub mod error;
pub mod fonts;
pub mod jobs;

/// Crate identifier used during workspace initialization and diagnostics.
pub const CRATE_NAME: &str = "coree-storage";

// Ergonomic top-level re-exports
pub use assets::{
    AppPreferenceRecord, BrushPresetRecord, CustomPaletteRecord, PaletteSwatchRecord,
    RecentDocumentRecord, TemplateRecord, WorkspaceLayoutRecord,
};
pub use autosave::{
    sha256_hex, AutosaveSnapshotInfo, DifferentialSnapshotPatch, JournalEntry, UncleanSession,
    DEFAULT_SESSION_ID, MAX_SNAPSHOT_BYTES, WAL_COMPACTION_INTERVAL,
};
pub use db::{quarantine_corrupt_database, StorageDb, SCHEMA_SQL, SCHEMA_VERSION};
pub use error::{StorageError, StorageResult};
pub use fonts::{
    inspect_font_bytes, panose_distance, standard_font_directories, FontQuery, FontRecord,
    FontScanReport, FontSubstitutionRecord, FontVariationAxis, PanoseClassification,
    ParsedFontFace,
};
pub use jobs::{JobContext, JobHandle, JobId, JobOutcome, JobPool, JobState};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_metadata_is_valid() {
        assert_eq!(CRATE_NAME, "coree-storage");
    }

    #[test]
    fn bundled_sqlite_opens_in_memory() {
        let db = StorageDb::open_in_memory().expect("in-memory storage db");
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    }
}
