// SPDX-License-Identifier: MIT OR Apache-2.0

//! Universal offline file open, import, save, and export pipeline bridging
//! `coree-io`, `coree-cdr`, `coree-prod`, and `coree-render`.

use crate::cdr_bridge::{
    classify_magic_bytes, import_and_inspect_binary_file, is_coree_archive, lower_extension,
    peek_file_header, write_cdr_file, write_cdt_file, write_cmx_file, FileInspectionReport,
    MagicKind,
};
use coree_cdr::CdrWriteVersion;
use coree_core::math::MM_PER_INCH;
use coree_core::{
    Affine2D, ColorMode, Document, Effect, Fill, NodePayload, ObjectId, OutlineStyle,
    RasterLayerData, SceneObject,
};
use coree_io::{
    atomic_write_bytes, export_dxf_with_options, export_eps, export_pdf, export_plt_with_options,
    export_raster, export_svg, import_dxf, import_eps, import_pdf, import_plt, import_psd,
    import_raster, import_svg, load_coree, save_coree_with_preview, DxfExportOptions,
    PdfColorSpace, PdfExportOptions, PltExportOptions, RasterExportOptions, RasterFormat,
};
use coree_prod::{export_laser_gcode, plan_laser_toolpath, GcodeDialect, LaserJobConfig};
use coree_render::{render_page_to_rgba, ViewMode};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

/// Maximum text file read size for CSV / Rhai script inputs (64 MiB).
pub const MAX_TEXT_READ_BYTES: u64 = 64 * 1024 * 1024;

/// Maximum object nesting depth during recursive document import cloning.
const MAX_IMPORT_DEPTH: usize = 64;

/// Safely reads a UTF-8 text file bounded to `max_bytes` to avoid unbounded OOM reads.
pub fn read_text_file_bounded(path: &Path, max_bytes: u64) -> Result<String, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("failed to open '{}': {e}", path.display()))?;
    if let Ok(meta) = file.metadata() {
        if meta.len() > max_bytes {
            return Err(format!(
                "file '{}' is too large ({} bytes, limit {} bytes)",
                path.display(),
                meta.len(),
                max_bytes
            ));
        }
    }
    let mut buf = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|e| format!("failed to read '{}': {e}", path.display()))?;
    if (buf.len() as u64) > max_bytes {
        return Err(format!(
            "file '{}' exceeds maximum allowed size of {} bytes",
            path.display(),
            max_bytes
        ));
    }
    String::from_utf8(buf).map_err(|e| format!("file '{}' is not valid UTF-8: {e}", path.display()))
}

/// Export configuration shared by the CLI `convert` subcommand and GUI Export dialog.
#[derive(Debug, Clone)]
pub struct UniversalExportOptions {
    pub cdr_version: CdrWriteVersion,
    pub dpi: f32,
    pub force_cmyk: bool,
    pub crop_marks: bool,
    pub registration_marks: bool,
    pub color_calibration_bars: bool,
    pub page_info_slug: bool,
    pub include_bleed: bool,
    pub cut_layers_only: bool,
    pub page_index: usize,
    pub jpeg_quality: u8,
    pub gcode_dialect: GcodeDialect,
}

impl Default for UniversalExportOptions {
    fn default() -> Self {
        Self {
            cdr_version: CdrWriteVersion::ZipX6,
            dpi: 300.0,
            force_cmyk: false,
            crop_marks: false,
            registration_marks: false,
            color_calibration_bars: false,
            page_info_slug: false,
            include_bleed: true,
            cut_layers_only: false,
            page_index: 0,
            jpeg_quality: 92,
            gcode_dialect: GcodeDialect::Grbl11,
        }
    }
}

/// Result of opening any supported file format into a studio [`Document`].
#[derive(Debug, Clone)]
pub struct OpenedDocument {
    pub document: Document,
    pub format_tag: String,
    pub cdr_version: Option<String>,
    pub warnings: Vec<String>,
    pub inspection: Option<FileInspectionReport>,
}

