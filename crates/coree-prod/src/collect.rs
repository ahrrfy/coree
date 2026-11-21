// SPDX-License-Identifier: MPL-2.0

//! `Collect for Output`: packages a complete print-production handoff directory containing
//! the serialized document JSON, extracted raster PNG assets (`images/`), font inventory
//! and copied system font files (`fonts/`), preflight report (`preflight.txt` + `preflight.json`),
//! and a structured production summary (`manifest.json`).

use crate::cut_packaging::{compute_production_metrics, ProductionMetrics};
use crate::error::{ProdError, ProdResult};
use crate::preflight::{run_preflight, PreflightReport, PreflightRules};
use coree_core::{Color, ColorMode, Document, ImageAssetId, NodePayload};
use image::{ImageFormat, RgbaImage};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration options for [`collect_for_output`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectOptions {
    /// Write the serialized `.coree.json` document into the handoff bundle.
    pub include_document_json: bool,
    /// Extract all embedded `RasterAsset`s as PNG files under `images/`.
    pub extract_images: bool,
    /// Write used-font manifests (`fonts/fonts.txt` & `fonts/fonts_manifest.json`).
    pub collect_fonts: bool,
    /// Copy matching system/local font files (`.ttf`/`.otf`/`.ttc`) into `fonts/` when permitted.
    pub copy_system_fonts: bool,
    /// Additional font search directories checked before system defaults (useful for custom vaults & tests).
    pub extra_font_search_dirs: Vec<PathBuf>,
    /// Run preflight and write `preflight.txt` and `preflight.json`.
    pub include_preflight: bool,
    /// Preflight rules applied when `include_preflight` is `true`.
    pub preflight_rules: PreflightRules,
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self {
            include_document_json: true,
            extract_images: true,
            collect_fonts: true,
            copy_system_fonts: true,
            extra_font_search_dirs: Vec::new(),
            include_preflight: true,
            preflight_rules: PreflightRules::default(),
        }
    }
}

/// Metadata for a raster asset extracted into `images/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectedImageInfo {
    pub asset_id: ImageAssetId,
    pub relative_path: String,
    pub width: u32,
    pub height: u32,
    pub source_dpi: f32,
    pub color_mode: ColorMode,
}

/// Metadata for a font family referenced by the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectedFontInfo {
    pub family: String,
    /// Relative path inside the bundle if a matching font file was found and copied.
    pub copied_relative_path: Option<String>,
}

/// Page dimension summary stored in `manifest.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageSummary {
    pub name: String,
    pub width_mm: f64,
    pub height_mm: f64,
    pub bleed_mm: f64,
    pub layer_count: usize,
}

/// Structured production handoff manifest serialized to `manifest.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProductionManifest {
    pub format: String,
    pub document_title: String,
    pub color_mode: ColorMode,
    pub dpi: f32,
    pub pages: Vec<PageSummary>,
    pub total_objects: usize,
    pub spot_inks: Vec<String>,
    pub used_fonts: Vec<CollectedFontInfo>,
    pub extracted_images: Vec<CollectedImageInfo>,
    pub production_metrics: ProductionMetrics,
    pub preflight_passed: Option<bool>,
    pub preflight_errors: usize,
    pub preflight_warnings: usize,
    pub files: Vec<String>,
}

/// Result returned by [`collect_for_output`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectReport {
    pub output_dir: PathBuf,
    pub document_json_path: Option<PathBuf>,
    pub manifest_path: PathBuf,
    pub preflight_txt_path: Option<PathBuf>,
    pub preflight_json_path: Option<PathBuf>,
    pub extracted_images: Vec<CollectedImageInfo>,
    pub used_fonts: Vec<CollectedFontInfo>,
    pub spot_inks: Vec<String>,
    pub preflight_report: Option<PreflightReport>,
    pub production_metrics: ProductionMetrics,
    pub total_files_written: usize,
}

