// SPDX-License-Identifier: MPL-2.0

//! Clean-room Corel PHOTO-PAINT (`.cpt`) multi-layer raster container reader
//! (and test fixture writer), supporting `CPT7FILE`, `CPT8FILE`, `CPT9FILE`,
//! and RIFF `CPT ` containers with RGB/RGBA/CMYK/Gray color models, multi-layer
//! raster strips, separate alpha mask channels, and Raw / PackBits-RLE / Zlib /
//! Deflate decoding.

use crate::cdr_riff::{decode_blend_mode, encode_blend_mode};
use crate::coord::{decode_cdr_text, CODEPAGE_UTF8};
use crate::error::{CdrError, CdrResult};
use crate::limits::{decompress_deflate_bounded, decompress_zlib_bounded, ByteReader, CodecLimits};
use crate::riff::{parse_riff_with_budget, write_riff, RiffDocument, RiffEndian, RiffNode};
use crate::{CorelFormat, ImportReport};
use coree_core::{
    math::{sanitize_f32, MM_PER_INCH},
    Affine2D, BlendMode, Color, ColorMode, Document, Layer, LayerId, NodePayload, Page, PageId,
    RasterAsset, RasterLayerData, SceneObject,
};
use flate2::{
    write::{DeflateEncoder, ZlibEncoder},
    Compression,
};
use std::io::Write;

/// Pixel color model inside a `.cpt` PHOTO-PAINT container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum CptColorModel {
    Rgb24 = 1,
    Rgba32 = 2,
    Cmyk32 = 3,
    Gray8 = 4,
}

impl CptColorModel {
    #[must_use]
    pub const fn from_u16(raw: u16) -> Option<Self> {
        match raw {
            1 => Some(Self::Rgb24),
            2 => Some(Self::Rgba32),
            3 => Some(Self::Cmyk32),
            4 => Some(Self::Gray8),
            _ => None,
        }
    }

    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb24 => 3,
            Self::Rgba32 | Self::Cmyk32 => 4,
            Self::Gray8 => 1,
        }
    }

    #[must_use]
    pub const fn to_core_color_mode(self) -> ColorMode {
        match self {
            Self::Cmyk32 => ColorMode::Cmyk,
            Self::Rgb24 | Self::Rgba32 | Self::Gray8 => ColorMode::Rgb,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rgb24 => "RGB",
            Self::Rgba32 => "RGBA",
            Self::Cmyk32 => "CMYK",
            Self::Gray8 => "Gray",
        }
    }
}

/// Strip/channel compression codec inside a `.cpt` container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CptCompression {
    Raw = 0,
    Rle = 1,
    Zlib = 2,
    Deflate = 3,
}

impl CptCompression {
    #[must_use]
    pub const fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Raw),
            1 => Some(Self::Rle),
            2 => Some(Self::Zlib),
            3 => Some(Self::Deflate),
            _ => None,
        }
    }
}

/// Specification for writing a synthetic `.cpt` layer (used by fixtures and tests).
#[derive(Debug, Clone)]
pub struct CptLayerSpec {
    pub name: String,
    pub offset_x: i32,
    pub offset_y: i32,
    pub width: u32,
    pub height: u32,
    pub opacity: u8,
    pub blend_mode: BlendMode,
    pub visible: bool,
    pub locked: bool,
    pub color_model: CptColorModel,
    pub compression: CptCompression,
    /// Raw pixels matching `color_model.bytes_per_pixel() * width * height`.
    pub raw_pixels: Vec<u8>,
    /// Optional 8-bit alpha mask channel (`width * height` bytes) and its compression codec.
    pub alpha_mask: Option<(CptCompression, Vec<u8>)>,
    /// Rows per strip (`0` = single strip covering entire height).
    pub rows_per_strip: u32,
}

/// Detailed result of parsing a `.cpt` file (including version and optional RIFF tree).
#[derive(Debug, Clone)]
pub struct ParsedCpt {
    pub document: Document,
    pub version: u8,
    pub color_model: CptColorModel,
    pub warnings: Vec<String>,
    pub riff_tree: Option<RiffDocument>,
}

/// Returns `Some(version)` if `bytes` begins with `CPT7FILE`, `CPT8FILE`, `CPT9FILE`,
/// or a RIFF `CPT ` container header.
#[must_use]
pub fn detect_cpt_version(bytes: &[u8]) -> Option<u8> {
    if bytes.len() >= 8 {
        match &bytes[0..8] {
            b"CPT7FILE" => return Some(7),
            b"CPT8FILE" => return Some(8),
            b"CPT9FILE" => return Some(9),
            _ => {}
        }
    }
    if bytes.len() >= 12
        && (&bytes[0..4] == b"RIFF" || &bytes[0..4] == b"RIFX")
        && &bytes[8..12] == b"CPT "
    {
        return Some(9);
    }
    None
}

