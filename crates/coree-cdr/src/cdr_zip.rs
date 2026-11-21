// SPDX-License-Identifier: MPL-2.0

//! Clean-room ZIP/OPC CorelDRAW container reader and writer supporting:
//! - **CDR X4–X5** (`mimetype`, `content/riffData.cdr`, `metadata/metadata.xml`, `color/`, `preview/`)
//! - **CDR X6–2025** (`mimetype`, `content/root.dat`, `content/dataFileList.dat`, `content/data/*.dat`,
//!   `styles/default.xml`, `metadata/metadata.xml`, `color/`, `preview/`)
//!
//! All archive entries are validated strictly against Zip-Slip and Zip-Bomb limits.

use crate::cdr_riff::{
    encode_bmp_file_rgba, parse_cdr_riff_with_budget, write_cdr_riff_with_options, RiffWriteOptions,
};
use crate::error::{CdrError, CdrResult};
use crate::limits::{read_bounded, sanitize_zip_path, CodecLimits};
use crate::riff::RiffDocument;
use coree_core::{ColorMode, Document};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// ZIP-based CDR format era.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CdrZipEra {
    /// CorelDRAW X4–X5 (`content/riffData.cdr`).
    X4,
    /// CorelDRAW X6–2025 (`content/root.dat` + `content/dataFileList.dat` + `content/data/*.dat`).
    X6,
}

/// Summary metadata for a single validated ZIP entry (used by the structural inspector).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntryInfo {
    pub name: String,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}

/// Result of parsing a ZIP/OPC CDR or CDT container.
#[derive(Debug, Clone)]
pub struct ParsedZipCdr {
    pub document: Document,
    pub era: CdrZipEra,
    pub version: u16,
    pub is_template: bool,
    pub warnings: Vec<String>,
    pub zip_entries: Vec<ZipEntryInfo>,
    pub riff_tree: RiffDocument,
}

/// Returns `true` if `bytes` begins with a ZIP local file header (`PK\x03\x04`) or
/// empty archive EOCD (`PK\x05\x06`).
#[must_use]
pub fn is_zip_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && (&bytes[0..4] == b"PK\x03\x04" || &bytes[0..4] == b"PK\x05\x06")
}

