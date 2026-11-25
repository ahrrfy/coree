// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room pure-Rust Adobe Photoshop `.psd` (v1) and `.psb` (v2) multi-layer importer & exporter.
//!
//! Features:
//! - Streaming `Read + Seek` parser with row-by-row PackBits RLE & ZIP decompression so
//!   multi-hundred-megabyte PSD files (including `4096x4096` 16-bit multi-layer documents)
//!   decode well below `768 MiB` RSS.
//! - Extracts physical DPI from Image Resource `1005` (`ResolutionInfo`) and Spot/Alpha
//!   channel names from Image Resources `1007` & `1045`.
//! - Genuine `16-bit` (`u16`) channel preservation into [`PixelDepth::Bit16`] [`RasterAsset`]s
//!   as well as 1-bit Bitmap and 8-bit channels across Grayscale, Indexed, RGB, CMYK, and Lab.
//! - Decodes `-1` alpha, `-2`/`-3` user/vector layer masks, `>= 4` Spot channels, and
//!   `lsct`/`lsdk` folder hierarchy (`NodePayload::Group`).
//! - High-resolution ceiling (`16_384 px` output dim) with automatic streaming box downscaling
//!   above `16_384 px` while preserving exact physical millimeter dimensions and placement.
//! - Pure-Rust multi-layer `export_psd` / `export_psd_to_bytes` (`8BPS` v1 / `PSB` v2).

use crate::atomic::atomic_write_with;
use crate::error::{IoError, IoResult};
use coree_core::{
    Affine2D, BlendMode, Color, ColorMode, ColorSwatch, Document, Fill, MasterLayerScope,
    NodePayload, ObjectId, OutlineStyle, PixelDepth, RasterAsset, RasterLayerData, SceneObject,
    RASTER_TILE_DIM,
};
use coree_render::{render_isolated_object_to_rgba, render_page_to_rgba, ViewMode};
use flate2::read::ZlibDecoder;
use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Maximum header canvas dimension accepted (PSB spec ceiling = 300,000 px).
const MAX_PSD_DIMENSION: u32 = 300_000;
/// Maximum number of layers accepted in a single PSD/PSB document.
const MAX_PSD_LAYERS: usize = 4_096;
/// Maximum output pixel dimension per imported layer/composite (`16,384 px`).
const MAX_LAYER_OUTPUT_DIM: u32 = 16_384;
/// Strict cumulative decoded pixel byte budget across all imported layers (`512 MiB`).
const MAX_CUMULATIVE_RGBA_BYTES: usize = 512 * 1024 * 1024;
/// Maximum compressed/decompressed bytes allowed for a single ZIP channel stream (`128 MiB`).
const MAX_ZIP_CHANNEL_BYTES: usize = 128 * 1024 * 1024;
/// Maximum single Image Resources section scanned in memory (`16 MiB`).
const MAX_IMAGE_RESOURCES_SCAN: u64 = 16 * 1024 * 1024;

/// Options controlling `.psd`/`.psb` binary export.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PsdExportOptions {
    /// Emit PSB (v2, 64-bit section lengths) instead of classic PSD (v1).
    pub is_psb: bool,
    /// Optional explicit rasterization DPI (defaults to `doc.dpi()`).
    pub dpi: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PsdColorMode {
    Bitmap,
    Grayscale,
    Indexed,
    Rgb,
    Cmyk,
    Multichannel,
    Duotone,
    Lab,
}

impl PsdColorMode {
    fn from_u16(code: u16) -> IoResult<Self> {
        match code {
            0 => Ok(Self::Bitmap),
            1 => Ok(Self::Grayscale),
            2 => Ok(Self::Indexed),
            3 => Ok(Self::Rgb),
            4 => Ok(Self::Cmyk),
            7 => Ok(Self::Multichannel),
            8 => Ok(Self::Duotone),
            9 => Ok(Self::Lab),
            other => Err(IoError::InvalidFormat {
                format: "psd".into(),
                message: format!("unsupported PSD color mode {other}"),
            }),
        }
    }

