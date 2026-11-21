// SPDX-License-Identifier: MPL-2.0

//! Universal offline file open, import, save, and export pipeline bridging
//! `coree-io`, `coree-cdr`, and `coree-render`.

use crate::cdr_bridge::{
    import_and_inspect_binary_file, write_cdr_file, write_cdt_file, write_cmx_file,
    FileInspectionReport,
};
use coree_cdr::CdrWriteVersion;
use coree_core::math::MM_PER_INCH;
use coree_core::{
    Affine2D, ColorMode, Document, Effect, Fill, NodePayload, ObjectId, OutlineStyle,
    RasterLayerData, SceneObject,
};
use coree_io::{
    export_dxf_with_options, export_eps, export_pdf, export_plt_with_options, export_raster,
    export_svg, import_raster, import_svg, load_coree, save_coree_with_preview, DxfExportOptions,
    PdfColorSpace, PdfExportOptions, PltExportOptions, RasterExportOptions, RasterFormat,
};
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

/// Opens any supported vector, native, or raster file into a [`Document`].
pub fn open_any_document(path: &Path) -> Result<OpenedDocument, String> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_owned();

    match ext.as_str() {
        "coree" => {
            let doc = load_coree(path).map_err(|e| format!("failed to open .coree: {e}"))?;
            Ok(OpenedDocument {
                document: doc,
                format_tag: "coree".to_owned(),
                cdr_version: None,
                warnings: Vec::new(),
                inspection: None,
            })
        }
        "cdr" | "cdt" | "cmx" | "cpt" => {
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
        "svg" | "svgz" => {
            let doc = import_svg(path).map_err(|e| format!("failed to import SVG: {e}"))?;
            Ok(OpenedDocument {
                document: doc,
                format_tag: "svg".to_owned(),
                cdr_version: None,
                warnings: Vec::new(),
                inspection: None,
            })
        }
        "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "bmp" => {
            let asset = import_raster(path).map_err(|e| format!("failed to import raster: {e}"))?;
            let dpi = f64::from(asset.source_dpi.max(1.0));
            let w_mm = (f64::from(asset.width) / dpi * MM_PER_INCH).max(10.0);
            let h_mm = (f64::from(asset.height) / dpi * MM_PER_INCH).max(10.0);
            let mut doc = Document::new(&stem, w_mm, h_mm, ColorMode::Rgb, asset.source_dpi);
            let asset_id = asset.id;
            doc.assets.insert(asset_id, asset);
            let obj = SceneObject::new(
                ObjectId(0),
                stem,
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
            let storage_fmt = match ext.as_str() {
                "jpg" | "jpeg" => "jpg",
                "tif" | "tiff" => "tiff",
                _ => "png",
            };
            Ok(OpenedDocument {
                document: doc,
                format_tag: storage_fmt.to_owned(),
                cdr_version: None,
                warnings: Vec::new(),
                inspection: None,
            })
        }
        _ => {
            // Attempt binary signature detection for extensionless files
            if let Ok(imported) = import_and_inspect_binary_file(path) {
                let format_tag = imported.report.detected_format.storage_tag().to_owned();
                let cdr_version = Some(imported.report.format_era.clone());
                let warnings = imported.report.warnings.clone();
                return Ok(OpenedDocument {
                    document: imported.document,
                    format_tag,
                    cdr_version,
                    warnings,
                    inspection: Some(imported.report),
                });
            }
            if let Ok(doc) = load_coree(path) {
                return Ok(OpenedDocument {
                    document: doc,
                    format_tag: "coree".to_owned(),
                    cdr_version: None,
                    warnings: Vec::new(),
                    inspection: None,
                });
            }
            Err(format!(
                "unsupported file extension '.{ext}' for '{}'",
                path.display()
            ))
        }
    }
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
            // Generate a lightweight preview PNG thumbnail (72 DPI) when saving native `.coree`
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
        "plt" => {
            let plt_opts = PltExportOptions {
                cut_layers_only: opts.cut_layers_only,
                ..PltExportOptions::default()
            };
            export_plt_with_options(doc, page_idx, &plt_opts, output_path)
                .map_err(|e| format!("failed to export .plt: {e}"))?;
            Ok(format!("Exported .plt to {}", output_path.display()))
        }
        "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "bmp" => {
            let fmt = RasterFormat::from_extension(&ext).ok_or_else(|| {
                format!("unsupported raster format extension '.{ext}'")
            })?;
            let safe_dpi = if opts.dpi.is_finite() && opts.dpi > 0.0 {
                opts.dpi.clamp(1.0, 2400.0)
            } else {
                300.0
            };
            let rendered = render_page_to_rgba(doc, page_idx, f64::from(safe_dpi), ViewMode::Enhanced)
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
            "unsupported export format '.{other}' (supported: .coree, .cdr, .cdt, .cmx, .svg, .pdf, .eps, .dxf, .plt, .png, .jpg, .webp, .tiff, .bmp)"
        )),
    }
}
