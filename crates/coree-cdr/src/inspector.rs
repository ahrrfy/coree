// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room **CDR Structural Inspector** powering `coree inspect file.cdr`.
//!
//! Inspects RIFF CDR (v4–v13), ZIP/OPC CDR (X4–2025), CDT, CMX (16/32-bit),
//! and CPT (PHOTO-PAINT) binaries and produces both structured reports and
//! human-readable diagnostic summaries.

use crate::cdr_riff::{is_cdr_form_type, is_cdt_form_type, parse_cdr_riff};
use crate::cdr_zip::{is_zip_bytes, parse_cdr_zip, CdrZipEra};
use crate::cmx::{is_cmx_form_type, parse_cmx_bytes, CmxPrecision};
use crate::coord::color_model_name;
use crate::cpt::{detect_cpt_version, parse_cpt_bytes};
use crate::error::{CdrError, CdrResult};
use crate::limits::{read_file_bounded, CodecLimits};
use crate::riff::{fourcc_to_string, RiffDocument, RiffNode};
use crate::CorelFormat;
use coree_core::{Document, NodePayload};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::Path;

/// Summary of one node inside a parsed RIFF chunk hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkSummary {
    pub fourcc: String,
    pub size: u64,
    pub depth: usize,
    pub is_list: bool,
    pub compressed: bool,
}

/// Summary of one entry inside a ZIP/OPC CDR or CDT container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZipEntrySummary {
    pub name: String,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}

/// Summary of an unrecognized chunk preserved by Unknown-Chunk Passthrough.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownChunkSummary {
    pub fourcc: String,
    pub byte_len: usize,
    pub version_hint: u32,
    pub scope: String,
}

/// Complete structural inspection report for any supported Corel binary file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CdrInspectionReport {
    pub detected_format: CorelFormat,
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

impl CdrInspectionReport {
    /// Formats this inspection report into a crisp human-readable summary string.
    #[must_use]
    pub fn format_report(&self) -> String {
        format_report(self)
    }
}

/// Inspects raw bytes of any supported Corel format using default [`CodecLimits`].
pub fn inspect_corel_bytes(bytes: &[u8]) -> CdrResult<CdrInspectionReport> {
    inspect_corel_bytes_with_limits(bytes, &CodecLimits::default())
}