/// Opens any supported vector, native, CAD, print, or raster file into a [`Document`].
///
/// Uses magic-byte detection FIRST so `.cdr.zip`, `.cdt.zip`, extensionless files, and
/// misnamed files open reliably without false rejections.
pub fn open_any_document(path: &Path) -> Result<OpenedDocument, String> {
    let ext = lower_extension(path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_owned();

    let header = peek_file_header(path)?;
    let magic = classify_magic_bytes(&header);

    // 1. Magic-byte-first dispatch
    match magic {
        MagicKind::Zip => {
            if ext == "coree" {
                match open_coree_doc(path) {
                    Ok(doc) => return Ok(doc),
                    Err(coree_err) => {
                        if let Ok(cdr) = open_binary_cdr_doc(path) {
                            return Ok(cdr);
                        }
                        return Err(coree_err);
                    }
                }
            }
            if is_coree_archive(path) {
                return open_coree_doc(path);
            }
            match open_binary_cdr_doc(path) {
                Ok(cdr) => return Ok(cdr),
                Err(cdr_err) => {
                    if let Ok(doc) = open_coree_doc(path) {
                        return Ok(doc);
                    }
                    return Err(cdr_err);
                }
            }
        }
        MagicKind::RiffCdr | MagicKind::Cpt => {
            return open_binary_cdr_doc(path);
        }
        MagicKind::Pdf => {
            return open_pdf_doc(path);
        }
        MagicKind::Psd => {
            return open_psd_doc(path);
        }
        MagicKind::Eps => {
            return open_eps_doc(path);
        }
        MagicKind::Svg => {
            return open_svg_doc(path);
        }
        MagicKind::Raster(tag) => {
            return open_raster_doc(path, &stem, tag);
        }
        MagicKind::Dxf => {
            return open_dxf_doc(path);
        }
        MagicKind::Plt => {
            return open_plt_doc(path);
        }
        MagicKind::Unknown => {}
    }

    // 2. Extension-guided fallback (provides accurate format errors on truncated/malformed inputs)
    match ext.as_str() {
        "coree" => open_coree_doc(path),
        "cdr" | "cdt" | "cmx" | "cpt" => open_binary_cdr_doc(path),
        "pdf" | "ai" => open_pdf_doc(path),
        "eps" | "ps" => open_eps_doc(path),
        "dxf" => open_dxf_doc(path),
        "plt" | "hpgl" => open_plt_doc(path),
        "psd" | "psb" => open_psd_doc(path),
        "svg" | "svgz" => open_svg_doc(path),
        "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "bmp" => {
            let storage_fmt = match ext.as_str() {
                "jpg" | "jpeg" => "jpg",
                "tif" | "tiff" => "tiff",
                _ => "png",
            };
            open_raster_doc(path, &stem, storage_fmt)
        }
        _ => {
            if let Ok(cdr) = open_binary_cdr_doc(path) {
                return Ok(cdr);
            }
            if let Ok(doc) = open_coree_doc(path) {
                return Ok(doc);
            }
            Err(format!(
                "unsupported file extension '.{ext}' for '{}'",
                path.display()
            ))
        }
    }
}

fn open_coree_doc(path: &Path) -> Result<OpenedDocument, String> {
    let doc = load_coree(path).map_err(|e| format!("failed to open .coree: {e}"))?;
    Ok(OpenedDocument {
        document: doc,
        format_tag: "coree".to_owned(),
        cdr_version: None,
        warnings: Vec::new(),
        inspection: None,
    })
}

fn open_binary_cdr_doc(path: &Path) -> Result<OpenedDocument, String> {
    let imported =
        import_and_inspect_binary_file(path).map_err(|e| format!("import failed: {e}"))?;
    let format_tag = imported.report.detected_format.storage_tag().to_owned();
    let cdr_version = Some(imported.report.format_era.clone());
    let warnings = imported.report.warnings.clone();
    Ok(OpenedDocument {
        document: imported.document,
        format_tag,
        cdr_version,
        warnings,
        inspection: Some(imported.report),
    })
}

fn open_svg_doc(path: &Path) -> Result<OpenedDocument, String> {
    let doc = import_svg(path).map_err(|e| format!("failed to import SVG: {e}"))?;
    Ok(OpenedDocument {
        document: doc,
        format_tag: "svg".to_owned(),
        cdr_version: None,
        warnings: Vec::new(),
        inspection: None,
    })
}

fn open_pdf_doc(path: &Path) -> Result<OpenedDocument, String> {
    let doc = import_pdf(path).map_err(|e| format!("failed to import PDF/AI: {e}"))?;
    // Uses `"pdf"` storage tag so `coree-storage` Recent Documents CHECK constraint always accepts `.pdf` and `.ai`.
    Ok(OpenedDocument {
        document: doc,
        format_tag: "pdf".to_owned(),
        cdr_version: None,
        warnings: Vec::new(),
        inspection: None,
    })
}

