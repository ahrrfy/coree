// SPDX-License-Identifier: MIT OR Apache-2.0

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
use std::io::{Read, Seek, SeekFrom};
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

fn default_true() -> bool {
    true
}

/// Metadata for a font family referenced by the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectedFontInfo {
    pub family: String,
    /// Relative path inside the bundle if a matching font file was found and copied.
    pub copied_relative_path: Option<String>,
    /// Raw SFNT `OS/2` `fsType` embedding flags, if present in the matched font file.
    #[serde(default)]
    pub fs_type: Option<u16>,
    /// Whether the font's `OS/2` `fsType` license permits copying/embedding (`false` for `0x0002` Restricted License).
    #[serde(default = "default_true")]
    pub embedding_allowed: bool,
    /// Human-readable embedding status / restriction summary.
    #[serde(default)]
    pub embedding_status: String,
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
    #[serde(default)]
    pub restricted_fonts: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
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
    #[serde(default)]
    pub restricted_fonts: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
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
        let safe_name = sanitize_filename(doc.title(), "document");
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
    if options.extract_images && !doc.assets().is_empty() {
        let images_dir = output_dir.join("images");
        fs::create_dir_all(&images_dir)?;
        for (&asset_id, asset) in doc.assets() {
            if asset.width == 0 || asset.height == 0 {
                return Err(ProdError::ImageError(format!(
                    "raster asset {asset_id} has zero dimensions ({}x{})",
                    asset.width, asset.height
                )));
            }
            let file_name = format!("{}.png", asset_id.0);
            let rel = format!("images/{file_name}");
            let full = images_dir.join(&file_name);

            let img = RgbaImage::from_raw(asset.width, asset.height, asset.to_rgba8_vec())
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
    let mut restricted_fonts: Vec<String> = Vec::new();
    let mut collect_warnings: Vec<String> = Vec::new();

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
            let mut fs_type = None;
            let mut embedding_allowed = true;
            let mut embedding_status = "not checked".to_owned();

            if options.copy_system_fonts {
                if let Some(src_path) = match_font_file(family, &font_index) {
                    let fstype = read_sfnt_os2_fs_type(&src_path);
                    fs_type = fstype;
                    if let Some(ft) = fstype {
                        embedding_allowed = fs_type_allows_embedding(ft);
                        embedding_status = fs_type_label(ft).to_owned();
                    } else {
                        embedding_status = "Installable / No OS/2 fsType".to_owned();
                    }

                    if !embedding_allowed {
                        let ft_val = fs_type.unwrap_or(0x0002);
                        let warn = format!(
                            "Font '{family}' has Restricted License embedding (OS/2 fsType=0x{ft_val:04X}); font file copy blocked"
                        );
                        collect_warnings.push(warn);
                        restricted_fonts.push(family.clone());
                    } else if let Some(existing_rel) = copied_sources.get(&src_path) {
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
                } else {
                    embedding_status = "missing from system font directories".to_owned();
                }
            }
            used_fonts.push(CollectedFontInfo {
                family: family.clone(),
                copied_relative_path: copied_rel,
                fs_type,
                embedding_allowed,
                embedding_status,
            });
        }

        // Write fonts.txt & fonts_manifest.json
        let mut fonts_txt = String::from("=== USED FONTS ===\n");
        if used_fonts.is_empty() {
            fonts_txt.push_str("(No live text fonts referenced)\n");
        } else {
            for f in &used_fonts {
                match (&f.copied_relative_path, f.embedding_allowed) {
                    (Some(rel), true) => {
                        fonts_txt.push_str(&format!("- {} [copied -> {rel}]\n", f.family));
                    }
                    (_, false) => {
                        fonts_txt.push_str(&format!(
                            "- {} [BLOCKED: {}]\n",
                            f.family, f.embedding_status
                        ));
                    }
                    (None, true) => {
                        fonts_txt.push_str(&format!(
                            "- {} [not copied: {}]\n",
                            f.family, f.embedding_status
                        ));
                    }
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
                fs_type: None,
                embedding_allowed: true,
                embedding_status: "collection skipped".to_owned(),
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
        document_title: doc.title().to_owned(),
        color_mode: doc.color_mode(),
        dpi: if doc.dpi().is_finite() && doc.dpi() > 0.0 {
            doc.dpi()
        } else {
            300.0
        },
        pages: doc
            .pages()
            .iter()
            .map(|p| PageSummary {
                name: p.name.clone(),
                width_mm: if p.width_mm.is_finite() {
                    p.width_mm
                } else {
                    0.0
                },
                height_mm: if p.height_mm.is_finite() {
                    p.height_mm
                } else {
                    0.0
                },
                bleed_mm: if p.bleed_mm.is_finite() {
                    p.bleed_mm
                } else {
                    0.0
                },
                layer_count: p.layers.len(),
            })
            .collect(),
        total_objects: doc.object_count(),
        spot_inks: spot_inks.clone(),
        used_fonts: used_fonts.clone(),
        restricted_fonts: restricted_fonts.clone(),
        warnings: collect_warnings.clone(),
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
        restricted_fonts,
        warnings: collect_warnings,
        spot_inks,
        preflight_report,
        production_metrics,
        total_files_written: written_files.len(),
    })
}

fn collect_used_font_families(doc: &Document) -> Vec<String> {
    let mut set = BTreeSet::new();
    let all_objs = doc
        .objects()
        .values()
        .chain(doc.symbols().values().flat_map(|s| s.objects.iter()));

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
        .objects()
        .values()
        .chain(doc.symbols().values().flat_map(|s| s.objects.iter()));

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

const MAX_SFNT_NAME_TABLE_BYTES: usize = 1_048_576;

fn normalize_font_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn insert_font_candidate(
    index: &mut BTreeMap<String, (PathBuf, u8)>,
    key: String,
    path: &Path,
    priority: u8,
) {
    if key.is_empty() {
        return;
    }
    match index.get_mut(&key) {
        Some((existing_path, existing_prio)) => {
            if priority > *existing_prio {
                *existing_path = path.to_path_buf();
                *existing_prio = priority;
            }
        }
        None => {
            index.insert(key, (path.to_path_buf(), priority));
        }
    }
}

fn decode_sfnt_name_bytes(platform_id: u16, encoding_id: u16, raw: &[u8]) -> Option<String> {
    if platform_id == 0 || (platform_id == 3 && matches!(encoding_id, 0 | 1 | 10)) {
        if raw.len() < 2 {
            return None;
        }
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        let s = String::from_utf16_lossy(&units);
        let trimmed = s.trim_matches('\0').trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    } else if platform_id == 1 && encoding_id == 0 {
        let s: String = raw.iter().map(|&b| char::from(b)).collect();
        let trimmed = s.trim_matches('\0').trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    } else {
        None
    }
}

fn extract_sfnt_face_names(file: &mut fs::File, face_offset: u64, out: &mut Vec<(String, u8)>) {
    let Ok(_) = file.seek(SeekFrom::Start(face_offset)) else {
        return;
    };
    let mut hdr = [0u8; 12];
    if file.read_exact(&mut hdr).is_err() {
        return;
    }
    let sfnt_tag = &hdr[0..4];
    if sfnt_tag != [0x00, 0x01, 0x00, 0x00]
        && sfnt_tag != b"OTTO"
        && sfnt_tag != b"true"
        && sfnt_tag != b"typ1"
    {
        return;
    }
    let num_tables = usize::from(u16::from_be_bytes([hdr[4], hdr[5]])).min(128);
    let mut dir = vec![0u8; num_tables * 16];
    if file.read_exact(&mut dir).is_err() {
        return;
    }

    let mut name_range: Option<(u64, usize)> = None;
    for rec in dir.as_chunks::<16>().0 {
        if &rec[0..4] == b"name" {
            let offset = u64::from(u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]]));
            let len = u32::from_be_bytes([rec[12], rec[13], rec[14], rec[15]]) as usize;
            if (6..=MAX_SFNT_NAME_TABLE_BYTES).contains(&len) {
                name_range = Some((offset, len));
            }
            break;
        }
    }
    let Some((name_off, name_len)) = name_range else {
        return;
    };
    if file.seek(SeekFrom::Start(name_off)).is_err() {
        return;
    }
    let mut buf = vec![0u8; name_len];
    if file.read_exact(&mut buf).is_err() {
        return;
    }

    let count = usize::from(u16::from_be_bytes([buf[2], buf[3]]));
    let storage_off = usize::from(u16::from_be_bytes([buf[4], buf[5]]));
    if 6usize.saturating_add(count.saturating_mul(12)) > buf.len() || storage_off > buf.len() {
        return;
    }

    let mut is_regular = false;
    let mut is_non_regular = false;
    let mut families = Vec::new();
    let mut full_or_ps = Vec::new();

    for i in 0..count {
        let off = 6 + i * 12;
        let platform_id = u16::from_be_bytes([buf[off], buf[off + 1]]);
        let encoding_id = u16::from_be_bytes([buf[off + 2], buf[off + 3]]);
        let name_id = u16::from_be_bytes([buf[off + 6], buf[off + 7]]);
        let str_len = usize::from(u16::from_be_bytes([buf[off + 8], buf[off + 9]]));
        let str_off = usize::from(u16::from_be_bytes([buf[off + 10], buf[off + 11]]));

        if !matches!(name_id, 1 | 2 | 4 | 6 | 16 | 17 | 21) {
            continue;
        }
        let Some(start) = storage_off.checked_add(str_off) else {
            continue;
        };
        let Some(end) = start.checked_add(str_len) else {
            continue;
        };
        if end > buf.len() {
            continue;
        }
        let Some(decoded) = decode_sfnt_name_bytes(platform_id, encoding_id, &buf[start..end])
        else {
            continue;
        };
        let key = normalize_font_key(&decoded);
        if key.is_empty() {
            continue;
        }
        match name_id {
            2 | 17 => {
                if matches!(key.as_str(), "regular" | "book" | "roman" | "normal") {
                    is_regular = true;
                } else {
                    is_non_regular = true;
                }
            }
            1 | 16 | 21 => families.push(key),
            4 | 6 => full_or_ps.push(key),
            _ => {}
        }
    }

    let family_prio = if is_regular || !is_non_regular { 3 } else { 2 };
    for fam in families {
        out.push((fam, family_prio));
    }
    for full in full_or_ps {
        out.push((full, 3));
    }
}