/// Imports a Corel PHOTO-PAINT (`.cpt`) file from raw bytes.
pub fn import_cpt_bytes(bytes: &[u8], limits: &CodecLimits) -> CdrResult<ImportReport> {
    let parsed = parse_cpt_bytes(bytes, limits)?;
    Ok(ImportReport {
        document: parsed.document,
        detected_format: CorelFormat::Cpt {
            version: parsed.version,
        },
        warnings: parsed.warnings,
    })
}

/// Parses either a native `CPT7FILE`/`CPT8FILE`/`CPT9FILE` container OR a RIFF `CPT `
/// container into a [`ParsedCpt`].
pub fn parse_cpt_bytes(bytes: &[u8], limits: &CodecLimits) -> CdrResult<ParsedCpt> {
    if bytes.len() >= 12
        && (&bytes[0..4] == b"RIFF" || &bytes[0..4] == b"RIFX")
        && &bytes[8..12] == b"CPT "
    {
        return parse_cpt_riff(bytes, limits);
    }
    parse_cpt_native(bytes, limits)
}

fn parse_cpt_native(bytes: &[u8], limits: &CodecLimits) -> CdrResult<ParsedCpt> {
    let mut r = ByteReader::new(bytes);
    let magic = r.read_array::<8>("CPT magic")?;
    let version = match &magic {
        b"CPT7FILE" => 7u8,
        b"CPT8FILE" => 8u8,
        b"CPT9FILE" => 9u8,
        _ => {
            return Err(CdrError::InvalidMagic {
                expected: "CPT7FILE/CPT8FILE/CPT9FILE or RIFF CPT ".to_owned(),
                found: String::from_utf8_lossy(&magic).into_owned(),
            });
        }
    };

    let _header_size = r.read_u32_le("CPT header_size")?;
    let canvas_w = r.read_u32_le("CPT canvas width")?;
    let canvas_h = r.read_u32_le("CPT canvas height")?;
    limits.validate_bitmap_dims(canvas_w, canvas_h)?;

    let model_raw = r.read_u16_le("CPT color_model")?;
    let canvas_model = CptColorModel::from_u16(model_raw).ok_or_else(|| {
        CdrError::MalformedHeader(format!("unsupported CPT color model {model_raw}"))
    })?;
    let _bpp = r.read_u16_le("CPT bpp")?;
    let dpi_x = sanitize_f32(r.read_f32_le("CPT dpi_x")?, 300.0).max(1.0);
    let dpi_y = sanitize_f32(r.read_f32_le("CPT dpi_y")?, dpi_x).max(1.0);
    let layer_count = usize::try_from(r.read_u32_le("CPT layer_count")?)
        .map_err(|_| CdrError::ArithmeticOverflow("CPT layer_count"))?;
    if layer_count > limits.max_chunks || layer_count > 4096 {
        return Err(CdrError::ChunkLimitExceeded {
            count: layer_count,
            limit: limits.max_chunks.min(4096),
        });
    }

    let title_len = usize::from(r.read_u16_le("CPT title_len")?);
    let title_bytes = r.read_bytes(title_len, "CPT title")?;
    let title = decode_cdr_text(title_bytes, Some(CODEPAGE_UTF8), None);

    let page_w_mm = f64::from(canvas_w) * MM_PER_INCH / f64::from(dpi_x);
    let page_h_mm = f64::from(canvas_h) * MM_PER_INCH / f64::from(dpi_y);

    let mut doc = Document::new(
        if title.is_empty() {
            "Imported CPT"
        } else {
            &title
        },
        page_w_mm,
        page_h_mm,
        canvas_model.to_core_color_mode(),
        dpi_x,
    );
    doc.pages[0].layers.clear();
    doc.metadata.source_format = Some(format!("CPT v{version}"));

    let mut budget_remaining = limits.max_uncompressed_bytes;
    let warnings = Vec::new();

    for layer_idx in 0..layer_count {
        let tag = r.read_fourcc("CPT layer tag")?;
        if &tag != b"LAYR" {
            return Err(CdrError::InvalidChunk {
                fourcc: String::from_utf8_lossy(&tag).into_owned(),
                reason: "expected LAYR block in CPT stream".to_owned(),
            });
        }
        let name_len = usize::from(r.read_u16_le("CPT layer name_len")?);
        let name_bytes = r.read_bytes(name_len, "CPT layer name")?;
        let layer_name = decode_cdr_text(name_bytes, None, None);
        let name = if layer_name.is_empty() {
            format!("Raster Layer {}", layer_idx + 1)
        } else {
            layer_name
        };

        let offset_x = r.read_i32_le("CPT layer offset_x")?;
        let offset_y = r.read_i32_le("CPT layer offset_y")?;
        let lw = r.read_u32_le("CPT layer width")?;
        let lh = r.read_u32_le("CPT layer height")?;
        limits.validate_bitmap_dims(lw, lh)?;

        let opacity_u8 = r.read_u8("CPT layer opacity")?;
        let blend_mode = decode_blend_mode(r.read_u8("CPT layer blend_mode")?);
        let flags = r.read_u8("CPT layer flags")?;
        let visible = (flags & 0x01) != 0;
        let locked = (flags & 0x02) != 0;

        let lmodel_raw = r.read_u16_le("CPT layer color_model")?;
        let lmodel = CptColorModel::from_u16(lmodel_raw).unwrap_or(canvas_model);
        let comp_raw = r.read_u8("CPT layer compression")?;
        let compression =
            CptCompression::from_u8(comp_raw).ok_or_else(|| CdrError::InvalidChunk {
                fourcc: "LAYR".to_owned(),
                reason: format!("unsupported CPT compression codec {comp_raw}"),
            })?;

        let strip_count = usize::try_from(r.read_u32_le("CPT strip_count")?)
            .map_err(|_| CdrError::ArithmeticOverflow("CPT strip_count"))?;
        if strip_count == 0 || strip_count > (lh as usize).saturating_add(1) {
            return Err(CdrError::InvalidChunk {
                fourcc: "LAYR".to_owned(),
                reason: format!("invalid CPT strip count {strip_count} for height {lh}"),
            });
        }

        let bpp = lmodel.bytes_per_pixel();
        let expected_layer_raw = (lw as usize)
            .checked_mul(lh as usize)
            .and_then(|px| px.checked_mul(bpp))
            .ok_or(CdrError::ArithmeticOverflow("CPT layer pixel buffer"))?;
        let mut raw_pixels = Vec::with_capacity(expected_layer_raw.min(64 * 1024));
        let mut rows_decoded: u32 = 0;

        for _ in 0..strip_count {
            let strip_rows = r.read_u32_le("CPT strip_rows")?;
            let encoded_len = usize::try_from(r.read_u32_le("CPT strip encoded_len")?)
                .map_err(|_| CdrError::ArithmeticOverflow("CPT strip encoded_len"))?;
            let encoded = r.read_bytes(encoded_len, "CPT strip payload")?;

            let next_rows = rows_decoded
                .checked_add(strip_rows)
                .ok_or(CdrError::ArithmeticOverflow("CPT strip rows"))?;
            if strip_rows == 0 || next_rows > lh {
                return Err(CdrError::InvalidChunk {
                    fourcc: "LAYR".to_owned(),
                    reason: format!("CPT strip rows {next_rows} exceeds layer height {lh}"),
                });
            }
            let expected_strip_bytes = (lw as usize)
                .checked_mul(strip_rows as usize)
                .and_then(|px| px.checked_mul(bpp))
                .ok_or(CdrError::ArithmeticOverflow("CPT strip bytes"))?;

            let strip_decoded = decode_cpt_channel(
                encoded,
                compression,
                expected_strip_bytes,
                limits,
                &mut budget_remaining,
            )?;
            raw_pixels.extend_from_slice(&strip_decoded);
            rows_decoded = next_rows;
        }

        if rows_decoded != lh || raw_pixels.len() != expected_layer_raw {
            return Err(CdrError::InvalidChunk {
                fourcc: "LAYR".to_owned(),
                reason: format!(
                    "CPT decoded {rows_decoded}/{lh} rows ({}/{expected_layer_raw} bytes)",
                    raw_pixels.len()
                ),
            });
        }

        // Optional separate 8-bit alpha mask channel
        let has_mask = r.read_u8("CPT has_alpha_mask")? != 0;
        let alpha_mask = if has_mask {
            let mask_comp = CptCompression::from_u8(r.read_u8("CPT mask compression")?)
                .ok_or_else(|| CdrError::InvalidChunk {
                    fourcc: "MASK".to_owned(),
                    reason: "unsupported CPT alpha mask compression".to_owned(),
                })?;
            let mask_len = usize::try_from(r.read_u32_le("CPT mask len")?)
                .map_err(|_| CdrError::ArithmeticOverflow("CPT mask len"))?;
            let mask_encoded = r.read_bytes(mask_len, "CPT mask bytes")?;
            let expected_mask = (lw as usize)
                .checked_mul(lh as usize)
                .ok_or(CdrError::ArithmeticOverflow("CPT mask pixel count"))?;
            Some(decode_cpt_channel(
                mask_encoded,
                mask_comp,
                expected_mask,
                limits,
                &mut budget_remaining,
            )?)
        } else {
            None
        };

        insert_cpt_layer_into_doc(
            &mut doc,
            limits,
            &mut budget_remaining,
            &name,
            offset_x,
            offset_y,
            lw,
            lh,
            dpi_x,
            dpi_y,
            opacity_u8,
            blend_mode,
            visible,
            locked,
            lmodel,
            &raw_pixels,
            alpha_mask.as_deref(),
        )?;
    }

    if doc.pages[0].layers.is_empty() {
        let lid = LayerId(doc.alloc_id());
        doc.pages[0].layers.push(Layer::new(lid, "Background"));
    }

    Ok(ParsedCpt {
        document: doc,
        version,
        color_model: canvas_model,
        warnings,
        riff_tree: None,
    })
}

