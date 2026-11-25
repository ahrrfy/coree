// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room bridge to `coree-cdr` container parsers, exporters, magic-byte
//! file detection, bounded thumbnail extraction, and structural inspection
//! utilities across `.cdr`, `.cdr.zip`, `.cdt`, `.cmx`, `.cpt`, `.coree`,
//! `.svg`, `.pdf`, `.ai`, `.eps`, `.dxf`, `.plt`, and `.psd`.

use coree_cdr::cdr_riff::{
    is_cdr_form_type, is_cdt_form_type, parse_cdr_riff, write_cdr_riff_with_options,
    RiffWriteOptions,
};
use coree_cdr::cdr_zip::{is_zip_bytes, parse_cdr_zip, write_cdr_zip, CdrZipEra};
use coree_cdr::cmx::{export_cmx, is_cmx_form_type, parse_cmx_bytes, CmxPrecision};
use coree_cdr::coord::color_model_name;
use coree_cdr::cpt::{detect_cpt_version, parse_cpt_bytes};
use coree_cdr::riff::{fourcc_to_string, RiffDocument, RiffNode};
use coree_cdr::{
    export_cdr, export_cdt, extract_cdr_preview_png, read_file_bounded, CdrError, CdrResult,
    CdrWriteVersion, ChunkSummary, CodecLimits, UnknownChunkSummary, ZipEntrySummary,
};
use coree_core::math::MM_PER_INCH;
use coree_core::{Document, NodePayload};
use coree_io::{
    decode_raster_bytes_with_limits, export_raster_to_bytes, IoLimits, RasterExportOptions,
    RasterFormat,
};
use coree_render::{
    resample, CanvasBackend, RenderOptions, SceneRenderer, TinySkiaBackend, ViewMode, Viewport,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

/// Maximum header bytes peeked for magic-byte format detection.
const MAGIC_PEEK_BYTES: usize = 4096;

/// Maximum allowed thumbnail edge dimension in pixels (4K safety ceiling).
pub const MAX_THUMBNAIL_SIZE_PX: u32 = 4096;

/// Standard 8-byte PNG signature.
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Detected container format and version era for vector/raster binary files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContainerFormat {
    CdrRiff { version: u16 },
    CdrZipX4,
    CdrZipX6,
    CdtRiff { version: u16 },
    CdtZip,
    Cmx16,
    Cmx32,
    Cpt { version: u8 },
    NativeCoree,
    Svg,
    Pdf,
    Ai,
    Eps,
    Dxf,
    Plt,
    Psd,
    Dwg,
    Wmf,
    Emf,
    Acb,
}

impl ContainerFormat {
    /// Short lowercase file-format tag matching `coree-storage` schema constraints.
    #[must_use]
    pub const fn storage_tag(self) -> &'static str {
        match self {
            Self::CdrRiff { .. } | Self::CdrZipX4 | Self::CdrZipX6 => "cdr",
            Self::CdtRiff { .. } | Self::CdtZip => "cdt",
            Self::Cmx16 | Self::Cmx32 => "cmx",
            Self::Cpt { .. } => "cpt",
            Self::NativeCoree => "coree",
            Self::Svg => "svg",
            Self::Pdf | Self::Ai => "pdf",
            Self::Eps => "eps",
            Self::Dxf => "dxf",
            Self::Plt => "plt",
            Self::Psd => "psd",
            Self::Dwg => "dwg",
            Self::Wmf => "wmf",
            Self::Emf => "emf",
            Self::Acb => "acb",
        }
    }
}

/// Magic-byte format classification produced before falling back to file extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MagicKind {
    /// ZIP archive (`PK\x03\x04` / `PK\x05\x06` / `PK\x07\x08`): `.coree`, `.cdr`, `.cdr.zip`, `.cdt`, `.cdt.zip`.
    Zip,
    /// RIFF/RIFX vector container (`CDR*`, `CDT*`, `CMX*`).
    RiffCdr,
    /// Multi-layer CPT raster container (`CPT7`/`CPT8`/`CPT9`).
    Cpt,
    /// PDF 1.0–2.0 or PDF-compatible Adobe Illustrator (`%PDF-` within first 1024 bytes).
    Pdf,
    /// Adobe Photoshop `.psd` / `.psb` (`8BPS`).
    Psd,
    /// PostScript / Encapsulated PostScript (`%!PS` or DOS EPS binary `C5 D0 D3 C6`).
    Eps,
    /// SVG XML (`<svg` / `<!DOCTYPE svg`) or gzipped `.svgz` (`1F 8B`).
    Svg,
    /// Raster image (`PNG`, `JPEG`, `WebP`, `TIFF`, `BMP`) carrying its storage tag.
    Raster(&'static str),
    /// AutoCAD ASCII DXF (`0\nSECTION` / `999\n...`).
    Dxf,
    /// AutoCAD Binary DWG (`AC1012`..`AC1032`).
    Dwg,
    /// Windows Metafile (`.wmf`, Aldus Placeable `D7 CD C6 9A`).
    Wmf,
    /// Enhanced Metafile (`.emf`, `[MS-EMF]` signature ` EMF`).
    Emf,
    /// Adobe Color Book (`.acb`, signature `8BCB`).
    Acb,
    /// HPGL cutting plotter stream (`IN;`, `SP`, `PU`, `PD`, `PA`, etc.).
    Plt,
    /// No unambiguous magic signature detected; caller may inspect file extension.
    Unknown,
}

/// Peeks at most [`MAGIC_PEEK_BYTES`] from `path` without loading the whole file into memory.
pub(crate) fn peek_file_header(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("failed to open '{}': {e}", path.display()))?;
    let mut header = Vec::with_capacity(512);
    file.take(MAGIC_PEEK_BYTES as u64)
        .read_to_end(&mut header)
        .map_err(|e| format!("failed to read header of '{}': {e}", path.display()))?;
    Ok(header)
}