    fn doc_color_mode(self) -> ColorMode {
        match self {
            Self::Cmyk => ColorMode::Cmyk,
            _ => ColorMode::Rgb,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PsdHeader {
    is_psb: bool,
    channels: u16,
    height: u32,
    width: u32,
    depth: u16,
    color_mode: PsdColorMode,
}

#[derive(Debug, Clone)]
struct PsdChannelRef {
    id: i16,
    data_len: u64,
}

#[derive(Debug, Clone, Copy)]
struct PsdMaskInfo {
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    default_color: u8,
}

#[derive(Debug, Clone)]
struct PsdLayerRecord {
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
    channels: Vec<PsdChannelRef>,
    blend_mode: BlendMode,
    opacity: f32,
    visible: bool,
    name: String,
    divider_type: Option<u32>,
    mask_info: Option<PsdMaskInfo>,
}

impl PsdLayerRecord {
    fn width(&self) -> u32 {
        i64::from(self.right)
            .saturating_sub(i64::from(self.left))
            .clamp(0, i64::from(MAX_PSD_DIMENSION)) as u32
    }

    fn height(&self) -> u32 {
        i64::from(self.bottom)
            .saturating_sub(i64::from(self.top))
            .clamp(0, i64::from(MAX_PSD_DIMENSION)) as u32
    }
}

struct ImageResourcesMeta {
    dpi: f32,
    spot_names: Vec<String>,
}

// =============================================================================
// Public Import Entrypoints
// =============================================================================

/// Imports an Adobe Photoshop `.psd` or `.psb` file from disk via streaming I/O.
pub fn import_psd(path: &Path) -> IoResult<Document> {
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Imported PSD");
    let reader = BufReader::with_capacity(64 * 1024, file);
    import_psd_reader(reader, file_len, title)
}

/// Imports an Adobe Photoshop `.psd` or `.psb` document from an in-memory byte slice.
pub fn import_psd_bytes(bytes: &[u8], title: &str) -> IoResult<Document> {
    let clean_title = if title.trim().is_empty() {
        "Imported PSD"
    } else {
        title
    };
    import_psd_reader(Cursor::new(bytes), bytes.len() as u64, clean_title)
}

fn import_psd_reader<R: Read + Seek>(
    mut reader: R,
    file_len: u64,
    title: &str,
) -> IoResult<Document> {
    if file_len < 26 {
        return Err(IoError::InvalidFormat {
            format: "psd".into(),
            message: format!("file too short for PSD header ({file_len} bytes)"),
        });
    }

    // -------------------------------------------------------------------------
    // Section 1: File Header (26 bytes)
    // -------------------------------------------------------------------------
    let header = read_header(&mut reader)?;

    // -------------------------------------------------------------------------
    // Section 2: Color Mode Data
    // -------------------------------------------------------------------------
    let color_mode_len = u64::from(read_u32_be(&mut reader)?);
    ensure_within_file(&mut reader, color_mode_len, file_len, "color mode data")?;
    let indexed_palette = if header.color_mode == PsdColorMode::Indexed && color_mode_len == 768 {
        let mut pal = vec![0u8; 768];
        reader.read_exact(&mut pal)?;
        Some(pal)
    } else {
        skip_bytes(&mut reader, color_mode_len)?;
        None
    };

    // -------------------------------------------------------------------------
    // Section 3: Image Resources (1005 ResolutionInfo DPI + 1007/1045 Spot names)
    // -------------------------------------------------------------------------
    let img_res_len = u64::from(read_u32_be(&mut reader)?);
    ensure_within_file(&mut reader, img_res_len, file_len, "image resources")?;
    let img_res_start = reader.stream_position()?;
    let img_res_end = img_res_start
        .checked_add(img_res_len)
        .ok_or_else(|| psd_err("image resources section offset overflow"))?;
    let res_meta = parse_image_resources(&mut reader, img_res_len)?;
    reader.seek(SeekFrom::Start(img_res_end))?;

    let dpi = res_meta.dpi;
    let dpi_f64 = f64::from(dpi);
    let page_w_mm = (f64::from(header.width) * 25.4 / dpi_f64).max(1.0);
    let page_h_mm = (f64::from(header.height) * 25.4 / dpi_f64).max(1.0);

    let mut doc = Document::new(
        title,
        page_w_mm,
        page_h_mm,
        header.color_mode.doc_color_mode(),
        dpi,
    );
    doc.metadata_mut().source_format = Some(if header.is_psb { "psb" } else { "psd" }.to_owned());
    let page_id = doc.pages()[0].id;
    let default_layer_id = doc.pages()[0].layers[0].id;

    // -------------------------------------------------------------------------
    // Section 4: Layer & Mask Information
    // -------------------------------------------------------------------------
    let lm_len = read_section_len(&mut reader, header.is_psb)?;
    ensure_within_file(&mut reader, lm_len, file_len, "layer and mask info")?;
    let lm_start = reader.stream_position()?;
    let lm_end = lm_start
        .checked_add(lm_len)
        .ok_or_else(|| psd_err("layer and mask section offset overflow"))?;

    let mut budget_remaining = MAX_CUMULATIVE_RGBA_BYTES;
    let mut imported_layers = 0usize;
    let mut composite_has_alpha = false;
    let mut discovered_spot_names: Vec<String> = res_meta.spot_names;
    let bytes_per_px: usize = if header.depth == 16 { 8 } else { 4 };

    if lm_len > 0 {
        let li_len = read_section_len(&mut reader, header.is_psb)?;
        let li_start = reader.stream_position()?;
        let li_end = li_start
            .checked_add(li_len)
            .ok_or_else(|| psd_err("layer info offset overflow"))?;
        if li_end > lm_end {
            return Err(psd_err("layer info section exceeds layer & mask bounds"));
        }

        if li_len >= 2 {
            let raw_layer_count = read_i16_be(&mut reader)?;
            if raw_layer_count < 0 {
                composite_has_alpha = true;
            }
            let layer_count = usize::from(raw_layer_count.unsigned_abs());
            if layer_count > MAX_PSD_LAYERS {
                return Err(IoError::LimitExceeded {
                    limit: format!("max_psd_layers={MAX_PSD_LAYERS}"),
                    actual: format!("{layer_count} layers"),
                });
            }

            let mut records = Vec::with_capacity(layer_count);
            for idx in 0..layer_count {
                let rec = read_layer_record(&mut reader, header.is_psb, li_end, idx)?;
                records.push(rec);
            }

            // Stack of open `lsct`/`lsdk` group frames (PSD stores layers bottom-to-top:
            // divider_type == 3 opens the bottom boundary, divider_type == 1|2 closes the folder).
            let mut group_stack: Vec<Vec<ObjectId>> = Vec::new();

            for (idx, rec) in records.iter().enumerate() {
                let lw = rec.width();
                let lh = rec.height();

                let mut channel_positions = Vec::with_capacity(rec.channels.len());
                for ch in &rec.channels {
                    let pos = reader.stream_position()?;
                    let end = pos
                        .checked_add(ch.data_len)
                        .ok_or_else(|| psd_err("layer channel offset overflow"))?;
                    if end > li_end {
                        return Err(psd_err("layer channel data exceeds layer info bounds"));
                    }
                    channel_positions.push((ch.id, pos, ch.data_len));
                    reader.seek(SeekFrom::Start(end))?;
                }
                let layer_end = reader.stream_position()?;

                match rec.divider_type {
                    Some(3) => {
                        group_stack.push(Vec::new());
                        continue;
                    }
                    Some(1 | 2) => {
                        let children = group_stack.pop().unwrap_or_default();
                        let grp_name = if rec.name.trim().is_empty() {
                            format!("Group {}", idx + 1)
                        } else {
                            rec.name.clone()
                        };
                        let mut grp = SceneObject::new(
                            ObjectId(0),
                            grp_name.clone(),
                            NodePayload::Group(children),
                        );
                        grp.opacity = rec.opacity;
                        grp.blend_mode = rec.blend_mode;
                        grp.visible = rec.visible;
                        if let Some(parent_children) = group_stack.last_mut() {
                            let gid = doc.insert_loaded_object(grp)?;
                            parent_children.push(gid);
                        } else {
                            let lid =
                                doc.add_layer(page_id, grp_name, false, MasterLayerScope::None)?;
                            if let Some(layer) = doc.find_layer_mut(lid) {
                                layer.visible = rec.visible;
                            }
                            doc.add_object(grp, Some(lid))?;
                            imported_layers += 1;
                        }
                        continue;
                    }
                    _ => {}
                }

                // Track any >= 4 spot channel IDs cleanly without desyncing the stream.
                for &(ch_id, _, _) in &channel_positions {
                    if ch_id >= 4 {
                        let slot = (ch_id - 4) as usize;
                        if discovered_spot_names.get(slot).is_none() {
                            while discovered_spot_names.len() <= slot {
                                let n = discovered_spot_names.len() + 1;
                                discovered_spot_names.push(format!("Spot {n}"));
                            }
                        }
                    }
                }

                if lw == 0 || lh == 0 {
                    continue;
                }

                let Some((dst_w, dst_h)) = compute_target_dimensions(
                    lw,
                    lh,
                    MAX_LAYER_OUTPUT_DIM,
                    budget_remaining,
                    bytes_per_px,
                ) else {
                    continue;
                };

                let decoded = decode_layer_asset(
                    &mut reader,
                    &header,
                    indexed_palette.as_deref(),
                    rec,
                    lw,
                    lh,
                    dst_w,
                    dst_h,
                    dpi,
                    &channel_positions,
                )?;
                reader.seek(SeekFrom::Start(layer_end))?;

                let Some((asset, byte_cost)) = decoded else {
                    continue;
                };

                budget_remaining = budget_remaining.saturating_sub(byte_cost);
                let asset_id = doc.register_asset(asset);

                let layer_name = if rec.name.trim().is_empty() {
                    format!("Layer {}", idx + 1)
                } else {
                    rec.name.clone()
                };

                let left_mm = f64::from(rec.left) * 25.4 / dpi_f64;
                let top_mm = f64::from(rec.top) * 25.4 / dpi_f64;
                let width_mm = (f64::from(lw) * 25.4 / dpi_f64).max(0.01);
                let height_mm = (f64::from(lh) * 25.4 / dpi_f64).max(0.01);

                let mut obj = SceneObject::new(
                    ObjectId(0),
                    layer_name.clone(),
                    NodePayload::Raster(RasterLayerData {
                        asset_id,
                        width_mm,
                        height_mm,
                        filters: Vec::new(),
                        crop_rect: None,
                    }),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none())
                .with_transform(Affine2D::translate(left_mm, top_mm));
                obj.opacity = rec.opacity;
                obj.blend_mode = rec.blend_mode;
                obj.visible = rec.visible;

                if let Some(parent_children) = group_stack.last_mut() {
                    let oid = doc.insert_loaded_object(obj)?;
                    parent_children.push(oid);
                    imported_layers += 1;
                } else {
                    let lid = doc.add_layer(page_id, layer_name, false, MasterLayerScope::None)?;
                    if let Some(layer) = doc.find_layer_mut(lid) {
                        layer.visible = rec.visible;
                    }
                    doc.add_object(obj, Some(lid))?;
                    imported_layers += 1;
                }
            }

            // Flush any unclosed group frames from malformed PSDs so no objects are dropped.
            while let Some(rem) = group_stack.pop() {
                if !rem.is_empty() {
                    let grp =
                        SceneObject::new(ObjectId(0), "Group".to_owned(), NodePayload::Group(rem));
                    if let Some(parent_children) = group_stack.last_mut() {
                        let gid = doc.insert_loaded_object(grp)?;
                        parent_children.push(gid);
                    } else {
                        let lid = doc.add_layer(
                            page_id,
                            "Group".to_owned(),
                            false,
                            MasterLayerScope::None,
                        )?;
                        doc.add_object(grp, Some(lid))?;
                        imported_layers += 1;
                    }
                }
            }
        }
    }

    // Seek to Section 5 (Composite Image Data).
    reader.seek(SeekFrom::Start(lm_end))?;

    // -------------------------------------------------------------------------
    // Section 5: Composite Image Data fallback when flattened / no raster layers
    // -------------------------------------------------------------------------
    if imported_layers == 0 && lm_end + 2 <= file_len {
        if let Some((dst_w, dst_h)) = compute_target_dimensions(
            header.width,
            header.height,
            MAX_LAYER_OUTPUT_DIM,
            budget_remaining,
            bytes_per_px,
        ) {
            if let Some(asset) = decode_composite_asset(
                &mut reader,
                file_len,
                &header,
                indexed_palette.as_deref(),
                composite_has_alpha,
                dst_w,
                dst_h,
                dpi,
            )? {
                let asset_id = doc.register_asset(asset);

                let lid = doc.add_layer(page_id, "Composite", false, MasterLayerScope::None)?;
                let obj = SceneObject::new(
                    ObjectId(0),
                    "Composite",
                    NodePayload::Raster(RasterLayerData {
                        asset_id,
                        width_mm: page_w_mm,
                        height_mm: page_h_mm,
                        filters: Vec::new(),
                        crop_rect: None,
                    }),
                )
                .with_fill(Fill::None)
                .with_outline(OutlineStyle::none());
                doc.add_object(obj, Some(lid))?;
                imported_layers += 1;
            }
        }
    }

    if imported_layers > 0 && doc.pages()[0].layers.len() > 1 {
        if let Some(pos) = doc.pages()[0]
            .layers
            .iter()
            .position(|l| l.id == default_layer_id && l.objects.is_empty())
        {
            doc.remove_page_layer(0, pos);
        }
    }

    doc.refresh_document_palette();
    for sname in discovered_spot_names {
        let spot_col = if sname.eq_ignore_ascii_case("CutContour") {
            Color::cut_contour_spot()
        } else {
            Color::spot(sname.clone(), 1.0, [0.0, 1.0, 0.0, 0.0])
        };
        doc.document_palette_mut()
            .add_swatch(ColorSwatch::new(sname, spot_col));
    }
    Ok(doc)
}

// =============================================================================
// Public Export Entrypoints (PSD v1 / PSB v2)
// =============================================================================

/// Exports `page_index` of `doc` as an Adobe Photoshop `.psd` byte vector.
pub fn export_psd_to_bytes(doc: &Document, page_index: usize) -> IoResult<Vec<u8>> {
    export_psd_to_bytes_with_options(doc, page_index, &PsdExportOptions::default())
}

/// Exports `page_index` of `doc` as `.psd` or `.psb` bytes with explicit options.
pub fn export_psd_to_bytes_with_options(
    doc: &Document,
    page_index: usize,
    opts: &PsdExportOptions,
) -> IoResult<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::with_capacity(4096));
    export_psd_to_writer_with_options(doc, page_index, opts, &mut cursor)?;
    Ok(cursor.into_inner())
}

fn export_psd_to_writer_with_options<W: Write + Seek>(
    doc: &Document,
    page_index: usize,
    opts: &PsdExportOptions,
    writer: &mut W,
) -> IoResult<()> {
    let page = doc.page(page_index).ok_or(IoError::PageOutOfRange {
        page_index,
        page_count: doc.pages().len(),
    })?;
    let dpi = opts
        .dpi
        .filter(|d| d.is_finite() && *d >= 1.0)
        .unwrap_or_else(|| doc.dpi().max(72.0));
    let dpi_f64 = f64::from(dpi);
    let page_w_mm = if page.width_mm.is_finite() && page.width_mm > 0.0 {
        page.width_mm
    } else {
        210.0
    };
    let page_h_mm = if page.height_mm.is_finite() && page.height_mm > 0.0 {
        page.height_mm
    } else {
        297.0
    };
    let canvas_w = ((page_w_mm * dpi_f64 / 25.4).round() as u32).clamp(1, MAX_LAYER_OUTPUT_DIM);
    let canvas_h = ((page_h_mm * dpi_f64 / 25.4).round() as u32).clamp(1, MAX_LAYER_OUTPUT_DIM);
    let is_psb = opts.is_psb || canvas_w > 30_000 || canvas_h > 30_000;
    let use_cmyk = doc.color_mode() == ColorMode::Cmyk;
    let color_mode_code: u16 = if use_cmyk { 4 } else { 3 };
    let color_channels: u16 = if use_cmyk { 4 } else { 3 };

    // Determine whether page contains any 16-bit raster asset -> emit native 16-bit PSD!
    let mut use_16bit = false;
    for layer in &page.layers {
        for &oid in &layer.objects {
            if object_uses_16bit_asset(doc, oid, 0) {
                use_16bit = true;
                break;
            }
        }
        if use_16bit {
            break;
        }
    }
    let depth: u16 = if use_16bit { 16 } else { 8 };

    enum ExportLayerSource {
        Empty,
        Asset(coree_core::ImageAssetId),
        Rendered {
            oid: ObjectId,
            cached_rgba: Option<Vec<u8>>,
            width: u32,
            height: u32,
        },
    }

    struct ExportLayerMeta {
        name: String,
        top: i32,
        left: i32,
        bottom: i32,
        right: i32,
        opacity: u8,
        visible: bool,
        blend: [u8; 4],
        divider_type: Option<u32>,
        ch_ids: Vec<i16>,
        ch_len_offsets: Vec<u64>,
        ch_written_lens: Vec<u64>,
        source: ExportLayerSource,
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_psd_obj_meta(
        doc: &Document,
        page_index: usize,
        oid: ObjectId,
        dpi: f32,
        use_cmyk: bool,
        out: &mut Vec<ExportLayerMeta>,
        depth_rec: usize,
    ) {
        if depth_rec > 32 {
            return;
        }
        let Some(obj) = doc.object(oid) else {
            return;
        };
        let blend = blend_mode_to_psd_key(obj.blend_mode);
        let opacity = (obj.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        match &obj.payload {
            NodePayload::Group(children) => {
                // Bottom-to-top: bounding section divider (3) first, then children, then folder (1)
                let empty_ch = vec![0, 1, 2, -1];
                out.push(ExportLayerMeta {
                    name: "</Layer group>".to_owned(),
                    top: 0,
                    left: 0,
                    bottom: 0,
                    right: 0,
                    opacity: 255,
                    visible: true,
                    blend: *b"norm",
                    divider_type: Some(3),
                    ch_ids: empty_ch.clone(),
                    ch_len_offsets: Vec::with_capacity(empty_ch.len()),
                    ch_written_lens: Vec::with_capacity(empty_ch.len()),
                    source: ExportLayerSource::Empty,
                });
                for &cid in children {
                    collect_psd_obj_meta(doc, page_index, cid, dpi, use_cmyk, out, depth_rec + 1);
                }
                out.push(ExportLayerMeta {
                    name: obj.name.clone(),
                    top: 0,
                    left: 0,
                    bottom: 0,
                    right: 0,
                    opacity,
                    visible: obj.visible,
                    blend,
                    divider_type: Some(1),
                    ch_ids: empty_ch.clone(),
                    ch_len_offsets: Vec::with_capacity(empty_ch.len()),
                    ch_written_lens: Vec::with_capacity(empty_ch.len()),
                    source: ExportLayerSource::Empty,
                });
            }
            NodePayload::Raster(r) if obj.effects.is_empty() && obj.perspective.is_none() => {
                if let Some(asset) = doc.asset(r.asset_id) {
                    let tx = obj.transform.m[4];
                    let ty = obj.transform.m[5];
                    let left = (tx * f64::from(dpi) / 25.4).round() as i32;
                    let top = (ty * f64::from(dpi) / 25.4).round() as i32;
                    let lw = asset.width;
                    let lh = asset.height;
                    let right = left.saturating_add(lw as i32);
                    let bottom = top.saturating_add(lh as i32);
                    let ch_ids = if use_cmyk {
                        vec![0, 1, 2, 3, -1]
                    } else {
                        vec![0, 1, 2, -1]
                    };
                    out.push(ExportLayerMeta {
                        name: obj.name.clone(),
                        top,
                        left,
                        bottom,
                        right,
                        opacity,
                        visible: obj.visible,
                        blend,
                        divider_type: None,
                        ch_len_offsets: Vec::with_capacity(ch_ids.len()),
                        ch_written_lens: Vec::with_capacity(ch_ids.len()),
                        ch_ids,
                        source: ExportLayerSource::Asset(r.asset_id),
                    });
                }
            }
            _ => {
                if let Ok(Some((rect_mm, buf))) = render_isolated_object_to_rgba(
                    doc,
                    page_index,
                    oid,
                    Affine2D::IDENTITY,
                    f64::from(dpi),
                ) {
                    let left = (rect_mm.min_x * f64::from(dpi) / 25.4).round() as i32;
                    let top = (rect_mm.min_y * f64::from(dpi) / 25.4).round() as i32;
                    let width = buf.width;
                    let height = buf.height;
                    let right = left.saturating_add(width as i32);
                    let bottom = top.saturating_add(height as i32);
                    let ch_ids = if use_cmyk {
                        vec![0, 1, 2, 3, -1]
                    } else {
                        vec![0, 1, 2, -1]
                    };
                    // Cache small renders (<= 16 MiB) so we do not render twice; drop larger ones
                    // and re-render one at a time while streaming channels.
                    let cached_rgba = if buf.rgba.len() <= 16 * 1024 * 1024 {
                        Some(buf.rgba)
                    } else {
                        None
                    };
                    out.push(ExportLayerMeta {
                        name: obj.name.clone(),
                        top,
                        left,
                        bottom,
                        right,
                        opacity,
                        visible: obj.visible,
                        blend,
                        divider_type: None,
                        ch_len_offsets: Vec::with_capacity(ch_ids.len()),
                        ch_written_lens: Vec::with_capacity(ch_ids.len()),
                        ch_ids,
                        source: ExportLayerSource::Rendered {
                            oid,
                            cached_rgba,
                            width,
                            height,
                        },
                    });
                }
            }
        }
    }

    let mut layer_metas: Vec<ExportLayerMeta> = Vec::new();
    for layer in &page.layers {
        for &oid in &layer.objects {
            collect_psd_obj_meta(doc, page_index, oid, dpi, use_cmyk, &mut layer_metas, 0);
        }
    }

    // Collect any Spot colors in the document to emit in Image Resource 1007.
    let mut spot_names: Vec<String> = Vec::new();
    for sw in &doc.document_palette().swatches {
        if let Color::Spot { name, .. } = &sw.color {
            if !spot_names.contains(name) {
                spot_names.push(name.clone());
            }
        }
    }

    // 1. Header
    writer.write_all(b"8BPS")?;
    writer.write_all(&(if is_psb { 2u16 } else { 1u16 }).to_be_bytes())?;
    writer.write_all(&[0u8; 6])?;
    writer.write_all(&(color_channels + 1).to_be_bytes())?; // color + alpha
    writer.write_all(&canvas_h.to_be_bytes())?;
    writer.write_all(&canvas_w.to_be_bytes())?;
    writer.write_all(&depth.to_be_bytes())?;
    writer.write_all(&color_mode_code.to_be_bytes())?;

    // 2. Color Mode Data (empty)
    writer.write_all(&0u32.to_be_bytes())?;

    // 3. Image Resources (1005 DPI + optional 1007 spot channel names)
    let mut ir = Vec::with_capacity(128);
    ir.extend_from_slice(b"8BIM");
    ir.extend_from_slice(&1005u16.to_be_bytes());
    ir.extend_from_slice(&[0u8, 0u8]);
    ir.extend_from_slice(&16u32.to_be_bytes());
    let fixed = ((f64::from(dpi) * 65_536.0).round() as u32).to_be_bytes();
    ir.extend_from_slice(&fixed);
    ir.extend_from_slice(&1u16.to_be_bytes());
    ir.extend_from_slice(&1u16.to_be_bytes());
    ir.extend_from_slice(&fixed);
    ir.extend_from_slice(&1u16.to_be_bytes());
    ir.extend_from_slice(&1u16.to_be_bytes());

    if !spot_names.is_empty() {
        let mut r1007 = Vec::new();
        for sname in &spot_names {
            let b = sname.as_bytes();
            let len = b.len().min(255) as u8;
            r1007.push(len);
            r1007.extend_from_slice(&b[..usize::from(len)]);
        }
        ir.extend_from_slice(b"8BIM");
        ir.extend_from_slice(&1007u16.to_be_bytes());
        ir.extend_from_slice(&[0u8, 0u8]);
        ir.extend_from_slice(&(r1007.len() as u32).to_be_bytes());
        ir.extend_from_slice(&r1007);
        if r1007.len() % 2 != 0 {
            ir.push(0);
        }
    }
    writer.write_all(&(ir.len() as u32).to_be_bytes())?;
    writer.write_all(&ir)?;

    // 4. Layer & Mask Info (streamed directly with length backpatching)
    let lm_len_pos = writer.stream_position()?;
    write_section_len_placeholder(writer, is_psb)?;
    let lm_start = writer.stream_position()?;

    if !layer_metas.is_empty() {
        let li_len_pos = writer.stream_position()?;
        write_section_len_placeholder(writer, is_psb)?;
        let li_start = writer.stream_position()?;

        writer.write_all(&(-(layer_metas.len() as i16)).to_be_bytes())?;
        for layer in &mut layer_metas {
            writer.write_all(&layer.top.to_be_bytes())?;
            writer.write_all(&layer.left.to_be_bytes())?;
            writer.write_all(&layer.bottom.to_be_bytes())?;
            writer.write_all(&layer.right.to_be_bytes())?;
            writer.write_all(&(layer.ch_ids.len() as u16).to_be_bytes())?;
            for &cid in &layer.ch_ids {
                writer.write_all(&cid.to_be_bytes())?;
                layer.ch_len_offsets.push(writer.stream_position()?);
                write_section_len_placeholder(writer, is_psb)?;
            }
            writer.write_all(b"8BIM")?;
            writer.write_all(&layer.blend)?;
            writer.write_all(&[layer.opacity, 0, if layer.visible { 0 } else { 2 }, 0])?;

            let mut extra = Vec::new();
            extra.extend_from_slice(&0u32.to_be_bytes()); // mask len
            extra.extend_from_slice(&0u32.to_be_bytes()); // blend ranges len
            let pbytes = layer.name.as_bytes();
            let plen = pbytes.len().min(255);
            let total_pascal = (1 + plen + 3) & !3;
            extra.push(plen as u8);
            extra.extend_from_slice(&pbytes[..plen]);
            extra.resize(extra.len() + (total_pascal - 1 - plen), 0);

            // `luni` Unicode name
            let units: Vec<u16> = layer.name.encode_utf16().collect();
            let luni_len = 4 + units.len() * 2;
            extra.extend_from_slice(b"8BIMluni");
            extra.extend_from_slice(&(luni_len as u32).to_be_bytes());
            extra.extend_from_slice(&(units.len() as u32).to_be_bytes());
            for u in units {
                extra.extend_from_slice(&u.to_be_bytes());
            }
            if !luni_len.is_multiple_of(2) {
                extra.push(0);
            }

            if let Some(div) = layer.divider_type {
                let lsct: &[u8; 4] = b"lsct";
                extra.extend_from_slice(b"8BIM");
                extra.extend_from_slice(lsct);
                extra.extend_from_slice(&4u32.to_be_bytes());
                extra.extend_from_slice(&div.to_be_bytes());
            }

            writer.write_all(&(extra.len() as u32).to_be_bytes())?;
            writer.write_all(&extra)?;
        }

        // Stream each layer's channels one channel at a time directly to `writer`
        for layer in &mut layer_metas {
            match &mut layer.source {
                ExportLayerSource::Empty => {
                    for _ in 0..layer.ch_ids.len() {
                        writer.write_all(&0u16.to_be_bytes())?;
                        layer.ch_written_lens.push(2);
                    }
                }
                ExportLayerSource::Asset(aid) => {
                    if let Some(asset) = doc.asset(*aid) {
                        for &cid in &layer.ch_ids {
                            let ch_start = writer.stream_position()?;
                            write_asset_channel_rle(writer, asset, cid, depth, use_cmyk, is_psb)?;
                            let ch_end = writer.stream_position()?;
                            layer.ch_written_lens.push(ch_end - ch_start);
                        }
                    }
                }
                ExportLayerSource::Rendered {
                    oid,
                    cached_rgba,
                    width,
                    height,
                } => {
                    let rgba = if let Some(buf) = cached_rgba.take() {
                        buf
                    } else if let Ok(Some((_, buf))) = render_isolated_object_to_rgba(
                        doc,
                        page_index,
                        *oid,
                        Affine2D::IDENTITY,
                        f64::from(dpi),
                    ) {
                        buf.rgba
                    } else {
                        Vec::new()
                    };
                    for &cid in &layer.ch_ids {
                        let ch_start = writer.stream_position()?;
                        write_rgba8_slice_channel_rle(
                            writer, &rgba, *width, *height, cid, depth, use_cmyk, is_psb,
                        )?;
                        let ch_end = writer.stream_position()?;
                        layer.ch_written_lens.push(ch_end - ch_start);
                    }
                }
            }
        }

        let li_pos = writer.stream_position()?;
        if (li_pos - li_start) % 2 != 0 {
            writer.write_all(&[0u8])?;
        }
        let li_end = writer.stream_position()?;
        let li_len = li_end - li_start;

        // Global layer mask info (empty)
        write_section_len_placeholder(writer, is_psb)?;
        let lm_end = writer.stream_position()?;
        let lm_len = lm_end - lm_start;

        // Backpatch layer/channel lengths and section lengths
        write_section_len_at(writer, lm_len_pos, lm_len, is_psb)?;
        write_section_len_at(writer, li_len_pos, li_len, is_psb)?;
        for layer in &layer_metas {
            for (&off, &len) in layer
                .ch_len_offsets
                .iter()
                .zip(layer.ch_written_lens.iter())
            {
                write_section_len_at(writer, off, len, is_psb)?;
            }
        }
        writer.seek(SeekFrom::Start(lm_end))?;
    }

    // 5. Section 5 Composite Image Data (streamed PackBits RLE)
    if let Ok(comp_buf) = render_page_to_rgba(doc, page_index, f64::from(dpi), ViewMode::Enhanced) {
        if comp_buf.width > 0
            && comp_buf.height > 0
            && comp_buf.rgba.len() >= (comp_buf.width as usize) * (comp_buf.height as usize) * 4
        {
            write_composite_rle(
                writer,
                &comp_buf.rgba,
                comp_buf.width,
                comp_buf.height,
                canvas_w,
                canvas_h,
                depth,
                use_cmyk,
                is_psb,
            )?;
            return Ok(());
        }
    }

    writer.write_all(&0u16.to_be_bytes())?;
    let mut remaining = (canvas_w as usize)
        * (canvas_h as usize)
        * usize::from(color_channels + 1)
        * (usize::from(depth) / 8);
    let fill_chunk = [255u8; 8192];
    while remaining > 0 {
        let take = remaining.min(fill_chunk.len());
        writer.write_all(&fill_chunk[..take])?;
        remaining -= take;
    }
    Ok(())
}

/// Atomically exports `page_index` of `doc` to a `.psd` file at `path`.
pub fn export_psd(doc: &Document, page_index: usize, path: &Path) -> IoResult<()> {
    let is_psb = path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("psb"));
    let opts = PsdExportOptions {
        is_psb,
        ..PsdExportOptions::default()
    };
    atomic_write_with(path, |file| {
        let mut bw = BufWriter::with_capacity(64 * 1024, file);
        export_psd_to_writer_with_options(doc, page_index, &opts, &mut bw)?;
        bw.flush()?;
        Ok(())
    })
}

fn write_section_len_placeholder<W: Write>(writer: &mut W, is_psb: bool) -> IoResult<()> {
    if is_psb {
        writer.write_all(&0u64.to_be_bytes())?;
    } else {
        writer.write_all(&0u32.to_be_bytes())?;
    }
    Ok(())
}

fn write_section_len_at<W: Write + Seek>(
    writer: &mut W,
    offset: u64,
    len: u64,
    is_psb: bool,
) -> IoResult<()> {
    writer.seek(SeekFrom::Start(offset))?;
    if is_psb {
        writer.write_all(&len.to_be_bytes())?;
    } else {
        writer.write_all(&(len.min(u64::from(u32::MAX)) as u32).to_be_bytes())?;
    }
    Ok(())
}

fn write_zero_placeholder<W: Write>(writer: &mut W, byte_len: usize) -> IoResult<()> {
    let zeros = [0u8; 4096];
    let mut left = byte_len;
    while left > 0 {
        let n = left.min(zeros.len());
        writer.write_all(&zeros[..n])?;
        left -= n;
    }
    Ok(())
}

fn write_row_counts_at<W: Write + Seek>(
    writer: &mut W,
    counts_pos: u64,
    counts: &[usize],
    is_psb: bool,
) -> IoResult<()> {
    let end_pos = writer.stream_position()?;
    writer.seek(SeekFrom::Start(counts_pos))?;
    for &cnt in counts {
        if is_psb {
            writer.write_all(&(cnt as u32).to_be_bytes())?;
        } else {
            writer.write_all(&(cnt.min(usize::from(u16::MAX)) as u16).to_be_bytes())?;
        }
    }
    writer.seek(SeekFrom::Start(end_pos))?;
    Ok(())
}

fn object_uses_16bit_asset(doc: &Document, oid: ObjectId, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    let Some(obj) = doc.object(oid) else {
        return false;
    };
    match &obj.payload {
        NodePayload::Raster(r) => doc
            .asset(r.asset_id)
            .is_some_and(|a| a.pixel_depth == PixelDepth::Bit16),
        NodePayload::Group(c) => c
            .iter()
            .any(|&cid| object_uses_16bit_asset(doc, cid, depth + 1)),
        _ => false,
    }
}

fn blend_mode_to_psd_key(bm: BlendMode) -> [u8; 4] {
    match bm {
        BlendMode::Normal => *b"norm",
        BlendMode::Multiply => *b"mul ",
        BlendMode::Screen => *b"scrn",
        BlendMode::Overlay => *b"over",
        BlendMode::Darken => *b"dark",
        BlendMode::Lighten => *b"lite",
        BlendMode::Difference => *b"diff",
        BlendMode::Exclusion => *b"smud",
        BlendMode::SoftLight => *b"sLit",
        BlendMode::HardLight => *b"hLit",
        BlendMode::ColorDodge => *b"div ",
        BlendMode::ColorBurn => *b"idiv",
        BlendMode::Hue => *b"hue ",
        BlendMode::Saturation => *b"sat ",
        BlendMode::Color => *b"colr",
        BlendMode::Luminosity => *b"lum ",
    }
}

#[allow(clippy::too_many_arguments)]
fn write_asset_channel_rle<W: Write + Seek>(
    writer: &mut W,
    asset: &RasterAsset,
    cid: i16,
    depth: u16,
    use_cmyk: bool,
    is_psb: bool,
) -> IoResult<()> {
    let w = asset.width as usize;
    let h = asset.height as usize;
    if w == 0 || h == 0 || !asset.has_valid_layout() {
        writer.write_all(&0u16.to_be_bytes())?;
        return Ok(());
    }

    writer.write_all(&1u16.to_be_bytes())?;
    let counts_pos = writer.stream_position()?;
    let count_stride = if is_psb { 4 } else { 2 };
    write_zero_placeholder(writer, h * count_stride)?;

    let row_bytes = w * if depth == 16 { 2 } else { 1 };
    let mut row_raw = vec![0u8; row_bytes];
    let mut row_enc = Vec::with_capacity(row_bytes + 64);
    let mut counts: Vec<usize> = Vec::with_capacity(h);
    let bpp = asset.pixel_depth.bytes_per_pixel();

    for y in 0..h {
        let ty = (y as u32) / RASTER_TILE_DIM;
        let dy = y % (RASTER_TILE_DIM as usize);
        for tx in 0..asset.tiles_x {
            let tile = &asset.tiles[(ty * asset.tiles_x + tx) as usize];
            let tw = tile.width as usize;
            let x0 = (tx as usize) * (RASTER_TILE_DIM as usize);
            let tdata = tile.as_slice();
            let row_off = dy * tw * bpp;

            match (asset.pixel_depth, depth) {
                (PixelDepth::Bit16, 16) => {
                    for lx in 0..tw {
                        let x = x0 + lx;
                        let px_off = row_off + lx * 8;
                        let r16 = u16::from_le_bytes([tdata[px_off], tdata[px_off + 1]]);
                        let g16 = u16::from_le_bytes([tdata[px_off + 2], tdata[px_off + 3]]);
                        let b16 = u16::from_le_bytes([tdata[px_off + 4], tdata[px_off + 5]]);
                        let a16 = u16::from_le_bytes([tdata[px_off + 6], tdata[px_off + 7]]);
                        let val16 = if use_cmyk {
                            match cid {
                                0..=3 => {
                                    let cmyk = Color::rgba(
                                        f32::from(r16) / 65535.0,
                                        f32::from(g16) / 65535.0,
                                        f32::from(b16) / 65535.0,
                                        1.0,
                                    )
                                    .to_cmyk();
                                    ((1.0 - cmyk[cid as usize]).clamp(0.0, 1.0) * 65535.0).round()
                                        as u16
                                }
                                _ => a16,
                            }
                        } else {
                            match cid {
                                0 => r16,
                                1 => g16,
                                2 => b16,
                                _ => a16,
                            }
                        };
                        let be = val16.to_be_bytes();
                        row_raw[x * 2] = be[0];
                        row_raw[x * 2 + 1] = be[1];
                    }
                }
                (PixelDepth::Bit8, 16) => {
                    for lx in 0..tw {
                        let x = x0 + lx;
                        let px_off = row_off + lx * 4;
                        let r16 = u16::from(tdata[px_off]) * 257;
                        let g16 = u16::from(tdata[px_off + 1]) * 257;
                        let b16 = u16::from(tdata[px_off + 2]) * 257;
                        let a16 = u16::from(tdata[px_off + 3]) * 257;
                        let val16 = if use_cmyk {
                            match cid {
                                0..=3 => {
                                    let cmyk = Color::rgba(
                                        f32::from(r16) / 65535.0,
                                        f32::from(g16) / 65535.0,
                                        f32::from(b16) / 65535.0,
                                        1.0,
                                    )
                                    .to_cmyk();
                                    ((1.0 - cmyk[cid as usize]).clamp(0.0, 1.0) * 65535.0).round()
                                        as u16
                                }
                                _ => a16,
                            }
                        } else {
                            match cid {
                                0 => r16,
                                1 => g16,
                                2 => b16,
                                _ => a16,
                            }
                        };
                        let be = val16.to_be_bytes();
                        row_raw[x * 2] = be[0];
                        row_raw[x * 2 + 1] = be[1];
                    }
                }
                (PixelDepth::Bit16, _) => {
                    for lx in 0..tw {
                        let x = x0 + lx;
                        let px_off = row_off + lx * 8;
                        let to8 = |off: usize| -> u8 {
                            let v = u16::from_le_bytes([tdata[off], tdata[off + 1]]);
                            ((u32::from(v) * 255 + 32_767) / 65_535) as u8
                        };
                        let r8 = to8(px_off);
                        let g8 = to8(px_off + 2);
                        let b8 = to8(px_off + 4);
                        let a8 = to8(px_off + 6);
                        row_raw[x] = if use_cmyk {
                            match cid {
                                0..=3 => {
                                    let cmyk = Color::rgba8(r8, g8, b8, 255).to_cmyk();
                                    ((1.0 - cmyk[cid as usize]).clamp(0.0, 1.0) * 255.0).round()
                                        as u8
                                }
                                _ => a8,
                            }
                        } else {
                            match cid {
                                0 => r8,
                                1 => g8,
                                2 => b8,
                                _ => a8,
                            }
                        };
                    }
                }
                (PixelDepth::Bit8, _) => {
                    for lx in 0..tw {
                        let x = x0 + lx;
                        let px_off = row_off + lx * 4;
                        let r8 = tdata[px_off];
                        let g8 = tdata[px_off + 1];
                        let b8 = tdata[px_off + 2];
                        let a8 = tdata[px_off + 3];
                        row_raw[x] = if use_cmyk {
                            match cid {
                                0..=3 => {
                                    let cmyk = Color::rgba8(r8, g8, b8, 255).to_cmyk();
                                    ((1.0 - cmyk[cid as usize]).clamp(0.0, 1.0) * 255.0).round()
                                        as u8
                                }
                                _ => a8,
                            }
                        } else {
                            match cid {
                                0 => r8,
                                1 => g8,
                                2 => b8,
                                _ => a8,
                            }
                        };
                    }
                }
            }
        }
        row_enc.clear();
        encode_packbits_row_into(&row_raw, &mut row_enc);
        writer.write_all(&row_enc)?;
        counts.push(row_enc.len());
    }

    write_row_counts_at(writer, counts_pos, &counts, is_psb)
}

#[allow(clippy::too_many_arguments)]
fn write_rgba8_slice_channel_rle<W: Write + Seek>(
    writer: &mut W,
    rgba8: &[u8],
    width: u32,
    height: u32,
    cid: i16,
    depth: u16,
    use_cmyk: bool,
    is_psb: bool,
) -> IoResult<()> {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 || rgba8.len() < w * h * 4 {
        writer.write_all(&0u16.to_be_bytes())?;
        return Ok(());
    }

    writer.write_all(&1u16.to_be_bytes())?;
    let counts_pos = writer.stream_position()?;
    let count_stride = if is_psb { 4 } else { 2 };
    write_zero_placeholder(writer, h * count_stride)?;

    let row_bytes = w * if depth == 16 { 2 } else { 1 };
    let mut row_raw = vec![0u8; row_bytes];
    let mut row_enc = Vec::with_capacity(row_bytes + 64);
    let mut counts: Vec<usize> = Vec::with_capacity(h);

    for y in 0..h {
        for x in 0..w {
            let base = (y * w + x) * 4;
            let v8 = if use_cmyk {
                match cid {
                    0..=3 => {
                        let cmyk = Color::rgba8(rgba8[base], rgba8[base + 1], rgba8[base + 2], 255)
                            .to_cmyk();
                        ((1.0 - cmyk[cid as usize]).clamp(0.0, 1.0) * 255.0).round() as u8
                    }
                    _ => rgba8[base + 3],
                }
            } else {
                match cid {
                    0 => rgba8[base],
                    1 => rgba8[base + 1],
                    2 => rgba8[base + 2],
                    _ => rgba8[base + 3],
                }
            };
            if depth == 16 {
                let v16 = u16::from(v8) * 257;
                let be = v16.to_be_bytes();
                row_raw[x * 2] = be[0];
                row_raw[x * 2 + 1] = be[1];
            } else {
                row_raw[x] = v8;
            }
        }
        row_enc.clear();
        encode_packbits_row_into(&row_raw, &mut row_enc);
        writer.write_all(&row_enc)?;
        counts.push(row_enc.len());
    }

    write_row_counts_at(writer, counts_pos, &counts, is_psb)
}

#[allow(clippy::too_many_arguments)]
fn write_composite_rle<W: Write + Seek>(
    writer: &mut W,
    rgba8: &[u8],
    src_w_u32: u32,
    src_h_u32: u32,
    dst_w: u32,
    dst_h: u32,
    depth: u16,
    use_cmyk: bool,
    is_psb: bool,
) -> IoResult<()> {
    let w = dst_w as usize;
    let h = dst_h as usize;
    let src_w = src_w_u32 as usize;
    let src_h = src_h_u32 as usize;
    let ch_count = if use_cmyk { 5usize } else { 4usize };
    let row_bytes = w * if depth == 16 { 2 } else { 1 };
    let mut row_raw = vec![0u8; row_bytes];
    let mut row_enc = Vec::with_capacity(row_bytes + 64);
    let mut counts: Vec<usize> = Vec::with_capacity(ch_count * h);

    writer.write_all(&1u16.to_be_bytes())?;
    let counts_pos = writer.stream_position()?;
    let count_stride = if is_psb { 4 } else { 2 };
    write_zero_placeholder(writer, ch_count * h * count_stride)?;

    for ch in 0..ch_count {
        for y in 0..h {
            let sy = (y * src_h / h.max(1)).min(src_h.saturating_sub(1));
            for x in 0..w {
                let sx = (x * src_w / w.max(1)).min(src_w.saturating_sub(1));
                let base = (sy * src_w + sx) * 4;
                let v8 = if use_cmyk {
                    if ch < 4 {
                        let cmyk = Color::rgba8(rgba8[base], rgba8[base + 1], rgba8[base + 2], 255)
                            .to_cmyk();
                        ((1.0 - cmyk[ch]).clamp(0.0, 1.0) * 255.0).round() as u8
                    } else {
                        rgba8[base + 3]
                    }
                } else {
                    rgba8[base + ch]
                };
                if depth == 16 {
                    let v16 = u16::from(v8) * 257;
                    let be = v16.to_be_bytes();
                    row_raw[x * 2] = be[0];
                    row_raw[x * 2 + 1] = be[1];
                } else {
                    row_raw[x] = v8;
                }
            }
            row_enc.clear();
            encode_packbits_row_into(&row_raw, &mut row_enc);
            writer.write_all(&row_enc)?;
            counts.push(row_enc.len());
        }
    }

    write_row_counts_at(writer, counts_pos, &counts, is_psb)
}

fn encode_packbits_row_into(row: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < row.len() {
        let mut run = 1;
        while i + run < row.len() && run < 128 && row[i + run] == row[i] {
            run += 1;
        }
        if run >= 3 {
            out.push(((1isize - (run as isize)) as i8) as u8);
            out.push(row[i]);
            i += run;
        } else {
            let start = i;
            let mut lit = 0;
            while i < row.len() && lit < 128 {
                if i + 2 < row.len() && row[i] == row[i + 1] && row[i] == row[i + 2] {
                    break;
                }
                i += 1;
                lit += 1;
            }
            out.push((lit - 1) as u8);
            out.extend_from_slice(&row[start..start + lit]);
        }
    }
}

// =============================================================================
// Section Parsers & Streaming Decoders
// =============================================================================

fn read_header<R: Read>(reader: &mut R) -> IoResult<PsdHeader> {
    let mut buf = [0u8; 26];
    reader.read_exact(&mut buf)?;
    if &buf[0..4] != b"8BPS" {
        return Err(psd_err("invalid PSD signature (expected '8BPS')"));
    }
    let version = u16::from_be_bytes([buf[4], buf[5]]);
    let is_psb = match version {
        1 => false,
        2 => true,
        other => {
            return Err(psd_err(format!("unsupported PSD version {other}")));
        }
    };
    let channels = u16::from_be_bytes([buf[12], buf[13]]);
    let height = u32::from_be_bytes([buf[14], buf[15], buf[16], buf[17]]);
    let width = u32::from_be_bytes([buf[18], buf[19], buf[20], buf[21]]);
    let depth = u16::from_be_bytes([buf[22], buf[23]]);
    let color_mode_raw = u16::from_be_bytes([buf[24], buf[25]]);

    if channels == 0 || channels > 56 {
        return Err(psd_err(format!("invalid channel count {channels}")));
    }
    if width == 0 || height == 0 {
        return Err(psd_err(format!(
            "invalid zero canvas dimensions ({width}x{height})"
        )));
    }
    let dim_cap = if is_psb { MAX_PSD_DIMENSION } else { 30_000 };
    if width > dim_cap || height > dim_cap {
        return Err(IoError::LimitExceeded {
            limit: format!("max_psd_dimension={dim_cap}"),
            actual: format!("{width}x{height}"),
        });
    }
    if !matches!(depth, 1 | 8 | 16) {
        return Err(psd_err(format!(
            "unsupported bit depth {depth} (supported: 1, 8, 16)"
        )));
    }
    let color_mode = PsdColorMode::from_u16(color_mode_raw)?;
    if depth == 1 && color_mode != PsdColorMode::Bitmap {
        return Err(psd_err("1-bit depth is only valid for Bitmap color mode"));
    }

    Ok(PsdHeader {
        is_psb,
        channels,
        height,
        width,
        depth,
        color_mode,
    })
}

fn parse_image_resources<R: Read + Seek>(
    reader: &mut R,
    section_len: u64,
) -> IoResult<ImageResourcesMeta> {
    let start = reader.stream_position()?;
    let end = start.saturating_add(section_len);
    let scan_limit = start.saturating_add(section_len.min(MAX_IMAGE_RESOURCES_SCAN));
    let mut dpi = 72.0_f32;
    let mut pascal_names: Vec<String> = Vec::new();
    let mut unicode_names: Vec<String> = Vec::new();

    while reader.stream_position()? + 12 <= scan_limit {
        let mut sig = [0u8; 4];
        if reader.read_exact(&mut sig).is_err() {
            break;
        }
        if !matches!(&sig, b"8BIM" | b"MeSa" | b"PHUT" | b"AgHg" | b"DCSR") {
            break;
        }
        let res_id = read_u16_be(reader)?;
        let name_len = read_u8(reader)?;
        let name_pad = if name_len % 2 == 0 { 1u64 } else { 0u64 };
        let name_skip = u64::from(name_len) + name_pad;
        if reader.stream_position()? + name_skip + 4 > end {
            break;
        }
        skip_bytes(reader, name_skip)?;
        let data_len = u64::from(read_u32_be(reader)?);
        let padded_len = (data_len + 1) & !1;
        let data_start = reader.stream_position()?;
        if data_start.saturating_add(data_len) > end {
            break;
        }

        if res_id == 1005 && data_len >= 16 {
            let mut info = [0u8; 16];
            reader.read_exact(&mut info)?;
            let h_res_fixed = u32::from_be_bytes([info[0], info[1], info[2], info[3]]);
            let v_res_fixed = u32::from_be_bytes([info[8], info[9], info[10], info[11]]);
            let h_dpi = f64::from(h_res_fixed) / 65_536.0;
            let v_dpi = f64::from(v_res_fixed) / 65_536.0;
            if h_dpi.is_finite() && (1.0..=100_000.0).contains(&h_dpi) {
                dpi = h_dpi as f32;
            } else if v_dpi.is_finite() && (1.0..=100_000.0).contains(&v_dpi) {
                dpi = v_dpi as f32;
            }
        } else if res_id == 1007 && data_len > 0 && data_len <= 1_048_576 {
            let mut buf = vec![0u8; data_len as usize];
            reader.read_exact(&mut buf)?;
            let mut i = 0usize;
            while i < buf.len() {
                let plen = usize::from(buf[i]);
                i += 1;
                if i + plen > buf.len() {
                    break;
                }
                let s = decode_latin1(&buf[i..i + plen]).trim().to_owned();
                if !s.is_empty() {
                    pascal_names.push(s);
                }
                i += plen;
            }
        } else if res_id == 1045 && (4..=1_048_576).contains(&data_len) {
            let mut buf = vec![0u8; data_len as usize];
            reader.read_exact(&mut buf)?;
            let mut i = 0usize;
            while i + 4 <= buf.len() {
                let units =
                    u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) as usize;
                i += 4;
                let bytes_len = units.saturating_mul(2);
                if i + bytes_len > buf.len() {
                    break;
                }
                let u16s: Vec<u16> = buf[i..i + bytes_len]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes(*c))
                    .collect();
                let s = String::from_utf16_lossy(&u16s)
                    .trim_end_matches('\0')
                    .trim()
                    .to_owned();
                if !s.is_empty() {
                    unicode_names.push(s);
                }
                i += bytes_len;
            }
        }

        let next_pos = data_start.saturating_add(padded_len).min(end);
        reader.seek(SeekFrom::Start(next_pos))?;
    }