/// Inspects raw bytes of any supported Corel format under explicit [`CodecLimits`].
pub fn inspect_corel_bytes_with_limits(
    bytes: &[u8],
    limits: &CodecLimits,
) -> CdrResult<CdrInspectionReport> {
    let file_size_bytes = bytes.len() as u64;

    // 1. ZIP/OPC CDR X4–X5, CDR X6–2025, or CDT ZIP
    if is_zip_bytes(bytes) {
        let parsed = parse_cdr_zip(bytes, limits)?;
        let (detected_format, format_era) = match (parsed.era, parsed.is_template) {
            (CdrZipEra::X4, false) => (CorelFormat::CdrZipX4, "CDR ZIP/OPC X4–X5".to_owned()),
            (CdrZipEra::X6, false) => (CorelFormat::CdrZipX6, "CDR ZIP/OPC X6–2025".to_owned()),
            (CdrZipEra::X4, true) => (CorelFormat::CdtZip, "CDT Template ZIP X4–X5".to_owned()),
            (CdrZipEra::X6, true) => (CorelFormat::CdtZip, "CDT Template ZIP X6–2025".to_owned()),
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
        return Ok(build_report_from_doc(
            &parsed.document,
            detected_format,
            format_era,
            file_size_bytes,
            Some(u32::from(parsed.version)),
            chunks,
            zip_entries,
            parsed.warnings,
        ));
    }

    // 2. PHOTO-PAINT CPT (native CPT7/8/9 or RIFF CPT)
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
            CorelFormat::Cpt {
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
        return Ok(report);
    }

    // 3. RIFF / RIFX containers (CDR, CDT, CMX)
    if bytes.len() >= 12 && (&bytes[0..4] == b"RIFF" || &bytes[0..4] == b"RIFX") {
        let form: [u8; 4] = [bytes[8], bytes[9], bytes[10], bytes[11]];
        if is_cmx_form_type(&form) {
            let parsed = parse_cmx_bytes(bytes, limits)?;
            let (fmt, era) = match parsed.precision {
                CmxPrecision::Bit16 => (CorelFormat::Cmx16, "CMX 16-bit Presentation Exchange"),
                CmxPrecision::Bit32 => (CorelFormat::Cmx32, "CMX 32-bit Presentation Exchange"),
            };
            let chunks = summarize_riff_tree(&parsed.riff_tree);
            return Ok(build_report_from_doc(
                &parsed.document,
                fmt,
                era.to_owned(),
                file_size_bytes,
                None,
                chunks,
                Vec::new(),
                parsed.warnings,
            ));
        }
        if is_cdr_form_type(&form) || is_cdt_form_type(&form) {
            let parsed = parse_cdr_riff(bytes, limits, &HashMap::new())?;
            let (fmt, era) = if parsed.is_template {
                (
                    CorelFormat::CdtRiff {
                        version: parsed.version,
                    },
                    format!("CDT RIFF Template v{}", parsed.version / 100),
                )
            } else {
                (
                    CorelFormat::CdrRiff {
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
            return Ok(build_report_from_doc(
                &parsed.document,
                fmt,
                era,
                file_size_bytes,
                Some(u32::from(parsed.version)),
                chunks,
                Vec::new(),
                parsed.warnings,
            ));
        }
    }

    Err(CdrError::UnsupportedFormat(
        "unrecognized file signature (expected RIFF CDR/CDT/CMX, ZIP CDR/CDT, or CPT)".to_owned(),
    ))
}

/// Reads and inspects a `.cdr`/`.cdt`/`.cmx`/`.cpt` file on disk using default [`CodecLimits`].
pub fn inspect_corel_file(path: &Path) -> CdrResult<CdrInspectionReport> {
    inspect_corel_file_with_limits(path, &CodecLimits::default())
}

/// Reads and inspects a `.cdr`/`.cdt`/`.cmx`/`.cpt` file on disk under explicit [`CodecLimits`],
/// enforcing bounded disk reads before parsing.
pub fn inspect_corel_file_with_limits(
    path: &Path,
    limits: &CodecLimits,
) -> CdrResult<CdrInspectionReport> {
    let bytes = read_file_bounded(path, limits)?;
    inspect_corel_bytes_with_limits(&bytes, limits)
}

/// Formats a [`CdrInspectionReport`] into a structured, human-readable multi-line report.
#[must_use]
pub fn format_report(report: &CdrInspectionReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "=== coree CDR Structural Inspector ===");
    let _ = writeln!(
        out,
        "Format:      {} ({:?})",
        report.format_era, report.detected_format
    );
    let _ = writeln!(out, "File Size:   {} bytes", report.file_size_bytes);
    if let Some(v) = report.version_number {
        let _ = writeln!(out, "Version:     {v}");
    }
    let _ = writeln!(
        out,
        "Scene Graph: {} page(s), {} layer(s), {} object(s), {} raster asset(s)",
        report.page_count, report.layer_count, report.object_count, report.bitmap_count
    );
    let _ = writeln!(
        out,
        "Fonts ({}):  {}",
        report.fonts.len(),
        if report.fonts.is_empty() {
            "(none)".to_owned()
        } else {
            report.fonts.join(", ")
        }
    );
    let _ = writeln!(
        out,
        "Colors ({}): {}",
        report.color_models.len(),
        if report.color_models.is_empty() {
            "(none)".to_owned()
        } else {
            report.color_models.join(", ")
        }
    );

    if !report.zip_entries.is_empty() {
        let _ = writeln!(out, "\nZIP Entries ({}):", report.zip_entries.len());
        for entry in &report.zip_entries {
            let _ = writeln!(
                out,
                "  - {} ({} -> {} bytes)",
                entry.name, entry.compressed_size, entry.uncompressed_size
            );
        }
    }

    if !report.chunks.is_empty() {
        let _ = writeln!(out, "\nRIFF Chunk Tree ({}):", report.chunks.len());
        for ch in &report.chunks {
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

    if !report.unknown_chunks.is_empty() {
        let _ = writeln!(
            out,
            "\nUnknown Chunks Preserved ({}):",
            report.unknown_chunks.len()
        );
        for unk in &report.unknown_chunks {
            let _ = writeln!(
                out,
                "  - '{}' [{}] ({} bytes, hint v{})",
                unk.fourcc, unk.scope, unk.byte_len, unk.version_hint
            );
        }
    }

    if !report.warnings.is_empty() {
        let _ = writeln!(out, "\nWarnings ({}):", report.warnings.len());
        for w in &report.warnings {
            let _ = writeln!(out, "  ! {w}");
        }
    }

    out
}

#[allow(clippy::too_many_arguments)]
fn build_report_from_doc(
    doc: &Document,
    detected_format: CorelFormat,
    format_era: String,
    file_size_bytes: u64,
    version_number: Option<u32>,
    chunks: Vec<ChunkSummary>,
    zip_entries: Vec<ZipEntrySummary>,
    warnings: Vec<String>,
) -> CdrInspectionReport {
    let page_count = doc.pages.len();
    let layer_count: usize = doc.pages.iter().map(|p| p.layers.len()).sum();
    let object_count = doc.objects.len();
    let bitmap_count = doc.assets.len();

    let mut fonts = BTreeSet::new();
    let mut color_models = BTreeSet::new();
    let mut unknown_chunks = Vec::new();

    for unk in &doc.unknown_doc_chunks {
        unknown_chunks.push(UnknownChunkSummary {
            fourcc: unk.fourcc_str(),
            byte_len: unk.raw_bytes.len(),
            version_hint: unk.version_hint,
            scope: "document".to_owned(),
        });
    }

    for obj in doc.objects.values() {
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

    for sw in &doc.document_palette.swatches {
        color_models.insert(color_model_name(&sw.color).to_owned());
    }

    CdrInspectionReport {
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