/// Packages a complete print-production handoff directory at `output_dir`.
pub fn collect_for_output(
    doc: &Document,
    output_dir: &Path,
    options: &CollectOptions,
) -> ProdResult<CollectReport> {
    fs::create_dir_all(output_dir)?;

    let mut written_files: Vec<String> = Vec::new();

    // 1. Document JSON
    let document_json_path = if options.include_document_json {
        let safe_name = sanitize_filename(&doc.title, "document");
        let rel = format!("{safe_name}.coree.json");
        let full = output_dir.join(&rel);
        let json = serde_json::to_string_pretty(doc)?;
        fs::write(&full, json)?;
        written_files.push(rel);
        Some(full)
    } else {
        None
    };

    // 2. Extract embedded raster assets into `images/`
    let mut extracted_images = Vec::new();
    if options.extract_images && !doc.assets.is_empty() {
        let images_dir = output_dir.join("images");
        fs::create_dir_all(&images_dir)?;
        for (&asset_id, asset) in &doc.assets {
            let file_name = format!("{}.png", asset_id.0);
            let rel = format!("images/{file_name}");
            let full = images_dir.join(&file_name);

            let img = RgbaImage::from_raw(asset.width, asset.height, asset.rgba_bytes.clone())
                .ok_or_else(|| {
                    ProdError::ImageError(format!(
                        "invalid RGBA buffer length for asset {asset_id} ({}x{})",
                        asset.width, asset.height
                    ))
                })?;
            img.save_with_format(&full, ImageFormat::Png)
                .map_err(|e| ProdError::ImageError(e.to_string()))?;

            written_files.push(rel.clone());
            extracted_images.push(CollectedImageInfo {
                asset_id,
                relative_path: rel,
                width: asset.width,
                height: asset.height,
                source_dpi: asset.source_dpi,
                color_mode: asset.color_mode,
            });
        }
    }

    // 3. Collect used fonts & optionally copy matched font files into `fonts/`
    let used_families = collect_used_font_families(doc);
    let mut used_fonts = Vec::with_capacity(used_families.len());

    if options.collect_fonts {
        let fonts_dir = output_dir.join("fonts");
        fs::create_dir_all(&fonts_dir)?;

        let search_dirs = if options.copy_system_fonts {
            build_font_search_dirs(&options.extra_font_search_dirs)
        } else {
            Vec::new()
        };
        let font_index = if options.copy_system_fonts && !used_families.is_empty() {
            index_font_files(&search_dirs)
        } else {
            BTreeMap::new()
        };

        let mut used_font_filenames: HashSet<String> = HashSet::new();
        let mut copied_sources: HashMap<PathBuf, String> = HashMap::new();

        for family in &used_families {
            let mut copied_rel = None;
            if options.copy_system_fonts {
                if let Some(src_path) = match_font_file(family, &font_index) {
                    if let Some(existing_rel) = copied_sources.get(&src_path) {
                        copied_rel = Some(existing_rel.clone());
                    } else if let Some(fname) = src_path.file_name().and_then(|s| s.to_str()) {
                        let unique_fname = allocate_unique_font_filename(
                            &sanitize_filename_keep_ext(fname),
                            &mut used_font_filenames,
                        );
                        let rel = format!("fonts/{unique_fname}");
                        let dst_path = fonts_dir.join(&unique_fname);
                        if fs::copy(&src_path, &dst_path).is_ok() {
                            if !written_files.contains(&rel) {
                                written_files.push(rel.clone());
                            }
                            copied_sources.insert(src_path, rel.clone());
                            copied_rel = Some(rel);
                        }
                    }
                }
            }
            used_fonts.push(CollectedFontInfo {
                family: family.clone(),
                copied_relative_path: copied_rel,
            });
        }

        // Write fonts.txt & fonts_manifest.json
        let mut fonts_txt = String::from("=== USED FONTS ===\n");
        if used_fonts.is_empty() {
            fonts_txt.push_str("(No live text fonts referenced)\n");
        } else {
            for f in &used_fonts {
                match &f.copied_relative_path {
                    Some(rel) => fonts_txt.push_str(&format!("- {} [copied -> {rel}]\n", f.family)),
                    None => fonts_txt.push_str(&format!("- {} [not copied]\n", f.family)),
                }
            }
        }
        fs::write(fonts_dir.join("fonts.txt"), fonts_txt)?;
        written_files.push("fonts/fonts.txt".to_owned());

        let fonts_json = serde_json::to_string_pretty(&used_fonts)?;
        fs::write(fonts_dir.join("fonts_manifest.json"), fonts_json)?;
        written_files.push("fonts/fonts_manifest.json".to_owned());
    } else {
        for family in used_families {
            used_fonts.push(CollectedFontInfo {
                family,
                copied_relative_path: None,
            });
        }
    }

    // 4. Preflight reports (`preflight.txt` + `preflight.json`)
    let (preflight_report, preflight_txt_path, preflight_json_path) = if options.include_preflight {
        let report = run_preflight(doc, &options.preflight_rules);
        let txt_path = output_dir.join("preflight.txt");
        let json_path = output_dir.join("preflight.json");
        fs::write(&txt_path, report.to_plain_text())?;
        fs::write(&json_path, serde_json::to_string_pretty(&report)?)?;
        written_files.push("preflight.txt".to_owned());
        written_files.push("preflight.json".to_owned());
        (Some(report), Some(txt_path), Some(json_path))
    } else {
        (None, None, None)
    };

    // 5. Production metrics + spot ink list + `manifest.json`
    let production_metrics = compute_production_metrics(doc, None);
    let spot_inks = collect_spot_inks(doc);

    written_files.push("manifest.json".to_owned());
    let manifest = ProductionManifest {
        format: "coree-collect-for-output/v1".to_owned(),
        document_title: doc.title.clone(),
        color_mode: doc.color_mode,
        dpi: doc.dpi,
        pages: doc
            .pages
            .iter()
            .map(|p| PageSummary {
                name: p.name.clone(),
                width_mm: p.width_mm,
                height_mm: p.height_mm,
                bleed_mm: p.bleed_mm,
                layer_count: p.layers.len(),
            })
            .collect(),
        total_objects: doc.objects.len(),
        spot_inks: spot_inks.clone(),
        used_fonts: used_fonts.clone(),
        extracted_images: extracted_images.clone(),
        production_metrics: production_metrics.clone(),
        preflight_passed: preflight_report.as_ref().map(PreflightReport::passed),
        preflight_errors: preflight_report
            .as_ref()
            .map_or(0, PreflightReport::error_count),
        preflight_warnings: preflight_report
            .as_ref()
            .map_or(0, PreflightReport::warning_count),
        files: written_files.clone(),
    };

    let manifest_path = output_dir.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;

    Ok(CollectReport {
        output_dir: output_dir.to_path_buf(),
        document_json_path,
        manifest_path,
        preflight_txt_path,
        preflight_json_path,
        extracted_images,
        used_fonts,
        spot_inks,
        preflight_report,
        production_metrics,
        total_files_written: written_files.len(),
    })
}