    let spot_names = if !unicode_names.is_empty() {
        unicode_names
    } else {
        pascal_names
    };
    Ok(ImageResourcesMeta { dpi, spot_names })
}

fn read_layer_record<R: Read + Seek>(
    reader: &mut R,
    is_psb: bool,
    li_end: u64,
    layer_idx: usize,
) -> IoResult<PsdLayerRecord> {
    let top = read_i32_be(reader)?;
    let left = read_i32_be(reader)?;
    let bottom = read_i32_be(reader)?;
    let right = read_i32_be(reader)?;
    let num_channels = usize::from(read_u16_be(reader)?);
    if num_channels > 64 {
        return Err(psd_err(format!(
            "layer {layer_idx} has excessive channel count ({num_channels})"
        )));
    }

    let mut channels = Vec::with_capacity(num_channels);
    for _ in 0..num_channels {
        let id = read_i16_be(reader)?;
        let data_len = read_section_len(reader, is_psb)?;
        channels.push(PsdChannelRef { id, data_len });
    }

    let mut blend_hdr = [0u8; 12];
    reader.read_exact(&mut blend_hdr)?;
    if &blend_hdr[0..4] != b"8BIM" {
        return Err(psd_err(format!(
            "layer {layer_idx} missing '8BIM' blend mode signature"
        )));
    }
    let blend_mode = map_psd_blend_mode(&blend_hdr[4..8]);
    let opacity = (f32::from(blend_hdr[8]) / 255.0).clamp(0.0, 1.0);
    let flags = blend_hdr[10];
    let visible = (flags & 0x02) == 0;

    let extra_len = u64::from(read_u32_be(reader)?);
    let extra_start = reader.stream_position()?;
    let extra_end = extra_start
        .checked_add(extra_len)
        .ok_or_else(|| psd_err("layer extra data overflow"))?;
    if extra_end > li_end {
        return Err(psd_err("layer extra data exceeds layer info bounds"));
    }

    let mut name = format!("Layer {}", layer_idx + 1);
    let mut divider_type: Option<u32> = None;
    let mut mask_info: Option<PsdMaskInfo> = None;

    if extra_len >= 8 {
        // 1. Layer mask data
        let mask_len = u64::from(read_u32_be(reader)?);
        let mask_start = reader.stream_position()?;
        if mask_start.saturating_add(mask_len) <= extra_end {
            if mask_len >= 20 {
                let m_top = read_i32_be(reader)?;
                let m_left = read_i32_be(reader)?;
                let m_bottom = read_i32_be(reader)?;
                let m_right = read_i32_be(reader)?;
                let def_col = read_u8(reader)?;
                mask_info = Some(PsdMaskInfo {
                    top: m_top,
                    left: m_left,
                    bottom: m_bottom,
                    right: m_right,
                    default_color: def_col,
                });
            }
            reader.seek(SeekFrom::Start(mask_start + mask_len))?;
        } else {
            reader.seek(SeekFrom::Start(extra_end))?;
        }

        // 2. Layer blending ranges
        if reader.stream_position()? + 4 <= extra_end {
            let ranges_len = u64::from(read_u32_be(reader)?);
            if reader.stream_position()?.saturating_add(ranges_len) <= extra_end {
                skip_bytes(reader, ranges_len)?;
            } else {
                reader.seek(SeekFrom::Start(extra_end))?;
            }
        }

        // 3. Pascal layer name (padded to a multiple of 4 bytes)
        if reader.stream_position()? < extra_end {
            let name_len = usize::from(read_u8(reader)?);
            let total_pascal = (1 + name_len + 3) & !3;
            let remaining_after_len = total_pascal - 1;
            if reader.stream_position()? + (remaining_after_len as u64) <= extra_end {
                let mut raw_name = vec![0u8; remaining_after_len];
                reader.read_exact(&mut raw_name)?;
                let pascal_str = decode_latin1(&raw_name[..name_len]);
                if !pascal_str.is_empty() {
                    name = pascal_str;
                }
            }
        }

        // 4. Additional Layer Information blocks (`luni`, `lsct`, `lsdk`)
        while reader.stream_position()? + 12 <= extra_end {
            let mut sig = [0u8; 4];
            reader.read_exact(&mut sig)?;
            if &sig != b"8BIM" && &sig != b"8B64" {
                break;
            }
            let mut key = [0u8; 4];
            reader.read_exact(&mut key)?;
            let is_64 = is_psb && psb_additional_key_is_64bit(&key);
            let block_len = if is_64 {
                if reader.stream_position()? + 8 > extra_end {
                    break;
                }
                read_u64_be(reader)?
            } else {
                u64::from(read_u32_be(reader)?)
            };
            let block_start = reader.stream_position()?;
            if block_start.saturating_add(block_len) > extra_end {
                break;
            }

            if &key == b"luni" && (4..=65_536).contains(&block_len) {
                let utf16_units = read_u32_be(reader)? as usize;
                let byte_needed = utf16_units.saturating_mul(2);
                if (byte_needed as u64) + 4 <= block_len {
                    let mut raw_u16 = vec![0u8; byte_needed];
                    reader.read_exact(&mut raw_u16)?;
                    let units: Vec<u16> = raw_u16
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect();
                    let decoded = String::from_utf16_lossy(&units)
                        .trim_end_matches('\0')
                        .to_owned();
                    if !decoded.is_empty() {
                        name = decoded;
                    }
                }
            } else if (&key == b"lsct" || &key == b"lsdk") && block_len >= 4 {
                let div_type = read_u32_be(reader)?;
                if matches!(div_type, 1..=3) {
                    divider_type = Some(div_type);
                }
            }

            let padded = (block_len + 1) & !1;
            let next_pos = block_start.saturating_add(padded).min(extra_end);
            reader.seek(SeekFrom::Start(next_pos))?;
        }
    }

    reader.seek(SeekFrom::Start(extra_end))?;

    Ok(PsdLayerRecord {
        top,
        left,
        bottom,
        right,
        channels,
        blend_mode,
        opacity,
        visible,
        name,
        divider_type,
        mask_info,
    })
}

