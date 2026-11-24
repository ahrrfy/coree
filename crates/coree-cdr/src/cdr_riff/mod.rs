// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room RIFF CDR (`v3`–`v26`) parser and writer with `LIST cmpr` Zlib
//! support, `20B` reverse-ordered `loda` encoding + dual `20B`/`12B` decoding,
//! `.wi` wavelet + `bmpt` alpha mask codecs, full geometry/style/11-Lens/text/
//! bitmap fidelity, CP1251/CP1256 legacy decoding, and lossless `UnknownChunkMap`.

mod dib;
mod fild_outl;
mod legacy_v3_v5;
mod loda;
mod parser;
mod parser_objects;
mod text_style;
mod wavelet_wi;
mod writer;

#[cfg(test)]
mod tests;

use crate::error::{CdrError, CdrResult};
use crate::limits::CodecLimits;
use crate::riff::{parse_riff_with_budget, RiffDocument};
use crate::zcf::ZcfContext;
use coree_core::Document;
use std::collections::HashMap;

pub use dib::{
    decode_dib_to_rgba, decode_dib_to_rgba_with_budget, encode_bmp_file_rgba, encode_dib_rgba,
};
#[allow(unused_imports)]
pub use legacy_v3_v5::{cdr16_to_mm, is_legacy_v3_v5, mm_to_cdr16};
#[allow(unused_imports)]
pub use wavelet_wi::{decode_wi_to_rgba_with_budget, encode_wi_rgba, is_wi_bitmap};

pub(crate) use loda::{decode_curve_points, encode_curve_points};
pub(crate) use parser::{parse_cdr_riff_tree, GuidPayload, StlyPayload};
pub(crate) use text_style::{
    decode_blend_mode, encode_blend_mode, encode_legacy_cp1256_txsm, encode_stlt_chunk,
};
pub(crate) use writer::render_doc_mini_thumbnail_rgba;

/// Options for writing RIFF CDR streams.
#[derive(Debug, Clone, Copy)]
pub struct RiffWriteOptions {
    /// Four-byte RIFF form type (e.g. `*b"CDRC"` for v12, `*b"CDRD"` for X3, `*b"CDTC"` for template).
    pub form_type: [u8; 4],
    /// Version number written into the `vrsn` chunk (e.g. `1200`, `1300`, `1400`, `1600`, `2400`).
    pub version: u16,
    /// Whether to wrap `LIST doc ` inside a CDR `LIST cmpr` Zlib + `CPng` block.
    pub compress_doc: bool,
    /// When `true`, emit embedded bitmaps as external references (`bmp ` omitted from RIFF stream
    /// and returned via [`RiffWriteOutput::external_bitmaps`] for CDR X6+ `content/data/*.dat`).
    pub externalize_bitmaps: bool,
}

impl Default for RiffWriteOptions {
    fn default() -> Self {
        Self {
            form_type: *b"CDRC",
            version: 1200,
            compress_doc: true,
            externalize_bitmaps: false,
        }
    }
}

/// Result of parsing a RIFF CDR/CDT stream.
#[derive(Debug, Clone)]
pub struct ParsedRiffCdr {
    pub document: Document,
    pub version: u16,
    pub is_template: bool,
    pub warnings: Vec<String>,
    pub riff_tree: RiffDocument,
}

/// Output from [`write_cdr_riff_with_options`], optionally separating externalized raster payloads
/// for CDR X6+ ZIP containers.
#[derive(Debug, Clone)]
pub struct RiffWriteOutput {
    pub riff_bytes: Vec<u8>,
    /// Pairs of `(external_id, raw_bmp_chunk_payload)` when `externalize_bitmaps` is `true`.
    pub external_bitmaps: Vec<(u32, Vec<u8>)>,
}

/// Returns `true` if `form` is a RIFF CDR form type (`CDR3`..`CDRZ`, `CDR `).
#[must_use]
pub fn is_cdr_form_type(form: &[u8; 4]) -> bool {
    &form[0..3] == b"CDR" && (form[3].is_ascii_alphanumeric() || form[3] == b' ')
}

/// Returns `true` if `form` is a CDR Template RIFF form type (`CDT4`..`CDTZ`, `CDT `).
#[must_use]
pub fn is_cdt_form_type(form: &[u8; 4]) -> bool {
    &form[0..3] == b"CDT" && (form[3].is_ascii_alphanumeric() || form[3] == b' ')
}

/// Derives a numeric CDR version (e.g. `1200` for `CDRC`, `1300` for `CDRD`) from a form FourCC.
#[must_use]
pub fn version_from_form_type(form: &[u8; 4]) -> u16 {
    let suffix = form[3];
    match suffix {
        b'3'..=b'9' => u16::from(suffix - b'0') * 100,
        b'A'..=b'Z' => (10u16 + u16::from(suffix - b'A')) * 100,
        _ => 1200,
    }
}

/// Parses a standalone RIFF CDR/CDT byte stream into a [`ParsedRiffCdr`].
pub fn parse_cdr_riff(
    bytes: &[u8],
    limits: &CodecLimits,
    external_bitmaps: &HashMap<u32, Vec<u8>>,
) -> CdrResult<ParsedRiffCdr> {
    let mut budget = limits.max_uncompressed_bytes;
    parse_cdr_riff_with_budget(bytes, limits, external_bitmaps, &mut budget)
}

/// Parses a standalone RIFF CDR/CDT byte stream using a shared decompression budget.
pub fn parse_cdr_riff_with_budget(
    bytes: &[u8],
    limits: &CodecLimits,
    external_bitmaps: &HashMap<u32, Vec<u8>>,
    budget_remaining: &mut u64,
) -> CdrResult<ParsedRiffCdr> {
    if bytes.len() < 12 {
        return Err(CdrError::UnexpectedEof {
            available: bytes.len(),
            needed: 12,
            context: "RIFF CDR header",
        });
    }
    let riff_tree = parse_riff_with_budget(bytes, limits, budget_remaining, true)?;
    let empty_zcf = ZcfContext::default();
    parse_cdr_riff_tree(
        riff_tree,
        limits,
        external_bitmaps,
        budget_remaining,
        &empty_zcf,
    )
}

/// Serializes a [`Document`] into a standard RIFF CDR v12 (`CDRC`, `LIST cmpr`) byte stream.
pub fn write_cdr_riff(doc: &Document) -> CdrResult<Vec<u8>> {
    let out = write_cdr_riff_with_options(doc, RiffWriteOptions::default())?;
    Ok(out.riff_bytes)
}

/// Serializes a [`Document`] into a RIFF CDR/CDT stream with configurable form, compression,
/// and externalized bitmap handling.
pub fn write_cdr_riff_with_options(
    doc: &Document,
    opts: RiffWriteOptions,
) -> CdrResult<RiffWriteOutput> {
    writer::write_cdr_riff_with_options_inner(doc, opts)
}