fn collect_used_font_families(doc: &Document) -> Vec<String> {
    let mut set = BTreeSet::new();
    let all_objs = doc
        .objects
        .values()
        .chain(doc.symbols.values().flat_map(|s| s.objects.iter()));

    for obj in all_objs {
        match &obj.payload {
            NodePayload::ArtisticText(t) => {
                for r in &t.runs {
                    insert_non_empty_font(&r.style.font_family, &mut set);
                }
            }
            NodePayload::ParagraphText(t) => {
                for r in &t.runs {
                    insert_non_empty_font(&r.style.font_family, &mut set);
                }
            }
            NodePayload::TextOnPath(top) => {
                for r in &top.text.runs {
                    insert_non_empty_font(&r.style.font_family, &mut set);
                }
            }
            NodePayload::Table(table) => {
                for cell in &table.cells {
                    if !cell.is_covered() {
                        insert_non_empty_font(&cell.text_style.font_family, &mut set);
                    }
                }
            }
            NodePayload::Dimension(dim) => {
                insert_non_empty_font(&dim.text_style.font_family, &mut set);
            }
            _ => {}
        }
    }
    set.into_iter().collect()
}

fn insert_non_empty_font(family: &str, set: &mut BTreeSet<String>) {
    let trimmed = family.trim();
    if !trimmed.is_empty() {
        set.insert(trimmed.to_owned());
    }
}