fn open_eps_doc(path: &Path) -> Result<OpenedDocument, String> {
    let doc = import_eps(path).map_err(|e| format!("failed to import EPS/PS: {e}"))?;
    Ok(OpenedDocument {
        document: doc,
        format_tag: "eps".to_owned(),
        cdr_version: None,
        warnings: Vec::new(),
        inspection: None,
    })
}

fn open_dxf_doc(path: &Path) -> Result<OpenedDocument, String> {
    let doc = import_dxf(path).map_err(|e| format!("failed to import DXF: {e}"))?;
    Ok(OpenedDocument {
        document: doc,
        format_tag: "dxf".to_owned(),
        cdr_version: None,
        warnings: Vec::new(),
        inspection: None,
    })
}

fn open_plt_doc(path: &Path) -> Result<OpenedDocument, String> {
    let doc = import_plt(path).map_err(|e| format!("failed to import PLT/HPGL: {e}"))?;
    Ok(OpenedDocument {
        document: doc,
        format_tag: "plt".to_owned(),
        cdr_version: None,
        warnings: Vec::new(),
        inspection: None,
    })
}

fn open_psd_doc(path: &Path) -> Result<OpenedDocument, String> {
    let doc = import_psd(path).map_err(|e| format!("failed to import PSD/PSB: {e}"))?;
    Ok(OpenedDocument {
        document: doc,
        format_tag: "psd".to_owned(),
        cdr_version: None,
        warnings: Vec::new(),
        inspection: None,
    })
}

fn open_raster_doc(path: &Path, stem: &str, storage_fmt: &str) -> Result<OpenedDocument, String> {
    let asset = import_raster(path).map_err(|e| format!("failed to import raster: {e}"))?;
    let dpi = f64::from(asset.source_dpi.max(1.0));
    let w_mm = (f64::from(asset.width) / dpi * MM_PER_INCH).max(10.0);
    let h_mm = (f64::from(asset.height) / dpi * MM_PER_INCH).max(10.0);
    let mut doc = Document::new(stem, w_mm, h_mm, ColorMode::Rgb, asset.source_dpi);
    let asset_id = asset.id;
    doc.assets.insert(asset_id, asset);
    let obj = SceneObject::new(
        ObjectId(0),
        stem.to_owned(),
        NodePayload::Raster(RasterLayerData {
            asset_id,
            width_mm: w_mm,
            height_mm: h_mm,
            filters: Vec::new(),
            crop_rect: None,
        }),
    )
    .with_fill(Fill::None)
    .with_outline(OutlineStyle::none());
    let _ = doc.add_object(obj, None);
    Ok(OpenedDocument {
        document: doc,
        format_tag: storage_fmt.to_owned(),
        cdr_version: None,
        warnings: Vec::new(),
        inspection: None,
    })
}

/// Imports an external vector or raster file directly onto the active page of `target`.
/// Returns the newly created top-level object IDs.
pub fn import_into_document(target: &mut Document, path: &Path) -> Result<Vec<ObjectId>, String> {
    let opened = open_any_document(path)?;
    Ok(import_document_into(target, &opened.document))
}

/// Imports the first page, raster assets, and symbol definitions of `src` into `target`'s
/// active page with recursion/cycle protection and cross-object [`ObjectId`] remapping.
pub fn import_document_into(target: &mut Document, src: &Document) -> Vec<ObjectId> {
    // Copy raster assets and reusable symbol definitions into target
    for (aid, asset) in &src.assets {
        target.assets.insert(*aid, asset.clone());
    }
    for (sid, sym) in &src.symbols {
        target.symbols.insert(*sid, sym.clone());
    }

    let mut created_ids = Vec::new();
    let Some(first_page) = src.pages.first() else {
        return created_ids;
    };
    let page_offset = target
        .active_page()
        .map(|p| Affine2D::translate(p.width_mm * 0.05, p.height_mm * 0.05))
        .unwrap_or(Affine2D::IDENTITY);

    let mut id_map: HashMap<ObjectId, ObjectId> = HashMap::new();
    let mut visiting: HashSet<ObjectId> = HashSet::new();

    for layer in &first_page.layers {
        for &src_oid in &layer.objects {
            if let Some(id) = clone_object_tree_into(
                src,
                src_oid,
                target,
                true,
                page_offset,
                0,
                &mut visiting,
                &mut id_map,
            ) {
                created_ids.push(id);
            }
        }
    }

    // Remap cross-object references on every newly cloned object
    remap_object_references(target, &id_map);
    created_ids
}

