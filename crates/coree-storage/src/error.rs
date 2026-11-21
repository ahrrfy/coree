// SPDX-License-Identifier: MPL-2.0

//! Unified error type for local offline SQLite persistence, crash recovery,
//! font indexing, and background job execution.

use thiserror::Error;

/// Error returned by `coree-storage` operations.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("core document error: {0}")]
    Core(#[from] coree_core::CoreError),

    #[error("record not found: {0}")]
    NotFound(String),

    #[error("snapshot integrity mismatch: expected sha256 {expected}, got {actual}")]
    IntegrityMismatch { expected: String, actual: String },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("crash recovery failed: {0}")]
    RecoveryFailed(String),

    #[error("thread pool initialization failed: {0}")]
    ThreadPool(String),
}

/// Result alias for `coree-storage`.
pub type StorageResult<T> = Result<T, StorageError>;
