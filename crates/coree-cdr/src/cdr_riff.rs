// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room RIFF CDR (v4–v13 / X3) parser and writer with `LIST cmpr` Zlib
//! support, full geometry/style/11-Lens/text/bitmap fidelity, CP1256 legacy
//! Arabic decoding, and lossless Unknown-Chunk Passthrough.

use crate::coord::{
    cdr32_to_mm, contains_arabic, decode_cdr_color, decode_cdr_text, mm_to_cdr32,
    read_tagged_color, write_tagged_color, CODEPAGE_ARABIC_CP1256, CODEPAGE_UTF16LE, CODEPAGE_UTF8,
};
use crate::error::{CdrError, CdrResult};
use crate::limits::{ByteReader, CodecLimits};
use crate::riff::{
    fourcc_to_string, parse_list_body, parse_riff_with_budget, serialize_list_body, write_riff,
    RiffDocument, RiffEndian, RiffNode,
};
use crate::zcf::{parse_c9_style_override, CdssStyle, ZcfContext};
use coree_core::{
    math::{sanitize_f32, sanitize_f64},
    Affine2D, ArrowheadStyle, ArtisticText, BasicShapeKind, BezierPath, BlendMode, ColorMapMode,
    ColorMode, ColorSwatch, CornerStyle, Document, EllipseMode, Fill, GradientStop, ImageAssetId,
    ImpactStyle, Layer, LayerId, LensConfig, LensKind, LineCap, LineJoin, MasterLayerScope,
    MeshNode, NodeKind, NodePayload, ObjectId, OutlineStyle, Page, PageId, ParagraphText,
    ParametricShape, PathNode, Point2D, RasterAsset, RasterLayerData, Rect2D, SceneObject, Subpath,
    TextAlign, TextDirection, TextRun, TextStyle, UnknownCdrChunk, Vec2D,
};
use std::collections::{HashMap, HashSet};

// LODA argument type IDs
const LODA_ARG_RECTANGLE: u32 = 0x0001;
const LODA_ARG_ELLIPSE: u32 = 0x0002;
const LODA_ARG_POLYGON: u32 = 0x0003;
const LODA_ARG_STAR: u32 = 0x0004;
const LODA_ARG_BITMAP_REF: u32 = 0x0005;
const LODA_ARG_TEXT_META: u32 = 0x0006;
const LODA_ARG_OUTLINE_ID: u32 = 0x000A;
const LODA_ARG_FILL_ID: u32 = 0x0014;
const LODA_ARG_CURVE: u32 = 0x001E;
const LODA_ARG_STYLE_ID: u32 = 0x00C8;
const LODA_ARG_STYLE_JSON: u32 = 0x00C9;
const LODA_ARG_OBJ_PROPS: u32 = 0x00D0;
const LODA_ARG_EXTRA_SHAPE: u32 = 0x00D1;
const LODA_ARG_NAME: u32 = 0x03E8;

// Authentic CorelDRAW LODA chunk types (offset 16 in 20-byte LODA header)
const CDR_CTYPE_GROUP_OR_LAYER: u32 = 0x00;
const CDR_CTYPE_RECTANGLE: u32 = 0x01;
const CDR_CTYPE_ELLIPSE: u32 = 0x02;
const CDR_CTYPE_CURVE: u32 = 0x03;
const CDR_CTYPE_POLYGON: u32 = 0x04;
const CDR_CTYPE_BITMAP: u32 = 0x05;
const CDR_CTYPE_ARTISTIC_TEXT: u32 = 0x06;
const CDR_CTYPE_PARAGRAPH_TEXT: u32 = 0x10;
const CDR_CTYPE_LAYER_GRID: u32 = 0x0B;
const CDR_CTYPE_LAYER_GUIDES: u32 = 0x0C;
const CDR_CTYPE_LAYER_DESKTOP: u32 = 0x11;

// Curve node flags (bitmask)
const NODE_KIND_MASK: u8 = 0xC0;
const NODE_MOVE_TO: u8 = 0x00;
const NODE_LINE_TO: u8 = 0x40;
const NODE_CURVE_TO: u8 = 0x80;
const NODE_CTRL_PT: u8 = 0xC0;
const NODE_CONT_MASK: u8 = 0x30;
const NODE_CONT_CUSP: u8 = 0x00;
const NODE_CONT_SMOOTH: u8 = 0x10;
const NODE_CONT_SYMMETRICAL: u8 = 0x20;
const NODE_FLAG_CLOSED: u8 = 0x08;
const NODE_CTRL_PRESENT: u8 = 0x01;
/// Marker bit written by clean-room `encode_curve_points` when one of the two cubic
/// control handles is absent (`None`), distinguishing it from real CorelDRAW `0xC0`
/// control points (which always represent present off-curve Bezier handles).
const NODE_CTRL_ABSENT: u8 = 0x02;

// Tagged bitmap chunk header magic
const CBMP_MAGIC: &[u8; 4] = b"CBMP";

/// Options for writing RIFF CDR streams.
#[derive(Debug, Clone, Copy)]
pub struct RiffWriteOptions {
    /// Four-byte RIFF form type (e.g. `*b"CDRC"` for v12, `*b"CDRD"` for X3, `*b"CDTC"` for template).
    pub form_type: [u8; 4],
    /// Version number written into the `vrsn` chunk (e.g. `1200`, `1300`, `1400`, `1600`).
    pub version: u16,
    /// Whether to wrap `LIST doc ` inside a Corel `LIST cmpr` Zlib + `CPng` block.
    pub compress_doc: bool,
    /// When `true`, emit embedded bitmaps as external references (`bmp ` omitted from RIFF stream
    /// and returned via [`RiffWriteOutput::external_bitmaps`] for CDR X6+ `content/data/*.dat`).
    pub externalize_bitmaps: bool,
}

impl Default for RiffWriteOptions {
    fn default() -> Self {
        Self {
            form_type: *b"CDRC",
            version: 1200,
            compress_doc: true,
            externalize_bitmaps: false,
        }
    }
}

/// Result of parsing a RIFF CDR/CDT stream.
#[derive(Debug, Clone)]
pub struct ParsedRiffCdr {
    pub document: Document,
    pub version: u16,
    pub is_template: bool,
    pub warnings: Vec<String>,
    pub riff_tree: RiffDocument,
}

/// Output from [`write_cdr_riff_with_options`], optionally separating externalized raster payloads
/// for CDR X6+ ZIP containers.
#[derive(Debug, Clone)]
pub struct RiffWriteOutput {
    pub riff_bytes: Vec<u8>,
    /// Pairs of `(external_id, raw_bmp_chunk_payload)` when `externalize_bitmaps` is `true`.
    pub external_bitmaps: Vec<(u32, Vec<u8>)>,
}

/// Returns `true` if `form` is a CorelDRAW RIFF CDR form type (`CDR3`..`CDRZ`, `CDR `).
#[must_use]
pub fn is_cdr_form_type(form: &[u8; 4]) -> bool {
    &form[0..3] == b"CDR" && (form[3].is_ascii_alphanumeric() || form[3] == b' ')
}

/// Returns `true` if `form` is a CorelDRAW Template RIFF form type (`CDT4`..`CDTZ`, `CDT `).
#[must_use]
pub fn is_cdt_form_type(form: &[u8; 4]) -> bool {
    &form[0..3] == b"CDT" && (form[3].is_ascii_alphanumeric() || form[3] == b' ')
}

/// Derives a numeric Corel version (e.g. `1200` for `CDRC`, `1300` for `CDRD`) from a form FourCC.
#[must_use]
pub fn version_from_form_type(form: &[u8; 4]) -> u16 {
    let suffix = form[3];
    match suffix {
        b'3'..=b'9' => u16::from(suffix - b'0') * 100,
        b'A'..=b'Z' => (10u16 + u16::from(suffix - b'A')) * 100,
        _ => 1200,
    }
}

// =============================================================================
// Shared DIB / BMP Encoding & Decoding (Pure Rust, Security Hardened)
// =============================================================================

/// Encodes an RGBA8 pixel buffer into a standard bottom-up 32-bit BGRA Windows DIB
/// (`BITMAPINFOHEADER` 40 bytes + pixel rows).
pub fn encode_dib_rgba(width: u32, height: u32, rgba: &[u8]) -> CdrResult<Vec<u8>> {
    let expected = usize::try_from(
        u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|n| n.checked_mul(4))
            .ok_or(CdrError::ArithmeticOverflow("DIB encode size"))?,
    )
    .map_err(|_| CdrError::ArithmeticOverflow("DIB encode usize"))?;
    if rgba.len() != expected {
        return Err(CdrError::InvalidChunk {
            fourcc: "bmp ".to_owned(),
            reason: format!("RGBA length {} != expected {expected}", rgba.len()),
        });
    }
    let height_i32 = i32::try_from(height)
        .map_err(|_| CdrError::ArithmeticOverflow("DIB height exceeds i32"))?;
    let width_i32 =
        i32::try_from(width).map_err(|_| CdrError::ArithmeticOverflow("DIB width exceeds i32"))?;
    let pixel_size_u32 = u32::try_from(expected)
        .map_err(|_| CdrError::ArithmeticOverflow("DIB image size exceeds u32"))?;

    let mut out = Vec::with_capacity(40usize.saturating_add(expected));
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&width_i32.to_le_bytes()); // biWidth
    out.extend_from_slice(&height_i32.to_le_bytes()); // biHeight (bottom-up)
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&pixel_size_u32.to_le_bytes()); // biSizeImage
    out.extend_from_slice(&11_811i32.to_le_bytes()); // biXPelsPerMeter (~300 DPI)
    out.extend_from_slice(&11_811i32.to_le_bytes()); // biYPelsPerMeter (~300 DPI)
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    let row_bytes = (width as usize) * 4;
    for y in (0..height as usize).rev() {
        let row_start = y * row_bytes;
        let row = &rgba[row_start..row_start + row_bytes];
        let (pixels_chunks, _) = row.as_chunks::<4>();
        for px in pixels_chunks {
            out.push(px[2]); // B
            out.push(px[1]); // G
            out.push(px[0]); // R
            out.push(px[3]); // A
        }
    }
    Ok(out)
}

/// Encodes an RGBA8 buffer as a complete `.bmp` file (14-byte `BITMAPFILEHEADER` + DIB).
pub fn encode_bmp_file_rgba(width: u32, height: u32, rgba: &[u8]) -> CdrResult<Vec<u8>> {
    let dib = encode_dib_rgba(width, height, rgba)?;
    let total_size = u32::try_from(dib.len().saturating_add(14))
        .map_err(|_| CdrError::ArithmeticOverflow("BMP file size"))?;
    let mut out = Vec::with_capacity(dib.len().saturating_add(14));
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&total_size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    out.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset (14 + 40)
    out.extend_from_slice(&dib);
    Ok(out)
}

/// Decodes a Windows DIB (or full `"BM"` BMP file) into `(width, height, rgba_bytes)`
/// enforcing `limits.validate_bitmap_dims` and `max_uncompressed_bytes` BEFORE allocating.
pub fn decode_dib_to_rgba(raw: &[u8], limits: &CodecLimits) -> CdrResult<(u32, u32, Vec<u8>)> {
    let mut budget = limits.max_uncompressed_bytes;
    decode_dib_to_rgba_with_budget(raw, limits, &mut budget)
}

/// Decodes a Windows DIB/BMP while deducting the resulting RGBA allocation from a
/// shared cumulative document `budget_remaining` BEFORE allocating pixel memory.
pub fn decode_dib_to_rgba_with_budget(
    raw: &[u8],
    limits: &CodecLimits,
    budget_remaining: &mut u64,
) -> CdrResult<(u32, u32, Vec<u8>)> {
    let mut dib = raw;
    let mut explicit_pixel_offset: Option<usize> = None;

    // Strip optional 14-byte BITMAPFILEHEADER ("BM")
    if raw.len() >= 14 && &raw[0..2] == b"BM" {
        let mut hdr = ByteReader::new(raw);
        hdr.skip(10, "BMP file header")?;
        let off = hdr.read_u32_le("BMP pixel offset")? as usize;
        if off < 14 || off > raw.len() {
            return Err(CdrError::InvalidChunk {
                fourcc: "bmp ".to_owned(),
                reason: "BMP pixel data offset out of bounds".to_owned(),
            });
        }
        explicit_pixel_offset = Some(off - 14);
        dib = &raw[14..];
    }

    let mut r = ByteReader::new(dib);
    let header_size = r.read_u32_le("DIB biSize")? as usize;
    if header_size < 40 || header_size > dib.len() {
        return Err(CdrError::InvalidChunk {
            fourcc: "bmp ".to_owned(),
            reason: format!("unsupported or truncated DIB header size {header_size}"),
        });
    }

    let width_i32 = r.read_i32_le("DIB biWidth")?;
    let height_i32 = r.read_i32_le("DIB biHeight")?;
    let _planes = r.read_u16_le("DIB biPlanes")?;
    let bpp = r.read_u16_le("DIB biBitCount")?;
    let compression = r.read_u32_le("DIB biCompression")?;
    let _size_image = r.read_u32_le("DIB biSizeImage")?;
    let _x_ppm = r.read_i32_le("DIB biXPelsPerMeter")?;
    let _y_ppm = r.read_i32_le("DIB biYPelsPerMeter")?;
    let clr_used = r.read_u32_le("DIB biClrUsed")?;
    let _clr_important = r.read_u32_le("DIB biClrImportant")?;

    if width_i32 <= 0 || height_i32 == 0 {
        return Err(CdrError::InvalidBitmapDimensions {
            width: width_i32.unsigned_abs(),
            height: height_i32.unsigned_abs(),
            max_dim: limits.max_bitmap_dim,
        });
    }
    let width = width_i32 as u32;
    let height = height_i32.unsigned_abs();
    let top_down = height_i32 < 0;

    // Validate dimensions & total RGBA byte budget BEFORE allocating!
    let rgba_len = limits.validate_bitmap_dims(width, height)?;

    if !matches!(bpp, 1 | 8 | 24 | 32) {
        return Err(CdrError::InvalidChunk {
            fourcc: "bmp ".to_owned(),
            reason: format!("unsupported DIB bit depth {bpp}"),
        });
    }

    // Parse optional 32-bit BI_BITFIELDS (compression == 3) channel masks
    let bitfields: Option<(u32, u32, u32, u32)> = match compression {
        0 => {
            r.seek(header_size, "DIB post-header")?;
            None
        }
        3 => {
            if bpp != 32 {
                return Err(CdrError::InvalidChunk {
                    fourcc: "bmp ".to_owned(),
                    reason: format!(
                        "BI_BITFIELDS compression requires 32-bpp DIB, found {bpp}-bpp"
                    ),
                });
            }
            let masks = if header_size >= 56 {
                // BITMAPV3/V4/V5HEADER embeds R, G, B, A masks at offsets 40..56 inside the header
                r.seek(40, "DIB V4 bitfields")?;
                let rm = r.read_u32_le("DIB red mask")?;
                let gm = r.read_u32_le("DIB green mask")?;
                let bm = r.read_u32_le("DIB blue mask")?;
                let am = r.read_u32_le("DIB alpha mask")?;
                r.seek(header_size, "DIB V4 post-header")?;
                (rm, gm, bm, am)
            } else {
                // Standard 40-byte BITMAPINFOHEADER stores 3 x u32 masks immediately after the header
                r.seek(header_size, "DIB BI_BITFIELDS masks")?;
                let rm = r.read_u32_le("DIB red mask")?;
                let gm = r.read_u32_le("DIB green mask")?;
                let bm = r.read_u32_le("DIB blue mask")?;
                (rm, gm, bm, 0u32)
            };
            if masks.0 == 0 || masks.1 == 0 || masks.2 == 0 {
                return Err(CdrError::InvalidChunk {
                    fourcc: "bmp ".to_owned(),
                    reason: "BI_BITFIELDS RGB channel masks must be non-zero".to_owned(),
                });
            }
            Some(masks)
        }
        other => {
            return Err(CdrError::InvalidChunk {
                fourcc: "bmp ".to_owned(),
                reason: format!("unsupported DIB compression mode {other}"),
            });
        }
    };

    // Optional color palette for 1-bit / 8-bit indexed images
    let palette_entries = if bpp <= 8 {
        if clr_used > 0 {
            (clr_used as usize).min(256)
        } else {
            1usize << usize::from(bpp)
        }
    } else {
        0
    };
    let mut palette: Vec<[u8; 4]> = Vec::with_capacity(palette_entries);
    for _ in 0..palette_entries {
        let b = r.read_u8("DIB palette B")?;
        let g = r.read_u8("DIB palette G")?;
        let red = r.read_u8("DIB palette R")?;
        let _pad = r.read_u8("DIB palette pad")?;
        palette.push([red, g, b, 255]);
    }

    if let Some(off) = explicit_pixel_offset {
        r.seek(off, "DIB explicit pixel start")?;
    }

    let row_bits = u64::from(width)
        .checked_mul(u64::from(bpp))
        .ok_or(CdrError::ArithmeticOverflow("DIB row bits"))?;
    let row_stride_u64 = row_bits
        .checked_add(31)
        .ok_or(CdrError::ArithmeticOverflow("DIB row stride"))?
        / 32
        * 4;
    let row_stride = usize::try_from(row_stride_u64)
        .map_err(|_| CdrError::ArithmeticOverflow("DIB row stride usize"))?;

    let needed_pixel_bytes = row_stride
        .checked_mul(height as usize)
        .ok_or(CdrError::ArithmeticOverflow("DIB pixel byte total"))?;
    let pixels = r.read_bytes(needed_pixel_bytes, "DIB pixel data")?;

    // Charge decoded RGBA allocation against the cumulative document budget BEFORE allocating!
    limits.consume_budget(budget_remaining, rgba_len as u64)?;

    let mut rgba = vec![0u8; rgba_len];
    let w_usize = width as usize;
    let h_usize = height as usize;

    for src_row in 0..h_usize {
        let dst_y = if top_down {
            src_row
        } else {
            h_usize - 1 - src_row
        };
        let src_base = src_row * row_stride;
        let dst_base = dst_y * w_usize * 4;
        match bpp {
            32 => {
                if let Some((rm, gm, bm, am)) = bitfields {
                    for x in 0..w_usize {
                        let s = src_base + x * 4;
                        let d = dst_base + x * 4;
                        let px = u32::from_le_bytes([
                            pixels[s],
                            pixels[s + 1],
                            pixels[s + 2],
                            pixels[s + 3],
                        ]);
                        rgba[d] = extract_bitfield_channel(px, rm);
                        rgba[d + 1] = extract_bitfield_channel(px, gm);
                        rgba[d + 2] = extract_bitfield_channel(px, bm);
                        rgba[d + 3] = if am != 0 {
                            extract_bitfield_channel(px, am)
                        } else {
                            255
                        };
                    }
                } else {
                    for x in 0..w_usize {
                        let s = src_base + x * 4;
                        let d = dst_base + x * 4;
                        rgba[d] = pixels[s + 2]; // R
                        rgba[d + 1] = pixels[s + 1]; // G
                        rgba[d + 2] = pixels[s]; // B
                        rgba[d + 3] = pixels[s + 3]; // A
                    }
                }
            }
            24 => {
                for x in 0..w_usize {
                    let s = src_base + x * 3;
                    let d = dst_base + x * 4;
                    rgba[d] = pixels[s + 2];
                    rgba[d + 1] = pixels[s + 1];
                    rgba[d + 2] = pixels[s];
                    rgba[d + 3] = 255;
                }
            }
            8 => {
                for x in 0..w_usize {
                    let idx = usize::from(pixels[src_base + x]);
                    let color = palette.get(idx).copied().unwrap_or([0, 0, 0, 255]);
                    let d = dst_base + x * 4;
                    rgba[d..d + 4].copy_from_slice(&color);
                }
            }
            1 => {
                for x in 0..w_usize {
                    let byte = pixels[src_base + (x / 8)];
                    let bit = (byte >> (7 - (x % 8))) & 1;
                    let color = palette
                        .get(usize::from(bit))
                        .copied()
                        .unwrap_or(if bit == 1 {
                            [255, 255, 255, 255]
                        } else {
                            [0, 0, 0, 255]
                        });
                    let d = dst_base + x * 4;
                    rgba[d..d + 4].copy_from_slice(&color);
                }
            }
            other => {
                return Err(CdrError::InvalidChunk {
                    fourcc: "bmp ".to_owned(),
                    reason: format!("unsupported DIB bit depth {other}"),
                });
            }
        }
    }

    Ok((width, height, rgba))
}

#[inline]
fn extract_bitfield_channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max_val = mask >> shift;
    if max_val == 0 {
        return 0;
    }
    let raw = (pixel & mask) >> shift;
    if max_val == 255 {
        raw as u8
    } else {
        ((u64::from(raw) * 255 + u64::from(max_val) / 2) / u64::from(max_val)).min(255) as u8
    }
}

// =============================================================================
// RIFF CDR Parser
// =============================================================================

#[derive(Debug, Clone)]
struct ParsedStyleEntry {
    style: TextStyle,
    codepage: Option<u16>,
    charset: Option<u8>,
}

struct ParseContext<'a> {
    limits: &'a CodecLimits,
    budget_remaining: &'a mut u64,
    endian: RiffEndian,
    version: u16,
    is_zcf: bool,
    doc_page_width_mm: f64,
    doc_page_height_mm: f64,
    current_page_w_mm: f64,
    current_page_h_mm: f64,
    zcf: &'a ZcfContext,
    fills: HashMap<u32, Fill>,
    normalized_fills: HashSet<u32>,
    outlines: HashMap<u32, OutlineStyle>,
    outlines_explicit_none: HashSet<u32>,
    styles: HashMap<u32, ParsedStyleEntry>,
    bitmaps: HashMap<u32, ImageAssetId>,
    text_streams: HashMap<u32, Vec<u8>>,
    master_layers: Vec<Layer>,
    warnings: Vec<String>,
}

/// Parses a RIFF CDR/CDT byte buffer (optionally providing externalized bitmap chunks
/// resolved from a CDR X6+ ZIP container's `content/data/*.dat`).
pub fn parse_cdr_riff(
    bytes: &[u8],
    limits: &CodecLimits,
    external_bitmaps: &HashMap<u32, Vec<u8>>,
) -> CdrResult<ParsedRiffCdr> {
    let mut budget = limits.max_uncompressed_bytes;
    parse_cdr_riff_with_budget(bytes, limits, external_bitmaps, &mut budget, true)
}

/// Parses a RIFF CDR/CDT byte buffer sharing a cumulative `budget_remaining` across
/// ZIP decompression, `LIST cmpr` streams, and decoded raster bitmap allocations.
pub fn parse_cdr_riff_with_budget(
    bytes: &[u8],
    limits: &CodecLimits,
    external_bitmaps: &HashMap<u32, Vec<u8>>,
    budget_remaining: &mut u64,
    charge_input_bytes: bool,
) -> CdrResult<ParsedRiffCdr> {
    let riff_tree = parse_riff_with_budget(bytes, limits, budget_remaining, charge_input_bytes)?;
    let default_zcf = ZcfContext::default();
    parse_cdr_riff_tree(
        riff_tree,
        limits,
        external_bitmaps,
        budget_remaining,
        &default_zcf,
    )
}