fn parse_cpt_riff(bytes: &[u8], limits: &CodecLimits) -> CdrResult<ParsedCpt> {
    let mut budget_remaining = limits.max_uncompressed_bytes;
    let riff_tree = parse_riff_with_budget(bytes, limits, &mut budget_remaining, true)?;
    if &riff_tree.form_type != b"CPT " {
        return Err(CdrError::UnsupportedFormat(
            "RIFF form is not 'CPT '".to_owned(),
        ));
    }

    let head = riff_tree
        .find_chunk(b"head")
        .ok_or_else(|| CdrError::MalformedHeader("RIFF CPT missing 'head' chunk".to_owned()))?;
    let mut hr = ByteReader::new(head);
    let canvas_w = hr.read_u32_le("RIFF CPT width")?;
    let canvas_h = hr.read_u32_le("RIFF CPT height")?;
    limits.validate_bitmap_dims(canvas_w, canvas_h)?;
    let model_raw = hr.read_u16_le("RIFF CPT color_model")?;
    let canvas_model = CptColorModel::from_u16(model_raw).ok_or_else(|| {
        CdrError::MalformedHeader(format!("unsupported RIFF CPT color model {model_raw}"))
    })?;
    let dpi_x = sanitize_f32(hr.read_f32_le("RIFF CPT dpi_x")?, 300.0).max(1.0);
    let dpi_y = sanitize_f32(hr.read_f32_le("RIFF CPT dpi_y")?, dpi_x).max(1.0);
    let tlen = usize::from(hr.read_u16_le("RIFF CPT title_len").unwrap_or(0));
    let title = if tlen > 0 && tlen <= hr.remaining() {
        decode_cdr_text(
            hr.read_bytes(tlen, "RIFF CPT title")?,
            Some(CODEPAGE_UTF8),
            None,
        )
    } else {
        "Imported RIFF CPT".to_owned()
    };

    let page_w_mm = f64::from(canvas_w) * MM_PER_INCH / f64::from(dpi_x);
    let page_h_mm = f64::from(canvas_h) * MM_PER_INCH / f64::from(dpi_y);
    let mut doc = Document::new(
        title,
        page_w_mm,
        page_h_mm,
        canvas_model.to_core_color_mode(),
        dpi_x,
    );
    doc.pages[0].layers.clear();
    doc.metadata.source_format = Some("CPT RIFF".to_owned());

    let warnings = Vec::new();

    for node in &riff_tree.children {
        if let RiffNode::List {
            list_type,
            children,
            ..
        } = node
        {
            if list_type != b"layr" {
                continue;
            }
            let info_bytes = node
                .find_chunk(b"info")
                .ok_or_else(|| CdrError::InvalidChunk {
                    fourcc: "layr".to_owned(),
                    reason: "missing 'info' chunk in RIFF CPT layer".to_owned(),
                })?;
            let mut ir = ByteReader::new(info_bytes);
            let nlen = usize::from(ir.read_u16_le("CPT info name_len")?);
            let name = decode_cdr_text(ir.read_bytes(nlen, "CPT info name")?, None, None);
            let ox = ir.read_i32_le("CPT info ox")?;
            let oy = ir.read_i32_le("CPT info oy")?;
            let lw = ir.read_u32_le("CPT info w")?;
            let lh = ir.read_u32_le("CPT info h")?;
            limits.validate_bitmap_dims(lw, lh)?;
            let opacity = ir.read_u8("CPT info opacity")?;
            let blend = decode_blend_mode(ir.read_u8("CPT info blend")?);
            let flags = ir.read_u8("CPT info flags")?;
            let lmodel =
                CptColorModel::from_u16(ir.read_u16_le("CPT info model")?).unwrap_or(canvas_model);

            let pxls_bytes = node
                .find_chunk(b"pxls")
                .ok_or_else(|| CdrError::InvalidChunk {
                    fourcc: "layr".to_owned(),
                    reason: "missing 'pxls' chunk in RIFF CPT layer".to_owned(),
                })?;
            let mut pr = ByteReader::new(pxls_bytes);
            let comp = CptCompression::from_u8(pr.read_u8("CPT pxls comp")?).ok_or_else(|| {
                CdrError::InvalidChunk {
                    fourcc: "pxls".to_owned(),
                    reason: "invalid CPT pxls compression".to_owned(),
                }
            })?;
            let expected_raw = (lw as usize)
                .checked_mul(lh as usize)
                .and_then(|px| px.checked_mul(lmodel.bytes_per_pixel()))
                .ok_or(CdrError::ArithmeticOverflow("RIFF CPT pxls size"))?;
            let raw_pixels = decode_cpt_channel(
                pr.read_rest(),
                comp,
                expected_raw,
                limits,
                &mut budget_remaining,
            )?;

            let alpha_mask = if let Some(mask_bytes) = node.find_chunk(b"mask") {
                let mut mr = ByteReader::new(mask_bytes);
                let mcomp =
                    CptCompression::from_u8(mr.read_u8("CPT mask comp")?).ok_or_else(|| {
                        CdrError::InvalidChunk {
                            fourcc: "mask".to_owned(),
                            reason: "invalid CPT mask compression".to_owned(),
                        }
                    })?;
                let expected_mask = (lw as usize)
                    .checked_mul(lh as usize)
                    .ok_or(CdrError::ArithmeticOverflow("RIFF CPT mask size"))?;
                Some(decode_cpt_channel(
                    mr.read_rest(),
                    mcomp,
                    expected_mask,
                    limits,
                    &mut budget_remaining,
                )?)
            } else {
                None
            };

            let _ = children;
            insert_cpt_layer_into_doc(
                &mut doc,
                limits,
                &mut budget_remaining,
                if name.is_empty() { "Layer" } else { &name },
                ox,
                oy,
                lw,
                lh,
                dpi_x,
                dpi_y,
                opacity,
                blend,
                (flags & 0x01) != 0,
                (flags & 0x02) != 0,
                lmodel,
                &raw_pixels,
                alpha_mask.as_deref(),
            )?;
        }
    }

    if doc.pages[0].layers.is_empty() {
        let lid = LayerId(doc.alloc_id());
        doc.pages[0].layers.push(Layer::new(lid, "Background"));
    }

    Ok(ParsedCpt {
        document: doc,
        version: 9,
        color_model: canvas_model,
        warnings,
        riff_tree: Some(riff_tree),
    })
}

