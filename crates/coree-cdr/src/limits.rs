// SPDX-License-Identifier: MIT OR Apache-2.0

//! Security limits, Zip-Slip path sanitization, bounded decompression readers,
//! and panic-free checked binary readers.

use crate::error::{CdrError, CdrResult};
use flate2::read::{DeflateDecoder, ZlibDecoder};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Default hard cap on total uncompressed bytes decoded from any file (`512 MiB`).
pub const MAX_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

/// Default maximum allowed decompression expansion ratio (`100:1`).
pub const MAX_COMPRESSION_RATIO: u64 = 100;

/// Default maximum container / group nesting depth (`64`).
pub const MAX_TREE_DEPTH: usize = 64;

/// Default maximum allowed bitmap width or height in pixels (`32_768`).
pub const MAX_BITMAP_DIM: u32 = 32_768;

/// Default maximum number of chunks parsed from a single document (`500_000`).
pub const MAX_CHUNKS: usize = 500_000;

/// Default maximum number of entries permitted in a ZIP container (`10_000`).
pub const MAX_ZIP_ENTRIES: usize = 10_000;

/// Minimum decompressed payload bytes before applying compression-ratio rejection
/// (avoids false positives on tiny legitimate streams such as 40-byte MIME headers).
const RATIO_CHECK_MIN_BYTES: u64 = 4096;

/// Configurable security bounds enforced across all `coree-cdr` parsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodecLimits {
    pub max_uncompressed_bytes: u64,
    pub max_compression_ratio: u64,
    pub max_tree_depth: usize,
    pub max_bitmap_dim: u32,
    pub max_chunks: usize,
    pub max_zip_entries: usize,
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_uncompressed_bytes: MAX_UNCOMPRESSED_BYTES,
            max_compression_ratio: MAX_COMPRESSION_RATIO,
            max_tree_depth: MAX_TREE_DEPTH,
            max_bitmap_dim: MAX_BITMAP_DIM,
            max_chunks: MAX_CHUNKS,
            max_zip_entries: MAX_ZIP_ENTRIES,
        }
    }
}

impl CodecLimits {
    /// Checks whether `depth` exceeds [`Self::max_tree_depth`].
    pub fn check_depth(&self, depth: usize) -> CdrResult<()> {
        if depth > self.max_tree_depth {
            return Err(CdrError::DepthLimitExceeded {
                depth,
                limit: self.max_tree_depth,
            });
        }
        Ok(())
    }

    /// Increments `counter` and ensures total parsed chunks do not exceed [`Self::max_chunks`].
    pub fn check_and_inc_chunks(&self, counter: &mut usize) -> CdrResult<()> {
        let next = counter
            .checked_add(1)
            .ok_or(CdrError::ArithmeticOverflow("chunk counter"))?;
        if next > self.max_chunks {
            return Err(CdrError::ChunkLimitExceeded {
                count: next,
                limit: self.max_chunks,
            });
        }
        *counter = next;
        Ok(())
    }

    /// Checks whether a ZIP archive entry count is within [`Self::max_zip_entries`].
    pub fn check_zip_entries(&self, count: usize) -> CdrResult<()> {
        if count > self.max_zip_entries {
            return Err(CdrError::ZipEntryLimitExceeded {
                count,
                limit: self.max_zip_entries,
            });
        }
        Ok(())
    }

    /// Checks whether a decompressed byte count violates the compression ratio guard.
    pub fn check_compression_ratio(&self, compressed: u64, uncompressed: u64) -> CdrResult<()> {
        if compressed == 0 && uncompressed > 0 {
            return Err(CdrError::CompressionRatioExceeded {
                compressed,
                uncompressed,
                max_ratio: self.max_compression_ratio,
            });
        }
        if uncompressed > RATIO_CHECK_MIN_BYTES {
            let max_allowed = compressed.saturating_mul(self.max_compression_ratio);
            if uncompressed > max_allowed {
                return Err(CdrError::CompressionRatioExceeded {
                    compressed,
                    uncompressed,
                    max_ratio: self.max_compression_ratio,
                });
            }
        }
        Ok(())
    }

