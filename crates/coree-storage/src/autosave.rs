// SPDX-License-Identifier: MIT OR Apache-2.0

//! Crash-safe autosave snapshots and deterministic command journal replay.
//!
//! Guarantees zero lost clicks: periodic atomic document snapshots (`autosave_snapshots`)
//! are paired with every subsequent [`CommandOp`] written to `command_journal`. On crash
//! recovery, [`StorageDb::recover_document`] verifies the snapshot SHA-256 checksum,
//! loads the snapshot, and deterministically replays all subsequent journal commands.

use crate::db::StorageDb;
use crate::error::{StorageError, StorageResult};
use coree_core::{
    apply_command, ColorMode, ColorPalette, ColorStyle, CommandHistory, CommandOp, Document,
    DocumentMetadata, GridConfig, Guide, ImageAssetId, ObjectId, ObjectStyle, Page,
    PerspectiveGridConfig, RasterAsset, SceneObject, SymbolDef, SymbolId, Unit,
};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tempfile::NamedTempFile;
use uuid::Uuid;

/// Default session identifier used when commands are journaled before an initial snapshot.
pub const DEFAULT_SESSION_ID: &str = "default";

/// Maximum snapshot blob size read during crash recovery (512 MiB).
pub const MAX_SNAPSHOT_BYTES: u64 = 512 * 1024 * 1024;

/// Number of differential WAL delta snapshots before the background worker compacts
/// the chain into a fresh baseline snapshot (well below `MAX_DELTA_CHAIN = 512`).
pub const WAL_COMPACTION_INTERVAL: usize = 128;

/// Metadata row stored in `autosave_snapshots`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutosaveSnapshotInfo {
    pub id: String,
    pub document_id: String,
    pub session_id: String,
    pub original_file_path: Option<String>,
    pub display_name: String,
    pub snapshot_blob_path: String,
    pub snapshot_sha256: String,
    pub last_command_seq: u64,
    pub byte_size: u64,
    pub clean_shutdown: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Summary of an uncleanly terminated editing session eligible for crash recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncleanSession {
    pub document_id: String,
    pub session_id: String,
    pub display_name: String,
    pub original_file_path: Option<String>,
    pub latest_snapshot_id: String,
    pub last_command_seq: u64,
    pub pending_journal_ops: usize,
    pub updated_at: String,
}

/// Row stored in `command_journal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub id: i64,
    pub document_id: String,
    pub session_id: String,
    pub snapshot_id: Option<String>,
    pub seq_no: u64,
    pub command_type: String,
    pub payload_json: String,
    pub inverse_json: String,
    pub rhai_snippet: Option<String>,
    pub applied: bool,
    pub committed_at: String,
}

/// Incremental differential autosave payload containing only changed objects and optional
/// structural fields (`pages`, `assets`, styles, symbols) relative to `base_snapshot_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DifferentialSnapshotPatch {
    pub doc_id: String,
    pub session_id: String,
    pub title: String,
    pub source_path: Option<String>,
    pub base_snapshot_id: String,
    pub last_command_seq: u64,
    pub upserted_objects: Vec<(ObjectId, SceneObject)>,
    pub deleted_object_ids: Vec<ObjectId>,
    pub upserted_assets: Vec<(ImageAssetId, RasterAsset)>,
    pub deleted_asset_ids: Vec<ImageAssetId>,
    pub full_assets: Option<BTreeMap<ImageAssetId, RasterAsset>>,
    pub pages: Option<Vec<Page>>,
    pub active_page_index: Option<usize>,
    pub next_id: Option<u64>,
    pub color_mode: Option<ColorMode>,
    pub default_unit: Option<Unit>,
    pub dpi: Option<f32>,
    pub multipage_view: Option<bool>,
    pub palettes: Option<Vec<ColorPalette>>,
    pub document_palette: Option<ColorPalette>,
    pub guides: Option<Vec<Guide>>,
    pub grid: Option<GridConfig>,
    pub perspective_grid: Option<PerspectiveGridConfig>,
    pub color_styles: Option<Vec<ColorStyle>>,
    pub object_styles: Option<Vec<ObjectStyle>>,
    pub symbols: Option<BTreeMap<SymbolId, SymbolDef>>,
    pub metadata: Option<DocumentMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DifferentialSnapshotEnvelope {
    autosave_format: String,
    patch: DifferentialSnapshotPatch,
}

enum AutosaveTask {
    BaselineSnapshot {
        snapshot_id: String,
        doc_id: String,
        session_id: String,
        title: String,
        source_path: Option<String>,
        doc: Box<Document>,
        last_command_seq: u64,
    },
    DifferentialDelta {
        delta_id: String,
        patch: Box<DifferentialSnapshotPatch>,
    },
    CompactBaseline {
        new_snapshot_id: String,
        doc_id: String,
        session_id: String,
        title: String,
        source_path: Option<String>,
        latest_delta_id: String,
        last_command_seq: u64,
    },
    AppendCommand {
        doc_id: String,
        session_id: String,
        snapshot_id: Option<String>,
        seq: u64,
        op: Box<CommandOp>,
        inverse_json: String,
        rhai_snippet: Option<String>,
    },
    Flush(SyncSender<()>),
    Shutdown(SyncSender<()>),
}

enum CompletedMemoryRow {
    Snapshot {
        id: String,
        doc_id: String,
        session_id: String,
        orig_path: Option<String>,
        title: String,
        blob_path: String,
        sha256: String,
        last_command_seq: i64,
        byte_size: i64,
    },
    Command {
        doc_id: String,
        session_id: String,
        snapshot_id: Option<String>,
        seq: i64,
        command_type: String,
        payload_json: String,
        inverse_json: String,
        rhai_snippet: Option<String>,
    },
}

/// Dedicated background FIFO thread handling JSON serialization, gzip compression,
/// SHA-256 hashing, atomic blob writes, and SQLite WAL inserts off the UI thread.
pub(crate) struct AsyncAutosaveHandle {
    tx: Option<SyncSender<AutosaveTask>>,
    pending_memory_rows: Arc<Mutex<Vec<CompletedMemoryRow>>>,
    thread: Option<JoinHandle<()>>,
}

