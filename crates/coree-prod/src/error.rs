// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error types for the `coree-prod` production, prepress, packaging, and automation engine.

use coree_core::CoreError;
use thiserror::Error;

/// Convenience result alias for `coree-prod` operations.
pub type ProdResult<T> = Result<T, ProdError>;

/// Errors returned by `coree-prod` operations.
#[derive(Debug, Error)]
pub enum ProdError {
    /// Underlying error from `coree-core`.
    #[error("core error: {0}")]
    Core(#[from] CoreError),

    /// Filesystem I/O error (e.g., during Collect for Output).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Invalid numeric or structural parameter supplied to a production tool.
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),

    /// Document has no pages or required content is missing.
    #[error("empty document: {0}")]
    EmptyDocument(String),

    /// Page index is out of bounds for the document.
    #[error("page index {index} out of range (total pages: {total})")]
    PageOutOfRange { index: usize, total: usize },

    /// Barcode or QR code encoding/validation failure.
    #[error("barcode error: {0}")]
    BarcodeError(String),

    /// RFC-4180 CSV/TSV parsing failure.
    #[error("CSV parse error on line {line}: {reason}")]
    CsvParseError { line: usize, reason: String },

    /// Variable-data print merge failure.
    #[error("print merge error: {0}")]
    MergeError(String),

    /// Sandboxed Rhai macro compilation or runtime error (including op-limit exceeded).
    #[error("macro error: {0}")]
    MacroError(String),

    /// Image encoding failure (e.g., extracting raster assets).
    #[error("image encoding error: {0}")]
    ImageError(String),
}