/// Classifies a file header slice purely from magic bytes and text headers.
#[must_use]
pub(crate) fn classify_magic_bytes(header: &[u8]) -> MagicKind {
    // 1. ZIP containers (`.coree`, `.cdr`, `.cdr.zip`, `.cdt`, `.cdt.zip`)
    if header.len() >= 4
        && (&header[0..4] == b"PK\x03\x04"
            || &header[0..4] == b"PK\x05\x06"
            || &header[0..4] == b"PK\x07\x08")
    {
        return MagicKind::Zip;
    }

    // 2. RIFF / RIFX (CDR/CDT/CMX vector containers vs WebP raster)
    if header.len() >= 12 && (&header[0..4] == b"RIFF" || &header[0..4] == b"RIFX") {
        let form = [header[8], header[9], header[10], header[11]];
        if &form == b"WEBP" {
            return MagicKind::Raster("png");
        }
        if is_cdr_form_type(&form) || is_cdt_form_type(&form) || is_cmx_form_type(&form) {
            return MagicKind::RiffCdr;
        }
    }

    // 3. CPT7 / CPT8 / CPT9 multi-layer raster
    if detect_cpt_version(header).is_some()
        || (header.len() >= 4
            && (&header[0..4] == b"CPT7" || &header[0..4] == b"CPT8" || &header[0..4] == b"CPT9"))
    {
        return MagicKind::Cpt;
    }

    // 4. Exact-prefix PDF / PDF-compatible `.ai` (`%PDF-`)
    if header.starts_with(b"%PDF-") {
        return MagicKind::Pdf;
    }

    // 5. Photoshop PSD (version 1) / PSB (version 2)
    if header.len() >= 6
        && &header[0..4] == b"8BPS"
        && (header[4..6] == [0, 1] || header[4..6] == [0, 2])
    {
        return MagicKind::Psd;
    }

    // 5b. Adobe Color Book (`.acb`, magic `8BCB`)
    if header.len() >= 4 && &header[0..4] == b"8BCB" {
        return MagicKind::Acb;
    }

    // 6. DOS EPS Binary (`C5 D0 D3 C6`) or ASCII PostScript/EPS (`%!PS-Adobe` / `%!PS`)
    // Checked before the 1024-byte offset `%PDF-` scan so EPS DSC comments mentioning `%PDF-`
    // are never misclassified as PDF.
    if header.len() >= 4 && header[0..4] == [0xC5, 0xD0, 0xD3, 0xC6] {
        return MagicKind::Eps;
    }
    let trimmed = strip_bom_and_leading_ws(header);
    if trimmed.starts_with(b"%!PS") {
        return MagicKind::Eps;
    }

    // 7. PDF with leading prelude bytes (`%PDF-` anywhere within first 1024 bytes per PDF 1.7 §7.5.1)
    let pdf_scan = &header[..header.len().min(1024)];
    if pdf_scan.windows(5).any(|w| w == b"%PDF-") {
        return MagicKind::Pdf;
    }

    // 8. GZIP compressed `.svgz`
    if header.len() >= 2 && header[0] == 0x1F && header[1] == 0x8B {
        return MagicKind::Svg;
    }

    // 9. Standard raster signatures (PNG, JPEG, TIFF, BMP)
    if header.len() >= 8 && &header[0..8] == PNG_SIGNATURE {
        return MagicKind::Raster("png");
    }
    if header.len() >= 3 && header[0..3] == [0xFF, 0xD8, 0xFF] {
        return MagicKind::Raster("jpg");
    }
    if header.len() >= 4 && (&header[0..4] == b"II*\x00" || &header[0..4] == b"MM\x00*") {
        return MagicKind::Raster("tiff");
    }
    if is_bmp_header(header) {
        return MagicKind::Raster("png");
    }

    // 10. XML / SVG text signature (must begin with `<` after optional BOM/whitespace)
    if looks_like_svg(trimmed) {
        return MagicKind::Svg;
    }

    // 10b. Binary DWG (`AC1012`..`AC1032`), Aldus Placeable WMF (`D7 CD C6 9A`), EMF (` EMF`)
    if header.len() >= 6 && coree_io::DwgVersion::from_magic(&header[0..6]).is_some() {
        return MagicKind::Dwg;
    }
    if header.len() >= 4 && header[0..4] == [0xD7, 0xCD, 0xC6, 0x9A] {
        return MagicKind::Wmf;
    }
    if header.len() >= 44
        && u32::from_le_bytes([header[0], header[1], header[2], header[3]]) == 1
        && &header[40..44] == b" EMF"
    {
        return MagicKind::Emf;
    }

    // 11. ASCII DXF (`0` / `SECTION` or `999` comment + `0` / `SECTION`)
    if looks_like_dxf(trimmed) {
        return MagicKind::Dxf;
    }

    // 12. HPGL / PLT plotter stream (`IN;`, `SP1;`, `PU`, `PD`, `PA`, etc.)
    if looks_like_plt(trimmed) {
        return MagicKind::Plt;
    }

    MagicKind::Unknown
}

fn strip_bom_and_leading_ws(bytes: &[u8]) -> &[u8] {
    let mut s = bytes;
    if let Some(rest) = s.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        s = rest;
    }
    while let Some((&first, rest)) = s.split_first() {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}

fn is_bmp_header(header: &[u8]) -> bool {
    if header.len() < 18 || &header[0..2] != b"BM" {
        return false;
    }
    // Reserved bytes 6..10 are zero in valid BMP files, and DIB header size at 14..18 is standard.
    let reserved = u32::from_le_bytes([header[6], header[7], header[8], header[9]]);
    let dib_size = u32::from_le_bytes([header[14], header[15], header[16], header[17]]);
    reserved == 0 && matches!(dib_size, 12 | 40 | 52 | 56 | 108 | 124)
}

fn looks_like_svg(trimmed: &[u8]) -> bool {
    if !trimmed.starts_with(b"<") {
        return false;
    }
    let Ok(text) = std::str::from_utf8(trimmed) else {
        // Fallback: lossy ASCII scan in case peek cut inside a multi-byte UTF-8 char at the end.
        let lossy = String::from_utf8_lossy(trimmed);
        return contains_svg_marker(&lossy);
    };
    contains_svg_marker(text)
}

fn contains_svg_marker(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("<!doctype svg") {
        return true;
    }
    let bytes = lower.as_bytes();
    let mut pos = 0usize;
    while let Some(rel) = lower[pos..].find("<svg") {
        let abs = pos + rel;
        let after = abs + 4;
        if after >= bytes.len()
            || bytes[after].is_ascii_whitespace()
            || matches!(bytes[after], b'>' | b'/' | b':')
        {
            return true;
        }
        pos = after;
    }
    false
}

