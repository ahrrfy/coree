// SPDX-License-Identifier: MPL-2.0

//! Error types for the core document, geometry, and command engine.

use crate::ids::{ImageAssetId, LayerId, ObjectId, PageId, SymbolId};
use thiserror::Error;

/// Convenience result alias for `coree-core` operations.
pub type CoreResult<T> = Result<T, CoreError>;

/// Errors returned by `coree-core` operations.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Referenced scene object does not exist in the document.
    #[error("object not found: {0}")]
    ObjectNotFound(ObjectId),

    /// Referenced page does not exist in the document.
    #[error("page not found: {0}")]
    PageNotFound(PageId),

    /// Page index is outside the document's page list.
    #[error("page index {index} out of range (total pages: {len})")]
    PageIndexOutOfRange { index: usize, len: usize },

    /// Referenced layer does not exist in the document.
    #[error("layer not found: {0}")]
    LayerNotFound(LayerId),

    /// Referenced raster image asset does not exist.
    #[error("image asset not found: {0}")]
    AssetNotFound(ImageAssetId),

    /// Referenced symbol definition does not exist.
    #[error("symbol not found: {0}")]
    SymbolNotFound(SymbolId),

    /// Referenced guide does not exist.
    #[error("guide not found: #{0}")]
    GuideNotFound(u64),

    /// Attempted to delete the last remaining page in a document.
    #[error("cannot delete the last remaining page in a document")]
    CannotDeleteLastPage,

    /// Attempted to delete the last remaining layer on a page.
    #[error("cannot delete the last remaining layer on a page")]
    CannotDeleteLastLayer,

    /// Attempted to mutate a locked scene object.
    #[error("object is locked: {0}")]
    ObjectLocked(ObjectId),

    /// Malformed, non-finite, or degenerate geometry was supplied.
    #[error("invalid geometry: {0}")]
    InvalidGeometry(String),

    /// A numeric or structural parameter was out of bounds.
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),

    /// A color string or component could not be parsed.
    #[error("invalid color: {0}")]
    InvalidColor(String),

    /// A table manipulation failed (e.g. out-of-range cell or overlap).
    #[error("invalid table operation: {0}")]
    InvalidTableOp(String),

    /// A boolean polygon operation failed.
    #[error("boolean operation failed: {0}")]
    BooleanOpFailed(String),

    /// Cyclic container reference detected (e.g., nested group or PowerClip).
    #[error("cyclic reference detected: {0}")]
    CycleDetected(String),

    /// Undo requested with no commands on the undo stack.
    #[error("undo stack is empty")]
    EmptyUndoStack,

    /// Redo requested with no commands on the redo stack.
    #[error("redo stack is empty")]
    EmptyRedoStack,

    /// Commit or cancel requested when no transaction was started.
    #[error("no active command transaction")]
    TransactionNotActive,

    /// Nested transaction start was attempted while one is already open.
    #[error("a command transaction is already active")]
    TransactionAlreadyActive,

    /// Replaying a deterministic command log failed at a specific index.
    #[error("command replay failed at index {index}: {reason}")]
    CommandReplayFailed { index: usize, reason: String },

    /// Executing or compiling a Rhai macro script failed.
    #[error("macro execution failed: {0}")]
    MacroError(String),

    /// JSON serialization or deserialization failed.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