#[allow(clippy::too_many_arguments)]
fn insert_cpt_layer_into_doc(
    doc: &mut Document,
    limits: &CodecLimits,
    budget_remaining: &mut u64,
    name: &str,
    offset_x: i32,
    offset_y: i32,
    lw: u32,
    lh: u32,
    dpi_x: f32,
    dpi_y: f32,
    opacity_u8: u8,
    blend_mode: BlendMode,
    visible: bool,
    locked: bool,
    model: CptColorModel,
    raw_pixels: &[u8],
    alpha_mask: Option<&[u8]>,
) -> CdrResult<()> {
    let pixel_count = (lw as usize)
        .checked_mul(lh as usize)
        .ok_or(CdrError::ArithmeticOverflow("CPT pixel count"))?;
    let rgba_bytes = pixel_count
        .checked_mul(4)
        .ok_or(CdrError::ArithmeticOverflow("CPT RGBA bytes"))?;
    // Charge cumulative decoded RGBA buffer allocation BEFORE allocating pixel memory!
    limits.consume_budget(budget_remaining, rgba_bytes as u64)?;
    let mut rgba = Vec::with_capacity(rgba_bytes);

    match model {
        CptColorModel::Rgb24 => {
            for i in 0..pixel_count {
                let s = i * 3;
                let a = alpha_mask.map_or(255, |m| m[i]);
                rgba.extend_from_slice(&[raw_pixels[s], raw_pixels[s + 1], raw_pixels[s + 2], a]);
            }
        }
        CptColorModel::Rgba32 => {
            for i in 0..pixel_count {
                let s = i * 4;
                let base_a = raw_pixels[s + 3];
                let a = match alpha_mask {
                    Some(m) => ((u16::from(base_a) * u16::from(m[i])) / 255) as u8,
                    None => base_a,
                };
                rgba.extend_from_slice(&[raw_pixels[s], raw_pixels[s + 1], raw_pixels[s + 2], a]);
            }
        }
        CptColorModel::Cmyk32 => {
            for i in 0..pixel_count {
                let s = i * 4;
                let c = f32::from(raw_pixels[s]) / 255.0;
                let m = f32::from(raw_pixels[s + 1]) / 255.0;
                let y = f32::from(raw_pixels[s + 2]) / 255.0;
                let k = f32::from(raw_pixels[s + 3]) / 255.0;
                let [r8, g8, b8, _] = Color::cmyk(c, m, y, k).to_rgba8();
                let a = alpha_mask.map_or(255, |mask| mask[i]);
                rgba.extend_from_slice(&[r8, g8, b8, a]);
            }
        }
        CptColorModel::Gray8 => {
            for i in 0..pixel_count {
                let v = raw_pixels[i];
                let a = alpha_mask.map_or(255, |m| m[i]);
                rgba.extend_from_slice(&[v, v, v, a]);
            }
        }
    }

    let asset = RasterAsset::new(lw, lh, rgba, dpi_x, model.to_core_color_mode())?;
    let asset_id = doc.register_asset(asset);

    let layer_id = LayerId(doc.alloc_id());
    let mut layer = Layer::new(layer_id, name);
    layer.visible = visible;
    layer.locked = locked;

    let oid = doc.alloc_object_id();
    let w_mm = f64::from(lw) * MM_PER_INCH / f64::from(dpi_x);
    let h_mm = f64::from(lh) * MM_PER_INCH / f64::from(dpi_y);
    let tx_mm = f64::from(offset_x) * MM_PER_INCH / f64::from(dpi_x);
    let ty_mm = f64::from(offset_y) * MM_PER_INCH / f64::from(dpi_y);

    let mut obj = SceneObject::new(
        oid,
        name,
        NodePayload::Raster(RasterLayerData {
            asset_id,
            width_mm: w_mm,
            height_mm: h_mm,
            filters: Vec::new(),
            crop_rect: None,
        }),
    );
    obj.transform = Affine2D::translate(tx_mm, ty_mm);
    obj.opacity = f32::from(opacity_u8) / 255.0;
    obj.blend_mode = blend_mode;
    obj.visible = visible;
    obj.locked = locked;

    doc.objects.insert(oid, obj);
    layer.objects.push(oid);
    if doc.pages.is_empty() {
        let pid = PageId(doc.alloc_id());
        doc.pages.push(Page {
            id: pid,
            name: "Canvas".to_owned(),
            width_mm: w_mm,
            height_mm: h_mm,
            bleed_mm: 0.0,
            facing_pages: false,
            layers: vec![layer],
        });
    } else {
        doc.pages[0].layers.push(layer);
    }
    Ok(())
}