#[allow(clippy::too_many_arguments)]
fn clone_object_tree_into(
    src: &Document,
    src_oid: ObjectId,
    dst: &mut Document,
    attach_to_layer: bool,
    extra_xf: Affine2D,
    depth: usize,
    visiting: &mut HashSet<ObjectId>,
    id_map: &mut HashMap<ObjectId, ObjectId>,
) -> Option<ObjectId> {
    if depth > MAX_IMPORT_DEPTH || !visiting.insert(src_oid) {
        return None;
    }

    let mut obj = match src.objects.get(&src_oid) {
        Some(o) => o.clone(),
        None => {
            visiting.remove(&src_oid);
            return None;
        }
    };
    let new_id = dst.alloc_object_id();
    obj.id = new_id;
    id_map.insert(src_oid, new_id);

    if attach_to_layer {
        obj.transform = extra_xf.compose(&obj.transform);
    }

    match &mut obj.payload {
        NodePayload::Group(children) => {
            let old_children = children.clone();
            let mut new_children = Vec::with_capacity(old_children.len());
            for cid in old_children {
                if let Some(nid) = clone_object_tree_into(
                    src,
                    cid,
                    dst,
                    false,
                    Affine2D::IDENTITY,
                    depth + 1,
                    visiting,
                    id_map,
                ) {
                    new_children.push(nid);
                }
            }
            *children = new_children;
        }
        NodePayload::PowerClip { contents, .. } => {
            let old_contents = contents.clone();
            let mut new_contents = Vec::with_capacity(old_contents.len());
            for cid in old_contents {
                if let Some(nid) = clone_object_tree_into(
                    src,
                    cid,
                    dst,
                    false,
                    Affine2D::IDENTITY,
                    depth + 1,
                    visiting,
                    id_map,
                ) {
                    new_contents.push(nid);
                }
            }
            *contents = new_contents;
        }
        _ => {}
    }

    visiting.remove(&src_oid);

    if attach_to_layer {
        dst.add_object(obj, None).ok()
    } else {
        dst.objects.insert(new_id, obj);
        Some(new_id)
    }
}

fn remap_object_references(target: &mut Document, id_map: &HashMap<ObjectId, ObjectId>) {
    if id_map.is_empty() {
        return;
    }
    let cloned_ids: Vec<ObjectId> = id_map.values().copied().collect();
    for nid in cloned_ids {
        let Some(obj) = target.objects.get_mut(&nid) else {
            continue;
        };
        match &mut obj.payload {
            NodePayload::TextOnPath(top) => {
                if let Some(&mapped) = id_map.get(&top.path_id) {
                    top.path_id = mapped;
                }
            }
            NodePayload::Connector(conn) => {
                if let Some(fid) = conn.from_obj {
                    if let Some(&mapped) = id_map.get(&fid) {
                        conn.from_obj = Some(mapped);
                    }
                }
                if let Some(tid) = conn.to_obj {
                    if let Some(&mapped) = id_map.get(&tid) {
                        conn.to_obj = Some(mapped);
                    }
                }
            }
            NodePayload::ParagraphText(para) => {
                if let Some(next_id) = para.linked_next_frame {
                    if let Some(&mapped) = id_map.get(&next_id) {
                        para.linked_next_frame = Some(mapped);
                    }
                }
            }
            _ => {}
        }
        for eff in &mut obj.effects {
            if let Effect::Blend {
                target_id,
                along_path,
                ..
            } = eff
            {
                if let Some(&mapped) = id_map.get(target_id) {
                    *target_id = mapped;
                }
                if let Some(ap) = *along_path {
                    if let Some(&mapped) = id_map.get(&ap) {
                        *along_path = Some(mapped);
                    }
                }
            }
        }
    }
}

