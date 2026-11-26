// SPDX-License-Identifier: MIT OR Apache-2.0

//! Clean-room `.wi` wavelet-compressed bitmap codec (`IMP-06` / `REQ-06`).
//!
//! Uses a lossless reversible 2D Haar / integer lifting wavelet transform per
//! RGBA channel with zlib-compressed subbands, enforcing [`CodecLimits`]
//! dimension and memory budget checks BEFORE any pixel buffer allocation.

use crate::error::{CdrError, CdrResult};
use crate::limits::{decompress_zlib_bounded, ByteReader, CodecLimits};
use flate2::{write::ZlibEncoder, Compression};
use std::io::Write;

/// Clean-room `.wi` wavelet bitmap signature (`"WI1\0"` or wavelet `"WIFF"` magic).
const COREE_WI_MAGIC: &[u8; 4] = b"WI1\0";

/// Returns `true` if `raw` begins with a recognized `.wi` wavelet bitmap header.
#[must_use]
pub fn is_wi_bitmap(raw: &[u8]) -> bool {
    raw.len() >= 16 && (&raw[0..4] == COREE_WI_MAGIC || &raw[0..4] == b"WIFF")
}

/// Encodes an RGBA8 image using a reversible 1-level 2D Haar integer lifting wavelet
/// transform (`LL`, `LH`, `HL`, `HH`) per channel and zlib-compresses the subband stream.
pub fn encode_wi_rgba(width: u32, height: u32, rgba: &[u8]) -> CdrResult<Vec<u8>> {
    let w = width as usize;
    let h = height as usize;
    let expected = w
        .checked_mul(h)
        .and_then(|n| n.checked_mul(4))
        .ok_or(CdrError::ArithmeticOverflow("WI encode size"))?;
    if width == 0 || height == 0 || rgba.len() != expected {
        return Err(CdrError::InvalidChunk {
            fourcc: "wi  ".to_owned(),
            reason: format!(
                "invalid RGBA buffer len {} for {}x{}",
                rgba.len(),
                width,
                height
            ),
        });
    }

    let pw = w + (w & 1);
    let ph = h + (h & 1);
    let hw = pw / 2;
    let hh = ph / 2;

    let mut subbands = Vec::with_capacity(pw * ph * 4 * 2);
    for ch in 0..4 {
        let mut grid = vec![0i16; pw * ph];
        for y in 0..h {
            for x in 0..w {
                grid[y * pw + x] = i16::from(rgba[(y * w + x) * 4 + ch]);
            }
            if pw > w {
                grid[y * pw + w] = grid[y * pw + (w - 1)];
            }
        }
        if ph > h {
            for x in 0..pw {
                grid[h * pw + x] = grid[(h - 1) * pw + x];
            }
        }

        // Horizontal 1D lifting step
        let mut horiz = vec![0i16; pw * ph];
        for y in 0..ph {
            let row = &grid[y * pw..(y + 1) * pw];
            for x in 0..hw {
                let even = row[x * 2];
                let odd = row[x * 2 + 1];
                let d = odd - even;
                let s = even + (d >> 1);
                horiz[y * pw + x] = s;
                horiz[y * pw + hw + x] = d;
            }
        }

        // Vertical 1D lifting step -> LL, LH, HL, HH subbands (i16 LE)
        let mut ll = Vec::with_capacity(hw * hh);
        let mut lh = Vec::with_capacity(hw * hh);
        let mut hl = Vec::with_capacity(hw * hh);
        let mut hh_band = Vec::with_capacity(hw * hh);
        for y in 0..hh {
            let r0 = &horiz[(y * 2) * pw..(y * 2 + 1) * pw];
            let r1 = &horiz[(y * 2 + 1) * pw..(y * 2 + 2) * pw];
            for x in 0..hw {
                let d_l = r1[x] - r0[x];
                let s_l = r0[x] + (d_l >> 1);
                ll.push(s_l);
                lh.push(d_l);

                let d_h = r1[hw + x] - r0[hw + x];
                let s_h = r0[hw + x] + (d_h >> 1);
                hl.push(s_h);
                hh_band.push(d_h);
            }
        }

        for band in [&ll, &lh, &hl, &hh_band] {
            for &v in band {
                subbands.extend_from_slice(&v.to_le_bytes());
            }
        }
    }

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&subbands)
        .map_err(|e| CdrError::DecompressionFailed(format!("WI zlib write failed: {e}")))?;
    let compressed = enc
        .finish()
        .map_err(|e| CdrError::DecompressionFailed(format!("WI zlib finish failed: {e}")))?;

    let mut out = Vec::with_capacity(16 + compressed.len());
    out.extend_from_slice(COREE_WI_MAGIC);
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // levels = 1
    out.extend_from_slice(&4u16.to_le_bytes()); // channels = 4 (RGBA)
    out.extend_from_slice(&compressed);
    Ok(out)
}

