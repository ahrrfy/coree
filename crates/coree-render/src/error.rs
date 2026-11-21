// SPDX-License-Identifier: MPL-2.0

//! Error types for the rendering, text layout, raster studio, tracing, and export engines.

use coree_core::CoreError;
use thiserror::Error;

/// Convenience result alias for `coree-render` operations.
pub type RenderResult<T> = Result<T, RenderError>;

/// Errors returned by `coree-render` operations.
#[derive(Debug, Error)]
pub enum RenderError {
    /// Target canvas or raster dimensions are zero, negative, non-finite, or exceed safe limits.
    #[error("invalid render dimensions: {width}x{height} ({reason})")]
    InvalidDimensions {
        width: u32,
        height: u32,
        reason: String,
    },

    /// Requested page index does not exist in the document.
    #[error("page index {index} out of range (total pages: {len})")]
    PageOutOfRange { index: usize, len: usize },

    /// Underlying pixmap or image buffer allocation failed.
    #[error("failed to allocate pixel buffer ({width}x{height})")]
    AllocationFailed { width: u32, height: u32 },

    /// An invalid parameter (e.g. non-positive DPI, missing object) was supplied.
    #[error("invalid render parameter: {0}")]
    InvalidParameter(String),

    /// Text shaping or glyph outline extraction failed.
    #[error("text layout error: {0}")]
    TextLayout(String),

    /// Raster image encoding, decoding, or filtering failed.
    #[error("raster engine error: {0}")]
    Raster(String),

    /// Propagated core document or geometry error.
    #[error("core error: {0}")]
    Core(#[from] CoreError),
}