fn psb_additional_key_is_64bit(key: &[u8; 4]) -> bool {
    matches!(
        key,
        b"LMsk"
            | b"Lr16"
            | b"Lr32"
            | b"Layr"
            | b"Mt16"
            | b"Mt32"
            | b"Mtrn"
            | b"Alph"
            | b"FMsk"
            | b"lnk2"
            | b"FEid"
            | b"FXid"
            | b"PxSD"
    )
}

fn map_psd_blend_mode(key: &[u8]) -> BlendMode {
    match key {
        b"mul " => BlendMode::Multiply,
        b"scrn" => BlendMode::Screen,
        b"over" => BlendMode::Overlay,
        b"dark" | b"dkCl" => BlendMode::Darken,
        b"lite" | b"lgCl" => BlendMode::Lighten,
        b"diff" => BlendMode::Difference,
        b"smud" => BlendMode::Exclusion,
        b"sLit" => BlendMode::SoftLight,
        b"hLit" => BlendMode::HardLight,
        b"div " => BlendMode::ColorDodge,
        b"idiv" => BlendMode::ColorBurn,
        b"hue " => BlendMode::Hue,
        b"sat " => BlendMode::Saturation,
        b"colr" => BlendMode::Color,
        b"lum " => BlendMode::Luminosity,
        _ => BlendMode::Normal,
    }
}

/// Computes target `(dst_w, dst_h)` respecting both `max_dim` and `budget_remaining` bytes.
fn compute_target_dimensions(
    src_w: u32,
    src_h: u32,
    max_dim: u32,
    budget_remaining: usize,
    bytes_per_px: usize,
) -> Option<(u32, u32)> {
    let bpp = bytes_per_px.max(4);
    if src_w == 0 || src_h == 0 || budget_remaining < bpp {
        return None;
    }
    let mut scale = 1.0_f64;
    let longest = src_w.max(src_h);
    if longest > max_dim {
        scale = f64::from(max_dim) / f64::from(longest);
    }
    let mut dst_w = ((f64::from(src_w) * scale).round() as u32).clamp(1, max_dim);
    let mut dst_h = ((f64::from(src_h) * scale).round() as u32).clamp(1, max_dim);

    let max_pixels = budget_remaining / bpp;
    let cur_pixels = (dst_w as usize).saturating_mul(dst_h as usize);
    if cur_pixels > max_pixels {
        let factor = ((max_pixels as f64) / (cur_pixels as f64)).sqrt();
        dst_w = ((f64::from(dst_w) * factor).floor() as u32).max(1);
        dst_h = ((f64::from(dst_h) * factor).floor() as u32).max(1);
        while (dst_w as usize).saturating_mul(dst_h as usize) > max_pixels {
            if dst_w >= dst_h && dst_w > 1 {
                dst_w -= 1;
            } else if dst_h > 1 {
                dst_h -= 1;
            } else {
                break;
            }
        }
    }
    if (dst_w as usize).saturating_mul(dst_h as usize) > max_pixels {
        return None;
    }
    Some((dst_w, dst_h))
}

#[allow(clippy::too_many_arguments)]
fn decode_layer_asset<R: Read + Seek>(
    reader: &mut R,
    header: &PsdHeader,
    indexed_palette: Option<&[u8]>,
    rec: &PsdLayerRecord,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    dpi: f32,
    channels: &[(i16, u64, u64)],
) -> IoResult<Option<(RasterAsset, usize)>> {
    if header.depth == 16
        && matches!(
            header.color_mode,
            PsdColorMode::Rgb | PsdColorMode::Grayscale | PsdColorMode::Cmyk | PsdColorMode::Lab
        )
    {
        return decode_layer_asset_16bit(
            reader, header, rec, src_w, src_h, dst_w, dst_h, dpi, channels,
        );
    }

    let mut ch0: Option<Vec<u8>> = None;
    let mut ch1: Option<Vec<u8>> = None;
    let mut ch2: Option<Vec<u8>> = None;
    let mut ch3: Option<Vec<u8>> = None;
    let mut alpha: Option<Vec<u8>> = None;
    let mut user_mask: Option<Vec<u8>> = None;
    let mut vec_mask: Option<Vec<u8>> = None;

    for &(ch_id, pos, data_len) in channels {
        if data_len < 2 {
            continue;
        }
        let is_mask = matches!(ch_id, -2 | -3);
        let needed = match header.color_mode {
            PsdColorMode::Bitmap
            | PsdColorMode::Grayscale
            | PsdColorMode::Indexed
            | PsdColorMode::Duotone
            | PsdColorMode::Multichannel => matches!(ch_id, -3..=0),
            PsdColorMode::Rgb | PsdColorMode::Lab => matches!(ch_id, -3..=2),
            PsdColorMode::Cmyk => matches!(ch_id, -3..=3),
        };
        if !needed {
            continue;
        }

        let (ch_src_w, ch_src_h, ch_dst_w, ch_dst_h) = if is_mask {
            if let Some(m) = rec.mask_info {
                let mw = i64::from(m.right)
                    .saturating_sub(i64::from(m.left))
                    .clamp(0, i64::from(MAX_PSD_DIMENSION)) as u32;
                let mh = i64::from(m.bottom)
                    .saturating_sub(i64::from(m.top))
                    .clamp(0, i64::from(MAX_PSD_DIMENSION)) as u32;
                if mw == 0 || mh == 0 {
                    continue;
                }
                let mdw = ((u64::from(mw) * u64::from(dst_w) / u64::from(src_w.max(1))) as u32)
                    .clamp(1, dst_w.max(1));
                let mdh = ((u64::from(mh) * u64::from(dst_h) / u64::from(src_h.max(1))) as u32)
                    .clamp(1, dst_h.max(1));
                (mw, mh, mdw, mdh)
            } else {
                (src_w, src_h, dst_w, dst_h)
            }
        } else {
            (src_w, src_h, dst_w, dst_h)
        };

        reader.seek(SeekFrom::Start(pos))?;
        let compression = read_u16_be(reader)?;
        let payload_len = data_len - 2;
        let mut bounded_reader = reader.by_ref().take(payload_len);
        let plane = decode_single_channel_plane(
            &mut bounded_reader,
            compression,
            payload_len,
            header.is_psb,
            header.depth,
            ch_src_w,
            ch_src_h,
            ch_dst_w,
            ch_dst_h,
        )?;

        match ch_id {
            0 => ch0 = Some(plane),
            1 => ch1 = Some(plane),
            2 => ch2 = Some(plane),
            3 => ch3 = Some(plane),
            -1 => alpha = Some(plane),
            -2 => user_mask = Some(plane),
            -3 => vec_mask = Some(plane),
            _ => {}
        }
    }

    if ch0.is_none() && alpha.is_none() {
        return Ok(None);
    }

    let mut rgba = assemble_rgba_planes(
        header.color_mode,
        indexed_palette,
        dst_w,
        dst_h,
        ch0.as_deref(),
        ch1.as_deref(),
        ch2.as_deref(),
        ch3.as_deref(),
        alpha.as_deref(),
    );

    if let Some(m) = user_mask {
        apply_layer_mask_u8(&mut rgba, dst_w, dst_h, rec, src_w, src_h, &m);
    }
    if let Some(m) = vec_mask {
        apply_layer_mask_u8(&mut rgba, dst_w, dst_h, rec, src_w, src_h, &m);
    }

    let cost = rgba.len();
    let asset = RasterAsset::new(dst_w, dst_h, rgba, dpi, header.color_mode.doc_color_mode())?;
    Ok(Some((asset, cost)))
}

