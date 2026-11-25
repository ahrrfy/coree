// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room ZIP/OPC CDR container reader and writer supporting:
//! - **CDR X4–X5** (`mimetype`, `content/riffData.cdr`, `metadata/metadata.xml`, `color/`, `preview/`)
//! - **CDR X6–2025** (`mimetype`, `content/root.dat`, `content/dataFileList.dat`, `content/data/*.dat`,
//!   `styles/default.xml`, `metadata/metadata.xml`, `color/`, `preview/`)
//!
//! All archive entries are validated strictly against Zip-Slip and Zip-Bomb limits.

use crate::cdr_riff::{
    encode_bmp_file_rgba, parse_cdr_riff_tree_with_options, render_doc_mini_thumbnail_rgba,
    write_cdr_riff_with_options, RiffWriteOptions,
};
use crate::error::{CdrError, CdrResult};
use crate::limits::{read_bounded, sanitize_zip_path, CodecLimits};
use crate::riff::{parse_riff_with_budget, write_riff, RiffDocument, RiffNode};
use crate::zcf::{
    apply_xmp_metadata_xml, encode_png_rgba, is_zcf_indirect_tree, parse_color_context_xml,
    parse_doc_palette_xml, parse_document_cdss, parse_textinfo_xml, resolve_zcf_riff_tree,
    ZcfContext,
};
use coree_core::{Color, ColorMode, Document};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// ZIP-based CDR format era.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CdrZipEra {
    /// CDR X4–X5 (`content/riffData.cdr`).
    X4,
    /// CDR X6–2025 (`content/root.dat` + `content/dataFileList.dat` + `content/data/*.dat`).
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
    parse_cdr_zip_with_options(bytes, limits, crate::CdrReadOptions::default())
}