fn collect_spot_inks(doc: &Document) -> Vec<String> {
    let mut spots = BTreeSet::new();
    let mut push_color = |c: &Color| {
        if let Color::Spot { name, .. } = c {
            spots.insert(name.clone());
        }
    };

    let all_objs = doc
        .objects
        .values()
        .chain(doc.symbols.values().flat_map(|s| s.objects.iter()));

    for obj in all_objs {
        for c in obj.fill.referenced_colors() {
            push_color(&c);
        }
        if obj.outline.is_visible() {
            push_color(&obj.outline.color);
        }
        match &obj.payload {
            NodePayload::ArtisticText(t) => {
                for r in &t.runs {
                    push_color(&r.style.fill);
                }
            }
            NodePayload::ParagraphText(t) => {
                for r in &t.runs {
                    push_color(&r.style.fill);
                }
            }
            NodePayload::TextOnPath(top) => {
                for r in &top.text.runs {
                    push_color(&r.style.fill);
                }
            }
            NodePayload::Table(table) => {
                if table.border_style.is_visible() {
                    push_color(&table.border_style.color);
                }
                for cell in &table.cells {
                    if !cell.is_covered() {
                        for c in cell.fill.referenced_colors() {
                            push_color(&c);
                        }
                        push_color(&cell.text_style.fill);
                    }
                }
            }
            NodePayload::Dimension(dim) => {
                push_color(&dim.text_style.fill);
            }
            _ => {}
        }
    }
    spots.into_iter().collect()
}

fn allocate_unique_font_filename(base: &str, used: &mut HashSet<String>) -> String {
    if used.insert(base.to_ascii_lowercase()) {
        return base.to_owned();
    }
    let path = Path::new(base);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("font");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("ttf");
    let mut counter = 2usize;
    loop {
        let candidate = format!("{stem}_{counter}.{ext}");
        if used.insert(candidate.to_ascii_lowercase()) {
            return candidate;
        }
        counter += 1;
    }
}

fn build_font_search_dirs(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = extra.to_vec();
    if let Ok(home) = std::env::var("HOME") {
        let h = PathBuf::from(home);
        dirs.push(h.join(".local/share/fonts"));
        dirs.push(h.join(".fonts"));
    }
    dirs.push(PathBuf::from("/usr/share/fonts"));
    dirs.push(PathBuf::from("/usr/local/share/fonts"));
    dirs.push(PathBuf::from("C:\\Windows\\Fonts"));
    dirs.push(PathBuf::from("/Library/Fonts"));
    dirs
}

fn normalize_font_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn index_font_files(dirs: &[PathBuf]) -> BTreeMap<String, PathBuf> {
    let mut index = BTreeMap::new();
    for dir in dirs {
        if dir.is_dir() {
            scan_font_dir(dir, 0, &mut index);
        }
    }
    index
}

fn scan_font_dir(dir: &Path, depth: usize, index: &mut BTreeMap<String, PathBuf>) {
    if depth > 4 || index.len() >= 10_000 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            scan_font_dir(&path, depth + 1, index);
        } else if meta.is_file() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            if matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc" | "woff2")) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let key = normalize_font_key(stem);
                    if !key.is_empty() {
                        index.entry(key).or_insert(path);
                    }
                }
            }
        }
    }
}

fn match_font_file(family: &str, index: &BTreeMap<String, PathBuf>) -> Option<PathBuf> {
    let key = normalize_font_key(family);
    if key.is_empty() {
        return None;
    }
    if let Some(p) = index.get(&key) {
        return Some(p.clone());
    }
    // Common suffix pattern e.g. "NotoSans" -> "notosansregular"
    let reg_key = format!("{key}regular");
    if let Some(p) = index.get(&reg_key) {
        return Some(p.clone());
    }
    // Prefix match fallback
    index
        .iter()
        .find_map(|(k, p)| k.starts_with(&key).then(|| p.clone()))
}

fn sanitize_filename(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.chars().take(64).collect()
    }
}

