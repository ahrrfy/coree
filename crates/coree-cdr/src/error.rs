// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error types for clean-room `.cdr`, `.cdt`, `.cmx`, `.cpt` codecs.

use thiserror::Error;

/// Unified error type returned by all `coree-cdr` parsing, writing, and inspection APIs.
#[derive(Debug, Error)]
pub enum CdrError {
    /// Underlying filesystem or stream I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Error propagated from `coree-core` document or asset validation.
    #[error("core document error: {0}")]
    Core(#[from] coree_core::CoreError),

    /// File signature or container layout is not a recognized `.cdr`/`.cdt`/`.cmx`/`.cpt` format.
    #[error("unsupported vector/raster container format: {0}")]
    UnsupportedFormat(String),

    /// Expected magic signature did not match the input header.
    #[error("invalid magic header: expected {expected}, found {found}")]
    InvalidMagic { expected: String, found: String },

    /// Structural header fields are malformed or inconsistent.
    #[error("malformed header: {0}")]
    MalformedHeader(String),

    /// Input buffer ended before the required number of bytes could be read.
    #[error(
        "unexpected EOF while reading {context}: needed {needed} bytes, available {available}"
    )]
    UnexpectedEof {
        context: &'static str,
        needed: usize,
        available: usize,
    },

    /// A RIFF, CMX, or CPT chunk had invalid size, structure, or payload.
    #[error("invalid chunk '{fourcc}': {reason}")]
    InvalidChunk { fourcc: String, reason: String },

    /// Container nesting exceeded [`crate::limits::CodecLimits::max_tree_depth`].
    #[error("container nesting depth {depth} exceeds limit {limit}")]
    DepthLimitExceeded { depth: usize, limit: usize },

    /// Total chunk count exceeded [`crate::limits::CodecLimits::max_chunks`].
    #[error("chunk count {count} exceeds limit {limit}")]
    ChunkLimitExceeded { count: usize, limit: usize },

    /// ZIP entry count exceeded [`crate::limits::CodecLimits::max_zip_entries`].
    #[error("ZIP entry count {count} exceeds limit {limit}")]
    ZipEntryLimitExceeded { count: usize, limit: usize },

    /// Decompressed byte count exceeded [`crate::limits::CodecLimits::max_uncompressed_bytes`].
    #[error("uncompressed payload exceeds security limit of {limit} bytes")]
    UncompressedSizeLimitExceeded { limit: u64 },

    /// Decompression expansion ratio exceeded [`crate::limits::CodecLimits::max_compression_ratio`].
    #[error("compression ratio exceeded ({uncompressed} bytes from {compressed} compressed bytes, max ratio {max_ratio}x)")]
    CompressionRatioExceeded {
        compressed: u64,
        uncompressed: u64,
        max_ratio: u64,
    },

    /// ZIP entry path attempted path traversal, absolute path, backslash, drive prefix, or null byte.
    #[error("unsafe ZIP path rejected (Zip-Slip protection): '{path}'")]
    ZipSlipDetected { path: String },

    /// Bitmap dimensions are zero, overflow RGBA buffer math, or exceed security cap.
    #[error("invalid bitmap dimensions {width}x{height} (limit {max_dim}px per axis)")]
    InvalidBitmapDimensions {
        width: u32,
        height: u32,
        max_dim: u32,
    },

    /// Zlib, Deflate, or RLE decompression failed.
    #[error("decompression failed: {0}")]
    DecompressionFailed(String),

    /// ZIP container reading or writing failed.
    #[error("ZIP container error: {0}")]
    ZipError(String),

    /// Checked integer arithmetic overflowed while decoding untrusted sizes/offsets.
    #[error("arithmetic overflow while computing {0}")]
    ArithmeticOverflow(&'static str),

    /// Text or binary encoding error.
    #[error("encoding error: {0}")]
    EncodingError(String),
}

/// Convenience result alias for `coree-cdr`.
pub type CdrResult<T> = Result<T, CdrError>;