// =============================================================================
// Bounded CPT Channel Codecs (Raw, PackBits RLE, Zlib, Deflate)
// =============================================================================

fn decode_cpt_channel(
    encoded: &[u8],
    comp: CptCompression,
    expected_len: usize,
    limits: &CodecLimits,
    budget_remaining: &mut u64,
) -> CdrResult<Vec<u8>> {
    let decoded = match comp {
        CptCompression::Raw => {
            limits.consume_budget(budget_remaining, encoded.len() as u64)?;
            encoded.to_vec()
        }
        CptCompression::Rle => {
            // Check budget & compression ratio BEFORE allocating RLE output to block RLE bombs!
            if (expected_len as u64) > *budget_remaining {
                return Err(CdrError::UncompressedSizeLimitExceeded {
                    limit: limits.max_uncompressed_bytes,
                });
            }
            limits.check_compression_ratio(encoded.len() as u64, expected_len as u64)?;
            let out = decode_packbits_rle(encoded, expected_len)?;
            limits.check_compression_ratio(encoded.len() as u64, out.len() as u64)?;
            limits.consume_budget(budget_remaining, out.len() as u64)?;
            out
        }
        CptCompression::Zlib => decompress_zlib_bounded(encoded, limits, budget_remaining)?,
        CptCompression::Deflate => decompress_deflate_bounded(encoded, limits, budget_remaining)?,
    };

    if decoded.len() != expected_len {
        return Err(CdrError::DecompressionFailed(format!(
            "CPT channel decoded {} bytes, expected {expected_len}",
            decoded.len()
        )));
    }
    Ok(decoded)
}