fn sanitize_filename_keep_ext(raw: &str) -> String {
    let path = Path::new(raw);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("font");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("ttf");
    format!(
        "{}.{}",
        sanitize_filename(stem, "font"),
        sanitize_filename(ext, "ttf")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        Affine2D, ArtisticText, CornerStyle, Fill, MasterLayerScope, ObjectId, OutlineStyle,
        ParametricShape, RasterAsset, RasterLayerData, SceneObject, TextStyle,
    };

    #[test]
    fn collect_for_output_creates_complete_handoff_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let font_vault = tmp.path().join("mock_fonts");
        fs::create_dir_all(&font_vault).unwrap();
        fs::write(font_vault.join("Cairo-Regular.ttf"), b"MOCK_TTF_DATA").unwrap();

        let bundle_dir = tmp.path().join("handoff_bundle");

        let mut doc = Document::new_a4("Packaging Handoff / 2026");
        let page_id = doc.pages[0].id;
        let cut_layer = doc
            .add_layer(page_id, "CutContour", true, MasterLayerScope::None)
            .unwrap();

        // Vector shape with CutContour spot ink
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Die Cut",
                NodePayload::Shape(ParametricShape::Rectangle {
                    width: 80.0,
                    height: 50.0,
                    corner_radii: [3.0; 4],
                    corner_style: CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_fill(Fill::Solid(Color::CYAN))
            .with_outline(OutlineStyle::cut_contour())
            .with_transform(Affine2D::translate(20.0, 20.0)),
            Some(cut_layer),
        )
        .unwrap();

        // Text referencing Cairo font
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Brand Title",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "كوري ستوديو",
                    TextStyle {
                        font_family: "Cairo".to_owned(),
                        fill: Color::KEY_BLACK,
                        ..TextStyle::default()
                    },
                )),
            )
            .with_transform(Affine2D::translate(25.0, 35.0))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();

        // Embedded raster image asset
        let asset = RasterAsset::new(4, 4, vec![200u8; 4 * 4 * 4], 300.0, ColorMode::Cmyk).unwrap();
        let asset_id = doc.register_asset(asset);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Texture",
                NodePayload::Raster(RasterLayerData {
                    asset_id,
                    width_mm: 20.0,
                    height_mm: 20.0,
                    filters: Vec::new(),
                    crop_rect: None,
                }),
            )
            .with_transform(Affine2D::translate(120.0, 20.0))
            .with_outline(OutlineStyle::none()),
            None,
        )
        .unwrap();

        let opts = CollectOptions {
            extra_font_search_dirs: vec![font_vault],
            ..CollectOptions::default()
        };
        let report = collect_for_output(&doc, &bundle_dir, &opts).unwrap();

        assert!(report.document_json_path.as_ref().unwrap().is_file());
        assert!(report.manifest_path.is_file());
        assert!(report.preflight_txt_path.as_ref().unwrap().is_file());
        assert!(report.preflight_json_path.as_ref().unwrap().is_file());
        assert_eq!(report.extracted_images.len(), 1);
        assert!(bundle_dir
            .join(&report.extracted_images[0].relative_path)
            .is_file());
        assert_eq!(report.used_fonts.len(), 1);
        assert_eq!(report.used_fonts[0].family, "Cairo");
        assert!(report.used_fonts[0].copied_relative_path.is_some());
        assert!(bundle_dir
            .join(report.used_fonts[0].copied_relative_path.as_ref().unwrap())
            .is_file());
        assert!(report.spot_inks.contains(&"CutContour".to_owned()));

        // Verify extracted PNG decodes cleanly back to the exact original 4x4 RGBA image
        let decoded = image::open(bundle_dir.join(&report.extracted_images[0].relative_path))
            .unwrap()
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (4, 4));
        assert_eq!(decoded.as_raw(), &vec![200u8; 4 * 4 * 4]);

        // Verify manifest.json deserializes cleanly
        let manifest_raw = fs::read_to_string(&report.manifest_path).unwrap();
        let manifest: ProductionManifest = serde_json::from_str(&manifest_raw).unwrap();
        assert_eq!(manifest.document_title, doc.title);
        assert_eq!(manifest.extracted_images.len(), 1);
        assert!(bundle_dir.join("fonts/fonts.txt").is_file());
        assert!(bundle_dir.join("fonts/fonts_manifest.json").is_file());
    }
}