fn looks_like_dxf(trimmed: &[u8]) -> bool {
    let text = String::from_utf8_lossy(trimmed);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else {
        return false;
    };
    let Some(second) = lines.next() else {
        return false;
    };
    if first == "0" && second.eq_ignore_ascii_case("SECTION") {
        return true;
    }
    if first == "999" {
        let mut prev = second;
        for line in lines.take(24) {
            if prev == "0" && line.eq_ignore_ascii_case("SECTION") {
                return true;
            }
            prev = line;
        }
    }
    false
}

fn looks_like_plt(trimmed: &[u8]) -> bool {
    let mut s = trimmed;
    // Skip optional ESC plotter handshake sequences (e.g., `\x1b.(;`)
    while s.first() == Some(&0x1B) {
        if let Some(pos) = s.iter().position(|&b| b == b';' || b == b':' || b == b'\n') {
            s = strip_bom_and_leading_ws(&s[pos + 1..]);
        } else {
            return false;
        }
    }
    if s.len() < 3 {
        return false;
    }
    let cmd = [s[0].to_ascii_uppercase(), s[1].to_ascii_uppercase()];
    let is_hpgl_cmd = matches!(
        &cmd,
        b"IN" | b"DF" | b"SP" | b"PU" | b"PD" | b"PA" | b"PR" | b"IP" | b"SC" | b"VS" | b"PW"
    );
    if !is_hpgl_cmd {
        return false;
    }
    let next = s[2];
    let has_terminator = s.contains(&b';');
    has_terminator
        && (next == b';'
            || next.is_ascii_digit()
            || next.is_ascii_whitespace()
            || matches!(next, b'-' | b'+' | b'S' | b'P' | b'I' | b'D'))
}

/// Returns `true` if `path` is a `.coree` ZIP archive (fast manifest check).
pub(crate) fn is_coree_archive(path: &Path) -> bool {
    coree_io::read_coree_manifest(path).is_ok()
}

/// Extracts the lowercased file extension (empty string if none).
pub(crate) fn lower_extension(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// Reads PNG `(width, height)` directly from a valid PNG byte slice's `IHDR` chunk.
#[must_use]
pub fn read_png_dimensions(png_bytes: &[u8]) -> Option<(u32, u32)> {
    if png_bytes.len() < 24 || &png_bytes[0..8] != PNG_SIGNATURE || &png_bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([png_bytes[16], png_bytes[17], png_bytes[18], png_bytes[19]]);
    let h = u32::from_be_bytes([png_bytes[20], png_bytes[21], png_bytes[22], png_bytes[23]]);
    (w > 0 && h > 0).then_some((w, h))
}

/// Complete structural inspection report for `.cdr`, `.cdr.zip`, `.cdt`, `.cmx`,
/// `.cpt`, `.coree`, `.svg`, `.pdf`, `.ai`, `.eps`, `.dxf`, `.plt`, and `.psd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInspectionReport {
    pub detected_format: ContainerFormat,
    pub format_era: String,
    pub file_size_bytes: u64,
    pub version_number: Option<u32>,
    pub chunks: Vec<ChunkSummary>,
    pub zip_entries: Vec<ZipEntrySummary>,
    pub page_count: usize,
    pub layer_count: usize,
    pub object_count: usize,
    pub bitmap_count: usize,
    pub fonts: Vec<String>,
    pub color_models: Vec<String>,
    pub unknown_chunks: Vec<UnknownChunkSummary>,
    pub warnings: Vec<String>,
}

impl FileInspectionReport {
    /// Formats the inspection report into a readable multi-line CLI/panel summary.
    #[must_use]
    pub fn format_human(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "=== coree Structural Inspector ===");
        let _ = writeln!(
            out,
            "Format:      {} ({:?})",
            self.format_era, self.detected_format
        );
        let _ = writeln!(out, "File Size:   {} bytes", self.file_size_bytes);
        if let Some(v) = self.version_number {
            let _ = writeln!(out, "Version:     {v}");
        }
        let _ = writeln!(
            out,
            "Scene Graph: {} page(s), {} layer(s), {} object(s), {} raster asset(s)",
            self.page_count, self.layer_count, self.object_count, self.bitmap_count
        );
        let _ = writeln!(
            out,
            "Fonts ({}):  {}",
            self.fonts.len(),
            if self.fonts.is_empty() {
                "(none)".to_owned()
            } else {
                self.fonts.join(", ")
            }
        );
        let _ = writeln!(
            out,
            "Colors ({}): {}",
            self.color_models.len(),
            if self.color_models.is_empty() {
                "(none)".to_owned()
            } else {
                self.color_models.join(", ")
            }
        );

        if !self.zip_entries.is_empty() {
            let _ = writeln!(out, "\nZIP Entries ({}):", self.zip_entries.len());
            for entry in &self.zip_entries {
                let _ = writeln!(
                    out,
                    "  - {} ({} -> {} bytes)",
                    entry.name, entry.compressed_size, entry.uncompressed_size
                );
            }
        }

        if !self.chunks.is_empty() {
            let _ = writeln!(out, "\nRIFF Chunk Tree ({}):", self.chunks.len());
            for ch in &self.chunks {
                let indent = "  ".repeat(ch.depth);
                let list_tag = if ch.is_list { "LIST " } else { "" };
                let cmpr_tag = if ch.compressed { " [cmpr]" } else { "" };
                let _ = writeln!(
                    out,
                    "{indent}{list_tag}'{}' ({} bytes){cmpr_tag}",
                    ch.fourcc, ch.size
                );
            }
        }

        if !self.unknown_chunks.is_empty() {
            let _ = writeln!(
                out,
                "\nUnknown Chunks Preserved ({}):",
                self.unknown_chunks.len()
            );
            for unk in &self.unknown_chunks {
                let _ = writeln!(
                    out,
                    "  - '{}' [{}] ({} bytes, hint v{})",
                    unk.fourcc, unk.scope, unk.byte_len, unk.version_hint
                );
            }
        }

        if !self.warnings.is_empty() {
            let _ = writeln!(out, "\nWarnings ({}):", self.warnings.len());
            for w in &self.warnings {
                let _ = writeln!(out, "  ! {w}");
            }
        }
        out
    }
}