#[allow(clippy::too_many_arguments)]
fn decode_layer_asset_16bit<R: Read + Seek>(
    reader: &mut R,
    header: &PsdHeader,
    rec: &PsdLayerRecord,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    dpi: f32,
    channels: &[(i16, u64, u64)],
) -> IoResult<Option<(RasterAsset, usize)>> {
    let pixel_count = (dst_w as usize) * (dst_h as usize);
    let mut ch0: Option<Vec<u16>> = None;
    let mut ch1: Option<Vec<u16>> = None;
    let mut ch2: Option<Vec<u16>> = None;
    let mut ch3: Option<Vec<u16>> = None;
    let mut alpha: Option<Vec<u16>> = None;
    let mut user_mask: Option<Vec<u16>> = None;
    let mut vec_mask: Option<Vec<u16>> = None;

    for &(ch_id, pos, data_len) in channels {
        if data_len < 2 {
            continue;
        }
        let is_mask = matches!(ch_id, -2 | -3);
        let needed = match header.color_mode {
            PsdColorMode::Grayscale => matches!(ch_id, -3..=0),
            PsdColorMode::Rgb | PsdColorMode::Lab => matches!(ch_id, -3..=2),
            PsdColorMode::Cmyk => matches!(ch_id, -3..=3),
            _ => false,
        };
        if !needed {
            continue;
        }

        let (ch_src_w, ch_src_h, ch_dst_w, ch_dst_h) = if is_mask {
            if let Some(m) = rec.mask_info {
                let mw = i64::from(m.right)
                    .saturating_sub(i64::from(m.left))
                    .clamp(0, i64::from(MAX_PSD_DIMENSION)) as u32;
                let mh = i64::from(m.bottom)
                    .saturating_sub(i64::from(m.top))
                    .clamp(0, i64::from(MAX_PSD_DIMENSION)) as u32;
                if mw == 0 || mh == 0 {
                    continue;
                }
                let mdw = ((u64::from(mw) * u64::from(dst_w) / u64::from(src_w.max(1))) as u32)
                    .clamp(1, dst_w.max(1));
                let mdh = ((u64::from(mh) * u64::from(dst_h) / u64::from(src_h.max(1))) as u32)
                    .clamp(1, dst_h.max(1));
                (mw, mh, mdw, mdh)
            } else {
                (src_w, src_h, dst_w, dst_h)
            }
        } else {
            (src_w, src_h, dst_w, dst_h)
        };

        reader.seek(SeekFrom::Start(pos))?;
        let compression = read_u16_be(reader)?;
        let payload_len = data_len - 2;
        let mut bounded_reader = reader.by_ref().take(payload_len);
        let plane = decode_single_channel_plane_16(
            &mut bounded_reader,
            compression,
            payload_len,
            header.is_psb,
            ch_src_w,
            ch_src_h,
            ch_dst_w,
            ch_dst_h,
        )?;
        match ch_id {
            0 => ch0 = Some(plane),
            1 => ch1 = Some(plane),
            2 => ch2 = Some(plane),
            3 => ch3 = Some(plane),
            -1 => alpha = Some(plane),
            -2 => user_mask = Some(plane),
            -3 => vec_mask = Some(plane),
            _ => {}
        }
    }

    if ch0.is_none() && alpha.is_none() {
        return Ok(None);
    }

    let mut rgba16_bytes = vec![0u8; pixel_count * 8];
    match header.color_mode {
        PsdColorMode::Grayscale => {
            for i in 0..pixel_count {
                let g = ch0
                    .as_ref()
                    .and_then(|c| c.get(i).copied())
                    .unwrap_or(65535);
                let a = alpha
                    .as_ref()
                    .and_then(|c| c.get(i).copied())
                    .unwrap_or(65535);
                write_rgba16_px(&mut rgba16_bytes, i, [g, g, g, a]);
            }
        }
        PsdColorMode::Rgb => {
            for i in 0..pixel_count {
                let r = ch0.as_ref().and_then(|c| c.get(i).copied()).unwrap_or(0);
                let g = ch1.as_ref().and_then(|c| c.get(i).copied()).unwrap_or(0);
                let b = ch2.as_ref().and_then(|c| c.get(i).copied()).unwrap_or(0);
                let a = alpha
                    .as_ref()
                    .and_then(|c| c.get(i).copied())
                    .unwrap_or(65535);
                write_rgba16_px(&mut rgba16_bytes, i, [r, g, b, a]);
            }
        }
        PsdColorMode::Cmyk => {
            for i in 0..pixel_count {
                let raw_c = u64::from(
                    ch0.as_ref()
                        .and_then(|c| c.get(i).copied())
                        .unwrap_or(65535),
                );
                let raw_m = u64::from(
                    ch1.as_ref()
                        .and_then(|c| c.get(i).copied())
                        .unwrap_or(65535),
                );
                let raw_y = u64::from(
                    ch2.as_ref()
                        .and_then(|c| c.get(i).copied())
                        .unwrap_or(65535),
                );
                let raw_k = u64::from(
                    ch3.as_ref()
                        .and_then(|c| c.get(i).copied())
                        .unwrap_or(65535),
                );
                let a = alpha
                    .as_ref()
                    .and_then(|c| c.get(i).copied())
                    .unwrap_or(65535);
                let r = ((raw_c * raw_k + 32767) / 65535) as u16;
                let g = ((raw_m * raw_k + 32767) / 65535) as u16;
                let b = ((raw_y * raw_k + 32767) / 65535) as u16;
                write_rgba16_px(&mut rgba16_bytes, i, [r, g, b, a]);
            }
        }
        PsdColorMode::Lab => {
            for i in 0..pixel_count {
                let l_raw = f32::from(ch0.as_ref().and_then(|c| c.get(i).copied()).unwrap_or(0));
                let a_raw = f32::from(
                    ch1.as_ref()
                        .and_then(|c| c.get(i).copied())
                        .unwrap_or(32768),
                );
                let b_raw = f32::from(
                    ch2.as_ref()
                        .and_then(|c| c.get(i).copied())
                        .unwrap_or(32768),
                );
                let alpha_u = alpha
                    .as_ref()
                    .and_then(|c| c.get(i).copied())
                    .unwrap_or(65535);
                let col = Color::lab(
                    l_raw * (100.0 / 65535.0),
                    (a_raw - 32768.0) * (128.0 / 32768.0),
                    (b_raw - 32768.0) * (128.0 / 32768.0),
                );
                let [r, g, b, _] = col.to_rgba_f32();
                write_rgba16_px(
                    &mut rgba16_bytes,
                    i,
                    [
                        (r * 65535.0).round().clamp(0.0, 65535.0) as u16,
                        (g * 65535.0).round().clamp(0.0, 65535.0) as u16,
                        (b * 65535.0).round().clamp(0.0, 65535.0) as u16,
                        alpha_u,
                    ],
                );
            }
        }
        _ => {}
    }

    drop(ch0);
    drop(ch1);
    drop(ch2);
    drop(ch3);
    drop(alpha);

    if let Some(m) = user_mask {
        apply_layer_mask_u16(&mut rgba16_bytes, dst_w, dst_h, rec, src_w, src_h, &m);
    }
    if let Some(m) = vec_mask {
        apply_layer_mask_u16(&mut rgba16_bytes, dst_w, dst_h, rec, src_w, src_h, &m);
    }

    let cost = rgba16_bytes.len();
    let asset = RasterAsset::new_16bit_bytes(
        dst_w,
        dst_h,
        rgba16_bytes,
        dpi,
        header.color_mode.doc_color_mode(),
    )?;
    Ok(Some((asset, cost)))
}

fn write_rgba16_px(buf: &mut [u8], px_idx: usize, rgba: [u16; 4]) {
    let off = px_idx * 8;
    for (k, ch) in rgba.into_iter().enumerate() {
        let le = ch.to_le_bytes();
        buf[off + k * 2] = le[0];
        buf[off + k * 2 + 1] = le[1];
    }
}

fn apply_layer_mask_u8(
    rgba: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    rec: &PsdLayerRecord,
    src_w: u32,
    src_h: u32,
    mask: &[u8],
) {
    let dw = dst_w as usize;
    let dh = dst_h as usize;
    if let Some(mi) = rec.mask_info {
        let mw_src = i64::from(mi.right)
            .saturating_sub(i64::from(mi.left))
            .max(0) as u32;
        let mh_src = i64::from(mi.bottom)
            .saturating_sub(i64::from(mi.top))
            .max(0) as u32;
        let mdw = ((u64::from(mw_src) * u64::from(dst_w) / u64::from(src_w.max(1))) as usize)
            .clamp(1, dw.max(1));
        let mdh = ((u64::from(mh_src) * u64::from(dst_h) / u64::from(src_h.max(1))) as usize)
            .clamp(1, dh.max(1));
        let off_x = ((i64::from(mi.left) - i64::from(rec.left)) * i64::from(dst_w)
            / i64::from(src_w.max(1))) as isize;
        let off_y = ((i64::from(mi.top) - i64::from(rec.top)) * i64::from(dst_h)
            / i64::from(src_h.max(1))) as isize;
        for y in 0..dh {
            let my = y as isize - off_y;
            for x in 0..dw {
                let mx = x as isize - off_x;
                let mv = if mx >= 0 && mx < mdw as isize && my >= 0 && my < mdh as isize {
                    mask.get((my as usize) * mdw + (mx as usize))
                        .copied()
                        .unwrap_or(mi.default_color)
                } else {
                    mi.default_color
                };
                let idx = (y * dw + x) * 4 + 3;
                rgba[idx] = ((u16::from(rgba[idx]) * u16::from(mv) + 127) / 255) as u8;
            }
        }
    } else {
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let mv = mask.get(i).copied().unwrap_or(255);
            px[3] = ((u16::from(px[3]) * u16::from(mv) + 127) / 255) as u8;
        }
    }
}