/// Decodes a pre-parsed (and for ZCF v16–26, already dereferenced) [`RiffDocument`]
/// into a [`ParsedRiffCdr`], merging any document-wide [`ZcfContext`] resources
/// (`styles/document.cdss`, `META-INF/textinfo.xml`, metadata page dimensions).
pub(crate) fn parse_cdr_riff_tree(
    riff_tree: RiffDocument,
    limits: &CodecLimits,
    external_bitmaps: &HashMap<u32, Vec<u8>>,
    budget_remaining: &mut u64,
    zcf: &ZcfContext,
) -> CdrResult<ParsedRiffCdr> {
    let is_template = is_cdt_form_type(&riff_tree.form_type);
    if !is_cdr_form_type(&riff_tree.form_type) && !is_template {
        return Err(CdrError::UnsupportedFormat(format!(
            "RIFF form '{}' is not a .cdr/.cdt container",
            fourcc_to_string(&riff_tree.form_type)
        )));
    }

    let mut version = version_from_form_type(&riff_tree.form_type);
    if let Some(v) = zcf.version_hint {
        if v > 0 {
            version = v;
        }
    }
    if let Some(fver_bytes) = riff_tree.find_chunk(b"fver") {
        if fver_bytes.len() >= 2 {
            let v = u16::from_le_bytes([fver_bytes[0], fver_bytes[1]]);
            if v > 0 && v != u16::MAX {
                version = v;
            }
        }
    }
    if let Some(vrsn_bytes) = riff_tree.find_chunk(b"vrsn") {
        if vrsn_bytes.len() >= 2 {
            let mut vr = ByteReader::new(vrsn_bytes);
            if let Ok(v) = vr.read_u16_le("vrsn") {
                if v > 0 && v != u16::MAX {
                    version = v;
                }
            }
        }
    }

    let (init_w, init_h) = zcf.metadata_page_size_mm.unwrap_or((210.0, 297.0));
    let mut doc = Document::new_a4("Imported CDR");
    doc.pages.clear();
    doc.metadata.source_format = Some(if is_template {
        format!("CDT RIFF v{}", version / 100)
    } else {
        format!("CDR RIFF v{}", version / 100)
    });

    let mut ctx = ParseContext {
        limits,
        budget_remaining,
        endian: riff_tree.endian,
        version,
        is_zcf: zcf.is_zcf || &riff_tree.form_type == b"CDRO" || &riff_tree.form_type == b"CDTO",
        doc_page_width_mm: init_w,
        doc_page_height_mm: init_h,
        current_page_w_mm: init_w,
        current_page_h_mm: init_h,
        zcf,
        fills: HashMap::new(),
        normalized_fills: HashSet::new(),
        outlines: HashMap::new(),
        outlines_explicit_none: HashSet::new(),
        styles: HashMap::new(),
        bitmaps: HashMap::new(),
        text_streams: HashMap::new(),
        master_layers: Vec::new(),
        warnings: Vec::new(),
    };

    // Pre-register any external bitmaps supplied by a CDR X6+ ZIP container
    for (&ext_id, payload) in external_bitmaps {
        if let Err(e) = register_bmp_chunk_payload(payload, Some(ext_id), &mut doc, &mut ctx) {
            // Propagate hard security errors (huge bitmap dims / size limits)
            if matches!(
                e,
                CdrError::InvalidBitmapDimensions { .. }
                    | CdrError::UncompressedSizeLimitExceeded { .. }
            ) {
                return Err(e);
            }
            ctx.warnings
                .push(format!("skipped external bitmap #{ext_id}: {e}"));
        }
    }

    // Walk top-level children: unwrap `LIST cmpr` transparently to reach `LIST doc ` & top-level `LIST page`
    let mut found_doc_list = false;
    let mut is_template_flag = is_template;
    let mut top_nodes = Vec::new();
    flatten_cmpr_nodes(&riff_tree.children, &mut top_nodes);

    // Pass 1 (top-level): parse `LIST doc ` and any top-level metadata/resources BEFORE pages
    for &node in &top_nodes {
        match node {
            RiffNode::Chunk { fourcc, data, .. } => match fourcc {
                b"vrsn" | b"fver" | b"DISP" => {}
                b"tmpt" => {
                    is_template_flag = true;
                }
                _ => {
                    doc.unknown_doc_chunks.push(UnknownCdrChunk {
                        fourcc: *fourcc,
                        version_hint: u32::from(version),
                        raw_bytes: data.clone(),
                    });
                }
            },
            RiffNode::List {
                list_type,
                children,
                ..
            } => match list_type {
                b"doc " => {
                    found_doc_list = true;
                    parse_doc_list(children, &mut doc, &mut ctx)?;
                }
                b"page" => {
                    // Parsed in Pass 2 after `LIST doc ` fills/outlines/mcfg are loaded
                }
                _ => {
                    // Preserve unknown top-level LIST verbatim (list_type + full nested structure)
                    let raw = serialize_list_body(list_type, children, ctx.endian)?;
                    doc.unknown_doc_chunks.push(UnknownCdrChunk {
                        fourcc: *b"LIST",
                        version_hint: u32::from(version),
                        raw_bytes: raw,
                    });
                }
            },
        }
    }

    // Pass 2 (top-level): parse top-level `LIST page` containers (CorelDRAW X6–2025 `CDRO` layout)
    for &node in &top_nodes {
        if let RiffNode::List {
            list_type,
            children: page_ch,
            ..
        } = node
        {
            if list_type == b"page" {
                parse_page_list(page_ch, &mut doc, &mut ctx)?;
            }
        }
    }

    if !found_doc_list {
        ctx.warnings
            .push("CDR stream contained no 'LIST doc ' container; initialized default page".into());
    }

    if doc.pages.is_empty() {
        let pid = PageId(doc.alloc_id());
        let lid = LayerId(doc.alloc_id());
        doc.pages.push(Page::new(
            pid,
            "Page 1",
            ctx.doc_page_width_mm,
            ctx.doc_page_height_mm,
            3.0,
            lid,
        ));
    }

    // Attach any non-empty Master Page layers onto the first document page
    if !ctx.master_layers.is_empty() {
        doc.pages[0].layers.extend(ctx.master_layers);
    }

    // Apply optional `META-INF/textinfo.xml` page names when binary chunks had defaults
    for (idx, pname) in ctx.zcf.textinfo.page_names.iter().enumerate() {
        if let Some(page) = doc.pages.get_mut(idx) {
            if !pname.is_empty() {
                page.name = pname.clone();
            }
        }
    }

    if doc.document_palette.swatches.is_empty() {
        doc.refresh_document_palette();
    }

    Ok(ParsedRiffCdr {
        document: doc,
        version,
        is_template: is_template_flag,
        warnings: ctx.warnings,
        riff_tree,
    })
}

fn flatten_cmpr_nodes<'a>(nodes: &'a [RiffNode], out: &mut Vec<&'a RiffNode>) {
    for node in nodes {
        if let RiffNode::List {
            list_type,
            children,
            ..
        } = node
        {
            if list_type == b"cmpr" {
                flatten_cmpr_nodes(children, out);
                continue;
            }
        }
        out.push(node);
    }
}

fn parse_doc_list(
    children: &[RiffNode],
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<()> {
    // Flatten any nested `LIST cmpr` blocks BEFORE Pass 1 & Pass 2 so compressed pages
    // can resolve later sibling resource tables (`LIST fill`/`filt`/`outl`/`otlt`/`stlt`/`bmps`/`bmpt`).
    let mut flat_nodes = Vec::new();
    flatten_cmpr_nodes(children, &mut flat_nodes);

    // Pass 1: load ALL shared document resources
    for &node in &flat_nodes {
        match node {
            RiffNode::Chunk { fourcc, data, .. } => match fourcc {
                b"mcfg" => parse_doc_mcfg(data, doc, ctx),
                b"plte" => parse_plte_chunk(data, doc, ctx),
                b"fild" => {
                    if let Ok((id, fill, is_norm)) = parse_fild_chunk(data, ctx.is_zcf) {
                        if is_norm {
                            ctx.normalized_fills.insert(id);
                        }
                        ctx.fills.insert(id, fill);
                    }
                }
                b"outl" => {
                    if let Ok((id, outl, is_none)) = parse_outl_chunk(data, ctx.is_zcf) {
                        if is_none {
                            ctx.outlines_explicit_none.insert(id);
                        }
                        ctx.outlines.insert(id, outl);
                    }
                }
                b"stlt" => {
                    if let Ok((id, entry)) = parse_stlt_chunk(data) {
                        ctx.styles.insert(id, entry);
                    }
                }
                b"bmp " => {
                    register_bmp_chunk_payload(data, None, doc, ctx)?;
                }
                b"vrsn" | b"fver" | b"DISP" | b"tmpt" | b"pref" | b"ptrt" | b"flgs" | b"spid"
                | b"bbox" | b"obbx" | b"usdn" => {}
                _ => {
                    doc.unknown_doc_chunks.push(UnknownCdrChunk {
                        fourcc: *fourcc,
                        version_hint: u32::from(ctx.version),
                        raw_bytes: data.clone(),
                    });
                }
            },
            RiffNode::List {
                list_type,
                children: list_ch,
                ..
            } => match list_type {
                b"fill" | b"filt" => {
                    for ch in list_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = ch {
                            if fourcc == b"fild" {
                                if let Ok((id, fill, is_norm)) = parse_fild_chunk(data, ctx.is_zcf)
                                {
                                    if is_norm {
                                        ctx.normalized_fills.insert(id);
                                    }
                                    ctx.fills.insert(id, fill);
                                }
                            }
                        }
                    }
                }
                b"outl" | b"otlt" => {
                    for ch in list_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = ch {
                            if fourcc == b"outl" {
                                if let Ok((id, outl, is_none)) = parse_outl_chunk(data, ctx.is_zcf)
                                {
                                    if is_none {
                                        ctx.outlines_explicit_none.insert(id);
                                    }
                                    ctx.outlines.insert(id, outl);
                                }
                            }
                        }
                    }
                }
                b"stlt" | b"styd" | b"font" => {
                    for ch in list_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = ch {
                            if fourcc == b"stlt" {
                                if let Ok((id, entry)) = parse_stlt_chunk(data) {
                                    ctx.styles.insert(id, entry);
                                }
                            }
                        }
                    }
                }
                b"bmps" | b"bmpt" => {
                    for ch in list_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = ch {
                            if fourcc == b"bmp " {
                                if let Err(e) = register_bmp_chunk_payload(data, None, doc, ctx) {
                                    if matches!(
                                        e,
                                        CdrError::InvalidBitmapDimensions { .. }
                                            | CdrError::UncompressedSizeLimitExceeded { .. }
                                    ) {
                                        return Err(e);
                                    }
                                    ctx.warnings.push(format!("skipped document bitmap: {e}"));
                                }
                            }
                        }
                    }
                }
                b"txsm" => {
                    // Document-level text stream table (CorelDRAW v12–26 `LIST txsm`)
                    for ch in list_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = ch {
                            if (fourcc == b"txsm" || fourcc == b"txst") && data.len() >= 8 {
                                let tid = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                                ctx.text_streams.insert(tid, data.clone());
                            }
                        }
                    }
                }
                b"page" | b"arrw" | b"bmpf" | b"osmk" | b"vpat" | b"ppdt" | b"clrs" => {}
                _ => {
                    // Preserve unknown document-level LIST verbatim!
                    let raw = serialize_list_body(list_type, list_ch, ctx.endian)?;
                    doc.unknown_doc_chunks.push(UnknownCdrChunk {
                        fourcc: *b"LIST",
                        version_hint: u32::from(ctx.version),
                        raw_bytes: raw,
                    });
                }
            },
        }
    }

    // Pass 2: parse ALL pages inside `LIST doc ` (classic RIFF / X4–X5 / synthetic layout)
    for &node in &flat_nodes {
        if let RiffNode::List {
            list_type,
            children: page_ch,
            ..
        } = node
        {
            if list_type == b"page" {
                parse_page_list(page_ch, doc, ctx)?;
            }
        }
    }

    Ok(())
}

fn parse_doc_mcfg(data: &[u8], doc: &mut Document, ctx: &mut ParseContext<'_>) {
    // 1. Authentic CorelDRAW v16–26 ZCF `mcfg` (large struct >= 64 bytes; page size at offset 12..20)
    if data.len() >= 64 {
        let mut r = ByteReader::new(data);
        let first = r.read_u32_le("mcfg hdr0").unwrap_or(0);
        // Real v16+ mcfg stores struct marker (e.g. 2100) at 0..4, body size at 4..8,
        // and page (width, height) in 0.1 µm at 12..20.
        if (100..100_000).contains(&first) && data.len() >= 20 {
            let w_cdr = i32::from_le_bytes([data[12], data[13], data[14], data[15]]);
            let h_cdr = i32::from_le_bytes([data[16], data[17], data[18], data[19]]);
            let wm = cdr32_to_mm(w_cdr);
            let hm = cdr32_to_mm(h_cdr);
            if wm > 1.0 && hm > 1.0 && wm < 50_000.0 && hm < 50_000.0 {
                ctx.doc_page_width_mm = wm;
                ctx.doc_page_height_mm = hm;
                return;
            }
        }
        // Real v7–v15 mcfg stores (width, height) at offset 4..12.
        let w_cdr = i32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let h_cdr = i32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let wm = cdr32_to_mm(w_cdr);
        let hm = cdr32_to_mm(h_cdr);
        if wm > 1.0 && hm > 1.0 && wm < 50_000.0 && hm < 50_000.0 {
            ctx.doc_page_width_mm = wm;
            ctx.doc_page_height_mm = hm;
            return;
        }
    }

    // 2. Clean-room synthetic `mcfg` (compact: w_cdr, h_cdr, color_mode, dpi, title_len, title)
    let mut r = ByteReader::new(data);
    let Ok(w_cdr) = r.read_i32_le("doc mcfg w") else {
        ctx.warnings.push("truncated doc mcfg chunk".into());
        return;
    };
    let Ok(h_cdr) = r.read_i32_le("doc mcfg h") else {
        return;
    };
    let wm = cdr32_to_mm(w_cdr);
    let hm = cdr32_to_mm(h_cdr);
    if wm > 1.0 && hm > 1.0 && wm < 50_000.0 && hm < 50_000.0 {
        ctx.doc_page_width_mm = wm;
        ctx.doc_page_height_mm = hm;
    }
    if let Ok(cm) = r.read_u8("doc mcfg color_mode") {
        doc.color_mode = if cm == 1 {
            ColorMode::Rgb
        } else {
            ColorMode::Cmyk
        };
    }
    if let Ok(dpi) = r.read_f32_le("doc mcfg dpi") {
        doc.dpi = sanitize_f32(dpi, 300.0).max(1.0);
    }
    if let Ok(title_len) = r.read_u16_le("doc mcfg title_len") {
        if let Ok(tb) = r.read_bytes(usize::from(title_len), "doc mcfg title") {
            let t = decode_cdr_text(tb, Some(CODEPAGE_UTF8), None);
            if !t.is_empty() {
                doc.title = t;
            }
        }
    }
}

fn parse_plte_chunk(data: &[u8], doc: &mut Document, ctx: &mut ParseContext<'_>) {
    let mut r = ByteReader::new(data);
    let Ok(count) = r.read_u16_le("plte count") else {
        ctx.warnings.push("truncated plte chunk".into());
        return;
    };
    doc.document_palette.swatches.clear();
    for _ in 0..count {
        let Ok(name_len) = r.read_u16_le("plte swatch name len") else {
            break;
        };
        let Ok(nb) = r.read_bytes(usize::from(name_len), "plte swatch name") else {
            break;
        };
        let name = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
        let Ok(color) = read_tagged_color(&mut r, "plte swatch color") else {
            break;
        };
        doc.document_palette
            .add_swatch(ColorSwatch::new(name, color));
    }
}