    /// Validates raster bitmap dimensions and returns the exact RGBA8 byte count (`w * h * 4`).
    pub fn validate_bitmap_dims(&self, width: u32, height: u32) -> CdrResult<usize> {
        if width == 0 || height == 0 || width > self.max_bitmap_dim || height > self.max_bitmap_dim
        {
            return Err(CdrError::InvalidBitmapDimensions {
                width,
                height,
                max_dim: self.max_bitmap_dim,
            });
        }
        let rgba_bytes_u64 = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|px| px.checked_mul(4))
            .ok_or(CdrError::ArithmeticOverflow("bitmap RGBA byte count"))?;
        if rgba_bytes_u64 > self.max_uncompressed_bytes {
            return Err(CdrError::UncompressedSizeLimitExceeded {
                limit: self.max_uncompressed_bytes,
            });
        }
        usize::try_from(rgba_bytes_u64).map_err(|_| CdrError::UncompressedSizeLimitExceeded {
            limit: self.max_uncompressed_bytes,
        })
    }

    /// Consumes `amount` bytes from a remaining uncompressed budget, rejecting overflow.
    pub fn consume_budget(&self, budget_remaining: &mut u64, amount: u64) -> CdrResult<()> {
        if amount > *budget_remaining {
            return Err(CdrError::UncompressedSizeLimitExceeded {
                limit: self.max_uncompressed_bytes,
            });
        }
        *budget_remaining -= amount;
        Ok(())
    }
}

/// Strictly validates a raw ZIP entry path against Zip-Slip and path-injection attacks.
///
/// Rejects:
/// - Embedded NUL bytes (`\0`) or ASCII control characters
/// - Invalid UTF-8 sequences
/// - Empty paths
/// - Backslashes (`\`)
/// - Leading slashes (`/`) or double slashes (`//`)
/// - Windows drive prefixes or colons (`C:`, `c:`, `foo:bar`)
/// - Parent directory traversal (`..`) or dot segments (`.`)
pub fn sanitize_zip_path(raw: &[u8]) -> CdrResult<String> {
    if raw.contains(&0) {
        return Err(CdrError::ZipSlipDetected {
            path: String::from_utf8_lossy(raw).into_owned(),
        });
    }
    let path = std::str::from_utf8(raw).map_err(|_| CdrError::ZipSlipDetected {
        path: String::from_utf8_lossy(raw).into_owned(),
    })?;

    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.bytes().any(|b| b < 0x20 || b == 0x7F)
    {
        return Err(CdrError::ZipSlipDetected {
            path: path.to_owned(),
        });
    }

    // Allow a single trailing slash for directory entries, but reject bare "/" or "a//".
    let trimmed = path.strip_suffix('/').unwrap_or(path);
    if trimmed.is_empty() {
        return Err(CdrError::ZipSlipDetected {
            path: path.to_owned(),
        });
    }

    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(CdrError::ZipSlipDetected {
                path: path.to_owned(),
            });
        }
    }

    Ok(path.to_owned())
}