/// Parses a ZIP/OPC CDR/CDT archive with explicit [`crate::CdrReadOptions`].
pub fn parse_cdr_zip_with_options(
    bytes: &[u8],
    limits: &CodecLimits,
    opts: crate::CdrReadOptions,
) -> CdrResult<ParsedZipCdr> {
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

    // Load external data files for CDR X6+ (`content/dataFileList.dat` + `content/data/*.dat`).
    // Preserve exact 0-based `dataFileList.dat` indices via `file_index_map -> data_pool`, while
    // deduplicating ZIP entry reads so repeated entries cannot amplify CPU or memory budget.
    let mut external_bitmaps: HashMap<u32, Vec<u8>> = HashMap::new();
    let mut data_pool: Vec<Vec<u8>> = Vec::new();
    let mut file_index_map: Vec<Option<usize>> = Vec::new();
    let mut file_index_names: Vec<Option<String>> = Vec::new();
    let mut pool_by_zip_idx: HashMap<usize, usize> = HashMap::new();
    let mut unknown_data_blobs: Vec<(String, Vec<u8>)> = Vec::new();

    if era == CdrZipEra::X6 {
        let mut seen_zip_indices: HashSet<usize> = HashSet::new();
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
                // Validate raw entry listed inside dataFileList.dat against Zip-Slip!
                let safe_raw = sanitize_zip_path(trimmed.as_bytes())?;
                let lower_raw = safe_raw.to_ascii_lowercase();

                // Skip primary RIFF entries (anti-amplification) without corrupting subsequent indices
                if lower_raw == "content/root.dat"
                    || lower_raw == "content/riffdata.cdr"
                    || lower_raw == "root.dat"
                {
                    file_index_map.push(None);
                    file_index_names.push(None);
                    continue;
                }

                // Resolve against authentic `content/data/<name>`, `content/<name>`, or `<name>`
                let (resolved_zip_idx, resolved_path) = if lower_raw.starts_with("content/") {
                    (index_by_path.get(&lower_raw).copied(), lower_raw.clone())
                } else {
                    let in_data = format!("content/data/{lower_raw}");
                    let in_content = format!("content/{lower_raw}");
                    if let Some(&idx) = index_by_path.get(&in_data) {
                        (Some(idx), in_data)
                    } else if let Some(&idx) = index_by_path.get(&in_content) {
                        (Some(idx), in_content)
                    } else {
                        (index_by_path.get(&lower_raw).copied(), lower_raw.clone())
                    }
                };

                let Some(zip_idx) = resolved_zip_idx else {
                    file_index_map.push(None);
                    file_index_names.push(None);
                    continue;
                };

                let pool_idx = if let Some(&existing) = pool_by_zip_idx.get(&zip_idx) {
                    existing
                } else {
                    let data_bytes = read_entry_by_index(
                        &mut archive,
                        zip_idx,
                        archive_len,
                        limits,
                        &mut budget_remaining,
                    )?;
                    // Recognize clean-room externalized CBMP payloads (`b"CBMP"` + u32 id + ...)
                    if data_bytes.len() >= 13 && &data_bytes[0..4] == b"CBMP" {
                        let id = u32::from_le_bytes([
                            data_bytes[4],
                            data_bytes[5],
                            data_bytes[6],
                            data_bytes[7],
                        ]);
                        external_bitmaps.insert(id, data_bytes.clone());
                    }
                    let idx_in_pool = data_pool.len();
                    data_pool.push(data_bytes);
                    pool_by_zip_idx.insert(zip_idx, idx_in_pool);
                    seen_zip_indices.insert(zip_idx);
                    idx_in_pool
                };
                file_index_map.push(Some(pool_idx));
                file_index_names.push(Some(resolved_path));
            }
        }

        // Also discover any unlisted `content/data/*.dat` (externalized bitmaps or unknown X6 data blobs)
        for info in &entries {
            let lower = info.name.to_ascii_lowercase();
            if lower.starts_with("content/data/") && lower.ends_with(".dat") {
                if let Some(&zip_idx) = index_by_path.get(&lower) {
                    if seen_zip_indices.insert(zip_idx) {
                        let data_bytes = read_entry_by_index(
                            &mut archive,
                            zip_idx,
                            archive_len,
                            limits,
                            &mut budget_remaining,
                        )?;
                        if data_bytes.len() >= 13 && &data_bytes[0..4] == b"CBMP" {
                            let id = u32::from_le_bytes([
                                data_bytes[4],
                                data_bytes[5],
                                data_bytes[6],
                                data_bytes[7],
                            ]);
                            external_bitmaps.insert(id, data_bytes);
                        } else if lower != "content/data/manifest.dat" {
                            unknown_data_blobs.push((info.name.clone(), data_bytes));
                        }
                    }
                }
            }
        }
    }

    // Pre-load ZCF auxiliary context (`styles/document.cdss`, `META-INF/textinfo.xml`, XMP metadata)
    let mut zcf_ctx = ZcfContext::default();
    if let Some(&cdss_idx) = index_by_path.get("styles/document.cdss") {
        let cdss_bytes = read_entry_by_index(
            &mut archive,
            cdss_idx,
            archive_len,
            limits,
            &mut budget_remaining,
        )?;
        zcf_ctx.stylesheet = parse_document_cdss(&cdss_bytes);
        if zcf_ctx.version_hint.is_none() {
            zcf_ctx.version_hint = zcf_ctx.stylesheet.version;
        }
    }
    if let Some(&ti_idx) = index_by_path.get("meta-inf/textinfo.xml") {
        let ti_bytes = read_entry_by_index(
            &mut archive,
            ti_idx,
            archive_len,
            limits,
            &mut budget_remaining,
        )?;
        let xml = String::from_utf8_lossy(&ti_bytes);
        zcf_ctx.textinfo = parse_textinfo_xml(&xml);
    }
    let mut xmp_metadata_xml: Option<String> = None;
    if let Some(&xmp_idx) = index_by_path.get("meta-inf/metadata.xml") {
        let xmp_bytes = read_entry_by_index(
            &mut archive,
            xmp_idx,
            archive_len,
            limits,
            &mut budget_remaining,
        )?;
        let xml = String::from_utf8_lossy(&xmp_bytes).into_owned();
        // Extract version/page-size hints before scene decoding
        let mut dummy_doc = Document::new_a4("");
        apply_xmp_metadata_xml(&xml, &mut dummy_doc, &mut is_template, &mut zcf_ctx);
        xmp_metadata_xml = Some(xml);
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

    // Parse raw RIFF tree, dereferencing ZCF 16-byte indirect pointers when present!
    let raw_riff_tree =
        parse_riff_with_budget(&primary_bytes, limits, &mut budget_remaining, false)?;
    let mut referenced_file_indices = HashSet::new();
    collect_referenced_zcf_indices(&raw_riff_tree.children, &mut referenced_file_indices);
    for (fidx, path_opt) in file_index_names.iter().enumerate() {
        if let (Some(path), Some(Some(pool_idx))) = (path_opt, file_index_map.get(fidx)) {
            if !referenced_file_indices.contains(&fidx)
                && path != "content/data/manifest.dat"
                && !path
                    .ends_with(".dat")
                    .then(|| data_pool.get(*pool_idx))
                    .flatten()
                    .is_some_and(|b| b.len() >= 13 && &b[0..4] == b"CBMP")
            {
                if let Some(blob) = data_pool.get(*pool_idx) {
                    unknown_data_blobs.push((path.clone(), blob.clone()));
                }
            }
        }
    }

    let resolved_riff_tree =
        if era == CdrZipEra::X6 && is_zcf_indirect_tree(&raw_riff_tree, file_index_map.len()) {
            zcf_ctx.is_zcf = true;
            resolve_zcf_riff_tree(
                raw_riff_tree,
                &data_pool,
                &file_index_map,
                limits,
                &mut budget_remaining,
                &mut warnings,
            )?
        } else {
            raw_riff_tree
        };

    // Share remaining cumulative budget across decoded RGBA bitmaps & scene objects!
    let mut parsed_riff = parse_cdr_riff_tree_with_options(
        resolved_riff_tree,
        limits,
        &external_bitmaps,
        &mut budget_remaining,
        &zcf_ctx,
        opts.default_codepage,
    )?;
    warnings.extend(parsed_riff.warnings);
    if parsed_riff.is_template {
        is_template = true;
    }
    for (path, bytes) in unknown_data_blobs {
        parsed_riff.document.insert_unknown_data_file(path, bytes);
    }
    if parsed_riff.document.custom_icc_profile().is_none() {
        for info in &entries {
            let lower = info.name.to_ascii_lowercase();
            if lower.starts_with("color/") && (lower.ends_with(".icc") || lower.ends_with(".icm")) {
                if let Some(&icc_idx) = index_by_path.get(&lower) {
                    if let Ok(icc_bytes) = read_entry_by_index(
                        &mut archive,
                        icc_idx,
                        archive_len,
                        limits,
                        &mut budget_remaining,
                    ) {
                        let _ = parsed_riff.document.set_custom_icc_profile(icc_bytes);
                        break;
                    }
                }
            }
        }
    }

    // Apply XMP `META-INF/metadata.xml` onto the decoded Document
    if let Some(ref xml) = xmp_metadata_xml {
        apply_xmp_metadata_xml(
            xml,
            &mut parsed_riff.document,
            &mut is_template,
            &mut zcf_ctx,
        );
    }

    // Parse optional `metadata/metadata.xml` (X4–X5 & clean-room synthetic writer)
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

    // Parse optional `color/color.xml` (primary document color space)
    if let Some(&cc_idx) = index_by_path.get("color/color.xml") {
        let cc_bytes = read_entry_by_index(
            &mut archive,
            cc_idx,
            archive_len,
            limits,
            &mut budget_remaining,
        )?;
        let xml = String::from_utf8_lossy(&cc_bytes);
        if let Some(mode) = parse_color_context_xml(&xml) {
            parsed_riff.document.set_color_mode(mode);
        }
    }

    // Parse optional `color/docPalette.xml` (authentic X6–2025) or `color/documentPalette.xml`
    for pal_path in ["color/docpalette.xml", "color/documentpalette.xml"] {
        if let Some(&pal_idx) = index_by_path.get(pal_path) {
            let pal_bytes = read_entry_by_index(
                &mut archive,
                pal_idx,
                archive_len,
                limits,
                &mut budget_remaining,
            )?;
            let xml = String::from_utf8_lossy(&pal_bytes);
            let swatches = parse_doc_palette_xml(&xml);
            if !swatches.is_empty() {
                parsed_riff.document.document_palette_mut().swatches = swatches;
                break;
            }
        }
    }

    // Parse optional `styles/default.xml` (clean-room synthetic X6+)
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
    parsed_riff.document.metadata_mut().source_format = Some(match (era, is_template) {
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
                let form_type = if is_template { *b"CDTO" } else { *b"CDRO" };
                let riff_out = write_cdr_riff_with_options(
                    doc,
                    RiffWriteOptions {
                        form_type,
                        version: 2400,
                        compress_doc: false,
                        externalize_bitmaps: true,
                    },
                )?;
                let mut budget = CodecLimits::default().max_uncompressed_bytes;
                let mut raw_tree = parse_riff_with_budget(
                    &riff_out.riff_bytes,
                    &CodecLimits::default(),
                    &mut budget,
                    false,
                )?;
                let mut data0_blob = Vec::new();
                indirection_rewrite_zcf_tree(&mut raw_tree.children, 2400, &mut data0_blob);
                let root_bytes = write_riff(&raw_tree)?;
                zip_write_file(&mut zip, "content/root.dat", &root_bytes, deflated)?;

                let mut data_file_list = String::new();
                let mut written_paths: HashSet<String> = [
                    "mimetype",
                    "content/root.dat",
                    "content/data/data0.dat",
                    "content/data/manifest.dat",
                    "content/datafilelist.dat",
                    "styles/document.cdss",
                    "styles/default.xml",
                    "meta-inf/metadata.xml",
                    "meta-inf/textinfo.xml",
                    "color/color.xml",
                    "color/docpalette.xml",
                    "previews/thumbnail.png",
                    "metadata/metadata.xml",
                    "color/documentpalette.xml",
                    "preview/thumbnail.bmp",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect();
                if doc.custom_icc_profile().is_some() {
                    written_paths.insert("color/document.icc".to_owned());
                }
                let reserved_unknown_paths: HashSet<String> = doc
                    .unknown_data_files()
                    .keys()
                    .filter_map(|p| sanitize_zip_path(p.as_bytes()).ok())
                    .map(|p| p.to_ascii_lowercase())
                    .filter(|p| !written_paths.contains(p))
                    .collect();

                // Index 0 (`content/data/data0.dat`) carries all 16B externalized `loda` payloads
                zip_write_file(&mut zip, "content/data/data0.dat", &data0_blob, deflated)?;
                data_file_list.push_str("data/data0.dat\n");

                // Write externalized raster payloads into `content/data/XXXX.dat` (avoiding collisions
                // with preserved `unknown_data_files()` via `data/bmp_XXXX.dat`)
                for (bmp_id, payload) in &riff_out.external_bitmaps {
                    let mut rel = format!("data/{bmp_id:04}.dat");
                    let mut full = format!("content/{rel}");
                    let mut lower = full.to_ascii_lowercase();
                    if reserved_unknown_paths.contains(&lower) || written_paths.contains(&lower) {
                        rel = format!("data/bmp_{bmp_id:04}.dat");
                        full = format!("content/{rel}");
                        lower = full.to_ascii_lowercase();
                        let mut seq = 1usize;
                        while reserved_unknown_paths.contains(&lower)
                            || written_paths.contains(&lower)
                        {
                            rel = format!("data/bmp_{bmp_id:04}_{seq}.dat");
                            full = format!("content/{rel}");
                            lower = full.to_ascii_lowercase();
                            seq += 1;
                        }
                    }
                    written_paths.insert(lower);
                    zip_write_file(&mut zip, &full, payload, deflated)?;
                    data_file_list.push_str(&rel);
                    data_file_list.push('\n');
                }
                // Include lightweight manifest data block
                let manifest_rel = "data/manifest.dat";
                let manifest_body = format!(
                    "coree-cdr-x6\npages={}\nobjects={}\nassets={}\n",
                    doc.pages().len(),
                    doc.object_count(),
                    doc.assets().len()
                );
                zip_write_file(
                    &mut zip,
                    &format!("content/{manifest_rel}"),
                    manifest_body.as_bytes(),
                    deflated,
                )?;
                data_file_list.push_str(manifest_rel);
                data_file_list.push('\n');

                // Re-emit preserved unknown X6 `content/data/*.dat` (or other opaque) blobs
                for (unk_path, unk_bytes) in doc.unknown_data_files() {
                    if let Ok(safe_path) = sanitize_zip_path(unk_path.as_bytes()) {
                        let lower = safe_path.to_ascii_lowercase();
                        if written_paths.insert(lower) {
                            zip_write_file(&mut zip, &safe_path, unk_bytes, deflated)?;
                            if let Some(rel) = safe_path.strip_prefix("content/") {
                                data_file_list.push_str(rel);
                                data_file_list.push('\n');
                            }
                        }
                    }
                }

                zip_write_file(
                    &mut zip,
                    "content/dataFileList.dat",
                    data_file_list.as_bytes(),
                    deflated,
                )?;

                // `styles/document.cdss` (authentic X6+ JSON stylesheet)
                let cdss_json = build_document_cdss_json(doc);
                zip_write_file(
                    &mut zip,
                    "styles/document.cdss",
                    cdss_json.as_bytes(),
                    deflated,
                )?;
                // `styles/default.xml` (compatibility metadata)
                let styles_xml = build_default_styles_xml(doc);
                zip_write_file(
                    &mut zip,
                    "styles/default.xml",
                    styles_xml.as_bytes(),
                    deflated,
                )?;

                // `META-INF/metadata.xml` & `META-INF/textinfo.xml`
                let xmp_xml = build_xmp_metadata_xml(doc, is_template);
                zip_write_file(
                    &mut zip,
                    "META-INF/metadata.xml",
                    xmp_xml.as_bytes(),
                    deflated,
                )?;
                let textinfo_xml = build_textinfo_xml(doc);
                zip_write_file(
                    &mut zip,
                    "META-INF/textinfo.xml",
                    textinfo_xml.as_bytes(),
                    deflated,
                )?;

                // `color/color.xml` & `color/docPalette.xml`
                let color_ctx_xml = build_color_context_xml(doc);
                zip_write_file(
                    &mut zip,
                    "color/color.xml",
                    color_ctx_xml.as_bytes(),
                    deflated,
                )?;
                let doc_pal_xml = build_zcf_doc_palette_xml(doc);
                zip_write_file(
                    &mut zip,
                    "color/docPalette.xml",
                    doc_pal_xml.as_bytes(),
                    deflated,
                )?;

                // `previews/thumbnail.png` rendered from page 0 scene objects
                let thumb_rgba = render_doc_mini_thumbnail_rgba(doc, 128, 128);
                let thumb_png = encode_png_rgba(128, 128, &thumb_rgba)?;
                zip_write_file(&mut zip, "previews/thumbnail.png", &thumb_png, deflated)?;
            }
        }

        // 3. `metadata/metadata.xml`, `color/documentPalette.xml`, optional `color/document.icc`, & `preview/thumbnail.bmp`
        let meta_xml = build_metadata_xml(doc, is_template);
        zip_write_file(
            &mut zip,
            "metadata/metadata.xml",
            meta_xml.as_bytes(),
            deflated,
        )?;

        let color_xml = build_color_palette_xml(doc);
        zip_write_file(
            &mut zip,
            "color/documentPalette.xml",
            color_xml.as_bytes(),
            deflated,
        )?;

        let thumb_rgba = render_doc_mini_thumbnail_rgba(doc, 64, 64);
        let thumb_bmp = encode_bmp_file_rgba(64, 64, &thumb_rgba)?;
        zip_write_file(&mut zip, "preview/thumbnail.bmp", &thumb_bmp, deflated)?;

        if let Some(icc_bytes) = doc.custom_icc_profile() {
            zip_write_file(&mut zip, "color/document.icc", icc_bytes, deflated)?;
        }

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
// ZCF Indirection Rewriter & Pure-Rust Structural XML Helpers (`quick-xml`)
// =============================================================================

fn collect_referenced_zcf_indices(nodes: &[RiffNode], out: &mut HashSet<usize>) {
    for node in nodes {
        match node {
            RiffNode::List { children, .. } => collect_referenced_zcf_indices(children, out),
            RiffNode::Chunk { data, .. } => {
                if data.len() == 16 {
                    let fidx = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                    let dlen = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                    if fidx != u32::MAX && dlen > 0 {
                        out.insert(fidx as usize);
                    }
                }
            }
        }
    }
}

fn indirection_rewrite_zcf_tree(nodes: &mut [RiffNode], version: u16, data0_blob: &mut Vec<u8>) {
    for node in nodes {
        match node {
            RiffNode::List { children, .. } => {
                indirection_rewrite_zcf_tree(children, version, data0_blob);
            }
            RiffNode::Chunk { fourcc, data, .. } => {
                if fourcc == b"vrsn" || fourcc == b"fver" {
                    let mut ptr = Vec::with_capacity(16);
                    ptr.extend_from_slice(&u32::MAX.to_le_bytes());
                    ptr.extend_from_slice(&2u32.to_le_bytes());
                    ptr.extend_from_slice(&version.to_le_bytes());
                    ptr.extend_from_slice(&[0u8; 6]);
                    *data = ptr;
                } else if fourcc != b"loda" && data.len() <= 8 {
                    let len = u32::try_from(data.len()).unwrap_or(0);
                    let mut ptr = Vec::with_capacity(16);
                    ptr.extend_from_slice(&u32::MAX.to_le_bytes());
                    ptr.extend_from_slice(&len.to_le_bytes());
                    ptr.extend_from_slice(data);
                    ptr.resize(16, 0);
                    *data = ptr;
                } else {
                    let offset = u32::try_from(data0_blob.len()).unwrap_or(u32::MAX);
                    let len = u32::try_from(data.len()).unwrap_or(0);
                    data0_blob.extend_from_slice(data);
                    let mut ptr = Vec::with_capacity(16);
                    ptr.extend_from_slice(&0u32.to_le_bytes()); // file_index = 0 -> data/data0.dat
                    ptr.extend_from_slice(&len.to_le_bytes());
                    ptr.extend_from_slice(&offset.to_le_bytes());
                    ptr.extend_from_slice(&0u32.to_le_bytes());
                    *data = ptr;
                }
            }
        }
    }
}

fn xml_escape(s: &str) -> String {
    quick_xml::escape::escape(s).into_owned()
}

fn parse_simple_xml_tags(xml: &str) -> Option<HashMap<String, String>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<(String, String)> = Vec::new();
    let mut out: HashMap<String, String> = HashMap::new();
    let mut total_events = 0usize;

    loop {
        total_events += 1;
        if total_events > 65_536 {
            return None;
        }
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::DocType(_)) | Err(_) => return None,
            Ok(Event::Comment(_)) | Ok(Event::Decl(_)) | Ok(Event::PI(_)) => {}
            Ok(Event::Start(e)) => {
                let name = std::str::from_utf8(e.name().as_ref()).ok()?.to_owned();
                stack.push((name, String::new()));
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::End(e)) => {
                let eq = e.name();
                let end_name = std::str::from_utf8(eq.as_ref()).ok()?;
                let (tag, text) = stack.pop()?;
                if tag != end_name {
                    return None;
                }
                out.entry(tag).or_insert_with(|| text.trim().to_owned());
            }
            Ok(Event::Text(t)) => {
                if let Some((_, buf)) = stack.last_mut() {
                    let s = t.xml10_content().ok()?;
                    buf.push_str(&s);
                }
            }
            Ok(Event::CData(c)) => {
                if let Some((_, buf)) = stack.last_mut() {
                    let s = c.decode().ok()?;
                    buf.push_str(&s);
                }
            }
            Ok(Event::GeneralRef(r)) => {
                let resolved = if let Ok(Some(ch)) = r.resolve_char_ref() {
                    ch.to_string()
                } else {
                    let rb: &[u8] = &r;
                    match rb {
                        b"amp" => "&".to_owned(),
                        b"lt" => "<".to_owned(),
                        b"gt" => ">".to_owned(),
                        b"quot" => "\"".to_owned(),
                        b"apos" => "'".to_owned(),
                        _ => return None,
                    }
                };
                if let Some((_, buf)) = stack.last_mut() {
                    buf.push_str(&resolved);
                }
            }
        }
    }
    Some(out)
}

