// SPDX-License-Identifier: MPL-2.0

//! Clean-room bridge to `coree-cdr` container parsers, exporters, and structural
//! inspection utilities across `.cdr`, `.cdt`, `.cmx`, `.cpt`, `.coree`, and `.svg`.

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
    export_cdr, export_cdt, read_file_bounded, CdrError, CdrResult, CdrWriteVersion, ChunkSummary,
    CodecLimits, UnknownChunkSummary, ZipEntrySummary,
};
use coree_core::{Document, NodePayload};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::Path;

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
        }
    }
}

/// Complete structural inspection report for `.cdr`, `.cdt`, `.cmx`, `.cpt`, `.coree`, or `.svg`.
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

/// Imports and structurally inspects any `.cdr`, `.cdt`, `.cmx`, or `.cpt` binary file.
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

/// Inspects any supported vector container file (`.cdr`, `.cdt`, `.cmx`, `.cpt`, `.coree`, `.svg`).
pub fn inspect_any_file(path: &Path) -> Result<FileInspectionReport, String> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let file_size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    match ext.as_str() {
        "coree" => {
            let doc =
                coree_io::load_coree(path).map_err(|e| format!("failed to load .coree: {e}"))?;
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
        "svg" | "svgz" => {
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
        _ => import_and_inspect_binary_file(path)
            .map(|res| res.report)
            .map_err(|e| format!("inspection failed: {e}")),
    }
}

/// Builds a structural report from an already-loaded in-memory [`Document`].
#[must_use]
pub fn inspect_in_memory_document(doc: &Document) -> FileInspectionReport {
    build_report_from_doc(
        doc,
        ContainerFormat::NativeCoree,
        doc.metadata
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
