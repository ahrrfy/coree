// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Clean-room 100% Pure-Rust `.cdr`, `.cdt`, `.cmx`, `.cpt` format
//! engine with hardened security limits, Arabic CP1256 legacy text decoding,
//! Unknown-Chunk Passthrough, and the CDR Structural Inspector.
//!
//! `coree-cdr` is an independent clean-room open-source implementation with zero
//! proprietary code or C/C++ dependencies.

pub mod cdr_riff;
pub mod cdr_zip;
pub mod cdt;
pub mod cmx;
pub mod coord;
pub mod cpt;
pub mod error;
pub mod fixtures;
pub mod inspector;
pub mod limits;
pub mod riff;
pub mod zcf;

use crate::cdr_riff::{
    is_cdr_form_type, is_cdt_form_type, parse_cdr_riff_with_options, write_cdr_riff,
    RiffWriteOptions,
};
use crate::cdr_zip::{is_zip_bytes, parse_cdr_zip_with_options, write_cdr_zip, CdrZipEra};
use crate::cmx::{import_cmx_bytes_with_options, is_cmx_form_type};
use crate::cpt::{detect_cpt_version, import_cpt_bytes};
use coree_core::Document;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub use cdt::{export_cdt, export_cdt_bytes, import_cdt_bytes};
pub use cmx::{export_cmx, export_cmx_bytes, export_cmx_bytes_with_endian, CmxPrecision};
pub use error::{CdrError, CdrResult};
pub use fixtures::generate_sample_document;
pub use inspector::{
    format_hex_ascii_dump, format_report, inspect_cdr_bytes, inspect_cdr_bytes_with_limits,
    inspect_cdr_file, inspect_cdr_file_with_limits, CdrInspectionReport, ChunkSummary,
    UnknownChunkSummary, ZipEntrySummary,
};
pub use limits::{
    read_file_bounded, CodecLimits, MAX_BITMAP_DIM, MAX_CHUNKS, MAX_COMPRESSION_RATIO,
    MAX_TREE_DEPTH, MAX_UNCOMPRESSED_BYTES, MAX_ZIP_ENTRIES,
};
pub use zcf::extract_cdr_preview_png;

/// Crate identifier used during workspace initialization and diagnostics.
pub const CRATE_NAME: &str = "coree-cdr";

/// Detected CDR/CDT/CMX/CPT binary container format and version era.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CdrFormat {
    /// Classic RIFF `.cdr` (v3–v13 / X3).
    CdrRiff { version: u16 },
    /// ZIP/OPC `.cdr` (X4–X5 with `content/riffData.cdr`).
    CdrZipX4,
    /// ZIP/OPC `.cdr` (X6–2025 with `content/root.dat` + `content/data/*.dat`).
    CdrZipX6,
    /// Classic RIFF `.cdt` template.
    CdtRiff { version: u16 },
    /// ZIP/OPC `.cdt` template.
    CdtZip,
    /// 16-bit Presentation Exchange (`.cmx`).
    Cmx16,
    /// 32-bit Presentation Exchange (`.cmx`).
    Cmx32,
    /// Multi-layer raster image container (`.cpt`).
    Cpt { version: u8 },
}

/// Target format version when exporting `.cdr` or `.cdt` files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CdrWriteVersion {
    /// RIFF CDR v12 (`CDRC` + `LIST cmpr` Zlib compression).
    RiffV12,
    /// ZIP/OPC CDR X4–X5 (`content/riffData.cdr`).
    ZipX4,
    /// ZIP/OPC CDR X6–2025 (`content/root.dat` + `content/data/*.dat`).
    #[default]
    ZipX6,
}

/// Import options for CDR/CDT/CMX container decoding (`S15` codepage override).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CdrReadOptions {
    /// Fallback Windows/UTF codepage (`1256` = Arabic, `1251` = Cyrillic, `1252` = Latin-1,
    /// `65001` = UTF-8) used when a legacy chunk omits or zeroes its codepage field.
    pub default_codepage: Option<u16>,
}

/// Result returned by unified CDR/CDT/CMX/CPT file importers.
#[derive(Debug, Clone)]
pub struct ImportReport {
    pub document: Document,
    pub detected_format: CdrFormat,
    pub warnings: Vec<String>,
}

/// Auto-detects the container format from raw file bytes (enforcing security checks
/// on ZIP archives).
pub fn detect_format(bytes: &[u8]) -> CdrResult<CdrFormat> {
    detect_format_with_limits(bytes, &CodecLimits::default())
}

/// Auto-detects the container format under explicit [`CodecLimits`], validating full
/// container integrity (`vrsn`, `tmpt`, ZIP/RIFF/CMX/CPT structure) so results always
/// match [`import_cdr_bytes_with_limits`].
pub fn detect_format_with_limits(bytes: &[u8], limits: &CodecLimits) -> CdrResult<CdrFormat> {
    import_cdr_bytes_with_limits(bytes, limits).map(|report| report.detected_format)
}