fn build_document_cdss_json(doc: &Document) -> String {
    let mut style_map = serde_json::Map::new();
    let mode_color = match doc.color_mode() {
        ColorMode::Cmyk => "CMYK,USER,0,0,0,100,100",
        ColorMode::Rgb => "RGB255,USER,0,0,0,100",
    };
    style_map.insert(
        "~graphic~".to_owned(),
        serde_json::json!({
            "outline": { "width": "2000", "color": mode_color },
            "fill": { "type": "0" }
        }),
    );
    style_map.insert(
        "~artistic~".to_owned(),
        serde_json::json!({
            "character": { "latin": { "font": "Noto Sans", "size": "84667" } }
        }),
    );
    style_map.insert(
        "~paragraph~".to_owned(),
        serde_json::json!({
            "character": { "latin": { "font": "Noto Sans", "size": "42333" } }
        }),
    );
    for os in doc.object_styles() {
        let mut entry = serde_json::Map::new();
        entry.insert(
            "name".to_owned(),
            serde_json::Value::String(os.name.clone()),
        );
        if let Some(coree_core::Fill::Solid(c)) = os.fill.as_ref() {
            let [r, g, b, a] = c.to_rgba8();
            entry.insert(
                "fill".to_owned(),
                serde_json::json!({
                    "type": "1",
                    "primaryColor": format!(
                        "RGB255,USER,{r},{g},{b},{}",
                        (f32::from(a) * 100.0 / 255.0).round() as u32
                    )
                }),
            );
        }
        if let Some(ref outl) = os.outline {
            let [r, g, b, _] = outl.color.to_rgba8();
            entry.insert(
                "outline".to_owned(),
                serde_json::json!({
                    "width": ((outl.width_mm * 10_000.0).round() as i32).to_string(),
                    "color": format!("RGB255,USER,{r},{g},{b},100")
                }),
            );
        }
        style_map.insert(os.id.0.to_string(), serde_json::Value::Object(entry));
    }
    serde_json::json!({
        "version": 2300,
        "style": style_map
    })
    .to_string()
}