impl AsyncAutosaveHandle {
    pub(crate) fn spawn(db_path: Option<PathBuf>, autosave_dir: PathBuf) -> StorageResult<Self> {
        let (tx, rx) = mpsc::sync_channel::<AutosaveTask>(512);
        let pending_memory_rows = Arc::new(Mutex::new(Vec::new()));
        let mem_clone = Arc::clone(&pending_memory_rows);

        let handle = thread::Builder::new()
            .name("coree-autosave-worker".to_owned())
            .spawn(move || {
                let worker_conn = db_path.as_ref().and_then(|p| {
                    let conn = Connection::open(p).ok()?;
                    let _ = conn.pragma_update(None, "foreign_keys", "ON");
                    let _ = conn.pragma_update(None, "busy_timeout", 5000);
                    let _ = conn.pragma_update(None, "journal_mode", "WAL");
                    let _ = conn.pragma_update(None, "synchronous", "NORMAL");
                    Some(conn)
                });

                while let Ok(task) = rx.recv() {
                    match task {
                        AutosaveTask::BaselineSnapshot {
                            snapshot_id,
                            doc_id,
                            session_id,
                            title,
                            source_path,
                            doc,
                            last_command_seq,
                        } => {
                            if let Ok((blob_path, sha256, byte_size)) =
                                write_baseline_blob(&autosave_dir, &snapshot_id, &doc)
                            {
                                let blob_str = blob_path.to_string_lossy().into_owned();
                                if let Some(ref conn) = worker_conn {
                                    if insert_snapshot_row(
                                        conn,
                                        &snapshot_id,
                                        &doc_id,
                                        &session_id,
                                        source_path.as_deref(),
                                        &title,
                                        &blob_str,
                                        &sha256,
                                        last_command_seq as i64,
                                        byte_size as i64,
                                    )
                                    .is_err()
                                    {
                                        let _ = std::fs::remove_file(&blob_path);
                                    }
                                } else if let Ok(mut guard) = mem_clone.lock() {
                                    guard.push(CompletedMemoryRow::Snapshot {
                                        id: snapshot_id,
                                        doc_id,
                                        session_id,
                                        orig_path: source_path,
                                        title,
                                        blob_path: blob_str,
                                        sha256,
                                        last_command_seq: last_command_seq as i64,
                                        byte_size: byte_size as i64,
                                    });
                                }
                            }
                        }
                        AutosaveTask::DifferentialDelta { delta_id, patch } => {
                            let envelope = DifferentialSnapshotEnvelope {
                                autosave_format: "differential_v1".to_owned(),
                                patch: *patch,
                            };
                            if let Ok((blob_path, sha256, byte_size)) =
                                write_differential_blob(&autosave_dir, &delta_id, &envelope)
                            {
                                let blob_str = blob_path.to_string_lossy().into_owned();
                                let p = &envelope.patch;
                                if let Some(ref conn) = worker_conn {
                                    if insert_snapshot_row(
                                        conn,
                                        &delta_id,
                                        &p.doc_id,
                                        &p.session_id,
                                        p.source_path.as_deref(),
                                        &p.title,
                                        &blob_str,
                                        &sha256,
                                        p.last_command_seq as i64,
                                        byte_size as i64,
                                    )
                                    .is_err()
                                    {
                                        let _ = std::fs::remove_file(&blob_path);
                                    }
                                } else if let Ok(mut guard) = mem_clone.lock() {
                                    guard.push(CompletedMemoryRow::Snapshot {
                                        id: delta_id,
                                        doc_id: p.doc_id.clone(),
                                        session_id: p.session_id.clone(),
                                        orig_path: p.source_path.clone(),
                                        title: p.title.clone(),
                                        blob_path: blob_str,
                                        sha256,
                                        last_command_seq: p.last_command_seq as i64,
                                        byte_size: byte_size as i64,
                                    });
                                }
                            }
                        }
                        AutosaveTask::CompactBaseline {
                            new_snapshot_id,
                            doc_id,
                            session_id,
                            title,
                            source_path,
                            latest_delta_id,
                            last_command_seq,
                        } => {
                            if let Ok(mut doc) =
                                materialize_chain_from_dir(&autosave_dir, &latest_delta_id)
                            {
                                let mut replay_ok = true;
                                if let Some(ref conn) = worker_conn {
                                    let base_seq: i64 = conn
                                        .query_row(
                                            "SELECT last_command_seq FROM autosave_snapshots WHERE id = ?1",
                                            params![latest_delta_id],
                                            |row| row.get(0),
                                        )
                                        .optional()
                                        .ok()
                                        .flatten()
                                        .unwrap_or(0);
                                    let target_seq = last_command_seq.min(i64::MAX as u64) as i64;
                                    if target_seq > base_seq {
                                        if let Ok(mut stmt) = conn.prepare(
                                            "SELECT payload_json FROM command_journal
                                             WHERE document_id = ?1 AND session_id = ?2
                                               AND seq_no > ?3 AND seq_no <= ?4 AND applied = 1
                                             ORDER BY seq_no ASC",
                                        ) {
                                            if let Ok(rows) = stmt.query_map(
                                                params![doc_id, session_id, base_seq, target_seq],
                                                |row| row.get::<_, String>(0),
                                            ) {
                                                for row in rows {
                                                    let Ok(payload_json) = row else {
                                                        replay_ok = false;
                                                        break;
                                                    };
                                                    let Ok(op) = serde_json::from_str::<CommandOp>(
                                                        &payload_json,
                                                    ) else {
                                                        replay_ok = false;
                                                        break;
                                                    };
                                                    if apply_command(&mut doc, &op).is_err() {
                                                        replay_ok = false;
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                if replay_ok {
                                    if let Ok((blob_path, sha256, byte_size)) =
                                        write_baseline_blob(&autosave_dir, &new_snapshot_id, &doc)
                                    {
                                        let blob_str = blob_path.to_string_lossy().into_owned();
                                        if let Some(ref conn) = worker_conn {
                                            if insert_snapshot_row(
                                                conn,
                                                &new_snapshot_id,
                                                &doc_id,
                                                &session_id,
                                                source_path.as_deref(),
                                                &title,
                                                &blob_str,
                                                &sha256,
                                                last_command_seq as i64,
                                                byte_size as i64,
                                            )
                                            .is_err()
                                            {
                                                let _ = std::fs::remove_file(&blob_path);
                                            }
                                        } else if let Ok(mut guard) = mem_clone.lock() {
                                            guard.push(CompletedMemoryRow::Snapshot {
                                                id: new_snapshot_id,
                                                doc_id,
                                                session_id,
                                                orig_path: source_path,
                                                title,
                                                blob_path: blob_str,
                                                sha256,
                                                last_command_seq: last_command_seq as i64,
                                                byte_size: byte_size as i64,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        AutosaveTask::AppendCommand {
                            doc_id,
                            session_id,
                            snapshot_id,
                            seq,
                            op,
                            inverse_json,
                            rhai_snippet,
                        } => {
                            let command_type = command_op_type_name(&op).to_owned();
                            if let Ok(payload_json) = serde_json::to_string(&*op) {
                                if let Some(ref conn) = worker_conn {
                                    let _ = insert_command_row(
                                        conn,
                                        &doc_id,
                                        &session_id,
                                        snapshot_id.as_deref(),
                                        seq as i64,
                                        &command_type,
                                        &payload_json,
                                        &inverse_json,
                                        rhai_snippet.as_deref(),
                                    );
                                } else if let Ok(mut guard) = mem_clone.lock() {
                                    guard.push(CompletedMemoryRow::Command {
                                        doc_id,
                                        session_id,
                                        snapshot_id,
                                        seq: seq as i64,
                                        command_type,
                                        payload_json,
                                        inverse_json,
                                        rhai_snippet,
                                    });
                                }
                            }
                        }
                        AutosaveTask::Flush(ack) => {
                            let _ = ack.send(());
                        }
                        AutosaveTask::Shutdown(ack) => {
                            let _ = ack.send(());
                            break;
                        }
                    }
                }
            })
            .map_err(|e| StorageError::ThreadPool(format!("autosave worker spawn: {e}")))?;

        Ok(Self {
            tx: Some(tx),
            pending_memory_rows,
            thread: Some(handle),
        })
    }

    pub(crate) fn shutdown(&mut self) {
        if let Some(tx) = self.tx.take() {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            if tx.send(AutosaveTask::Shutdown(ack_tx)).is_ok() {
                let _ = ack_rx.recv_timeout(Duration::from_secs(5));
            }
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn read_gz_json_from_path(path: &Path) -> StorageResult<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut blob = Vec::new();
    file.take(MAX_SNAPSHOT_BYTES.saturating_add(1))
        .read_to_end(&mut blob)?;
    if (blob.len() as u64) > MAX_SNAPSHOT_BYTES {
        return Err(StorageError::RecoveryFailed(
            "autosave blob exceeds MAX_SNAPSHOT_BYTES".into(),
        ));
    }
    if blob.starts_with(&[0x1F, 0x8B]) {
        let mut decoded = Vec::new();
        GzDecoder::new(&blob[..])
            .take(MAX_SNAPSHOT_BYTES.saturating_add(1))
            .read_to_end(&mut decoded)?;
        Ok(decoded)
    } else {
        Ok(blob)
    }
}

fn materialize_chain_from_dir(autosave_dir: &Path, latest_id: &str) -> StorageResult<Document> {
    const MAX_DELTA_CHAIN: usize = 512;
    let mut patches: Vec<DifferentialSnapshotPatch> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut cur_id = latest_id.to_owned();
    let mut doc = loop {
        if !visited.insert(cur_id.clone()) || patches.len() >= MAX_DELTA_CHAIN {
            return Err(StorageError::RecoveryFailed(
                "compaction chain cycle or depth exceeded".into(),
            ));
        }
        let delta_path = autosave_dir.join(format!("{cur_id}.delta.json.gz"));
        if delta_path.is_file() {
            let raw = read_gz_json_from_path(&delta_path)?;
            let env: DifferentialSnapshotEnvelope = serde_json::from_slice(&raw)?;
            cur_id = env.patch.base_snapshot_id.clone();
            patches.push(env.patch);
            continue;
        }
        let snap_path = autosave_dir.join(format!("{cur_id}.snapshot.json.gz"));
        let raw = read_gz_json_from_path(&snap_path)?;
        break serde_json::from_slice::<Document>(&raw)?;
    };
    for patch in patches.into_iter().rev() {
        apply_differential_patch(&mut doc, &patch)?;
    }
    Ok(doc)
}

fn write_baseline_blob(
    autosave_dir: &Path,
    snapshot_id: &str,
    doc: &Document,
) -> StorageResult<(PathBuf, String, u64)> {
    let blob_path = autosave_dir.join(format!("{snapshot_id}.snapshot.json.gz"));
    let raw_json = serde_json::to_vec(doc)?;
    write_compressed_blob(&blob_path, &raw_json)
}

fn write_differential_blob(
    autosave_dir: &Path,
    delta_id: &str,
    envelope: &DifferentialSnapshotEnvelope,
) -> StorageResult<(PathBuf, String, u64)> {
    let blob_path = autosave_dir.join(format!("{delta_id}.delta.json.gz"));
    let raw_json = serde_json::to_vec(envelope)?;
    write_compressed_blob(&blob_path, &raw_json)
}

fn write_compressed_blob(
    blob_path: &Path,
    raw_json: &[u8],
) -> StorageResult<(PathBuf, String, u64)> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
    gz.write_all(raw_json)?;
    let payload = gz.finish()?;
    let byte_size = payload.len() as u64;
    if byte_size > MAX_SNAPSHOT_BYTES || byte_size > i64::MAX as u64 {
        return Err(StorageError::InvalidArgument(format!(
            "snapshot size {byte_size}B exceeds limit {MAX_SNAPSHOT_BYTES}B"
        )));
    }
    let sha256 = sha256_hex(&payload);
    atomic_write_blob(blob_path, &payload)?;
    Ok((blob_path.to_path_buf(), sha256, byte_size))
}

#[allow(clippy::too_many_arguments)]
fn insert_snapshot_row(
    conn: &Connection,
    snapshot_id: &str,
    doc_id: &str,
    session_id: &str,
    orig_path_str: Option<&str>,
    title: &str,
    blob_path_str: &str,
    sha256: &str,
    last_command_seq: i64,
    byte_size: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO autosave_snapshots (
             id, document_id, session_id, original_file_path, display_name,
             snapshot_blob_path, snapshot_sha256, last_command_seq, byte_size, clean_shutdown
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
        params![
            snapshot_id,
            doc_id,
            session_id,
            orig_path_str,
            title,
            blob_path_str,
            sha256,
            last_command_seq,
            byte_size,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_command_row(
    conn: &Connection,
    doc_id: &str,
    session_id: &str,
    snapshot_id: Option<&str>,
    seq: i64,
    command_type: &str,
    payload_json: &str,
    inverse_json: &str,
    rhai_snippet: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO command_journal (
             document_id, session_id, snapshot_id, seq_no,
             command_type, payload_json, inverse_json, rhai_snippet, applied
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)",
        params![
            doc_id,
            session_id,
            snapshot_id,
            seq,
            command_type,
            payload_json,
            inverse_json,
            rhai_snippet,
        ],
    )?;
    Ok(())
}

impl StorageDb {
    /// Flushes all pending background autosave snapshots, differential deltas, and async
    /// command journal rows to disk/SQLite within `timeout`.
    pub fn flush_autosave(&self, timeout: Duration) -> StorageResult<()> {
        let Some(ref worker) = self.autosave_worker else {
            return Ok(());
        };
        if let Some(ref tx) = worker.tx {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            if tx.send(AutosaveTask::Flush(ack_tx)).is_ok() {
                let _ = ack_rx.recv_timeout(timeout);
            }
        }
        let rows: Vec<CompletedMemoryRow> = {
            let mut guard = worker.pending_memory_rows.lock().map_err(|_| {
                StorageError::RecoveryFailed("autosave memory lock poisoned".into())
            })?;
            std::mem::take(&mut *guard)
        };
        for row in rows {
            match row {
                CompletedMemoryRow::Snapshot {
                    id,
                    doc_id,
                    session_id,
                    orig_path,
                    title,
                    blob_path,
                    sha256,
                    last_command_seq,
                    byte_size,
                } => {
                    insert_snapshot_row(
                        &self.conn,
                        &id,
                        &doc_id,
                        &session_id,
                        orig_path.as_deref(),
                        &title,
                        &blob_path,
                        &sha256,
                        last_command_seq,
                        byte_size,
                    )?;
                }
                CompletedMemoryRow::Command {
                    doc_id,
                    session_id,
                    snapshot_id,
                    seq,
                    command_type,
                    payload_json,
                    inverse_json,
                    rhai_snippet,
                } => {
                    insert_command_row(
                        &self.conn,
                        &doc_id,
                        &session_id,
                        snapshot_id.as_deref(),
                        seq,
                        &command_type,
                        &payload_json,
                        &inverse_json,
                        rhai_snippet.as_deref(),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Enqueues a baseline full snapshot onto the background autosave worker thread (`<= 2 ms`
    /// non-blocking enqueue when channel capacity is available), returning the allocated snapshot ID.
    pub fn enqueue_baseline_snapshot(
        &self,
        doc_id: &str,
        session_id: &str,
        title: &str,
        source_path: Option<&Path>,
        doc: Document,
        last_command_seq: u64,
    ) -> StorageResult<String> {
        if doc_id.trim().is_empty() || session_id.trim().is_empty() {
            return Err(StorageError::InvalidArgument(
                "doc_id and session_id must be non-empty".into(),
            ));
        }
        if last_command_seq > i64::MAX as u64 {
            return Err(StorageError::InvalidArgument(
                "last_command_seq exceeds i64::MAX".into(),
            ));
        }
        let snapshot_id = Uuid::new_v4().to_string();
        let orig_path = source_path.map(|p| p.to_string_lossy().into_owned());
        if let Some(tx) = self.autosave_worker.as_ref().and_then(|w| w.tx.as_ref()) {
            let task = AutosaveTask::BaselineSnapshot {
                snapshot_id: snapshot_id.clone(),
                doc_id: doc_id.to_owned(),
                session_id: session_id.to_owned(),
                title: title.to_owned(),
                source_path: orig_path,
                doc: Box::new(doc),
                last_command_seq,
            };
            tx.send(task)
                .map_err(|e| StorageError::ThreadPool(format!("autosave queue closed: {e}")))?;
            Ok(snapshot_id)
        } else {
            let info = self.save_snapshot_for_session(
                doc_id,
                session_id,
                title,
                source_path,
                &doc,
                last_command_seq,
            )?;
            Ok(info.id)
        }
    }

    /// Enqueues a lightweight differential autosave delta (`O(changed_objects)`, `<= 2 ms` UI
    /// thread enqueue) onto the background worker thread and returns the allocated delta ID.
    pub fn enqueue_differential_autosave(
        &self,
        patch: DifferentialSnapshotPatch,
    ) -> StorageResult<String> {
        if patch.doc_id.trim().is_empty()
            || patch.session_id.trim().is_empty()
            || patch.base_snapshot_id.trim().is_empty()
        {
            return Err(StorageError::InvalidArgument(
                "differential patch requires non-empty doc_id, session_id, and base_snapshot_id"
                    .into(),
            ));
        }
        if patch.last_command_seq > i64::MAX as u64 {
            return Err(StorageError::InvalidArgument(
                "last_command_seq exceeds i64::MAX".into(),
            ));
        }
        let delta_id = Uuid::new_v4().to_string();
        if let Some(tx) = self.autosave_worker.as_ref().and_then(|w| w.tx.as_ref()) {
            tx.send(AutosaveTask::DifferentialDelta {
                delta_id: delta_id.clone(),
                patch: Box::new(patch),
            })
            .map_err(|e| StorageError::ThreadPool(format!("autosave queue closed: {e}")))?;
            Ok(delta_id)
        } else {
            let autosave_dir = self.autosave_dir()?;
            let differential = DifferentialSnapshotEnvelope {
                autosave_format: "differential_v1".to_owned(),
                patch,
            };
            let (blob_path, sha256, byte_size) =
                write_differential_blob(&autosave_dir, &delta_id, &differential)?;
            let p = &differential.patch;
            insert_snapshot_row(
                &self.conn,
                &delta_id,
                &p.doc_id,
                &p.session_id,
                p.source_path.as_deref(),
                &p.title,
                &blob_path.to_string_lossy(),
                &sha256,
                p.last_command_seq as i64,
                byte_size as i64,
            )?;
            Ok(delta_id)
        }
    }

    /// Enqueues a zero-UI-clone background WAL baseline compaction materializing the delta
    /// chain ending at `latest_delta_id` into a fresh baseline snapshot, returning its ID.
    pub fn enqueue_compact_baseline(
        &self,
        doc_id: &str,
        session_id: &str,
        title: &str,
        source_path: Option<&Path>,
        latest_delta_id: &str,
        last_command_seq: u64,
    ) -> StorageResult<String> {
        if doc_id.trim().is_empty()
            || session_id.trim().is_empty()
            || latest_delta_id.trim().is_empty()
        {
            return Err(StorageError::InvalidArgument(
                "compact baseline requires non-empty doc_id, session_id, and latest_delta_id"
                    .into(),
            ));
        }
        let new_snapshot_id = Uuid::new_v4().to_string();
        let orig_path = source_path.map(|p| p.to_string_lossy().into_owned());
        if let Some(tx) = self.autosave_worker.as_ref().and_then(|w| w.tx.as_ref()) {
            tx.send(AutosaveTask::CompactBaseline {
                new_snapshot_id: new_snapshot_id.clone(),
                doc_id: doc_id.to_owned(),
                session_id: session_id.to_owned(),
                title: title.to_owned(),
                source_path: orig_path,
                latest_delta_id: latest_delta_id.to_owned(),
                last_command_seq,
            })
            .map_err(|e| StorageError::ThreadPool(format!("autosave queue closed: {e}")))?;
            Ok(new_snapshot_id)
        } else {
            let autosave_dir = self.autosave_dir()?;
            let doc = materialize_chain_from_dir(&autosave_dir, latest_delta_id)?;
            let (blob_path, sha256, byte_size) =
                write_baseline_blob(&autosave_dir, &new_snapshot_id, &doc)?;
            insert_snapshot_row(
                &self.conn,
                &new_snapshot_id,
                doc_id,
                session_id,
                orig_path.as_deref(),
                title,
                &blob_path.to_string_lossy(),
                &sha256,
                last_command_seq as i64,
                byte_size as i64,
            )?;
            Ok(new_snapshot_id)
        }
    }

    /// Enqueues a [`CommandOp`] journal append onto the background autosave worker thread so
    /// UI-thread command execution never stalls on SQLite WAL writes.
    pub fn enqueue_command_async(
        &self,
        doc_id: &str,
        session_id: &str,
        snapshot_id: Option<&str>,
        seq: u64,
        op: CommandOp,
    ) -> StorageResult<()> {
        if seq == 0 || seq > i64::MAX as u64 {
            return Err(StorageError::InvalidArgument(
                "command_journal seq_no must be in 1..=i64::MAX".into(),
            ));
        }
        if let Some(tx) = self.autosave_worker.as_ref().and_then(|w| w.tx.as_ref()) {
            tx.send(AutosaveTask::AppendCommand {
                doc_id: doc_id.to_owned(),
                session_id: session_id.to_owned(),
                snapshot_id: snapshot_id.map(str::to_owned),
                seq,
                op: Box::new(op),
                inverse_json: "{}".to_owned(),
                rhai_snippet: None,
            })
            .map_err(|e| StorageError::ThreadPool(format!("autosave queue closed: {e}")))?;
            Ok(())
        } else {
            self.append_command_for_session(doc_id, session_id, snapshot_id, seq, &op, "{}", None)?;
            Ok(())
        }
    }

    /// Atomically saves a full JSON snapshot of `doc` to disk with SHA-256 verification
    /// and records it in `autosave_snapshots`.
    ///
    /// Continues the active unclean editing session if one exists for `doc_id`; otherwise
    /// allocates a fresh session ID (preventing `UNIQUE` sequence collisions when reopening a
    /// document after [`Self::mark_clean_close`]) and captures any unlinked pre-snapshot
    /// journal sequence number.
    pub fn save_snapshot(
        &self,
        doc_id: &str,
        title: &str,
        source_path: Option<&Path>,
        doc: &Document,
    ) -> StorageResult<AutosaveSnapshotInfo> {
        self.flush_autosave(Duration::from_secs(5))?;
        let (session_id, _) = self.resolve_or_open_session(doc_id)?;
        let current_seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq_no), 0) FROM command_journal
             WHERE document_id = ?1 AND session_id = ?2 AND applied = 1",
            params![doc_id, session_id],
            |row| row.get(0),
        )?;
        self.save_snapshot_for_session(
            doc_id,
            &session_id,
            title,
            source_path,
            doc,
            current_seq.max(0) as u64,
        )
    }

    /// Atomically saves a snapshot for an explicit `session_id` and `last_command_seq`.
    pub fn save_snapshot_for_session(
        &self,
        doc_id: &str,
        session_id: &str,
        title: &str,
        source_path: Option<&Path>,
        doc: &Document,
        last_command_seq: u64,
    ) -> StorageResult<AutosaveSnapshotInfo> {
        if doc_id.trim().is_empty() || session_id.trim().is_empty() {
            return Err(StorageError::InvalidArgument(
                "doc_id and session_id must be non-empty".into(),
            ));
        }
        if last_command_seq > i64::MAX as u64 {
            return Err(StorageError::InvalidArgument(
                "last_command_seq exceeds i64::MAX".into(),
            ));
        }

        let snapshot_id = Uuid::new_v4().to_string();
        let autosave_dir = self.autosave_dir()?;
        let blob_path = autosave_dir.join(format!("{snapshot_id}.snapshot.json.gz"));

        let raw_json = serde_json::to_vec(doc)?;
        let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
        gz.write_all(&raw_json)?;
        let payload = gz.finish()?;
        let byte_size = payload.len() as u64;
        if byte_size > MAX_SNAPSHOT_BYTES || byte_size > i64::MAX as u64 {
            return Err(StorageError::InvalidArgument(format!(
                "snapshot size {byte_size}B exceeds limit {MAX_SNAPSHOT_BYTES}B"
            )));
        }
        let sha256 = sha256_hex(&payload);

        atomic_write_blob(&blob_path, &payload)?;

        let orig_path_str = source_path.map(|p| p.to_string_lossy().into_owned());
        let blob_path_str = blob_path.to_string_lossy().into_owned();

        let insert_res = self.conn.execute(
            "INSERT INTO autosave_snapshots (
                 id, document_id, session_id, original_file_path, display_name,
                 snapshot_blob_path, snapshot_sha256, last_command_seq, byte_size, clean_shutdown
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
            params![
                snapshot_id,
                doc_id,
                session_id,
                orig_path_str,
                title,
                blob_path_str,
                sha256,
                last_command_seq as i64,
                byte_size as i64,
            ],
        );
        if let Err(err) = insert_res {
            let _ = std::fs::remove_file(&blob_path);
            return Err(StorageError::from(err));
        }

        self.get_snapshot_by_id(&snapshot_id)?
            .ok_or_else(|| StorageError::NotFound(format!("snapshot {snapshot_id}")))
    }

    /// Appends a deterministic [`CommandOp`] to `command_journal` (`seq >= 1`) under the
    /// active unclean session for `doc_id` (allocating a fresh session if none is open yet,
    /// so reopening a document after [`Self::mark_clean_close`] never collides on sequence numbers).
    pub fn append_command(&self, doc_id: &str, seq: u64, op: &CommandOp) -> StorageResult<i64> {
        self.flush_autosave(Duration::from_secs(5))?;
        let (session_id, latest_snap_id) = self.resolve_or_open_session(doc_id)?;
        self.append_command_for_session(
            doc_id,
            &session_id,
            latest_snap_id.as_deref(),
            seq,
            op,
            "{}",
            None,
        )
    }

    /// Drains any unreferenced asset IDs pruned by the latest [`CommandHistory::gc_after_commit`]
    /// and appends a deterministic [`CommandOp::PruneAssets`] entry to `command_journal`
    /// (incrementing `seq` when pruned IDs are present), returning the number of pruned IDs journaled.
    pub fn sync_pruned_assets(
        &self,
        doc_id: &str,
        seq: &mut u64,
        history: &mut CommandHistory,
    ) -> StorageResult<usize> {
        let pruned = history.take_last_gc_pruned();
        let count = pruned.len();
        if count > 0 {
            *seq = seq.saturating_add(1);
            self.append_command(doc_id, *seq, &CommandOp::PruneAssets { ids: pruned })?;
        }
        Ok(count)
    }

    /// Executes `op` through `history` on `doc`, journals `op` (`seq += 1`), and automatically
    /// journals any `max_history` eviction asset pruning (`CommandOp::PruneAssets`, `seq += 1`)
    /// so `live_doc.assets()`, `CommandHistory::replay_log()`, and `recover_document`/`recover_session`
    /// remain identical across `max_history` evictions.
    pub fn execute_and_journal(
        &self,
        doc_id: &str,
        seq: &mut u64,
        history: &mut CommandHistory,
        doc: &mut Document,
        op: CommandOp,
    ) -> StorageResult<()> {
        history.execute(doc, op.clone())?;
        let pruned = history.take_last_gc_pruned();
        *seq = seq.saturating_add(1);
        self.append_command(doc_id, *seq, &op)?;
        if !pruned.is_empty() {
            *seq = seq.saturating_add(1);
            self.append_command(doc_id, *seq, &CommandOp::PruneAssets { ids: pruned })?;
        }
        Ok(())
    }

    /// Resolves an active unclean session (plus latest snapshot ID) or allocates a fresh one.
    pub fn resolve_or_open_session(&self, doc_id: &str) -> StorageResult<(String, Option<String>)> {
        self.flush_autosave(Duration::from_secs(5))?;
        if let Some((sess, snap_id)) = self.active_unclean_session(doc_id)? {
            return Ok((sess, Some(snap_id)));
        }
        if let Some(pending_sess) = self.pending_unattached_session(doc_id)? {
            return Ok((pending_sess, None));
        }
        Ok((format!("session-{}", Uuid::new_v4()), None))
    }

    fn pending_unattached_session(&self, doc_id: &str) -> StorageResult<Option<String>> {
        self.conn
            .query_row(
                "SELECT j.session_id FROM command_journal j
                 WHERE j.document_id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM autosave_snapshots s
                       WHERE s.document_id = j.document_id
                         AND s.session_id = j.session_id
                   )
                 ORDER BY j.id DESC LIMIT 1",
                params![doc_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    fn active_unclean_session(&self, doc_id: &str) -> StorageResult<Option<(String, String)>> {
        self.conn
            .query_row(
                "SELECT session_id, id FROM autosave_snapshots
                 WHERE document_id = ?1 AND clean_shutdown = 0
                 ORDER BY rowid DESC LIMIT 1",
                params![doc_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Appends a [`CommandOp`] for an explicit session, optional snapshot link, inverse JSON, and Rhai snippet.
    #[allow(clippy::too_many_arguments)]
    pub fn append_command_for_session(
        &self,
        doc_id: &str,
        session_id: &str,
        snapshot_id: Option<&str>,
        seq: u64,
        op: &CommandOp,
        inverse_json: &str,
        rhai_snippet: Option<&str>,
    ) -> StorageResult<i64> {
        if seq == 0 || seq > i64::MAX as u64 {
            return Err(StorageError::InvalidArgument(
                "command_journal seq_no must be in 1..=i64::MAX".into(),
            ));
        }
        let command_type = command_op_type_name(op);
        let payload_json = serde_json::to_string(op)?;
        self.conn.execute(
            "INSERT INTO command_journal (
                 document_id, session_id, snapshot_id, seq_no,
                 command_type, payload_json, inverse_json, rhai_snippet, applied
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)",
            params![
                doc_id,
                session_id,
                snapshot_id,
                seq as i64,
                command_type,
                payload_json,
                inverse_json,
                rhai_snippet,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Marks all autosave snapshots for `doc_id` as cleanly closed (`clean_shutdown = 1`).
    pub fn mark_clean_close(&self, doc_id: &str) -> StorageResult<usize> {
        self.flush_autosave(Duration::from_secs(5))?;
        let updated = self.conn.execute(
            "UPDATE autosave_snapshots
             SET clean_shutdown = 1,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE document_id = ?1 AND clean_shutdown = 0",
            params![doc_id],
        )?;
        Ok(updated)
    }

    /// Marks all autosave snapshots for a specific `(doc_id, session_id)` as cleanly closed.
    pub fn mark_session_clean_close(&self, doc_id: &str, session_id: &str) -> StorageResult<usize> {
        self.flush_autosave(Duration::from_secs(5))?;
        let updated = self.conn.execute(
            "UPDATE autosave_snapshots
             SET clean_shutdown = 1,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE document_id = ?1 AND session_id = ?2 AND clean_shutdown = 0",
            params![doc_id, session_id],
        )?;
        Ok(updated)
    }

    /// Lists all documents/sessions that have unclean snapshots (`clean_shutdown = 0`),
    /// most recently updated first, along with the count of journaled ops after the latest snapshot.
    pub fn list_unclean_sessions(&self) -> StorageResult<Vec<UncleanSession>> {
        self.flush_autosave(Duration::from_secs(5))?;
        let mut stmt = self.conn.prepare(
            "SELECT s.document_id, s.session_id, s.display_name, s.original_file_path,
                    s.id, s.last_command_seq, s.updated_at,
                    (SELECT COUNT(*) FROM command_journal j
                     WHERE j.document_id = s.document_id
                       AND j.session_id = s.session_id
                       AND j.applied = 1
                       AND j.seq_no > s.last_command_seq) AS pending_ops
             FROM autosave_snapshots s
             WHERE s.clean_shutdown = 0
               AND s.rowid = (
                   SELECT MAX(s2.rowid) FROM autosave_snapshots s2
                   WHERE s2.document_id = s.document_id
                     AND s2.session_id = s.session_id
                     AND s2.clean_shutdown = 0
               )
             ORDER BY s.updated_at DESC, s.rowid DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(UncleanSession {
                document_id: row.get(0)?,
                session_id: row.get(1)?,
                display_name: row.get(2)?,
                original_file_path: row.get(3)?,
                latest_snapshot_id: row.get(4)?,
                last_command_seq: row.get::<_, i64>(5)? as u64,
                updated_at: row.get(6)?,
                pending_journal_ops: row.get::<_, i64>(7)? as usize,
            })
        })?;

        let mut out = Vec::new();
        for item in rows {
            out.push(item?);
        }
        Ok(out)
    }

    /// Lists journal entries for `doc_id` with `seq_no > after_seq` in ascending order.
    pub fn list_journal_entries(
        &self,
        doc_id: &str,
        session_id: Option<&str>,
        after_seq: u64,
    ) -> StorageResult<Vec<JournalEntry>> {
        self.flush_autosave(Duration::from_secs(5))?;
        if after_seq > i64::MAX as u64 {
            return Err(StorageError::InvalidArgument(
                "after_seq exceeds i64::MAX".into(),
            ));
        }
        let resolved_session: String = match session_id {
            Some(s) => s.to_owned(),
            None => {
                if let Some((s, _)) = self.active_unclean_session(doc_id)? {
                    s
                } else if let Some(s) = self.pending_unattached_session(doc_id)? {
                    s
                } else {
                    self.conn
                        .query_row(
                            "SELECT session_id FROM autosave_snapshots
                             WHERE document_id = ?1
                             ORDER BY rowid DESC LIMIT 1",
                            params![doc_id],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()?
                        .unwrap_or_else(|| DEFAULT_SESSION_ID.to_owned())
                }
            }
        };
        let mut stmt = self.conn.prepare(
            "SELECT id, document_id, session_id, snapshot_id, seq_no, command_type,
                    payload_json, inverse_json, rhai_snippet, applied, committed_at
             FROM command_journal
             WHERE document_id = ?1 AND session_id = ?2 AND seq_no > ?3
             ORDER BY seq_no ASC",
        )?;

        let rows = stmt.query_map(params![doc_id, resolved_session, after_seq as i64], |row| {
            Ok(JournalEntry {
                id: row.get(0)?,
                document_id: row.get(1)?,
                session_id: row.get(2)?,
                snapshot_id: row.get(3)?,
                seq_no: row.get::<_, i64>(4)? as u64,
                command_type: row.get(5)?,
                payload_json: row.get(6)?,
                inverse_json: row.get(7)?,
                rhai_snippet: row.get(8)?,
                applied: row.get::<_, i64>(9)? != 0,
                committed_at: row.get(10)?,
            })
        })?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Recovers `doc_id` by loading its latest autosave snapshot (verifying SHA-256 integrity)
    /// and deterministically replaying every subsequent applied [`CommandOp`] from `command_journal`.
    pub fn recover_document(&self, doc_id: &str) -> StorageResult<Document> {
        self.flush_autosave(Duration::from_secs(5))?;
        // Prefer latest unclean snapshot; fall back to latest snapshot overall.
        let snapshot: AutosaveSnapshotInfo = self
            .latest_snapshot_for_doc(doc_id, true)?
            .or(self.latest_snapshot_for_doc(doc_id, false)?)
            .ok_or_else(|| StorageError::NotFound(format!("autosave snapshot for {doc_id}")))?;
        self.recover_from_snapshot(&snapshot)
    }

    /// Recovers a specific `(doc_id, session_id)` by loading its latest snapshot (verifying
    /// SHA-256 integrity) and replaying every subsequent applied command in that session.
    pub fn recover_session(&self, doc_id: &str, session_id: &str) -> StorageResult<Document> {
        self.flush_autosave(Duration::from_secs(5))?;
        let snapshot: AutosaveSnapshotInfo = self
            .conn
            .query_row(
                "SELECT id, document_id, session_id, original_file_path, display_name,
                        snapshot_blob_path, snapshot_sha256, last_command_seq, byte_size,
                        clean_shutdown, created_at, updated_at
                 FROM autosave_snapshots
                 WHERE document_id = ?1 AND session_id = ?2
                 ORDER BY rowid DESC LIMIT 1",
                params![doc_id, session_id],
                row_to_snapshot,
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::NotFound(format!("autosave snapshot for {doc_id}/{session_id}"))
            })?;
        self.recover_from_snapshot(&snapshot)
    }

    fn recover_from_snapshot(&self, snapshot: &AutosaveSnapshotInfo) -> StorageResult<Document> {
        const MAX_DELTA_CHAIN: usize = 512;
        let mut patches: Vec<DifferentialSnapshotPatch> = Vec::new();
        let mut visited_ids: HashSet<String> = HashSet::new();
        let mut cursor = snapshot.clone();

        let mut doc = loop {
            if !visited_ids.insert(cursor.id.clone()) || patches.len() >= MAX_DELTA_CHAIN {
                return Err(StorageError::RecoveryFailed(format!(
                    "differential snapshot chain cycle or depth limit exceeded at {}",
                    cursor.id
                )));
            }
            let json_bytes = read_and_verify_snapshot_blob(&cursor)?;
            if let Ok(differential) =
                serde_json::from_slice::<DifferentialSnapshotEnvelope>(&json_bytes)
            {
                if differential.autosave_format == "differential_v1" {
                    let base_id = differential.patch.base_snapshot_id.clone();
                    patches.push(differential.patch);
                    cursor = self.get_snapshot_by_id(&base_id)?.ok_or_else(|| {
                        StorageError::NotFound(format!("base autosave snapshot {base_id}"))
                    })?;
                    continue;
                }
            }
            break serde_json::from_slice::<Document>(&json_bytes)?;
        };

        for patch in patches.into_iter().rev() {
            apply_differential_patch(&mut doc, &patch)?;
        }

        let pending = self.list_journal_entries(
            &snapshot.document_id,
            Some(&snapshot.session_id),
            snapshot.last_command_seq,
        )?;

        for entry in pending {
            if !entry.applied {
                continue;
            }
            let op: CommandOp = serde_json::from_str(&entry.payload_json)?;
            apply_command(&mut doc, &op).map_err(|err| {
                StorageError::RecoveryFailed(format!(
                    "failed replaying seq_no {} ({}): {err}",
                    entry.seq_no, entry.command_type
                ))
            })?;
        }

        Ok(doc)
    }

    /// Deletes all autosave snapshots (both SQLite metadata and blob files) and journal rows for `doc_id`.
    pub fn purge_document_autosaves(&self, doc_id: &str) -> StorageResult<()> {
        self.flush_autosave(Duration::from_secs(5))?;
        let mut stmt = self
            .conn
            .prepare("SELECT snapshot_blob_path FROM autosave_snapshots WHERE document_id = ?1")?;
        let paths: Vec<String> = stmt
            .query_map(params![doc_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        self.conn.execute(
            "DELETE FROM command_journal WHERE document_id = ?1",
            params![doc_id],
        )?;
        self.conn.execute(
            "DELETE FROM autosave_snapshots WHERE document_id = ?1",
            params![doc_id],
        )?;

        for p in paths {
            let _ = std::fs::remove_file(p);
        }
        Ok(())
    }

    fn get_snapshot_by_id(&self, id: &str) -> StorageResult<Option<AutosaveSnapshotInfo>> {
        self.conn
            .query_row(
                "SELECT id, document_id, session_id, original_file_path, display_name,
                        snapshot_blob_path, snapshot_sha256, last_command_seq, byte_size,
                        clean_shutdown, created_at, updated_at
                 FROM autosave_snapshots WHERE id = ?1",
                params![id],
                row_to_snapshot,
            )
            .optional()
            .map_err(StorageError::from)
    }

    fn latest_snapshot_for_doc(
        &self,
        doc_id: &str,
        unclean_only: bool,
    ) -> StorageResult<Option<AutosaveSnapshotInfo>> {
        let sql = if unclean_only {
            "SELECT id, document_id, session_id, original_file_path, display_name,
                    snapshot_blob_path, snapshot_sha256, last_command_seq, byte_size,
                    clean_shutdown, created_at, updated_at
             FROM autosave_snapshots
             WHERE document_id = ?1 AND clean_shutdown = 0
             ORDER BY rowid DESC LIMIT 1"
        } else {
            "SELECT id, document_id, session_id, original_file_path, display_name,
                    snapshot_blob_path, snapshot_sha256, last_command_seq, byte_size,
                    clean_shutdown, created_at, updated_at
             FROM autosave_snapshots
             WHERE document_id = ?1
             ORDER BY rowid DESC LIMIT 1"
        };
        self.conn
            .query_row(sql, params![doc_id], row_to_snapshot)
            .optional()
            .map_err(StorageError::from)
    }
}

fn read_and_verify_snapshot_blob(snapshot: &AutosaveSnapshotInfo) -> StorageResult<Vec<u8>> {
    let file = std::fs::File::open(&snapshot.snapshot_blob_path)?;
    let mut blob = Vec::new();
    file.take(MAX_SNAPSHOT_BYTES.saturating_add(1))
        .read_to_end(&mut blob)?;
    if (blob.len() as u64) > MAX_SNAPSHOT_BYTES {
        return Err(StorageError::RecoveryFailed(format!(
            "snapshot blob '{}' exceeds maximum allowed size of {} bytes",
            snapshot.snapshot_blob_path, MAX_SNAPSHOT_BYTES
        )));
    }

    let actual_sha = sha256_hex(&blob);
    if actual_sha != snapshot.snapshot_sha256 {
        return Err(StorageError::IntegrityMismatch {
            expected: snapshot.snapshot_sha256.clone(),
            actual: actual_sha,
        });
    }

    if blob.starts_with(&[0x1F, 0x8B]) {
        let mut decoded = Vec::new();
        GzDecoder::new(&blob[..])
            .take(MAX_SNAPSHOT_BYTES.saturating_add(1))
            .read_to_end(&mut decoded)?;
        if (decoded.len() as u64) > MAX_SNAPSHOT_BYTES {
            return Err(StorageError::RecoveryFailed(format!(
                "decompressed snapshot '{}' exceeds maximum allowed size of {} bytes",
                snapshot.snapshot_blob_path, MAX_SNAPSHOT_BYTES
            )));
        }
        Ok(decoded)
    } else {
        Ok(blob)
    }
}

fn apply_differential_patch(
    doc: &mut Document,
    patch: &DifferentialSnapshotPatch,
) -> StorageResult<()> {
    doc.set_title(patch.title.clone());
    if let Some(cm) = patch.color_mode {
        doc.set_color_mode(cm);
    }
    if let Some(unit) = patch.default_unit {
        doc.set_default_unit(unit);
    }
    if let Some(dpi) = patch.dpi {
        doc.set_dpi(dpi);
    }
    if let Some(mp) = patch.multipage_view {
        doc.set_multipage_view(mp);
    }
    if let Some(ref pages) = patch.pages {
        doc.clear_pages_for_load();
        for p in pages {
            doc.push_loaded_page(p.clone());
        }
    }
    if let Some(idx) = patch.active_page_index {
        if idx < doc.pages().len() {
            doc.set_active_page_index(idx)?;
        }
    }
    for &del_id in &patch.deleted_object_ids {
        doc.remove_snapshot_object(del_id);
    }
    for (_, obj) in &patch.upserted_objects {
        doc.upsert_snapshot_object(obj.clone());
    }
    if let Some(ref full_assets) = patch.full_assets {
        doc.restore_assets(full_assets.clone());
    } else {
        for &del_aid in &patch.deleted_asset_ids {
            doc.remove_asset(del_aid);
        }
        for (_, asset) in &patch.upserted_assets {
            doc.replace_asset(asset.clone());
        }
    }
    if let Some(ref palettes) = patch.palettes {
        doc.replace_palettes(palettes.clone());
    }
    if let Some(ref doc_pal) = patch.document_palette {
        doc.replace_document_palette(doc_pal.clone());
    }
    if let Some(ref guides) = patch.guides {
        doc.replace_guides(guides.clone());
    }
    if let Some(grid) = patch.grid {
        doc.set_grid(grid);
    }
    if let Some(ref pg) = patch.perspective_grid {
        doc.set_perspective_grid(pg.clone());
    }
    if let Some(ref cs) = patch.color_styles {
        doc.replace_color_styles(cs.clone());
    }
    if let Some(ref os) = patch.object_styles {
        doc.replace_object_styles(os.clone());
    }
    if let Some(ref syms) = patch.symbols {
        doc.replace_symbols(syms.clone());
    }
    if let Some(ref meta) = patch.metadata {
        doc.set_metadata(meta.clone());
    }
    if let Some(nid) = patch.next_id {
        doc.restore_next_id_from_snapshot(nid);
    }
    Ok(())
}

fn row_to_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<AutosaveSnapshotInfo> {
    Ok(AutosaveSnapshotInfo {
        id: row.get(0)?,
        document_id: row.get(1)?,
        session_id: row.get(2)?,
        original_file_path: row.get(3)?,
        display_name: row.get(4)?,
        snapshot_blob_path: row.get(5)?,
        snapshot_sha256: row.get(6)?,
        last_command_seq: row.get::<_, i64>(7)? as u64,
        byte_size: row.get::<_, i64>(8)? as u64,
        clean_shutdown: row.get::<_, i64>(9)? != 0,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn atomic_write_blob(path: &Path, bytes: &[u8]) -> StorageResult<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    crate::db::harden_dir_permissions(parent)?;
    let mut tmp = NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    tmp.as_file_mut().write_all(bytes)?;
    tmp.as_file_mut().flush()?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| StorageError::Io(e.error))?;
    crate::db::harden_file_permissions(path)?;
    Ok(())
}

const fn command_op_type_name(op: &CommandOp) -> &'static str {
    match op {
        CommandOp::CreateObject { .. } => "CreateObject",
        CommandOp::DeleteObjects { .. } => "DeleteObjects",
        CommandOp::TransformObjects { .. } => "TransformObjects",
        CommandOp::SetFill { .. } => "SetFill",
        CommandOp::SetOutline { .. } => "SetOutline",
        CommandOp::SetLens { .. } => "SetLens",
        CommandOp::AddEffect { .. } => "AddEffect",
        CommandOp::RemoveEffect { .. } => "RemoveEffect",
        CommandOp::EditPath { .. } => "EditPath",
        CommandOp::BooleanOp { .. } => "BooleanOp",
        CommandOp::Group { .. } => "Group",
        CommandOp::Ungroup { .. } => "Ungroup",
        CommandOp::SetZOrder { .. } => "SetZOrder",
        CommandOp::AlignObjects { .. } => "AlignObjects",
        CommandOp::DistributeObjects { .. } => "DistributeObjects",
        CommandOp::AddPage { .. } => "AddPage",
        CommandOp::DeletePage { .. } => "DeletePage",
        CommandOp::RenamePage { .. } => "RenamePage",
        CommandOp::AddLayer { .. } => "AddLayer",
        CommandOp::SetLayerFlags { .. } => "SetLayerFlags",
        CommandOp::AddGuide { .. } => "AddGuide",
        CommandOp::MoveGuide { .. } => "MoveGuide",
        CommandOp::RemoveGuide { .. } => "RemoveGuide",
        CommandOp::ToggleGuideLock { .. } => "ToggleGuideLock",
        CommandOp::EditText { .. } => "EditText",
        CommandOp::EditTable { .. } => "EditTable",
        CommandOp::CreatePowerClip { .. } => "CreatePowerClip",
        CommandOp::ExtractPowerClip { .. } => "ExtractPowerClip",
        CommandOp::SetPowerClipLock { .. } => "SetPowerClipLock",
        CommandOp::MoveToDesktopLayer { .. } => "MoveToDesktopLayer",
        CommandOp::ConvertToCurves { .. } => "ConvertToCurves",
        CommandOp::ConvertOutlineToObject { .. } => "ConvertOutlineToObject",
        CommandOp::UpdateRaster { .. } => "UpdateRaster",
        CommandOp::DuplicateObjects { .. } => "DuplicateObjects",
        CommandOp::SetObjectVisibility { .. } => "SetObjectVisibility",
        CommandOp::SetObjectLocked { .. } => "SetObjectLocked",
        CommandOp::SetObjectPrintable { .. } => "SetObjectPrintable",
        CommandOp::RenameObject { .. } => "RenameObject",
        CommandOp::SetOpacity { .. } => "SetOpacity",
        CommandOp::SetBlendMode { .. } => "SetBlendMode",
        CommandOp::SetDocumentProperties { .. } => "SetDocumentProperties",
        CommandOp::SetPageProperties { .. } => "SetPageProperties",
        CommandOp::ReorderPages { .. } => "ReorderPages",
        CommandOp::RenameLayer { .. } => "RenameLayer",
        CommandOp::ReorderLayerObjects { .. } => "ReorderLayerObjects",
        CommandOp::SetGridConfig { .. } => "SetGridConfig",
        CommandOp::SetPerspectiveGrid { .. } => "SetPerspectiveGrid",
        CommandOp::UpsertColorStyle { .. } => "UpsertColorStyle",
        CommandOp::DeleteColorStyle { .. } => "DeleteColorStyle",
        CommandOp::UpsertObjectStyle { .. } => "UpsertObjectStyle",
        CommandOp::DeleteObjectStyle { .. } => "DeleteObjectStyle",
        CommandOp::SetObjectStyleLink { .. } => "SetObjectStyleLink",
        CommandOp::SetObjectColorStyleLink { .. } => "SetObjectColorStyleLink",
        CommandOp::SetObjectPerspective { .. } => "SetObjectPerspective",
        CommandOp::EditBSpline { .. } => "EditBSpline",
        CommandOp::EditArtisticMedia { .. } => "EditArtisticMedia",
        CommandOp::UnlinkClone { .. } => "UnlinkClone",
        CommandOp::UpsertSymbol { .. } => "UpsertSymbol",
        CommandOp::DeleteSymbol { .. } => "DeleteSymbol",
        CommandOp::InstantiateSymbol { .. } => "InstantiateSymbol",
        CommandOp::UnlinkSymbolInstance { .. } => "UnlinkSymbolInstance",
        CommandOp::FitPowerClip { .. } => "FitPowerClip",
        CommandOp::FindAndReplace { .. } => "FindAndReplace",
        CommandOp::EditShape { .. } => "EditShape",
        CommandOp::DeleteLayer { .. } => "DeleteLayer",
        CommandOp::RegisterAsset { .. } => "RegisterAsset",
        CommandOp::CreateDetachedObject { .. } => "CreateDetachedObject",
        CommandOp::EditDimension { .. } => "EditDimension",
        CommandOp::PruneAssets { .. } => "PruneAssets",
        CommandOp::ReplaceObjectInParent { .. } => "ReplaceObjectInParent",
        CommandOp::Batch(_) => "Batch",
    }
}

/// Pure-Rust FIPS 180-4 SHA-256 digest returned as lowercase 64-char hex string.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 9).div_ceil(64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for block in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*chunk);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut hex = String::with_capacity(64);
    for word in h {
        use std::fmt::Write as _;
        let _ = write!(hex, "{word:08x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        Affine2D, BezierPath, Color, Fill, NodePayload, ObjectId, Rect2D, SceneObject,
    };

    #[test]
    fn sha256_matches_nist_test_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn crash_recovery_loads_snapshot_and_replays_subsequent_commands_with_zero_lost_clicks() {
        let db = StorageDb::open_in_memory().unwrap();
        let mut live_doc = Document::new_a4("Poster Draft");
        let doc_id = live_doc.id().to_string();

        // Initial object before snapshot
        let base_id = live_doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "Background",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 210.0, 297.0,
                    ))),
                )
                .with_fill(Fill::Solid(Color::WHITE)),
                None,
            )
            .unwrap();

        // 1. Take autosave snapshot
        let snap = db
            .save_snapshot(
                &doc_id,
                live_doc.title(),
                Some(Path::new("/work/poster.coree")),
                &live_doc,
            )
            .unwrap();
        assert!(!snap.clean_shutdown);
        assert_eq!(snap.last_command_seq, 0);

        // 2. User performs 3 more clicks/commands AFTER snapshot, then app crashes!
        let op1 = CommandOp::create_object(
            SceneObject::new(
                ObjectId(0),
                "Hero Card",
                NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                    10.0, 10.0, 80.0, 50.0,
                ))),
            )
            .with_fill(Fill::Solid(Color::CYAN)),
            None,
        );
        let expected_hero_id = ObjectId(live_doc.next_id());
        apply_command(&mut live_doc, &op1).unwrap();
        db.append_command(&doc_id, 1, &op1).unwrap();

        let op2 = CommandOp::TransformObjects {
            ids: vec![expected_hero_id],
            affine: Affine2D::translate(25.0, 15.0),
        };
        apply_command(&mut live_doc, &op2).unwrap();
        db.append_command(&doc_id, 2, &op2).unwrap();

        let op3 = CommandOp::SetFill {
            ids: vec![base_id],
            fill: Fill::Solid(Color::YELLOW),
        };
        apply_command(&mut live_doc, &op3).unwrap();
        db.append_command(&doc_id, 3, &op3).unwrap();

        // 3. On next launch, detect unclean session with 3 pending ops
        let unclean = db.list_unclean_sessions().unwrap();
        assert_eq!(unclean.len(), 1);
        assert_eq!(unclean[0].document_id, doc_id);
        assert_eq!(unclean[0].pending_journal_ops, 3);

        // 4. Recover document -> must match post-click state bit-for-bit (excluding timestamps)
        let recovered = db.recover_document(&doc_id).unwrap();
        assert_eq!(recovered.objects().len(), 2);
        assert_eq!(
            recovered.object(base_id).unwrap().fill,
            Fill::Solid(Color::YELLOW)
        );
        let bb = recovered.object_bounds(expected_hero_id).unwrap();
        assert!((bb.min_x - 35.0).abs() < 1e-6);
        assert!((bb.min_y - 25.0).abs() < 1e-6);

        // 5. Mark clean close -> no longer listed as unclean
        db.mark_clean_close(&doc_id).unwrap();
        assert!(db.list_unclean_sessions().unwrap().is_empty());
    }

    #[test]
    fn crash_recovery_detects_corrupted_snapshot_sha256() {
        let db = StorageDb::open_in_memory().unwrap();
        let doc = Document::new_a4("Tamper Test");
        let doc_id = doc.id().to_string();
        let snap = db
            .save_snapshot(&doc_id, "Tamper Test", None, &doc)
            .unwrap();

        // Corrupt the snapshot blob on disk
        std::fs::write(&snap.snapshot_blob_path, b"corrupted").unwrap();
        let err = db.recover_document(&doc_id).unwrap_err();
        assert!(matches!(err, StorageError::IntegrityMismatch { .. }));
    }

    #[test]
    fn async_differential_autosave_roundtrip_and_sub_2ms_ui_enqueue() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("diff_autosave.db");
        let db = StorageDb::open(&db_path).unwrap();

        let mut doc = Document::new_a4("10k Differential Doc");
        let doc_id = doc.id().to_string();
        let layer_id = doc.page(0).unwrap().layers[0].id;
        let mut obj_ids = Vec::with_capacity(10_000);
        for i in 0..10_000 {
            let x = (i % 100) as f64 * 2.0;
            let y = (i / 100) as f64 * 2.0;
            let oid = doc
                .add_object(
                    SceneObject::new(
                        ObjectId(0),
                        format!("Obj{i}"),
                        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(x, y, 1.5, 1.5))),
                    )
                    .with_fill(Fill::Solid(Color::CYAN)),
                    Some(layer_id),
                )
                .unwrap();
            obj_ids.push(oid);
        }

        let baseline = db.save_snapshot(&doc_id, doc.title(), None, &doc).unwrap();

        // Mutate 5 objects and enqueue a differential patch on the calling thread
        let mut upserted_objects = Vec::with_capacity(5);
        for &oid in &obj_ids[0..5] {
            let op = CommandOp::SetFill {
                ids: vec![oid],
                fill: Fill::Solid(Color::MAGENTA),
            };
            apply_command(&mut doc, &op).unwrap();
            upserted_objects.push((oid, doc.object(oid).unwrap().clone()));
        }

        let patch = DifferentialSnapshotPatch {
            doc_id: doc_id.clone(),
            session_id: baseline.session_id.clone(),
            title: doc.title().to_owned(),
            source_path: None,
            base_snapshot_id: baseline.id.clone(),
            last_command_seq: 0,
            upserted_objects,
            deleted_object_ids: Vec::new(),
            upserted_assets: Vec::new(),
            deleted_asset_ids: Vec::new(),
            full_assets: None,
            pages: None,
            active_page_index: None,
            next_id: Some(doc.next_id()),
            color_mode: None,
            default_unit: None,
            dpi: None,
            multipage_view: None,
            palettes: None,
            document_palette: None,
            guides: None,
            grid: None,
            perspective_grid: None,
            color_styles: None,
            object_styles: None,
            symbols: None,
            metadata: None,
        };

        let start = std::time::Instant::now();
        let delta_id = db.enqueue_differential_autosave(patch).unwrap();
        let enqueue_ms = start.elapsed().as_secs_f64() * 1000.0;
        let limit_ms: f64 = if cfg!(debug_assertions) { 25.0 } else { 2.0 };
        assert!(
            enqueue_ms < limit_ms,
            "UI-thread differential enqueue took {enqueue_ms:.3} ms (expected < {limit_ms:.1} ms)"
        );
        assert!(!delta_id.is_empty());

        db.flush_autosave(Duration::from_secs(5)).unwrap();
        drop(db);

        // Reopen and verify full 10,000-object + 5 mutated objects recovery
        let db2 = StorageDb::open(&db_path).unwrap();
        let recovered = db2.recover_document(&doc_id).unwrap();
        assert_eq!(recovered.objects().len(), 10_000);
        for &oid in &obj_ids[0..5] {
            assert_eq!(
                recovered.object(oid).unwrap().fill,
                Fill::Solid(Color::MAGENTA)
            );
        }
        assert_eq!(
            recovered.object(obj_ids[5000]).unwrap().fill,
            Fill::Solid(Color::CYAN)
        );
    }

    #[test]
    fn differential_autosave_detects_tampered_delta_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("tamper_delta.db");
        let db = StorageDb::open(&db_path).unwrap();

        let mut doc = Document::new_a4("Tamper Delta");
        let doc_id = doc.id().to_string();
        let lid = doc.page(0).unwrap().layers[0].id;
        let oid = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    "R",
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                        0.0, 0.0, 10.0, 10.0,
                    ))),
                ),
                Some(lid),
            )
            .unwrap();
        let base = db.save_snapshot(&doc_id, doc.title(), None, &doc).unwrap();
        let patch = DifferentialSnapshotPatch {
            doc_id: doc_id.clone(),
            session_id: base.session_id.clone(),
            title: doc.title().to_owned(),
            source_path: None,
            base_snapshot_id: base.id.clone(),
            last_command_seq: 0,
            upserted_objects: vec![(oid, doc.object(oid).unwrap().clone())],
            deleted_object_ids: Vec::new(),
            upserted_assets: Vec::new(),
            deleted_asset_ids: Vec::new(),
            full_assets: None,
            pages: None,
            active_page_index: None,
            next_id: None,
            color_mode: None,
            default_unit: None,
            dpi: None,
            multipage_view: None,
            palettes: None,
            document_palette: None,
            guides: None,
            grid: None,
            perspective_grid: None,
            color_styles: None,
            object_styles: None,
            symbols: None,
            metadata: None,
        };
        let delta_id = db.enqueue_differential_autosave(patch).unwrap();
        db.flush_autosave(Duration::from_secs(5)).unwrap();

        let delta_path: String = db
            .connection()
            .query_row(
                "SELECT snapshot_blob_path FROM autosave_snapshots WHERE id = ?1",
                params![delta_id],
                |r| r.get(0),
            )
            .unwrap();
        std::fs::write(delta_path, b"corrupted-delta").unwrap();

        let err = db.recover_document(&doc_id).unwrap_err();
        assert!(matches!(err, StorageError::IntegrityMismatch { .. }));
    }

    #[test]
    fn background_wal_baseline_compaction_resets_chain_depth_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let db = StorageDb::open(&dir.path().join("wal_compact.db")).unwrap();
        let mut doc = Document::new_a4("CompactTest");
        let doc_id = doc.id().to_string();
        let lid = doc.pages()[0].layers[0].id;
        let base = db.save_snapshot(&doc_id, doc.title(), None, &doc).unwrap();
        let mut cur_base = base.id;
        let mut oid = None;
        for i in 0..5u64 {
            let id = doc
                .add_object(
                    SceneObject::new(
                        ObjectId(0),
                        format!("R{i}"),
                        NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                            (i as f64) * 5.0,
                            0.0,
                            10.0,
                            10.0,
                        ))),
                    ),
                    Some(lid),
                )
                .unwrap();
            oid = Some(id);
            let patch = DifferentialSnapshotPatch {
                doc_id: doc_id.clone(),
                session_id: base.session_id.clone(),
                title: doc.title().to_owned(),
                source_path: None,
                base_snapshot_id: cur_base.clone(),
                last_command_seq: 0,
                upserted_objects: vec![(id, doc.object(id).unwrap().clone())],
                deleted_object_ids: Vec::new(),
                upserted_assets: Vec::new(),
                deleted_asset_ids: Vec::new(),
                full_assets: None,
                pages: Some(doc.pages().to_vec()),
                active_page_index: None,
                next_id: Some(doc.next_id()),
                color_mode: None,
                default_unit: None,
                dpi: None,
                multipage_view: None,
                palettes: None,
                document_palette: None,
                guides: None,
                grid: None,
                perspective_grid: None,
                color_styles: None,
                object_styles: None,
                symbols: None,
                metadata: None,
            };
            cur_base = db.enqueue_differential_autosave(patch).unwrap();
        }
        let compacted_id = db
            .enqueue_compact_baseline(&doc_id, &base.session_id, doc.title(), None, &cur_base, 0)
            .unwrap();
        db.flush_autosave(Duration::from_secs(5)).unwrap();

        let recovered = db.recover_document(&doc_id).unwrap();
        assert_eq!(recovered.objects().len(), 5);
        assert!(recovered.has_object(oid.unwrap()));
        let info = db.get_snapshot_by_id(&compacted_id).unwrap().unwrap();
        assert!(info.snapshot_blob_path.ends_with(".snapshot.json.gz"));
    }
}
