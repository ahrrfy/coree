// SPDX-License-Identifier: MIT OR Apache-2.0

//! Windows DIB/BMP, `LIST bmpt` alpha mask extraction, and `.wi` wavelet dispatch.

use super::wavelet_wi::{decode_wi_to_rgba_with_budget, is_wi_bitmap};
use crate::error::{CdrError, CdrResult};
use crate::limits::{ByteReader, CodecLimits};

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

/// Decodes a Windows DIB (or full `"BM"` BMP file, or `.wi` wavelet payload) into
/// `(width, height, rgba_bytes)` enforcing `limits` BEFORE allocating.
pub fn decode_dib_to_rgba(raw: &[u8], limits: &CodecLimits) -> CdrResult<(u32, u32, Vec<u8>)> {
    let mut budget = limits.max_uncompressed_bytes;
    decode_dib_to_rgba_with_budget(raw, limits, &mut budget)
}

/// Decodes a Windows DIB/BMP (or `.wi` wavelet bitmap) while deducting the resulting
/// RGBA allocation from `budget_remaining` BEFORE allocating pixel memory.
pub fn decode_dib_to_rgba_with_budget(
    raw: &[u8],
    limits: &CodecLimits,
    budget_remaining: &mut u64,
) -> CdrResult<(u32, u32, Vec<u8>)> {
    if is_wi_bitmap(raw) {
        return decode_wi_to_rgba_with_budget(raw, limits, budget_remaining);
    }

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

    if !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32) {
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
            if bpp != 32 && bpp != 16 {
                return Err(CdrError::InvalidChunk {
                    fourcc: "bmp ".to_owned(),
                    reason: format!(
                        "BI_BITFIELDS compression requires 16/32-bpp DIB, found {bpp}-bpp"
                    ),
                });
            }
            let masks = if header_size >= 56 {
                r.seek(40, "DIB V4 bitfields")?;
                let rm = r.read_u32_le("DIB red mask")?;
                let gm = r.read_u32_le("DIB green mask")?;
                let bm = r.read_u32_le("DIB blue mask")?;
                let am = r.read_u32_le("DIB alpha mask")?;
                r.seek(header_size, "DIB V4 post-header")?;
                (rm, gm, bm, am)
            } else {
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

    // Optional color palette for 1-bit / 4-bit / 8-bit indexed images
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
            16 => {
                let (rm, gm, bm, am) = bitfields.unwrap_or((0x7C00, 0x03E0, 0x001F, 0));
                for x in 0..w_usize {
                    let s = src_base + x * 2;
                    let d = dst_base + x * 4;
                    let px = u32::from(u16::from_le_bytes([pixels[s], pixels[s + 1]]));
                    rgba[d] = extract_bitfield_channel(px, rm);
                    rgba[d + 1] = extract_bitfield_channel(px, gm);
                    rgba[d + 2] = extract_bitfield_channel(px, bm);
                    rgba[d + 3] = if am != 0 {
                        extract_bitfield_channel(px, am)
                    } else {
                        255
                    };
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
            4 => {
                for x in 0..w_usize {
                    let byte = pixels[src_base + (x / 2)];
                    let idx = usize::from(if (x & 1) == 0 { byte >> 4 } else { byte & 0x0F });
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

/// Decodes an alpha-mask payload from a `LIST bmpt` chunk (`DIB` / `BM` / raw 8-bit luminance
/// / `.wi` wavelet) into a `width * height` alpha byte plane.
pub(crate) fn decode_bmpt_alpha_mask(
    mask_data: &[u8],
    expected_w: u32,
    expected_h: u32,
    limits: &CodecLimits,
    budget_remaining: &mut u64,
) -> CdrResult<Vec<u8>> {
    let pixel_count = (expected_w as usize)
        .checked_mul(expected_h as usize)
        .ok_or(CdrError::ArithmeticOverflow("bmpt alpha pixel count"))?;
    if mask_data.len() == pixel_count {
        limits.consume_budget(budget_remaining, pixel_count as u64)?;
        return Ok(mask_data.to_vec());
    }
    if (mask_data.len() >= 40
        && u32::from_le_bytes([mask_data[0], mask_data[1], mask_data[2], mask_data[3]]) >= 40)
        || (mask_data.len() >= 14 && &mask_data[0..2] == b"BM")
        || is_wi_bitmap(mask_data)
    {
        let (mw, mh, mrgba) = decode_dib_to_rgba_with_budget(mask_data, limits, budget_remaining)?;
        if mw == expected_w && mh == expected_h {
            let alpha: Vec<u8> = mrgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|px| {
                    // Use luminance if opaque grayscale DIB, or alpha if non-255 alpha exists
                    ((u16::from(px[0]) + u16::from(px[1]) + u16::from(px[2])) / 3) as u8
                })
                .collect();
            return Ok(alpha);
        }
    }
    Err(CdrError::InvalidChunk {
        fourcc: "bmpt".to_owned(),
        reason: format!(
            "alpha mask len {} does not match {}x{} raster",
            mask_data.len(),
            expected_w,
            expected_h
        ),
    })
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