fn build_xmp_metadata_xml(doc: &Document, is_template: bool) -> String {
    let format_str = if is_template {
        "application/x-vnd.corel.zcf.draw.template+zip"
    } else {
        "application/x-vnd.corel.zcf.draw.document+zip"
    };
    let (pw_cdr, ph_cdr) = doc
        .pages()
        .first()
        .map_or((2_100_000i32, 2_970_000i32), |p| {
            (
                crate::coord::mm_to_cdr32(p.width_mm),
                crate::coord::mm_to_cdr32(p.height_mm),
            )
        });
    let mut kw_li = String::new();
    for kw in &doc.metadata().keywords {
        kw_li.push_str(&format!("<rdf:li>{}</rdf:li>", xml_escape(kw)));
    }
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n",
            "  <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n",
            "    <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:cdr=\"http://ns.coree.dev/cdr/1.0/\" xmlns:crl=\"http://ns.coree.dev/crl/1.0/\" xmlns:cdrinfo=\"http://ns.coree.dev/cdrinfo/1.0/\">\n",
            "      <dc:format>{fmt}</dc:format>\n",
            "      <cdr:CoreVersion>2400</cdr:CoreVersion>\n",
            "      <dc:title><rdf:Alt><rdf:li>{title}</rdf:li></rdf:Alt></dc:title>\n",
            "      <dc:creator><rdf:Seq><rdf:li>{author}</rdf:li></rdf:Seq></dc:creator>\n",
            "      <crl:LastAuthor>{author}</crl:LastAuthor>\n",
            "      <crl:Subject><rdf:Bag><rdf:li>{subj}</rdf:li></rdf:Bag></crl:Subject>\n",
            "      <crl:Notes><rdf:Bag><rdf:li>{notes}</rdf:li></rdf:Bag></crl:Notes>\n",
            "      <crl:Keywords><rdf:Bag>{kw_li}</rdf:Bag></crl:Keywords>\n",
            "      <cdrinfo:PageWidth>{pw_cdr}</cdrinfo:PageWidth>\n",
            "      <cdrinfo:PageHeight>{ph_cdr}</cdrinfo:PageHeight>\n",
            "    </rdf:Description>\n",
            "  </rdf:RDF>\n",
            "</x:xmpmeta>\n"
        ),
        fmt = xml_escape(format_str),
        title = xml_escape(doc.title()),
        author = xml_escape(&doc.metadata().author),
        subj = xml_escape(&doc.metadata().subject),
        notes = xml_escape(&doc.metadata().notes),
        kw_li = kw_li,
        pw_cdr = pw_cdr,
        ph_cdr = ph_cdr,
    )
}