/// Result returned when importing a `.cdr`/`.cdt`/`.cmx`/`.cpt` file.
#[derive(Debug, Clone)]
pub struct BinaryImportResult {
    pub document: Document,
    pub report: FileInspectionReport,
}

/// Imports and structurally inspects any `.cdr`, `.cdr.zip`, `.cdt`, `.cmx`, or `.cpt` binary file.
pub fn import_and_inspect_binary_file(path: &Path) -> CdrResult<BinaryImportResult> {
    let limits = CodecLimits::default();
    let bytes = read_file_bounded(path, &limits)?;
    import_and_inspect_binary_bytes(&bytes, &limits)
}

/// Imports and structurally inspects raw `.cdr`/`.cdt`/`.cmx`/`.cpt` bytes under `limits`.
pub fn import_and_inspect_binary_bytes(
    bytes: &[u8],
    limits: &CodecLimits,
) -> CdrResult<BinaryImportResult> {
    let file_size_bytes = bytes.len() as u64;

    // 1. ZIP/OPC CDR X4-X5, CDR X6+, or CDT ZIP
    if is_zip_bytes(bytes) {
        let parsed = parse_cdr_zip(bytes, limits)?;
        let (detected_format, format_era) = match (parsed.era, parsed.is_template) {
            (CdrZipEra::X4, false) => (ContainerFormat::CdrZipX4, "CDR ZIP/OPC X4-X5".to_owned()),
            (CdrZipEra::X6, false) => (ContainerFormat::CdrZipX6, "CDR ZIP/OPC X6-2025".to_owned()),
            (CdrZipEra::X4, true) => (ContainerFormat::CdtZip, "CDT Template ZIP X4-X5".to_owned()),
            (CdrZipEra::X6, true) => (
                ContainerFormat::CdtZip,
                "CDT Template ZIP X6-2025".to_owned(),
            ),
        };
        let chunks = summarize_riff_tree(&parsed.riff_tree);
        let zip_entries = parsed
            .zip_entries
            .into_iter()
            .map(|e| ZipEntrySummary {
                name: e.name,
                compressed_size: e.compressed_size,
                uncompressed_size: e.uncompressed_size,
            })
            .collect();
        let report = build_report_from_doc(
            &parsed.document,
            detected_format,
            format_era,
            file_size_bytes,
            Some(u32::from(parsed.version)),
            chunks,
            zip_entries,
            parsed.warnings,
        );
        return Ok(BinaryImportResult {
            document: parsed.document,
            report,
        });
    }

    // 2. Multi-layer CPT raster format
    if detect_cpt_version(bytes).is_some() {
        let parsed = parse_cpt_bytes(bytes, limits)?;
        let chunks = parsed
            .riff_tree
            .as_ref()
            .map(summarize_riff_tree)
            .unwrap_or_default();
        let era = format!(
            "CPT Raster v{} ({})",
            parsed.version,
            parsed.color_model.label()
        );
        let mut report = build_report_from_doc(
            &parsed.document,
            ContainerFormat::Cpt {
                version: parsed.version,
            },
            era,
            file_size_bytes,
            Some(u32::from(parsed.version)),
            chunks,
            Vec::new(),
            parsed.warnings,
        );
        if !report
            .color_models
            .iter()
            .any(|m| m == parsed.color_model.label())
        {
            report
                .color_models
                .push(parsed.color_model.label().to_owned());
        }
        return Ok(BinaryImportResult {
            document: parsed.document,
            report,
        });
    }

    // 3. RIFF / RIFX containers (CDR, CDT, CMX)
    if bytes.len() >= 12 && (&bytes[0..4] == b"RIFF" || &bytes[0..4] == b"RIFX") {
        let form: [u8; 4] = [bytes[8], bytes[9], bytes[10], bytes[11]];
        if is_cmx_form_type(&form) {
            let parsed = parse_cmx_bytes(bytes, limits)?;
            let (fmt, era) = match parsed.precision {
                CmxPrecision::Bit16 => (ContainerFormat::Cmx16, "CMX 16-bit Presentation Exchange"),
                CmxPrecision::Bit32 => (ContainerFormat::Cmx32, "CMX 32-bit Presentation Exchange"),
            };
            let chunks = summarize_riff_tree(&parsed.riff_tree);
            let report = build_report_from_doc(
                &parsed.document,
                fmt,
                era.to_owned(),
                file_size_bytes,
                None,
                chunks,
                Vec::new(),
                parsed.warnings,
            );
            return Ok(BinaryImportResult {
                document: parsed.document,
                report,
            });
        }
        if is_cdr_form_type(&form) || is_cdt_form_type(&form) {
            let parsed = parse_cdr_riff(bytes, limits, &HashMap::new())?;
            let (fmt, era) = if parsed.is_template {
                (
                    ContainerFormat::CdtRiff {
                        version: parsed.version,
                    },
                    format!("CDT RIFF Template v{}", parsed.version / 100),
                )
            } else {
                (
                    ContainerFormat::CdrRiff {
                        version: parsed.version,
                    },
                    format!(
                        "CDR RIFF v{} ({})",
                        parsed.version / 100,
                        fourcc_to_string(&form)
                    ),
                )
            };
            let chunks = summarize_riff_tree(&parsed.riff_tree);
            let report = build_report_from_doc(
                &parsed.document,
                fmt,
                era,
                file_size_bytes,
                Some(u32::from(parsed.version)),
                chunks,
                Vec::new(),
                parsed.warnings,
            );
            return Ok(BinaryImportResult {
                document: parsed.document,
                report,
            });
        }
    }

    Err(CdrError::UnsupportedFormat(
        "unrecognized file signature (expected RIFF CDR/CDT/CMX, ZIP CDR/CDT, or CPT)".to_owned(),
    ))
}

