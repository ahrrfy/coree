// SPDX-License-Identifier: MIT OR Apache-2.0

//! `.coree` open native lossless ZIP container with externalized PNG raster assets,
//! optional preview thumbnail, atomic saves, and Zip-Bomb / Zip-Slip hardening.

use crate::atomic::atomic_write_with;
use crate::error::{sanitize_zip_path, IoError, IoLimits, IoResult};
use crate::raster::{
    decode_raster_bytes_with_limits, export_raster_to_bytes, validate_raster_dimensions,
    RasterExportOptions, RasterFormat,
};
use chrono::{DateTime, Utc};
use coree_core::brand::{APP_NAME, VERSION};
use coree_core::{ColorMode, Document, ImageAssetId, RasterAsset};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Current `.coree` container schema version.
pub const COREE_FORMAT_VERSION: u32 = 1;

const MANIFEST_ENTRY: &str = "manifest.json";
const DOCUMENT_ENTRY: &str = "document.json";
const ASSETS_MANIFEST_ENTRY: &str = "assets/manifest.json";
const PREVIEW_ENTRY: &str = "preview.png";

/// Top-level container metadata written to `manifest.json` inside `.coree` archives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreeManifest {
    pub format_version: u32,
    pub app: String,
    pub app_version: String,
    pub document_id: Uuid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
    pub page_count: usize,
    pub asset_count: usize,
    pub has_preview: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AssetIndexEntry {
    id: ImageAssetId,
    width: u32,
    height: u32,
    source_dpi: f32,
    color_mode: ColorMode,
    path: String,
}

/// Atomically saves `doc` into a lossless `.coree` ZIP container at `path`.
pub fn save_coree(doc: &Document, path: &Path) -> IoResult<()> {
    save_coree_with_preview(doc, None, path)
}

/// Atomically saves `doc` (with an optional PNG `preview_png` thumbnail) into a `.coree` container.
pub fn save_coree_with_preview(
    doc: &Document,
    preview_png: Option<&[u8]>,
    path: &Path,
) -> IoResult<()> {
    // Externalize heavy raster assets as standalone `assets/<uuid>.png` files so
    // `document.json` stays compact and fast to parse while preserving 100% lossless data.
    let mut light_doc = doc.clone();
    let externalized_assets = light_doc.take_assets();

    let mut asset_entries = Vec::with_capacity(externalized_assets.len());
    let mut encoded_assets: Vec<(String, Vec<u8>)> = Vec::with_capacity(externalized_assets.len());

    for (asset_id, asset) in &externalized_assets {
        let entry_path = format!("assets/{}.png", asset_id.0);
        let png_bytes = export_raster_to_bytes(
            asset.width,
            asset.height,
            &asset.rgba_bytes,
            &RasterExportOptions::new(RasterFormat::Png, asset.source_dpi),
        )?;
        asset_entries.push(AssetIndexEntry {
            id: *asset_id,
            width: asset.width,
            height: asset.height,
            source_dpi: asset.source_dpi,
            color_mode: asset.color_mode,
            path: entry_path.clone(),
        });
        encoded_assets.push((entry_path, png_bytes));
    }

    let manifest = CoreeManifest {
        format_version: COREE_FORMAT_VERSION,
        app: APP_NAME.to_owned(),
        app_version: VERSION.to_owned(),
        document_id: doc.id(),
        title: doc.title().to_owned(),
        created_at: doc.metadata().created_at,
        modified_at: doc.metadata().modified_at,
        page_count: doc.pages().len(),
        asset_count: externalized_assets.len(),
        has_preview: preview_png.is_some(),
    };

    let manifest_json = serde_json::to_vec_pretty(&manifest)?;
    let document_json = serde_json::to_vec(&light_doc)?;
    let assets_manifest_json = if !asset_entries.is_empty() {
        Some(serde_json::to_vec_pretty(&asset_entries)?)
    } else {
        None
    };

    atomic_write_with(path, |file| {
        let mut zip = ZipWriter::new(file);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

        zip.start_file(MANIFEST_ENTRY, deflated)?;
        zip.write_all(&manifest_json)?;

        zip.start_file(DOCUMENT_ENTRY, deflated)?;
        zip.write_all(&document_json)?;

        if let Some(ref meta_bytes) = assets_manifest_json {
            zip.start_file(ASSETS_MANIFEST_ENTRY, deflated)?;
            zip.write_all(meta_bytes)?;
        }

        for (entry_path, png_bytes) in &encoded_assets {
            zip.start_file(entry_path, stored)?;
            zip.write_all(png_bytes)?;
        }

        if let Some(preview) = preview_png {
            zip.start_file(PREVIEW_ENTRY, stored)?;
            zip.write_all(preview)?;
        }

        zip.finish()?;
        Ok(())
    })
}