fn apply_layer_mask_u16(
    rgba_le: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    rec: &PsdLayerRecord,
    src_w: u32,
    src_h: u32,
    mask: &[u16],
) {
    let dw = dst_w as usize;
    let dh = dst_h as usize;
    if let Some(mi) = rec.mask_info {
        let def16 = u16::from(mi.default_color) * 257;
        let mw_src = i64::from(mi.right)
            .saturating_sub(i64::from(mi.left))
            .max(0) as u32;
        let mh_src = i64::from(mi.bottom)
            .saturating_sub(i64::from(mi.top))
            .max(0) as u32;
        let mdw = ((u64::from(mw_src) * u64::from(dst_w) / u64::from(src_w.max(1))) as usize)
            .clamp(1, dw.max(1));
        let mdh = ((u64::from(mh_src) * u64::from(dst_h) / u64::from(src_h.max(1))) as usize)
            .clamp(1, dh.max(1));
        let off_x = ((i64::from(mi.left) - i64::from(rec.left)) * i64::from(dst_w)
            / i64::from(src_w.max(1))) as isize;
        let off_y = ((i64::from(mi.top) - i64::from(rec.top)) * i64::from(dst_h)
            / i64::from(src_h.max(1))) as isize;
        for y in 0..dh {
            let my = y as isize - off_y;
            for x in 0..dw {
                let mx = x as isize - off_x;
                let mv = if mx >= 0 && mx < mdw as isize && my >= 0 && my < mdh as isize {
                    mask.get((my as usize) * mdw + (mx as usize))
                        .copied()
                        .unwrap_or(def16)
                } else {
                    def16
                };
                let off = (y * dw + x) * 8 + 6;
                let cur = u64::from(u16::from_le_bytes([rgba_le[off], rgba_le[off + 1]]));
                let out = ((cur * u64::from(mv) + 32767) / 65535) as u16;
                let le = out.to_le_bytes();
                rgba_le[off] = le[0];
                rgba_le[off + 1] = le[1];
            }
        }
    } else {
        for (i, px) in rgba_le.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            let mv = mask.get(i).copied().unwrap_or(65535);
            let cur = u64::from(u16::from_le_bytes([px[6], px[7]]));
            let out = ((cur * u64::from(mv) + 32767) / 65535) as u16;
            let le = out.to_le_bytes();
            px[6] = le[0];
            px[7] = le[1];
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_composite_asset<R: Read + Seek>(
    reader: &mut R,
    file_len: u64,
    header: &PsdHeader,
    indexed_palette: Option<&[u8]>,
    has_alpha: bool,
    dst_w: u32,
    dst_h: u32,
    dpi: f32,
) -> IoResult<Option<RasterAsset>> {
    let comp_start = reader.stream_position()?;
    if comp_start + 2 > file_len {
        return Ok(None);
    }
    let compression = read_u16_be(reader)?;
    let num_channels = usize::from(header.channels);
    let src_w = header.width;
    let src_h = header.height;

    let color_channels = match header.color_mode {
        PsdColorMode::Bitmap
        | PsdColorMode::Grayscale
        | PsdColorMode::Indexed
        | PsdColorMode::Duotone
        | PsdColorMode::Multichannel => 1usize,
        PsdColorMode::Rgb | PsdColorMode::Lab => 3usize,
        PsdColorMode::Cmyk => 4usize,
    };
    let alpha_channel_idx = if has_alpha && num_channels > color_channels {
        Some(color_channels)
    } else {
        None
    };
    let max_needed_ch = alpha_channel_idx.unwrap_or(color_channels.saturating_sub(1));

    if header.depth == 16
        && matches!(
            header.color_mode,
            PsdColorMode::Rgb | PsdColorMode::Grayscale | PsdColorMode::Cmyk | PsdColorMode::Lab
        )
    {
        let mut ch0: Option<Vec<u16>> = None;
        let mut ch1: Option<Vec<u16>> = None;
        let mut ch2: Option<Vec<u16>> = None;
        let mut ch3: Option<Vec<u16>> = None;
        let mut alpha: Option<Vec<u16>> = None;
        let row_bytes = scanline_byte_width(src_w, 16)?;
        let assign16 = |ch_idx: usize,
                        plane: Vec<u16>,
                        c0: &mut Option<Vec<u16>>,
                        c1: &mut Option<Vec<u16>>,
                        c2: &mut Option<Vec<u16>>,
                        c3: &mut Option<Vec<u16>>,
                        al: &mut Option<Vec<u16>>| {
            if Some(ch_idx) == alpha_channel_idx {
                *al = Some(plane);
                return;
            }
            match ch_idx {
                0 => *c0 = Some(plane),
                1 => *c1 = Some(plane),
                2 => *c2 = Some(plane),
                3 => *c3 = Some(plane),
                _ => {}
            }
        };

        match compression {
            0 => {
                for ch_idx in 0..num_channels {
                    if ch_idx > max_needed_ch {
                        break;
                    }
                    let plane = stream_decode_raw_plane_16(
                        reader, src_w, src_h, dst_w, dst_h, row_bytes, false,
                    )?;
                    assign16(
                        ch_idx, plane, &mut ch0, &mut ch1, &mut ch2, &mut ch3, &mut alpha,
                    );
                }
            }
            1 => {
                let total_rows = num_channels
                    .checked_mul(src_h as usize)
                    .ok_or_else(|| psd_err("composite RLE row count overflow"))?;
                let count_bytes_per_row = if header.is_psb { 4u64 } else { 2u64 };
                let table_bytes = (total_rows as u64)
                    .checked_mul(count_bytes_per_row)
                    .ok_or_else(|| psd_err("composite RLE row length table overflow"))?;
                let remaining = file_len.saturating_sub(reader.stream_position()?);
                if table_bytes > remaining {
                    return Err(psd_err(format!(
                        "composite RLE row table ({table_bytes} bytes) exceeds remaining file size ({remaining} bytes)"
                    )));
                }
                let mut all_row_counts = Vec::with_capacity(total_rows);
                for _ in 0..total_rows {
                    let len = if header.is_psb {
                        read_u32_be(reader)? as usize
                    } else {
                        usize::from(read_u16_be(reader)?)
                    };
                    all_row_counts.push(len);
                }
                for ch_idx in 0..num_channels {
                    if ch_idx > max_needed_ch {
                        break;
                    }
                    let row_slice =
                        &all_row_counts[ch_idx * (src_h as usize)..(ch_idx + 1) * (src_h as usize)];
                    let plane = stream_decode_rle_plane_16(
                        reader, row_slice, src_w, src_h, dst_w, dst_h, row_bytes,
                    )?;
                    assign16(
                        ch_idx, plane, &mut ch0, &mut ch1, &mut ch2, &mut ch3, &mut alpha,
                    );
                }
            }
            other => {
                return Err(psd_err(format!(
                    "unsupported composite compression mode {other}"
                )));
            }
        }
        if ch0.is_none() {
            return Ok(None);
        }
        let pixel_count = (dst_w as usize) * (dst_h as usize);
        let mut rgba16_bytes = vec![0u8; pixel_count * 8];
        match header.color_mode {
            PsdColorMode::Grayscale => {
                for i in 0..pixel_count {
                    let g = ch0
                        .as_ref()
                        .and_then(|c| c.get(i).copied())
                        .unwrap_or(65535);
                    let a = alpha
                        .as_ref()
                        .and_then(|c| c.get(i).copied())
                        .unwrap_or(65535);
                    write_rgba16_px(&mut rgba16_bytes, i, [g, g, g, a]);
                }
            }
            PsdColorMode::Rgb => {
                for i in 0..pixel_count {
                    let r = ch0.as_ref().and_then(|c| c.get(i).copied()).unwrap_or(0);
                    let g = ch1.as_ref().and_then(|c| c.get(i).copied()).unwrap_or(0);
                    let b = ch2.as_ref().and_then(|c| c.get(i).copied()).unwrap_or(0);
                    let a = alpha
                        .as_ref()
                        .and_then(|c| c.get(i).copied())
                        .unwrap_or(65535);
                    write_rgba16_px(&mut rgba16_bytes, i, [r, g, b, a]);
                }
            }
            PsdColorMode::Cmyk => {
                for i in 0..pixel_count {
                    let c = u64::from(
                        ch0.as_ref()
                            .and_then(|c| c.get(i).copied())
                            .unwrap_or(65535),
                    );
                    let m = u64::from(
                        ch1.as_ref()
                            .and_then(|c| c.get(i).copied())
                            .unwrap_or(65535),
                    );
                    let y = u64::from(
                        ch2.as_ref()
                            .and_then(|c| c.get(i).copied())
                            .unwrap_or(65535),
                    );
                    let k = u64::from(
                        ch3.as_ref()
                            .and_then(|c| c.get(i).copied())
                            .unwrap_or(65535),
                    );
                    let a = alpha
                        .as_ref()
                        .and_then(|c| c.get(i).copied())
                        .unwrap_or(65535);
                    write_rgba16_px(
                        &mut rgba16_bytes,
                        i,
                        [
                            ((c * k + 32767) / 65535) as u16,
                            ((m * k + 32767) / 65535) as u16,
                            ((y * k + 32767) / 65535) as u16,
                            a,
                        ],
                    );
                }
            }
            _ => {}
        }
        return Ok(Some(RasterAsset::new_16bit_bytes(
            dst_w,
            dst_h,
            rgba16_bytes,
            dpi,
            header.color_mode.doc_color_mode(),
        )?));
    }

    let mut ch0: Option<Vec<u8>> = None;
    let mut ch1: Option<Vec<u8>> = None;
    let mut ch2: Option<Vec<u8>> = None;
    let mut ch3: Option<Vec<u8>> = None;
    let mut alpha: Option<Vec<u8>> = None;

    match compression {
        0 => {
            let row_bytes = scanline_byte_width(src_w, header.depth)?;
            for ch_idx in 0..num_channels {
                if ch_idx > max_needed_ch {
                    break;
                }
                let plane = stream_decode_raw_plane(
                    reader,
                    src_w,
                    src_h,
                    dst_w,
                    dst_h,
                    header.depth,
                    row_bytes,
                )?;
                assign_composite_plane(
                    ch_idx,
                    alpha_channel_idx,
                    plane,
                    &mut ch0,
                    &mut ch1,
                    &mut ch2,
                    &mut ch3,
                    &mut alpha,
                );
            }
        }
        1 => {
            let total_rows = num_channels
                .checked_mul(src_h as usize)
                .ok_or_else(|| psd_err("composite RLE row count overflow"))?;
            let count_bytes_per_row = if header.is_psb { 4u64 } else { 2u64 };
            let table_bytes = (total_rows as u64)
                .checked_mul(count_bytes_per_row)
                .ok_or_else(|| psd_err("composite RLE row length table overflow"))?;
            let remaining = file_len.saturating_sub(reader.stream_position()?);
            if table_bytes > remaining {
                return Err(psd_err(format!(
                    "composite RLE row table ({table_bytes} bytes) exceeds remaining file size ({remaining} bytes)"
                )));
            }
            let mut all_row_counts = Vec::with_capacity(total_rows);
            for _ in 0..total_rows {
                let len = if header.is_psb {
                    read_u32_be(reader)? as usize
                } else {
                    usize::from(read_u16_be(reader)?)
                };
                all_row_counts.push(len);
            }

            let row_bytes = scanline_byte_width(src_w, header.depth)?;
            for ch_idx in 0..num_channels {
                let row_slice =
                    &all_row_counts[ch_idx * (src_h as usize)..(ch_idx + 1) * (src_h as usize)];
                if ch_idx > max_needed_ch {
                    break;
                }
                let plane = stream_decode_rle_plane(
                    reader,
                    row_slice,
                    src_w,
                    src_h,
                    dst_w,
                    dst_h,
                    header.depth,
                    row_bytes,
                )?;
                assign_composite_plane(
                    ch_idx,
                    alpha_channel_idx,
                    plane,
                    &mut ch0,
                    &mut ch1,
                    &mut ch2,
                    &mut ch3,
                    &mut alpha,
                );
            }
        }
        other => {
            return Err(psd_err(format!(
                "unsupported composite compression mode {other}"
            )));
        }
    }

    if ch0.is_none() {
        return Ok(None);
    }

    let rgba = assemble_rgba_planes(
        header.color_mode,
        indexed_palette,
        dst_w,
        dst_h,
        ch0.as_deref(),
        ch1.as_deref(),
        ch2.as_deref(),
        ch3.as_deref(),
        alpha.as_deref(),
    );
    Ok(Some(RasterAsset::new(
        dst_w,
        dst_h,
        rgba,
        dpi,
        header.color_mode.doc_color_mode(),
    )?))
}

#[allow(clippy::too_many_arguments)]
fn assign_composite_plane(
    ch_idx: usize,
    alpha_idx: Option<usize>,
    plane: Vec<u8>,
    ch0: &mut Option<Vec<u8>>,
    ch1: &mut Option<Vec<u8>>,
    ch2: &mut Option<Vec<u8>>,
    ch3: &mut Option<Vec<u8>>,
    alpha: &mut Option<Vec<u8>>,
) {
    if Some(ch_idx) == alpha_idx {
        *alpha = Some(plane);
        return;
    }
    match ch_idx {
        0 => *ch0 = Some(plane),
        1 => *ch1 = Some(plane),
        2 => *ch2 = Some(plane),
        3 => *ch3 = Some(plane),
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_single_channel_plane<R: Read>(
    reader: &mut R,
    compression: u16,
    payload_len: u64,
    is_psb: bool,
    depth: u16,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> IoResult<Vec<u8>> {
    let row_bytes = scanline_byte_width(src_w, depth)?;
    match compression {
        0 => stream_decode_raw_plane(reader, src_w, src_h, dst_w, dst_h, depth, row_bytes),
        1 => {
            let count_bytes_per_row = if is_psb { 4u64 } else { 2u64 };
            let table_bytes = u64::from(src_h).saturating_mul(count_bytes_per_row);
            if table_bytes > payload_len {
                return Err(psd_err("RLE row length table exceeds channel payload"));
            }
            let mut row_counts = Vec::with_capacity(src_h as usize);
            for _ in 0..src_h {
                let len = if is_psb {
                    read_u32_be(reader)? as usize
                } else {
                    usize::from(read_u16_be(reader)?)
                };
                row_counts.push(len);
            }
            stream_decode_rle_plane(
                reader,
                &row_counts,
                src_w,
                src_h,
                dst_w,
                dst_h,
                depth,
                row_bytes,
            )
        }
        2 | 3 => {
            if payload_len > (MAX_ZIP_CHANNEL_BYTES as u64) {
                return Err(IoError::LimitExceeded {
                    limit: format!("max_zip_channel_bytes={MAX_ZIP_CHANNEL_BYTES}"),
                    actual: format!("{payload_len} bytes"),
                });
            }
            let expected_raw = row_bytes
                .checked_mul(src_h as usize)
                .ok_or_else(|| psd_err("ZIP channel byte size overflow"))?;
            if expected_raw > MAX_ZIP_CHANNEL_BYTES {
                return Err(IoError::LimitExceeded {
                    limit: format!("max_zip_channel_bytes={MAX_ZIP_CHANNEL_BYTES}"),
                    actual: format!("{expected_raw} uncompressed bytes"),
                });
            }
            let mut decoder = ZlibDecoder::new(reader).take((MAX_ZIP_CHANNEL_BYTES as u64) + 1);
            stream_decode_raw_plane_with_pred(
                &mut decoder,
                src_w,
                src_h,
                dst_w,
                dst_h,
                depth,
                row_bytes,
                compression == 3,
            )
        }
        other => Err(psd_err(format!(
            "unsupported layer channel compression mode {other}"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_single_channel_plane_16<R: Read>(
    reader: &mut R,
    compression: u16,
    payload_len: u64,
    is_psb: bool,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> IoResult<Vec<u16>> {
    let row_bytes = scanline_byte_width(src_w, 16)?;
    match compression {
        0 => stream_decode_raw_plane_16(reader, src_w, src_h, dst_w, dst_h, row_bytes, false),
        1 => {
            let count_bytes_per_row = if is_psb { 4u64 } else { 2u64 };
            let table_bytes = u64::from(src_h).saturating_mul(count_bytes_per_row);
            if table_bytes > payload_len {
                return Err(psd_err("RLE row length table exceeds channel payload"));
            }
            let mut row_counts = Vec::with_capacity(src_h as usize);
            for _ in 0..src_h {
                let len = if is_psb {
                    read_u32_be(reader)? as usize
                } else {
                    usize::from(read_u16_be(reader)?)
                };
                row_counts.push(len);
            }
            stream_decode_rle_plane_16(reader, &row_counts, src_w, src_h, dst_w, dst_h, row_bytes)
        }
        2 | 3 => {
            if payload_len > (MAX_ZIP_CHANNEL_BYTES as u64) {
                return Err(IoError::LimitExceeded {
                    limit: format!("max_zip_channel_bytes={MAX_ZIP_CHANNEL_BYTES}"),
                    actual: format!("{payload_len} bytes"),
                });
            }
            let expected_raw = row_bytes
                .checked_mul(src_h as usize)
                .ok_or_else(|| psd_err("ZIP channel byte size overflow"))?;
            if expected_raw > MAX_ZIP_CHANNEL_BYTES {
                return Err(IoError::LimitExceeded {
                    limit: format!("max_zip_channel_bytes={MAX_ZIP_CHANNEL_BYTES}"),
                    actual: format!("{expected_raw} uncompressed bytes"),
                });
            }
            let mut decoder = ZlibDecoder::new(reader).take((MAX_ZIP_CHANNEL_BYTES as u64) + 1);
            stream_decode_raw_plane_16(
                &mut decoder,
                src_w,
                src_h,
                dst_w,
                dst_h,
                row_bytes,
                compression == 3,
            )
        }
        other => Err(psd_err(format!(
            "unsupported layer channel compression mode {other}"
        ))),
    }
}

fn scanline_byte_width(src_w: u32, depth: u16) -> IoResult<usize> {
    let w = src_w as usize;
    match depth {
        1 => Ok(w.div_ceil(8)),
        8 => Ok(w),
        16 => w
            .checked_mul(2)
            .ok_or_else(|| psd_err("scanline width overflow")),
        _ => Err(psd_err(format!("unsupported depth {depth}"))),
    }
}

fn stream_decode_raw_plane<R: Read>(
    reader: &mut R,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    depth: u16,
    row_bytes: usize,
) -> IoResult<Vec<u8>> {
    stream_decode_raw_plane_with_pred(reader, src_w, src_h, dst_w, dst_h, depth, row_bytes, false)
}

#[allow(clippy::too_many_arguments)]
fn stream_decode_raw_plane_with_pred<R: Read>(
    reader: &mut R,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    depth: u16,
    row_bytes: usize,
    predict: bool,
) -> IoResult<Vec<u8>> {
    let mut raw_row = vec![0u8; row_bytes];
    let mut u8_row = vec![0u8; src_w as usize];

    let apply_pred = |row: &mut [u8]| {
        if !predict {
            return;
        }
        if depth == 16 {
            let mut acc = 0u16;
            for px in row.as_chunks_mut::<2>().0 {
                acc = acc.wrapping_add(u16::from_be_bytes([px[0], px[1]]));
                let be = acc.to_be_bytes();
                px[0] = be[0];
                px[1] = be[1];
            }
        } else {
            let mut acc = 0u8;
            for b in row.iter_mut() {
                acc = acc.wrapping_add(*b);
                *b = acc;
            }
        }
    };

    if dst_w == src_w && dst_h == src_h {
        let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize)];
        for y in 0..(src_h as usize) {
            read_exact_or_zero_pad(reader, &mut raw_row)?;
            apply_pred(&mut raw_row);
            unpack_scanline_to_u8(&raw_row, depth, &mut u8_row);
            let dst_off = y * (dst_w as usize);
            out[dst_off..dst_off + (dst_w as usize)].copy_from_slice(&u8_row);
        }
        return Ok(out);
    }

    let mut acc = BoxAccumulator::new(src_w, src_h, dst_w, dst_h);
    for y in 0..(src_h as usize) {
        read_exact_or_zero_pad(reader, &mut raw_row)?;
        apply_pred(&mut raw_row);
        unpack_scanline_to_u8(&raw_row, depth, &mut u8_row);
        acc.push_row(y, &u8_row);
    }
    Ok(acc.finish())
}

fn stream_decode_raw_plane_16<R: Read>(
    reader: &mut R,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    row_bytes: usize,
    predict: bool,
) -> IoResult<Vec<u16>> {
    let mut raw_row = vec![0u8; row_bytes];
    let mut u16_row = vec![0u16; src_w as usize];

    let apply_pred = |row: &mut [u8]| {
        if !predict {
            return;
        }
        let mut acc = 0u16;
        for px in row.as_chunks_mut::<2>().0 {
            acc = acc.wrapping_add(u16::from_be_bytes([px[0], px[1]]));
            let be = acc.to_be_bytes();
            px[0] = be[0];
            px[1] = be[1];
        }
    };

    if dst_w == src_w && dst_h == src_h {
        let mut out = vec![0u16; (dst_w as usize) * (dst_h as usize)];
        for y in 0..(src_h as usize) {
            read_exact_or_zero_pad(reader, &mut raw_row)?;
            apply_pred(&mut raw_row);
            for (dst, pair) in u16_row.iter_mut().zip(raw_row.as_chunks::<2>().0) {
                *dst = u16::from_be_bytes(*pair);
            }
            let dst_off = y * (dst_w as usize);
            out[dst_off..dst_off + (dst_w as usize)].copy_from_slice(&u16_row);
        }
        return Ok(out);
    }

    let mut acc = BoxAccumulator::new(src_w, src_h, dst_w, dst_h);
    for y in 0..(src_h as usize) {
        read_exact_or_zero_pad(reader, &mut raw_row)?;
        apply_pred(&mut raw_row);
        for (dst, pair) in u16_row.iter_mut().zip(raw_row.as_chunks::<2>().0) {
            *dst = u16::from_be_bytes(*pair);
        }
        acc.push_row_16(y, &u16_row);
    }
    Ok(acc.finish_16())
}

fn read_exact_or_zero_pad<R: Read>(reader: &mut R, buf: &mut [u8]) -> IoResult<()> {
    let mut off = 0usize;
    while off < buf.len() {
        let n = reader.read(&mut buf[off..])?;
        if n == 0 {
            if off == 0 {
                return Err(IoError::InvalidFormat {
                    format: "psd".into(),
                    message: "unexpected EOF in channel scanline".into(),
                });
            }
            buf[off..].fill(0);
            break;
        }
        off += n;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn stream_decode_rle_plane<R: Read>(
    reader: &mut R,
    row_counts: &[usize],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    depth: u16,
    row_bytes: usize,
) -> IoResult<Vec<u8>> {
    let max_encoded_row = row_bytes
        .saturating_mul(2)
        .saturating_add(128)
        .min(32 * 1024 * 1024);
    let mut packed = Vec::new();
    let mut raw_row = vec![0u8; row_bytes];
    let mut u8_row = vec![0u8; src_w as usize];

    if dst_w == src_w && dst_h == src_h {
        let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize)];
        for (y, &enc_len) in row_counts.iter().enumerate().take(src_h as usize) {
            if enc_len > max_encoded_row {
                return Err(psd_err(format!(
                    "PackBits scanline length {enc_len} exceeds safe bound {max_encoded_row}"
                )));
            }
            packed.resize(enc_len, 0);
            reader.read_exact(&mut packed)?;
            decode_packbits_row(&packed, &mut raw_row);
            unpack_scanline_to_u8(&raw_row, depth, &mut u8_row);
            let dst_off = y * (dst_w as usize);
            out[dst_off..dst_off + (dst_w as usize)].copy_from_slice(&u8_row);
        }
        return Ok(out);
    }

    let mut acc = BoxAccumulator::new(src_w, src_h, dst_w, dst_h);
    for (y, &enc_len) in row_counts.iter().enumerate().take(src_h as usize) {
        if enc_len > max_encoded_row {
            return Err(psd_err(format!(
                "PackBits scanline length {enc_len} exceeds safe bound {max_encoded_row}"
            )));
        }
        packed.resize(enc_len, 0);
        reader.read_exact(&mut packed)?;
        decode_packbits_row(&packed, &mut raw_row);
        unpack_scanline_to_u8(&raw_row, depth, &mut u8_row);
        acc.push_row(y, &u8_row);
    }
    Ok(acc.finish())
}

#[allow(clippy::too_many_arguments)]
fn stream_decode_rle_plane_16<R: Read>(
    reader: &mut R,
    row_counts: &[usize],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    row_bytes: usize,
) -> IoResult<Vec<u16>> {
    let max_encoded_row = row_bytes
        .saturating_mul(2)
        .saturating_add(128)
        .min(32 * 1024 * 1024);
    let mut packed = Vec::new();
    let mut raw_row = vec![0u8; row_bytes];
    let mut u16_row = vec![0u16; src_w as usize];

    if dst_w == src_w && dst_h == src_h {
        let mut out = vec![0u16; (dst_w as usize) * (dst_h as usize)];
        for (y, &enc_len) in row_counts.iter().enumerate().take(src_h as usize) {
            if enc_len > max_encoded_row {
                return Err(psd_err(format!(
                    "PackBits scanline length {enc_len} exceeds safe bound {max_encoded_row}"
                )));
            }
            packed.resize(enc_len, 0);
            reader.read_exact(&mut packed)?;
            decode_packbits_row(&packed, &mut raw_row);
            for (dst, pair) in u16_row.iter_mut().zip(raw_row.as_chunks::<2>().0) {
                *dst = u16::from_be_bytes(*pair);
            }
            let dst_off = y * (dst_w as usize);
            out[dst_off..dst_off + (dst_w as usize)].copy_from_slice(&u16_row);
        }
        return Ok(out);
    }

    let mut acc = BoxAccumulator::new(src_w, src_h, dst_w, dst_h);
    for (y, &enc_len) in row_counts.iter().enumerate().take(src_h as usize) {
        if enc_len > max_encoded_row {
            return Err(psd_err(format!(
                "PackBits scanline length {enc_len} exceeds safe bound {max_encoded_row}"
            )));
        }
        packed.resize(enc_len, 0);
        reader.read_exact(&mut packed)?;
        decode_packbits_row(&packed, &mut raw_row);
        for (dst, pair) in u16_row.iter_mut().zip(raw_row.as_chunks::<2>().0) {
            *dst = u16::from_be_bytes(*pair);
        }
        acc.push_row_16(y, &u16_row);
    }
    Ok(acc.finish_16())
}

fn decode_packbits_row(src: &[u8], dst: &mut [u8]) {
    dst.fill(0);
    let mut si = 0usize;
    let mut di = 0usize;
    let slen = src.len();
    let dlen = dst.len();

    while si < slen && di < dlen {
        let header = src[si] as i8;
        si += 1;
        if header >= 0 {
            let count = (header as usize) + 1;
            let avail = (slen - si).min(dlen - di).min(count);
            dst[di..di + avail].copy_from_slice(&src[si..si + avail]);
            si += avail;
            di += avail;
        } else if header != -128 {
            let count = (1i16 - i16::from(header)) as usize;
            if si >= slen {
                break;
            }
            let val = src[si];
            si += 1;
            let avail = (dlen - di).min(count);
            dst[di..di + avail].fill(val);
            di += avail;
        }
    }
}

fn unpack_scanline_to_u8(raw_row: &[u8], depth: u16, out: &mut [u8]) {
    match depth {
        8 => {
            let n = raw_row.len().min(out.len());
            out[..n].copy_from_slice(&raw_row[..n]);
        }
        16 => {
            for (dst, pair) in out.iter_mut().zip(raw_row.as_chunks::<2>().0) {
                *dst = pair[0];
            }
        }
        1 => {
            for (x, dst) in out.iter_mut().enumerate() {
                let byte = raw_row.get(x / 8).copied().unwrap_or(0);
                let bit = (byte >> (7 - (x % 8))) & 1;
                *dst = if bit == 0 { 255 } else { 0 };
            }
        }
        _ => out.fill(0),
    }
}

struct BoxAccumulator {
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
    x_map: Vec<usize>,
    sums: Vec<u64>,
    counts: Vec<u32>,
}

impl BoxAccumulator {
    fn new(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Self {
        let sw = src_w as usize;
        let dw = dst_w as usize;
        let dh = dst_h as usize;
        let mut x_map = Vec::with_capacity(sw);
        for x in 0..sw {
            let dx = ((x as u64) * (dw as u64) / (sw.max(1) as u64)) as usize;
            x_map.push(dx.min(dw - 1));
        }
        let len = dw * dh;
        Self {
            src_h: src_h as usize,
            dst_w: dw,
            dst_h: dh,
            x_map,
            sums: vec![0u64; len],
            counts: vec![0u32; len],
        }
    }

    fn push_row(&mut self, src_y: usize, row: &[u8]) {
        let dy = (((src_y as u64) * (self.dst_h as u64)) / (self.src_h.max(1) as u64)) as usize;
        let row_off = dy.min(self.dst_h - 1) * self.dst_w;
        for (&dx, &sample) in self.x_map.iter().zip(row.iter()) {
            let idx = row_off + dx;
            self.sums[idx] += u64::from(sample);
            self.counts[idx] += 1;
        }
    }

    fn push_row_16(&mut self, src_y: usize, row: &[u16]) {
        let dy = (((src_y as u64) * (self.dst_h as u64)) / (self.src_h.max(1) as u64)) as usize;
        let row_off = dy.min(self.dst_h - 1) * self.dst_w;
        for (&dx, &sample) in self.x_map.iter().zip(row.iter()) {
            let idx = row_off + dx;
            self.sums[idx] += u64::from(sample);
            self.counts[idx] += 1;
        }
    }

    fn finish(self) -> Vec<u8> {
        self.sums
            .into_iter()
            .zip(self.counts)
            .map(|(sum, cnt)| {
                if cnt == 0 {
                    0
                } else {
                    ((sum + u64::from(cnt / 2)) / u64::from(cnt)).min(255) as u8
                }
            })
            .collect()
    }

    fn finish_16(self) -> Vec<u16> {
        self.sums
            .into_iter()
            .zip(self.counts)
            .map(|(sum, cnt)| {
                if cnt == 0 {
                    0
                } else {
                    ((sum + u64::from(cnt / 2)) / u64::from(cnt)).min(65535) as u16
                }
            })
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn assemble_rgba_planes(
    color_mode: PsdColorMode,
    indexed_palette: Option<&[u8]>,
    width: u32,
    height: u32,
    ch0: Option<&[u8]>,
    ch1: Option<&[u8]>,
    ch2: Option<&[u8]>,
    ch3: Option<&[u8]>,
    alpha: Option<&[u8]>,
) -> Vec<u8> {
    let pixel_count = (width as usize) * (height as usize);
    let mut rgba = vec![0u8; pixel_count * 4];

    match color_mode {
        PsdColorMode::Bitmap
        | PsdColorMode::Grayscale
        | PsdColorMode::Duotone
        | PsdColorMode::Multichannel => {
            for i in 0..pixel_count {
                let g = ch0.and_then(|c| c.get(i).copied()).unwrap_or(255);
                let a = alpha.and_then(|c| c.get(i).copied()).unwrap_or(255);
                let o = i * 4;
                rgba[o] = g;
                rgba[o + 1] = g;
                rgba[o + 2] = g;
                rgba[o + 3] = a;
            }
        }
        PsdColorMode::Indexed => {
            for i in 0..pixel_count {
                let idx = usize::from(ch0.and_then(|c| c.get(i).copied()).unwrap_or(0));
                let (r, g, b) = if let Some(pal) = indexed_palette {
                    (pal[idx], pal[256 + idx], pal[512 + idx])
                } else {
                    (idx as u8, idx as u8, idx as u8)
                };
                let a = alpha.and_then(|c| c.get(i).copied()).unwrap_or(255);
                let o = i * 4;
                rgba[o] = r;
                rgba[o + 1] = g;
                rgba[o + 2] = b;
                rgba[o + 3] = a;
            }
        }
        PsdColorMode::Rgb => {
            for i in 0..pixel_count {
                let r = ch0.and_then(|c| c.get(i).copied()).unwrap_or(0);
                let g = ch1.and_then(|c| c.get(i).copied()).unwrap_or(0);
                let b = ch2.and_then(|c| c.get(i).copied()).unwrap_or(0);
                let a = alpha.and_then(|c| c.get(i).copied()).unwrap_or(255);
                let o = i * 4;
                rgba[o] = r;
                rgba[o + 1] = g;
                rgba[o + 2] = b;
                rgba[o + 3] = a;
            }
        }
        PsdColorMode::Cmyk => {
            for i in 0..pixel_count {
                let raw_c = u16::from(ch0.and_then(|c| c.get(i).copied()).unwrap_or(255));
                let raw_m = u16::from(ch1.and_then(|c| c.get(i).copied()).unwrap_or(255));
                let raw_y = u16::from(ch2.and_then(|c| c.get(i).copied()).unwrap_or(255));
                let raw_k = u16::from(ch3.and_then(|c| c.get(i).copied()).unwrap_or(255));
                let a = alpha.and_then(|c| c.get(i).copied()).unwrap_or(255);
                let o = i * 4;
                rgba[o] = ((raw_c * raw_k + 127) / 255) as u8;
                rgba[o + 1] = ((raw_m * raw_k + 127) / 255) as u8;
                rgba[o + 2] = ((raw_y * raw_k + 127) / 255) as u8;
                rgba[o + 3] = a;
            }
        }
        PsdColorMode::Lab => {
            for i in 0..pixel_count {
                let l_raw = f32::from(ch0.and_then(|c| c.get(i).copied()).unwrap_or(0));
                let a_raw = f32::from(ch1.and_then(|c| c.get(i).copied()).unwrap_or(128));
                let b_raw = f32::from(ch2.and_then(|c| c.get(i).copied()).unwrap_or(128));
                let alpha_b = alpha.and_then(|c| c.get(i).copied()).unwrap_or(255);
                let col = Color::lab(l_raw * (100.0 / 255.0), a_raw - 128.0, b_raw - 128.0);
                let [r, g, b, _] = col.to_rgba_f32();
                let o = i * 4;
                rgba[o] = (r * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[o + 1] = (g * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[o + 2] = (b * 255.0).round().clamp(0.0, 255.0) as u8;
                rgba[o + 3] = alpha_b;
            }
        }
    }

    rgba
}

fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

fn psd_err(msg: impl Into<String>) -> IoError {
    IoError::InvalidFormat {
        format: "psd".into(),
        message: msg.into(),
    }
}

fn ensure_within_file<R: Seek>(
    reader: &mut R,
    section_len: u64,
    file_len: u64,
    label: &str,
) -> IoResult<()> {
    let pos = reader.stream_position()?;
    let end = pos
        .checked_add(section_len)
        .ok_or_else(|| psd_err(format!("{label} length overflow")))?;
    if end > file_len {
        return Err(psd_err(format!(
            "{label} ({section_len} bytes at {pos}) exceeds file size {file_len}"
        )));
    }
    Ok(())
}

fn skip_bytes<R: Seek>(reader: &mut R, len: u64) -> IoResult<()> {
    let pos = reader.stream_position()?;
    let target = pos
        .checked_add(len)
        .ok_or_else(|| psd_err("seek offset overflow"))?;
    reader.seek(SeekFrom::Start(target))?;
    Ok(())
}

fn read_u8<R: Read>(reader: &mut R) -> IoResult<u8> {
    let mut b = [0u8; 1];
    reader.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u16_be<R: Read>(reader: &mut R) -> IoResult<u16> {
    let mut b = [0u8; 2];
    reader.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

fn read_i16_be<R: Read>(reader: &mut R) -> IoResult<i16> {
    let mut b = [0u8; 2];
    reader.read_exact(&mut b)?;
    Ok(i16::from_be_bytes(b))
}

fn read_u32_be<R: Read>(reader: &mut R) -> IoResult<u32> {
    let mut b = [0u8; 4];
    reader.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_i32_be<R: Read>(reader: &mut R) -> IoResult<i32> {
    let mut b = [0u8; 4];
    reader.read_exact(&mut b)?;
    Ok(i32::from_be_bytes(b))
}

fn read_u64_be<R: Read>(reader: &mut R) -> IoResult<u64> {
    let mut b = [0u8; 8];
    reader.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}

fn read_section_len<R: Read>(reader: &mut R, is_psb: bool) -> IoResult<u64> {
    if is_psb {
        read_u64_be(reader)
    } else {
        Ok(u64::from(read_u32_be(reader)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{BezierPath, Rect2D};
    use flate2::write::ZlibEncoder;
    use flate2::Compression;

    fn encode_packbits_row(row: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        encode_packbits_row_into(row, &mut out);
        out
    }

    struct SynthLayer<'a> {
        top: i32,
        left: i32,
        bottom: i32,
        right: i32,
        blend: &'a [u8; 4],
        opacity: u8,
        visible: bool,
        pascal_name: &'a str,
        unicode_name: Option<&'a str>,
        channels: Vec<(i16, u16, Vec<u8>)>,
    }

    #[allow(clippy::too_many_arguments)]
    fn build_synthetic_psd(
        is_psb: bool,
        width: u32,
        height: u32,
        channels: u16,
        depth: u16,
        color_mode: u16,
        dpi: Option<f32>,
        layers: &[SynthLayer<'_>],
        composite: Option<(u16, Vec<Vec<u8>>)>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"8BPS");
        out.extend_from_slice(&(if is_psb { 2u16 } else { 1u16 }).to_be_bytes());
        out.extend_from_slice(&[0u8; 6]);
        out.extend_from_slice(&channels.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&depth.to_be_bytes());
        out.extend_from_slice(&color_mode.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());

        let mut ir = Vec::new();
        if let Some(d) = dpi {
            ir.extend_from_slice(b"8BIM");
            ir.extend_from_slice(&1005u16.to_be_bytes());
            ir.extend_from_slice(&[0u8, 0u8]);
            ir.extend_from_slice(&16u32.to_be_bytes());
            let fixed = ((f64::from(d) * 65_536.0).round() as u32).to_be_bytes();
            ir.extend_from_slice(&fixed);
            ir.extend_from_slice(&1u16.to_be_bytes());
            ir.extend_from_slice(&1u16.to_be_bytes());
            ir.extend_from_slice(&fixed);
            ir.extend_from_slice(&1u16.to_be_bytes());
            ir.extend_from_slice(&1u16.to_be_bytes());
        }
        out.extend_from_slice(&(ir.len() as u32).to_be_bytes());
        out.extend_from_slice(&ir);

        let mut li = Vec::new();
        if !layers.is_empty() {
            li.extend_from_slice(&(layers.len() as i16).to_be_bytes());
            let mut encoded_layers: Vec<Vec<(i16, Vec<u8>)>> = Vec::with_capacity(layers.len());
            for layer in layers {
                let lh = (layer.bottom - layer.top).max(0) as usize;
                let lw = (layer.right - layer.left).max(0) as usize;
                let row_bytes = lw * (usize::from(depth) / 8).max(1);
                let mut ch_payloads = Vec::with_capacity(layer.channels.len());
                for (cid, comp, raw) in &layer.channels {
                    let mut payload = Vec::new();
                    payload.extend_from_slice(&comp.to_be_bytes());
                    if *comp == 0 {
                        payload.extend_from_slice(raw);
                    } else if *comp == 1 {
                        let mut enc_rows = Vec::with_capacity(lh);
                        for r in 0..lh {
                            let start = r * row_bytes;
                            let end = (start + row_bytes).min(raw.len());
                            enc_rows.push(encode_packbits_row(&raw[start..end]));
                        }
                        for er in &enc_rows {
                            if is_psb {
                                payload.extend_from_slice(&(er.len() as u32).to_be_bytes());
                            } else {
                                payload.extend_from_slice(&(er.len() as u16).to_be_bytes());
                            }
                        }
                        for er in enc_rows {
                            payload.extend_from_slice(&er);
                        }
                    } else if *comp == 2 || *comp == 3 {
                        let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
                        if *comp == 3 {
                            let mut pred = Vec::with_capacity(raw.len());
                            for r in 0..lh {
                                let start = r * row_bytes;
                                let row = &raw[start..(start + row_bytes).min(raw.len())];
                                let mut prev = 0u8;
                                for &b in row {
                                    pred.push(b.wrapping_sub(prev));
                                    prev = b;
                                }
                            }
                            enc.write_all(&pred).unwrap();
                        } else {
                            enc.write_all(raw).unwrap();
                        }
                        payload.extend_from_slice(&enc.finish().unwrap());
                    }
                    ch_payloads.push((*cid, payload));
                }
                encoded_layers.push(ch_payloads);
            }

            for (layer, ch_payloads) in layers.iter().zip(encoded_layers.iter()) {
                li.extend_from_slice(&layer.top.to_be_bytes());
                li.extend_from_slice(&layer.left.to_be_bytes());
                li.extend_from_slice(&layer.bottom.to_be_bytes());
                li.extend_from_slice(&layer.right.to_be_bytes());
                li.extend_from_slice(&(ch_payloads.len() as u16).to_be_bytes());
                for (cid, payload) in ch_payloads {
                    li.extend_from_slice(&cid.to_be_bytes());
                    if is_psb {
                        li.extend_from_slice(&(payload.len() as u64).to_be_bytes());
                    } else {
                        li.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                    }
                }
                li.extend_from_slice(b"8BIM");
                li.extend_from_slice(layer.blend);
                li.push(layer.opacity);
                li.push(0);
                li.push(if layer.visible { 0 } else { 2 });
                li.push(0);

                let mut extra = Vec::new();
                extra.extend_from_slice(&0u32.to_be_bytes());
                extra.extend_from_slice(&0u32.to_be_bytes());
                let pbytes = layer.pascal_name.as_bytes();
                let plen = pbytes.len().min(255);
                let total_pascal = (1 + plen + 3) & !3;
                extra.push(plen as u8);
                extra.extend_from_slice(&pbytes[..plen]);
                extra.resize(extra.len() + (total_pascal - 1 - plen), 0);

                if let Some(uname) = layer.unicode_name {
                    let units: Vec<u16> = uname.encode_utf16().collect();
                    let luni_len = 4 + units.len() * 2;
                    extra.extend_from_slice(b"8BIMluni");
                    extra.extend_from_slice(&(luni_len as u32).to_be_bytes());
                    extra.extend_from_slice(&(units.len() as u32).to_be_bytes());
                    for u in units {
                        extra.extend_from_slice(&u.to_be_bytes());
                    }
                    if !luni_len.is_multiple_of(2) {
                        extra.push(0);
                    }
                }
                li.extend_from_slice(&(extra.len() as u32).to_be_bytes());
                li.extend_from_slice(&extra);
            }
            for ch_payloads in encoded_layers {
                for (_, payload) in ch_payloads {
                    li.extend_from_slice(&payload);
                }
            }
            if li.len() % 2 != 0 {
                li.push(0);
            }
        }

        let mut lm = Vec::new();
        if !li.is_empty() {
            if is_psb {
                lm.extend_from_slice(&(li.len() as u64).to_be_bytes());
            } else {
                lm.extend_from_slice(&(li.len() as u32).to_be_bytes());
            }
            lm.extend_from_slice(&li);
            if is_psb {
                lm.extend_from_slice(&0u64.to_be_bytes());
            } else {
                lm.extend_from_slice(&0u32.to_be_bytes());
            }
        }
        if is_psb {
            out.extend_from_slice(&(lm.len() as u64).to_be_bytes());
        } else {
            out.extend_from_slice(&(lm.len() as u32).to_be_bytes());
        }
        out.extend_from_slice(&lm);

        if let Some((comp_mode, planes)) = composite {
            out.extend_from_slice(&comp_mode.to_be_bytes());
            if comp_mode == 0 {
                for p in planes {
                    out.extend_from_slice(&p);
                }
            } else if comp_mode == 1 {
                let row_bytes = (width as usize) * (usize::from(depth) / 8).max(1);
                let mut enc_rows = Vec::new();
                for p in &planes {
                    for r in 0..(height as usize) {
                        let s = r * row_bytes;
                        let e = (s + row_bytes).min(p.len());
                        enc_rows.push(encode_packbits_row(&p[s..e]));
                    }
                }
                for er in &enc_rows {
                    if is_psb {
                        out.extend_from_slice(&(er.len() as u32).to_be_bytes());
                    } else {
                        out.extend_from_slice(&(er.len() as u16).to_be_bytes());
                    }
                }
                for er in enc_rows {
                    out.extend_from_slice(&er);
                }
            }
        } else {
            out.extend_from_slice(&0u16.to_be_bytes());
        }

        out
    }

    #[test]
    fn psd_multilayer_rle_zip_blend_dpi_and_unicode_luni_import() {
        let mut r0 = Vec::new();
        let mut g0 = Vec::new();
        let mut b0 = Vec::new();
        for _ in 0..16 {
            r0.extend_from_slice(&200u16.wrapping_mul(257).to_be_bytes());
            g0.extend_from_slice(&100u16.wrapping_mul(257).to_be_bytes());
            b0.extend_from_slice(&50u16.wrapping_mul(257).to_be_bytes());
        }
        let l0 = SynthLayer {
            top: 0,
            left: 0,
            bottom: 4,
            right: 4,
            blend: b"norm",
            opacity: 255,
            visible: true,
            pascal_name: "Back",
            unicode_name: Some("خلفية"),
            channels: vec![(0, 0, r0), (1, 0, g0), (2, 0, b0)],
        };
        let l1 = SynthLayer {
            top: 2,
            left: 2,
            bottom: 6,
            right: 6,
            blend: b"mul ",
            opacity: 128,
            visible: true,
            pascal_name: "Top",
            unicode_name: None,
            channels: vec![
                (0, 1, vec![255; 32]),
                (1, 1, vec![0; 32]),
                (2, 1, vec![0; 32]),
            ],
        };
        let bytes = build_synthetic_psd(false, 10, 10, 3, 16, 3, Some(300.0), &[l0, l1], None);
        let doc = import_psd_bytes(&bytes, "Test16").unwrap();
        assert!((doc.dpi() - 300.0).abs() < 0.01);
        assert_eq!(doc.pages()[0].layers.len(), 2);
        assert_eq!(doc.pages()[0].layers[0].name, "خلفية");
        let o0 = &doc.objects()[&doc.pages()[0].layers[0].objects[0]];
        let NodePayload::Raster(r) = &o0.payload else {
            panic!("expected raster");
        };
        let asset = &doc.assets()[&r.asset_id];
        assert_eq!(asset.pixel_depth, PixelDepth::Bit16);
        assert_eq!(asset.to_rgba16_vec()[0], 200 * 257);
    }

    #[test]
    fn psd_cmyk_composite_rle_and_bad_magic_guard() {
        let comp = vec![
            vec![255; 4], // C=0%
            vec![0; 4],   // M=100%
            vec![255; 4], // Y=0%
            vec![255; 4], // K=0%
        ];
        let bytes = build_synthetic_psd(false, 2, 2, 4, 8, 4, Some(150.0), &[], Some((1, comp)));
        let doc = import_psd_bytes(&bytes, "CMYK Comp").unwrap();
        assert_eq!(doc.color_mode(), ColorMode::Cmyk);
        let obj = doc.objects().values().next().unwrap();
        let NodePayload::Raster(r) = &obj.payload else {
            panic!("expected raster");
        };
        let asset = &doc.assets()[&r.asset_id];
        assert_eq!(&asset.to_rgba8_vec()[0..4], &[255, 0, 255, 255]);

        let mut bad_dim = vec![0u8; 38];
        bad_dim[0..4].copy_from_slice(b"8BPS");
        bad_dim[5] = 1;
        bad_dim[13] = 3;
        bad_dim[23] = 8;
        bad_dim[25] = 3;
        assert!(import_psd_bytes(&bad_dim, "zero").is_err());
    }

    #[test]
    fn psd_preserves_4096_native_layer_and_downscales_above_16384() {
        let big4096 = SynthLayer {
            top: 0,
            left: 0,
            bottom: 4,
            right: 4096,
            blend: b"norm",
            opacity: 255,
            visible: true,
            pascal_name: "Native4096",
            unicode_name: None,
            channels: vec![(0, 1, vec![200u8; 4096 * 4])],
        };
        let bytes = build_synthetic_psd(false, 4096, 4, 1, 8, 1, Some(254.0), &[big4096], None);
        let doc = import_psd_bytes(&bytes, "Native 4096").unwrap();
        let obj = doc.objects().values().next().unwrap();
        let NodePayload::Raster(r) = &obj.payload else {
            panic!("expected raster");
        };
        assert!((r.width_mm - 409.6).abs() < 0.05);
        assert!((r.height_mm - 0.4).abs() < 0.05);
        let asset = &doc.assets()[&r.asset_id];
        assert_eq!(asset.width, 4096);
        assert_eq!(asset.height, 4);

        let huge = SynthLayer {
            top: 0,
            left: 0,
            bottom: 2,
            right: 20_000,
            blend: b"norm",
            opacity: 255,
            visible: true,
            pascal_name: "WideBanner",
            unicode_name: None,
            channels: vec![(0, 1, vec![200u8; 20_000 * 2])],
        };
        let psb_bytes = build_synthetic_psd(true, 20_000, 2, 1, 8, 1, Some(254.0), &[huge], None);
        let psb_doc = import_psd_bytes(&psb_bytes, "Wide PSB").unwrap();
        let o2 = psb_doc.objects().values().next().unwrap();
        let NodePayload::Raster(r2) = &o2.payload else {
            panic!("expected raster");
        };
        let a2 = &psb_doc.assets()[&r2.asset_id];
        assert!(a2.width <= MAX_LAYER_OUTPUT_DIM);
    }

    #[test]
    fn psd_multilayer_with_user_mask_and_spot_channel_does_not_desync_next_layer() {
        let l0 = SynthLayer {
            top: 0,
            left: 0,
            bottom: 2,
            right: 2,
            blend: b"norm",
            opacity: 255,
            visible: true,
            pascal_name: "MaskedBase",
            unicode_name: None,
            channels: vec![
                (0, 0, vec![200; 4]),
                (1, 0, vec![10; 4]),
                (2, 0, vec![20; 4]),
                (-2, 0, vec![128, 255, 0, 255]), // user layer mask modulates alpha!
                (4, 0, vec![99; 4]),             // spot channel registered cleanly!
            ],
        };
        let l1 = SynthLayer {
            top: 0,
            left: 0,
            bottom: 2,
            right: 2,
            blend: b"norm",
            opacity: 255,
            visible: true,
            pascal_name: "TopGreen",
            unicode_name: None,
            channels: vec![
                (0, 1, vec![15; 4]),
                (1, 1, vec![230; 4]),
                (2, 1, vec![45; 4]),
            ],
        };
        let bytes = build_synthetic_psd(false, 2, 2, 3, 8, 3, Some(150.0), &[l0, l1], None);
        let doc = import_psd_bytes(&bytes, "Masked Multi").unwrap();
        assert_eq!(doc.pages()[0].layers.len(), 2);

        let base_obj = &doc.objects()[&doc.pages()[0].layers[0].objects[0]];
        let NodePayload::Raster(rb) = &base_obj.payload else {
            panic!("expected raster");
        };
        let base_px = doc.assets()[&rb.asset_id].to_rgba8_vec();
        assert_eq!(base_px[3], 128, "-2 mask pixel 0 must modulate alpha");
        assert_eq!(base_px[11], 0, "-2 mask pixel 2 must mask alpha to 0");

        let top_obj = &doc.objects()[&doc.pages()[0].layers[1].objects[0]];
        let NodePayload::Raster(r) = &top_obj.payload else {
            panic!("expected raster");
        };
        let asset = &doc.assets()[&r.asset_id];
        assert_eq!(&asset.to_rgba8_vec()[0..4], &[15, 230, 45, 255]);
        assert!(doc
            .document_palette()
            .swatches
            .iter()
            .any(|s| matches!(&s.color, Color::Spot { .. })));
    }

    #[test]
    fn psd_export_roundtrips_16bit_groups_spot_resources_and_zip_prediction() {
        let mut doc = Document::new("PSD Export Test", 20.0, 20.0, ColorMode::Rgb, 72.0);
        doc.document_palette_mut()
            .add_swatch(ColorSwatch::new("CutContour", Color::cut_contour_spot()));
        let mut child = SceneObject::new(
            ObjectId(0),
            "VectorInGroup",
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(
                2.0, 2.0, 10.0, 10.0,
            ))),
        )
        .with_fill(Fill::Solid(Color::rgba8(220, 40, 40, 255)));
        let cid = doc
            .insert_loaded_object(std::mem::replace(
                &mut child,
                SceneObject::new(ObjectId(0), "tmp", NodePayload::Group(vec![])),
            ))
            .unwrap();
        let grp = SceneObject::new(ObjectId(0), "MyFolder", NodePayload::Group(vec![cid]));
        doc.add_object(grp, None).unwrap();

        let asset16 = RasterAsset::new_16bit(
            4,
            4,
            [60_000, 30_000, 10_000, 65_535].repeat(16),
            72.0,
            ColorMode::Rgb,
        )
        .unwrap();
        let aid16 = doc.register_asset(asset16);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Layer16",
                NodePayload::Raster(RasterLayerData {
                    asset_id: aid16,
                    width_mm: 4.0 * 25.4 / 72.0,
                    height_mm: 4.0 * 25.4 / 72.0,
                    filters: vec![],
                    crop_rect: None,
                }),
            ),
            None,
        )
        .unwrap();

        let psd_bytes = export_psd_to_bytes(&doc, 0).unwrap();
        assert!(psd_bytes.starts_with(b"8BPS"));
        let back = import_psd_bytes(&psd_bytes, "Back").unwrap();
        assert!(back
            .objects()
            .values()
            .any(|o| matches!(&o.payload, NodePayload::Group(c) if !c.is_empty())));
        assert!(back
            .assets()
            .values()
            .any(|a| a.pixel_depth == PixelDepth::Bit16));
        assert!(back
            .document_palette()
            .swatches
            .iter()
            .any(|s| matches!(&s.color, Color::Spot { name, .. } if name == "CutContour")));
    }

    #[test]
    fn psd_crafted_composite_rle_table_overflow_is_rejected_without_oom() {
        let mut crafted = Vec::new();
        crafted.extend_from_slice(b"8BPS");
        crafted.extend_from_slice(&2u16.to_be_bytes());
        crafted.extend_from_slice(&[0u8; 6]);
        crafted.extend_from_slice(&56u16.to_be_bytes());
        crafted.extend_from_slice(&300_000u32.to_be_bytes());
        crafted.extend_from_slice(&1u32.to_be_bytes());
        crafted.extend_from_slice(&8u16.to_be_bytes());
        crafted.extend_from_slice(&3u16.to_be_bytes());
        crafted.extend_from_slice(&0u32.to_be_bytes());
        crafted.extend_from_slice(&0u32.to_be_bytes());
        crafted.extend_from_slice(&0u64.to_be_bytes());
        crafted.extend_from_slice(&1u16.to_be_bytes());

        let err = import_psd_bytes(&crafted, "OOM PSB").unwrap_err();
        assert!(matches!(err, IoError::InvalidFormat { .. }));
    }

    #[test]
    fn psd_rejects_packbits_overrun_and_zip_bomb_payload() {
        let mut overrun = Vec::new();
        overrun.extend_from_slice(b"8BPS");
        overrun.extend_from_slice(&1u16.to_be_bytes());
        overrun.extend_from_slice(&[0u8; 6]);
        overrun.extend_from_slice(&1u16.to_be_bytes());
        overrun.extend_from_slice(&2u32.to_be_bytes());
        overrun.extend_from_slice(&16u32.to_be_bytes());
        overrun.extend_from_slice(&8u16.to_be_bytes());
        overrun.extend_from_slice(&1u16.to_be_bytes()); // Grayscale
        overrun.extend_from_slice(&[0u8; 12]);
        overrun.extend_from_slice(&1u16.to_be_bytes()); // PackBits
        overrun.extend_from_slice(&65000u16.to_be_bytes());
        overrun.extend_from_slice(&65000u16.to_be_bytes());
        overrun.extend_from_slice(&[0x81, 0xFF]);
        assert!(import_psd_bytes(&overrun, "PackBitsOverrun").is_err());

        let mut zip_bomb = Vec::new();
        zip_bomb.extend_from_slice(b"8BPS");
        zip_bomb.extend_from_slice(&1u16.to_be_bytes());
        zip_bomb.extend_from_slice(&[0u8; 6]);
        zip_bomb.extend_from_slice(&1u16.to_be_bytes());
        zip_bomb.extend_from_slice(&64u32.to_be_bytes());
        zip_bomb.extend_from_slice(&64u32.to_be_bytes());
        zip_bomb.extend_from_slice(&8u16.to_be_bytes());
        zip_bomb.extend_from_slice(&1u16.to_be_bytes());
        zip_bomb.extend_from_slice(&[0u8; 12]);
        zip_bomb.extend_from_slice(&2u16.to_be_bytes()); // ZIP
        let big = vec![0u8; 128 * 1024];
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut enc, &big).unwrap();
        zip_bomb.extend_from_slice(&enc.finish().unwrap());
        assert!(import_psd_bytes(&zip_bomb, "ZipOverflow").is_err());
    }

    #[test]
    fn psd_psb_v2_export_roundtrips_cmyk_and_vector_shapes() {
        let mut doc = Document::new("PSB v2 CMYK", 50.0, 50.0, ColorMode::Cmyk, 72.0);
        doc.add_object(
            SceneObject::new(
                ObjectId(0),
                "Cyan Rect",
                NodePayload::Shape(coree_core::ParametricShape::Rectangle {
                    width: 20.0,
                    height: 20.0,
                    corner_radii: [0.0; 4],
                    corner_style: coree_core::CornerStyle::Rounded,
                    lock_corners: true,
                }),
            )
            .with_transform(Affine2D::translate(10.0, 10.0))
            .with_fill(Fill::Solid(Color::cmyk(1.0, 0.0, 0.0, 0.0))),
            None,
        )
        .unwrap();

        let psb = export_psd_to_bytes_with_options(
            &doc,
            0,
            &PsdExportOptions {
                is_psb: true,
                dpi: Some(72.0),
            },
        )
        .unwrap();
        assert_eq!(&psb[0..6], b"8BPS\x00\x02");
        let back = import_psd_bytes(&psb, "PSB Back").unwrap();
        assert_eq!(back.color_mode(), ColorMode::Cmyk);
        assert!(!back.assets().is_empty());
    }

    #[test]
    fn psd_composite_16bit_grayscale_and_lab_decode_cleanly() {
        // 2x2 16-bit Grayscale raw composite PSD
        let mut gray16 = Vec::new();
        gray16.extend_from_slice(b"8BPS");
        gray16.extend_from_slice(&1u16.to_be_bytes());
        gray16.extend_from_slice(&[0u8; 6]);
        gray16.extend_from_slice(&1u16.to_be_bytes());
        gray16.extend_from_slice(&2u32.to_be_bytes());
        gray16.extend_from_slice(&2u32.to_be_bytes());
        gray16.extend_from_slice(&16u16.to_be_bytes());
        gray16.extend_from_slice(&1u16.to_be_bytes()); // Grayscale
        gray16.extend_from_slice(&[0u8; 12]);
        gray16.extend_from_slice(&0u16.to_be_bytes()); // Raw
        for v in [0u16, 20_000, 40_000, 65_535] {
            gray16.extend_from_slice(&v.to_be_bytes());
        }
        let doc = import_psd_bytes(&gray16, "Gray16").unwrap();
        let asset = doc.assets().values().next().unwrap();
        assert_eq!(asset.pixel_depth, PixelDepth::Bit16);
        assert_eq!((asset.width, asset.height), (2, 2));
    }

    #[test]
    fn psd_multiplies_raster_and_vector_masks_for_8bit_and_16bit_layers() {
        // 8-bit 2x2 layer with both -2 raster mask and -3 vector mask
        let l8 = SynthLayer {
            top: 0,
            left: 0,
            bottom: 2,
            right: 2,
            blend: b"norm",
            opacity: 255,
            visible: true,
            pascal_name: "DualMask8",
            unicode_name: None,
            channels: vec![
                (0, 0, vec![200; 4]),
                (1, 0, vec![100; 4]),
                (2, 0, vec![50; 4]),
                (-2, 0, vec![255, 255, 0, 128]),
                (-3, 0, vec![128, 0, 255, 128]),
            ],
        };
        let bytes8 = build_synthetic_psd(false, 2, 2, 3, 8, 3, Some(72.0), &[l8], None);
        let doc8 = import_psd_bytes(&bytes8, "DualMask8").unwrap();
        let asset8 = doc8.assets().values().next().unwrap().to_rgba8_vec();
        // px0: 255 * 128 / 255 = 128; px1: 255 * 0 = 0; px2: 0 * 255 = 0; px3: 128 * 128 / 255 = 64
        assert_eq!(asset8[3], 128);
        assert_eq!(asset8[7], 0);
        assert_eq!(asset8[11], 0);
        assert_eq!(asset8[15], 64);

        // 16-bit 2x2 layer with both -2 raster mask and -3 vector mask
        let be16 = |vals: [u16; 4]| -> Vec<u8> {
            vals.into_iter().flat_map(|v| v.to_be_bytes()).collect()
        };
        let l16 = SynthLayer {
            top: 0,
            left: 0,
            bottom: 2,
            right: 2,
            blend: b"norm",
            opacity: 255,
            visible: true,
            pascal_name: "DualMask16",
            unicode_name: None,
            channels: vec![
                (0, 0, be16([50_000; 4])),
                (1, 0, be16([30_000; 4])),
                (2, 0, be16([10_000; 4])),
                (-2, 0, be16([65_535, 65_535, 0, 32_768])),
                (-3, 0, be16([32_768, 0, 65_535, 32_768])),
            ],
        };
        let bytes16 = build_synthetic_psd(false, 2, 2, 3, 16, 3, Some(72.0), &[l16], None);
        let doc16 = import_psd_bytes(&bytes16, "DualMask16").unwrap();
        let asset16 = doc16.assets().values().next().unwrap().to_rgba16_vec();
        // px0 alpha: ~32_768; px1 alpha: 0; px2 alpha: 0; px3 alpha: (32768*32768)/65535 = ~16384
        assert!((i32::from(asset16[3]) - 32_768).abs() <= 1);
        assert_eq!(asset16[7], 0);
        assert_eq!(asset16[11], 0);
        assert!((i32::from(asset16[15]) - 16_384).abs() <= 1);
    }
}