/// Inspects any supported vector/raster container file (`.cdr`, `.cdr.zip`, `.cdt`,
/// `.cmx`, `.cpt`, `.coree`, `.svg`, `.svgz`, `.pdf`, `.ai`, `.eps`, `.ps`, `.dxf`,
/// `.plt`, `.hpgl`, `.psd`, `.psb`) using magic-byte-first detection.
pub fn inspect_any_file(path: &Path) -> Result<FileInspectionReport, String> {
    let ext = lower_extension(path);
    let file_size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let header = peek_file_header(path)?;
    let magic = classify_magic_bytes(&header);

    // Magic-byte-first dispatch so `.cdr.zip`, `.cdt.zip`, extensionless, and misnamed files work reliably
    match magic {
        MagicKind::Zip => {
            if ext == "coree" {
                if let Ok(report) = inspect_coree_file(path, file_size_bytes) {
                    return Ok(report);
                }
                return import_and_inspect_binary_file(path)
                    .map(|res| res.report)
                    .map_err(|e| format!("inspection failed: {e}"));
            }
            if is_coree_archive(path) {
                return inspect_coree_file(path, file_size_bytes);
            }
            match import_and_inspect_binary_file(path) {
                Ok(res) => return Ok(res.report),
                Err(cdr_err) => {
                    if let Ok(report) = inspect_coree_file(path, file_size_bytes) {
                        return Ok(report);
                    }
                    return Err(format!("inspection failed: {cdr_err}"));
                }
            }
        }
        MagicKind::RiffCdr | MagicKind::Cpt => {
            return import_and_inspect_binary_file(path)
                .map(|res| res.report)
                .map_err(|e| format!("inspection failed: {e}"));
        }
        MagicKind::Pdf => {
            return inspect_pdf_or_ai_file(path, ext == "ai", file_size_bytes);
        }
        MagicKind::Psd => {
            return inspect_psd_file(path, file_size_bytes);
        }
        MagicKind::Eps => {
            return inspect_eps_file(path, ext == "ai", file_size_bytes);
        }
        MagicKind::Svg => {
            return inspect_svg_file(path, file_size_bytes);
        }
        MagicKind::Dxf => {
            return inspect_dxf_file(path, file_size_bytes);
        }
        MagicKind::Dwg => {
            return inspect_dwg_file(path, file_size_bytes);
        }
        MagicKind::Wmf => {
            return inspect_wmf_file(path, file_size_bytes);
        }
        MagicKind::Emf => {
            return inspect_emf_file(path, file_size_bytes);
        }
        MagicKind::Acb => {
            return inspect_acb_file(path, file_size_bytes);
        }
        MagicKind::Plt => {
            return inspect_plt_file(path, file_size_bytes);
        }
        MagicKind::Raster(_) | MagicKind::Unknown => {}
    }

    // Extension fallback (handles truncated/malformed inputs with clear format-specific diagnostics)
    match ext.as_str() {
        "coree" => inspect_coree_file(path, file_size_bytes),
        "svg" | "svgz" => inspect_svg_file(path, file_size_bytes),
        "pdf" => inspect_pdf_or_ai_file(path, false, file_size_bytes),
        "ai" => inspect_pdf_or_ai_file(path, true, file_size_bytes),
        "eps" | "ps" => inspect_eps_file(path, false, file_size_bytes),
        "dxf" => inspect_dxf_file(path, file_size_bytes),
        "dwg" => inspect_dwg_file(path, file_size_bytes),
        "wmf" => inspect_wmf_file(path, file_size_bytes),
        "emf" => inspect_emf_file(path, file_size_bytes),
        "acb" => inspect_acb_file(path, file_size_bytes),
        "plt" | "hpgl" => inspect_plt_file(path, file_size_bytes),
        "psd" | "psb" => inspect_psd_file(path, file_size_bytes),
        _ => import_and_inspect_binary_file(path)
            .map(|res| res.report)
            .map_err(|e| format!("inspection failed: {e}")),
    }
}