/// Reads a file from disk with strict upfront metadata AND streaming `.take(limit + 1)`
/// size enforcement against [`CodecLimits::max_uncompressed_bytes`].
pub fn read_file_bounded(path: &Path, limits: &CodecLimits) -> CdrResult<Vec<u8>> {
    let file = File::open(path)?;
    if let Ok(meta) = file.metadata() {
        if meta.len() > limits.max_uncompressed_bytes {
            return Err(CdrError::UncompressedSizeLimitExceeded {
                limit: limits.max_uncompressed_bytes,
            });
        }
    }
    let take_cap = limits.max_uncompressed_bytes.saturating_add(1);
    let mut limited = file.take(take_cap);
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    let mut total: u64 = 0;

    loop {
        let n = limited.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .ok_or(CdrError::ArithmeticOverflow("file read length"))?;
        if total > limits.max_uncompressed_bytes {
            return Err(CdrError::UncompressedSizeLimitExceeded {
                limit: limits.max_uncompressed_bytes,
            });
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

/// Streams bytes from `reader` using `.take(effective_limit + 1)` so Zip-Bombs NEVER OOM.
///
/// Enforces BOTH the byte cap (`effective_limit`) AND optional compression ratio on every chunk read.
pub fn read_bounded<R: Read>(
    reader: R,
    limits: &CodecLimits,
    budget_remaining: &mut u64,
    compressed_size: Option<u64>,
) -> CdrResult<Vec<u8>> {
    let effective_limit = (*budget_remaining).min(limits.max_uncompressed_bytes);
    let take_cap = effective_limit.saturating_add(1);
    let mut limited = reader.take(take_cap);

    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    let mut total: u64 = 0;

    loop {
        let n = limited
            .read(&mut buf)
            .map_err(|e| CdrError::DecompressionFailed(e.to_string()))?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .ok_or(CdrError::ArithmeticOverflow("decompressed stream length"))?;
        if total > effective_limit {
            return Err(CdrError::UncompressedSizeLimitExceeded {
                limit: limits.max_uncompressed_bytes,
            });
        }
        if let Some(comp) = compressed_size {
            limits.check_compression_ratio(comp, total)?;
        }
        out.extend_from_slice(&buf[..n]);
    }

    limits.consume_budget(budget_remaining, total)?;
    Ok(out)
}

/// Decompresses a Zlib stream with strict streaming size and compression-ratio caps.
pub fn decompress_zlib_bounded(
    compressed: &[u8],
    limits: &CodecLimits,
    budget_remaining: &mut u64,
) -> CdrResult<Vec<u8>> {
    let decoder = ZlibDecoder::new(compressed);
    read_bounded(
        decoder,
        limits,
        budget_remaining,
        Some(compressed.len() as u64),
    )
}

/// Decompresses a raw Deflate stream with strict streaming size and compression-ratio caps.
pub fn decompress_deflate_bounded(
    compressed: &[u8],
    limits: &CodecLimits,
    budget_remaining: &mut u64,
) -> CdrResult<Vec<u8>> {
    let decoder = DeflateDecoder::new(compressed);
    read_bounded(
        decoder,
        limits,
        budget_remaining,
        Some(compressed.len() as u64),
    )
}

/// Panic-free, checked binary cursor over a byte slice.
#[derive(Debug, Clone, Copy)]
pub struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    /// Creates a new reader positioned at byte `0`.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Current byte offset within the underlying slice.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Total length of the underlying slice.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns `true` if the underlying slice has zero length.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Number of unread bytes remaining.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// Returns `true` when no bytes remain.
    #[must_use]
    pub const fn is_eof(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// Seeks to an absolute byte offset, validating bounds.
    pub fn seek(&mut self, offset: usize, context: &'static str) -> CdrResult<()> {
        if offset > self.data.len() {
            return Err(CdrError::UnexpectedEof {
                context,
                needed: offset,
                available: self.data.len(),
            });
        }
        self.pos = offset;
        Ok(())
    }

    /// Advances the cursor by `len` bytes without returning them.
    pub fn skip(&mut self, len: usize, context: &'static str) -> CdrResult<()> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(CdrError::ArithmeticOverflow(context))?;
        if end > self.data.len() {
            return Err(CdrError::UnexpectedEof {
                context,
                needed: len,
                available: self.remaining(),
            });
        }
        self.pos = end;
        Ok(())
    }

    /// Reads an exact sub-slice of `len` bytes.
    pub fn read_bytes(&mut self, len: usize, context: &'static str) -> CdrResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(CdrError::ArithmeticOverflow(context))?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or(CdrError::UnexpectedEof {
                context,
                needed: len,
                available: self.remaining(),
            })?;
        self.pos = end;
        Ok(slice)
    }

    /// Reads a fixed-size byte array `[u8; N]`.
    pub fn read_array<const N: usize>(&mut self, context: &'static str) -> CdrResult<[u8; N]> {
        let slice = self.read_bytes(N, context)?;
        let mut arr = [0u8; N];
        arr.copy_from_slice(slice);
        Ok(arr)
    }

    /// Reads a 4-byte FourCC identifier.
    pub fn read_fourcc(&mut self, context: &'static str) -> CdrResult<[u8; 4]> {
        self.read_array::<4>(context)
    }

    /// Reads one `u8`.
    pub fn read_u8(&mut self, context: &'static str) -> CdrResult<u8> {
        Ok(self.read_array::<1>(context)?[0])
    }

    /// Reads one signed `i8`.
    pub fn read_i8(&mut self, context: &'static str) -> CdrResult<i8> {
        Ok(self.read_u8(context)? as i8)
    }

    /// Reads a little-endian `u16`.
    pub fn read_u16_le(&mut self, context: &'static str) -> CdrResult<u16> {
        Ok(u16::from_le_bytes(self.read_array::<2>(context)?))
    }

    /// Reads a big-endian `u16`.
    pub fn read_u16_be(&mut self, context: &'static str) -> CdrResult<u16> {
        Ok(u16::from_be_bytes(self.read_array::<2>(context)?))
    }

    /// Reads a little-endian `i16`.
    pub fn read_i16_le(&mut self, context: &'static str) -> CdrResult<i16> {
        Ok(i16::from_le_bytes(self.read_array::<2>(context)?))
    }

    /// Reads a big-endian `i16`.
    pub fn read_i16_be(&mut self, context: &'static str) -> CdrResult<i16> {
        Ok(i16::from_be_bytes(self.read_array::<2>(context)?))
    }

    /// Reads a little-endian `u32`.
    pub fn read_u32_le(&mut self, context: &'static str) -> CdrResult<u32> {
        Ok(u32::from_le_bytes(self.read_array::<4>(context)?))
    }

    /// Reads a big-endian `u32`.
    pub fn read_u32_be(&mut self, context: &'static str) -> CdrResult<u32> {
        Ok(u32::from_be_bytes(self.read_array::<4>(context)?))
    }

    /// Reads a little-endian `i32`.
    pub fn read_i32_le(&mut self, context: &'static str) -> CdrResult<i32> {
        Ok(i32::from_le_bytes(self.read_array::<4>(context)?))
    }

    /// Reads a big-endian `i32`.
    pub fn read_i32_be(&mut self, context: &'static str) -> CdrResult<i32> {
        Ok(i32::from_be_bytes(self.read_array::<4>(context)?))
    }

    /// Reads a little-endian `f32`.
    pub fn read_f32_le(&mut self, context: &'static str) -> CdrResult<f32> {
        Ok(f32::from_le_bytes(self.read_array::<4>(context)?))
    }

    /// Reads a big-endian `f32`.
    pub fn read_f32_be(&mut self, context: &'static str) -> CdrResult<f32> {
        Ok(f32::from_be_bytes(self.read_array::<4>(context)?))
    }

    /// Reads a little-endian `f64`.
    pub fn read_f64_le(&mut self, context: &'static str) -> CdrResult<f64> {
        Ok(f64::from_le_bytes(self.read_array::<8>(context)?))
    }

    /// Reads a big-endian `f64`.
    pub fn read_f64_be(&mut self, context: &'static str) -> CdrResult<f64> {
        Ok(f64::from_be_bytes(self.read_array::<8>(context)?))
    }

    /// Returns a slice of all unread remaining bytes and advances to EOF.
    #[must_use]
    pub fn read_rest(&mut self) -> &'a [u8] {
        let rest = self.data.get(self.pos..).unwrap_or(&[]);
        self.pos = self.data.len();
        rest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    #[test]
    fn zip_slip_sanitization_blocks_all_vectors() {
        assert_eq!(
            sanitize_zip_path(b"content/riffData.cdr").unwrap(),
            "content/riffData.cdr"
        );
        assert_eq!(sanitize_zip_path(b"preview/").unwrap(), "preview/");
        assert!(sanitize_zip_path(b"../../etc/passwd").is_err());
        assert!(sanitize_zip_path(b"content/../../../secret").is_err());
        assert!(sanitize_zip_path(b"/etc/passwd").is_err());
        assert!(sanitize_zip_path(b"content\\evil.cdr").is_err());
        assert!(sanitize_zip_path(b"C:/Windows/System32").is_err());
        assert!(sanitize_zip_path(b"content/null\0byte").is_err());
        assert!(sanitize_zip_path(b"").is_err());
        assert!(sanitize_zip_path(b"./relative").is_err());
        assert!(sanitize_zip_path(b"a//b").is_err());
    }

    #[test]
    fn bounded_zlib_rejects_zip_bomb_without_oom() {
        let zeros = vec![0u8; 512 * 1024];
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
        enc.write_all(&zeros).unwrap();
        let compressed = enc.finish().unwrap();
        assert!(compressed.len() < 2048);

        let limits = CodecLimits::default();
        let mut budget = limits.max_uncompressed_bytes;
        let err = decompress_zlib_bounded(&compressed, &limits, &mut budget).unwrap_err();
        assert!(matches!(err, CdrError::CompressionRatioExceeded { .. }));

        // Also verify hard byte limit rejects before allocating full payload
        let small_limits = CodecLimits {
            max_uncompressed_bytes: 8 * 1024,
            max_compression_ratio: 100_000,
            ..CodecLimits::default()
        };
        let mut small_budget = small_limits.max_uncompressed_bytes;
        let err2 =
            decompress_zlib_bounded(&compressed, &small_limits, &mut small_budget).unwrap_err();
        assert!(matches!(
            err2,
            CdrError::UncompressedSizeLimitExceeded { .. }
        ));
    }

    #[test]
    fn bitmap_dims_validation_blocks_huge_and_zero_sizes() {
        let limits = CodecLimits::default();
        assert_eq!(limits.validate_bitmap_dims(100, 100).unwrap(), 40_000);
        assert!(limits.validate_bitmap_dims(0, 10).is_err());
        assert!(limits.validate_bitmap_dims(10, 0).is_err());
        assert!(limits.validate_bitmap_dims(MAX_BITMAP_DIM + 1, 10).is_err());
        // 32768 x 32768 x 4 = 4 GiB > 512 MiB default budget
        assert!(limits
            .validate_bitmap_dims(MAX_BITMAP_DIM, MAX_BITMAP_DIM)
            .is_err());
    }
}
