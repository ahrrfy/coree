// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room pure-Rust Adobe Photoshop `.psd` (v1) and `.psb` (v2) multi-layer importer.
//!
//! Features:
//! - Streaming `Read + Seek` parser so multi-hundred-megabyte PSD files never allocate
//!   a full copy of the raw file or full-resolution uncompressed layers in memory.
//! - Extracts physical DPI from Image Resource `1005` (`ResolutionInfo`).
//! - Decodes multi-layer records (bounds, channel IDs `0/1/2/3/-1`, blend modes mapped to
//!   [`coree_core::BlendMode`], opacity, visibility flag, Pascal + Unicode `luni` names).
//! - Supports RAW (`0`), PackBits RLE (`1`), ZIP (`2`), and ZIP-with-prediction (`3`)
//!   compression in 1-bit Bitmap, 8-bit, and 16-bit (downsampled to 8-bit) depths across
//!   Bitmap, Grayscale, Indexed, RGB, CMYK (Photoshop inverted channels -> sRGB), and Lab.
//! - Enforces strict cumulative decoded RGBA RAM budgets (`256 MiB`) with automatic
//!   streaming box downscaling when any layer exceeds `2048 px` on its longest axis, while
//!   preserving exact physical millimeter dimensions and placement.
//! - Falls back to Section 5 Composite Image Data when `layer_count == 0` or flattened.

use crate::error::{IoError, IoResult};
use coree_core::{
    Affine2D, BlendMode, Color, ColorMode, Document, Fill, MasterLayerScope, NodePayload, ObjectId,
    OutlineStyle, RasterAsset, RasterLayerData, SceneObject,
};
use flate2::read::ZlibDecoder;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

/// Maximum header canvas dimension accepted (PSB spec ceiling = 300,000 px).
const MAX_PSD_DIMENSION: u32 = 300_000;
/// Maximum number of layers accepted in a single PSD/PSB document.
const MAX_PSD_LAYERS: usize = 4_096;
/// Maximum output pixel dimension per imported layer/composite (larger layers are
/// stream-downscaled with a box filter while preserving exact millimeter placement).
const MAX_LAYER_OUTPUT_DIM: u32 = 2_048;
/// Strict cumulative decoded RGBA byte budget across all imported layers (`256 MiB`).
const MAX_CUMULATIVE_RGBA_BYTES: usize = 256 * 1024 * 1024;
/// Maximum compressed/decompressed bytes allowed for a single ZIP channel stream (`64 MiB`).
const MAX_ZIP_CHANNEL_BYTES: usize = 64 * 1024 * 1024;
/// Maximum single Image Resources section scanned in memory (`16 MiB`).
const MAX_IMAGE_RESOURCES_SCAN: u64 = 16 * 1024 * 1024;

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