/// Loads a `.coree` document from `path` with default Zip-Bomb and Zip-Slip safeguards.
pub fn load_coree(path: &Path) -> IoResult<Document> {
    load_coree_with_limits(path, &IoLimits::default())
}

/// Loads a `.coree` document from `path` enforcing custom [`IoLimits`].
pub fn load_coree_with_limits(path: &Path, limits: &IoLimits) -> IoResult<Document> {
    let file = File::open(path)?;
    load_coree_from_reader(file, limits)
}

/// Loads a `.coree` document from any seekable reader enforcing `limits`.
pub fn load_coree_from_reader<R: Read + Seek>(
    mut reader: R,
    limits: &IoLimits,
) -> IoResult<Document> {
    let archive_byte_len = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;

    let mut archive = ZipArchive::new(reader)?;
    let entry_index = validate_archive_entries(&mut archive, archive_byte_len, limits)?;

    let mut total_uncompressed: u64 = 0;

    // 1. Validate manifest.json
    let manifest_idx = *entry_index
        .get(MANIFEST_ENTRY)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "coree".into(),
            message: "missing manifest.json".into(),
        })?;
    let manifest_bytes = read_zip_entry_bounded(
        &mut archive,
        manifest_idx,
        archive_byte_len,
        limits,
        &mut total_uncompressed,
    )?;
    let manifest: CoreeManifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.app != APP_NAME {
        return Err(IoError::InvalidFormat {
            format: "coree".into(),
            message: format!("unexpected manifest app '{}'", manifest.app),
        });
    }
    if manifest.format_version == 0 || manifest.format_version > COREE_FORMAT_VERSION {
        return Err(IoError::InvalidFormat {
            format: "coree".into(),
            message: format!(
                "unsupported .coree format_version {} (max supported {})",
                manifest.format_version, COREE_FORMAT_VERSION
            ),
        });
    }

    // 2. Load document.json
    let doc_idx = *entry_index
        .get(DOCUMENT_ENTRY)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "coree".into(),
            message: "missing document.json".into(),
        })?;
    let doc_bytes = read_zip_entry_bounded(
        &mut archive,
        doc_idx,
        archive_byte_len,
        limits,
        &mut total_uncompressed,
    )?;
    let mut doc: Document = serde_json::from_slice(&doc_bytes)?;

    // 3. Rehydrate externalized raster assets enforcing deduplication, path scoping,
    // and cumulative RGBA memory & pixel limits across all assets.
    let mut restored_assets: BTreeMap<ImageAssetId, RasterAsset> = doc.take_assets();
    let mut total_raster_pixels: u64 = 0;
    let mut total_rgba_bytes: u64 = 0;
    for asset in restored_assets.values() {
        validate_raster_dimensions(asset.width, asset.height, limits)?;
        let expected_len = u64::from(asset.width)
            .saturating_mul(u64::from(asset.height))
            .saturating_mul(4);
        if (asset.rgba_bytes.len() as u64) != expected_len {
            return Err(IoError::InvalidFormat {
                format: "coree".into(),
                message: format!(
                    "inline asset {} rgba_bytes length {} does not match {}x{}x4",
                    asset.id,
                    asset.rgba_bytes.len(),
                    asset.width,
                    asset.height
                ),
            });
        }
        charge_raster_budget(
            asset.width,
            asset.height,
            limits,
            &mut total_raster_pixels,
            &mut total_rgba_bytes,
        )?;
    }

    if let Some(&assets_meta_idx) = entry_index.get(ASSETS_MANIFEST_ENTRY) {
        let meta_bytes = read_zip_entry_bounded(
            &mut archive,
            assets_meta_idx,
            archive_byte_len,
            limits,
            &mut total_uncompressed,
        )?;
        let entries: Vec<AssetIndexEntry> = serde_json::from_slice(&meta_bytes)?;
        if entries.len() > limits.max_zip_entries {
            return Err(IoError::ZipBomb {
                reason: format!(
                    "assets manifest lists {} entries (limit {})",
                    entries.len(),
                    limits.max_zip_entries
                ),
            });
        }

        let mut seen_paths: HashSet<String> = HashSet::with_capacity(entries.len());
        let mut seen_ids: HashSet<ImageAssetId> = HashSet::with_capacity(entries.len());

        for meta in entries {
            let safe_path = sanitize_zip_path(&meta.path)?;
            if !safe_path.starts_with("assets/") {
                return Err(IoError::ZipSlip { path: meta.path });
            }
            if !seen_paths.insert(safe_path.clone()) {
                return Err(IoError::InvalidFormat {
                    format: "coree".into(),
                    message: format!("duplicate asset path '{safe_path}' in assets/manifest.json"),
                });
            }
            if !seen_ids.insert(meta.id) {
                return Err(IoError::InvalidFormat {
                    format: "coree".into(),
                    message: format!("duplicate asset id '{}' in assets/manifest.json", meta.id),
                });
            }

            // Validate and reserve declared raster RAM budget BEFORE decoding the PNG.
            validate_raster_dimensions(meta.width, meta.height, limits)?;
            charge_raster_budget(
                meta.width,
                meta.height,
                limits,
                &mut total_raster_pixels,
                &mut total_rgba_bytes,
            )?;

            let idx = *entry_index
                .get(&safe_path)
                .ok_or_else(|| IoError::InvalidFormat {
                    format: "coree".into(),
                    message: format!("missing externalized asset '{safe_path}'"),
                })?;
            let png_bytes = read_zip_entry_bounded(
                &mut archive,
                idx,
                archive_byte_len,
                limits,
                &mut total_uncompressed,
            )?;
            // Inspect actual image header dimensions BEFORE full RGBA decode so a forged
            // `assets/manifest.json` declaring 1x1 for a giant PNG cannot allocate RAM first.
            let (actual_w, actual_h) = image::ImageReader::new(std::io::Cursor::new(&png_bytes))
                .with_guessed_format()?
                .into_dimensions()?;
            if actual_w != meta.width || actual_h != meta.height {
                return Err(IoError::InvalidFormat {
                    format: "coree".into(),
                    message: format!(
                        "asset {} dimensions mismatch: manifest {}x{}, png {}x{}",
                        meta.id, meta.width, meta.height, actual_w, actual_h
                    ),
                });
            }
            let mut decoded = decode_raster_bytes_with_limits(&png_bytes, limits)?;
            if decoded.width != meta.width || decoded.height != meta.height {
                return Err(IoError::InvalidFormat {
                    format: "coree".into(),
                    message: format!(
                        "asset {} dimensions mismatch: manifest {}x{}, png {}x{}",
                        meta.id, meta.width, meta.height, decoded.width, decoded.height
                    ),
                });
            }
            decoded.id = meta.id;
            decoded.source_dpi = meta.source_dpi;
            decoded.color_mode = meta.color_mode;
            restored_assets.insert(meta.id, decoded);
        }
    } else {
        // Fallback: load any `assets/<uuid>.png` entries directly if no assets/manifest.json exists.
        let mut fallback_paths: Vec<(String, usize)> = entry_index
            .iter()
            .filter(|(name, _)| name.starts_with("assets/") && name.ends_with(".png"))
            .map(|(name, &idx)| (name.clone(), idx))
            .collect();
        fallback_paths.sort_by(|a, b| a.0.cmp(&b.0));

        for (name, idx) in fallback_paths {
            let stem = name.trim_start_matches("assets/").trim_end_matches(".png");
            if let Ok(uuid) = Uuid::parse_str(stem) {
                let id = ImageAssetId(uuid);
                if let std::collections::btree_map::Entry::Vacant(entry) = restored_assets.entry(id)
                {
                    let png_bytes = read_zip_entry_bounded(
                        &mut archive,
                        idx,
                        archive_byte_len,
                        limits,
                        &mut total_uncompressed,
                    )?;
                    // Inspect header dimensions and check cumulative budget before full decode.
                    let (w, h) = image::ImageReader::new(std::io::Cursor::new(&png_bytes))
                        .with_guessed_format()?
                        .into_dimensions()?;
                    validate_raster_dimensions(w, h, limits)?;
                    charge_raster_budget(
                        w,
                        h,
                        limits,
                        &mut total_raster_pixels,
                        &mut total_rgba_bytes,
                    )?;
                    let mut decoded = decode_raster_bytes_with_limits(&png_bytes, limits)?;
                    decoded.id = id;
                    entry.insert(decoded);
                }
            }
        }
    }

    doc.restore_assets(restored_assets);
    Ok(doc)
}