/// Decodes PackBits Byte-RLE with a hard cap at `max_output` bytes (zero OOM risk).
pub fn decode_packbits_rle(input: &[u8], max_output: usize) -> CdrResult<Vec<u8>> {
    let mut r = ByteReader::new(input);
    let mut out = Vec::with_capacity(max_output.min(64 * 1024));

    while !r.is_eof() {
        let header = r.read_i8("RLE control byte")?;
        if header == -128 {
            continue; // noop
        } else if header >= 0 {
            let count = (header as usize) + 1;
            let next_len = out
                .len()
                .checked_add(count)
                .ok_or(CdrError::ArithmeticOverflow("RLE literal len"))?;
            if next_len > max_output {
                return Err(CdrError::DecompressionFailed(format!(
                    "RLE output {next_len} exceeds expected buffer {max_output}"
                )));
            }
            let lit = r.read_bytes(count, "RLE literals")?;
            out.extend_from_slice(lit);
        } else {
            let count = (1 - i16::from(header)) as usize;
            let next_len = out
                .len()
                .checked_add(count)
                .ok_or(CdrError::ArithmeticOverflow("RLE run len"))?;
            if next_len > max_output {
                return Err(CdrError::DecompressionFailed(format!(
                    "RLE run output {next_len} exceeds expected buffer {max_output}"
                )));
            }
            let val = r.read_u8("RLE repeat byte")?;
            out.resize(next_len, val);
        }
    }

    Ok(out)
}