/// Parses a ZIP/OPC CDR (X4–X5 or X6–2025) or CDT template archive with full
/// Zip-Slip and Zip-Bomb security enforcement.
pub fn parse_cdr_zip(bytes: &[u8], limits: &CodecLimits) -> CdrResult<ParsedZipCdr> {
    let archive_len = bytes.len() as u64;
    let cursor = Cursor::new(bytes);
    let mut archive =
        ZipArchive::new(cursor).map_err(|e| CdrError::ZipError(format!("invalid ZIP: {e}")))?;

    let entry_count = archive.len();
    limits.check_zip_entries(entry_count)?;

    // First pass: validate EVERY entry path (Zip-Slip & duplicates) and declared sizes/ratios (Zip-Bomb)
    let mut entries: Vec<ZipEntryInfo> = Vec::with_capacity(entry_count);
    let mut index_by_path: HashMap<String, usize> = HashMap::with_capacity(entry_count);
    let mut declared_total: u64 = 0;
    let mut declared_compressed_total: u64 = 0;

    for i in 0..entry_count {
        let entry = archive
            .by_index(i)
            .map_err(|e| CdrError::ZipError(format!("ZIP entry #{i}: {e}")))?;
        let clean_name = sanitize_zip_path(entry.name_raw())?;
        let comp_sz = entry.compressed_size();
        let uncomp_sz = entry.size();

        if !entry.is_dir() {
            // Guard against forged huge `compressed_size` headers weakening ratio checks
            // and against overlapping-entry Zip-Bombs where sum(compressed_size) > archive size.
            if comp_sz > archive_len {
                return Err(CdrError::ZipError(format!(
                    "forged ZIP compressed_size ({comp_sz}) exceeds archive length ({archive_len})"
                )));
            }
            declared_compressed_total = declared_compressed_total
                .checked_add(comp_sz)
                .ok_or(CdrError::ArithmeticOverflow("ZIP total compressed size"))?;
            if declared_compressed_total > archive_len {
                return Err(CdrError::ZipError(format!(
                    "cumulative ZIP compressed sizes ({declared_compressed_total}) exceed archive length ({archive_len})"
                )));
            }
            if uncomp_sz > limits.max_uncompressed_bytes {
                return Err(CdrError::UncompressedSizeLimitExceeded {
                    limit: limits.max_uncompressed_bytes,
                });
            }
            limits.check_compression_ratio(comp_sz, uncomp_sz)?;
            declared_total = declared_total
                .checked_add(uncomp_sz)
                .ok_or(CdrError::ArithmeticOverflow("ZIP declared total size"))?;
            if declared_total > limits.max_uncompressed_bytes {
                return Err(CdrError::UncompressedSizeLimitExceeded {
                    limit: limits.max_uncompressed_bytes,
                });
            }
        }

        let lower_name = clean_name.to_ascii_lowercase();
        if index_by_path.insert(lower_name, i).is_some() {
            return Err(CdrError::ZipError(format!(
                "duplicate ZIP entry path '{clean_name}'"
            )));
        }
        entries.push(ZipEntryInfo {
            name: clean_name,
            compressed_size: comp_sz,
            uncompressed_size: uncomp_sz,
        });
    }

    let mut budget_remaining = limits.max_uncompressed_bytes;
    let mut warnings = Vec::new();

    // Detect era & template status (case-insensitive lookup for real-world ZIP resilience)
    let has_root_dat = index_by_path.contains_key("content/root.dat");
    let has_riff_data = index_by_path.contains_key("content/riffdata.cdr");
    if !has_root_dat && !has_riff_data {
        return Err(CdrError::UnsupportedFormat(
            "ZIP archive is missing both 'content/root.dat' (X6+) and 'content/riffData.cdr' (X4–X5)"
                .to_owned(),
        ));
    }

    let era = if has_root_dat {
        CdrZipEra::X6
    } else {
        CdrZipEra::X4
    };

    let mut is_template = false;
    if let Some(&mime_idx) = index_by_path.get("mimetype") {
        let mime_bytes = read_entry_by_index(
            &mut archive,
            mime_idx,
            archive_len,
            limits,
            &mut budget_remaining,
        )?;
        let mime_str = String::from_utf8_lossy(&mime_bytes).to_ascii_lowercase();
        if mime_str.contains("template") {
            is_template = true;
        }
    }

    // Load external data files for CDR X6+ (`content/dataFileList.dat` + `content/data/*.dat`)
    let mut external_bitmaps: HashMap<u32, Vec<u8>> = HashMap::new();
    if era == CdrZipEra::X6 {
        let mut listed_paths: Vec<String> = Vec::new();
        let mut seen_paths: HashSet<String> = HashSet::new();
        if let Some(&list_idx) = index_by_path.get("content/datafilelist.dat") {
            let raw_list = read_entry_by_index(
                &mut archive,
                list_idx,
                archive_len,
                limits,
                &mut budget_remaining,
            )?;
            let text = String::from_utf8_lossy(&raw_list);
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let candidate = if trimmed.to_ascii_lowercase().starts_with("content/") {
                    trimmed.to_owned()
                } else {
                    format!("content/{trimmed}")
                };
                // Validate paths listed inside dataFileList.dat against Zip-Slip too!
                let safe = sanitize_zip_path(candidate.as_bytes())?;
                let lower = safe.to_ascii_lowercase();
                // Skip primary RIFF entries and deduplicate repeated references (anti-amplification)
                if lower == "content/root.dat" || lower == "content/riffdata.cdr" {
                    continue;
                }
                if seen_paths.insert(lower.clone()) {
                    listed_paths.push(lower);
                }
            }
        }
        // Also discover any `content/data/*.dat` present in the archive
        for info in &entries {
            let lower = info.name.to_ascii_lowercase();
            if lower.starts_with("content/data/")
                && lower.ends_with(".dat")
                && seen_paths.insert(lower.clone())
            {
                listed_paths.push(lower);
            }
        }

        for data_path in listed_paths {
            if let Some(&idx) = index_by_path.get(&data_path) {
                let data_bytes = read_entry_by_index(
                    &mut archive,
                    idx,
                    archive_len,
                    limits,
                    &mut budget_remaining,
                )?;
                // Recognize externalized CBMP payloads (`b"CBMP"` + u32 id + ...)
                if data_bytes.len() >= 13 && &data_bytes[0..4] == b"CBMP" {
                    let id = u32::from_le_bytes([
                        data_bytes[4],
                        data_bytes[5],
                        data_bytes[6],
                        data_bytes[7],
                    ]);
                    external_bitmaps.insert(id, data_bytes);
                }
            }
        }
    }

    // Read primary RIFF payload (`content/root.dat` or `content/riffData.cdr`)
    let primary_path = if era == CdrZipEra::X6 {
        "content/root.dat"
    } else {
        "content/riffdata.cdr"
    };
    let primary_idx = *index_by_path
        .get(primary_path)
        .ok_or_else(|| CdrError::UnsupportedFormat("missing primary CDR payload in ZIP".into()))?;
    let primary_bytes = read_entry_by_index(
        &mut archive,
        primary_idx,
        archive_len,
        limits,
        &mut budget_remaining,
    )?;

    // Share remaining cumulative budget across `LIST cmpr` streams and decoded RGBA bitmaps!
    let mut parsed_riff = parse_cdr_riff_with_budget(
        &primary_bytes,
        limits,
        &external_bitmaps,
        &mut budget_remaining,
        false,
    )?;
    warnings.extend(parsed_riff.warnings);
    if parsed_riff.is_template {
        is_template = true;
    }

    // Parse optional `metadata/metadata.xml`
    if let Some(&meta_idx) = index_by_path.get("metadata/metadata.xml") {
        let meta_bytes = read_entry_by_index(
            &mut archive,
            meta_idx,
            archive_len,
            limits,
            &mut budget_remaining,
        )?;
        let xml = String::from_utf8_lossy(&meta_bytes);
        apply_metadata_xml(&xml, &mut parsed_riff.document, &mut is_template);
    }

    // Parse optional `styles/default.xml` (X6+)
    if let Some(&style_idx) = index_by_path.get("styles/default.xml") {
        let style_bytes = read_entry_by_index(
            &mut archive,
            style_idx,
            archive_len,
            limits,
            &mut budget_remaining,
        )?;
        let xml = String::from_utf8_lossy(&style_bytes);
        apply_default_styles_xml(&xml, &mut parsed_riff.document);
    }

    // Update source format provenance
    parsed_riff.document.metadata.source_format = Some(match (era, is_template) {
        (CdrZipEra::X4, false) => "CDR X4-X5 ZIP".to_owned(),
        (CdrZipEra::X6, false) => "CDR X6-2025 ZIP".to_owned(),
        (CdrZipEra::X4, true) => "CDT X4-X5 ZIP".to_owned(),
        (CdrZipEra::X6, true) => "CDT X6-2025 ZIP".to_owned(),
    });

    Ok(ParsedZipCdr {
        document: parsed_riff.document,
        era,
        version: parsed_riff.version,
        is_template,
        warnings,
        zip_entries: entries,
        riff_tree: parsed_riff.riff_tree,
    })
}