/// Reads only `manifest.json` from a `.coree` file (fast Welcome Hub inspection).
pub fn read_coree_manifest(path: &Path) -> IoResult<CoreeManifest> {
    let limits = IoLimits::default();
    let mut file = File::open(path)?;
    let archive_byte_len = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(0))?;
    let mut archive = ZipArchive::new(file)?;
    let entry_index = validate_archive_entries(&mut archive, archive_byte_len, &limits)?;
    let idx = *entry_index
        .get(MANIFEST_ENTRY)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "coree".into(),
            message: "missing manifest.json".into(),
        })?;
    let mut total = 0u64;
    let bytes = read_zip_entry_bounded(&mut archive, idx, archive_byte_len, &limits, &mut total)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Reads the optional `preview.png` thumbnail bytes from a `.coree` container.
pub fn read_coree_preview(path: &Path) -> IoResult<Option<Vec<u8>>> {
    let limits = IoLimits::default();
    let mut file = File::open(path)?;
    let archive_byte_len = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(0))?;
    let mut archive = ZipArchive::new(file)?;
    let entry_index = validate_archive_entries(&mut archive, archive_byte_len, &limits)?;
    let Some(&idx) = entry_index.get(PREVIEW_ENTRY) else {
        return Ok(None);
    };
    let mut total = 0u64;
    let bytes = read_zip_entry_bounded(&mut archive, idx, archive_byte_len, &limits, &mut total)?;
    Ok(Some(bytes))
}