fn build_textinfo_xml(doc: &Document) -> String {
    let mut pages_xml = String::new();
    let mut layers_xml = String::new();
    let mut objs_xml = String::new();
    for p in doc.pages() {
        pages_xml.push_str(&format!("<rdf:li>{}</rdf:li>", xml_escape(&p.name)));
        for l in &p.layers {
            layers_xml.push_str(&format!("<rdf:li>{}</rdf:li>", xml_escape(&l.name)));
            for &oid in &l.objects {
                if let Some(obj) = doc.object(oid) {
                    objs_xml.push_str(&format!("<rdf:li>{}</rdf:li>", xml_escape(&obj.name)));
                }
            }
        }
    }
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n",
            "  <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n",
            "    <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:cdr=\"http://ns.coree.dev/cdr/1.0/\" xmlns:crl=\"http://ns.coree.dev/crl/1.0/\" xmlns:cdrinfo=\"http://ns.coree.dev/cdrinfo/1.0/\">\n",
            "      <PageNames><rdf:Bag>{pages_xml}</rdf:Bag></PageNames>\n",
            "      <LayerNames><rdf:Bag>{layers_xml}</rdf:Bag></LayerNames>\n",
            "      <ObjectNames><rdf:Bag>{objs_xml}</rdf:Bag></ObjectNames>\n",
            "    </rdf:Description>\n",
            "  </rdf:RDF>\n",
            "</x:xmpmeta>\n"
        ),
        pages_xml = pages_xml,
        layers_xml = layers_xml,
        objs_xml = objs_xml,
    )
}