/// Imports any supported format (`.cdr` RIFF/X4/X6+, `.cdt`, `.cmx` 16/32-bit, `.cpt`)
/// from raw bytes using default [`CodecLimits`].
pub fn import_cdr_bytes(bytes: &[u8]) -> CdrResult<ImportReport> {
    import_cdr_bytes_with_limits(bytes, &CodecLimits::default())
}

/// Imports any supported container format from raw bytes under explicit [`CodecLimits`].
pub fn import_cdr_bytes_with_limits(bytes: &[u8], limits: &CodecLimits) -> CdrResult<ImportReport> {
    import_cdr_bytes_with_options(bytes, limits, CdrReadOptions::default())
}

/// Imports any supported container format from raw bytes under explicit [`CodecLimits`] and
/// [`CdrReadOptions`] (e.g. legacy single-byte codepage fallback).
pub fn import_cdr_bytes_with_options(
    bytes: &[u8],
    limits: &CodecLimits,
    opts: CdrReadOptions,
) -> CdrResult<ImportReport> {
    if is_zip_bytes(bytes) {
        let parsed = parse_cdr_zip_with_options(bytes, limits, opts)?;
        let detected_format = if parsed.is_template {
            CdrFormat::CdtZip
        } else {
            match parsed.era {
                CdrZipEra::X4 => CdrFormat::CdrZipX4,
                CdrZipEra::X6 => CdrFormat::CdrZipX6,
            }
        };
        return Ok(ImportReport {
            document: parsed.document,
            detected_format,
            warnings: parsed.warnings,
        });
    }

    if detect_cpt_version(bytes).is_some() {
        return import_cpt_bytes(bytes, limits);
    }

    if bytes.len() >= 12 && (&bytes[0..4] == b"RIFF" || &bytes[0..4] == b"RIFX") {
        let form: [u8; 4] = [bytes[8], bytes[9], bytes[10], bytes[11]];
        if is_cmx_form_type(&form) {
            return import_cmx_bytes_with_options(bytes, limits, opts);
        }
        if is_cdr_form_type(&form) || is_cdt_form_type(&form) {
            let parsed = parse_cdr_riff_with_options(bytes, limits, &HashMap::new(), opts)?;
            let detected_format = if parsed.is_template {
                CdrFormat::CdtRiff {
                    version: parsed.version,
                }
            } else {
                CdrFormat::CdrRiff {
                    version: parsed.version,
                }
            };
            return Ok(ImportReport {
                document: parsed.document,
                detected_format,
                warnings: parsed.warnings,
            });
        }
    }

    Err(CdrError::UnsupportedFormat(
        "input bytes do not match any supported .cdr/.cdt/.cmx/.cpt format".to_owned(),
    ))
}

/// Imports any supported `.cdr`/`.cdt`/`.cmx`/`.cpt` file from disk using default [`CodecLimits`].
pub fn import_cdr_file(path: &Path) -> CdrResult<ImportReport> {
    import_cdr_file_with_limits(path, &CodecLimits::default())
}

/// Imports any supported `.cdr`/`.cdt`/`.cmx`/`.cpt` file from disk under explicit [`CodecLimits`],
/// enforcing bounded disk reads before parsing.
pub fn import_cdr_file_with_limits(path: &Path, limits: &CodecLimits) -> CdrResult<ImportReport> {
    import_cdr_file_with_options(path, limits, CdrReadOptions::default())
}

/// Imports any supported `.cdr`/`.cdt`/`.cmx`/`.cpt` file from disk under explicit [`CodecLimits`]
/// and [`CdrReadOptions`].
pub fn import_cdr_file_with_options(
    path: &Path,
    limits: &CodecLimits,
    opts: CdrReadOptions,
) -> CdrResult<ImportReport> {
    let bytes = read_file_bounded(path, limits)?;
    import_cdr_bytes_with_options(&bytes, limits, opts)
}

/// Exports `doc` to `.cdr` bytes in the requested [`CdrWriteVersion`].
pub fn export_cdr_bytes(doc: &Document, version: CdrWriteVersion) -> CdrResult<Vec<u8>> {
    match version {
        CdrWriteVersion::RiffV12 => write_cdr_riff(doc),
        CdrWriteVersion::ZipX4 => write_cdr_zip(doc, CdrZipEra::X4, false),
        CdrWriteVersion::ZipX6 => write_cdr_zip(doc, CdrZipEra::X6, false),
    }
}

/// Exports `doc` to `path` as a `.cdr` file in the requested [`CdrWriteVersion`].
pub fn export_cdr(doc: &Document, path: &Path, version: CdrWriteVersion) -> CdrResult<()> {
    let bytes = export_cdr_bytes(doc, version)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Exports `doc` as an uncompressed RIFF CDR v12 stream (convenience helper for inspection/tools).
pub fn export_cdr_riff_uncompressed(doc: &Document) -> CdrResult<Vec<u8>> {
    Ok(cdr_riff::write_cdr_riff_with_options(
        doc,
        RiffWriteOptions {
            form_type: *b"CDRC",
            version: 1200,
            compress_doc: false,
            externalize_bitmaps: false,
        },
    )?
    .riff_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_metadata_is_valid() {
        assert_eq!(CRATE_NAME, "coree-cdr");
    }
}