fn charge_raster_budget(
    width: u32,
    height: u32,
    limits: &IoLimits,
    total_pixels: &mut u64,
    total_rgba_bytes: &mut u64,
) -> IoResult<()> {
    let px = u64::from(width).saturating_mul(u64::from(height));
    let rgba = px.saturating_mul(4);
    let next_px = total_pixels.saturating_add(px);
    if next_px > limits.max_raster_pixels {
        return Err(IoError::LimitExceeded {
            limit: format!("cumulative max_raster_pixels={}", limits.max_raster_pixels),
            actual: format!("{next_px} pixels"),
        });
    }
    let next_rgba = total_rgba_bytes.saturating_add(rgba);
    if next_rgba > limits.max_uncompressed_bytes {
        return Err(IoError::ZipBomb {
            reason: format!(
                "cumulative decoded RGBA asset memory {next_rgba}B exceeds limit {}B",
                limits.max_uncompressed_bytes
            ),
        });
    }
    *total_pixels = next_px;
    *total_rgba_bytes = next_rgba;
    Ok(())
}

/// Pre-scans every archive entry for Zip-Slip path traversal, duplicate paths,
/// forged compressed sizes, excessive entry count, and header-declared Zip-Bomb sizes/ratios.
fn validate_archive_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    archive_byte_len: u64,
    limits: &IoLimits,
) -> IoResult<HashMap<String, usize>> {
    let count = archive.len();
    if count > limits.max_zip_entries {
        return Err(IoError::ZipBomb {
            reason: format!(
                "archive contains {count} entries (limit {})",
                limits.max_zip_entries
            ),
        });
    }

    let mut declared_total: u64 = 0;
    let mut declared_compressed_total: u64 = 0;
    let mut index = HashMap::with_capacity(count);

    for i in 0..count {
        let entry = archive.by_index_raw(i)?;
        let raw_name = entry.name()?.into_owned();
        if entry.enclosed_name().is_none() {
            return Err(IoError::ZipSlip { path: raw_name });
        }
        let safe_name = sanitize_zip_path(&raw_name)?;

        let declared_size = entry.size();
        let compressed_size = entry.compressed_size();
        if compressed_size > archive_byte_len {
            return Err(IoError::ZipBomb {
                reason: format!(
                    "entry '{safe_name}' has forged compressed_size {compressed_size}B exceeding archive length {archive_byte_len}B"
                ),
            });
        }
        declared_compressed_total = declared_compressed_total.saturating_add(compressed_size);
        if declared_compressed_total > archive_byte_len {
            return Err(IoError::ZipBomb {
                reason: format!(
                    "archive entries overlap (sum of compressed_size {declared_compressed_total}B exceeds archive length {archive_byte_len}B)"
                ),
            });
        }

        declared_total = declared_total.saturating_add(declared_size);
        if declared_total > limits.max_uncompressed_bytes {
            return Err(IoError::ZipBomb {
                reason: format!(
                    "declared total uncompressed size {declared_total} exceeds limit {}",
                    limits.max_uncompressed_bytes
                ),
            });
        }
        check_compression_ratio(&safe_name, declared_size, compressed_size, limits)?;

        if index.insert(safe_name.clone(), i).is_some() {
            return Err(IoError::InvalidFormat {
                format: "coree".into(),
                message: format!("duplicate archive entry '{safe_name}'"),
            });
        }
    }

    Ok(index)
}

