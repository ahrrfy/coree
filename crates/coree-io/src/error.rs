// SPDX-License-Identifier: MPL-2.0

//! Error types and security/resource limits for hardened I/O operations.

use std::fmt;
use thiserror::Error;

/// Default maximum uncompressed archive payload size (512 MiB).
pub const DEFAULT_MAX_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;
/// Default maximum decompression ratio (100:1) to block Zip-Bombs.
pub const DEFAULT_MAX_COMPRESSION_RATIO: f64 = 100.0;
/// Minimum entry size before enforcing compression-ratio Zip-Bomb rejection (16 KiB).
pub const DEFAULT_MIN_RATIO_CHECK_BYTES: u64 = 16 * 1024;
/// Default maximum number of entries inside a ZIP container.
pub const DEFAULT_MAX_ZIP_ENTRIES: usize = 10_000;
/// Default maximum XML/SVG element nesting depth.
pub const DEFAULT_MAX_SVG_DEPTH: u32 = 256;
/// Default maximum SVG input size (64 MiB).
pub const DEFAULT_MAX_SVG_BYTES: usize = 64 * 1024 * 1024;
/// Default maximum raster width or height in pixels (`32768 px`).
pub const DEFAULT_MAX_RASTER_DIMENSION: u32 = 32_768;
/// Default maximum total pixel count (`256M` pixels ≈ 1 GiB RGBA).
pub const DEFAULT_MAX_RASTER_PIXELS: u64 = 256 * 1024 * 1024;

/// Unified error type returned by all `coree-io` codecs and containers.
#[derive(Debug, Error)]
pub enum IoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("ZIP container error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("image codec error: {0}")]
    Image(#[from] image::ImageError),

    #[error("core document error: {0}")]
    Core(#[from] coree_core::CoreError),

    #[error("Zip-Slip path traversal rejected: '{path}'")]
    ZipSlip { path: String },

    #[error("Zip-Bomb rejected: {reason}")]
    ZipBomb { reason: String },

    #[error("resource limit exceeded ({limit}): {actual}")]
    LimitExceeded { limit: String, actual: String },

    #[error("invalid {format} data: {message}")]
    InvalidFormat { format: String, message: String },

    #[error("SVG parse error: {0}")]
    SvgParse(String),

    #[error("page index {page_index} out of range (document has {page_count} page(s))")]
    PageOutOfRange {
        page_index: usize,
        page_count: usize,
    },

    #[error("unsupported palette format: {0}")]
    UnsupportedPaletteFormat(String),
}

/// Result alias for `coree-io` operations.
pub type IoResult<T> = Result<T, IoError>;

/// Security and memory-safety limits enforced across archive, SVG, and raster parsers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IoLimits {
    /// Maximum cumulative decompressed bytes allowed across a ZIP archive.
    pub max_uncompressed_bytes: u64,
    /// Maximum allowed uncompressed-to-compressed ratio for entries above `min_ratio_check_bytes`.
    pub max_compression_ratio: f64,
    /// Minimum decompressed entry size (bytes) at which `max_compression_ratio` is enforced.
    pub min_ratio_check_bytes: u64,
    /// Maximum number of entries allowed in a single ZIP archive.
    pub max_zip_entries: usize,
    /// Maximum XML/SVG nesting depth.
    pub max_svg_depth: u32,
    /// Maximum SVG input size in bytes.
    pub max_svg_bytes: usize,
    /// Maximum raster image width or height in pixels.
    pub max_raster_dimension: u32,
    /// Maximum total raster pixels (`width * height`).
    pub max_raster_pixels: u64,
}

impl Default for IoLimits {
    fn default() -> Self {
        Self {
            max_uncompressed_bytes: DEFAULT_MAX_UNCOMPRESSED_BYTES,
            max_compression_ratio: DEFAULT_MAX_COMPRESSION_RATIO,
            min_ratio_check_bytes: DEFAULT_MIN_RATIO_CHECK_BYTES,
            max_zip_entries: DEFAULT_MAX_ZIP_ENTRIES,
            max_svg_depth: DEFAULT_MAX_SVG_DEPTH,
            max_svg_bytes: DEFAULT_MAX_SVG_BYTES,
            max_raster_dimension: DEFAULT_MAX_RASTER_DIMENSION,
            max_raster_pixels: DEFAULT_MAX_RASTER_PIXELS,
        }
    }
}

impl fmt::Display for IoLimits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "IoLimits(max_zip={}B, ratio={}x, svg_depth={}, max_px={})",
            self.max_uncompressed_bytes,
            self.max_compression_ratio,
            self.max_svg_depth,
            self.max_raster_dimension
        )
    }
}

/// Strictly validates and normalizes a ZIP entry path to defeat Zip-Slip path traversal.
///
/// Rejects:
/// - Empty paths or NUL/control characters
/// - Absolute Unix (`/foo`) or Windows (`\foo`, `C:\foo`, `C:foo`, `\\server\share`) paths
/// - Any parent-directory traversal component (`..`) or current-directory segment (`.`)
/// - Backslash separators (rejected after checking so crafted Windows traversal cannot sneak in)
pub fn sanitize_zip_path(raw: &str) -> IoResult<String> {
    if raw.is_empty() {
        return Err(IoError::ZipSlip {
            path: raw.to_owned(),
        });
    }
    if raw.bytes().any(|b| b == 0 || b < 0x20) {
        return Err(IoError::ZipSlip {
            path: raw.to_owned(),
        });
    }
    if raw.starts_with('/') || raw.starts_with('\\') || raw.contains('\\') {
        return Err(IoError::ZipSlip {
            path: raw.to_owned(),
        });
    }
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(IoError::ZipSlip {
            path: raw.to_owned(),
        });
    }

    let trimmed = raw.strip_suffix('/').unwrap_or(raw);
    if trimmed.is_empty() {
        return Err(IoError::ZipSlip {
            path: raw.to_owned(),
        });
    }

    let mut clean_parts = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains(':') {
            return Err(IoError::ZipSlip {
                path: raw.to_owned(),
            });
        }
        clean_parts.push(segment);
    }

    Ok(clean_parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_zip_path_accepts_clean_relative_paths() {
        assert_eq!(sanitize_zip_path("manifest.json").unwrap(), "manifest.json");
        assert_eq!(
            sanitize_zip_path("assets/1234.png").unwrap(),
            "assets/1234.png"
        );
        assert_eq!(sanitize_zip_path("assets/").unwrap(), "assets");
    }

    #[test]
    fn sanitize_zip_path_rejects_traversal_and_absolute_paths() {
        let bad = [
            "",
            "/",
            "/etc/passwd",
            "../evil.txt",
            "assets/../../evil.txt",
            "assets/./doc.json",
            "assets\\evil.png",
            "C:evil.txt",
            "C:/Windows/System32",
            "foo\0bar.json",
            "assets//double.png",
        ];
        for path in bad {
            assert!(
                matches!(sanitize_zip_path(path), Err(IoError::ZipSlip { .. })),
                "expected ZipSlip for {path:?}"
            );
        }
    }
}