#[derive(Debug, Clone)]
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
    is_section_divider: bool,
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
    // Section 3: Image Resources (extract ResolutionInfo resource 1005 for DPI)
    // -------------------------------------------------------------------------
    let img_res_len = u64::from(read_u32_be(&mut reader)?);
    ensure_within_file(&mut reader, img_res_len, file_len, "image resources")?;
    let img_res_start = reader.stream_position()?;
    let img_res_end = img_res_start
        .checked_add(img_res_len)
        .ok_or_else(|| psd_err("image resources section offset overflow"))?;
    let dpi = parse_resolution_info_dpi(&mut reader, img_res_len)?;
    reader.seek(SeekFrom::Start(img_res_end))?;

    // Compute exact physical page dimensions in millimeters from pixel size and DPI.
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

            // Decode each layer's channel payloads sequentially.
            for (idx, rec) in records.iter().enumerate() {
                let lw = rec.width();
                let lh = rec.height();

                // Record channel start offsets so we always stay aligned even when skipping a layer.
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

                if rec.is_section_divider || lw == 0 || lh == 0 {
                    continue;
                }

                let Some((dst_w, dst_h)) =
                    compute_target_dimensions(lw, lh, MAX_LAYER_OUTPUT_DIM, budget_remaining)
                else {
                    // Cumulative RGBA budget exhausted; skip remaining pixel decoding safely.
                    continue;
                };

                let decoded = decode_layer_rgba(
                    &mut reader,
                    &header,
                    indexed_palette.as_deref(),
                    lw,
                    lh,
                    dst_w,
                    dst_h,
                    &channel_positions,
                )?;
                // Always restore stream cursor to the exact end of this layer's channels so
                // skipped masks (-2/-3), spot channels (>=4), or padded channels never desync Layer i+1.
                reader.seek(SeekFrom::Start(layer_end))?;

                let Some(rgba) = decoded else {
                    continue;
                };

                budget_remaining = budget_remaining.saturating_sub(rgba.len());

                let asset =
                    RasterAsset::new(dst_w, dst_h, rgba, dpi, header.color_mode.doc_color_mode())?;
                let asset_id = doc.register_asset(asset);

                let layer_name = if rec.name.trim().is_empty() {
                    format!("Layer {}", idx + 1)
                } else {
                    rec.name.clone()
                };
                let lid =
                    doc.add_layer(page_id, layer_name.clone(), false, MasterLayerScope::None)?;
                if let Some(layer) = doc.find_layer_mut(lid) {
                    layer.visible = rec.visible;
                }

                let left_mm = f64::from(rec.left) * 25.4 / dpi_f64;
                let top_mm = f64::from(rec.top) * 25.4 / dpi_f64;
                let width_mm = (f64::from(lw) * 25.4 / dpi_f64).max(0.01);
                let height_mm = (f64::from(lh) * 25.4 / dpi_f64).max(0.01);

                let mut obj = SceneObject::new(
                    ObjectId(0),
                    layer_name,
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

                doc.add_object(obj, Some(lid))?;
                imported_layers += 1;
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
        ) {
            if let Some(rgba) = decode_composite_rgba(
                &mut reader,
                file_len,
                &header,
                indexed_palette.as_deref(),
                composite_has_alpha,
                dst_w,
                dst_h,
            )? {
                let asset =
                    RasterAsset::new(dst_w, dst_h, rgba, dpi, header.color_mode.doc_color_mode())?;
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

    // Remove the empty placeholder "Layer 1" when PSD layers were imported.
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
    Ok(doc)
}

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

fn parse_resolution_info_dpi<R: Read + Seek>(reader: &mut R, section_len: u64) -> IoResult<f32> {
    let start = reader.stream_position()?;
    let end = start.saturating_add(section_len);
    let scan_limit = start.saturating_add(section_len.min(MAX_IMAGE_RESOURCES_SCAN));

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
        // Pascal string padded to even total length including the length byte.
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

        // Resource ID 1005 (0x03ED) = ResolutionInfo
        if res_id == 1005 && data_len >= 16 {
            let mut info = [0u8; 16];
            reader.read_exact(&mut info)?;
            let h_res_fixed = u32::from_be_bytes([info[0], info[1], info[2], info[3]]);
            let v_res_fixed = u32::from_be_bytes([info[8], info[9], info[10], info[11]]);
            // Per Adobe PSD spec, hRes and vRes are 16.16 fixed-point pixels per inch.
            let h_dpi = f64::from(h_res_fixed) / 65_536.0;
            let v_dpi = f64::from(v_res_fixed) / 65_536.0;
            let dpi = if h_dpi.is_finite() && (1.0..=100_000.0).contains(&h_dpi) {
                h_dpi
            } else if v_dpi.is_finite() && (1.0..=100_000.0).contains(&v_dpi) {
                v_dpi
            } else {
                72.0
            };
            return Ok(dpi as f32);
        }

        let next_pos = data_start.saturating_add(padded_len).min(end);
        reader.seek(SeekFrom::Start(next_pos))?;
    }

    Ok(72.0)
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
    // Per PSD spec, bit 1 (0x02) of layer flags indicates the layer is hidden.
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
    let mut is_section_divider = false;

    if extra_len >= 8 {
        // 1. Layer mask data
        let mask_len = u64::from(read_u32_be(reader)?);
        if reader.stream_position()?.saturating_add(mask_len) <= extra_end {
            skip_bytes(reader, mask_len)?;
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

        // 4. Additional Layer Information blocks (extract `luni` Unicode name & `lsct`/`lsdk`)
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
                // 1 = open folder, 2 = closed folder, 3 = bounding section divider
                if matches!(div_type, 1..=3) {
                    is_section_divider = true;
                }
            }

            // Additional layer blocks inside layer extra data are padded to 2- or 4-byte boundaries;
            // Photoshop pads to even byte boundary (2 bytes).
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
        is_section_divider,
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

/// Computes target `(dst_w, dst_h)` respecting both `max_dim` and `budget_remaining` RGBA bytes.
fn compute_target_dimensions(
    src_w: u32,
    src_h: u32,
    max_dim: u32,
    budget_remaining: usize,
) -> Option<(u32, u32)> {
    if src_w == 0 || src_h == 0 || budget_remaining < 4 {
        return None;
    }
    let mut scale = 1.0_f64;
    let longest = src_w.max(src_h);
    if longest > max_dim {
        scale = f64::from(max_dim) / f64::from(longest);
    }
    let mut dst_w = ((f64::from(src_w) * scale).round() as u32).clamp(1, max_dim);
    let mut dst_h = ((f64::from(src_h) * scale).round() as u32).clamp(1, max_dim);

    let max_pixels = budget_remaining / 4;
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
fn decode_layer_rgba<R: Read + Seek>(
    reader: &mut R,
    header: &PsdHeader,
    indexed_palette: Option<&[u8]>,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    channels: &[(i16, u64, u64)],
) -> IoResult<Option<Vec<u8>>> {
    let mut ch0: Option<Vec<u8>> = None;
    let mut ch1: Option<Vec<u8>> = None;
    let mut ch2: Option<Vec<u8>> = None;
    let mut ch3: Option<Vec<u8>> = None;
    let mut alpha: Option<Vec<u8>> = None;

    for &(ch_id, pos, data_len) in channels {
        if !matches!(ch_id, -1..=3) {
            continue;
        }
        // Only decode channels relevant to the document color mode (+ alpha -1).
        let needed = match header.color_mode {
            PsdColorMode::Bitmap
            | PsdColorMode::Grayscale
            | PsdColorMode::Indexed
            | PsdColorMode::Duotone
            | PsdColorMode::Multichannel => matches!(ch_id, -1 | 0),
            PsdColorMode::Rgb | PsdColorMode::Lab => matches!(ch_id, -1..=2),
            PsdColorMode::Cmyk => matches!(ch_id, -1..=3),
        };
        if !needed || data_len < 2 {
            continue;
        }

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
            src_w,
            src_h,
            dst_w,
            dst_h,
        )?;

        match ch_id {
            0 => ch0 = Some(plane),
            1 => ch1 = Some(plane),
            2 => ch2 = Some(plane),
            3 => ch3 = Some(plane),
            -1 => alpha = Some(plane),
            _ => {}
        }
    }

    if ch0.is_none() && alpha.is_none() {
        return Ok(None);
    }

    Ok(Some(assemble_rgba_planes(
        header.color_mode,
        indexed_palette,
        dst_w,
        dst_h,
        ch0.as_deref(),
        ch1.as_deref(),
        ch2.as_deref(),
        ch3.as_deref(),
        alpha.as_deref(),
    )))
}

#[allow(clippy::too_many_arguments)]
fn decode_composite_rgba<R: Read + Seek>(
    reader: &mut R,
    file_len: u64,
    header: &PsdHeader,
    indexed_palette: Option<&[u8]>,
    has_alpha: bool,
    dst_w: u32,
    dst_h: u32,
) -> IoResult<Option<Vec<u8>>> {
    let comp_start = reader.stream_position()?;
    if comp_start + 2 > file_len {
        return Ok(None);
    }
    let compression = read_u16_be(reader)?;
    let num_channels = usize::from(header.channels);
    let src_w = header.width;
    let src_h = header.height;

    let mut ch0: Option<Vec<u8>> = None;
    let mut ch1: Option<Vec<u8>> = None;
    let mut ch2: Option<Vec<u8>> = None;
    let mut ch3: Option<Vec<u8>> = None;
    let mut alpha: Option<Vec<u8>> = None;

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

    match compression {
        0 => {
            // RAW planar composite data
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
            // PackBits RLE: ALL row byte counts across ALL channels come first!
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

    Ok(Some(assemble_rgba_planes(
        header.color_mode,
        indexed_palette,
        dst_w,
        dst_h,
        ch0.as_deref(),
        ch1.as_deref(),
        ch2.as_deref(),
        ch3.as_deref(),
        alpha.as_deref(),
    )))
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
            // ZIP without prediction (2) or ZIP with horizontal differencing prediction (3)
            if payload_len > (MAX_ZIP_CHANNEL_BYTES as u64) {
                return Err(IoError::LimitExceeded {
                    limit: format!("max_zip_channel_bytes={MAX_ZIP_CHANNEL_BYTES}"),
                    actual: format!("{payload_len} bytes"),
                });
            }
            let mut compressed = vec![0u8; payload_len as usize];
            reader.read_exact(&mut compressed)?;

            let expected_raw = row_bytes
                .checked_mul(src_h as usize)
                .ok_or_else(|| psd_err("ZIP channel byte size overflow"))?;
            if expected_raw > MAX_ZIP_CHANNEL_BYTES {
                return Err(IoError::LimitExceeded {
                    limit: format!("max_zip_channel_bytes={MAX_ZIP_CHANNEL_BYTES}"),
                    actual: format!("{expected_raw} uncompressed bytes"),
                });
            }

            let mut decoder = ZlibDecoder::new(compressed.as_slice());
            let mut raw = Vec::with_capacity(expected_raw.min(1024 * 1024));
            let mut chunk = [0u8; 16 * 1024];
            loop {
                let n = decoder.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                if raw.len().saturating_add(n) > MAX_ZIP_CHANNEL_BYTES {
                    return Err(IoError::ZipBomb {
                        reason: format!(
                            "PSD ZIP channel exceeds {MAX_ZIP_CHANNEL_BYTES} bytes limit"
                        ),
                    });
                }
                raw.extend_from_slice(&chunk[..n]);
            }

            if raw.len() < expected_raw {
                raw.resize(expected_raw, 0);
            }

            if compression == 3 {
                // Reverse horizontal differencing prediction per row.
                for row in raw.chunks_exact_mut(row_bytes) {
                    if depth == 16 {
                        let mut acc = 0u16;
                        for px in row.as_chunks_mut::<2>().0 {
                            let d = u16::from_be_bytes([px[0], px[1]]);
                            acc = acc.wrapping_add(d);
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
                }
            }

            let mut cursor = Cursor::new(raw.as_slice());
            stream_decode_raw_plane(&mut cursor, src_w, src_h, dst_w, dst_h, depth, row_bytes)
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

/// Streaming box-downscaling (or 1:1 direct copy) for uncompressed planar scanlines.
fn stream_decode_raw_plane<R: Read>(
    reader: &mut R,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    depth: u16,
    row_bytes: usize,
) -> IoResult<Vec<u8>> {
    let mut raw_row = vec![0u8; row_bytes];
    let mut u8_row = vec![0u8; src_w as usize];

    if dst_w == src_w && dst_h == src_h {
        let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize)];
        for y in 0..(src_h as usize) {
            reader.read_exact(&mut raw_row)?;
            unpack_scanline_to_u8(&raw_row, depth, &mut u8_row);
            let dst_off = y * (dst_w as usize);
            out[dst_off..dst_off + (dst_w as usize)].copy_from_slice(&u8_row);
        }
        return Ok(out);
    }

    let mut acc = BoxAccumulator::new(src_w, src_h, dst_w, dst_h);
    for y in 0..(src_h as usize) {
        reader.read_exact(&mut raw_row)?;
        unpack_scanline_to_u8(&raw_row, depth, &mut u8_row);
        acc.push_row(y, &u8_row);
    }
    Ok(acc.finish())
}

/// Streaming PackBits RLE decoder with optional box-downscaling per scanline.
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

/// Decodes a single PackBits-compressed scanline into `dst` (zero-padding any short tail).
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
        // -128 is an explicit PackBits no-op.
    }
}

/// Converts a decoded 1-bit, 8-bit, or 16-bit big-endian scanline into 8-bit samples.
fn unpack_scanline_to_u8(raw_row: &[u8], depth: u16, out: &mut [u8]) {
    match depth {
        8 => {
            let n = raw_row.len().min(out.len());
            out[..n].copy_from_slice(&raw_row[..n]);
        }
        16 => {
            // Big-endian u16 -> take high byte (exact >> 8 downsampling).
            for (dst, pair) in out.iter_mut().zip(raw_row.as_chunks::<2>().0) {
                *dst = pair[0];
            }
        }
        1 => {
            // PSD 1-bit Bitmap: 0 = white (255), 1 = black (0), MSB first.
            for (x, dst) in out.iter_mut().enumerate() {
                let byte = raw_row.get(x / 8).copied().unwrap_or(0);
                let bit = (byte >> (7 - (x % 8))) & 1;
                *dst = if bit == 0 { 255 } else { 0 };
            }
        }
        _ => out.fill(0),
    }
}

/// Streaming box-filter downscaler avoiding full-resolution channel allocation.
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

    fn finish(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.sums.len());
        for (sum, cnt) in self.sums.into_iter().zip(self.counts) {
            if cnt == 0 {
                out.push(0);
            } else {
                let c = u64::from(cnt);
                out.push(((sum + c / 2) / c).min(255) as u8);
            }
        }
        out
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
            // In Adobe Photoshop PSD/PSB files, CMYK channels are stored inverted:
            // 255 = 0% ink, 0 = 100% ink, so (1 - C) = raw_c / 255 and (1 - K) = raw_k / 255.
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

    /// Simple PackBits RLE encoder used by synthetic PSD unit tests.
    fn encode_packbits_row(row: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < row.len() {
            // Check for run of >= 3 identical bytes
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
                    let mut ahead = 1;
                    while i + ahead < row.len() && ahead < 3 && row[i + ahead] == row[i] {
                        ahead += 1;
                    }
                    if ahead >= 3 && lit > 0 {
                        break;
                    }
                    i += 1;
                    lit += 1;
                }
                out.push((lit - 1) as u8);
                out.extend_from_slice(&row[start..start + lit]);
            }
        }
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
        channels: Vec<(i16, u16, Vec<u8>)>, // (channel_id, compression, raw_uncompressed_bytes)
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

        // 1. Header
        out.extend_from_slice(b"8BPS");
        out.extend_from_slice(&(if is_psb { 2u16 } else { 1u16 }).to_be_bytes());
        out.extend_from_slice(&[0u8; 6]);
        out.extend_from_slice(&channels.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&depth.to_be_bytes());
        out.extend_from_slice(&color_mode.to_be_bytes());

        // 2. Color Mode Data (empty)
        out.extend_from_slice(&0u32.to_be_bytes());

        // 3. Image Resources
        let mut ir = Vec::new();
        if let Some(d) = dpi {
            ir.extend_from_slice(b"8BIM");
            ir.extend_from_slice(&1005u16.to_be_bytes());
            ir.extend_from_slice(&[0u8, 0u8]); // empty Pascal name padded to even
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

        // 4. Layer & Mask Info
        let mut li = Vec::new();
        if !layers.is_empty() {
            li.extend_from_slice(&(layers.len() as i16).to_be_bytes());

            // Pre-encode all channel payloads so we know exact lengths
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
                li.push(0); // clipping
                li.push(if layer.visible { 0 } else { 0x02 });
                li.push(0); // filler

                let mut extra = Vec::new();
                extra.extend_from_slice(&0u32.to_be_bytes()); // mask data
                extra.extend_from_slice(&0u32.to_be_bytes()); // blending ranges
                let pname = layer.pascal_name.as_bytes();
                let plen = pname.len().min(255);
                extra.push(plen as u8);
                extra.extend_from_slice(&pname[..plen]);
                let total_pascal = (1 + plen + 3) & !3;
                extra.resize(8 + total_pascal, 0);

                if let Some(uname) = layer.unicode_name {
                    let units: Vec<u16> = uname.encode_utf16().collect();
                    let block_len = 4 + units.len() * 2;
                    extra.extend_from_slice(b"8BIM");
                    extra.extend_from_slice(b"luni");
                    extra.extend_from_slice(&(block_len as u32).to_be_bytes());
                    extra.extend_from_slice(&(units.len() as u32).to_be_bytes());
                    for u in units {
                        extra.extend_from_slice(&u.to_be_bytes());
                    }
                    if block_len % 2 == 1 {
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
            if li.len() % 2 == 1 {
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
        }
        if is_psb {
            out.extend_from_slice(&(lm.len() as u64).to_be_bytes());
        } else {
            out.extend_from_slice(&(lm.len() as u32).to_be_bytes());
        }
        out.extend_from_slice(&lm);

        // 5. Composite Image Data
        if let Some((comp, planes)) = composite {
            out.extend_from_slice(&comp.to_be_bytes());
            if comp == 0 {
                for plane in planes {
                    out.extend_from_slice(&plane);
                }
            } else if comp == 1 {
                let row_bytes = (width as usize) * (usize::from(depth) / 8).max(1);
                let mut enc_all = Vec::new();
                for plane in &planes {
                    for r in 0..(height as usize) {
                        let start = r * row_bytes;
                        let end = (start + row_bytes).min(plane.len());
                        enc_all.push(encode_packbits_row(&plane[start..end]));
                    }
                }
                for er in &enc_all {
                    if is_psb {
                        out.extend_from_slice(&(er.len() as u32).to_be_bytes());
                    } else {
                        out.extend_from_slice(&(er.len() as u16).to_be_bytes());
                    }
                }
                for er in enc_all {
                    out.extend_from_slice(&er);
                }
            }
        }

        out
    }

    #[test]
    fn psd_multilayer_rgb_raw_and_packbits_with_alpha_blend_and_unicode_name() {
        // 4x2 background (RAW solid blue) + 2x2 foreground (PackBits RLE red with 50% alpha, Multiply, hidden)
        let bg = SynthLayer {
            top: 0,
            left: 0,
            bottom: 2,
            right: 4,
            blend: b"norm",
            opacity: 255,
            visible: true,
            pascal_name: "Background",
            unicode_name: None,
            channels: vec![
                (0, 0, vec![10; 8]),
                (1, 0, vec![20; 8]),
                (2, 0, vec![240; 8]),
            ],
        };
        let fg = SynthLayer {
            top: 0,
            left: 1,
            bottom: 2,
            right: 3,
            blend: b"mul ",
            opacity: 128,
            visible: false,
            pascal_name: "Overlay",
            unicode_name: Some("طبقة الشعار"),
            channels: vec![
                (-1, 1, vec![200; 4]),
                (0, 1, vec![250; 4]),
                (1, 1, vec![30; 4]),
                (2, 1, vec![40; 4]),
            ],
        };

        let bytes = build_synthetic_psd(false, 4, 2, 3, 8, 3, Some(300.0), &[bg, fg], None);
        let doc = import_psd_bytes(&bytes, "Studio Poster").unwrap();

        assert_eq!(doc.title(), "Studio Poster");
        assert_eq!(doc.color_mode(), ColorMode::Rgb);
        assert!((doc.dpi() - 300.0).abs() < 0.01);
        assert_eq!(doc.pages().len(), 1);
        assert_eq!(doc.pages()[0].layers.len(), 2);

        let l0 = &doc.pages()[0].layers[0];
        let l1 = &doc.pages()[0].layers[1];
        assert_eq!(l0.name, "Background");
        assert!(l0.visible);
        assert_eq!(l1.name, "طبقة الشعار");
        assert!(!l1.visible);

        let obj0 = &doc.objects()[&l0.objects[0]];
        let obj1 = &doc.objects()[&l1.objects[0]];
        assert_eq!(obj0.blend_mode, BlendMode::Normal);
        assert_eq!(obj1.blend_mode, BlendMode::Multiply);
        assert!((obj1.opacity - (128.0 / 255.0)).abs() < 0.01);
        assert!(!obj1.visible);

        let NodePayload::Raster(r0) = &obj0.payload else {
            panic!("expected raster layer 0");
        };
        let a0 = &doc.assets()[&r0.asset_id];
        assert_eq!((a0.width, a0.height), (4, 2));
        assert_eq!(&a0.rgba_bytes[0..4], &[10, 20, 240, 255]);

        let NodePayload::Raster(r1) = &obj1.payload else {
            panic!("expected raster layer 1");
        };
        let a1 = &doc.assets()[&r1.asset_id];
        assert_eq!((a1.width, a1.height), (2, 2));
        assert_eq!(&a1.rgba_bytes[0..4], &[250, 30, 40, 200]);
    }

    #[test]
    fn psd_cmyk_16bit_and_psb_v2_decode_accurately() {
        // Pure Cyan in Photoshop CMYK 16-bit: C=0 (100% ink), M=65535 (0%), Y=65535 (0%), K=65535 (0%)
        let zero16 = [0u8, 0u8, 0u8, 0u8];
        let max16 = [0xFFu8, 0xFFu8, 0xFFu8, 0xFFu8];
        let cyan_layer = SynthLayer {
            top: 0,
            left: 0,
            bottom: 1,
            right: 2,
            blend: b"scrn",
            opacity: 255,
            visible: true,
            pascal_name: "CyanPlate",
            unicode_name: None,
            channels: vec![
                (0, 0, zero16.to_vec()),
                (1, 0, max16.to_vec()),
                (2, 0, max16.to_vec()),
                (3, 0, max16.to_vec()),
            ],
        };

        let bytes = build_synthetic_psd(true, 2, 1, 4, 16, 4, Some(150.0), &[cyan_layer], None);
        let doc = import_psd_bytes(&bytes, "CMYK PSB").unwrap();
        assert_eq!(doc.color_mode(), ColorMode::Cmyk);
        assert_eq!(doc.metadata().source_format.as_deref(), Some("psb"));
        assert!((doc.dpi() - 150.0).abs() < 0.01);

        let obj = doc.objects().values().next().unwrap();
        assert_eq!(obj.blend_mode, BlendMode::Screen);
        let NodePayload::Raster(r) = &obj.payload else {
            panic!("expected raster");
        };
        let asset = &doc.assets()[&r.asset_id];
        // Pure Cyan -> sRGB (0, 255, 255, 255)
        assert_eq!(&asset.rgba_bytes[0..4], &[0, 255, 255, 255]);
    }

    #[test]
    fn psd_flattened_composite_packbits_fallback() {
        let r = vec![120u8; 6];
        let g = vec![180u8; 6];
        let b = vec![60u8; 6];
        let bytes = build_synthetic_psd(
            false,
            3,
            2,
            3,
            8,
            3,
            Some(96.0),
            &[],
            Some((1, vec![r, g, b])),
        );
        let doc = import_psd_bytes(&bytes, "Flat Photo").unwrap();
        assert_eq!(doc.pages()[0].layers.len(), 1);
        assert_eq!(doc.pages()[0].layers[0].name, "Composite");
        let asset = doc.assets().values().next().unwrap();
        assert_eq!((asset.width, asset.height), (3, 2));
        assert_eq!(&asset.rgba_bytes[0..4], &[120, 180, 60, 255]);
    }

    #[test]
    fn psd_rejects_corrupt_and_truncated_inputs_cleanly() {
        assert!(import_psd_bytes(b"NOT_A_PSD", "bad").is_err());
        assert!(import_psd_bytes(b"8BPS\x00\x01\x00\x00", "short").is_err());
        // Corrupt zero dimensions
        let mut bad_dim = vec![0u8; 38];
        bad_dim[0..4].copy_from_slice(b"8BPS");
        bad_dim[5] = 1;
        bad_dim[13] = 3; // 3 channels, 0x0 dims
        bad_dim[23] = 8;
        bad_dim[25] = 3;
        assert!(import_psd_bytes(&bad_dim, "zero").is_err());
    }

    #[test]
    fn psd_downscales_massive_layer_while_preserving_physical_mm() {
        // 4096 x 4 solid grayscale layer -> downscaled to 2048 x 2 while keeping exact mm size!
        let big = SynthLayer {
            top: 0,
            left: 0,
            bottom: 4,
            right: 4096,
            blend: b"norm",
            opacity: 255,
            visible: true,
            pascal_name: "WideBanner",
            unicode_name: None,
            channels: vec![(0, 1, vec![200u8; 4096 * 4])],
        };
        let bytes = build_synthetic_psd(false, 4096, 4, 1, 8, 1, Some(254.0), &[big], None);
        let doc = import_psd_bytes(&bytes, "Wide Banner").unwrap();
        // At 254 DPI, 4096 px = 409.6 mm, 4 px = 0.4 mm
        let obj = doc.objects().values().next().unwrap();
        let NodePayload::Raster(r) = &obj.payload else {
            panic!("expected raster");
        };
        assert!((r.width_mm - 409.6).abs() < 0.05);
        assert!((r.height_mm - 0.4).abs() < 0.05);
        let asset = &doc.assets()[&r.asset_id];
        assert!(asset.width <= MAX_LAYER_OUTPUT_DIM);
        assert!(asset.height <= MAX_LAYER_OUTPUT_DIM);
        assert_eq!(&asset.rgba_bytes[0..4], &[200, 200, 200, 255]);
    }

    #[test]
    fn psd_multilayer_with_user_mask_and_spot_channel_does_not_desync_next_layer() {
        // Layer 0 has RGB (0,1,2) PLUS a user mask (-2) and spot channel (4) that are skipped.
        // Layer 1 immediately follows and must decode its exact green pixels without desync!
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
                (-2, 0, vec![255; 4]), // user layer mask (skipped)
                (4, 0, vec![99; 4]),   // spot channel (skipped)
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

        let top_obj = &doc.objects()[&doc.pages()[0].layers[1].objects[0]];
        let NodePayload::Raster(r) = &top_obj.payload else {
            panic!("expected raster");
        };
        let asset = &doc.assets()[&r.asset_id];
        assert_eq!(&asset.rgba_bytes[0..4], &[15, 230, 45, 255]);
    }

    #[test]
    fn psd_crafted_composite_rle_table_overflow_is_rejected_without_oom() {
        // 40-byte PSB claiming 56 channels, 300_000 height, RLE compression (would pre-alloc 134 MiB)
        let mut crafted = Vec::new();
        crafted.extend_from_slice(b"8BPS");
        crafted.extend_from_slice(&2u16.to_be_bytes()); // PSB v2
        crafted.extend_from_slice(&[0u8; 6]);
        crafted.extend_from_slice(&56u16.to_be_bytes()); // 56 channels
        crafted.extend_from_slice(&300_000u32.to_be_bytes()); // height
        crafted.extend_from_slice(&1u32.to_be_bytes()); // width
        crafted.extend_from_slice(&8u16.to_be_bytes()); // depth
        crafted.extend_from_slice(&3u16.to_be_bytes()); // RGB
        crafted.extend_from_slice(&0u32.to_be_bytes()); // color mode len
        crafted.extend_from_slice(&0u32.to_be_bytes()); // image resources len
        crafted.extend_from_slice(&0u64.to_be_bytes()); // layer/mask len (PSB u64)
        crafted.extend_from_slice(&1u16.to_be_bytes()); // composite PackBits RLE

        let err = import_psd_bytes(&crafted, "OOM PSB").unwrap_err();
        assert!(matches!(err, IoError::InvalidFormat { .. }));
    }
}