fn register_bmp_chunk_payload(
    data: &[u8],
    override_id: Option<u32>,
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<ImageAssetId> {
    let (bmp_id, dpi, color_mode, dib_bytes) = if data.len() >= 13 && &data[0..4] == CBMP_MAGIC {
        let mut r = ByteReader::new(data);
        r.skip(4, "CBMP magic")?;
        let id = override_id.unwrap_or(r.read_u32_le("CBMP id")?);
        let dpi_x100 = r.read_u32_le("CBMP dpi")?;
        let cm_byte = r.read_u8("CBMP color_mode")?;
        let dpi = if dpi_x100 > 0 {
            (dpi_x100 as f32) / 100.0
        } else {
            300.0
        };
        let cm = if cm_byte == 1 {
            ColorMode::Rgb
        } else {
            ColorMode::Cmyk
        };
        (id, dpi, cm, r.read_rest())
    } else {
        let id = override_id.unwrap_or_else(|| (ctx.bitmaps.len() as u32).saturating_add(1));
        (id, 300.0, ColorMode::Rgb, data)
    };

    let (w, h, rgba) = decode_dib_to_rgba_with_budget(dib_bytes, ctx.limits, ctx.budget_remaining)?;
    let asset = RasterAsset::new(w, h, rgba, dpi, color_mode)?;
    let asset_id = doc.register_asset(asset);
    ctx.bitmaps.insert(bmp_id, asset_id);
    Ok(asset_id)
}

fn parse_page_list(
    children: &[RiffNode],
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<()> {
    let page_num = doc.pages.len() + 1;
    let mut page_name = format!("Page {page_num}");
    let mut width_mm = ctx.doc_page_width_mm;
    let mut height_mm = ctx.doc_page_height_mm;
    let mut bleed_mm = 3.0;
    let mut facing_pages = false;
    let mut is_master_page = false;

    for node in children {
        if let RiffNode::Chunk { fourcc, data, .. } = node {
            match fourcc {
                b"mcfg" => {
                    if data.len() >= 64 {
                        if let Some((wm, hm)) = parse_real_page_mcfg(data) {
                            width_mm = wm;
                            height_mm = hm;
                        }
                    } else {
                        let mut r = ByteReader::new(data);
                        if let (Ok(w), Ok(h)) = (r.read_i32_le("page w"), r.read_i32_le("page h")) {
                            let wm = cdr32_to_mm(w);
                            let hm = cdr32_to_mm(h);
                            if wm > 0.1 && hm > 0.1 {
                                width_mm = wm;
                                height_mm = hm;
                            }
                        }
                        if let Ok(b) = r.read_i32_le("page bleed") {
                            bleed_mm = cdr32_to_mm(b).max(0.0);
                        }
                        if let Ok(fp) = r.read_u8("page facing") {
                            facing_pages = fp != 0;
                        }
                        if let Ok(nlen) = r.read_u16_le("page name len") {
                            if let Ok(nb) = r.read_bytes(usize::from(nlen), "page name") {
                                let s = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
                                if !s.is_empty() {
                                    page_name = s;
                                }
                            }
                        }
                    }
                }
                b"flgs" => {
                    if data.len() >= 4 {
                        let flgs = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                        // Bit 0x0001_0000 marks the CorelDRAW Master Page container (`masterPage.dat`)
                        if (flgs & 0x0001_0000) != 0 {
                            is_master_page = true;
                        }
                    }
                }
                b"spid" | b"bbox" | b"obbx" | b"usdn" => {}
                _ => {
                    doc.unknown_doc_chunks.push(UnknownCdrChunk {
                        fourcc: *fourcc,
                        version_hint: u32::from(ctx.version),
                        raw_bytes: data.clone(),
                    });
                }
            }
        }
    }

    ctx.current_page_w_mm = width_mm;
    ctx.current_page_h_mm = height_mm;

    // Collect `LIST layr` children both directly under `LIST page` AND inside `LIST gobj`
    let mut layer_lists: Vec<&[RiffNode]> = Vec::new();
    for node in children {
        if let RiffNode::List {
            list_type,
            children: sub_ch,
            ..
        } = node
        {
            match list_type {
                b"layr" => layer_lists.push(sub_ch.as_slice()),
                b"gobj" => {
                    for gnode in sub_ch {
                        if let RiffNode::List {
                            list_type: glt,
                            children: gch,
                            ..
                        } = gnode
                        {
                            if glt == b"layr" {
                                layer_lists.push(gch.as_slice());
                            }
                        }
                    }
                }
                b"lgob" => {
                    // Optional page-level loda carrying a custom page name
                    for lnode in sub_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = lnode {
                            if fourcc == b"loda" {
                                if let Some(name) = extract_real_loda_name(data) {
                                    page_name = name;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if is_master_page {
        // Master Page: keep only layers that actually carry artwork objects (as MasterLayerScope::AllPages)
        for layr_ch in layer_lists {
            if let Some(mut layer) = parse_layer_to_struct(layr_ch, true, 0, doc, ctx)? {
                if !layer.objects.is_empty() {
                    layer.master_scope = MasterLayerScope::AllPages;
                    ctx.master_layers.push(layer);
                }
            }
        }
        return Ok(());
    }

    let page_id = PageId(doc.alloc_id());
    let page = Page {
        id: page_id,
        name: page_name,
        width_mm,
        height_mm,
        bleed_mm,
        facing_pages,
        layers: Vec::new(),
    };
    doc.pages.push(page);
    let page_idx = doc.pages.len() - 1;

    for layr_ch in layer_lists {
        let next_num = doc.pages[page_idx].layers.len() + 1;
        if let Some(layer) = parse_layer_to_struct(layr_ch, false, next_num, doc, ctx)? {
            doc.pages[page_idx].layers.push(layer);
        }
    }

    if doc.pages[page_idx].layers.is_empty() {
        let fallback_name = ctx
            .zcf
            .textinfo
            .layer_names
            .first()
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Layer 1".to_owned());
        let lid = LayerId(doc.alloc_id());
        doc.pages[page_idx]
            .layers
            .push(Layer::new(lid, fallback_name));
    }

    Ok(())
}

fn parse_real_page_mcfg(data: &[u8]) -> Option<(f64, f64)> {
    if data.len() >= 20 {
        let first = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let (off_w, off_h) = if (100..100_000).contains(&first) {
            (12, 16)
        } else {
            (4, 8)
        };
        let w = cdr32_to_mm(i32::from_le_bytes([
            data[off_w],
            data[off_w + 1],
            data[off_w + 2],
            data[off_w + 3],
        ]));
        let h = cdr32_to_mm(i32::from_le_bytes([
            data[off_h],
            data[off_h + 1],
            data[off_h + 2],
            data[off_h + 3],
        ]));
        if w > 1.0 && h > 1.0 && w < 50_000.0 && h < 50_000.0 {
            return Some((w, h));
        }
    }
    None
}

fn parse_layer_to_struct(
    children: &[RiffNode],
    is_master_page: bool,
    layer_num: usize,
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<Option<Layer>> {
    let layer_id = LayerId(doc.alloc_id());
    let mut layer = Layer::new(layer_id, format!("Layer {}", layer_num.max(1)));
    let mut is_system_layer = false;
    let mut has_explicit_name = false;

    for node in children {
        match node {
            RiffNode::Chunk { fourcc, data, .. } => match fourcc {
                b"layr" | b"loda" => {
                    if is_real_corel_loda(data) {
                        if let Some((ctype, name)) = parse_real_layer_loda(data) {
                            if matches!(
                                ctype,
                                CDR_CTYPE_LAYER_GRID
                                    | CDR_CTYPE_LAYER_GUIDES
                                    | CDR_CTYPE_LAYER_DESKTOP
                            ) {
                                is_system_layer = true;
                                if ctype == CDR_CTYPE_LAYER_GRID || ctype == CDR_CTYPE_LAYER_GUIDES
                                {
                                    layer.printable = false;
                                }
                            }
                            if let Some(n) = name {
                                layer.name = n;
                                has_explicit_name = true;
                            }
                        }
                    } else {
                        let mut r = ByteReader::new(data);
                        if let Ok(flags) = r.read_u8("layer flags") {
                            layer.visible = (flags & 0x01) != 0;
                            layer.locked = (flags & 0x02) != 0;
                            layer.printable = (flags & 0x04) != 0;
                            layer.is_cut_layer = (flags & 0x08) != 0;
                        }
                        if let Ok(ms) = r.read_u8("layer master_scope") {
                            layer.master_scope = match ms {
                                1 => MasterLayerScope::AllPages,
                                2 => MasterLayerScope::OddPages,
                                3 => MasterLayerScope::EvenPages,
                                _ => MasterLayerScope::None,
                            };
                        }
                        if let Ok(nlen) = r.read_u16_le("layer name len") {
                            if let Ok(nb) = r.read_bytes(usize::from(nlen), "layer name") {
                                let s = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
                                if !s.is_empty() {
                                    layer.name = s;
                                    has_explicit_name = true;
                                }
                            }
                        }
                    }
                }
                b"flgs" => {
                    if data.len() >= 4 {
                        let flgs = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                        layer.visible = (flgs & 0x0010) == 0;
                        layer.printable = (flgs & 0x0002) == 0;
                        layer.locked = (flgs & 0x0004) != 0;
                        if (flgs & 0x0001_0000) != 0 {
                            layer.master_scope = MasterLayerScope::AllPages;
                        }
                    }
                }
                b"spid" | b"bbox" | b"obbx" | b"usdn" => {}
                _ => {}
            },
            RiffNode::List {
                list_type: [b'l', b'g', b'o', b'b'],
                children: lgob_ch,
                ..
            } => {
                for gc in lgob_ch {
                    if let RiffNode::Chunk { fourcc, data, .. } = gc {
                        if fourcc == b"loda" {
                            if let Some((ctype, name)) = parse_real_layer_loda(data) {
                                if matches!(
                                    ctype,
                                    CDR_CTYPE_LAYER_GRID
                                        | CDR_CTYPE_LAYER_GUIDES
                                        | CDR_CTYPE_LAYER_DESKTOP
                                ) {
                                    is_system_layer = true;
                                    if ctype == CDR_CTYPE_LAYER_GRID
                                        || ctype == CDR_CTYPE_LAYER_GUIDES
                                    {
                                        layer.printable = false;
                                    }
                                }
                                if let Some(n) = name {
                                    layer.name = n;
                                    has_explicit_name = true;
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !has_explicit_name && !is_system_layer && layer_num > 0 {
        if let Some(ti_name) = ctx.zcf.textinfo.layer_names.get(layer_num - 1) {
            if !ti_name.is_empty() {
                layer.name = ti_name.clone();
            }
        }
    }

    for node in children {
        if let RiffNode::List {
            list_type,
            children: sub_ch,
            ..
        } = node
        {
            match list_type {
                b"lgob" => {}
                b"obj " => {
                    if let Some(oid) = parse_object_list(sub_ch, 1, doc, ctx)? {
                        layer.objects.push(oid);
                    }
                }
                b"grp " => {
                    if let Some(gid) = parse_group_list(sub_ch, 1, doc, ctx)? {
                        layer.objects.push(gid);
                    }
                }
                b"cmpr" => {
                    for inner in sub_ch {
                        if let RiffNode::List {
                            list_type: ilt,
                            children: ich,
                            ..
                        } = inner
                        {
                            match ilt {
                                b"obj " => {
                                    if let Some(oid) = parse_object_list(ich, 1, doc, ctx)? {
                                        layer.objects.push(oid);
                                    }
                                }
                                b"grp " => {
                                    if let Some(gid) = parse_group_list(ich, 1, doc, ctx)? {
                                        layer.objects.push(gid);
                                    }
                                }
                                _ => {
                                    let raw = serialize_list_body(ilt, ich, ctx.endian)?;
                                    doc.unknown_doc_chunks.push(UnknownCdrChunk {
                                        fourcc: *b"LIST",
                                        version_hint: u32::from(ctx.version),
                                        raw_bytes: raw,
                                    });
                                }
                            }
                        }
                    }
                }
                _ => {
                    let raw = serialize_list_body(list_type, sub_ch, ctx.endian)?;
                    doc.unknown_doc_chunks.push(UnknownCdrChunk {
                        fourcc: *b"LIST",
                        version_hint: u32::from(ctx.version),
                        raw_bytes: raw,
                    });
                }
            }
        }
    }

    // Omit empty internal Corel system layers (`Guides`, `Desktop`, `Document Grid`) so real
    // documents present clean user layers; preserve them whenever they actually hold objects.
    if (is_system_layer || is_master_page) && layer.objects.is_empty() {
        return Ok(None);
    }

    Ok(Some(layer))
}

fn parse_group_list(
    children: &[RiffNode],
    depth: usize,
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<Option<ObjectId>> {
    ctx.limits.check_depth(depth)?;
    let mut child_ids = Vec::new();
    let mut name = "Group".to_owned();
    let mut transform = Affine2D::IDENTITY;
    let mut is_real_cdr_group = ctx.is_zcf;
    let mut visible = true;
    let mut locked = false;
    let mut printable = true;
    let mut opacity = 1.0_f32;
    let mut blend_mode = BlendMode::Normal;
    let mut lens: Option<LensConfig> = None;
    let mut unknown_chunks = Vec::new();

    for node in children {
        match node {
            RiffNode::List {
                list_type,
                children: sub_ch,
                ..
            } => match list_type {
                b"lgob" => {
                    is_real_cdr_group = true;
                    for gc in sub_ch {
                        match gc {
                            RiffNode::Chunk {
                                fourcc: [b'l', b'o', b'd', b'a'],
                                data,
                                ..
                            } => {
                                if let Some(n) = extract_real_loda_name(data) {
                                    name = n;
                                }
                            }
                            RiffNode::List {
                                list_type: [b't', b'r', b'f', b'l'],
                                ..
                            } => {
                                // In authentic CorelDRAW files, group transforms are already
                                // baked into every descendant leaf object's `trfd` matrix.
                            }
                            _ => {}
                        }
                    }
                }
                b"trfl" => {
                    is_real_cdr_group = true;
                }
                b"obj " => {
                    if let Some(oid) = parse_object_list(sub_ch, depth + 1, doc, ctx)? {
                        child_ids.push(oid);
                    }
                }
                b"grp " => {
                    if let Some(gid) = parse_group_list(sub_ch, depth + 1, doc, ctx)? {
                        child_ids.push(gid);
                    }
                }
                _ => {
                    let raw = serialize_list_body(list_type, sub_ch, ctx.endian)?;
                    unknown_chunks.push(UnknownCdrChunk {
                        fourcc: *b"LIST",
                        version_hint: u32::from(ctx.version),
                        raw_bytes: raw,
                    });
                }
            },
            RiffNode::Chunk { fourcc, data, .. } => match fourcc {
                b"trfd" => {
                    if is_real_corel_trfd(data) {
                        is_real_cdr_group = true;
                    } else if let Ok(xf) = parse_synthetic_trfd_chunk(data) {
                        transform = xf;
                    }
                }
                b"lens" => {
                    if let Ok(l) = parse_lens_chunk(data) {
                        lens = Some(l);
                    }
                }
                b"loda" => {
                    if is_real_corel_loda(data) {
                        is_real_cdr_group = true;
                        if let Some(n) = extract_real_loda_name(data) {
                            name = n;
                        }
                    } else if let Ok(parsed) = parse_loda_chunk(data) {
                        if let Some(p) = parsed.props {
                            name = p.name;
                            visible = p.visible;
                            locked = p.locked;
                            printable = p.printable;
                            opacity = p.opacity;
                            blend_mode = p.blend_mode;
                        }
                    }
                }
                b"flgs" | b"spid" | b"bbox" | b"obbx" | b"ftil" | b"usdn" => {}
                _ => {
                    unknown_chunks.push(UnknownCdrChunk {
                        fourcc: *fourcc,
                        version_hint: u32::from(ctx.version),
                        raw_bytes: data.clone(),
                    });
                }
            },
        }
    }

    if is_real_cdr_group {
        transform = Affine2D::IDENTITY;
    }

    let gid = doc.alloc_object_id();
    let mut grp = SceneObject::new(gid, name, NodePayload::Group(child_ids));
    grp.transform = transform;
    grp.fill = Fill::None;
    grp.outline = OutlineStyle::none();
    grp.visible = visible;
    grp.locked = locked;
    grp.printable = printable;
    grp.opacity = opacity;
    grp.blend_mode = blend_mode;
    grp.lens = lens;
    grp.unknown_chunks = unknown_chunks;
    doc.objects.insert(gid, grp);
    Ok(Some(gid))
}

fn parse_object_list(
    children: &[RiffNode],
    depth: usize,
    doc: &mut Document,
    ctx: &mut ParseContext<'_>,
) -> CdrResult<Option<ObjectId>> {
    ctx.limits.check_depth(depth)?;

    let mut loda_data: Option<ParsedLoda> = None;
    let mut synthetic_transform = Affine2D::IDENTITY;
    let mut real_trfd: Option<RealCorelMatrix> = None;
    let mut inline_fill: Option<Fill> = None;
    let mut inline_fill_normalized = false;
    let mut inline_outline: Option<OutlineStyle> = None;
    let mut inline_outline_explicit_none = false;
    let mut inline_style: Option<ParsedStyleEntry> = None;
    let mut inline_bmp_id: Option<ImageAssetId> = None;
    let mut txsm_payload: Option<ParsedTxsm> = None;
    let mut lens: Option<LensConfig> = None;
    let mut powerclip_children: Vec<ObjectId> = Vec::new();
    let mut unknown_chunks: Vec<UnknownCdrChunk> = Vec::new();

    // Helper closure-free inline scanner for both direct children and `LIST lgob` children
    let mut flat_chunks: Vec<(&[u8; 4], &[u8])> = Vec::new();
    for node in children {
        match node {
            RiffNode::Chunk { fourcc, data, .. } => flat_chunks.push((fourcc, data.as_slice())),
            RiffNode::List {
                list_type,
                children: sub_ch,
                ..
            } => match list_type {
                b"lgob" => {
                    for gc in sub_ch {
                        match gc {
                            RiffNode::Chunk { fourcc, data, .. } => {
                                flat_chunks.push((fourcc, data.as_slice()));
                            }
                            RiffNode::List {
                                list_type: [b't', b'r', b'f', b'l'],
                                children: gch,
                                ..
                            } => {
                                for tc in gch {
                                    if let RiffNode::Chunk { fourcc, data, .. } = tc {
                                        flat_chunks.push((fourcc, data.as_slice()));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                b"trfl" => {
                    for tc in sub_ch {
                        if let RiffNode::Chunk { fourcc, data, .. } = tc {
                            flat_chunks.push((fourcc, data.as_slice()));
                        }
                    }
                }
                b"pwrp" => {
                    for pnode in sub_ch {
                        if let RiffNode::List {
                            list_type: plt,
                            children: pch,
                            ..
                        } = pnode
                        {
                            match plt {
                                b"obj " => {
                                    if let Some(cid) = parse_object_list(pch, depth + 1, doc, ctx)?
                                    {
                                        powerclip_children.push(cid);
                                    }
                                }
                                b"grp " => {
                                    if let Some(cid) = parse_group_list(pch, depth + 1, doc, ctx)? {
                                        powerclip_children.push(cid);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {
                    let raw = serialize_list_body(list_type, sub_ch, ctx.endian)?;
                    unknown_chunks.push(UnknownCdrChunk {
                        fourcc: *b"LIST",
                        version_hint: u32::from(ctx.version),
                        raw_bytes: raw,
                    });
                }
            },
        }
    }

    for (fourcc, data) in flat_chunks {
        match fourcc {
            b"loda" => match parse_loda_chunk(data) {
                Ok(parsed) => loda_data = Some(parsed),
                Err(e) => ctx.warnings.push(format!("malformed loda chunk: {e}")),
            },
            b"trfd" => {
                if is_real_corel_trfd(data) {
                    match parse_real_corel_trfd(data) {
                        Ok(m) => real_trfd = Some(m),
                        Err(e) => ctx.warnings.push(format!("malformed real trfd chunk: {e}")),
                    }
                } else {
                    match parse_synthetic_trfd_chunk(data) {
                        Ok(xf) => synthetic_transform = xf,
                        Err(e) => ctx.warnings.push(format!("malformed trfd chunk: {e}")),
                    }
                }
            }
            b"fild" => match parse_fild_chunk(data, ctx.is_zcf) {
                Ok((id, fill, is_norm)) => {
                    if id != 0 {
                        if is_norm {
                            ctx.normalized_fills.insert(id);
                        }
                        ctx.fills.insert(id, fill.clone());
                    }
                    inline_fill_normalized = is_norm;
                    inline_fill = Some(fill);
                }
                Err(e) => ctx.warnings.push(format!("malformed fild chunk: {e}")),
            },
            b"outl" => match parse_outl_chunk(data, ctx.is_zcf) {
                Ok((id, outl, is_none)) => {
                    if id != 0 {
                        if is_none {
                            ctx.outlines_explicit_none.insert(id);
                        }
                        ctx.outlines.insert(id, outl.clone());
                    }
                    inline_outline_explicit_none = is_none;
                    inline_outline = Some(outl);
                }
                Err(e) => ctx.warnings.push(format!("malformed outl chunk: {e}")),
            },
            b"stlt" => match parse_stlt_chunk(data) {
                Ok((id, entry)) => {
                    if id != 0 {
                        ctx.styles.insert(id, entry.clone());
                    }
                    inline_style = Some(entry);
                }
                Err(e) => ctx.warnings.push(format!("malformed stlt chunk: {e}")),
            },
            b"txsm" | b"txst" => {
                txsm_payload = Some(ParsedTxsm::Raw(data.to_vec()));
            }
            b"bmp " => {
                let asset_id = register_bmp_chunk_payload(data, None, doc, ctx)?;
                inline_bmp_id = Some(asset_id);
            }
            b"lens" => match parse_lens_chunk(data) {
                Ok(l) => lens = Some(l),
                Err(e) => ctx.warnings.push(format!("malformed lens chunk: {e}")),
            },
            b"flgs" | b"spid" | b"bbox" | b"obbx" | b"ftil" | b"usdn" => {}
            _ => {
                unknown_chunks.push(UnknownCdrChunk {
                    fourcc: *fourcc,
                    version_hint: u32::from(ctx.version),
                    raw_bytes: data.to_vec(),
                });
            }
        }
    }

    let loda = loda_data.unwrap_or_default();

    // Resolve fill: inline `fild`, then `fill_id` lookup, then `0x00C9` JSON explicit fill type
    let mut fill_is_normalized = if inline_fill.is_some() {
        inline_fill_normalized
    } else {
        loda.fill_id
            .is_some_and(|id| ctx.normalized_fills.contains(&id))
    };
    let mut fill = inline_fill
        .or_else(|| loda.fill_id.and_then(|id| ctx.fills.get(&id).cloned()))
        .unwrap_or(Fill::None);
    if let Some(ref c9) = loda.style_json {
        if let Some(explicit_fill) = c9.explicit_fill() {
            fill = explicit_fill;
            fill_is_normalized = true;
        }
    } else if matches!(fill, Fill::None) {
        if let Some(sid) = loda.style_id {
            if let Some(resolved_style) = ctx.zcf.stylesheet.resolve(&sid.to_string()) {
                if let Some(st_fill) = resolved_style.explicit_fill() {
                    fill = st_fill;
                    fill_is_normalized = true;
                }
            }
        }
    }

    // Resolve outline: inline `outl`, then `outline_id` lookup, then merge `0x00C9` overrides
    let outline_explicit_none = inline_outline_explicit_none
        || loda
            .outline_id
            .is_some_and(|id| ctx.outlines_explicit_none.contains(&id));
    let mut outline = inline_outline
        .or_else(|| {
            loda.outline_id
                .and_then(|id| ctx.outlines.get(&id).cloned())
        })
        .unwrap_or_else(OutlineStyle::none);
    if let Some(ref c9) = loda.style_json {
        c9.apply_outline_overrides(&mut outline, outline_explicit_none);
    }

    // Resolve document-level `txsm` reference for real CorelDRAW text objects
    if txsm_payload.is_none() {
        if let Some(tid) = loda.text_stream_id {
            if let Some(raw) = ctx.text_streams.get(&tid) {
                txsm_payload = Some(ParsedTxsm::Raw(raw.clone()));
            }
        }
    }

    // Convert authentic CorelDRAW geometry + `trfd` (centered Y-up 0.1 µm) into `coree`
    // top-left Y-down millimeter coordinates on the active page.
    let mut final_transform = synthetic_transform;
    let mut resolved_curve = loda.curve;
    let mut resolved_shape = loda.shape;

    if loda.is_real_cdr {
        let m = real_trfd.unwrap_or(RealCorelMatrix::IDENTITY);
        let page_xf = m.to_page_mm_affine(ctx.current_page_w_mm, ctx.current_page_h_mm);

        if let Some(rect) = loda.real_rect {
            let (shape, rect_xf) =
                convert_real_corel_rect(rect, m, ctx.current_page_w_mm, ctx.current_page_h_mm);
            resolved_shape = Some(shape);
            final_transform = rect_xf;
        } else if let Some(ell) = loda.real_ellipse {
            let (shape, ell_xf) =
                convert_real_corel_ellipse(ell, m, ctx.current_page_w_mm, ctx.current_page_h_mm);
            resolved_shape = Some(shape);
            final_transform = ell_xf;
        } else if let Some(curve) = resolved_curve.take() {
            // Bake world page-mm transform directly into BezierPath nodes & handles so
            // outlines and editing never suffer from negative-Y reflection or non-uniform scale.
            resolved_curve = Some(curve.transformed(&page_xf));
            final_transform = Affine2D::IDENTITY;
        } else {
            final_transform = page_xf;
        }
    }

    // Resolve payload
    let payload: Option<NodePayload> = if let Some(ParsedTxsm::Raw(raw_txsm)) = txsm_payload {
        let cdss_default = if loda.text_meta.is_some_and(|m| m.is_paragraph) {
            ctx.zcf.stylesheet.default_paragraph_text()
        } else {
            ctx.zcf.stylesheet.default_artistic_text()
        };
        match parse_txsm_chunk(
            &raw_txsm,
            loda.style_id,
            inline_style.as_ref(),
            loda.text_meta,
            &ctx.styles,
            cdss_default,
        ) {
            Ok(p) => Some(p),
            Err(e) => {
                ctx.warnings.push(format!("malformed txsm chunk: {e}"));
                None
            }
        }
    } else if let Some(bmp_ref) = loda.bitmap_ref {
        let asset_id = inline_bmp_id
            .or_else(|| ctx.bitmaps.get(&bmp_ref.bmp_id).copied())
            .or_else(|| doc.assets.keys().next().copied());
        asset_id.map(|aid| {
            NodePayload::Raster(RasterLayerData {
                asset_id: aid,
                width_mm: bmp_ref.width_mm,
                height_mm: bmp_ref.height_mm,
                filters: Vec::new(),
                crop_rect: bmp_ref.crop_rect,
            })
        })
    } else if let Some(aid) = inline_bmp_id {
        let (w_mm, h_mm) = doc.assets.get(&aid).map_or((50.0, 50.0), |a| {
            let dpi = f64::from(a.source_dpi.max(1.0));
            (
                f64::from(a.width) * 25.4 / dpi,
                f64::from(a.height) * 25.4 / dpi,
            )
        });
        Some(NodePayload::Raster(RasterLayerData {
            asset_id: aid,
            width_mm: w_mm,
            height_mm: h_mm,
            filters: Vec::new(),
            crop_rect: None,
        }))
    } else if !powerclip_children.is_empty() {
        let container = resolved_curve
            .or_else(|| resolved_shape.as_ref().map(ParametricShape::to_bezier_path))
            .unwrap_or_else(|| BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 20.0, 20.0)));
        Some(NodePayload::PowerClip {
            container: Box::new(container),
            contents: powerclip_children,
        })
    } else if let Some(shape) = resolved_shape {
        Some(NodePayload::Shape(shape))
    } else {
        resolved_curve.map(NodePayload::Path)
    };

    let Some(payload) = payload else {
        // If an object only carries unknown chunks, preserve them on the document so nothing is lost
        doc.unknown_doc_chunks.extend(unknown_chunks);
        return Ok(None);
    };

    if fill_is_normalized {
        let local_bb = match &payload {
            NodePayload::Path(p) => p.bounding_box(),
            NodePayload::Shape(s) => s.to_bezier_path().bounding_box(),
            NodePayload::PowerClip { container, .. } => container.bounding_box(),
            _ => None,
        };
        if let Some(bb) = local_bb {
            rescale_normalized_fill_to_bounds(&mut fill, bb);
        }
    }

    let oid = doc.alloc_object_id();
    let props = loda.props.unwrap_or_else(|| {
        let default_name = match &payload {
            NodePayload::Shape(ParametricShape::Rectangle { .. }) => "Rectangle",
            NodePayload::Shape(ParametricShape::Ellipse { .. }) => "Ellipse",
            NodePayload::Shape(ParametricShape::Polygon { .. }) => "Polygon",
            NodePayload::Path(_) => "Curve",
            NodePayload::ArtisticText(_) => "Artistic Text",
            NodePayload::ParagraphText(_) => "Paragraph Text",
            NodePayload::Raster(_) => "Bitmap",
            _ => "Object",
        };
        ParsedObjProps {
            name: loda.real_name.unwrap_or_else(|| default_name.to_owned()),
            ..ParsedObjProps::default()
        }
    });
    let mut obj = SceneObject::new(oid, props.name, payload);
    obj.transform = final_transform;
    obj.fill = fill;
    obj.outline = outline;
    obj.lens = lens;
    obj.blend_mode = props.blend_mode;
    obj.opacity = props.opacity;
    obj.visible = props.visible;
    obj.locked = props.locked;
    obj.printable = props.printable;
    obj.unknown_chunks = unknown_chunks;

    doc.objects.insert(oid, obj);
    Ok(Some(oid))
}

#[derive(Debug, Clone)]
struct ParsedObjProps {
    name: String,
    visible: bool,
    locked: bool,
    printable: bool,
    blend_mode: BlendMode,
    opacity: f32,
}

impl Default for ParsedObjProps {
    fn default() -> Self {
        Self {
            name: "Object".to_owned(),
            visible: true,
            locked: false,
            printable: true,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ParsedBitmapRef {
    bmp_id: u32,
    width_mm: f64,
    height_mm: f64,
    crop_rect: Option<Rect2D>,
}

#[derive(Debug, Clone, Copy)]
struct ParsedTextMeta {
    is_paragraph: bool,
    frame_w_mm: f64,
    frame_h_mm: f64,
    columns: u32,
    gutter_mm: f64,
}

#[derive(Debug, Clone, Copy)]
struct RealCorelRect {
    local_w_mm: f64,
    local_h_mm: f64,
    /// Corner radii either as normalized fractions of `min(world_w, world_h) / 2` (when `relative == true`)
    /// or absolute millimeters (when `relative == false`), ordered `[TL, TR, BR, BL]`.
    corners: [f64; 4],
    relative_corners: bool,
    corner_style: CornerStyle,
}

#[derive(Debug, Clone, Copy)]
struct RealCorelEllipse {
    local_w_mm: f64,
    local_h_mm: f64,
    mode: EllipseMode,
    start_deg: f64,
    end_deg: f64,
}

/// Authentic CorelDRAW 2x3 row-major affine matrix in Corel `0.1 µm` centered Y-up space:
/// `x' = v0 * x + v1 * y + x0_mm`, `y' = v3 * x + v4 * y + y0_mm`.
#[derive(Debug, Clone, Copy)]
struct RealCorelMatrix {
    v0: f64,
    v1: f64,
    x0_mm: f64,
    v3: f64,
    v4: f64,
    y0_mm: f64,
}

impl RealCorelMatrix {
    const IDENTITY: Self = Self {
        v0: 1.0,
        v1: 0.0,
        x0_mm: 0.0,
        v3: 0.0,
        v4: 1.0,
        y0_mm: 0.0,
    };

    fn is_finite(self) -> bool {
        self.v0.is_finite()
            && self.v1.is_finite()
            && self.x0_mm.is_finite()
            && self.v3.is_finite()
            && self.v4.is_finite()
            && self.y0_mm.is_finite()
            && self.v0.hypot(self.v3).is_finite()
            && self.v1.hypot(self.v4).is_finite()
    }

    fn sanitized(self) -> Self {
        if self.is_finite() {
            self
        } else {
            Self::IDENTITY
        }
    }

    fn compose(self, other: Self) -> Self {
        let composed = Self {
            v0: self.v0 * other.v0 + self.v1 * other.v3,
            v1: self.v0 * other.v1 + self.v1 * other.v4,
            x0_mm: self.v0 * other.x0_mm + self.v1 * other.y0_mm + self.x0_mm,
            v3: self.v3 * other.v0 + self.v4 * other.v3,
            v4: self.v3 * other.v1 + self.v4 * other.v4,
            y0_mm: self.v3 * other.x0_mm + self.v4 * other.y0_mm + self.y0_mm,
        };
        composed.sanitized()
    }

    fn transform_point_cdr_mm(self, x_mm: f64, y_mm: f64) -> (f64, f64) {
        let tx = self.v0 * x_mm + self.v1 * y_mm + self.x0_mm;
        let ty = self.v3 * x_mm + self.v4 * y_mm + self.y0_mm;
        (sanitize_f64(tx, 0.0), sanitize_f64(ty, 0.0))
    }

    /// Maps local Corel Y-up mm coordinates directly into `coree` top-left Y-down page mm.
    fn to_page_mm_affine(self, page_w_mm: f64, page_h_mm: f64) -> Affine2D {
        let m = self.sanitized();
        let xf = Affine2D::new(
            m.v0,
            -m.v3,
            m.v1,
            -m.v4,
            m.x0_mm + page_w_mm * 0.5,
            page_h_mm * 0.5 - m.y0_mm,
        );
        if xf.is_finite() {
            xf
        } else {
            Affine2D::IDENTITY
        }
    }
}

fn convert_real_corel_rect(
    rect: RealCorelRect,
    m: RealCorelMatrix,
    page_w_mm: f64,
    page_h_mm: f64,
) -> (ParametricShape, Affine2D) {
    let m = m.sanitized();
    let sx = {
        let raw = m.v0.hypot(m.v3);
        if raw.is_finite() && raw > 1e-9 {
            raw
        } else {
            1.0
        }
    };
    let sy = {
        let raw = m.v1.hypot(m.v4);
        if raw.is_finite() && raw > 1e-9 {
            raw
        } else {
            1.0
        }
    };
    let world_w = sanitize_f64((rect.local_w_mm * sx).abs(), 0.0).max(0.0);
    let world_h = sanitize_f64((rect.local_h_mm * sy).abs(), 0.0).max(0.0);
    let max_r = sanitize_f64(0.5 * world_w.min(world_h), 0.0).max(0.0);

    let mut radii = rect.corners.map(|c| {
        let r = if rect.relative_corners {
            c * max_r
        } else {
            c * sx.min(sy)
        };
        sanitize_f64(r, 0.0).clamp(0.0, max_r)
    });

    // Axis-aligned (including horizontal/vertical mirror flips): absorb signs into corner order
    // and emit a pure translation so strokes & 45° chamfers remain undistorted.
    if m.v1.abs() < 1e-6 && m.v3.abs() < 1e-6 {
        if m.v0 < 0.0 {
            radii.swap(0, 1); // TL <-> TR
            radii.swap(2, 3); // BR <-> BL
        }
        if m.v4 < 0.0 {
            radii.swap(0, 3); // TL <-> BL
            radii.swap(1, 2); // TR <-> BR
        }
        let page_xf = m.to_page_mm_affine(page_w_mm, page_h_mm);
        let p0 = page_xf.transform_point(Point2D::new(0.0, 0.0));
        let p1 = page_xf.transform_point(Point2D::new(rect.local_w_mm, rect.local_h_mm));
        let min_x = sanitize_f64(p0.x.min(p1.x), 0.0);
        let min_y = sanitize_f64(p0.y.min(p1.y), 0.0);
        let lock_corners = radii.iter().all(|&r| (r - radii[0]).abs() < 1e-3);
        let shape = ParametricShape::Rectangle {
            width: world_w,
            height: world_h,
            corner_radii: radii,
            corner_style: rect.corner_style,
            lock_corners,
        };
        return (shape, Affine2D::translate(min_x, min_y));
    }

    // Rotated / sheared rectangle: factor scale `(sx, sy)` into `(world_w, world_h)` and map
    // `coree` Y-down local `(0..world_w, 0..world_h)` -> Corel Y-up local `(0..local_w, 0..local_h)`.
    let page_xf = m.to_page_mm_affine(page_w_mm, page_h_mm);
    let to_corel_local = Affine2D::new(1.0 / sx, 0.0, 0.0, -1.0 / sy, 0.0, rect.local_h_mm);
    let rect_xf = page_xf.compose(&to_corel_local);
    let rect_xf = if rect_xf.is_finite() {
        rect_xf
    } else {
        Affine2D::IDENTITY
    };
    let lock_corners = radii.iter().all(|&r| (r - radii[0]).abs() < 1e-3);
    let shape = ParametricShape::Rectangle {
        width: world_w,
        height: world_h,
        corner_radii: radii,
        corner_style: rect.corner_style,
        lock_corners,
    };
    (shape, rect_xf)
}

fn convert_real_corel_ellipse(
    ell: RealCorelEllipse,
    m: RealCorelMatrix,
    page_w_mm: f64,
    page_h_mm: f64,
) -> (ParametricShape, Affine2D) {
    let m = m.sanitized();
    let sx = {
        let raw = m.v0.hypot(m.v3);
        if raw.is_finite() && raw > 1e-9 {
            raw
        } else {
            1.0
        }
    };
    let sy = {
        let raw = m.v1.hypot(m.v4);
        if raw.is_finite() && raw > 1e-9 {
            raw
        } else {
            1.0
        }
    };
    let rx = sanitize_f64((ell.local_w_mm * 0.5 * sx).abs(), 0.0).max(0.0);
    let ry = sanitize_f64((ell.local_h_mm * 0.5 * sy).abs(), 0.0).max(0.0);
    let (cx_cdr, cy_cdr) = m.transform_point_cdr_mm(ell.local_w_mm * 0.5, ell.local_h_mm * 0.5);
    let cx = sanitize_f64(cx_cdr + page_w_mm * 0.5, 0.0);
    let cy = sanitize_f64(page_h_mm * 0.5 - cy_cdr, 0.0);

    let shape = ParametricShape::Ellipse {
        rx,
        ry,
        mode: ell.mode,
        start_deg: sanitize_f64(ell.start_deg, 0.0),
        end_deg: sanitize_f64(ell.end_deg, 360.0),
    };
    if m.v1.abs() < 1e-6 && m.v3.abs() < 1e-6 {
        return (shape, Affine2D::translate(cx, cy));
    }
    let rot_a = sanitize_f64(m.v0 / sx, 1.0);
    let rot_b = sanitize_f64(-m.v3 / sx, 0.0);
    let rot_c = sanitize_f64(m.v1 / sy, 0.0);
    let rot_d = sanitize_f64(-m.v4 / sy, 1.0);
    let xf = Affine2D::new(rot_a, rot_b, rot_c, rot_d, cx, cy);
    (
        shape,
        if xf.is_finite() {
            xf
        } else {
            Affine2D::translate(cx, cy)
        },
    )
}

#[derive(Debug, Default)]
struct ParsedLoda {
    is_real_cdr: bool,
    curve: Option<BezierPath>,
    shape: Option<ParametricShape>,
    real_rect: Option<RealCorelRect>,
    real_ellipse: Option<RealCorelEllipse>,
    bitmap_ref: Option<ParsedBitmapRef>,
    text_meta: Option<ParsedTextMeta>,
    text_stream_id: Option<u32>,
    fill_id: Option<u32>,
    outline_id: Option<u32>,
    style_id: Option<u32>,
    style_json: Option<CdssStyle>,
    real_name: Option<String>,
    props: Option<ParsedObjProps>,
}

enum ParsedTxsm {
    Raw(Vec<u8>),
}

/// Returns `true` if `data` matches the authentic CorelDRAW 20-byte `loda` header layout:
/// `[0..4] chunk_len == data.len()`, `[4..8] num_args`, `[8..12] args_offset >= 20`,
/// `[12..16] types_offset`, `[16..20] chunk_type`.
fn is_real_corel_loda(data: &[u8]) -> bool {
    if data.len() < 20 {
        return false;
    }
    let chunk_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let num_args = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let args_off = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    let types_off = u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
    chunk_len == data.len()
        && num_args <= 1024
        && args_off >= 20
        && types_off >= args_off
        && types_off
            .checked_add(num_args.saturating_mul(4))
            .is_some_and(|end| end <= data.len())
}

type RealLodaArgs<'a> = (u32, Vec<(u32, &'a [u8])>);

fn parse_real_loda_args(data: &[u8]) -> Option<RealLodaArgs<'_>> {
    if !is_real_corel_loda(data) {
        return None;
    }
    let num_args = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let args_off = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    let types_off = u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
    let chunk_type = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);

    let table_bytes = num_args.checked_mul(4)?;
    let offsets_slice = data.get(args_off..args_off.checked_add(table_bytes)?)?;
    let types_slice = data.get(types_off..types_off.checked_add(table_bytes)?)?;

    // Optional trailing end-offset entry at `args_off + num_args * 4` (present in v16–26)
    let trailing_end = if types_off >= args_off.saturating_add(table_bytes).saturating_add(4) {
        let p = args_off + table_bytes;
        Some(u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize)
    } else {
        None
    };

    let mut offsets = Vec::with_capacity(num_args);
    let mut types = Vec::with_capacity(num_args);
    for i in 0..num_args {
        let o = i * 4;
        offsets.push(u32::from_le_bytes([
            offsets_slice[o],
            offsets_slice[o + 1],
            offsets_slice[o + 2],
            offsets_slice[o + 3],
        ]) as usize);
        types.push(u32::from_le_bytes([
            types_slice[o],
            types_slice[o + 1],
            types_slice[o + 2],
            types_slice[o + 3],
        ]));
    }

    // Authentic CorelDRAW `loda` stores `types` in REVERSE order relative to `offsets`!
    // Enforce strictly non-overlapping monotonic arg offsets (`start >= prev_end`, `end <= data.len()`)
    // so adversarial alternating/overlapping offset tables cannot trigger 512x CPU/memory amplification.
    let mut out = Vec::with_capacity(num_args);
    let mut prev_end = 0usize;
    let mut total_bytes = 0usize;
    for i in 0..num_args {
        let start = offsets[i];
        let end = offsets
            .get(i + 1)
            .copied()
            .or(trailing_end)
            .unwrap_or(data.len());
        if start < prev_end || start > end || end > data.len() {
            return None;
        }
        let seg_len = end - start;
        total_bytes = total_bytes.checked_add(seg_len)?;
        if total_bytes > data.len() {
            return None;
        }
        prev_end = end;
        let arg_type = types[num_args - 1 - i];
        out.push((arg_type, &data[start..end]));
    }
    Some((chunk_type, out))
}

fn parse_real_layer_loda(data: &[u8]) -> Option<(u32, Option<String>)> {
    let (ctype, args) = parse_real_loda_args(data)?;
    let mut name = None;
    for (typ, seg) in args {
        if typ == LODA_ARG_NAME && !seg.is_empty() {
            let decoded = decode_cdr_text(seg, None, None);
            let trimmed = decoded.trim_matches('\0').trim();
            if !trimmed.is_empty() {
                name = Some(trimmed.to_owned());
            }
        }
    }
    Some((ctype, name))
}

fn extract_real_loda_name(data: &[u8]) -> Option<String> {
    parse_real_layer_loda(data).and_then(|(_, name)| name)
}

fn parse_real_corel_loda(data: &[u8]) -> CdrResult<ParsedLoda> {
    let Some((chunk_type, args)) = parse_real_loda_args(data) else {
        return Err(CdrError::InvalidChunk {
            fourcc: "loda".to_owned(),
            reason: "invalid authentic CorelDRAW loda header".to_owned(),
        });
    };

    let mut out = ParsedLoda {
        is_real_cdr: true,
        ..ParsedLoda::default()
    };

    for (typ, seg) in args {
        match typ {
            LODA_ARG_OUTLINE_ID if seg.len() >= 4 => {
                out.outline_id = Some(u32::from_le_bytes([seg[0], seg[1], seg[2], seg[3]]));
            }
            LODA_ARG_FILL_ID if seg.len() >= 4 => {
                out.fill_id = Some(u32::from_le_bytes([seg[0], seg[1], seg[2], seg[3]]));
            }
            LODA_ARG_STYLE_ID if seg.len() >= 4 => {
                out.style_id = Some(u32::from_le_bytes([seg[0], seg[1], seg[2], seg[3]]));
            }
            LODA_ARG_STYLE_JSON => {
                out.style_json = parse_c9_style_override(seg);
            }
            LODA_ARG_NAME if !seg.is_empty() => {
                let decoded = decode_cdr_text(seg, None, None);
                let trimmed = decoded.trim_matches('\0').trim();
                if !trimmed.is_empty() {
                    out.real_name = Some(trimmed.to_owned());
                }
            }
            LODA_ARG_CURVE => match chunk_type {
                CDR_CTYPE_RECTANGLE => {
                    if let Some(rect) = parse_real_rect_arg(seg) {
                        out.real_rect = Some(rect);
                    }
                }
                CDR_CTYPE_ELLIPSE => {
                    if let Some(ell) = parse_real_ellipse_arg(seg) {
                        out.real_ellipse = Some(ell);
                    }
                }
                CDR_CTYPE_CURVE | CDR_CTYPE_POLYGON => {
                    let mut ar = ByteReader::new(seg);
                    if let Ok(path) = parse_loda_curve(&mut ar) {
                        if !path.subpaths.is_empty() {
                            out.curve = Some(path);
                        }
                    }
                }
                CDR_CTYPE_BITMAP => {
                    if seg.len() >= 36 {
                        let w =
                            cdr32_to_mm(i32::from_le_bytes([seg[0], seg[1], seg[2], seg[3]])).abs();
                        let h =
                            cdr32_to_mm(i32::from_le_bytes([seg[4], seg[5], seg[6], seg[7]])).abs();
                        let bmp_id = u32::from_le_bytes([seg[32], seg[33], seg[34], seg[35]]);
                        out.bitmap_ref = Some(ParsedBitmapRef {
                            bmp_id,
                            width_mm: w.max(1.0),
                            height_mm: h.max(1.0),
                            crop_rect: None,
                        });
                    }
                }
                CDR_CTYPE_ARTISTIC_TEXT | CDR_CTYPE_PARAGRAPH_TEXT => {
                    let is_para = chunk_type == CDR_CTYPE_PARAGRAPH_TEXT;
                    if seg.len() >= 4 {
                        out.text_stream_id =
                            Some(u32::from_le_bytes([seg[0], seg[1], seg[2], seg[3]]));
                    }
                    let (fw, fh) = if is_para && seg.len() >= 12 {
                        (
                            cdr32_to_mm(i32::from_le_bytes([seg[4], seg[5], seg[6], seg[7]])).abs(),
                            cdr32_to_mm(i32::from_le_bytes([seg[8], seg[9], seg[10], seg[11]]))
                                .abs(),
                        )
                    } else {
                        (0.0, 0.0)
                    };
                    out.text_meta = Some(ParsedTextMeta {
                        is_paragraph: is_para,
                        frame_w_mm: fw,
                        frame_h_mm: fh,
                        columns: 1,
                        gutter_mm: 5.0,
                    });
                }
                CDR_CTYPE_GROUP_OR_LAYER => {}
                _ => {
                    // Fallback: if arg 0x1e has an exact curve point+flag array, parse it as a curve
                    let mut ar = ByteReader::new(seg);
                    if let Ok(path) = parse_loda_curve(&mut ar) {
                        if !path.subpaths.is_empty() {
                            out.curve = Some(path);
                        }
                    }
                }
            },
            _ => {}
        }
    }

    Ok(out)
}

fn parse_real_rect_arg(seg: &[u8]) -> Option<RealCorelRect> {
    // Authentic CorelDRAW X5–2025 (v15–26): 128-byte (or >=32-byte) f64 rectangle geometry
    if seg.len() >= 32 {
        let mut r = ByteReader::new(seg);
        let w_raw = sanitize_f64(r.read_f64_le("rect f64 w").ok()?, 0.0);
        let h_raw = sanitize_f64(r.read_f64_le("rect f64 h").ok()?, 0.0);
        if w_raw.abs() > 10.0 && h_raw.abs() > 10.0 && w_raw.abs() < 1e11 && h_raw.abs() < 1e11 {
            let local_w_mm = w_raw.abs() / coree_core::math::CDR_UNITS_PER_MM;
            let local_h_mm = h_raw.abs() / coree_core::math::CDR_UNITS_PER_MM;
            let mut corners = [0.0_f64; 4];
            let mut corner_style = CornerStyle::Rounded;
            let mut relative_corners = true;

            if seg.len() >= 128 {
                let mut any_non_zero_style = None;
                for (i, slot) in corners.iter_mut().enumerate() {
                    let base = 32 + i * 24;
                    let abs_r = i32::from_le_bytes([
                        seg[base],
                        seg[base + 1],
                        seg[base + 2],
                        seg[base + 3],
                    ]);
                    let mode_flag = u32::from_le_bytes([
                        seg[base + 4],
                        seg[base + 5],
                        seg[base + 6],
                        seg[base + 7],
                    ]);
                    let frac = sanitize_f64(
                        f64::from_le_bytes([
                            seg[base + 8],
                            seg[base + 9],
                            seg[base + 10],
                            seg[base + 11],
                            seg[base + 12],
                            seg[base + 13],
                            seg[base + 14],
                            seg[base + 15],
                        ]),
                        0.0,
                    );
                    let style_raw = u32::from_le_bytes([
                        seg[base + 16],
                        seg[base + 17],
                        seg[base + 18],
                        seg[base + 19],
                    ]);
                    if mode_flag == 0x7FFF || frac > 1e-9 {
                        *slot = frac.clamp(0.0, 1.0);
                    } else if abs_r > 0 {
                        relative_corners = false;
                        *slot = cdr32_to_mm(abs_r);
                    }
                    if *slot > 1e-9 || any_non_zero_style.is_none() {
                        let st = match style_raw {
                            1 => CornerStyle::Scalloped,
                            2 => CornerStyle::Chamfered,
                            _ => CornerStyle::Rounded,
                        };
                        if *slot > 1e-9 {
                            any_non_zero_style = Some(st);
                        } else {
                            corner_style = st;
                        }
                    }
                }
                if let Some(st) = any_non_zero_style {
                    corner_style = st;
                }
            }
            return Some(RealCorelRect {
                local_w_mm,
                local_h_mm,
                corners,
                relative_corners,
                corner_style,
            });
        }
    }

    // Legacy CorelDRAW v7–v14: i32 width, height, and 1 or 4 corner radii
    if seg.len() >= 8 {
        let local_w_mm = cdr32_to_mm(i32::from_le_bytes([seg[0], seg[1], seg[2], seg[3]])).abs();
        let local_h_mm = cdr32_to_mm(i32::from_le_bytes([seg[4], seg[5], seg[6], seg[7]])).abs();
        let mut corners = [0.0_f64; 4];
        if seg.len() >= 24 {
            for (i, slot) in corners.iter_mut().enumerate() {
                let b = 8 + i * 4;
                *slot = cdr32_to_mm(i32::from_le_bytes([
                    seg[b],
                    seg[b + 1],
                    seg[b + 2],
                    seg[b + 3],
                ]))
                .max(0.0);
            }
        } else if seg.len() >= 12 {
            let r = cdr32_to_mm(i32::from_le_bytes([seg[8], seg[9], seg[10], seg[11]])).max(0.0);
            corners = [r; 4];
        }
        return Some(RealCorelRect {
            local_w_mm,
            local_h_mm,
            corners,
            relative_corners: false,
            corner_style: CornerStyle::Rounded,
        });
    }
    None
}

fn parse_real_ellipse_arg(seg: &[u8]) -> Option<RealCorelEllipse> {
    if seg.len() < 8 {
        return None;
    }
    let local_w_mm = cdr32_to_mm(i32::from_le_bytes([seg[0], seg[1], seg[2], seg[3]])).abs();
    let local_h_mm = cdr32_to_mm(i32::from_le_bytes([seg[4], seg[5], seg[6], seg[7]])).abs();
    let start_deg = if seg.len() >= 12 {
        f64::from(i32::from_le_bytes([seg[8], seg[9], seg[10], seg[11]])) / 1_000_000.0
    } else {
        0.0
    };
    let end_deg = if seg.len() >= 16 {
        f64::from(i32::from_le_bytes([seg[12], seg[13], seg[14], seg[15]])) / 1_000_000.0
    } else {
        0.0
    };
    let pie_flag = if seg.len() >= 20 {
        u32::from_le_bytes([seg[16], seg[17], seg[18], seg[19]])
    } else {
        0
    };
    let (mode, s_deg, e_deg) = if (start_deg - end_deg).abs() < 1e-4 {
        (EllipseMode::Full, 0.0, 360.0)
    } else if pie_flag != 0 {
        (EllipseMode::Pie, start_deg, end_deg)
    } else {
        (EllipseMode::Arc, start_deg, end_deg)
    };
    Some(RealCorelEllipse {
        local_w_mm,
        local_h_mm,
        mode,
        start_deg: s_deg,
        end_deg: e_deg,
    })
}

fn parse_loda_chunk(data: &[u8]) -> CdrResult<ParsedLoda> {
    if is_real_corel_loda(data) {
        return parse_real_corel_loda(data);
    }

    let mut r = ByteReader::new(data);
    let num_args = usize::try_from(r.read_u32_le("loda num_args")?)
        .map_err(|_| CdrError::ArithmeticOverflow("loda num_args"))?;
    let args_offset = usize::try_from(r.read_u32_le("loda args_offset")?)
        .map_err(|_| CdrError::ArithmeticOverflow("loda args_offset"))?;
    let types_offset = usize::try_from(r.read_u32_le("loda types_offset")?)
        .map_err(|_| CdrError::ArithmeticOverflow("loda types_offset"))?;

    if num_args > 1024 {
        return Err(CdrError::InvalidChunk {
            fourcc: "loda".to_owned(),
            reason: format!("excessive loda argument count {num_args}"),
        });
    }

    let table_bytes = num_args
        .checked_mul(4)
        .ok_or(CdrError::ArithmeticOverflow("loda table bytes"))?;
    let offsets_slice = data
        .get(
            args_offset
                ..args_offset
                    .checked_add(table_bytes)
                    .ok_or(CdrError::ArithmeticOverflow("loda args_offset end"))?,
        )
        .ok_or(CdrError::UnexpectedEof {
            context: "loda offsets table",
            needed: table_bytes,
            available: data.len().saturating_sub(args_offset),
        })?;
    let types_slice = data
        .get(
            types_offset
                ..types_offset
                    .checked_add(table_bytes)
                    .ok_or(CdrError::ArithmeticOverflow("loda types_offset end"))?,
        )
        .ok_or(CdrError::UnexpectedEof {
            context: "loda types table",
            needed: table_bytes,
            available: data.len().saturating_sub(types_offset),
        })?;

    let mut off_r = ByteReader::new(offsets_slice);
    let mut typ_r = ByteReader::new(types_slice);
    let mut args = Vec::with_capacity(num_args);
    for _ in 0..num_args {
        let off = usize::try_from(off_r.read_u32_le("loda arg offset")?)
            .map_err(|_| CdrError::ArithmeticOverflow("loda arg offset"))?;
        let typ = typ_r.read_u32_le("loda arg type")?;
        args.push((typ, off));
    }

    let payload_limit = args_offset.min(types_offset).min(data.len());
    let mut prev_end = 0usize;
    let mut total_bytes = 0usize;
    let mut out = ParsedLoda::default();
    for (i, &(typ, off)) in args.iter().enumerate() {
        let end = args
            .get(i + 1)
            .map(|&(_, next_off)| next_off)
            .unwrap_or(payload_limit);
        if off > data.len() {
            return Err(CdrError::UnexpectedEof {
                context: "loda arg payload",
                needed: off,
                available: data.len(),
            });
        }
        if off < prev_end || off > end || end > payload_limit {
            return Err(CdrError::InvalidChunk {
                fourcc: "loda".to_owned(),
                reason: format!(
                    "overlapping or non-monotonic loda arg range {off}..{end} (prev_end={prev_end}, limit={payload_limit})"
                ),
            });
        }
        total_bytes = total_bytes
            .checked_add(end - off)
            .ok_or(CdrError::ArithmeticOverflow("loda total arg bytes"))?;
        if total_bytes > data.len() {
            return Err(CdrError::InvalidChunk {
                fourcc: "loda".to_owned(),
                reason: "cumulative loda arg bytes exceed chunk length".to_owned(),
            });
        }
        prev_end = end;
        let arg_slice = &data[off..end];

        let mut ar = ByteReader::new(arg_slice);
        match typ {
            LODA_ARG_CURVE => {
                out.curve = Some(parse_loda_curve(&mut ar)?);
            }
            LODA_ARG_RECTANGLE => {
                let w = cdr32_to_mm(ar.read_i32_le("rect w")?).abs();
                let h = cdr32_to_mm(ar.read_i32_le("rect h")?).abs();
                let r0 = cdr32_to_mm(ar.read_i32_le("rect r0")?).max(0.0);
                let r1 = cdr32_to_mm(ar.read_i32_le("rect r1")?).max(0.0);
                let r2 = cdr32_to_mm(ar.read_i32_le("rect r2")?).max(0.0);
                let r3 = cdr32_to_mm(ar.read_i32_le("rect r3")?).max(0.0);
                let corner_style = match ar.read_u8("rect corner_style").unwrap_or(0) {
                    1 => CornerStyle::Scalloped,
                    2 => CornerStyle::Chamfered,
                    _ => CornerStyle::Rounded,
                };
                let lock_corners = ar.read_u8("rect lock_corners").unwrap_or(1) != 0;
                out.shape = Some(ParametricShape::Rectangle {
                    width: w,
                    height: h,
                    corner_radii: [r0, r1, r2, r3],
                    corner_style,
                    lock_corners,
                });
            }
            LODA_ARG_ELLIPSE => {
                let rx = cdr32_to_mm(ar.read_i32_le("ellipse rx")?).abs();
                let ry = cdr32_to_mm(ar.read_i32_le("ellipse ry")?).abs();
                let mode = match ar.read_u8("ellipse mode").unwrap_or(0) {
                    1 => EllipseMode::Pie,
                    2 => EllipseMode::Arc,
                    _ => EllipseMode::Full,
                };
                let start_deg = sanitize_f64(ar.read_f64_le("ellipse start").unwrap_or(0.0), 0.0);
                let end_deg = sanitize_f64(ar.read_f64_le("ellipse end").unwrap_or(360.0), 360.0);
                out.shape = Some(ParametricShape::Ellipse {
                    rx,
                    ry,
                    mode,
                    start_deg,
                    end_deg,
                });
            }
            LODA_ARG_POLYGON => {
                let sides = ar.read_u32_le("poly sides")?.clamp(3, 512);
                let rx = cdr32_to_mm(ar.read_i32_le("poly rx")?).abs();
                let ry = cdr32_to_mm(ar.read_i32_le("poly ry")?).abs();
                out.shape = Some(ParametricShape::Polygon {
                    sides,
                    radius_x: rx,
                    radius_y: ry,
                });
            }
            LODA_ARG_STAR => {
                let points = ar.read_u32_le("star points")?.clamp(3, 256);
                let outer_radius = cdr32_to_mm(ar.read_i32_le("star outer_radius")?).abs();
                let inner_ratio = sanitize_f64(ar.read_f64_le("star inner_ratio")?, 0.5);
                let complex = ar.read_u8("star complex")? != 0;
                out.shape = Some(ParametricShape::Star {
                    points,
                    outer_radius,
                    inner_ratio,
                    complex,
                });
            }
            LODA_ARG_EXTRA_SHAPE => {
                out.shape = Some(parse_extra_shape(&mut ar)?);
            }
            LODA_ARG_BITMAP_REF => {
                let bmp_id = ar.read_u32_le("bmp ref id")?;
                let width_mm = cdr32_to_mm(ar.read_i32_le("bmp ref w")?).abs();
                let height_mm = cdr32_to_mm(ar.read_i32_le("bmp ref h")?).abs();
                let has_crop = ar.read_u8("bmp has_crop").unwrap_or(0) != 0;
                let crop_rect = if has_crop {
                    let x0 = cdr32_to_mm(ar.read_i32_le("bmp crop x0")?);
                    let y0 = cdr32_to_mm(ar.read_i32_le("bmp crop y0")?);
                    let x1 = cdr32_to_mm(ar.read_i32_le("bmp crop x1")?);
                    let y1 = cdr32_to_mm(ar.read_i32_le("bmp crop y1")?);
                    Some(Rect2D::new(x0, y0, x1, y1))
                } else {
                    None
                };
                out.bitmap_ref = Some(ParsedBitmapRef {
                    bmp_id,
                    width_mm,
                    height_mm,
                    crop_rect,
                });
            }
            LODA_ARG_TEXT_META => {
                let is_paragraph = ar.read_u8("text is_paragraph")? != 0;
                let frame_w_mm = cdr32_to_mm(ar.read_i32_le("text frame_w")?).abs();
                let frame_h_mm = cdr32_to_mm(ar.read_i32_le("text frame_h")?).abs();
                let columns = ar.read_u32_le("text columns").unwrap_or(1).max(1);
                let gutter_mm =
                    cdr32_to_mm(ar.read_i32_le("text gutter").unwrap_or(50_000)).max(0.0);
                out.text_meta = Some(ParsedTextMeta {
                    is_paragraph,
                    frame_w_mm,
                    frame_h_mm,
                    columns,
                    gutter_mm,
                });
            }
            LODA_ARG_FILL_ID => {
                out.fill_id = Some(ar.read_u32_le("loda fill_id")?);
            }
            LODA_ARG_OUTLINE_ID => {
                out.outline_id = Some(ar.read_u32_le("loda outline_id")?);
            }
            LODA_ARG_STYLE_ID => {
                out.style_id = Some(ar.read_u32_le("loda style_id")?);
            }
            LODA_ARG_OBJ_PROPS => {
                let flags = ar.read_u8("obj flags")?;
                let blend_mode = decode_blend_mode(ar.read_u8("obj blend_mode")?);
                let opacity = sanitize_f32(ar.read_f32_le("obj opacity")?, 1.0).clamp(0.0, 1.0);
                let nlen = usize::from(ar.read_u16_le("obj name len")?);
                let nb = ar.read_bytes(nlen, "obj name")?;
                let name = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
                out.props = Some(ParsedObjProps {
                    name,
                    visible: (flags & 0x01) != 0,
                    locked: (flags & 0x02) != 0,
                    printable: (flags & 0x04) != 0,
                    blend_mode,
                    opacity,
                });
            }
            _ => {}
        }
    }

    Ok(out)
}

fn parse_extra_shape(r: &mut ByteReader<'_>) -> CdrResult<ParametricShape> {
    let tag = r.read_u8("extra shape tag")?;
    match tag {
        1 => {
            let revolutions = sanitize_f64(r.read_f64_le("spiral rev")?, 3.0);
            let radius = cdr32_to_mm(r.read_i32_le("spiral rad")?).abs();
            let logarithmic = r.read_u8("spiral log")? != 0;
            let expansion = sanitize_f64(r.read_f64_le("spiral exp")?, 2.0);
            Ok(ParametricShape::Spiral {
                revolutions,
                radius,
                logarithmic,
                expansion,
            })
        }
        2 => {
            let cols = r.read_u32_le("gp cols")?.clamp(1, 200);
            let rows = r.read_u32_le("gp rows")?.clamp(1, 200);
            let width = cdr32_to_mm(r.read_i32_le("gp w")?).abs();
            let height = cdr32_to_mm(r.read_i32_le("gp h")?).abs();
            Ok(ParametricShape::GraphPaper {
                cols,
                rows,
                width,
                height,
            })
        }
        3 => {
            let rays = r.read_u32_le("impact rays")?.clamp(3, 512);
            let inner_radius = cdr32_to_mm(r.read_i32_le("impact r_in")?).abs();
            let outer_radius = cdr32_to_mm(r.read_i32_le("impact r_out")?).abs();
            let style = if r.read_u8("impact style")? == 1 {
                ImpactStyle::Parallel
            } else {
                ImpactStyle::Radial
            };
            Ok(ParametricShape::Impact {
                rays,
                inner_radius,
                outer_radius,
                style,
            })
        }
        4 => {
            let kind = match r.read_u8("basic shape kind")? {
                1 => BasicShapeKind::ArrowLeft,
                2 => BasicShapeKind::DoubleArrow,
                3 => BasicShapeKind::Banner,
                4 => BasicShapeKind::Callout,
                5 => BasicShapeKind::Heart,
                6 => BasicShapeKind::Cross,
                7 => BasicShapeKind::Diamond,
                8 => BasicShapeKind::Trapezoid,
                _ => BasicShapeKind::ArrowRight,
            };
            let width = cdr32_to_mm(r.read_i32_le("basic w")?).abs();
            let height = cdr32_to_mm(r.read_i32_le("basic h")?).abs();
            Ok(ParametricShape::BasicShape {
                kind,
                width,
                height,
            })
        }
        other => Err(CdrError::InvalidChunk {
            fourcc: "loda".to_owned(),
            reason: format!("unknown extra shape tag {other}"),
        }),
    }
}

/// Parses a Corel-style point+flag curve array (`u32 point_count`, `(x: i32, y: i32) * N`, `flags: u8 * N`).
pub(crate) fn parse_loda_curve(r: &mut ByteReader<'_>) -> CdrResult<BezierPath> {
    let count = usize::try_from(r.read_u32_le("curve point count")?)
        .map_err(|_| CdrError::ArithmeticOverflow("curve point count"))?;
    // Each point consumes 8 bytes coords + 1 byte flag = 9 bytes; verify BEFORE allocating!
    let needed = count
        .checked_mul(9)
        .ok_or(CdrError::ArithmeticOverflow("curve byte size"))?;
    if needed > r.remaining() {
        return Err(CdrError::UnexpectedEof {
            context: "loda curve points",
            needed,
            available: r.remaining(),
        });
    }

    let mut pts = Vec::with_capacity(count);
    for _ in 0..count {
        let x = cdr32_to_mm(r.read_i32_le("curve pt x")?);
        let y = cdr32_to_mm(r.read_i32_le("curve pt y")?);
        pts.push(Point2D::new(x, y));
    }
    let flags = r.read_bytes(count, "curve flags")?;
    Ok(decode_curve_points(&pts, flags))
}

/// Reconstructs a [`BezierPath`] from parallel `(points, node_flags)` arrays.
#[must_use]
pub(crate) fn decode_curve_points(pts: &[Point2D], flags: &[u8]) -> BezierPath {
    let mut subpaths: Vec<Subpath> = Vec::new();
    let mut current: Option<Subpath> = None;
    let mut pending_ctrls: Vec<(Point2D, bool)> = Vec::new();

    for (&pt, &flag) in pts.iter().zip(flags.iter()) {
        let kind_bits = flag & NODE_KIND_MASK;
        let closed = (flag & NODE_FLAG_CLOSED) != 0;
        let cont = match flag & NODE_CONT_MASK {
            NODE_CONT_SMOOTH => NodeKind::Smooth,
            NODE_CONT_SYMMETRICAL => NodeKind::Symmetrical,
            _ => NodeKind::Cusp,
        };

        match kind_bits {
            NODE_MOVE_TO => {
                if let Some(sp) = current.take() {
                    if !sp.nodes.is_empty() {
                        subpaths.push(sp);
                    }
                }
                pending_ctrls.clear();
                let mut sp = Subpath::new(closed);
                sp.nodes.push(PathNode::new(pt, None, None, cont));
                current = Some(sp);
            }
            NODE_CTRL_PT => {
                // Authentic CorelDRAW emits `0xC0` for every cubic Bezier handle; clean-room
                // `encode_curve_points` sets `NODE_CTRL_ABSENT` (`0x02`) when a handle is `None`.
                let present = (flag & NODE_CTRL_ABSENT) == 0;
                pending_ctrls.push((pt, present));
            }
            NODE_LINE_TO => {
                pending_ctrls.clear();
                let sp = current.get_or_insert_with(|| Subpath::new(false));
                if closed {
                    sp.closed = true;
                    // Closing edge back to start node does not duplicate node 0
                    if sp
                        .nodes
                        .first()
                        .is_some_and(|n0| n0.pos.distance_to(pt) < 1e-4)
                    {
                        if let Some(finished) = current.take() {
                            subpaths.push(finished);
                        }
                        continue;
                    }
                }
                sp.nodes.push(PathNode::new(pt, None, None, cont));
                if closed {
                    if let Some(finished) = current.take() {
                        subpaths.push(finished);
                    }
                }
            }
            NODE_CURVE_TO => {
                let sp = current.get_or_insert_with(|| Subpath::new(false));
                let (c1, c2) = if pending_ctrls.len() >= 2 {
                    let (p1, has1) = pending_ctrls[0];
                    let (p2, has2) = pending_ctrls[1];
                    (has1.then_some(p1), has2.then_some(p2))
                } else {
                    (None, None)
                };
                pending_ctrls.clear();

                if let Some(prev) = sp.nodes.last_mut() {
                    prev.handle_out = c1;
                }
                if closed {
                    sp.closed = true;
                    if sp
                        .nodes
                        .first()
                        .is_some_and(|n0| n0.pos.distance_to(pt) < 1e-4)
                    {
                        if let Some(n0) = sp.nodes.first_mut() {
                            n0.handle_in = c2;
                        }
                        if let Some(finished) = current.take() {
                            subpaths.push(finished);
                        }
                        continue;
                    }
                }
                sp.nodes.push(PathNode::new(pt, c2, None, cont));
                if closed {
                    if let Some(finished) = current.take() {
                        subpaths.push(finished);
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(sp) = current {
        if !sp.nodes.is_empty() {
            subpaths.push(sp);
        }
    }

    BezierPath { subpaths }
}

/// Encodes a [`BezierPath`] into parallel `(points, node_flags)` arrays preserving
/// exact `NodeKind` (`Cusp`, `Smooth`, `Symmetrical`), cubic handles, and subpath closure.
#[must_use]
pub(crate) fn encode_curve_points(path: &BezierPath) -> (Vec<Point2D>, Vec<u8>) {
    let mut pts = Vec::new();
    let mut flags = Vec::new();

    for sp in &path.subpaths {
        if sp.nodes.is_empty() {
            continue;
        }
        let first = sp.nodes[0];
        let mut first_flag = NODE_MOVE_TO | encode_node_kind(first.kind);
        if sp.closed && sp.nodes.len() == 1 {
            first_flag |= NODE_FLAG_CLOSED;
        }
        pts.push(first.pos);
        flags.push(first_flag);

        let seg_count = sp.segment_count();
        for s in 0..seg_count {
            let n0 = sp.nodes[s];
            let next_idx = (s + 1) % sp.nodes.len();
            let n1 = sp.nodes[next_idx];
            let is_closing = sp.closed && next_idx == 0;
            let closed_bit = if is_closing { NODE_FLAG_CLOSED } else { 0 };

            if n0.handle_out.is_none() && n1.handle_in.is_none() {
                pts.push(n1.pos);
                flags.push(NODE_LINE_TO | encode_node_kind(n1.kind) | closed_bit);
            } else {
                let (c1, f1) = match n0.handle_out {
                    Some(h) => (h, NODE_CTRL_PT | NODE_CTRL_PRESENT),
                    None => (n0.pos, NODE_CTRL_PT | NODE_CTRL_ABSENT),
                };
                let (c2, f2) = match n1.handle_in {
                    Some(h) => (h, NODE_CTRL_PT | NODE_CTRL_PRESENT),
                    None => (n1.pos, NODE_CTRL_PT | NODE_CTRL_ABSENT),
                };
                pts.push(c1);
                flags.push(f1);
                pts.push(c2);
                flags.push(f2);
                pts.push(n1.pos);
                flags.push(NODE_CURVE_TO | encode_node_kind(n1.kind) | closed_bit);
            }
        }
    }

    (pts, flags)
}

fn encode_node_kind(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Cusp => NODE_CONT_CUSP,
        NodeKind::Smooth => NODE_CONT_SMOOTH,
        NodeKind::Symmetrical => NODE_CONT_SYMMETRICAL,
    }
}

fn is_real_corel_trfd(data: &[u8]) -> bool {
    if data.len() < 56 {
        return false;
    }
    let chunk_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let num_args = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let args_off = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    chunk_len == data.len()
        && (1..=64).contains(&num_args)
        && args_off >= 12
        && args_off
            .checked_add(num_args.saturating_mul(4))
            .is_some_and(|end| end <= data.len())
}

fn parse_real_corel_trfd(data: &[u8]) -> CdrResult<RealCorelMatrix> {
    let mut r = ByteReader::new(data);
    let _chunk_len = r.read_u32_le("trfd len")?;
    let num_args = usize::try_from(r.read_u32_le("trfd num_args")?)
        .map_err(|_| CdrError::ArithmeticOverflow("trfd num_args"))?;
    let args_off = usize::try_from(r.read_u32_le("trfd args_off")?)
        .map_err(|_| CdrError::ArithmeticOverflow("trfd args_off"))?;

    let mut offsets = Vec::with_capacity(num_args);
    for i in 0..num_args {
        let p = args_off + i * 4;
        if p + 4 > data.len() {
            break;
        }
        offsets.push(u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize);
    }

    let mut acc = RealCorelMatrix::IDENTITY;
    for start in offsets {
        if start >= data.len() {
            continue;
        }
        // Locate the `0x0008` affine transform tag either at `start + 8` (v16+ ZCF) or `start` (v7–v15).
        let matrix_off = if start.checked_add(64).is_some_and(|e| e <= data.len())
            && u16::from_le_bytes([data[start + 8], data[start + 9]]) == 0x0008
        {
            Some(start + 16)
        } else if start.checked_add(56).is_some_and(|e| e <= data.len())
            && u16::from_le_bytes([data[start], data[start + 1]]) == 0x0008
        {
            Some(start + 8)
        } else {
            None
        };

        if let Some(moff) = matrix_off {
            if moff.checked_add(48).is_some_and(|e| e <= data.len()) {
                let mut mr = ByteReader::new(&data[moff..moff + 48]);
                let v0 = sanitize_f64(mr.read_f64_le("trfd v0")?, 1.0);
                let v1 = sanitize_f64(mr.read_f64_le("trfd v1")?, 0.0);
                let x0 = sanitize_f64(mr.read_f64_le("trfd x0")?, 0.0);
                let v3 = sanitize_f64(mr.read_f64_le("trfd v3")?, 0.0);
                let v4 = sanitize_f64(mr.read_f64_le("trfd v4")?, 1.0);
                let y0 = sanitize_f64(mr.read_f64_le("trfd y0")?, 0.0);
                let step = RealCorelMatrix {
                    v0,
                    v1,
                    x0_mm: x0 / coree_core::math::CDR_UNITS_PER_MM,
                    v3,
                    v4,
                    y0_mm: y0 / coree_core::math::CDR_UNITS_PER_MM,
                }
                .sanitized();
                acc = step.compose(acc);
            }
        }
    }

    Ok(acc)
}

fn parse_synthetic_trfd_chunk(data: &[u8]) -> CdrResult<Affine2D> {
    let mut r = ByteReader::new(data);
    // Support both tagged (4-byte header + 48 bytes) and raw (48 bytes = 6 * f64)
    if data.len() >= 52 {
        let tag = r.read_u16_le("trfd tag")?;
        let _reserved = r.read_u16_le("trfd reserved")?;
        if tag != 0x0008 {
            // Rewind if first bytes don't match our clean-room tag
            r.seek(0, "trfd rewind")?;
        }
    }
    let a = sanitize_f64(r.read_f64_le("trfd a")?, 1.0);
    let b = sanitize_f64(r.read_f64_le("trfd b")?, 0.0);
    let c = sanitize_f64(r.read_f64_le("trfd c")?, 0.0);
    let d = sanitize_f64(r.read_f64_le("trfd d")?, 1.0);
    let tx_cdr = sanitize_f64(r.read_f64_le("trfd tx")?, 0.0);
    let ty_cdr = sanitize_f64(r.read_f64_le("trfd ty")?, 0.0);
    Ok(Affine2D::new(
        a,
        b,
        c,
        d,
        tx_cdr / coree_core::math::CDR_UNITS_PER_MM,
        ty_cdr / coree_core::math::CDR_UNITS_PER_MM,
    ))
}

/// Decodes a CorelDRAW 12-byte inline color record (`u16 model`, `u16 palette`, `u32`, `[u8; 4] channels`).
fn decode_real_corel_color_record(raw: &[u8]) -> Option<coree_core::Color> {
    if raw.len() < 12 {
        return None;
    }
    let model_id = u16::from_le_bytes([raw[0], raw[1]]);
    let channels = &raw[8..12];
    decode_cdr_color(model_id, channels, None).ok()
}

fn rescale_normalized_fill_to_bounds(fill: &mut Fill, bb: Rect2D) {
    let w = bb.width().max(1e-3);
    let h = bb.height().max(1e-3);
    match fill {
        Fill::LinearGradient { start, end, .. } => {
            *start = Point2D::new(bb.min_x + start.x * w, bb.min_y + start.y * h);
            *end = Point2D::new(bb.min_x + end.x * w, bb.min_y + end.y * h);
        }
        Fill::RadialGradient {
            center,
            radius,
            focal,
            ..
        } => {
            *center = Point2D::new(bb.min_x + center.x * w, bb.min_y + center.y * h);
            *focal = Point2D::new(bb.min_x + focal.x * w, bb.min_y + focal.y * h);
            *radius = (*radius * w.max(h)).max(0.5);
        }
        Fill::ConicalGradient { center, .. } => {
            *center = Point2D::new(bb.min_x + center.x * w, bb.min_y + center.y * h);
        }
        Fill::SquareGradient { center, size, .. } => {
            *center = Point2D::new(bb.min_x + center.x * w, bb.min_y + center.y * h);
            *size = (*size * w.max(h)).max(0.5);
        }
        _ => {}
    }
}

fn parse_real_fild_v16(data: &[u8]) -> CdrResult<(u32, Fill, bool)> {
    let mut r = ByteReader::new(data);
    let fill_id = r.read_u32_le("real fild id")?;
    if data.len() < 14 {
        return Ok((fill_id, Fill::None, true));
    }
    let sub_tag = u16::from_le_bytes([data[4], data[5]]);
    let fill_type = if sub_tag > 16 || data.len() == 22 {
        u16::from_le_bytes([data[12], data[13]])
    } else {
        sub_tag
    };

    let collect_embedded_colors = |slice: &[u8], limit: usize| -> Vec<coree_core::Color> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos + 17 <= slice.len() && out.len() < limit {
            if slice[pos] == 0x01
                && slice[pos + 1] == 0x0C
                && slice[pos + 2] == 0x00
                && slice[pos + 3] == 0x00
                && slice[pos + 4] == 0x00
            {
                if let Some(c) = decode_real_corel_color_record(&slice[pos + 5..pos + 17]) {
                    out.push(c);
                }
                pos += 17;
            } else {
                pos += 1;
            }
        }
        out
    };

    let fill = match fill_type {
        0 => Fill::None,
        1 => {
            // Solid fill: locate embedded Corel color block (`01 0c 00 00 00 <model> ...`) or fixed offset
            let color = find_embedded_corel_color(&data[12..])
                .or_else(|| {
                    if data.len() >= 25 {
                        decode_real_corel_color_record(&data[13..25])
                    } else {
                        None
                    }
                })
                .unwrap_or(coree_core::Color::BLACK);
            Fill::Solid(color)
        }
        2 => {
            // Fountain fill: extract subtype (`0` linear, `1` radial, `2` conical, `3` square),
            // embedded rotation angle, and up to 16 embedded gradient stops.
            let fountain_sub = if data.len() >= 20 {
                u16::from_le_bytes([data[16], data[17]])
            } else {
                0
            };
            let angle_deg = if data.len() >= 28 {
                let raw_a = i32::from_le_bytes([data[24], data[25], data[26], data[27]]);
                let deg = f64::from(raw_a) / 1_000_000.0;
                if deg.is_finite() && deg.abs() <= 360.0 {
                    deg
                } else {
                    0.0
                }
            } else {
                0.0
            };
            let stops = collect_embedded_colors(&data[14..], 16);
            if stops.len() >= 2 {
                let last = (stops.len() - 1) as f32;
                let grad_stops: Vec<GradientStop> = stops
                    .into_iter()
                    .enumerate()
                    .map(|(i, c)| GradientStop::new(i as f32 / last, c))
                    .collect();
                match fountain_sub {
                    1 | 4 => Fill::RadialGradient {
                        center: Point2D::new(0.5, 0.5),
                        radius: 0.5,
                        focal: Point2D::new(0.5, 0.5),
                        stops: grad_stops,
                    },
                    2 => Fill::ConicalGradient {
                        center: Point2D::new(0.5, 0.5),
                        angle_deg,
                        stops: grad_stops,
                    },
                    3 => Fill::SquareGradient {
                        center: Point2D::new(0.5, 0.5),
                        size: 1.0,
                        angle_deg,
                        stops: grad_stops,
                    },
                    _ => {
                        let rad = angle_deg.to_radians();
                        let (s, c) = rad.sin_cos();
                        Fill::LinearGradient {
                            start: Point2D::new(0.5 - 0.5 * c, 0.5 - 0.5 * s),
                            end: Point2D::new(0.5 + 0.5 * c, 0.5 + 0.5 * s),
                            stops: grad_stops,
                        }
                    }
                }
            } else if let Some(c) = stops.into_iter().next() {
                Fill::Solid(c)
            } else {
                Fill::None
            }
        }
        6 => {
            // Two-color pattern fill: extract fg & bg embedded colors instead of dropping to None
            let cols = collect_embedded_colors(&data[14..], 2);
            let fg = cols.first().cloned().unwrap_or(coree_core::Color::BLACK);
            let bg = cols.get(1).cloned().unwrap_or(coree_core::Color::WHITE);
            Fill::TwoColorPattern {
                fg,
                bg,
                pattern_name: "Checker".to_owned(),
                tile_w: 8.0,
                tile_h: 8.0,
            }
        }
        7 | 9 | 10 | 11 => {
            // Procedural Texture / Full-color Pattern fill: extract embedded palette colors cleanly
            let cols = collect_embedded_colors(&data[14..], 2);
            if cols.is_empty() {
                Fill::None
            } else {
                let fg = cols[0].clone();
                let bg = cols.get(1).cloned().unwrap_or(coree_core::Color::WHITE);
                Fill::TextureFill {
                    preset: "marble".to_owned(),
                    seed: u64::from(fill_id).max(1),
                    fg,
                    bg,
                }
            }
        }
        _ => Fill::None,
    };
    Ok((fill_id, fill, true))
}

fn find_embedded_corel_color(slice: &[u8]) -> Option<coree_core::Color> {
    if slice.len() < 17 {
        return None;
    }
    for i in 0..=slice.len() - 17 {
        if slice[i] == 0x01
            && slice[i + 1] == 0x0C
            && slice[i + 2] == 0x00
            && slice[i + 3] == 0x00
            && slice[i + 4] == 0x00
        {
            if let Some(c) = decode_real_corel_color_record(&slice[i + 5..i + 17]) {
                return Some(c);
            }
        }
    }
    None
}

fn parse_fild_chunk(data: &[u8], is_zcf: bool) -> CdrResult<(u32, Fill, bool)> {
    if is_zcf
        || (data.len() >= 14 && u16::from_le_bytes([data[4], data[5]]) > 16)
        || (data.len() == 22 && u16::from_le_bytes([data[4], data[5]]) == 0x0514)
    {
        return parse_real_fild_v16(data);
    }

    let mut r = ByteReader::new(data);
    let fill_id = r.read_u32_le("fild id")?;
    let fill_type = r.read_u16_le("fild type")?;
    let fill = match fill_type {
        0 => Fill::None,
        1 => Fill::Solid(read_tagged_color(&mut r, "fild solid color")?),
        2 => {
            let subtype = r.read_u8("fild fountain subtype")?;
            let p0_x = cdr32_to_mm(r.read_i32_le("fild p0.x")?);
            let p0_y = cdr32_to_mm(r.read_i32_le("fild p0.y")?);
            let p1_x = cdr32_to_mm(r.read_i32_le("fild p1.x")?);
            let p1_y = cdr32_to_mm(r.read_i32_le("fild p1.y")?);
            let scalar = sanitize_f64(r.read_f64_le("fild scalar")?, 0.0);
            let angle = sanitize_f64(r.read_f64_le("fild angle")?, 0.0);
            let stop_count = usize::from(r.read_u16_le("fild stop_count")?);
            if stop_count > 1024 || stop_count.saturating_mul(8) > r.remaining() {
                return Err(CdrError::InvalidChunk {
                    fourcc: "fild".to_owned(),
                    reason: format!("invalid gradient stop count {stop_count}"),
                });
            }
            let mut stops = Vec::with_capacity(stop_count);
            for _ in 0..stop_count {
                let offset = sanitize_f32(r.read_f32_le("fild stop offset")?, 0.0);
                let color = read_tagged_color(&mut r, "fild stop color")?;
                stops.push(GradientStop::new(offset, color));
            }
            match subtype {
                1 => Fill::RadialGradient {
                    center: Point2D::new(p0_x, p0_y),
                    radius: scalar.abs(),
                    focal: Point2D::new(p1_x, p1_y),
                    stops,
                },
                2 => Fill::ConicalGradient {
                    center: Point2D::new(p0_x, p0_y),
                    angle_deg: angle,
                    stops,
                },
                3 => Fill::SquareGradient {
                    center: Point2D::new(p0_x, p0_y),
                    size: scalar.abs(),
                    angle_deg: angle,
                    stops,
                },
                _ => Fill::LinearGradient {
                    start: Point2D::new(p0_x, p0_y),
                    end: Point2D::new(p1_x, p1_y),
                    stops,
                },
            }
        }
        6 => {
            let fg = read_tagged_color(&mut r, "fild 2c fg")?;
            let bg = read_tagged_color(&mut r, "fild 2c bg")?;
            let tile_w = cdr32_to_mm(r.read_i32_le("fild 2c tile_w")?).abs();
            let tile_h = cdr32_to_mm(r.read_i32_le("fild 2c tile_h")?).abs();
            let nlen = usize::from(r.read_u16_le("fild 2c name len")?);
            let nb = r.read_bytes(nlen, "fild 2c name")?;
            let pattern_name = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
            Fill::TwoColorPattern {
                fg,
                bg,
                pattern_name,
                tile_w,
                tile_h,
            }
        }
        7 => {
            let fg = read_tagged_color(&mut r, "fild tex fg")?;
            let bg = read_tagged_color(&mut r, "fild tex bg")?;
            let seed_lo = u64::from(r.read_u32_le("fild tex seed lo")?);
            let seed_hi = u64::from(r.read_u32_le("fild tex seed hi")?);
            let nlen = usize::from(r.read_u16_le("fild tex preset len")?);
            let nb = r.read_bytes(nlen, "fild tex preset")?;
            let preset = decode_cdr_text(nb, Some(CODEPAGE_UTF8), None);
            Fill::TextureFill {
                preset,
                seed: seed_lo | (seed_hi << 32),
                fg,
                bg,
            }
        }
        9 => {
            let rows = usize::from(r.read_u16_le("fild mesh rows")?);
            let cols = usize::from(r.read_u16_le("fild mesh cols")?);
            let count = usize::from(r.read_u16_le("fild mesh count")?);
            if count > 4096 || count.saturating_mul(16) > r.remaining() {
                return Err(CdrError::InvalidChunk {
                    fourcc: "fild".to_owned(),
                    reason: format!("invalid mesh node count {count}"),
                });
            }
            let mut nodes = Vec::with_capacity(count);
            for _ in 0..count {
                let row = usize::from(r.read_u16_le("mesh row")?);
                let col = usize::from(r.read_u16_le("mesh col")?);
                let x = cdr32_to_mm(r.read_i32_le("mesh x")?);
                let y = cdr32_to_mm(r.read_i32_le("mesh y")?);
                let color = read_tagged_color(&mut r, "mesh color")?;
                nodes.push(MeshNode {
                    row,
                    col,
                    pos: Point2D::new(x, y),
                    color,
                });
            }
            Fill::MeshFill { rows, cols, nodes }
        }
        _ => Fill::None,
    };
    Ok((fill_id, fill, false))
}

fn is_real_outl_v16(data: &[u8]) -> bool {
    if data.len() < 90 {
        return false;
    }
    let color_block_len = u32::from_le_bytes([data[20], data[21], data[22], data[23]]) as usize;
    color_block_len >= 17
        && 24usize
            .checked_add(color_block_len)
            .and_then(|b| b.checked_add(24))
            .is_some_and(|end| end <= data.len())
        && data[24] == 0x01
}

fn parse_real_outl_v16(data: &[u8]) -> CdrResult<(u32, OutlineStyle, bool)> {
    if data.len() < 42 {
        return Err(CdrError::UnexpectedEof {
            context: "real outl v16 header",
            needed: 42,
            available: data.len(),
        });
    }
    let outline_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let miter_e6 = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    let miter_limit = if miter_e6 > 0 {
        (f64::from(miter_e6) / 1_000_000.0).clamp(1.0, 90.0)
    } else {
        4.0
    };
    let color_block_len = u32::from_le_bytes([data[20], data[21], data[22], data[23]]) as usize;
    let color = decode_real_corel_color_record(&data[29..41]).unwrap_or(coree_core::Color::BLACK);

    let Some(base) = 24usize.checked_add(color_block_len) else {
        return Ok((outline_id, OutlineStyle::none(), true));
    };
    if base.saturating_add(24) > data.len() {
        return Err(CdrError::UnexpectedEof {
            context: "real outl v16 body",
            needed: base.saturating_add(24),
            available: data.len(),
        });
    }
    let line_type = u16::from_le_bytes([data[base + 8], data[base + 9]]);
    let caps_raw = u16::from_le_bytes([data[base + 10], data[base + 11]]);
    let join_raw = u16::from_le_bytes([data[base + 12], data[base + 13]]);
    let width_cdr = i32::from_le_bytes([
        data[base + 14],
        data[base + 15],
        data[base + 16],
        data[base + 17],
    ]);
    let stretch_raw = u16::from_le_bytes([data[base + 18], data[base + 19]]);
    let angle_e6 = i32::from_le_bytes([
        data[base + 20],
        data[base + 21],
        data[base + 22],
        data[base + 23],
    ]);

    // In CorelDRAW `outl`, `line_type == 1` (bit 0x01 set without bit 0x02/0x04) means NO OUTLINE!
    if (line_type & 0x0001) != 0 && (line_type & 0x0006) == 0 {
        return Ok((outline_id, OutlineStyle::none(), true));
    }

    let width_mm = cdr32_to_mm(width_cdr).max(0.0);
    let cap = match caps_raw {
        1 => LineCap::Round,
        2 => LineCap::Square,
        _ => LineCap::Butt,
    };
    let join = match join_raw {
        1 => LineJoin::Round,
        2 => LineJoin::Bevel,
        _ => LineJoin::Miter,
    };
    let mut dash_pattern = Vec::new();
    if (line_type & 0x0004) != 0 {
        // Check optional dash segment table `[u16 count, u16 seg0, u16 seg1, ...]` after `base + 24`
        if base + 28 <= data.len() {
            let count = usize::from(u16::from_le_bytes([data[base + 24], data[base + 25]]));
            if (1..=32).contains(&count) && base + 26 + count * 2 <= data.len() {
                for i in 0..count {
                    let seg =
                        u16::from_le_bytes([data[base + 26 + i * 2], data[base + 27 + i * 2]]);
                    if seg > 0 {
                        dash_pattern.push((f64::from(seg) * 0.25).max(0.25));
                    }
                }
            }
        }
        if dash_pattern.is_empty() {
            dash_pattern = vec![2.0, 1.0];
        }
    }

    Ok((
        outline_id,
        OutlineStyle {
            color,
            width_mm,
            is_hairline: width_mm > 0.0 && width_mm <= 0.08,
            cap,
            join,
            miter_limit,
            dash_pattern,
            start_arrow: ArrowheadStyle::None,
            end_arrow: ArrowheadStyle::None,
            behind_fill: (line_type & 0x0010) != 0,
            scale_with_object: (line_type & 0x0020) != 0,
            overprint: (line_type & 0x0040) != 0,
            calligraphy_angle_deg: f64::from(angle_e6) / 1_000_000.0,
            calligraphy_stretch: if stretch_raw > 0 {
                (f64::from(stretch_raw) / 100.0).clamp(0.01, 10.0)
            } else {
                1.0
            },
        },
        false,
    ))
}

fn parse_outl_chunk(data: &[u8], is_zcf: bool) -> CdrResult<(u32, OutlineStyle, bool)> {
    if is_real_outl_v16(data) {
        return parse_real_outl_v16(data);
    }
    if is_zcf && data.len() >= 12 {
        // Fallback for non-standard ZCF outl
        let id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        return Ok((id, OutlineStyle::none(), true));
    }

    let mut r = ByteReader::new(data);
    let outline_id = r.read_u32_le("outl id")?;
    let flags = r.read_u16_le("outl flags")?;
    if (flags & 0x0001) != 0 {
        return Ok((outline_id, OutlineStyle::none(), true));
    }
    let width_mm = cdr32_to_mm(r.read_i32_le("outl width")?).max(0.0);
    let cap = match r.read_u8("outl cap")? {
        1 => LineCap::Round,
        2 => LineCap::Square,
        _ => LineCap::Butt,
    };
    let join = match r.read_u8("outl join")? {
        1 => LineJoin::Round,
        2 => LineJoin::Bevel,
        _ => LineJoin::Miter,
    };
    let miter_limit = sanitize_f64(r.read_f64_le("outl miter")?, 4.0).max(1.0);
    let calligraphy_angle_deg = sanitize_f64(r.read_f64_le("outl cal_angle")?, 0.0);
    let calligraphy_stretch = sanitize_f64(r.read_f64_le("outl cal_stretch")?, 1.0).max(0.01);
    let start_arrow = decode_arrow(r.read_u8("outl start_arrow")?);
    let end_arrow = decode_arrow(r.read_u8("outl end_arrow")?);
    let dash_count = usize::from(r.read_u16_le("outl dash_count")?);
    if dash_count > 256 || dash_count.saturating_mul(4) > r.remaining() {
        return Err(CdrError::InvalidChunk {
            fourcc: "outl".to_owned(),
            reason: format!("invalid dash count {dash_count}"),
        });
    }
    let mut dash_pattern = Vec::with_capacity(dash_count);
    for _ in 0..dash_count {
        dash_pattern.push(cdr32_to_mm(r.read_i32_le("outl dash len")?).max(0.0));
    }
    let color = read_tagged_color(&mut r, "outl color")?;

    Ok((
        outline_id,
        OutlineStyle {
            color,
            width_mm,
            is_hairline: (flags & 0x0002) != 0,
            cap,
            join,
            miter_limit,
            dash_pattern,
            start_arrow,
            end_arrow,
            behind_fill: (flags & 0x0004) != 0,
            scale_with_object: (flags & 0x0008) != 0,
            overprint: (flags & 0x0010) != 0,
            calligraphy_angle_deg,
            calligraphy_stretch,
        },
        false,
    ))
}

fn parse_stlt_chunk(data: &[u8]) -> CdrResult<(u32, ParsedStyleEntry)> {
    let mut r = ByteReader::new(data);
    let style_id = r.read_u32_le("stlt id")?;
    let entry = parse_stlt_body(&mut r)?;
    Ok((style_id, entry))
}

fn parse_stlt_body(r: &mut ByteReader<'_>) -> CdrResult<ParsedStyleEntry> {
    let codepage_raw = r.read_u16_le("stlt codepage")?;
    let charset_raw = r.read_u8("stlt charset")?;
    let flags = r.read_u8("stlt flags")?;
    let direction = decode_text_direction(r.read_u8("stlt dir")?);
    let align = decode_text_align(r.read_u8("stlt align")?);
    let font_size_pt = sanitize_f64(r.read_f64_le("stlt font_size")?, 12.0).max(1.0);
    let letter_spacing_mm = sanitize_f64(r.read_f64_le("stlt letter_spacing")?, 0.0);
    let line_height_ratio = sanitize_f64(r.read_f64_le("stlt line_height")?, 1.2).max(0.5);
    let kashida_stretch = sanitize_f32(r.read_f32_le("stlt kashida")?, 0.0).clamp(0.0, 2.0);
    let font_len = usize::from(r.read_u16_le("stlt font_len")?);
    let fb = r.read_bytes(font_len, "stlt font_family")?;
    let font_family = decode_cdr_text(
        fb,
        (codepage_raw != 0).then_some(codepage_raw),
        (charset_raw != 0).then_some(charset_raw),
    );
    let fill = read_tagged_color(r, "stlt color")?;

    Ok(ParsedStyleEntry {
        style: TextStyle {
            font_family: if font_family.is_empty() {
                "Noto Sans".to_owned()
            } else {
                font_family
            },
            font_size_pt,
            bold: (flags & 0x01) != 0,
            italic: (flags & 0x02) != 0,
            underline: (flags & 0x04) != 0,
            strikethrough: (flags & 0x08) != 0,
            letter_spacing_mm,
            line_height_ratio,
            fill,
            direction,
            align,
            kashida_stretch,
        },
        codepage: (codepage_raw != 0).then_some(codepage_raw),
        charset: (charset_raw != 0).then_some(charset_raw),
    })
}

fn parse_txsm_chunk(
    data: &[u8],
    loda_style_id: Option<u32>,
    inline_style: Option<&ParsedStyleEntry>,
    text_meta: Option<ParsedTextMeta>,
    styles: &HashMap<u32, ParsedStyleEntry>,
    cdss_default: Option<&TextStyle>,
) -> CdrResult<NodePayload> {
    let mut r = ByteReader::new(data);
    let format_ver = r.read_u8("txsm version")?;

    match format_ver {
        0x01 if data.len() >= 21 => {
            // Check whether this is our exact clean-room `0x01` layout (`21 + text_len == data.len()`)
            let declared_len =
                u32::from_le_bytes([data[17], data[18], data[19], data[20]]) as usize;
            if 21usize.checked_add(declared_len) == Some(data.len()) {
                // Legacy single-run text stream (supports CP1256 Arabic, UTF-16LE, UTF-8)
                let style_id = r.read_u32_le("txsm style_id")?;
                let cp_raw = r.read_u16_le("txsm codepage")?;
                let cs_raw = r.read_u8("txsm charset")?;
                let is_para = r.read_u8("txsm is_paragraph")? != 0;
                let frame_w = cdr32_to_mm(r.read_i32_le("txsm frame_w")?).abs();
                let frame_h = cdr32_to_mm(r.read_i32_le("txsm frame_h")?).abs();
                let text_len = usize::try_from(r.read_u32_le("txsm text_len")?)
                    .map_err(|_| CdrError::ArithmeticOverflow("txsm text_len"))?;
                let raw_text = r.read_bytes(text_len, "txsm text bytes")?;

                let resolved_entry = inline_style
                    .or_else(|| styles.get(&style_id))
                    .or_else(|| loda_style_id.and_then(|sid| styles.get(&sid)));

                let codepage = (cp_raw != 0)
                    .then_some(cp_raw)
                    .or_else(|| resolved_entry.and_then(|e| e.codepage));
                let charset = (cs_raw != 0)
                    .then_some(cs_raw)
                    .or_else(|| resolved_entry.and_then(|e| e.charset));

                let decoded = decode_cdr_text(raw_text, codepage, charset);
                let mut style = resolved_entry.map_or_else(
                    || cdss_default.cloned().unwrap_or_default(),
                    |e| e.style.clone(),
                );
                if contains_arabic(&decoded) && style.direction == TextDirection::Ltr {
                    style.direction = TextDirection::Rtl;
                }

                if is_para || text_meta.is_some_and(|m| m.is_paragraph) {
                    let (w, h, cols, gutter) = match text_meta {
                        Some(m) if m.frame_w_mm > 0.1 && m.frame_h_mm > 0.1 => {
                            (m.frame_w_mm, m.frame_h_mm, m.columns, m.gutter_mm)
                        }
                        _ => (frame_w.max(10.0), frame_h.max(10.0), 1, 5.0),
                    };
                    let mut para = ParagraphText::new(decoded, Vec2D::new(w, h), style);
                    para.columns = cols;
                    para.column_gutter_mm = gutter;
                    return Ok(NodePayload::ParagraphText(para));
                }
                return Ok(NodePayload::ArtisticText(ArtisticText::from_plain(
                    decoded, style,
                )));
            }
            parse_real_corel_txsm(data, inline_style, text_meta, cdss_default)
        }
        0x02 if data.len() >= 22 => {
            // Multi-run styled text stream
            let is_para = r.read_u8("txsm2 is_para")? != 0;
            let direction = decode_text_direction(r.read_u8("txsm2 dir")?);
            let align = decode_text_align(r.read_u8("txsm2 align")?);
            let frame_w = cdr32_to_mm(r.read_i32_le("txsm2 frame_w")?).abs();
            let frame_h = cdr32_to_mm(r.read_i32_le("txsm2 frame_h")?).abs();
            let columns = r.read_u32_le("txsm2 cols")?.max(1);
            let gutter = cdr32_to_mm(r.read_i32_le("txsm2 gutter")?).max(0.0);
            let run_count = usize::from(r.read_u16_le("txsm2 run_count")?);
            if run_count > 4096 || run_count.saturating_mul(12) > r.remaining() {
                return Err(CdrError::InvalidChunk {
                    fourcc: "txsm".to_owned(),
                    reason: format!("invalid run_count {run_count}"),
                });
            }

            let mut runs = Vec::with_capacity(run_count);
            for _ in 0..run_count {
                let cp_raw = r.read_u16_le("txsm2 run cp")?;
                let cs_raw = r.read_u8("txsm2 run cs")?;
                let style_id = r.read_u32_le("txsm2 run style_id")?;
                let has_inline = r.read_u8("txsm2 run has_inline")? != 0;
                let entry = if has_inline {
                    Some(parse_stlt_body(&mut r)?)
                } else {
                    styles
                        .get(&style_id)
                        .cloned()
                        .or_else(|| inline_style.cloned())
                };
                let text_len = usize::try_from(r.read_u32_le("txsm2 run text_len")?)
                    .map_err(|_| CdrError::ArithmeticOverflow("txsm2 text_len"))?;
                let raw_bytes = r.read_bytes(text_len, "txsm2 run text")?;

                let cp = (cp_raw != 0)
                    .then_some(cp_raw)
                    .or_else(|| entry.as_ref().and_then(|e| e.codepage));
                let cs = (cs_raw != 0)
                    .then_some(cs_raw)
                    .or_else(|| entry.as_ref().and_then(|e| e.charset));

                let text = decode_cdr_text(raw_bytes, cp, cs);
                let style =
                    entry.map_or_else(|| cdss_default.cloned().unwrap_or_default(), |e| e.style);
                runs.push(TextRun::new(text, style));
            }

            if is_para {
                Ok(NodePayload::ParagraphText(ParagraphText {
                    runs,
                    frame_size: Vec2D::new(frame_w.max(1.0), frame_h.max(1.0)),
                    columns,
                    column_gutter_mm: gutter,
                    linked_next_frame: None,
                    wrap_mode: coree_core::TextWrapMode::None,
                    drop_cap: None,
                    direction,
                    align,
                }))
            } else {
                Ok(NodePayload::ArtisticText(ArtisticText {
                    runs,
                    direction,
                    align,
                }))
            }
        }
        _ => parse_real_corel_txsm(data, inline_style, text_meta, cdss_default),
    }
}

/// Decodes an authentic CorelDRAW v12–26 `txsm`/`txst` binary text stream (supporting
/// UTF-16LE Unicode runs and legacy Windows-1256 Arabic CP1256 runs).
fn parse_real_corel_txsm(
    data: &[u8],
    inline_style: Option<&ParsedStyleEntry>,
    text_meta: Option<ParsedTextMeta>,
    cdss_default: Option<&TextStyle>,
) -> CdrResult<NodePayload> {
    let cp = inline_style.and_then(|s| s.codepage);
    let cs = inline_style.and_then(|s| s.charset);

    // Authentic CorelDRAW v16+ `txsm` header:
    // `[0..4]` frame_flag (u32), `[4..8]` text_id (u32), `[8..12]` encoding (`1` = UTF-16LE, `0` = 8-bit),
    // `[12..16]` char_count (u32), followed by text payload (2*char_count bytes for UTF-16LE, or char_count bytes).
    let decoded = if data.len() >= 16 {
        let encoding = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let char_count = u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
        if encoding == 1
            && char_count > 0
            && char_count
                .checked_mul(2)
                .and_then(|b| 16usize.checked_add(b))
                .is_some_and(|end| end <= data.len())
        {
            let byte_len = char_count * 2;
            decode_cdr_text(&data[16..16 + byte_len], Some(CODEPAGE_UTF16LE), None)
        } else if encoding == 0
            && char_count > 0
            && 16usize
                .checked_add(char_count)
                .is_some_and(|end| end <= data.len())
        {
            decode_cdr_text(&data[16..16 + char_count], cp, cs)
        } else {
            decode_cdr_text(data, cp, cs)
        }
    } else {
        decode_cdr_text(data, cp, cs)
    };

    let mut style = inline_style.map_or_else(
        || cdss_default.cloned().unwrap_or_default(),
        |s| s.style.clone(),
    );
    if contains_arabic(&decoded) && style.direction == TextDirection::Ltr {
        style.direction = TextDirection::Rtl;
    }

    if let Some(m) = text_meta {
        if m.is_paragraph {
            let mut para = ParagraphText::new(
                decoded,
                Vec2D::new(m.frame_w_mm.max(10.0), m.frame_h_mm.max(10.0)),
                style,
            );
            para.columns = m.columns.max(1);
            para.column_gutter_mm = m.gutter_mm;
            return Ok(NodePayload::ParagraphText(para));
        }
    }

    Ok(NodePayload::ArtisticText(ArtisticText::from_plain(
        decoded, style,
    )))
}

fn parse_lens_chunk(data: &[u8]) -> CdrResult<LensConfig> {
    let mut r = ByteReader::new(data);
    let lens_type = r.read_u8("lens type")?;
    let flags = r.read_u8("lens flags")?;
    let frozen = (flags & 0x01) != 0;
    let remove_face = (flags & 0x02) != 0;
    let has_viewpoint = (flags & 0x04) != 0;
    let viewpoint = if has_viewpoint {
        let vx = cdr32_to_mm(r.read_i32_le("lens vp x")?);
        let vy = cdr32_to_mm(r.read_i32_le("lens vp y")?);
        Some(Point2D::new(vx, vy))
    } else {
        None
    };

    let kind = match lens_type {
        1 => LensKind::Brighten {
            rate_pct: sanitize_f32(r.read_f32_le("lens brighten rate")?, 0.0),
        },
        2 => {
            let rate_pct = sanitize_f32(r.read_f32_le("lens color_add rate")?, 50.0);
            let color = read_tagged_color(&mut r, "lens color_add color")?;
            LensKind::ColorAdd { color, rate_pct }
        }
        3 => {
            let rate_pct = sanitize_f32(r.read_f32_le("lens color_limit rate")?, 50.0);
            let color = read_tagged_color(&mut r, "lens color_limit color")?;
            LensKind::ColorLimit { color, rate_pct }
        }
        4 => {
            let palette_mode = match r.read_u8("lens color_map mode")? {
                1 => ColorMapMode::ForwardRainbow,
                2 => ColorMapMode::ReverseRainbow,
                _ => ColorMapMode::Direct,
            };
            let from_color = read_tagged_color(&mut r, "lens color_map from")?;
            let to_color = read_tagged_color(&mut r, "lens color_map to")?;
            LensKind::CustomColorMap {
                from_color,
                to_color,
                palette_mode,
            }
        }
        5 => LensKind::FishEye {
            rate_pct: sanitize_f32(r.read_f32_le("lens fisheye rate")?, 100.0),
        },
        6 => LensKind::HeatMap {
            palette_rotation_deg: sanitize_f32(r.read_f32_le("lens heatmap rot")?, 0.0),
        },
        7 => LensKind::Invert,
        8 => LensKind::Magnify {
            amount: sanitize_f32(r.read_f32_le("lens magnify amount")?, 2.0),
        },
        9 => LensKind::TintedGrayscale {
            tint: read_tagged_color(&mut r, "lens tinted_gray color")?,
        },
        10 => {
            let rate_pct = sanitize_f32(r.read_f32_le("lens transparency rate")?, 50.0);
            let color = read_tagged_color(&mut r, "lens transparency color")?;
            LensKind::Transparency { color, rate_pct }
        }
        11 => {
            let has_outl = r.read_u8("lens wf has_outline")? != 0;
            let override_outline = if has_outl {
                Some(read_tagged_color(&mut r, "lens wf outline")?)
            } else {
                None
            };
            let has_fill = r.read_u8("lens wf has_fill")? != 0;
            let override_fill = if has_fill {
                Some(read_tagged_color(&mut r, "lens wf fill")?)
            } else {
                None
            };
            LensKind::Wireframe {
                override_outline,
                override_fill,
            }
        }
        other => {
            return Err(CdrError::InvalidChunk {
                fourcc: "lens".to_owned(),
                reason: format!("unknown lens type {other}"),
            });
        }
    };

    Ok(LensConfig {
        kind: kind.clamped(),
        frozen,
        viewpoint,
        remove_face,
    })
}

// =============================================================================
// RIFF CDR Writer
// =============================================================================

/// Exports a [`Document`] to clean-room RIFF CDR v12 bytes (with `LIST cmpr` compression).
pub fn write_cdr_riff(doc: &Document) -> CdrResult<Vec<u8>> {
    Ok(write_cdr_riff_with_options(doc, RiffWriteOptions::default())?.riff_bytes)
}

/// Exports a [`Document`] to clean-room RIFF CDR bytes with full control over version,
/// compression, and external bitmap separation.
pub fn write_cdr_riff_with_options(
    doc: &Document,
    opts: RiffWriteOptions,
) -> CdrResult<RiffWriteOutput> {
    let mut external_bitmaps: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut asset_id_to_num: HashMap<ImageAssetId, u32> = HashMap::new();
    let mut bmp_nodes: Vec<RiffNode> = Vec::new();

    for (idx, (&aid, asset)) in doc.assets.iter().enumerate() {
        let num_id = (idx as u32).saturating_add(1);
        asset_id_to_num.insert(aid, num_id);
        let payload = encode_bmp_chunk_payload(num_id, asset)?;
        if opts.externalize_bitmaps {
            external_bitmaps.push((num_id, payload));
        } else {
            bmp_nodes.push(RiffNode::chunk(*b"bmp ", payload));
        }
    }

    let mut doc_children: Vec<RiffNode> = Vec::new();

    // Document mcfg
    let (def_w, def_h) = doc
        .pages
        .first()
        .map_or((210.0, 297.0), |p| (p.width_mm, p.height_mm));
    doc_children.push(RiffNode::chunk(
        *b"mcfg",
        encode_doc_mcfg(def_w, def_h, doc.color_mode, doc.dpi, &doc.title),
    ));

    // Palette chunk (`plte`)
    if !doc.document_palette.swatches.is_empty() {
        doc_children.push(RiffNode::chunk(
            *b"plte",
            encode_plte_chunk(&doc.document_palette.swatches),
        ));
    }

    // Embedded bitmaps list (`LIST bmps`)
    if !bmp_nodes.is_empty() {
        doc_children.push(RiffNode::list(*b"bmps", bmp_nodes));
    }

    // Pages (`LIST page`)
    for page in &doc.pages {
        let mut page_children = Vec::new();
        page_children.push(RiffNode::chunk(*b"mcfg", encode_page_mcfg(page)));
        for layer in &page.layers {
            let mut layer_children = Vec::new();
            layer_children.push(RiffNode::chunk(*b"layr", encode_layer_meta(layer)));
            for &oid in &layer.objects {
                if let Some(node) = encode_scene_object(doc, oid, &asset_id_to_num, 0)? {
                    layer_children.push(node);
                }
            }
            page_children.push(RiffNode::list(*b"layr", layer_children));
        }
        doc_children.push(RiffNode::list(*b"page", page_children));
    }

    // Re-emit document-level Unknown-Chunk Passthrough verbatim!
    for unk in &doc.unknown_doc_chunks {
        doc_children.push(unknown_chunk_to_riff_node(unk, RiffEndian::Little));
    }

    let mut root_children = Vec::new();
    root_children.push(RiffNode::chunk(
        *b"vrsn",
        opts.version.to_le_bytes().to_vec(),
    ));

    // Tiny 4x4 RGBA preview DIB in `DISP`
    if let Ok(preview_dib) = encode_dib_rgba(4, 4, &[245u8; 64]) {
        root_children.push(RiffNode::chunk(*b"DISP", preview_dib));
    }

    let doc_list = RiffNode::list(*b"doc ", doc_children);
    if opts.compress_doc {
        root_children.push(RiffNode::cmpr_list(vec![doc_list]));
    } else {
        root_children.push(doc_list);
    }

    let riff_doc = RiffDocument {
        endian: RiffEndian::Little,
        form_type: opts.form_type,
        children: root_children,
    };
    let riff_bytes = write_riff(&riff_doc)?;

    Ok(RiffWriteOutput {
        riff_bytes,
        external_bitmaps,
    })
}

fn encode_bmp_chunk_payload(bmp_id: u32, asset: &RasterAsset) -> CdrResult<Vec<u8>> {
    let dib = encode_dib_rgba(asset.width, asset.height, &asset.rgba_bytes)?;
    let dpi_x100 = (sanitize_f32(asset.source_dpi, 300.0).max(1.0) * 100.0).round() as u32;
    let cm_byte = if asset.color_mode == ColorMode::Rgb {
        1u8
    } else {
        0u8
    };
    let mut out = Vec::with_capacity(13usize.saturating_add(dib.len()));
    out.extend_from_slice(CBMP_MAGIC);
    out.extend_from_slice(&bmp_id.to_le_bytes());
    out.extend_from_slice(&dpi_x100.to_le_bytes());
    out.push(cm_byte);
    out.extend_from_slice(&dib);
    Ok(out)
}

fn encode_doc_mcfg(w_mm: f64, h_mm: f64, mode: ColorMode, dpi: f32, title: &str) -> Vec<u8> {
    let tb = title.as_bytes();
    let tlen = tb.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(15 + tlen);
    out.extend_from_slice(&mm_to_cdr32(w_mm).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(h_mm).to_le_bytes());
    out.push(if mode == ColorMode::Rgb { 1 } else { 0 });
    out.extend_from_slice(&sanitize_f32(dpi, 300.0).to_le_bytes());
    out.extend_from_slice(&(tlen as u16).to_le_bytes());
    out.extend_from_slice(&tb[..tlen]);
    out
}

fn encode_plte_chunk(swatches: &[ColorSwatch]) -> Vec<u8> {
    let count = swatches.len().min(u16::MAX as usize);
    let mut out = Vec::new();
    out.extend_from_slice(&(count as u16).to_le_bytes());
    for sw in &swatches[..count] {
        let nb = sw.name.as_bytes();
        let nlen = nb.len().min(u16::MAX as usize);
        out.extend_from_slice(&(nlen as u16).to_le_bytes());
        out.extend_from_slice(&nb[..nlen]);
        write_tagged_color(&mut out, &sw.color);
    }
    out
}

fn encode_page_mcfg(page: &Page) -> Vec<u8> {
    let nb = page.name.as_bytes();
    let nlen = nb.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(15 + nlen);
    out.extend_from_slice(&mm_to_cdr32(page.width_mm).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(page.height_mm).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(page.bleed_mm).to_le_bytes());
    out.push(u8::from(page.facing_pages));
    out.extend_from_slice(&(nlen as u16).to_le_bytes());
    out.extend_from_slice(&nb[..nlen]);
    out
}

fn encode_layer_meta(layer: &Layer) -> Vec<u8> {
    let mut flags = 0u8;
    if layer.visible {
        flags |= 0x01;
    }
    if layer.locked {
        flags |= 0x02;
    }
    if layer.printable {
        flags |= 0x04;
    }
    if layer.is_cut_layer {
        flags |= 0x08;
    }
    let ms = match layer.master_scope {
        MasterLayerScope::None => 0u8,
        MasterLayerScope::AllPages => 1,
        MasterLayerScope::OddPages => 2,
        MasterLayerScope::EvenPages => 3,
    };
    let nb = layer.name.as_bytes();
    let nlen = nb.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(4 + nlen);
    out.push(flags);
    out.push(ms);
    out.extend_from_slice(&(nlen as u16).to_le_bytes());
    out.extend_from_slice(&nb[..nlen]);
    out
}

fn encode_scene_object(
    doc: &Document,
    oid: ObjectId,
    bmp_map: &HashMap<ImageAssetId, u32>,
    depth: usize,
) -> CdrResult<Option<RiffNode>> {
    if depth > crate::limits::MAX_TREE_DEPTH {
        return Err(CdrError::DepthLimitExceeded {
            depth,
            limit: crate::limits::MAX_TREE_DEPTH,
        });
    }
    let Some(obj) = doc.objects.get(&oid) else {
        return Ok(None);
    };

    if let NodePayload::Group(children) = &obj.payload {
        let mut grp_nodes = Vec::new();
        grp_nodes.push(RiffNode::chunk(
            *b"loda",
            encode_loda_args(&[(LODA_ARG_OBJ_PROPS, encode_obj_props(obj))]),
        ));
        if obj.transform != Affine2D::IDENTITY {
            grp_nodes.push(RiffNode::chunk(*b"trfd", encode_trfd_chunk(&obj.transform)));
        }
        if let Some(ref lens) = obj.lens {
            grp_nodes.push(RiffNode::chunk(*b"lens", encode_lens_chunk(lens)));
        }
        for &cid in children {
            if let Some(cn) = encode_scene_object(doc, cid, bmp_map, depth + 1)? {
                grp_nodes.push(cn);
            }
        }
        for unk in &obj.unknown_chunks {
            grp_nodes.push(unknown_chunk_to_riff_node(unk, RiffEndian::Little));
        }
        return Ok(Some(RiffNode::list(*b"grp ", grp_nodes)));
    }

    let mut obj_nodes: Vec<RiffNode> = Vec::new();
    let mut loda_args: Vec<(u32, Vec<u8>)> = Vec::new();
    loda_args.push((LODA_ARG_OBJ_PROPS, encode_obj_props(obj)));

    match &obj.payload {
        NodePayload::Path(path) => {
            loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(path)));
        }
        NodePayload::Shape(shape) => {
            encode_shape_loda_arg(shape, &mut loda_args);
        }
        NodePayload::ArtisticText(text) => {
            obj_nodes.push(RiffNode::chunk(*b"txsm", encode_txsm_artistic(text)));
        }
        NodePayload::ParagraphText(para) => {
            let mut tm = Vec::with_capacity(17);
            tm.push(1u8);
            tm.extend_from_slice(&mm_to_cdr32(para.frame_size.x).to_le_bytes());
            tm.extend_from_slice(&mm_to_cdr32(para.frame_size.y).to_le_bytes());
            tm.extend_from_slice(&para.columns.to_le_bytes());
            tm.extend_from_slice(&mm_to_cdr32(para.column_gutter_mm).to_le_bytes());
            loda_args.push((LODA_ARG_TEXT_META, tm));
            obj_nodes.push(RiffNode::chunk(*b"txsm", encode_txsm_paragraph(para)));
        }
        NodePayload::TextOnPath(top) => {
            obj_nodes.push(RiffNode::chunk(*b"txsm", encode_txsm_artistic(&top.text)));
        }
        NodePayload::Raster(r) => {
            let bmp_id = bmp_map.get(&r.asset_id).copied().unwrap_or(0);
            let mut br = Vec::with_capacity(29);
            br.extend_from_slice(&bmp_id.to_le_bytes());
            br.extend_from_slice(&mm_to_cdr32(r.width_mm).to_le_bytes());
            br.extend_from_slice(&mm_to_cdr32(r.height_mm).to_le_bytes());
            if let Some(cr) = r.crop_rect {
                br.push(1);
                br.extend_from_slice(&mm_to_cdr32(cr.min_x).to_le_bytes());
                br.extend_from_slice(&mm_to_cdr32(cr.min_y).to_le_bytes());
                br.extend_from_slice(&mm_to_cdr32(cr.max_x).to_le_bytes());
                br.extend_from_slice(&mm_to_cdr32(cr.max_y).to_le_bytes());
            } else {
                br.push(0);
            }
            loda_args.push((LODA_ARG_BITMAP_REF, br));
        }
        NodePayload::PowerClip {
            container,
            contents,
        } => {
            loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(container)));
            let mut pwr_nodes = Vec::new();
            for &cid in contents {
                if let Some(cn) = encode_scene_object(doc, cid, bmp_map, depth + 1)? {
                    pwr_nodes.push(cn);
                }
            }
            obj_nodes.push(RiffNode::list(*b"pwrp", pwr_nodes));
        }
        NodePayload::Table(t) => {
            loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(&t.to_grid_paths())));
        }
        NodePayload::Dimension(d) => {
            loda_args.push((
                LODA_ARG_CURVE,
                encode_loda_curve_arg(&BezierPath::from_rect(d.bounding_box())),
            ));
        }
        NodePayload::Connector(c) => {
            let (a, b) = doc.resolve_connector_endpoints(c);
            loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(&c.route_path(a, b))));
        }
        NodePayload::SymbolInstance(sym_id) => {
            if let Some(sym) = doc.symbols.get(sym_id) {
                let mut merged = BezierPath::new();
                for sub in &sym.objects {
                    match &sub.payload {
                        NodePayload::Path(p) => merged.subpaths.extend(p.subpaths.clone()),
                        NodePayload::Shape(s) => {
                            merged.subpaths.extend(s.to_bezier_path().subpaths)
                        }
                        _ => {}
                    }
                }
                loda_args.push((LODA_ARG_CURVE, encode_loda_curve_arg(&merged)));
            }
        }
        NodePayload::Group(_) => unreachable!(),
    }

    // Prepend `loda` first inside `LIST obj `
    obj_nodes.insert(0, RiffNode::chunk(*b"loda", encode_loda_args(&loda_args)));

    if obj.transform != Affine2D::IDENTITY {
        obj_nodes.push(RiffNode::chunk(*b"trfd", encode_trfd_chunk(&obj.transform)));
    }
    if obj.fill.is_visible() {
        obj_nodes.push(RiffNode::chunk(*b"fild", encode_fild_chunk(0, &obj.fill)));
    }
    if obj.outline.is_visible() {
        obj_nodes.push(RiffNode::chunk(
            *b"outl",
            encode_outl_chunk(0, &obj.outline),
        ));
    }
    if let Some(ref lens) = obj.lens {
        obj_nodes.push(RiffNode::chunk(*b"lens", encode_lens_chunk(lens)));
    }

    // Re-emit object-level Unknown-Chunk Passthrough verbatim!
    for unk in &obj.unknown_chunks {
        obj_nodes.push(unknown_chunk_to_riff_node(unk, RiffEndian::Little));
    }

    Ok(Some(RiffNode::list(*b"obj ", obj_nodes)))
}

fn unknown_chunk_to_riff_node(unk: &UnknownCdrChunk, endian: RiffEndian) -> RiffNode {
    if &unk.fourcc == b"LIST" && unk.raw_bytes.len() >= 4 {
        if let Ok(list_node) = parse_list_body(&unk.raw_bytes, endian, &CodecLimits::default()) {
            return list_node;
        }
    }
    RiffNode::chunk(unk.fourcc, unk.raw_bytes.clone())
}

fn encode_loda_args(args: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let num_args = args.len() as u32;
    let header_len = 12usize;
    let mut payload_buf = Vec::new();
    let mut offsets: Vec<u32> = Vec::with_capacity(args.len());
    let mut types: Vec<u32> = Vec::with_capacity(args.len());

    for (typ, data) in args {
        offsets.push((header_len + payload_buf.len()) as u32);
        types.push(*typ);
        payload_buf.extend_from_slice(data);
    }

    let args_offset = (header_len + payload_buf.len()) as u32;
    let types_offset = args_offset + num_args * 4;

    let mut out = Vec::with_capacity(header_len + payload_buf.len() + args.len() * 8);
    out.extend_from_slice(&num_args.to_le_bytes());
    out.extend_from_slice(&args_offset.to_le_bytes());
    out.extend_from_slice(&types_offset.to_le_bytes());
    out.extend_from_slice(&payload_buf);
    for off in offsets {
        out.extend_from_slice(&off.to_le_bytes());
    }
    for typ in types {
        out.extend_from_slice(&typ.to_le_bytes());
    }
    out
}

fn encode_obj_props(obj: &SceneObject) -> Vec<u8> {
    let mut flags = 0u8;
    if obj.visible {
        flags |= 0x01;
    }
    if obj.locked {
        flags |= 0x02;
    }
    if obj.printable {
        flags |= 0x04;
    }
    let nb = obj.name.as_bytes();
    let nlen = nb.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(8 + nlen);
    out.push(flags);
    out.push(encode_blend_mode(obj.blend_mode));
    out.extend_from_slice(&sanitize_f32(obj.opacity, 1.0).to_le_bytes());
    out.extend_from_slice(&(nlen as u16).to_le_bytes());
    out.extend_from_slice(&nb[..nlen]);
    out
}

fn encode_loda_curve_arg(path: &BezierPath) -> Vec<u8> {
    let (pts, flags) = encode_curve_points(path);
    let mut out = Vec::with_capacity(4 + pts.len() * 9);
    out.extend_from_slice(&(pts.len() as u32).to_le_bytes());
    for pt in &pts {
        out.extend_from_slice(&mm_to_cdr32(pt.x).to_le_bytes());
        out.extend_from_slice(&mm_to_cdr32(pt.y).to_le_bytes());
    }
    out.extend_from_slice(&flags);
    out
}

fn encode_shape_loda_arg(shape: &ParametricShape, args: &mut Vec<(u32, Vec<u8>)>) {
    match *shape {
        ParametricShape::Rectangle {
            width,
            height,
            corner_radii,
            corner_style,
            lock_corners,
        } => {
            let mut b = Vec::with_capacity(26);
            b.extend_from_slice(&mm_to_cdr32(width).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(height).to_le_bytes());
            for r in corner_radii {
                b.extend_from_slice(&mm_to_cdr32(r).to_le_bytes());
            }
            b.push(match corner_style {
                CornerStyle::Rounded => 0,
                CornerStyle::Scalloped => 1,
                CornerStyle::Chamfered => 2,
            });
            b.push(u8::from(lock_corners));
            args.push((LODA_ARG_RECTANGLE, b));
        }
        ParametricShape::Ellipse {
            rx,
            ry,
            mode,
            start_deg,
            end_deg,
        } => {
            let mut b = Vec::with_capacity(25);
            b.extend_from_slice(&mm_to_cdr32(rx).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(ry).to_le_bytes());
            b.push(match mode {
                EllipseMode::Full => 0,
                EllipseMode::Pie => 1,
                EllipseMode::Arc => 2,
            });
            b.extend_from_slice(&sanitize_f64(start_deg, 0.0).to_le_bytes());
            b.extend_from_slice(&sanitize_f64(end_deg, 360.0).to_le_bytes());
            args.push((LODA_ARG_ELLIPSE, b));
        }
        ParametricShape::Polygon {
            sides,
            radius_x,
            radius_y,
        } => {
            let mut b = Vec::with_capacity(12);
            b.extend_from_slice(&sides.to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(radius_x).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(radius_y).to_le_bytes());
            args.push((LODA_ARG_POLYGON, b));
        }
        ParametricShape::Star {
            points,
            outer_radius,
            inner_ratio,
            complex,
        } => {
            let mut b = Vec::with_capacity(17);
            b.extend_from_slice(&points.to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(outer_radius).to_le_bytes());
            b.extend_from_slice(&sanitize_f64(inner_ratio, 0.5).to_le_bytes());
            b.push(u8::from(complex));
            args.push((LODA_ARG_STAR, b));
        }
        ParametricShape::Spiral {
            revolutions,
            radius,
            logarithmic,
            expansion,
        } => {
            let mut b = Vec::with_capacity(22);
            b.push(1);
            b.extend_from_slice(&sanitize_f64(revolutions, 3.0).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(radius).to_le_bytes());
            b.push(u8::from(logarithmic));
            b.extend_from_slice(&sanitize_f64(expansion, 2.0).to_le_bytes());
            args.push((LODA_ARG_EXTRA_SHAPE, b));
        }
        ParametricShape::GraphPaper {
            cols,
            rows,
            width,
            height,
        } => {
            let mut b = Vec::with_capacity(17);
            b.push(2);
            b.extend_from_slice(&cols.to_le_bytes());
            b.extend_from_slice(&rows.to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(width).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(height).to_le_bytes());
            args.push((LODA_ARG_EXTRA_SHAPE, b));
        }
        ParametricShape::Impact {
            rays,
            inner_radius,
            outer_radius,
            style,
        } => {
            let mut b = Vec::with_capacity(14);
            b.push(3);
            b.extend_from_slice(&rays.to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(inner_radius).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(outer_radius).to_le_bytes());
            b.push(if style == ImpactStyle::Parallel { 1 } else { 0 });
            args.push((LODA_ARG_EXTRA_SHAPE, b));
        }
        ParametricShape::BasicShape {
            kind,
            width,
            height,
        } => {
            let mut b = Vec::with_capacity(10);
            b.push(4);
            b.push(match kind {
                BasicShapeKind::ArrowRight => 0,
                BasicShapeKind::ArrowLeft => 1,
                BasicShapeKind::DoubleArrow => 2,
                BasicShapeKind::Banner => 3,
                BasicShapeKind::Callout => 4,
                BasicShapeKind::Heart => 5,
                BasicShapeKind::Cross => 6,
                BasicShapeKind::Diamond => 7,
                BasicShapeKind::Trapezoid => 8,
            });
            b.extend_from_slice(&mm_to_cdr32(width).to_le_bytes());
            b.extend_from_slice(&mm_to_cdr32(height).to_le_bytes());
            args.push((LODA_ARG_EXTRA_SHAPE, b));
        }
    }
}

fn encode_trfd_chunk(xf: &Affine2D) -> Vec<u8> {
    let mut out = Vec::with_capacity(52);
    out.extend_from_slice(&0x0008u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    let [a, b, c, d, tx, ty] = xf.m;
    out.extend_from_slice(&sanitize_f64(a, 1.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(b, 0.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(c, 0.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(d, 1.0).to_le_bytes());
    out.extend_from_slice(
        &(sanitize_f64(tx, 0.0) * coree_core::math::CDR_UNITS_PER_MM).to_le_bytes(),
    );
    out.extend_from_slice(
        &(sanitize_f64(ty, 0.0) * coree_core::math::CDR_UNITS_PER_MM).to_le_bytes(),
    );
    out
}

fn encode_fild_chunk(fill_id: u32, fill: &Fill) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&fill_id.to_le_bytes());
    match fill {
        Fill::None | Fill::VectorPattern { .. } | Fill::BitmapPattern { .. } => {
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        Fill::Solid(c) => {
            out.extend_from_slice(&1u16.to_le_bytes());
            write_tagged_color(&mut out, c);
        }
        Fill::LinearGradient { start, end, stops } => {
            out.extend_from_slice(&2u16.to_le_bytes());
            encode_fountain_body(&mut out, 0, *start, *end, 0.0, 0.0, stops);
        }
        Fill::RadialGradient {
            center,
            radius,
            focal,
            stops,
        } => {
            out.extend_from_slice(&2u16.to_le_bytes());
            encode_fountain_body(&mut out, 1, *center, *focal, *radius, 0.0, stops);
        }
        Fill::ConicalGradient {
            center,
            angle_deg,
            stops,
        } => {
            out.extend_from_slice(&2u16.to_le_bytes());
            encode_fountain_body(&mut out, 2, *center, Point2D::ZERO, 0.0, *angle_deg, stops);
        }
        Fill::SquareGradient {
            center,
            size,
            angle_deg,
            stops,
        } => {
            out.extend_from_slice(&2u16.to_le_bytes());
            encode_fountain_body(
                &mut out,
                3,
                *center,
                Point2D::ZERO,
                *size,
                *angle_deg,
                stops,
            );
        }
        Fill::TwoColorPattern {
            fg,
            bg,
            pattern_name,
            tile_w,
            tile_h,
        } => {
            out.extend_from_slice(&6u16.to_le_bytes());
            write_tagged_color(&mut out, fg);
            write_tagged_color(&mut out, bg);
            out.extend_from_slice(&mm_to_cdr32(*tile_w).to_le_bytes());
            out.extend_from_slice(&mm_to_cdr32(*tile_h).to_le_bytes());
            let nb = pattern_name.as_bytes();
            let nlen = nb.len().min(u16::MAX as usize);
            out.extend_from_slice(&(nlen as u16).to_le_bytes());
            out.extend_from_slice(&nb[..nlen]);
        }
        Fill::TextureFill {
            preset,
            seed,
            fg,
            bg,
        } => {
            out.extend_from_slice(&7u16.to_le_bytes());
            write_tagged_color(&mut out, fg);
            write_tagged_color(&mut out, bg);
            out.extend_from_slice(&(*seed as u32).to_le_bytes());
            out.extend_from_slice(&((*seed >> 32) as u32).to_le_bytes());
            let nb = preset.as_bytes();
            let nlen = nb.len().min(u16::MAX as usize);
            out.extend_from_slice(&(nlen as u16).to_le_bytes());
            out.extend_from_slice(&nb[..nlen]);
        }
        Fill::MeshFill { rows, cols, nodes } => {
            out.extend_from_slice(&9u16.to_le_bytes());
            out.extend_from_slice(&((*rows).min(u16::MAX as usize) as u16).to_le_bytes());
            out.extend_from_slice(&((*cols).min(u16::MAX as usize) as u16).to_le_bytes());
            let count = nodes.len().min(u16::MAX as usize);
            out.extend_from_slice(&(count as u16).to_le_bytes());
            for n in &nodes[..count] {
                out.extend_from_slice(&((n.row).min(u16::MAX as usize) as u16).to_le_bytes());
                out.extend_from_slice(&((n.col).min(u16::MAX as usize) as u16).to_le_bytes());
                out.extend_from_slice(&mm_to_cdr32(n.pos.x).to_le_bytes());
                out.extend_from_slice(&mm_to_cdr32(n.pos.y).to_le_bytes());
                write_tagged_color(&mut out, &n.color);
            }
        }
    }
    out
}

fn encode_fountain_body(
    out: &mut Vec<u8>,
    subtype: u8,
    p0: Point2D,
    p1: Point2D,
    scalar: f64,
    angle: f64,
    stops: &[GradientStop],
) {
    out.push(subtype);
    out.extend_from_slice(&mm_to_cdr32(p0.x).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(p0.y).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(p1.x).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(p1.y).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(scalar, 0.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(angle, 0.0).to_le_bytes());
    let count = stops.len().min(u16::MAX as usize);
    out.extend_from_slice(&(count as u16).to_le_bytes());
    for s in &stops[..count] {
        out.extend_from_slice(&sanitize_f32(s.offset, 0.0).to_le_bytes());
        write_tagged_color(out, &s.color);
    }
}

fn encode_outl_chunk(outline_id: u32, outline: &OutlineStyle) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&outline_id.to_le_bytes());
    let mut flags = 0u16;
    if !outline.is_visible() {
        flags |= 0x0001;
        out.extend_from_slice(&flags.to_le_bytes());
        return out;
    }
    if outline.is_hairline {
        flags |= 0x0002;
    }
    if outline.behind_fill {
        flags |= 0x0004;
    }
    if outline.scale_with_object {
        flags |= 0x0008;
    }
    if outline.overprint {
        flags |= 0x0010;
    }
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(outline.width_mm).to_le_bytes());
    out.push(match outline.cap {
        LineCap::Butt => 0,
        LineCap::Round => 1,
        LineCap::Square => 2,
    });
    out.push(match outline.join {
        LineJoin::Miter => 0,
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
    });
    out.extend_from_slice(&sanitize_f64(outline.miter_limit, 4.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(outline.calligraphy_angle_deg, 0.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(outline.calligraphy_stretch, 1.0).to_le_bytes());
    out.push(encode_arrow(&outline.start_arrow));
    out.push(encode_arrow(&outline.end_arrow));
    let dcount = outline.dash_pattern.len().min(256);
    out.extend_from_slice(&(dcount as u16).to_le_bytes());
    for &d in &outline.dash_pattern[..dcount] {
        out.extend_from_slice(&mm_to_cdr32(d).to_le_bytes());
    }
    write_tagged_color(&mut out, &outline.color);
    out
}

pub(crate) fn encode_stlt_chunk(
    style_id: u32,
    style: &TextStyle,
    codepage: u16,
    charset: u8,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&style_id.to_le_bytes());
    encode_stlt_body(&mut out, style, codepage, charset);
    out
}

fn encode_stlt_body(out: &mut Vec<u8>, style: &TextStyle, codepage: u16, charset: u8) {
    out.extend_from_slice(&codepage.to_le_bytes());
    out.push(charset);
    let mut flags = 0u8;
    if style.bold {
        flags |= 0x01;
    }
    if style.italic {
        flags |= 0x02;
    }
    if style.underline {
        flags |= 0x04;
    }
    if style.strikethrough {
        flags |= 0x08;
    }
    out.push(flags);
    out.push(encode_text_direction(style.direction));
    out.push(encode_text_align(style.align));
    out.extend_from_slice(&sanitize_f64(style.font_size_pt, 12.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(style.letter_spacing_mm, 0.0).to_le_bytes());
    out.extend_from_slice(&sanitize_f64(style.line_height_ratio, 1.2).to_le_bytes());
    out.extend_from_slice(&sanitize_f32(style.kashida_stretch, 0.0).to_le_bytes());
    let fb = style.font_family.as_bytes();
    let flen = fb.len().min(u16::MAX as usize);
    out.extend_from_slice(&(flen as u16).to_le_bytes());
    out.extend_from_slice(&fb[..flen]);
    write_tagged_color(out, &style.fill);
}

fn encode_txsm_artistic(text: &ArtisticText) -> Vec<u8> {
    encode_txsm_runs(
        false,
        text.direction,
        text.align,
        Vec2D::ZERO,
        1,
        5.0,
        &text.runs,
    )
}

fn encode_txsm_paragraph(para: &ParagraphText) -> Vec<u8> {
    encode_txsm_runs(
        true,
        para.direction,
        para.align,
        para.frame_size,
        para.columns,
        para.column_gutter_mm,
        &para.runs,
    )
}

fn encode_txsm_runs(
    is_para: bool,
    direction: TextDirection,
    align: TextAlign,
    frame_size: Vec2D,
    columns: u32,
    gutter_mm: f64,
    runs: &[TextRun],
) -> Vec<u8> {
    let mut out = vec![
        0x02, // multi-run rich text stream
        u8::from(is_para),
        encode_text_direction(direction),
        encode_text_align(align),
    ];
    out.extend_from_slice(&mm_to_cdr32(frame_size.x).to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(frame_size.y).to_le_bytes());
    out.extend_from_slice(&columns.to_le_bytes());
    out.extend_from_slice(&mm_to_cdr32(gutter_mm).to_le_bytes());
    let count = runs.len().min(u16::MAX as usize);
    out.extend_from_slice(&(count as u16).to_le_bytes());
    for run in &runs[..count] {
        out.extend_from_slice(&CODEPAGE_UTF8.to_le_bytes());
        out.push(0); // charset
        out.extend_from_slice(&0u32.to_le_bytes()); // style_id
        out.push(1); // inline style present
        encode_stlt_body(&mut out, &run.style, CODEPAGE_UTF8, 0);
        let tb = run.text.as_bytes();
        out.extend_from_slice(&(tb.len() as u32).to_le_bytes());
        out.extend_from_slice(tb);
    }
    out
}

/// Helper used by fixtures to construct an authentic legacy Windows-1256 (CP1256) `txsm` chunk.
#[must_use]
pub(crate) fn encode_legacy_cp1256_txsm(
    style_id: u32,
    cp1256_bytes: &[u8],
    explicit_codepage: bool,
    charset: u8,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + cp1256_bytes.len());
    out.push(0x01); // legacy single-run format
    out.extend_from_slice(&style_id.to_le_bytes());
    let cp = if explicit_codepage {
        CODEPAGE_ARABIC_CP1256
    } else {
        0
    };
    out.extend_from_slice(&cp.to_le_bytes());
    out.push(charset);
    out.push(0); // artistic text
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&(cp1256_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(cp1256_bytes);
    out
}

fn encode_lens_chunk(lens: &LensConfig) -> Vec<u8> {
    let mut out = Vec::new();
    let kind_id = match &lens.kind {
        LensKind::Brighten { .. } => 1u8,
        LensKind::ColorAdd { .. } => 2,
        LensKind::ColorLimit { .. } => 3,
        LensKind::CustomColorMap { .. } => 4,
        LensKind::FishEye { .. } => 5,
        LensKind::HeatMap { .. } => 6,
        LensKind::Invert => 7,
        LensKind::Magnify { .. } => 8,
        LensKind::TintedGrayscale { .. } => 9,
        LensKind::Transparency { .. } => 10,
        LensKind::Wireframe { .. } => 11,
    };
    out.push(kind_id);
    let mut flags = 0u8;
    if lens.frozen {
        flags |= 0x01;
    }
    if lens.remove_face {
        flags |= 0x02;
    }
    if lens.viewpoint.is_some() {
        flags |= 0x04;
    }
    out.push(flags);
    if let Some(vp) = lens.viewpoint {
        out.extend_from_slice(&mm_to_cdr32(vp.x).to_le_bytes());
        out.extend_from_slice(&mm_to_cdr32(vp.y).to_le_bytes());
    }
    match &lens.kind {
        LensKind::Brighten { rate_pct } => {
            out.extend_from_slice(&sanitize_f32(*rate_pct, 0.0).to_le_bytes());
        }
        LensKind::ColorAdd { color, rate_pct } => {
            out.extend_from_slice(&sanitize_f32(*rate_pct, 50.0).to_le_bytes());
            write_tagged_color(&mut out, color);
        }
        LensKind::ColorLimit { color, rate_pct } => {
            out.extend_from_slice(&sanitize_f32(*rate_pct, 50.0).to_le_bytes());
            write_tagged_color(&mut out, color);
        }
        LensKind::CustomColorMap {
            from_color,
            to_color,
            palette_mode,
        } => {
            out.push(match palette_mode {
                ColorMapMode::Direct => 0,
                ColorMapMode::ForwardRainbow => 1,
                ColorMapMode::ReverseRainbow => 2,
            });
            write_tagged_color(&mut out, from_color);
            write_tagged_color(&mut out, to_color);
        }
        LensKind::FishEye { rate_pct } => {
            out.extend_from_slice(&sanitize_f32(*rate_pct, 100.0).to_le_bytes());
        }
        LensKind::HeatMap {
            palette_rotation_deg,
        } => {
            out.extend_from_slice(&sanitize_f32(*palette_rotation_deg, 0.0).to_le_bytes());
        }
        LensKind::Invert => {}
        LensKind::Magnify { amount } => {
            out.extend_from_slice(&sanitize_f32(*amount, 2.0).to_le_bytes());
        }
        LensKind::TintedGrayscale { tint } => {
            write_tagged_color(&mut out, tint);
        }
        LensKind::Transparency { color, rate_pct } => {
            out.extend_from_slice(&sanitize_f32(*rate_pct, 50.0).to_le_bytes());
            write_tagged_color(&mut out, color);
        }
        LensKind::Wireframe {
            override_outline,
            override_fill,
        } => {
            if let Some(c) = override_outline {
                out.push(1);
                write_tagged_color(&mut out, c);
            } else {
                out.push(0);
            }
            if let Some(c) = override_fill {
                out.push(1);
                write_tagged_color(&mut out, c);
            } else {
                out.push(0);
            }
        }
    }
    out
}

pub(crate) fn encode_blend_mode(m: BlendMode) -> u8 {
    match m {
        BlendMode::Normal => 0,
        BlendMode::Multiply => 1,
        BlendMode::Screen => 2,
        BlendMode::Overlay => 3,
        BlendMode::Darken => 4,
        BlendMode::Lighten => 5,
        BlendMode::Difference => 6,
        BlendMode::Exclusion => 7,
        BlendMode::SoftLight => 8,
        BlendMode::HardLight => 9,
        BlendMode::ColorDodge => 10,
        BlendMode::ColorBurn => 11,
        BlendMode::Hue => 12,
        BlendMode::Saturation => 13,
        BlendMode::Color => 14,
        BlendMode::Luminosity => 15,
    }
}

pub(crate) fn decode_blend_mode(v: u8) -> BlendMode {
    match v {
        1 => BlendMode::Multiply,
        2 => BlendMode::Screen,
        3 => BlendMode::Overlay,
        4 => BlendMode::Darken,
        5 => BlendMode::Lighten,
        6 => BlendMode::Difference,
        7 => BlendMode::Exclusion,
        8 => BlendMode::SoftLight,
        9 => BlendMode::HardLight,
        10 => BlendMode::ColorDodge,
        11 => BlendMode::ColorBurn,
        12 => BlendMode::Hue,
        13 => BlendMode::Saturation,
        14 => BlendMode::Color,
        15 => BlendMode::Luminosity,
        _ => BlendMode::Normal,
    }
}

fn encode_arrow(a: &ArrowheadStyle) -> u8 {
    match a {
        ArrowheadStyle::None => 0,
        ArrowheadStyle::Triangle => 1,
        ArrowheadStyle::OpenVee => 2,
        ArrowheadStyle::Diamond => 3,
        ArrowheadStyle::Circle => 4,
        ArrowheadStyle::Square => 5,
        ArrowheadStyle::Stealth => 6,
        ArrowheadStyle::Custom(_) => 1,
    }
}

fn decode_arrow(v: u8) -> ArrowheadStyle {
    match v {
        1 => ArrowheadStyle::Triangle,
        2 => ArrowheadStyle::OpenVee,
        3 => ArrowheadStyle::Diamond,
        4 => ArrowheadStyle::Circle,
        5 => ArrowheadStyle::Square,
        6 => ArrowheadStyle::Stealth,
        _ => ArrowheadStyle::None,
    }
}

fn encode_text_direction(d: TextDirection) -> u8 {
    match d {
        TextDirection::Auto => 0,
        TextDirection::Ltr => 1,
        TextDirection::Rtl => 2,
    }
}

fn decode_text_direction(v: u8) -> TextDirection {
    match v {
        1 => TextDirection::Ltr,
        2 => TextDirection::Rtl,
        _ => TextDirection::Auto,
    }
}

fn encode_text_align(a: TextAlign) -> u8 {
    match a {
        TextAlign::Left => 0,
        TextAlign::Center => 1,
        TextAlign::Right => 2,
        TextAlign::Justify => 3,
        TextAlign::ForceJustifyKashida => 4,
    }
}

fn decode_text_align(v: u8) -> TextAlign {
    match v {
        1 => TextAlign::Center,
        2 => TextAlign::Right,
        3 => TextAlign::Justify,
        4 => TextAlign::ForceJustifyKashida,
        _ => TextAlign::Left,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_zcf_gradient_rescales_onto_object_local_millimeter_bounds() {
        let mut fill = Fill::LinearGradient {
            start: Point2D::new(0.0, 0.5),
            end: Point2D::new(1.0, 0.5),
            stops: vec![
                GradientStop::new(0.0, coree_core::Color::rgb(1.0, 0.0, 0.0)),
                GradientStop::new(1.0, coree_core::Color::rgb(0.0, 0.0, 1.0)),
            ],
        };
        let bb = Rect2D::from_xywh(10.0, 20.0, 120.0, 60.0);
        rescale_normalized_fill_to_bounds(&mut fill, bb);
        match fill {
            Fill::LinearGradient { start, end, .. } => {
                assert!((start.x - 10.0).abs() < 1e-6);
                assert!((start.y - 50.0).abs() < 1e-6);
                assert!((end.x - 130.0).abs() < 1e-6);
                assert!((end.y - 50.0).abs() < 1e-6);
            }
            _ => panic!("expected LinearGradient"),
        }
    }

    #[test]
    fn parse_real_fild_v16_and_outl_v16_handle_subtypes_and_truncated_inputs_safely() {
        // Real v16 Type 2 Radial gradient with two embedded RGB stops
        let mut chunk = vec![0u8; 64];
        chunk[0..4].copy_from_slice(&7u32.to_le_bytes()); // fill_id
        chunk[4..6].copy_from_slice(&0x0514u16.to_le_bytes());
        chunk[12..14].copy_from_slice(&2u16.to_le_bytes()); // Fountain fill
        chunk[16..18].copy_from_slice(&1u16.to_le_bytes()); // Radial subtype
                                                            // Stop 1 @ offset 20 (`01 0c 00 00 00` + 12-byte RGB)
        chunk[20..25].copy_from_slice(&[0x01, 0x0C, 0, 0, 0]);
        chunk[25..27].copy_from_slice(&5u16.to_le_bytes()); // RGB model
        chunk[33..37].copy_from_slice(&[255, 0, 0, 255]);
        // Stop 2 @ offset 40
        chunk[40..45].copy_from_slice(&[0x01, 0x0C, 0, 0, 0]);
        chunk[45..47].copy_from_slice(&5u16.to_le_bytes()); // RGB model
        chunk[53..57].copy_from_slice(&[0, 0, 255, 255]);

        let (id, fill, is_norm) = parse_real_fild_v16(&chunk).unwrap();
        assert_eq!(id, 7);
        assert!(is_norm);
        assert!(matches!(fill, Fill::RadialGradient { .. }));

        // Truncated v16 outl must error cleanly without out-of-bounds panic
        let short_outl = vec![0u8; 16];
        assert!(parse_real_outl_v16(&short_outl).is_err());
    }
}