fn read_sfnt_index_keys(path: &Path) -> Vec<(String, u8)> {
    let Ok(mut file) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut head = [0u8; 12];
    if file.read_exact(&mut head).is_err() {
        return Vec::new();
    }
    let mut keys = Vec::new();
    if &head[0..4] == b"ttcf" {
        let num_fonts = u32::from_be_bytes([head[8], head[9], head[10], head[11]]).min(32);
        let mut offsets_raw = vec![0u8; (num_fonts as usize) * 4];
        if file.read_exact(&mut offsets_raw).is_err() {
            return keys;
        }
        for chunk in offsets_raw.as_chunks::<4>().0 {
            let off = u64::from(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            extract_sfnt_face_names(&mut file, off, &mut keys);
        }
    } else {
        extract_sfnt_face_names(&mut file, 0, &mut keys);
    }
    keys
}

fn index_font_files(dirs: &[PathBuf]) -> BTreeMap<String, PathBuf> {
    let mut ranked = BTreeMap::new();
    for dir in dirs {
        if dir.is_dir() {
            scan_font_dir(dir, 0, &mut ranked);
        }
    }
    ranked
        .into_iter()
        .map(|(k, (path, _prio))| (k, path))
        .collect()
}

fn scan_font_dir(dir: &Path, depth: usize, index: &mut BTreeMap<String, (PathBuf, u8)>) {
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
                    insert_font_candidate(index, key, &path, 1);
                }
                if !matches!(ext.as_deref(), Some("woff2")) {
                    for (sfnt_key, prio) in read_sfnt_index_keys(&path) {
                        insert_font_candidate(index, sfnt_key, &path, prio);
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

/// Shared SFNT font scanner reused by [`crate::preflight::run_preflight`].
#[must_use]
pub(crate) fn scan_system_font_index(extra_dirs: &[PathBuf]) -> BTreeMap<String, PathBuf> {
    let dirs = build_font_search_dirs(extra_dirs);
    index_font_files(&dirs)
}

/// Returns `true` if `family` resolves to any local/system SFNT font in `index`.
#[must_use]
pub(crate) fn font_index_contains(index: &BTreeMap<String, PathBuf>, family: &str) -> bool {
    match_font_file(family, index).is_some()
}

/// Returns `false` if OpenType `OS/2` `fsType` bits `0x000F` specify `0x0002` (Restricted License).
#[must_use]
pub fn fs_type_allows_embedding(fs_type: u16) -> bool {
    (fs_type & 0x000F) != 0x0002
}

/// Human-readable OpenType `OS/2` `fsType` license summary.
#[must_use]
pub fn fs_type_label(fs_type: u16) -> &'static str {
    let low = fs_type & 0x000F;
    if low == 0x0002 {
        "Restricted License embedding (OS/2 fsType=0x0002)"
    } else if (fs_type & 0x0200) != 0 {
        "Bitmap embedding only (OS/2 fsType=0x0200)"
    } else if (low & 0x0008) != 0 {
        "Editable embedding allowed"
    } else if (low & 0x0004) != 0 {
        "Preview & Print embedding allowed"
    } else {
        "Installable embedding allowed"
    }
}

/// Reads the OpenType/TrueType `OS/2` table `fsType` field from a font file (supports `.ttf/.otf/.ttc`).
#[must_use]
pub fn read_sfnt_os2_fs_type(path: &Path) -> Option<u16> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() < 12 {
        return None;
    }
    if let Ok(face) = ttf_parser::Face::parse(&bytes, 0) {
        if let Some(os2_bytes) = face.raw_face().table(ttf_parser::Tag::from_bytes(b"OS/2")) {
            if os2_bytes.len() >= 10 {
                return Some(u16::from_be_bytes([os2_bytes[8], os2_bytes[9]]));
            }
        }
    }
    // Direct SFNT `OS/2` table offset lookup (handles single-table/minimal font binaries).
    let face_off = if &bytes[0..4] == b"ttcf" && bytes.len() >= 16 {
        u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize
    } else {
        0usize
    };
    if face_off + 12 > bytes.len() {
        return None;
    }
    let num_tables = usize::from(u16::from_be_bytes([
        bytes[face_off + 4],
        bytes[face_off + 5],
    ]))
    .min(128);
    let dir_start = face_off + 12;
    let dir_end = dir_start.checked_add(num_tables * 16)?;
    if dir_end > bytes.len() {
        return None;
    }
    for rec in bytes[dir_start..dir_end].as_chunks::<16>().0 {
        if &rec[0..4] == b"OS/2" {
            let off = u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]]) as usize;
            let len = u32::from_be_bytes([rec[12], rec[13], rec[14], rec[15]]) as usize;
            if len >= 10 && off + 10 <= bytes.len() {
                return Some(u16::from_be_bytes([bytes[off + 8], bytes[off + 9]]));
            }
        }
    }
    None
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
        let page_id = doc.pages()[0].id;
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
        assert_eq!(manifest.document_title, doc.title());
        assert_eq!(manifest.extracted_images.len(), 1);
        assert!(bundle_dir.join("fonts/fonts.txt").is_file());
        assert!(bundle_dir.join("fonts/fonts_manifest.json").is_file());
    }

    fn build_mock_sfnt_with_names(records: &[(u16, &str)]) -> Vec<u8> {
        let mut string_pool = Vec::new();
        let mut raw_records = Vec::new();
        for &(name_id, text) in records {
            let off = string_pool.len() as u16;
            let mut utf16_bytes = Vec::new();
            for unit in text.encode_utf16() {
                utf16_bytes.extend_from_slice(&unit.to_be_bytes());
            }
            let len = utf16_bytes.len() as u16;
            string_pool.extend_from_slice(&utf16_bytes);
            raw_records.push((name_id, off, len));
        }

        let name_header_and_recs = 6 + raw_records.len() * 12;
        let mut name_table = Vec::new();
        name_table.extend_from_slice(&0u16.to_be_bytes()); // format 0
        name_table.extend_from_slice(&(raw_records.len() as u16).to_be_bytes());
        name_table.extend_from_slice(&(name_header_and_recs as u16).to_be_bytes());
        for (name_id, off, len) in raw_records {
            name_table.extend_from_slice(&3u16.to_be_bytes()); // Windows platformID
            name_table.extend_from_slice(&1u16.to_be_bytes()); // Unicode BMP encodingID
            name_table.extend_from_slice(&0x0409u16.to_be_bytes()); // en-US
            name_table.extend_from_slice(&name_id.to_be_bytes());
            name_table.extend_from_slice(&len.to_be_bytes());
            name_table.extend_from_slice(&off.to_be_bytes());
        }
        name_table.extend_from_slice(&string_pool);

        let name_offset: u32 = 12 + 16;
        let mut sfnt = Vec::new();
        sfnt.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]); // TrueType sfnt version
        sfnt.extend_from_slice(&1u16.to_be_bytes()); // numTables = 1
        sfnt.extend_from_slice(&[0u8; 6]); // searchRange / entrySelector / rangeShift
        sfnt.extend_from_slice(b"name");
        sfnt.extend_from_slice(&0u32.to_be_bytes()); // checkSum
        sfnt.extend_from_slice(&name_offset.to_be_bytes());
        sfnt.extend_from_slice(&(name_table.len() as u32).to_be_bytes());
        sfnt.extend_from_slice(&name_table);
        sfnt
    }

    #[test]
    fn collects_fonts_whose_filenames_differ_from_sfnt_family_including_arabic() {
        let tmp = tempfile::tempdir().unwrap();
        let font_vault = tmp.path().join("system_fonts");
        fs::create_dir_all(&font_vault).unwrap();

        // Bold face stored in generic cryptic filename `aa_bold.ttf`
        fs::write(
            font_vault.join("aa_bold.ttf"),
            build_mock_sfnt_with_names(&[(1, "Courier New"), (2, "Bold"), (4, "Courier New Bold")]),
        )
        .unwrap();

        // Regular face stored in `cour.ttf` — Regular must win bare family key over Bold
        fs::write(
            font_vault.join("cour.ttf"),
            build_mock_sfnt_with_names(&[
                (1, "Courier New"),
                (2, "Regular"),
                (6, "CourierNewPSMT"),
            ]),
        )
        .unwrap();

        // Arabic family name in SFNT table stored inside `ar_01.otf`
        fs::write(
            font_vault.join("ar_01.otf"),
            build_mock_sfnt_with_names(&[(1, "أميري"), (2, "Regular")]),
        )
        .unwrap();

        let index = index_font_files(&[font_vault]);
        let matched_cour = match_font_file("Courier New", &index).unwrap();
        assert_eq!(matched_cour.file_name().unwrap(), "cour.ttf");

        let matched_arabic = match_font_file("أميري", &index).unwrap();
        assert_eq!(matched_arabic.file_name().unwrap(), "ar_01.otf");
    }

    #[test]
    fn collect_blocks_restricted_license_fstype_font() {
        let tmp = tempfile::tempdir().unwrap();
        let font_vault = tmp.path().join("restricted_fonts");
        fs::create_dir_all(&font_vault).unwrap();

        // Build minimal SFNT with both `name` ("LockedSerif") and `OS/2` (fsType = 0x0002).
        let name_only = build_mock_sfnt_with_names(&[(1, "LockedSerif"), (2, "Regular")]);
        let name_table = &name_only[12 + 16..];
        let mut os2_table = vec![0u8; 78];
        os2_table[8..10].copy_from_slice(&0x0002u16.to_be_bytes()); // fsType = Restricted License

        let dir_size = 2 * 16u32;
        let name_off = 12 + dir_size;
        let os2_off = name_off + (name_table.len() as u32);
        let mut sfnt = Vec::new();
        sfnt.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        sfnt.extend_from_slice(&2u16.to_be_bytes());
        sfnt.extend_from_slice(&[0u8; 6]);
        // name record
        sfnt.extend_from_slice(b"name");
        sfnt.extend_from_slice(&0u32.to_be_bytes());
        sfnt.extend_from_slice(&name_off.to_be_bytes());
        sfnt.extend_from_slice(&(name_table.len() as u32).to_be_bytes());
        // OS/2 record
        sfnt.extend_from_slice(b"OS/2");
        sfnt.extend_from_slice(&0u32.to_be_bytes());
        sfnt.extend_from_slice(&os2_off.to_be_bytes());
        sfnt.extend_from_slice(&(os2_table.len() as u32).to_be_bytes());
        sfnt.extend_from_slice(name_table);
        sfnt.extend_from_slice(&os2_table);

        let font_file = font_vault.join("locked_serif.ttf");
        fs::write(&font_file, &sfnt).unwrap();
        assert_eq!(read_sfnt_os2_fs_type(&font_file), Some(0x0002));
        assert!(!fs_type_allows_embedding(0x0002));

        let mut doc = Document::new_a4("Restricted Font Test");
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Title",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "Restricted",
                    TextStyle {
                        font_family: "LockedSerif".to_owned(),
                        fill: Color::KEY_BLACK,
                        ..TextStyle::default()
                    },
                )),
            ),
            None,
        )
        .unwrap();

        let bundle = tmp.path().join("bundle");
        let report = collect_for_output(
            &doc,
            &bundle,
            &CollectOptions {
                extra_font_search_dirs: vec![font_vault],
                ..CollectOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.used_fonts[0].copied_relative_path, None);
        assert!(!report.used_fonts[0].embedding_allowed);
        assert_eq!(report.used_fonts[0].fs_type, Some(0x0002));
        assert!(report.restricted_fonts.contains(&"LockedSerif".to_owned()));
        assert!(!report.warnings.is_empty());
        assert!(!bundle.join("fonts/locked_serif.ttf").exists());
    }
}