/// Streams a single ZIP entry in 16 KiB chunks, incrementally enforcing BOTH cumulative
/// uncompressed byte limit AND compression ratio on every chunk so Zip-Bombs abort early.
fn read_zip_entry_bounded<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: usize,
    archive_byte_len: u64,
    limits: &IoLimits,
    total_read: &mut u64,
) -> IoResult<Vec<u8>> {
    let mut entry = archive.by_index(index)?;
    let name = entry.name()?.into_owned();
    let compressed_size = entry.compressed_size();
    if compressed_size > archive_byte_len {
        return Err(IoError::ZipBomb {
            reason: format!(
                "entry '{name}' has forged compressed_size {compressed_size}B exceeding archive length {archive_byte_len}B"
            ),
        });
    }

    let remaining = limits.max_uncompressed_bytes.saturating_sub(*total_read);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];

    loop {
        let n = entry.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let next_len = (buf.len() as u64).saturating_add(n as u64);
        if next_len > remaining {
            return Err(IoError::ZipBomb {
                reason: format!(
                    "entry '{name}' decompressed beyond total limit of {} bytes",
                    limits.max_uncompressed_bytes
                ),
            });
        }
        check_compression_ratio(&name, next_len, compressed_size, limits)?;
        buf.extend_from_slice(&chunk[..n]);
    }

    *total_read = total_read.saturating_add(buf.len() as u64);
    Ok(buf)
}