fn inspect_coree_file(path: &Path, file_size_bytes: u64) -> Result<FileInspectionReport, String> {
    let doc = coree_io::load_coree(path).map_err(|e| format!("failed to load .coree: {e}"))?;
    let manifest = coree_io::read_coree_manifest(path).ok();
    let ver = manifest.as_ref().map(|m| m.format_version);
    Ok(build_report_from_doc(
        &doc,
        ContainerFormat::NativeCoree,
        "Native .coree Archive".to_owned(),
        file_size_bytes,
        ver,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

fn inspect_svg_file(path: &Path, file_size_bytes: u64) -> Result<FileInspectionReport, String> {
    let doc = coree_io::import_svg(path).map_err(|e| format!("failed to load SVG: {e}"))?;
    Ok(build_report_from_doc(
        &doc,
        ContainerFormat::Svg,
        "Scalable Vector Graphics (SVG)".to_owned(),
        file_size_bytes,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

fn inspect_pdf_or_ai_file(
    path: &Path,
    is_ai: bool,
    file_size_bytes: u64,
) -> Result<FileInspectionReport, String> {
    let doc = coree_io::import_pdf(path).map_err(|e| format!("failed to load PDF/AI: {e}"))?;
    let (fmt, era) = if is_ai {
        (ContainerFormat::Ai, "Adobe Illustrator (PDF-Compatible)")
    } else {
        (ContainerFormat::Pdf, "Portable Document Format (PDF)")
    };
    Ok(build_report_from_doc(
        &doc,
        fmt,
        era.to_owned(),
        file_size_bytes,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

fn inspect_eps_file(
    path: &Path,
    is_ai: bool,
    file_size_bytes: u64,
) -> Result<FileInspectionReport, String> {
    let doc = coree_io::import_eps(path).map_err(|e| format!("failed to load EPS/PS: {e}"))?;
    let era = if is_ai {
        "Adobe Illustrator (EPS/PostScript)"
    } else {
        "Encapsulated PostScript (EPS/PS)"
    };
    Ok(build_report_from_doc(
        &doc,
        ContainerFormat::Eps,
        era.to_owned(),
        file_size_bytes,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

fn inspect_dxf_file(path: &Path, file_size_bytes: u64) -> Result<FileInspectionReport, String> {
    let doc = coree_io::import_dxf(path).map_err(|e| format!("failed to load DXF: {e}"))?;
    Ok(build_report_from_doc(
        &doc,
        ContainerFormat::Dxf,
        "AutoCAD Drawing Exchange Format (DXF)".to_owned(),
        file_size_bytes,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

fn inspect_plt_file(path: &Path, file_size_bytes: u64) -> Result<FileInspectionReport, String> {
    let doc = coree_io::import_plt(path).map_err(|e| format!("failed to load PLT/HPGL: {e}"))?;
    Ok(build_report_from_doc(
        &doc,
        ContainerFormat::Plt,
        "HPGL Cutting Plotter (PLT)".to_owned(),
        file_size_bytes,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

fn inspect_psd_file(path: &Path, file_size_bytes: u64) -> Result<FileInspectionReport, String> {
    let doc = coree_io::import_psd(path).map_err(|e| format!("failed to load PSD/PSB: {e}"))?;
    Ok(build_report_from_doc(
        &doc,
        ContainerFormat::Psd,
        "Photoshop Document (PSD/PSB)".to_owned(),
        file_size_bytes,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

fn inspect_dwg_file(path: &Path, file_size_bytes: u64) -> Result<FileInspectionReport, String> {
    let doc = coree_io::import_dwg(path).map_err(|e| format!("failed to load DWG: {e}"))?;
    Ok(build_report_from_doc(
        &doc,
        ContainerFormat::Dwg,
        "AutoCAD Binary Drawing (DWG)".to_owned(),
        file_size_bytes,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

fn inspect_wmf_file(path: &Path, file_size_bytes: u64) -> Result<FileInspectionReport, String> {
    let doc = coree_io::import_wmf(path).map_err(|e| format!("failed to load WMF: {e}"))?;
    Ok(build_report_from_doc(
        &doc,
        ContainerFormat::Wmf,
        "Windows Metafile (WMF)".to_owned(),
        file_size_bytes,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

fn inspect_emf_file(path: &Path, file_size_bytes: u64) -> Result<FileInspectionReport, String> {
    let doc = coree_io::import_emf(path).map_err(|e| format!("failed to load EMF: {e}"))?;
    Ok(build_report_from_doc(
        &doc,
        ContainerFormat::Emf,
        "Enhanced Metafile (EMF)".to_owned(),
        file_size_bytes,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

fn inspect_acb_file(path: &Path, file_size_bytes: u64) -> Result<FileInspectionReport, String> {
    const MAX_ACB_BYTES: u64 = 64 * 1024 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|e| format!("failed to open ACB '{}': {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_ACB_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("failed to read ACB '{}': {e}", path.display()))?;
    if (bytes.len() as u64) > MAX_ACB_BYTES {
        return Err(format!(
            "failed to load ACB: file exceeds {MAX_ACB_BYTES} bytes"
        ));
    }
    let palette = coree_io::import_acb(&bytes).map_err(|e| format!("failed to load ACB: {e}"))?;
    let swatch_count = palette.swatches.len();
    let mut doc = Document::new(
        &palette.name,
        210.0,
        297.0,
        coree_core::ColorMode::Cmyk,
        300.0,
    );
    *doc.document_palette_mut() = palette;
    Ok(build_report_from_doc(
        &doc,
        ContainerFormat::Acb,
        format!("Adobe Color Book (ACB, {swatch_count} swatches)"),
        file_size_bytes,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

/// Extracts or renders a bounded PNG thumbnail (both edges `<= max_size_px`) for any
/// supported file format.
///
/// Fast paths:
/// - `.cdr` / `.cdt` / `.cmx` / `.cdr.zip`: uses `coree_cdr::extract_cdr_preview_png` first,
///   downscaling cleanly with Catmull-Rom filtering if larger than `max_size_px`, and falling
///   back to opening and rendering Page 0 when no embedded preview is available.
/// - `.coree`: uses embedded `preview.png` first, downscaling cleanly if needed, else renders Page 0.
/// - `.svg` / `.pdf` / `.ai` / `.eps` / `.dxf` / `.plt` / `.psd` / rasters: opens and renders
///   Page 0 to an exact bounded straight-RGBA PNG (`<= max_size_px`).
pub fn extract_file_thumbnail_png(path: &Path, max_size_px: u32) -> Result<Vec<u8>, String> {
    if max_size_px == 0 {
        return Err("thumbnail max_size_px must be > 0".to_owned());
    }
    let limit = max_size_px.min(MAX_THUMBNAIL_SIZE_PX);
    let ext = lower_extension(path);
    let header = peek_file_header(path)?;
    let magic = classify_magic_bytes(&header);

    // 1. Fast embedded preview path for `.coree` and `.cdr`/`.cdt`/`.cmx`/`.cdr.zip`
    match magic {
        MagicKind::Zip => {
            if ext == "coree" || is_coree_archive(path) {
                if let Ok(Some(preview)) = coree_io::read_coree_preview(path) {
                    if let Ok(fitted) = fit_png_bytes_to_max(&preview, limit) {
                        return Ok(fitted);
                    }
                }
            } else {
                let limits = CodecLimits::default();
                if let Ok(bytes) = read_file_bounded(path, &limits) {
                    if let Ok(Some(preview)) = extract_cdr_preview_png(&bytes, &limits) {
                        if let Ok(fitted) = fit_png_bytes_to_max(&preview, limit) {
                            return Ok(fitted);
                        }
                    }
                }
            }
        }
        MagicKind::RiffCdr => {
            let limits = CodecLimits::default();
            if let Ok(bytes) = read_file_bounded(path, &limits) {
                if let Ok(Some(preview)) = extract_cdr_preview_png(&bytes, &limits) {
                    if let Ok(fitted) = fit_png_bytes_to_max(&preview, limit) {
                        return Ok(fitted);
                    }
                }
            }
        }
        _ => {}
    }

    // 2. Universal fallback: open document and render Page 0 at exact bounded dimensions
    let mut opened = crate::io_bridge::open_any_document(path)?;
    render_page0_thumbnail_png(&mut opened.document, limit)
}

fn fit_dimensions(width: u32, height: u32, max_dim: u32) -> (u32, u32) {
    let w = width.max(1);
    let h = height.max(1);
    if w <= max_dim && h <= max_dim {
        return (w, h);
    }
    let scale = (f64::from(max_dim) / f64::from(w.max(h))).min(1.0);
    let nw = (f64::from(w) * scale)
        .round()
        .clamp(1.0, f64::from(max_dim)) as u32;
    let nh = (f64::from(h) * scale)
        .round()
        .clamp(1.0, f64::from(max_dim)) as u32;
    (nw, nh)
}

fn fit_png_bytes_to_max(png_bytes: &[u8], limit: u32) -> Result<Vec<u8>, String> {
    let asset = decode_raster_bytes_with_limits(png_bytes, &IoLimits::default())
        .map_err(|e| format!("failed to decode embedded preview: {e}"))?;
    if asset.width <= limit && asset.height <= limit && png_bytes.starts_with(PNG_SIGNATURE) {
        return Ok(png_bytes.to_vec());
    }
    let (new_w, new_h) = fit_dimensions(asset.width, asset.height, limit);
    let resized = if (new_w, new_h) == (asset.width, asset.height) {
        asset
    } else {
        resample(&asset, new_w, new_h)
            .map_err(|e| format!("failed to resize embedded preview: {e}"))?
    };
    export_raster_to_bytes(
        resized.width,
        resized.height,
        &resized.to_rgba8_vec(),
        &RasterExportOptions::new(RasterFormat::Png, 96.0),
    )
    .map_err(|e| format!("failed to encode thumbnail PNG: {e}"))
}

fn render_page0_thumbnail_png(doc: &mut Document, limit: u32) -> Result<Vec<u8>, String> {
    let page = doc
        .pages()
        .first()
        .ok_or_else(|| "document has no pages to thumbnail".to_owned())?;
    let w_mm = if page.width_mm.is_finite() && page.width_mm > 0.0 {
        page.width_mm
    } else {
        210.0
    };
    let h_mm = if page.height_mm.is_finite() && page.height_mm > 0.0 {
        page.height_mm
    } else {
        297.0
    };
    let max_mm = w_mm.max(h_mm);
    let scale_px_per_mm = f64::from(limit) / max_mm;
    let target_w = (w_mm * scale_px_per_mm)
        .round()
        .clamp(1.0, f64::from(limit)) as u32;
    let target_h = (h_mm * scale_px_per_mm)
        .round()
        .clamp(1.0, f64::from(limit)) as u32;

    let raw_dpi = scale_px_per_mm * MM_PER_INCH;
    let clamped_dpi = raw_dpi.clamp(1.0, 4800.0);
    let zoom = (raw_dpi / clamped_dpi).clamp(0.01, 256.0);

    let mut backend = TinySkiaBackend::new(target_w, target_h)
        .map_err(|e| format!("thumbnail surface allocation failed: {e}"))?;
    let mut viewport = Viewport::for_export(target_w, target_h, clamped_dpi);
    viewport.zoom = zoom;

    let _ = doc.set_active_page_index(0);
    doc.set_multipage_view(false);

    let mut renderer = SceneRenderer::new();
    renderer.render_document(
        doc,
        &viewport,
        ViewMode::Enhanced,
        &RenderOptions::export(),
        &mut backend,
    );
    let rgba = backend.into_rgba();

    export_raster_to_bytes(
        rgba.width,
        rgba.height,
        &rgba.rgba,
        &RasterExportOptions::new(RasterFormat::Png, 96.0),
    )
    .map_err(|e| format!("failed to encode rendered thumbnail PNG: {e}"))
}

/// Builds a structural report from an already-loaded in-memory [`Document`].
#[must_use]
pub fn inspect_in_memory_document(doc: &Document) -> FileInspectionReport {
    build_report_from_doc(
        doc,
        ContainerFormat::NativeCoree,
        doc.metadata()
            .source_format
            .clone()
            .unwrap_or_else(|| "In-Memory Studio Document".to_owned()),
        0,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

/// Exports `doc` to `.cdr` in the requested version (`RiffV12`, `ZipX4`, `ZipX6`).
pub fn write_cdr_file(doc: &Document, path: &Path, version: CdrWriteVersion) -> CdrResult<()> {
    export_cdr(doc, path, version)
}

/// Exports `doc` to `.cdt` template format.
pub fn write_cdt_file(doc: &Document, path: &Path, version: CdrWriteVersion) -> CdrResult<()> {
    export_cdt(doc, path, version)
}

/// Exports `doc` to `.cmx` presentation exchange format.
pub fn write_cmx_file(doc: &Document, path: &Path) -> CdrResult<()> {
    export_cmx(doc, path)
}

/// Exports `doc` to raw `.cdr` bytes in memory (used by preview/tests).
pub fn write_cdr_bytes_for_version(doc: &Document, version: CdrWriteVersion) -> CdrResult<Vec<u8>> {
    match version {
        CdrWriteVersion::RiffV12 => {
            Ok(write_cdr_riff_with_options(doc, RiffWriteOptions::default())?.riff_bytes)
        }
        CdrWriteVersion::ZipX4 => write_cdr_zip(doc, CdrZipEra::X4, false),
        CdrWriteVersion::ZipX6 => write_cdr_zip(doc, CdrZipEra::X6, false),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_report_from_doc(
    doc: &Document,
    detected_format: ContainerFormat,
    format_era: String,
    file_size_bytes: u64,
    version_number: Option<u32>,
    chunks: Vec<ChunkSummary>,
    zip_entries: Vec<ZipEntrySummary>,
    warnings: Vec<String>,
) -> FileInspectionReport {
    let page_count = doc.pages().len();
    let layer_count: usize = doc.pages().iter().map(|p| p.layers.len()).sum();
    let object_count = doc.object_count();
    let bitmap_count = doc.assets().len();

    let mut fonts = BTreeSet::new();
    let mut color_models = BTreeSet::new();
    let mut unknown_chunks = Vec::new();

    for unk in doc.unknown_doc_chunks() {
        unknown_chunks.push(UnknownChunkSummary {
            fourcc: unk.fourcc_str(),
            byte_len: unk.raw_bytes.len(),
            version_hint: unk.version_hint,
            scope: "document".to_owned(),
        });
    }

    for page in doc.pages() {
        for unk in &page.unknown_chunks {
            unknown_chunks.push(UnknownChunkSummary {
                fourcc: unk.fourcc_str(),
                byte_len: unk.raw_bytes.len(),
                version_hint: unk.version_hint,
                scope: format!("page:{}", page.name),
            });
        }
        for layer in &page.layers {
            for unk in &layer.unknown_chunks {
                unknown_chunks.push(UnknownChunkSummary {
                    fourcc: unk.fourcc_str(),
                    byte_len: unk.raw_bytes.len(),
                    version_hint: unk.version_hint,
                    scope: format!("layer:{}", layer.name),
                });
            }
        }
    }

    for obj in doc.objects().values() {
        for c in obj.fill.referenced_colors() {
            color_models.insert(color_model_name(&c).to_owned());
        }
        if obj.outline.is_visible() {
            color_models.insert(color_model_name(&obj.outline.color).to_owned());
        }
        match &obj.payload {
            NodePayload::ArtisticText(t) => {
                for r in &t.runs {
                    fonts.insert(r.style.font_family.clone());
                    color_models.insert(color_model_name(&r.style.fill).to_owned());
                }
            }
            NodePayload::ParagraphText(p) => {
                for r in &p.runs {
                    fonts.insert(r.style.font_family.clone());
                    color_models.insert(color_model_name(&r.style.fill).to_owned());
                }
            }
            NodePayload::TextOnPath(top) => {
                for r in &top.text.runs {
                    fonts.insert(r.style.font_family.clone());
                    color_models.insert(color_model_name(&r.style.fill).to_owned());
                }
            }
            _ => {}
        }
        for unk in &obj.unknown_chunks {
            unknown_chunks.push(UnknownChunkSummary {
                fourcc: unk.fourcc_str(),
                byte_len: unk.raw_bytes.len(),
                version_hint: unk.version_hint,
                scope: format!("object:{}", obj.name),
            });
        }
    }

    for sw in &doc.document_palette().swatches {
        color_models.insert(color_model_name(&sw.color).to_owned());
    }

    FileInspectionReport {
        detected_format,
        format_era,
        file_size_bytes,
        version_number,
        chunks,
        zip_entries,
        page_count,
        layer_count,
        object_count,
        bitmap_count,
        fonts: fonts.into_iter().collect(),
        color_models: color_models.into_iter().collect(),
        unknown_chunks,
        warnings,
    }
}

fn summarize_riff_tree(tree: &RiffDocument) -> Vec<ChunkSummary> {
    let mut out = Vec::new();
    let root_size: u64 = tree.children.iter().map(estimate_node_size).sum();
    out.push(ChunkSummary {
        fourcc: fourcc_to_string(&tree.form_type),
        size: root_size,
        depth: 0,
        is_list: true,
        compressed: false,
    });
    for child in &tree.children {
        collect_node_summaries(child, 1, &mut out);
    }
    out
}

fn estimate_node_size(node: &RiffNode) -> u64 {
    match node {
        RiffNode::Chunk { data, .. } => data.len() as u64,
        RiffNode::List { children, .. } => {
            4u64.saturating_add(children.iter().map(estimate_node_size).sum::<u64>())
        }
    }
}

fn collect_node_summaries(node: &RiffNode, depth: usize, out: &mut Vec<ChunkSummary>) {
    match node {
        RiffNode::Chunk {
            fourcc,
            data,
            compressed,
        } => {
            out.push(ChunkSummary {
                fourcc: fourcc_to_string(fourcc),
                size: data.len() as u64,
                depth,
                is_list: false,
                compressed: *compressed,
            });
        }
        RiffNode::List {
            list_type,
            children,
            compressed,
        } => {
            out.push(ChunkSummary {
                fourcc: fourcc_to_string(list_type),
                size: estimate_node_size(node),
                depth,
                is_list: true,
                compressed: *compressed,
            });
            for ch in children {
                collect_node_summaries(ch, depth + 1, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_magic_bytes_detects_all_real_world_signatures() {
        assert_eq!(classify_magic_bytes(b"PK\x03\x04\x14\x00"), MagicKind::Zip);
        assert_eq!(
            classify_magic_bytes(b"RIFF\x10\x00\x00\x00CDRC"),
            MagicKind::RiffCdr
        );
        assert_eq!(
            classify_magic_bytes(b"RIFX\x00\x00\x00\x10CMX1"),
            MagicKind::RiffCdr
        );
        assert_eq!(
            classify_magic_bytes(b"RIFF\x10\x00\x00\x00WEBP"),
            MagicKind::Raster("png")
        );
        assert_eq!(classify_magic_bytes(b"CPT9FILE\x00\x00"), MagicKind::Cpt);
        assert_eq!(classify_magic_bytes(b"%PDF-1.7\n"), MagicKind::Pdf);
        assert_eq!(
            classify_magic_bytes(b"junk\x00\x01%PDF-2.0\n"),
            MagicKind::Pdf
        );
        assert_eq!(
            classify_magic_bytes(b"8BPS\x00\x01\x00\x00"),
            MagicKind::Psd
        );
        assert_eq!(
            classify_magic_bytes(b"8BPS\x00\x02\x00\x00"),
            MagicKind::Psd
        );
        assert_eq!(
            classify_magic_bytes(b"%!PS-Adobe-3.0 EPSF-3.0\n%%Creator: %PDF-1.4\n"),
            MagicKind::Eps
        );
        assert_eq!(
            classify_magic_bytes(&[0xC5, 0xD0, 0xD3, 0xC6, 0x1E, 0x00]),
            MagicKind::Eps
        );
        assert_eq!(
            classify_magic_bytes(b"<?xml version=\"1.0\"?>\n<svg width=\"10\"></svg>"),
            MagicKind::Svg
        );
        assert_eq!(
            classify_magic_bytes(&[0x1F, 0x8B, 0x08, 0x00]),
            MagicKind::Svg
        );
        assert_eq!(
            classify_magic_bytes(b"0\nSECTION\n2\nHEADER\n999\n<svg>\n"),
            MagicKind::Dxf
        );
        assert_eq!(
            classify_magic_bytes(b"IN;SP1;PU0,0;PD100,100;"),
            MagicKind::Plt
        );
        assert_eq!(
            classify_magic_bytes(b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR"),
            MagicKind::Raster("png")
        );
        assert_eq!(
            classify_magic_bytes(&[0xFF, 0xD8, 0xFF, 0xE0]),
            MagicKind::Raster("jpg")
        );
        assert_eq!(
            classify_magic_bytes(b"8BCB\x00\x01\x23\x29"),
            MagicKind::Acb
        );
    }

    #[test]
    fn fit_dimensions_preserves_aspect_and_bounds() {
        assert_eq!(fit_dimensions(256, 452, 256), (145, 256));
        assert_eq!(fit_dimensions(120, 80, 256), (120, 80));
        assert_eq!(fit_dimensions(4000, 10, 256), (256, 1));
    }
}
