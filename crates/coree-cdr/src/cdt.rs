// SPDX-License-Identifier: MPL-2.0

//! Clean-room CorelDRAW Template (`.cdt`) reader and writer supporting both
//! RIFF (`CDT*`) and ZIP/OPC (`X4–X5` / `X6+`) template containers.

use crate::cdr_riff::{
    is_cdt_form_type, parse_cdr_riff, write_cdr_riff_with_options, RiffWriteOptions,
};
use crate::cdr_zip::{is_zip_bytes, parse_cdr_zip, write_cdr_zip, CdrZipEra};
use crate::error::{CdrError, CdrResult};
use crate::limits::CodecLimits;
use crate::{CdrWriteVersion, CorelFormat, ImportReport};
use coree_core::Document;
use std::collections::HashMap;
use std::path::Path;

/// Imports a CorelDRAW Template (`.cdt`) from raw bytes (either RIFF or ZIP container).
pub fn import_cdt_bytes(bytes: &[u8], limits: &CodecLimits) -> CdrResult<ImportReport> {
    if is_zip_bytes(bytes) {
        let parsed = parse_cdr_zip(bytes, limits)?;
        let mut doc = parsed.document;
        if doc.metadata.source_format.is_none() {
            doc.metadata.source_format = Some("CDT ZIP".to_owned());
        }
        Ok(ImportReport {
            document: doc,
            detected_format: CorelFormat::CdtZip,
            warnings: parsed.warnings,
        })
    } else {
        let parsed = parse_cdr_riff(bytes, limits, &HashMap::new())?;
        if !parsed.is_template && !is_cdt_form_type(&parsed.riff_tree.form_type) {
            return Err(CdrError::UnsupportedFormat(
                "RIFF container is not marked as a .cdt template".to_owned(),
            ));
        }
        Ok(ImportReport {
            document: parsed.document,
            detected_format: CorelFormat::CdtRiff {
                version: parsed.version,
            },
            warnings: parsed.warnings,
        })
    }
}

/// Exports `doc` as a CorelDRAW Template (`.cdt`) in the selected container version.
pub fn export_cdt_bytes(doc: &Document, version: CdrWriteVersion) -> CdrResult<Vec<u8>> {
    match version {
        CdrWriteVersion::RiffV12 => {
            let out = write_cdr_riff_with_options(
                doc,
                RiffWriteOptions {
                    form_type: *b"CDTC",
                    version: 1200,
                    compress_doc: true,
                    externalize_bitmaps: false,
                },
            )?;
            Ok(out.riff_bytes)
        }
        CdrWriteVersion::ZipX4 => write_cdr_zip(doc, CdrZipEra::X4, true),
        CdrWriteVersion::ZipX6 => write_cdr_zip(doc, CdrZipEra::X6, true),
    }
}

/// Writes `doc` to `path` as a CorelDRAW Template (`.cdt`).
pub fn export_cdt(doc: &Document, path: &Path, version: CdrWriteVersion) -> CdrResult<()> {
    let bytes = export_cdt_bytes(doc, version)?;
    std::fs::write(path, bytes)?;
    Ok(())
}