/// Saves or exports `doc` to `output_path` inferring the target format from extension.
pub fn export_any_document(
    doc: &Document,
    output_path: &Path,
    opts: &UniversalExportOptions,
) -> Result<String, String> {
    let ext = output_path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "coree".to_owned());

    let page_idx = opts.page_index.min(doc.pages.len().saturating_sub(1));

    match ext.as_str() {
        "coree" => {
            // Generate a lightweight preview PNG thumbnail (48 DPI) when saving native `.coree`
            let preview_png = render_page_to_rgba(doc, page_idx, 48.0, ViewMode::Enhanced)
                .ok()
                .and_then(|img| img.encode_png().ok());
            save_coree_with_preview(doc, preview_png.as_deref(), output_path)
                .map_err(|e| format!("failed to save .coree: {e}"))?;
            Ok(format!("Saved native .coree to {}", output_path.display()))
        }
        "cdr" => {
            write_cdr_file(doc, output_path, opts.cdr_version)
                .map_err(|e| format!("failed to export .cdr: {e}"))?;
            Ok(format!(
                "Exported .cdr ({:?}) to {}",
                opts.cdr_version,
                output_path.display()
            ))
        }
        "cdt" => {
            write_cdt_file(doc, output_path, opts.cdr_version)
                .map_err(|e| format!("failed to export .cdt: {e}"))?;
            Ok(format!("Exported .cdt to {}", output_path.display()))
        }
        "cmx" => {
            write_cmx_file(doc, output_path)
                .map_err(|e| format!("failed to export .cmx: {e}"))?;
            Ok(format!("Exported .cmx to {}", output_path.display()))
        }
        "svg" => {
            export_svg(doc, page_idx, output_path)
                .map_err(|e| format!("failed to export .svg: {e}"))?;
            Ok(format!("Exported .svg to {}", output_path.display()))
        }
        "pdf" => {
            let pdf_opts = PdfExportOptions {
                color_space: if opts.force_cmyk {
                    PdfColorSpace::DeviceCmyk
                } else {
                    PdfColorSpace::DocumentDefault
                },
                include_bleed: opts.include_bleed,
                crop_marks: opts.crop_marks,
                registration_marks: opts.registration_marks,
                color_calibration_bars: opts.color_calibration_bars,
                page_info_slug: opts.page_info_slug,
                compress_streams: true,
                pages: None,
            };
            export_pdf(doc, &pdf_opts, output_path)
                .map_err(|e| format!("failed to export .pdf: {e}"))?;
            Ok(format!("Exported .pdf to {}", output_path.display()))
        }
        "eps" => {
            export_eps(doc, page_idx, output_path)
                .map_err(|e| format!("failed to export .eps: {e}"))?;
            Ok(format!("Exported .eps to {}", output_path.display()))
        }
        "dxf" => {
            let dxf_opts = DxfExportOptions {
                cut_layers_only: opts.cut_layers_only,
                ..DxfExportOptions::default()
            };
            export_dxf_with_options(doc, page_idx, &dxf_opts, output_path)
                .map_err(|e| format!("failed to export .dxf: {e}"))?;
            Ok(format!("Exported .dxf to {}", output_path.display()))
        }
        "plt" | "hpgl" => {
            let plt_opts = PltExportOptions {
                cut_layers_only: opts.cut_layers_only,
                ..PltExportOptions::default()
            };
            export_plt_with_options(doc, page_idx, &plt_opts, output_path)
                .map_err(|e| format!("failed to export .{ext}: {e}"))?;
            Ok(format!("Exported .{ext} to {}", output_path.display()))
        }
        "gcode" | "nc" => {
            let mut laser_cfg = LaserJobConfig {
                dialect: opts.gcode_dialect,
                ..LaserJobConfig::default()
            };
            if opts.cut_layers_only {
                for rule in &mut laser_cfg.rules {
                    if !rule.match_cut_layer {
                        rule.enabled = false;
                    }
                }
            }
            let plan = plan_laser_toolpath(doc, page_idx, None, &laser_cfg)
                .map_err(|e| format!("laser toolpath planning failed: {e}"))?;
            let gcode = export_laser_gcode(&plan, &laser_cfg);
            atomic_write_bytes(output_path, gcode.as_bytes())
                .map_err(|e| format!("failed to export .{ext}: {e}"))?;
            Ok(format!(
                "Exported .{ext} ({:?}, {} cut(s), {:.1} mm cut, {:.1} mm engrave) to {}",
                opts.gcode_dialect,
                plan.cut_count,
                plan.total_cut_mm,
                plan.total_engrave_mm,
                output_path.display()
            ))
        }
        "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "bmp" => {
            let fmt = RasterFormat::from_extension(&ext)
                .ok_or_else(|| format!("unsupported raster format extension '.{ext}'"))?;
            let safe_dpi = if opts.dpi.is_finite() && opts.dpi > 0.0 {
                opts.dpi.clamp(1.0, 2400.0)
            } else {
                300.0
            };
            let rendered =
                render_page_to_rgba(doc, page_idx, f64::from(safe_dpi), ViewMode::Enhanced)
                    .map_err(|e| format!("raster render failed: {e}"))?;
            let r_opts = RasterExportOptions {
                format: fmt,
                dpi: safe_dpi,
                jpeg_quality: opts.jpeg_quality.clamp(1, 100),
            };
            export_raster(
                rendered.width,
                rendered.height,
                &rendered.rgba,
                &r_opts,
                output_path,
            )
            .map_err(|e| format!("raster export failed: {e}"))?;
            Ok(format!(
                "Exported {} ({}x{} @ {} DPI) to {}",
                fmt.extension().to_uppercase(),
                rendered.width,
                rendered.height,
                safe_dpi,
                output_path.display()
            ))
        }
        other => Err(format!(
            "unsupported export format '.{other}' (supported: .coree, .cdr, .cdt, .cmx, .svg, .pdf, .eps, .dxf, .plt, .gcode, .nc, .png, .jpg, .webp, .tiff, .bmp)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdr_bridge::{
        extract_file_thumbnail_png, inspect_any_file, read_png_dimensions, ContainerFormat,
    };
    use crate::cli::{execute_cli, Cli, CliOutcome};
    use clap::Parser;
    use coree_cdr::generate_sample_document;
    use std::path::PathBuf;

    struct ScopedTempDir(PathBuf);

    impl ScopedTempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "coree_bridge_test_{tag}_{}_{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScopedTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Builds a minimal valid 4x4 8-bit RGB Photoshop `.psd` binary (clean-room test fixture).
    fn build_minimal_psd_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"8BPS"); // Signature
        out.extend_from_slice(&1u16.to_be_bytes()); // Version 1 (PSD)
        out.extend_from_slice(&[0u8; 6]); // Reserved
        out.extend_from_slice(&3u16.to_be_bytes()); // 3 channels (RGB)
        out.extend_from_slice(&4u32.to_be_bytes()); // Height = 4
        out.extend_from_slice(&4u32.to_be_bytes()); // Width = 4
        out.extend_from_slice(&8u16.to_be_bytes()); // Depth = 8 bpc
        out.extend_from_slice(&3u16.to_be_bytes()); // Color mode = RGB
        out.extend_from_slice(&0u32.to_be_bytes()); // Color Mode Data len = 0
        out.extend_from_slice(&0u32.to_be_bytes()); // Image Resources len = 0
        out.extend_from_slice(&0u32.to_be_bytes()); // Layer & Mask Info len = 0
        out.extend_from_slice(&0u16.to_be_bytes()); // Compression = Raw
                                                    // 3 planar channels * 16 pixels
        out.extend(std::iter::repeat_n(0x20u8, 16)); // R
        out.extend(std::iter::repeat_n(0xB9u8, 16)); // G
        out.extend(std::iter::repeat_n(0x81u8, 16)); // B
        out
    }

    #[test]
    fn universal_open_and_inspect_handles_all_wave_a_formats_and_magic_bytes() {
        let tmp = ScopedTempDir::new("wave-a-open");
        let doc = generate_sample_document();
        let opts = UniversalExportOptions::default();

        // Export base formats
        let coree_path = tmp.path().join("sample.coree");
        let cdr_path = tmp.path().join("sample.cdr");
        let cdt_path = tmp.path().join("sample.cdt");
        let svg_path = tmp.path().join("sample.svg");
        let pdf_path = tmp.path().join("sample.pdf");
        let eps_path = tmp.path().join("sample.eps");
        let dxf_path = tmp.path().join("sample.dxf");
        let plt_path = tmp.path().join("sample.plt");
        let psd_path = tmp.path().join("sample.psd");

        for p in [
            &coree_path,
            &cdr_path,
            &cdt_path,
            &svg_path,
            &pdf_path,
            &eps_path,
            &dxf_path,
            &plt_path,
        ] {
            export_any_document(&doc, p, &opts)
                .unwrap_or_else(|e| panic!("export {}: {e}", p.display()));
        }
        std::fs::write(&psd_path, build_minimal_psd_bytes()).unwrap();

        // Real-world extension variants & extensionless files (magic-byte-first)
        let cdr_zip = tmp.path().join("shield.cdr.zip");
        let cdt_zip = tmp.path().join("template.cdt.zip");
        let misnamed_coree = tmp.path().join("actually_coree.svg");
        let ai_pdf = tmp.path().join("artwork.ai");
        let ai_eps = tmp.path().join("legacy_artwork.ai");
        let ps_path = tmp.path().join("drawing.ps");
        let hpgl_path = tmp.path().join("cutter.hpgl");
        let no_ext_cdr = tmp.path().join("no_ext_cdr");
        let no_ext_pdf = tmp.path().join("no_ext_pdf");
        let no_ext_psd = tmp.path().join("no_ext_psd");
        let no_ext_dxf = tmp.path().join("no_ext_dxf");
        let no_ext_plt = tmp.path().join("no_ext_plt");

        std::fs::copy(&cdr_path, &cdr_zip).unwrap();
        std::fs::copy(&cdt_path, &cdt_zip).unwrap();
        std::fs::copy(&coree_path, &misnamed_coree).unwrap();
        std::fs::copy(&pdf_path, &ai_pdf).unwrap();
        std::fs::copy(&eps_path, &ai_eps).unwrap();
        std::fs::copy(&eps_path, &ps_path).unwrap();
        std::fs::copy(&plt_path, &hpgl_path).unwrap();
        std::fs::copy(&cdr_path, &no_ext_cdr).unwrap();
        std::fs::copy(&pdf_path, &no_ext_pdf).unwrap();
        std::fs::copy(&psd_path, &no_ext_psd).unwrap();
        std::fs::copy(&dxf_path, &no_ext_dxf).unwrap();
        std::fs::copy(&plt_path, &no_ext_plt).unwrap();

        let cases: &[(&Path, &str, ContainerFormat)] = &[
            (&coree_path, "coree", ContainerFormat::NativeCoree),
            (&misnamed_coree, "coree", ContainerFormat::NativeCoree),
            (&cdr_path, "cdr", ContainerFormat::CdrZipX6),
            (&cdr_zip, "cdr", ContainerFormat::CdrZipX6),
            (&no_ext_cdr, "cdr", ContainerFormat::CdrZipX6),
            (&cdt_zip, "cdt", ContainerFormat::CdtZip),
            (&svg_path, "svg", ContainerFormat::Svg),
            (&pdf_path, "pdf", ContainerFormat::Pdf),
            (&ai_pdf, "pdf", ContainerFormat::Ai),
            (&no_ext_pdf, "pdf", ContainerFormat::Pdf),
            (&eps_path, "eps", ContainerFormat::Eps),
            (&ps_path, "eps", ContainerFormat::Eps),
            (&ai_eps, "eps", ContainerFormat::Eps),
            (&dxf_path, "dxf", ContainerFormat::Dxf),
            (&no_ext_dxf, "dxf", ContainerFormat::Dxf),
            (&plt_path, "plt", ContainerFormat::Plt),
            (&hpgl_path, "plt", ContainerFormat::Plt),
            (&no_ext_plt, "plt", ContainerFormat::Plt),
            (&psd_path, "psd", ContainerFormat::Psd),
            (&no_ext_psd, "psd", ContainerFormat::Psd),
        ];

        for (path, expected_tag, expected_fmt) in cases {
            let opened = open_any_document(path)
                .unwrap_or_else(|e| panic!("open_any_document({}) failed: {e}", path.display()));
            assert_eq!(
                opened.format_tag,
                *expected_tag,
                "wrong format_tag for {}",
                path.display()
            );
            assert!(
                !opened.document.pages.is_empty(),
                "document must have >= 1 page for {}",
                path.display()
            );

            let report = inspect_any_file(path)
                .unwrap_or_else(|e| panic!("inspect_any_file({}) failed: {e}", path.display()));
            assert_eq!(
                report.detected_format,
                *expected_fmt,
                "wrong detected_format for {}",
                path.display()
            );
            assert!(report.page_count >= 1);
        }
    }

    #[test]
    fn export_gcode_and_nc_and_cli_thumbnail_and_laser_work_end_to_end() {
        let tmp = ScopedTempDir::new("gcode-thumb-laser");
        let doc = generate_sample_document();
        let coree_path = tmp.path().join("shield.coree");
        let cdr_path = tmp.path().join("shield.cdr");
        let cdr_zip = tmp.path().join("shield.cdr.zip");
        export_any_document(&doc, &coree_path, &UniversalExportOptions::default()).unwrap();
        export_any_document(&doc, &cdr_path, &UniversalExportOptions::default()).unwrap();
        std::fs::copy(&cdr_path, &cdr_zip).unwrap();

        // 1. export_any_document to .gcode (GRBL) and .nc (Marlin)
        let gcode_path = tmp.path().join("direct.gcode");
        let nc_path = tmp.path().join("direct.nc");
        export_any_document(&doc, &gcode_path, &UniversalExportOptions::default()).unwrap();
        export_any_document(
            &doc,
            &nc_path,
            &UniversalExportOptions {
                gcode_dialect: GcodeDialect::Marlin,
                ..UniversalExportOptions::default()
            },
        )
        .unwrap();
        let gcode_text = std::fs::read_to_string(&gcode_path).unwrap();
        let nc_text = std::fs::read_to_string(&nc_path).unwrap();
        assert!(gcode_text.contains("G21") && gcode_text.contains("G90"));
        assert!(nc_text.contains("G21") && nc_text.contains("G90"));

        // 2. extract_file_thumbnail_png + CLI `coree thumbnail` bounded to <= size
        for (src, size) in [(&coree_path, 96u32), (&cdr_zip, 128u32)] {
            let png = extract_file_thumbnail_png(src, size).unwrap();
            let (w, h) = read_png_dimensions(&png).expect("valid PNG IHDR");
            assert!(w >= 1 && w <= size && h >= 1 && h <= size);
        }
        let thumb_out = tmp.path().join("nested").join("thumb.png");
        let thumb_cli = Cli::try_parse_from([
            "coree",
            "thumbnail",
            &coree_path.to_string_lossy(),
            &thumb_out.to_string_lossy(),
            "--size",
            "64",
        ])
        .unwrap();
        let CliOutcome::Printed(thumb_msg) = execute_cli(&thumb_cli).unwrap() else {
            panic!("expected printed outcome");
        };
        assert!(thumb_msg.contains("Generated thumbnail"));
        let thumb_bytes = std::fs::read(&thumb_out).unwrap();
        let (tw, th) = read_png_dimensions(&thumb_bytes).unwrap();
        assert!(tw <= 64 && th <= 64);

        // 3. CLI `coree laser` across formats, dialects, kerf, hatch, weeding-box, and plugin bridge
        let laser_gcode = tmp.path().join("job_ruida.gcode");
        let laser_cli = Cli::try_parse_from([
            "coree",
            "laser",
            &cdr_zip.to_string_lossy(),
            &laser_gcode.to_string_lossy(),
            "--format",
            "gcode",
            "--dialect",
            "ruida",
            "--dedupe",
            "0.05",
            "--join",
            "0.05",
            "--kerf",
            "0.1",
            "--hatch-spacing",
            "0.5",
            "--hatch-angle",
            "30",
            "--cross-hatch",
            "--weeding-box",
        ])
        .unwrap();
        let CliOutcome::Printed(laser_msg) = execute_cli(&laser_cli).unwrap() else {
            panic!("expected printed laser summary");
        };
        assert!(laser_msg.contains("Cuts:"));
        assert!(laser_msg.contains("Duplicates Removed:"));
        assert!(laser_msg.contains("Curves Joined:"));
        assert!(laser_msg.contains("Est. Time:"));
        assert!(std::fs::read_to_string(&laser_gcode)
            .unwrap()
            .contains("G21"));

        let laser_dxf = tmp.path().join("job.dxf");
        let dxf_cli = Cli::try_parse_from([
            "coree",
            "laser",
            &coree_path.to_string_lossy(),
            &laser_dxf.to_string_lossy(),
            "--format",
            "dxf",
        ])
        .unwrap();
        execute_cli(&dxf_cli).unwrap();
        assert!(std::fs::read_to_string(&laser_dxf)
            .unwrap()
            .contains("SECTION"));

        let plugin_svg = tmp.path().join("lightburn_out.svg");
        let plugin_cli = Cli::try_parse_from([
            "coree",
            "laser",
            &coree_path.to_string_lossy(),
            &plugin_svg.to_string_lossy(),
            "--plugin",
            "lightburn-bridge",
        ])
        .unwrap();
        let CliOutcome::Printed(plugin_msg) = execute_cli(&plugin_cli).unwrap() else {
            panic!("expected printed plugin summary");
        };
        assert!(plugin_msg.contains("LightBurn"));
        assert!(std::fs::read_to_string(&plugin_svg)
            .unwrap()
            .contains("<svg"));
    }
}