/// Encodes raw bytes using standard PackBits Byte-RLE.
#[must_use]
pub fn encode_packbits_rle(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < input.len() {
        // Detect run of >= 3 identical bytes
        let mut run = 1usize;
        while i + run < input.len() && run < 128 && input[i + run] == input[i] {
            run += 1;
        }
        if run >= 3 {
            out.push((1i16 - run as i16) as i8 as u8);
            out.push(input[i]);
            i += run;
        } else {
            let lit_start = i;
            let mut lit_len = 0usize;
            while i < input.len() && lit_len < 128 {
                if i + 2 < input.len() && input[i] == input[i + 1] && input[i] == input[i + 2] {
                    break;
                }
                i += 1;
                lit_len += 1;
            }
            out.push((lit_len - 1) as u8);
            out.extend_from_slice(&input[lit_start..lit_start + lit_len]);
        }
    }
    out
}

fn encode_cpt_channel(raw: &[u8], comp: CptCompression) -> CdrResult<Vec<u8>> {
    match comp {
        CptCompression::Raw => Ok(raw.to_vec()),
        CptCompression::Rle => Ok(encode_packbits_rle(raw)),
        CptCompression::Zlib => {
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
            enc.write_all(raw)
                .map_err(|e| CdrError::DecompressionFailed(e.to_string()))?;
            enc.finish()
                .map_err(|e| CdrError::DecompressionFailed(e.to_string()))
        }
        CptCompression::Deflate => {
            let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
            enc.write_all(raw)
                .map_err(|e| CdrError::DecompressionFailed(e.to_string()))?;
            enc.finish()
                .map_err(|e| CdrError::DecompressionFailed(e.to_string()))
        }
    }
}

// =============================================================================
// Clean-Room CPT Writers (Native CPT7/8/9 & RIFF `CPT `)
// =============================================================================

/// Writes a clean-room native `CPT7FILE`/`CPT8FILE`/`CPT9FILE` multi-layer raster file.
pub fn write_cpt_bytes(
    version: u8,
    title: &str,
    canvas_width: u32,
    canvas_height: u32,
    canvas_model: CptColorModel,
    dpi: f32,
    layers: &[CptLayerSpec],
) -> CdrResult<Vec<u8>> {
    let magic: &[u8; 8] = match version {
        7 => b"CPT7FILE",
        8 => b"CPT8FILE",
        _ => b"CPT9FILE",
    };
    let mut out = Vec::new();
    out.extend_from_slice(magic);
    let tb = title.as_bytes();
    let tlen = tb.len().min(u16::MAX as usize);
    let header_size = (28 + tlen) as u32;
    out.extend_from_slice(&header_size.to_le_bytes());
    out.extend_from_slice(&canvas_width.to_le_bytes());
    out.extend_from_slice(&canvas_height.to_le_bytes());
    out.extend_from_slice(&(canvas_model as u16).to_le_bytes());
    out.extend_from_slice(&((canvas_model.bytes_per_pixel() * 8) as u16).to_le_bytes());
    out.extend_from_slice(&dpi.to_le_bytes());
    out.extend_from_slice(&dpi.to_le_bytes());
    out.extend_from_slice(&(layers.len() as u32).to_le_bytes());
    out.extend_from_slice(&(tlen as u16).to_le_bytes());
    out.extend_from_slice(&tb[..tlen]);

    for layer in layers {
        let bpp = layer.color_model.bytes_per_pixel();
        let row_bytes = (layer.width as usize)
            .checked_mul(bpp)
            .ok_or(CdrError::ArithmeticOverflow("CPT write row_bytes"))?;
        let expected = row_bytes
            .checked_mul(layer.height as usize)
            .ok_or(CdrError::ArithmeticOverflow("CPT write pixel buffer"))?;
        if layer.raw_pixels.len() != expected {
            return Err(CdrError::InvalidChunk {
                fourcc: "LAYR".to_owned(),
                reason: format!(
                    "CPT layer '{}' raw_pixels len {} != expected {expected}",
                    layer.name,
                    layer.raw_pixels.len()
                ),
            });
        }
        out.extend_from_slice(b"LAYR");
        let nb = layer.name.as_bytes();
        let nlen = nb.len().min(u16::MAX as usize);
        out.extend_from_slice(&(nlen as u16).to_le_bytes());
        out.extend_from_slice(&nb[..nlen]);
        out.extend_from_slice(&layer.offset_x.to_le_bytes());
        out.extend_from_slice(&layer.offset_y.to_le_bytes());
        out.extend_from_slice(&layer.width.to_le_bytes());
        out.extend_from_slice(&layer.height.to_le_bytes());
        out.push(layer.opacity);
        out.push(encode_blend_mode(layer.blend_mode));
        let mut flags = 0u8;
        if layer.visible {
            flags |= 0x01;
        }
        if layer.locked {
            flags |= 0x02;
        }
        out.push(flags);
        out.extend_from_slice(&(layer.color_model as u16).to_le_bytes());
        out.push(layer.compression as u8);

        let rps = if layer.rows_per_strip == 0 {
            layer.height.max(1)
        } else {
            layer.rows_per_strip.min(layer.height.max(1))
        };
        let strip_count = layer.height.div_ceil(rps);
        out.extend_from_slice(&strip_count.to_le_bytes());

        let mut y = 0u32;
        while y < layer.height {
            let rows = rps.min(layer.height - y);
            let start = (y as usize) * row_bytes;
            let end = start + (rows as usize) * row_bytes;
            let encoded = encode_cpt_channel(&layer.raw_pixels[start..end], layer.compression)?;
            out.extend_from_slice(&rows.to_le_bytes());
            out.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            out.extend_from_slice(&encoded);
            y += rows;
        }

        if let Some((mcomp, ref mask_raw)) = layer.alpha_mask {
            let encoded_mask = encode_cpt_channel(mask_raw, mcomp)?;
            out.push(1);
            out.push(mcomp as u8);
            out.extend_from_slice(&(encoded_mask.len() as u32).to_le_bytes());
            out.extend_from_slice(&encoded_mask);
        } else {
            out.push(0);
        }
    }

    Ok(out)
}