fn build_color_context_xml(doc: &Document) -> String {
    let model = match doc.color_mode() {
        ColorMode::Cmyk => "Cmyk",
        ColorMode::Rgb => "Rgb",
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Color><ColorContext><ColorModel>{model}</ColorModel></ColorContext></Color>\n"
    )
}

fn format_zcf_palette_color(name: &str, c: &Color) -> String {
    let (cs, tints) = match c {
        Color::Cmyk { c, m, y, k, .. } => ("CMYK", format!("{:.4},{:.4},{:.4},{:.4}", c, m, y, k)),
        Color::Rgba { r, g, b, .. } => ("RGB", format!("{:.4},{:.4},{:.4}", r, g, b)),
        Color::Gray { v, .. } => ("GRAY", format!("{:.4}", v)),
        Color::Lab { l, a_ch, b_ch, .. } => ("LAB", format!("{:.2},{:.2},{:.2}", l, a_ch, b_ch)),
        Color::Hsl { h, s, l, .. } => ("HSL", format!("{:.2},{:.4},{:.4}", h, s, l)),
        Color::Spot { fallback_cmyk, .. } => (
            "SPOT",
            format!(
                "{:.2},{:.2},{:.2},{:.2}",
                fallback_cmyk[0] * 100.0,
                fallback_cmyk[1] * 100.0,
                fallback_cmyk[2] * 100.0,
                fallback_cmyk[3] * 100.0
            ),
        ),
    };
    format!(
        "  <color cs=\"{cs}\" name=\"{}\" tints=\"{tints}\"/>\n",
        xml_escape(name)
    )
}