fn read_entry_by_index(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    index: usize,
    archive_len: u64,
    limits: &CodecLimits,
    budget_remaining: &mut u64,
) -> CdrResult<Vec<u8>> {
    let entry = archive
        .by_index(index)
        .map_err(|e| CdrError::ZipError(format!("ZIP read entry #{index}: {e}")))?;
    let comp_sz = entry.compressed_size().min(archive_len);
    read_bounded(entry, limits, budget_remaining, Some(comp_sz))
}

// =============================================================================
// ZIP/OPC CDR Writer (X4–X5 & X6–2025)
// =============================================================================

/// Exports `doc` into a clean-room ZIP/OPC CDR archive (`era`: X4–X5 or X6–2025,
/// `is_template`: true for `.cdt` templates).
pub fn write_cdr_zip(doc: &Document, era: CdrZipEra, is_template: bool) -> CdrResult<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        // 1. `mimetype` (stored uncompressed as first ZIP entry per OPC/ODF convention)
        let mimetype = match (era, is_template) {
            (CdrZipEra::X4, false) => "application/x-vnd.corel.draw.document+zip",
            (CdrZipEra::X4, true) => "application/x-vnd.corel.draw.template+zip",
            (CdrZipEra::X6, false) => "application/x-vnd.corel.zcf.draw.document+zip",
            (CdrZipEra::X6, true) => "application/x-vnd.corel.zcf.draw.template+zip",
        };
        zip_write_file(&mut zip, "mimetype", mimetype.as_bytes(), stored)?;

        // 2. Primary content payload (`content/riffData.cdr` for X4, `content/root.dat` + `content/data/*.dat` for X6+)
        match era {
            CdrZipEra::X4 => {
                let form_type = if is_template { *b"CDTE" } else { *b"CDRE" };
                let riff_out = write_cdr_riff_with_options(
                    doc,
                    RiffWriteOptions {
                        form_type,
                        version: 1400,
                        compress_doc: true,
                        externalize_bitmaps: false,
                    },
                )?;
                zip_write_file(
                    &mut zip,
                    "content/riffData.cdr",
                    &riff_out.riff_bytes,
                    deflated,
                )?;
            }
            CdrZipEra::X6 => {
                let form_type = if is_template { *b"CDTG" } else { *b"CDRG" };
                let riff_out = write_cdr_riff_with_options(
                    doc,
                    RiffWriteOptions {
                        form_type,
                        version: 1600,
                        compress_doc: true,
                        externalize_bitmaps: true,
                    },
                )?;
                zip_write_file(&mut zip, "content/root.dat", &riff_out.riff_bytes, deflated)?;

                let mut data_file_list = String::new();
                // Write externalized raster payloads into `content/data/XXXX.dat`
                for (bmp_id, payload) in &riff_out.external_bitmaps {
                    let rel = format!("data/{bmp_id:04}.dat");
                    let full = format!("content/{rel}");
                    zip_write_file(&mut zip, &full, payload, deflated)?;
                    data_file_list.push_str(&rel);
                    data_file_list.push('\n');
                }
                // Always include a lightweight manifest data block so X6+ archives exercise `content/data/*.dat`
                let manifest_rel = "data/manifest.dat";
                let manifest_body = format!(
                    "coree-cdr-x6\npages={}\nobjects={}\nassets={}\n",
                    doc.pages.len(),
                    doc.objects.len(),
                    doc.assets.len()
                );
                zip_write_file(
                    &mut zip,
                    &format!("content/{manifest_rel}"),
                    manifest_body.as_bytes(),
                    deflated,
                )?;
                data_file_list.push_str(manifest_rel);
                data_file_list.push('\n');

                zip_write_file(
                    &mut zip,
                    "content/dataFileList.dat",
                    data_file_list.as_bytes(),
                    deflated,
                )?;

                // `styles/default.xml` (X6+)
                let styles_xml = build_default_styles_xml(doc);
                zip_write_file(
                    &mut zip,
                    "styles/default.xml",
                    styles_xml.as_bytes(),
                    deflated,
                )?;
            }
        }

        // 3. `metadata/metadata.xml`
        let meta_xml = build_metadata_xml(doc, is_template);
        zip_write_file(
            &mut zip,
            "metadata/metadata.xml",
            meta_xml.as_bytes(),
            deflated,
        )?;

        // 4. `color/documentPalette.xml`
        let color_xml = build_color_palette_xml(doc);
        zip_write_file(
            &mut zip,
            "color/documentPalette.xml",
            color_xml.as_bytes(),
            deflated,
        )?;

        // 5. `preview/thumbnail.bmp`
        let thumb_bmp = encode_bmp_file_rgba(8, 8, &[250u8; 8 * 8 * 4])?;
        zip_write_file(&mut zip, "preview/thumbnail.bmp", &thumb_bmp, deflated)?;

        zip.finish()
            .map_err(|e| CdrError::ZipError(format!("ZIP finish failed: {e}")))?;
    }

    Ok(cursor.into_inner())
}