/// Writes a clean-room RIFF-wrapped `CPT ` multi-layer raster container.
pub fn write_cpt_riff_bytes(
    title: &str,
    canvas_width: u32,
    canvas_height: u32,
    canvas_model: CptColorModel,
    dpi: f32,
    layers: &[CptLayerSpec],
) -> CdrResult<Vec<u8>> {
    let mut head = Vec::new();
    head.extend_from_slice(&canvas_width.to_le_bytes());
    head.extend_from_slice(&canvas_height.to_le_bytes());
    head.extend_from_slice(&(canvas_model as u16).to_le_bytes());
    head.extend_from_slice(&dpi.to_le_bytes());
    head.extend_from_slice(&dpi.to_le_bytes());
    let tb = title.as_bytes();
    let tlen = tb.len().min(u16::MAX as usize);
    head.extend_from_slice(&(tlen as u16).to_le_bytes());
    head.extend_from_slice(&tb[..tlen]);

    let mut nodes = vec![RiffNode::chunk(*b"head", head)];
    for layer in layers {
        let mut info = Vec::new();
        let nb = layer.name.as_bytes();
        let nlen = nb.len().min(u16::MAX as usize);
        info.extend_from_slice(&(nlen as u16).to_le_bytes());
        info.extend_from_slice(&nb[..nlen]);
        info.extend_from_slice(&layer.offset_x.to_le_bytes());
        info.extend_from_slice(&layer.offset_y.to_le_bytes());
        info.extend_from_slice(&layer.width.to_le_bytes());
        info.extend_from_slice(&layer.height.to_le_bytes());
        info.push(layer.opacity);
        info.push(encode_blend_mode(layer.blend_mode));
        let mut flags = 0u8;
        if layer.visible {
            flags |= 0x01;
        }
        if layer.locked {
            flags |= 0x02;
        }
        info.push(flags);
        info.extend_from_slice(&(layer.color_model as u16).to_le_bytes());

        let mut pxls = vec![layer.compression as u8];
        pxls.extend_from_slice(&encode_cpt_channel(&layer.raw_pixels, layer.compression)?);

        let mut lch = vec![
            RiffNode::chunk(*b"info", info),
            RiffNode::chunk(*b"pxls", pxls),
        ];
        if let Some((mcomp, ref mraw)) = layer.alpha_mask {
            let mut mchunk = vec![mcomp as u8];
            mchunk.extend_from_slice(&encode_cpt_channel(mraw, mcomp)?);
            lch.push(RiffNode::chunk(*b"mask", mchunk));
        }
        nodes.push(RiffNode::list(*b"layr", lch));
    }

    write_riff(&RiffDocument {
        endian: RiffEndian::Little,
        form_type: *b"CPT ",
        children: nodes,
    })
}