fn build_zcf_doc_palette_xml(doc: &Document) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<palette name=\"Document Palette\">\n<colors><page>\n",
    );
    let mut count = 0usize;
    for sw in &doc.document_palette().swatches {
        xml.push_str(&format_zcf_palette_color(&sw.name, &sw.color));
        count += 1;
    }
    if count < 2 {
        xml.push_str("  <color cs=\"CMYK\" name=\"Black\" tints=\"0,0,0,1\"/>\n");
        if count == 0 {
            xml.push_str("  <color cs=\"CMYK\" name=\"White\" tints=\"0,0,0,0\"/>\n");
        }
    }
    xml.push_str("</page></colors></palette>\n");
    xml
}

fn build_metadata_xml(doc: &Document, is_template: bool) -> String {
    let kw = doc.metadata().keywords.join(", ");
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
        xml_escape(doc.title()),
        xml_escape(&doc.metadata().author),
        xml_escape(&doc.metadata().subject),
        xml_escape(&kw),
        xml_escape(&doc.metadata().notes),
        xml_escape(&doc.metadata().app_version),
        if is_template { "true" } else { "false" }
    )
}

fn apply_metadata_xml(xml: &str, doc: &mut Document, is_template: &mut bool) {
    let Some(tags) = parse_simple_xml_tags(xml) else {
        return;
    };
    if let Some(title) = tags.get("title") {
        if !title.is_empty() {
            doc.set_title(title.clone());
        }
    }
    if let Some(author) = tags.get("author") {
        doc.metadata_mut().author = author.clone();
    }
    if let Some(subject) = tags.get("subject") {
        doc.metadata_mut().subject = subject.clone();
    }
    if let Some(kw) = tags.get("keywords") {
        if !kw.is_empty() {
            doc.metadata_mut().keywords = kw
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    if let Some(notes) = tags.get("notes") {
        doc.metadata_mut().notes = notes.clone();
    }
    if let Some(tpl) = tags.get("isTemplate") {
        if tpl.eq_ignore_ascii_case("true") {
            *is_template = true;
        }
    }
}

fn build_default_styles_xml(doc: &Document) -> String {
    let mode = match doc.color_mode() {
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
        dpi = doc.dpi()
    )
}

fn apply_default_styles_xml(xml: &str, doc: &mut Document) {
    let Some(tags) = parse_simple_xml_tags(xml) else {
        return;
    };
    if let Some(mode) = tags.get("colorMode") {
        if mode.eq_ignore_ascii_case("RGB") {
            doc.set_color_mode(ColorMode::Rgb);
        } else if mode.eq_ignore_ascii_case("CMYK") {
            doc.set_color_mode(ColorMode::Cmyk);
        }
    }
    if let Some(dpi_str) = tags.get("dpi") {
        if let Ok(dpi) = dpi_str.parse::<f32>() {
            if dpi.is_finite() && dpi >= 1.0 {
                doc.set_dpi(dpi);
            }
        }
    }
}

fn build_color_palette_xml(doc: &Document) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<palette name=\"Document Palette\">\n",
    );
    for sw in &doc.document_palette().swatches {
        let hex = sw.color.to_hex_rgba();
        xml.push_str(&format!(
            "  <swatch name=\"{}\" hex=\"{hex}\"/>\n",
            xml_escape(&sw.name)
        ));
    }
    xml.push_str("</palette>\n");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{CornerStyle, Fill, NodePayload, ObjectId, ParametricShape, SceneObject};
    use std::io::Read;

    #[test]
    fn x6_cdro_writer_emits_external_loda_pointers_cdss_xmp_palette_and_unknown_data() {
        let mut doc = Document::new_a4("X6 CDRO ZCF");
        let layer_id = doc.pages()[0].layers[0].id;
        let rect = SceneObject::new(
            ObjectId(0),
            "Box",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 50.0,
                height: 30.0,
                corner_radii: [0.0; 4],
                corner_style: CornerStyle::default(),
                lock_corners: false,
            }),
        )
        .with_fill(Fill::Solid(Color::spot(
            "PantoneMagenta",
            1.0,
            [0.0, 1.0, 0.0, 0.0],
        )));
        doc.add_object(rect, Some(layer_id)).unwrap();
        doc.refresh_document_palette();
        doc.insert_unknown_data_file("content/data/custom_unknown.dat", vec![1, 2, 3, 4, 5]);

        let zip_bytes = write_cdr_zip(&doc, CdrZipEra::X6, false).unwrap();
        let mut archive = ZipArchive::new(Cursor::new(&zip_bytes)).unwrap();

        let mut root_bytes = Vec::new();
        archive
            .by_name("content/root.dat")
            .unwrap()
            .read_to_end(&mut root_bytes)
            .unwrap();
        assert_eq!(&root_bytes[8..12], b"CDRO");

        let mut data0_bytes = Vec::new();
        archive
            .by_name("content/data/data0.dat")
            .unwrap()
            .read_to_end(&mut data0_bytes)
            .unwrap();
        assert!(!data0_bytes.is_empty());

        let mut dfl = String::new();
        archive
            .by_name("content/dataFileList.dat")
            .unwrap()
            .read_to_string(&mut dfl)
            .unwrap();
        assert_eq!(dfl.lines().next(), Some("data/data0.dat"));

        let mut cdss = String::new();
        archive
            .by_name("styles/document.cdss")
            .unwrap()
            .read_to_string(&mut cdss)
            .unwrap();
        let cdss_val: serde_json::Value = serde_json::from_str(&cdss).unwrap();
        assert_eq!(cdss_val["version"], 2300);
        assert!(cdss_val["style"].as_object().unwrap().len() >= 3);

        let mut pal_xml = String::new();
        archive
            .by_name("color/docPalette.xml")
            .unwrap()
            .read_to_string(&mut pal_xml)
            .unwrap();
        let swatches = parse_doc_palette_xml(&pal_xml);
        assert!(swatches.len() >= 2);

        let mut thumb = Vec::new();
        archive
            .by_name("previews/thumbnail.png")
            .unwrap()
            .read_to_end(&mut thumb)
            .unwrap();
        assert!(thumb.starts_with(b"\x89PNG\r\n\x1a\n"));

        let parsed = parse_cdr_zip(&zip_bytes, &CodecLimits::default()).unwrap();
        assert_eq!(parsed.document.title(), "X6 CDRO ZCF");
        assert_eq!(
            parsed
                .document
                .unknown_data_files()
                .get("content/data/custom_unknown.dat"),
            Some(&vec![1u8, 2, 3, 4, 5])
        );
    }
}