fn zip_write_file(
    zip: &mut ZipWriter<&mut Cursor<Vec<u8>>>,
    path: &str,
    data: &[u8],
    options: SimpleFileOptions,
) -> CdrResult<()> {
    zip.start_file(path, options)
        .map_err(|e| CdrError::ZipError(format!("ZIP start_file '{path}': {e}")))?;
    zip.write_all(data)
        .map_err(|e| CdrError::ZipError(format!("ZIP write '{path}': {e}")))?;
    Ok(())
}

// =============================================================================
// Lightweight Pure-Rust XML Metadata / Style Helpers
// =============================================================================

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml_unescape(xml[start..end].trim()))
}

fn build_metadata_xml(doc: &Document, is_template: bool) -> String {
    let kw = doc.metadata.keywords.join(", ");
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<metadata>\n",
            "  <title>{}</title>\n",
            "  <author>{}</author>\n",
            "  <subject>{}</subject>\n",
            "  <keywords>{}</keywords>\n",
            "  <notes>{}</notes>\n",
            "  <appVersion>{}</appVersion>\n",
            "  <isTemplate>{}</isTemplate>\n",
            "</metadata>\n"
        ),
        xml_escape(&doc.title),
        xml_escape(&doc.metadata.author),
        xml_escape(&doc.metadata.subject),
        xml_escape(&kw),
        xml_escape(&doc.metadata.notes),
        xml_escape(&doc.metadata.app_version),
        if is_template { "true" } else { "false" }
    )
}