/// Decodes a `.wi` wavelet bitmap into `(width, height, rgba_bytes)` while strictly
/// enforcing `limits.validate_bitmap_dims` and `budget_remaining` BEFORE allocating.
pub fn decode_wi_to_rgba_with_budget(
    raw: &[u8],
    limits: &CodecLimits,
    budget_remaining: &mut u64,
) -> CdrResult<(u32, u32, Vec<u8>)> {
    let mut r = ByteReader::new(raw);
    let magic = r.read_fourcc("WI magic")?;
    if &magic != COREE_WI_MAGIC && &magic != b"WIFF" {
        return Err(CdrError::InvalidMagic {
            expected: "WI1\\0 or WIFF".to_owned(),
            found: String::from_utf8_lossy(&magic).into_owned(),
        });
    }
    let width = r.read_u32_le("WI width")?;
    let height = r.read_u32_le("WI height")?;
    let _levels = r.read_u16_le("WI levels")?;
    let channels = r.read_u16_le("WI channels")?;
    if !matches!(channels, 1 | 3 | 4) {
        return Err(CdrError::InvalidChunk {
            fourcc: "wi  ".to_owned(),
            reason: format!("unsupported WI channel count {channels}"),
        });
    }

    let rgba_len = limits.validate_bitmap_dims(width, height)?;
    let w = width as usize;
    let h = height as usize;
    let pw = w + (w & 1);
    let ph = h + (h & 1);
    let hw = pw / 2;
    let hh = ph / 2;
    let band_elems = hw
        .checked_mul(hh)
        .ok_or(CdrError::ArithmeticOverflow("WI subband elems"))?;
    let expected_subband_bytes = band_elems
        .checked_mul(4)
        .and_then(|n| n.checked_mul(usize::from(channels)))
        .and_then(|n| n.checked_mul(2))
        .ok_or(CdrError::ArithmeticOverflow("WI subband bytes"))?;
    let total_needed = (expected_subband_bytes as u64)
        .checked_add(rgba_len as u64)
        .ok_or(CdrError::ArithmeticOverflow("WI total budget"))?;
    if total_needed > *budget_remaining {
        return Err(CdrError::UncompressedSizeLimitExceeded {
            limit: limits.max_uncompressed_bytes,
        });
    }

    let decompressed = decompress_zlib_bounded(r.read_rest(), limits, budget_remaining)?;
    if decompressed.len() != expected_subband_bytes {
        return Err(CdrError::InvalidChunk {
            fourcc: "wi  ".to_owned(),
            reason: format!(
                "WI subband length {} != expected {expected_subband_bytes}",
                decompressed.len()
            ),
        });
    }

    limits.consume_budget(budget_remaining, rgba_len as u64)?;
    let mut rgba = vec![255u8; rgba_len];
    let ch_stride = band_elems * 4 * 2;

    for ch in 0..usize::from(channels) {
        let ch_bytes = &decompressed[ch * ch_stride..(ch + 1) * ch_stride];
        let band_bytes = band_elems * 2;
        let read_band = |idx: usize| -> Vec<i16> {
            let sl = &ch_bytes[idx * band_bytes..(idx + 1) * band_bytes];
            sl.as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect()
        };
        let ll = read_band(0);
        let lh = read_band(1);
        let hl = read_band(2);
        let hh_band = read_band(3);

        // Inverse vertical lifting
        let mut horiz = vec![0i16; pw * ph];
        for y in 0..hh {
            for x in 0..hw {
                let s_l = ll[y * hw + x];
                let d_l = lh[y * hw + x];
                let r0_l = s_l - (d_l >> 1);
                let r1_l = r0_l + d_l;
                horiz[(y * 2) * pw + x] = r0_l;
                horiz[(y * 2 + 1) * pw + x] = r1_l;

                let s_h = hl[y * hw + x];
                let d_h = hh_band[y * hw + x];
                let r0_h = s_h - (d_h >> 1);
                let r1_h = r0_h + d_h;
                horiz[(y * 2) * pw + hw + x] = r0_h;
                horiz[(y * 2 + 1) * pw + hw + x] = r1_h;
            }
        }

        // Inverse horizontal lifting
        for y in 0..h {
            let row = &horiz[y * pw..(y + 1) * pw];
            for x in 0..hw {
                let s = row[x];
                let d = row[hw + x];
                let even = s - (d >> 1);
                let odd = even + d;
                let x0 = x * 2;
                let x1 = x0 + 1;
                if x0 < w {
                    let v = even.clamp(0, 255) as u8;
                    if channels == 1 {
                        let base = (y * w + x0) * 4;
                        rgba[base] = v;
                        rgba[base + 1] = v;
                        rgba[base + 2] = v;
                    } else {
                        rgba[(y * w + x0) * 4 + ch] = v;
                    }
                }
                if x1 < w {
                    let v = odd.clamp(0, 255) as u8;
                    if channels == 1 {
                        let base = (y * w + x1) * 4;
                        rgba[base] = v;
                        rgba[base + 1] = v;
                        rgba[base + 2] = v;
                    } else {
                        rgba[(y * w + x1) * 4 + ch] = v;
                    }
                }
            }
        }
    }

    Ok((width, height, rgba))
}