fn check_compression_ratio(
    entry_name: &str,
    uncompressed: u64,
    compressed: u64,
    limits: &IoLimits,
) -> IoResult<()> {
    if uncompressed >= limits.min_ratio_check_bytes {
        let denom = compressed.max(1) as f64;
        let ratio = (uncompressed as f64) / denom;
        if ratio > limits.max_compression_ratio {
            return Err(IoError::ZipBomb {
                reason: format!(
                    "entry '{entry_name}' compression ratio {ratio:.1}x exceeds limit {:.1}x ({uncompressed}B / {compressed}B)",
                    limits.max_compression_ratio
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        ArtisticText, Color, CornerStyle, DimensionKind, DimensionObject, Effect, Fill,
        GuideOrientation, LensConfig, LensKind, MasterLayerScope, NodePayload, ObjectId,
        OutlineStyle, ParametricShape, Point2D, RasterLayerData, SceneObject, TextStyle,
        UnknownCdrChunk, VectorTable,
    };
    use std::io::Cursor;

    fn build_rich_document() -> Document {
        let mut doc = Document::new_a4("Rich Native Container Test");
        let page_id = doc.pages()[0].id;
        let master_layer = doc
            .add_layer(page_id, "Master Header", false, MasterLayerScope::AllPages)
            .unwrap();
        let cut_layer = doc
            .add_layer(page_id, "CutContour", true, MasterLayerScope::None)
            .unwrap();

        let asset =
            RasterAsset::new(4, 4, [20, 120, 220, 255].repeat(16), 300.0, ColorMode::Rgb).unwrap();
        let asset_id = doc.register_asset(asset);

        let mut shape = SceneObject::new(
            ObjectId(0),
            "Lensed Box",
            NodePayload::Shape(ParametricShape::Rectangle {
                width: 60.0,
                height: 40.0,
                corner_radii: [4.0; 4],
                corner_style: CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::CYAN))
        .with_outline(OutlineStyle::cut_contour());
        shape.lens = Some(LensConfig {
            kind: LensKind::Magnify { amount: 2.0 },
            frozen: false,
            viewpoint: None,
            remove_face: false,
        });
        shape.effects.push(Effect::DropShadow {
            dx_mm: 2.0,
            dy_mm: 2.0,
            blur_mm: 3.0,
            opacity: 0.4,
            color: Color::BLACK,
            feather_direction: coree_core::FeatherDirection::Middle,
        });
        shape.unknown_chunks.push(UnknownCdrChunk {
            fourcc: *b"loda",
            version_hint: 1700,
            raw_bytes: vec![1, 2, 3, 4, 5],
        });
        doc.add_object(shape, Some(cut_layer)).unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Photo",
                NodePayload::Raster(RasterLayerData {
                    asset_id,
                    width_mm: 50.0,
                    height_mm: 50.0,
                    filters: Vec::new(),
                    crop_rect: None,
                }),
            ),
            Some(master_layer),
        )
        .unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Table",
                NodePayload::Table(VectorTable::new(2, 3, 15.0, 8.0)),
            ),
            None,
        )
        .unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Dim",
                NodePayload::Dimension(DimensionObject::linear(
                    DimensionKind::Horizontal,
                    Point2D::ZERO,
                    Point2D::new(60.0, 0.0),
                    5.0,
                )),
            ),
            None,
        )
        .unwrap();

        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Arabic Title",
                NodePayload::ArtisticText(ArtisticText::from_plain(
                    "تصميم احترافي",
                    TextStyle::default(),
                )),
            ),
            None,
        )
        .unwrap();

        doc.add_guide(GuideOrientation::Horizontal(25.0), true);
        doc.push_unknown_doc_chunk(UnknownCdrChunk {
            fourcc: *b"vrsn",
            version_hint: 2400,
            raw_bytes: vec![0xAA, 0xBB],
        });
        doc
    }

    #[test]
    fn coree_native_roundtrip_is_100_percent_lossless_with_external_assets_and_preview() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.coree");
        let doc = build_rich_document();
        let preview = export_raster_to_bytes(
            2,
            2,
            &[255, 0, 0, 255].repeat(4),
            &RasterExportOptions::new(RasterFormat::Png, 96.0),
        )
        .unwrap();

        save_coree_with_preview(&doc, Some(&preview), &path).unwrap();

        let manifest = read_coree_manifest(&path).unwrap();
        assert_eq!(manifest.app, "coree");
        assert_eq!(manifest.format_version, COREE_FORMAT_VERSION);
        assert_eq!(manifest.asset_count, 1);
        assert!(manifest.has_preview);

        let loaded_preview = read_coree_preview(&path).unwrap().unwrap();
        assert_eq!(loaded_preview, preview);

        let restored = load_coree(&path).unwrap();
        assert_eq!(restored, doc);

        // Verify assets are genuinely externalized inside `assets/<uuid>.png`
        let file = File::open(&path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let mut names = Vec::new();
        for i in 0..archive.len() {
            names.push(archive.by_index(i).unwrap().name().unwrap().into_owned());
        }
        assert!(names.iter().any(|n| n == "manifest.json"));
        assert!(names.iter().any(|n| n == "document.json"));
        assert!(names
            .iter()
            .any(|n| n.starts_with("assets/") && n.ends_with(".png")));
        assert!(names.iter().any(|n| n == "preview.png"));
    }

    #[test]
    fn coree_rejects_zip_slip_traversal_archive() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let opts = SimpleFileOptions::default();
            zip.start_file("../escape.txt", opts).unwrap();
            zip.write_all(b"evil").unwrap();
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        let err = load_coree_from_reader(cursor, &IoLimits::default()).unwrap_err();
        assert!(matches!(err, IoError::ZipSlip { .. }));
    }

    #[test]
    fn coree_rejects_zip_bomb_high_compression_ratio() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            zip.start_file("document.json", opts).unwrap();
            // 2 MiB of zeroes compresses to ~2 KiB -> ratio > 500x (> 100x limit)
            let zeros = vec![b' '; 2 * 1024 * 1024];
            zip.write_all(&zeros).unwrap();
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        let err = load_coree_from_reader(cursor, &IoLimits::default()).unwrap_err();
        assert!(matches!(err, IoError::ZipBomb { .. }));
    }

    #[test]
    fn coree_rejects_zip_bomb_exceeding_max_uncompressed_bytes() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zip.start_file("document.json", opts).unwrap();
            zip.write_all(&vec![b'a'; 2048]).unwrap();
            zip.finish().unwrap();
        }
        cursor.set_position(0);
        let tiny_limits = IoLimits {
            max_uncompressed_bytes: 1024,
            ..IoLimits::default()
        };
        let err = load_coree_from_reader(cursor, &tiny_limits).unwrap_err();
        assert!(matches!(err, IoError::ZipBomb { .. }));
    }
}