fn apply_metadata_xml(xml: &str, doc: &mut Document, is_template: &mut bool) {
    if let Some(title) = extract_xml_tag(xml, "title") {
        if !title.is_empty() {
            doc.title = title;
        }
    }
    if let Some(author) = extract_xml_tag(xml, "author") {
        doc.metadata.author = author;
    }
    if let Some(subject) = extract_xml_tag(xml, "subject") {
        doc.metadata.subject = subject;
    }
    if let Some(kw) = extract_xml_tag(xml, "keywords") {
        if !kw.is_empty() {
            doc.metadata.keywords = kw
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    if let Some(notes) = extract_xml_tag(xml, "notes") {
        doc.metadata.notes = notes;
    }
    if let Some(tpl) = extract_xml_tag(xml, "isTemplate") {
        if tpl.eq_ignore_ascii_case("true") {
            *is_template = true;
        }
    }
}

fn build_default_styles_xml(doc: &Document) -> String {
    let mode = match doc.color_mode {
        ColorMode::Cmyk => "CMYK",
        ColorMode::Rgb => "RGB",
    };
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<styles>\n",
            "  <colorMode>{mode}</colorMode>\n",
            "  <dpi>{dpi}</dpi>\n",
            "  <defaultFont>Noto Sans</defaultFont>\n",
            "</styles>\n"
        ),
        mode = mode,
        dpi = doc.dpi
    )
}

fn apply_default_styles_xml(xml: &str, doc: &mut Document) {
    if let Some(mode) = extract_xml_tag(xml, "colorMode") {
        if mode.eq_ignore_ascii_case("RGB") {
            doc.color_mode = ColorMode::Rgb;
        } else if mode.eq_ignore_ascii_case("CMYK") {
            doc.color_mode = ColorMode::Cmyk;
        }
    }
    if let Some(dpi_str) = extract_xml_tag(xml, "dpi") {
        if let Ok(dpi) = dpi_str.parse::<f32>() {
            if dpi.is_finite() && dpi >= 1.0 {
                doc.dpi = dpi;
            }
        }
    }
}

fn build_color_palette_xml(doc: &Document) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<palette name=\"Document Palette\">\n",
    );
    for sw in &doc.document_palette.swatches {
        let hex = sw.color.to_hex_rgba();
        xml.push_str(&format!(
            "  <swatch name=\"{}\" hex=\"{hex}\"/>\n",
            xml_escape(&sw.name)
        ));
    }
    xml.push_str("</palette>\n");
    xml
}
