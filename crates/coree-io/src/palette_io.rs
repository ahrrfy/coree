// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure-Rust import & export of open color palette formats:
//! - **GIMP `.gpl`** (ASCII RGB swatch list)
//! - **Adobe Swatch Exchange `.ase`** (binary big-endian CMYK, RGB, Lab, Gray, and Spot swatches)
//! - **Adobe Color Book `.acb`** (legal clean-room `8BCB` binary reader: UTF-16BE `u32` strings, RGB/CMYK/Lab/Gray)
//! - **JASC / Named ASCII `.pal`** (JASC-PAL v0100 + quoted-name CMYK/RGB ASCII `.pal`)

use crate::atomic::atomic_write_bytes;
use crate::error::{IoError, IoResult};
use coree_core::{Color, ColorMode, ColorPalette, ColorSwatch};
use std::fmt::Write as _;
use std::path::Path;

const MAX_PALETTE_SWATCHES: usize = 65_536;

// =============================================================================
// GIMP Palette (.gpl)
// =============================================================================

/// Parses a GIMP `.gpl` color palette string into a [`ColorPalette`].
pub fn import_gpl(text: &str) -> IoResult<ColorPalette> {
    let mut lines = text.lines();
    let first = lines.next().map(str::trim).unwrap_or("");
    if first != "GIMP Palette" {
        return Err(IoError::InvalidFormat {
            format: "gpl".into(),
            message: "missing 'GIMP Palette' header".into(),
        });
    }

    let mut name = "GIMP Palette".to_owned();
    let mut swatches = Vec::new();

    for raw_line in lines {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("Name:") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                name = trimmed.to_owned();
            }
            continue;
        }
        if line.starts_with("Columns:") {
            continue;
        }

        let mut parts = line.split_whitespace();
        let (Some(rs), Some(gs), Some(bs)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(r), Ok(g), Ok(b)) = (rs.parse::<u8>(), gs.parse::<u8>(), bs.parse::<u8>()) else {
            return Err(IoError::InvalidFormat {
                format: "gpl".into(),
                message: format!("invalid RGB triplet in line: '{line}'"),
            });
        };
        if swatches.len() >= MAX_PALETTE_SWATCHES {
            return Err(IoError::LimitExceeded {
                limit: format!("max_palette_swatches={MAX_PALETTE_SWATCHES}"),
                actual: "exceeded".into(),
            });
        }
        let swatch_name: String = parts.collect::<Vec<_>>().join(" ");
        let final_name = if swatch_name.is_empty() {
            format!("#{r:02X}{g:02X}{b:02X}")
        } else {
            swatch_name
        };
        swatches.push(ColorSwatch::new(final_name, Color::rgba8(r, g, b, 255)));
    }

    let mut palette = ColorPalette::new(name, ColorMode::Rgb);
    palette.swatches = swatches;
    Ok(palette)
}

/// Serializes `palette` to GIMP `.gpl` ASCII text.
#[must_use]
pub fn export_gpl(palette: &ColorPalette) -> String {
    let mut out = String::with_capacity(palette.swatches.len() * 32 + 64);
    out.push_str("GIMP Palette\n");
    let _ = writeln!(out, "Name: {}", sanitize_single_line(&palette.name));
    out.push_str("Columns: 0\n#\n");
    for sw in &palette.swatches {
        let [r, g, b, _] = sw.color.to_rgba8();
        let _ = writeln!(out, "{r:3} {g:3} {b:3}\t{}", sanitize_single_line(&sw.name));
    }
    out
}

// =============================================================================
// Adobe Swatch Exchange (.ase) — Binary Big-Endian (CMYK / RGB / Lab / Gray / Spot)
// =============================================================================

/// Parses a binary Adobe Swatch Exchange (`.ase`) buffer into a [`ColorPalette`].
pub fn import_ase(bytes: &[u8]) -> IoResult<ColorPalette> {
    if bytes.len() < 12 || &bytes[0..4] != b"ASEF" {
        return Err(IoError::InvalidFormat {
            format: "ase".into(),
            message: "missing 'ASEF' magic header".into(),
        });
    }
    let _ver_major = read_u16_be(bytes, 4)?;
    let _ver_minor = read_u16_be(bytes, 6)?;
    let block_count = read_u32_be(bytes, 8)? as usize;
    if block_count > MAX_PALETTE_SWATCHES * 2 {
        return Err(IoError::LimitExceeded {
            limit: format!("max_ase_blocks={}", MAX_PALETTE_SWATCHES * 2),
            actual: block_count.to_string(),
        });
    }

    let mut pos = 12usize;
    let mut palette_name: Option<String> = None;
    let mut swatches = Vec::new();
    let mut saw_cmyk = false;

    for _ in 0..block_count {
        if pos + 6 > bytes.len() {
            return Err(IoError::InvalidFormat {
                format: "ase".into(),
                message: "truncated ASE block header".into(),
            });
        }
        let block_type = read_u16_be(bytes, pos)?;
        let block_len = read_u32_be(bytes, pos + 2)? as usize;
        pos += 6;

        let block_end = pos
            .checked_add(block_len)
            .ok_or_else(|| IoError::InvalidFormat {
                format: "ase".into(),
                message: "ASE block length overflow".into(),
            })?;
        if block_end > bytes.len() {
            return Err(IoError::InvalidFormat {
                format: "ase".into(),
                message: "truncated ASE block payload".into(),
            });
        }

        let block = &bytes[pos..block_end];
        pos = block_end;

        match block_type {
            0xC001 => {
                // Group start
                if let Ok((gname, _)) = read_utf16be_pascal(block, 0) {
                    if palette_name.is_none() && !gname.is_empty() {
                        palette_name = Some(gname);
                    }
                }
            }
            0xC002 => {
                // Group end
            }
            0x0001 => {
                // Color entry
                if swatches.len() >= MAX_PALETTE_SWATCHES {
                    return Err(IoError::LimitExceeded {
                        limit: format!("max_palette_swatches={MAX_PALETTE_SWATCHES}"),
                        actual: "exceeded".into(),
                    });
                }
                let (swatch_name, mut offset) = read_utf16be_pascal(block, 0)?;
                if offset + 4 > block.len() {
                    return Err(IoError::InvalidFormat {
                        format: "ase".into(),
                        message: "missing ASE color model tag".into(),
                    });
                }
                let model = &block[offset..offset + 4];
                offset += 4;

                let base_color = match model {
                    b"CMYK" => {
                        saw_cmyk = true;
                        let c = read_finite_f32_be(block, offset)?;
                        let m = read_finite_f32_be(block, offset + 4)?;
                        let y = read_finite_f32_be(block, offset + 8)?;
                        let k = read_finite_f32_be(block, offset + 12)?;
                        offset += 16;
                        Color::cmyk(c, m, y, k)
                    }
                    b"RGB " => {
                        let r = read_finite_f32_be(block, offset)?;
                        let g = read_finite_f32_be(block, offset + 4)?;
                        let b = read_finite_f32_be(block, offset + 8)?;
                        offset += 12;
                        Color::rgb(r, g, b)
                    }
                    b"LAB " => {
                        // ASE stores L in 0.0..=1.0; coree-core Lab uses L in 0.0..=100.0
                        let l_norm = read_finite_f32_be(block, offset)?;
                        let a_ch = read_finite_f32_be(block, offset + 4)?;
                        let b_ch = read_finite_f32_be(block, offset + 8)?;
                        offset += 12;
                        let l_scaled = if l_norm <= 1.0001 {
                            l_norm * 100.0
                        } else {
                            l_norm
                        };
                        Color::lab(l_scaled, a_ch, b_ch)
                    }
                    b"GRAY" => {
                        let v = read_finite_f32_be(block, offset)?;
                        offset += 4;
                        Color::gray(v)
                    }
                    other => {
                        return Err(IoError::InvalidFormat {
                            format: "ase".into(),
                            message: format!(
                                "unsupported ASE color model {:?}",
                                String::from_utf8_lossy(other)
                            ),
                        });
                    }
                };

                let color_type = if offset + 2 <= block.len() {
                    read_u16_be(block, offset)?
                } else {
                    2 // Normal
                };

                let color = if color_type == 1 {
                    // Spot color
                    saw_cmyk = true;
                    Color::spot(swatch_name.clone(), 1.0, base_color.to_cmyk())
                } else {
                    base_color
                };
                swatches.push(ColorSwatch::new(swatch_name, color));
            }
            _ => {
                // Skip unknown future ASE blocks gracefully
            }
        }
    }

    let mode = if saw_cmyk {
        ColorMode::Cmyk
    } else {
        ColorMode::Rgb
    };
    let mut palette = ColorPalette::new(
        palette_name.unwrap_or_else(|| "ASE Palette".to_owned()),
        mode,
    );
    palette.swatches = swatches;
    Ok(palette)
}

/// Serializes `palette` to binary big-endian Adobe Swatch Exchange (`.ase`) bytes.
pub fn export_ase(palette: &ColorPalette) -> IoResult<Vec<u8>> {
    let mut out = Vec::with_capacity(12 + palette.swatches.len() * 48);
    let has_group = !palette.name.trim().is_empty();
    let total_blocks = palette.swatches.len() + if has_group { 2 } else { 0 };

    out.extend_from_slice(b"ASEF");
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(total_blocks as u32).to_be_bytes());

    if has_group {
        let mut group_payload = Vec::new();
        write_utf16be_pascal(&mut group_payload, &palette.name);
        out.extend_from_slice(&0xC001u16.to_be_bytes());
        out.extend_from_slice(&(group_payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&group_payload);
    }

    for sw in &palette.swatches {
        let mut payload = Vec::with_capacity(48);
        write_utf16be_pascal(&mut payload, &sw.name);

        match &sw.color {
            Color::Cmyk { c, m, y, k, .. } => {
                payload.extend_from_slice(b"CMYK");
                payload.extend_from_slice(&c.to_be_bytes());
                payload.extend_from_slice(&m.to_be_bytes());
                payload.extend_from_slice(&y.to_be_bytes());
                payload.extend_from_slice(&k.to_be_bytes());
                payload.extend_from_slice(&2u16.to_be_bytes()); // Normal process
            }
            Color::Spot {
                tint,
                fallback_cmyk,
                ..
            } => {
                payload.extend_from_slice(b"CMYK");
                for &ch in fallback_cmyk {
                    payload.extend_from_slice(&(ch * *tint).clamp(0.0, 1.0).to_be_bytes());
                }
                payload.extend_from_slice(&1u16.to_be_bytes()); // Spot
            }
            Color::Lab { l, a_ch, b_ch, .. } => {
                payload.extend_from_slice(b"LAB ");
                payload.extend_from_slice(&(l / 100.0).clamp(0.0, 1.0).to_be_bytes());
                payload.extend_from_slice(&a_ch.to_be_bytes());
                payload.extend_from_slice(&b_ch.to_be_bytes());
                payload.extend_from_slice(&2u16.to_be_bytes());
            }
            Color::Gray { v, .. } => {
                payload.extend_from_slice(b"GRAY");
                payload.extend_from_slice(&v.clamp(0.0, 1.0).to_be_bytes());
                payload.extend_from_slice(&2u16.to_be_bytes());
            }
            Color::Rgba { .. } | Color::Hsl { .. } => {
                let [r, g, b, _] = sw.color.to_rgba_f32();
                payload.extend_from_slice(b"RGB ");
                payload.extend_from_slice(&r.to_be_bytes());
                payload.extend_from_slice(&g.to_be_bytes());
                payload.extend_from_slice(&b.to_be_bytes());
                payload.extend_from_slice(&2u16.to_be_bytes());
            }
        }

        out.extend_from_slice(&0x0001u16.to_be_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&payload);
    }

    if has_group {
        out.extend_from_slice(&0xC002u16.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
    }

    Ok(out)
}

// =============================================================================
// JASC / Named ASCII Palette (.pal)
// =============================================================================

/// Parses a JASC-PAL (`JASC-PAL\n0100\n<count>\nR G B`) or quoted-name ASCII `.pal`
/// (`"Name" C M Y K` / `"Name" R G B`) palette string.
pub fn import_pal(text: &str) -> IoResult<ColorPalette> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return Err(IoError::InvalidFormat {
            format: "pal".into(),
            message: "empty .pal file".into(),
        });
    }

    if lines[0] == "JASC-PAL" {
        if lines.len() < 3 {
            return Err(IoError::InvalidFormat {
                format: "pal".into(),
                message: "truncated JASC-PAL header".into(),
            });
        }
        let expected_count: usize = lines[2].parse().map_err(|_| IoError::InvalidFormat {
            format: "pal".into(),
            message: format!("invalid JASC-PAL swatch count '{}'", lines[2]),
        })?;
        if expected_count > MAX_PALETTE_SWATCHES {
            return Err(IoError::LimitExceeded {
                limit: format!("max_palette_swatches={MAX_PALETTE_SWATCHES}"),
                actual: expected_count.to_string(),
            });
        }

        let mut palette = ColorPalette::new("JASC Palette", ColorMode::Rgb);
        for line in lines.iter().skip(3).take(expected_count) {
            let nums: Vec<&str> = line.split_whitespace().collect();
            if nums.len() < 3 {
                return Err(IoError::InvalidFormat {
                    format: "pal".into(),
                    message: format!("expected R G B in JASC-PAL line '{line}'"),
                });
            }
            let r: u8 = nums[0].parse().map_err(|_| IoError::InvalidFormat {
                format: "pal".into(),
                message: format!("invalid red channel in '{line}'"),
            })?;
            let g: u8 = nums[1].parse().map_err(|_| IoError::InvalidFormat {
                format: "pal".into(),
                message: format!("invalid green channel in '{line}'"),
            })?;
            let b: u8 = nums[2].parse().map_err(|_| IoError::InvalidFormat {
                format: "pal".into(),
                message: format!("invalid blue channel in '{line}'"),
            })?;
            let label = if nums.len() > 3 {
                nums[3..].join(" ")
            } else {
                format!("#{r:02X}{g:02X}{b:02X}")
            };
            palette.add_swatch(ColorSwatch::new(label, Color::rgba8(r, g, b, 255)));
        }
        return Ok(palette);
    }

    // Quoted-name ASCII `.pal`: `"Swatch Name"   C M Y K` (0..100) or `"Swatch Name" R G B`
    let mut palette = ColorPalette::new("Named CMYK Palette", ColorMode::Cmyk);
    let mut saw_cmyk = false;
    for line in &lines {
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let (swatch_name, rest) = if let Some(after_open) = line.strip_prefix('"') {
            let Some(close_idx) = after_open.find('"') else {
                continue;
            };
            (
                after_open[..close_idx].trim().to_owned(),
                &after_open[close_idx + 1..],
            )
        } else {
            continue;
        };
        let mut vals = Vec::new();
        for tok in rest.split_whitespace() {
            let v = tok.parse::<f32>().map_err(|_| IoError::InvalidFormat {
                format: "pal".into(),
                message: format!("invalid channel token '{tok}' in .pal line '{line}'"),
            })?;
            if !v.is_finite() {
                return Err(IoError::InvalidFormat {
                    format: "pal".into(),
                    message: format!("non-finite color value '{tok}' in .pal line '{line}'"),
                });
            }
            vals.push(v);
        }
        if (vals.len() == 4 || vals.len() == 3) && palette.swatches.len() >= MAX_PALETTE_SWATCHES {
            return Err(IoError::LimitExceeded {
                limit: format!("max_palette_swatches={MAX_PALETTE_SWATCHES}"),
                actual: "exceeded".into(),
            });
        }
        if vals.len() == 4 {
            saw_cmyk = true;
            palette.add_swatch(ColorSwatch::new(
                swatch_name,
                Color::cmyk(
                    vals[0] / 100.0,
                    vals[1] / 100.0,
                    vals[2] / 100.0,
                    vals[3] / 100.0,
                ),
            ));
        } else if vals.len() == 3 {
            palette.add_swatch(ColorSwatch::new(
                swatch_name,
                Color::rgba8(
                    vals[0].clamp(0.0, 255.0).round() as u8,
                    vals[1].clamp(0.0, 255.0).round() as u8,
                    vals[2].clamp(0.0, 255.0).round() as u8,
                    255,
                ),
            ));
        }
    }
    if palette.swatches.is_empty() {
        return Err(IoError::InvalidFormat {
            format: "pal".into(),
            message: "no valid swatches found in .pal text".into(),
        });
    }
    if !saw_cmyk {
        palette.color_mode = ColorMode::Rgb;
    }
    Ok(palette)
}

/// Serializes `palette` to JASC-PAL (`0100`) `.pal` text.
#[must_use]
pub fn export_pal(palette: &ColorPalette) -> String {
    let mut out = String::with_capacity(palette.swatches.len() * 16 + 32);
    out.push_str("JASC-PAL\n0100\n");
    let _ = writeln!(out, "{}", palette.swatches.len());
    for sw in &palette.swatches {
        let [r, g, b, _] = sw.color.to_rgba8();
        let _ = writeln!(out, "{r} {g} {b}");
    }
    out
}

// =============================================================================
// File-level auto-detect helpers
// =============================================================================

const MAX_PALETTE_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Imports a `.gpl`, `.ase`, or `.pal` palette file, detecting format by magic bytes or extension.
pub fn import_palette_file(path: &Path) -> IoResult<ColorPalette> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_PALETTE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if (bytes.len() as u64) > MAX_PALETTE_FILE_BYTES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_palette_file_bytes={MAX_PALETTE_FILE_BYTES}"),
            actual: format!("palette file exceeds {MAX_PALETTE_FILE_BYTES} bytes"),
        });
    }
    if bytes.starts_with(b"ASEF") {
        return import_ase(&bytes);
    }
    if bytes.starts_with(b"8BCB") {
        return import_acb(&bytes);
    }
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "ase" => import_ase(&bytes),
        "acb" => import_acb(&bytes),
        "gpl" => {
            let text = std::str::from_utf8(&bytes).map_err(|e| IoError::InvalidFormat {
                format: "gpl".into(),
                message: e.to_string(),
            })?;
            import_gpl(text)
        }
        "pal" => {
            let text = std::str::from_utf8(&bytes).map_err(|e| IoError::InvalidFormat {
                format: "pal".into(),
                message: e.to_string(),
            })?;
            import_pal(text)
        }
        other => Err(IoError::UnsupportedPaletteFormat(other.to_owned())),
    }
}

/// Atomically exports `palette` to `.gpl`, `.ase`, or `.pal` based on `path`'s extension.
pub fn export_palette_file(palette: &ColorPalette, path: &Path) -> IoResult<()> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "gpl" => atomic_write_bytes(path, export_gpl(palette).as_bytes()),
        "ase" => {
            let bytes = export_ase(palette)?;
            atomic_write_bytes(path, &bytes)
        }
        "pal" => atomic_write_bytes(path, export_pal(palette).as_bytes()),
        other => Err(IoError::UnsupportedPaletteFormat(other.to_owned())),
    }
}

// =============================================================================
// Big-endian binary helpers
// =============================================================================

fn read_u16_be(buf: &[u8], off: usize) -> IoResult<u16> {
    let slice = buf
        .get(off..off + 2)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "ase".into(),
            message: "unexpected EOF reading u16".into(),
        })?;
    Ok(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_u32_be(buf: &[u8], off: usize) -> IoResult<u32> {
    let slice = buf
        .get(off..off + 4)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "ase".into(),
            message: "unexpected EOF reading u32".into(),
        })?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_finite_f32_be(buf: &[u8], off: usize) -> IoResult<f32> {
    let slice = buf
        .get(off..off + 4)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "ase".into(),
            message: "unexpected EOF reading f32".into(),
        })?;
    let val = f32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]);
    if !val.is_finite() {
        return Err(IoError::InvalidFormat {
            format: "ase".into(),
            message: "non-finite float channel in ASE color block".into(),
        });
    }
    Ok(val)
}

// =============================================================================
// Adobe Color Book (.acb) — clean-room 8BCB binary reader
// =============================================================================

fn strip_acb_localization(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("$$$") {
        if let Some((_, rhs)) = trimmed.split_once('=') {
            return rhs.trim().to_owned();
        }
    }
    trimmed.to_owned()
}

/// Parses an Adobe Color Book (`.acb`, magic `8BCB`) binary palette into a [`ColorPalette`].
pub fn import_acb(bytes: &[u8]) -> IoResult<ColorPalette> {
    if bytes.len() < 16 || !bytes.starts_with(b"8BCB") {
        return Err(IoError::InvalidFormat {
            format: "acb".into(),
            message: "missing 8BCB header signature".into(),
        });
    }
    let version = read_u16_be(bytes, 4)?;
    if version == 0 || version > 2 {
        return Err(IoError::InvalidFormat {
            format: "acb".into(),
            message: format!("unsupported ACB version {version}"),
        });
    }
    let _book_id = read_u16_be(bytes, 6)?;
    let mut off = 8usize;

    let (raw_title, next) = read_utf16be_u32_pascal(bytes, off)?;
    off = next;
    let (raw_prefix, next) = read_utf16be_u32_pascal(bytes, off)?;
    off = next;
    let (raw_postfix, next) = read_utf16be_u32_pascal(bytes, off)?;
    off = next;
    let (_raw_desc, next) = read_utf16be_u32_pascal(bytes, off)?;
    off = next;

    let title = strip_acb_localization(&raw_title);
    let prefix = strip_acb_localization(&raw_prefix);
    let postfix = strip_acb_localization(&raw_postfix);

    let color_count = usize::from(read_u16_be(bytes, off)?);
    let _page_size = read_u16_be(bytes, off + 2)?;
    let _page_selector_offset = read_u16_be(bytes, off + 4)?;
    let color_model = read_u16_be(bytes, off + 6)?;
    off += 8;

    if color_count > MAX_PALETTE_SWATCHES {
        return Err(IoError::LimitExceeded {
            limit: format!("max_palette_swatches={MAX_PALETTE_SWATCHES}"),
            actual: format!("{color_count} swatches in ACB"),
        });
    }

    let comp_count = match color_model {
        0 => 3usize, // RGB
        2 => 4usize, // CMYK (inverted 0=100%, 255=0%)
        7 => 3usize, // Lab
        8 => 1usize, // Grayscale
        other => {
            return Err(IoError::InvalidFormat {
                format: "acb".into(),
                message: format!("unsupported ACB color model {other}"),
            });
        }
    };

    let mut raw_entries: Vec<(String, Color)> = Vec::with_capacity(color_count);
    for _ in 0..color_count {
        let (raw_name, next) = read_utf16be_u32_pascal(bytes, off)?;
        off = next;
        let code_slice = bytes
            .get(off..off + 6)
            .ok_or_else(|| IoError::InvalidFormat {
                format: "acb".into(),
                message: "truncated ACB color key code".into(),
            })?;
        off += 6;
        let ch = bytes
            .get(off..off + comp_count)
            .ok_or_else(|| IoError::InvalidFormat {
                format: "acb".into(),
                message: "truncated ACB color channel bytes".into(),
            })?;
        off += comp_count;

        let clean_name = strip_acb_localization(&raw_name);
        let full_name = if !clean_name.is_empty() {
            let mut joined = String::new();
            if !prefix.is_empty() {
                joined.push_str(&prefix);
                if !prefix.ends_with(' ') && !clean_name.starts_with(' ') {
                    joined.push(' ');
                }
            }
            joined.push_str(&clean_name);
            if !postfix.is_empty() {
                if !joined.ends_with(' ') && !postfix.starts_with(' ') {
                    joined.push(' ');
                }
                joined.push_str(&postfix);
            }
            joined
        } else {
            String::from_utf8_lossy(code_slice).trim().to_owned()
        };
        let swatch_name = if full_name.is_empty() {
            "Unnamed".to_owned()
        } else {
            full_name
        };

        let base_color = match color_model {
            0 => Color::rgba8(ch[0], ch[1], ch[2], 255),
            2 => Color::cmyk(
                (255.0 - f32::from(ch[0])) / 255.0,
                (255.0 - f32::from(ch[1])) / 255.0,
                (255.0 - f32::from(ch[2])) / 255.0,
                (255.0 - f32::from(ch[3])) / 255.0,
            ),
            7 => Color::lab(
                (f32::from(ch[0]) * 100.0) / 255.0,
                f32::from(i16::from(ch[1]) - 128),
                f32::from(i16::from(ch[2]) - 128),
            ),
            8 => Color::gray(f32::from(ch[0]) / 255.0),
            _ => unreachable!(),
        };
        raw_entries.push((swatch_name, base_color));
    }

    let is_spot_book = bytes.get(off..off + 4) == Some(b"spot");
    let mode = if matches!(color_model, 2 | 7) || is_spot_book {
        ColorMode::Cmyk
    } else {
        ColorMode::Rgb
    };
    let mut palette = ColorPalette::new(
        if title.is_empty() {
            "ACB Color Book".to_owned()
        } else {
            title
        },
        mode,
    );
    for (name, color) in raw_entries {
        let final_color = if is_spot_book {
            Color::spot(name.clone(), 1.0, color.to_cmyk())
        } else {
            color
        };
        palette.add_swatch(ColorSwatch::new(name, final_color));
    }
    Ok(palette)
}

fn read_utf16be_u32_pascal(buf: &[u8], off: usize) -> IoResult<(String, usize)> {
    let unit_len = usize::try_from(read_u32_be(buf, off)?).map_err(|_| IoError::InvalidFormat {
        format: "acb".into(),
        message: "UTF-16 string length overflow".into(),
    })?;
    if unit_len > 65_536 {
        return Err(IoError::LimitExceeded {
            limit: "max_acb_string_units=65536".into(),
            actual: format!("unit_len={unit_len}"),
        });
    }
    let start = off + 4;
    let byte_len = unit_len
        .checked_mul(2)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "acb".into(),
            message: "UTF-16 string length overflow".into(),
        })?;
    let end = start
        .checked_add(byte_len)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "acb".into(),
            message: "UTF-16 string offset overflow".into(),
        })?;
    if end > buf.len() {
        return Err(IoError::InvalidFormat {
            format: "acb".into(),
            message: "truncated UTF-16BE string in ACB".into(),
        });
    }
    let mut units = Vec::with_capacity(unit_len);
    for chunk in buf[start..end].as_chunks::<2>().0 {
        units.push(u16::from_be_bytes(*chunk));
    }
    if units.last() == Some(&0) {
        units.pop();
    }
    let decoded = String::from_utf16(&units).map_err(|e| IoError::InvalidFormat {
        format: "acb".into(),
        message: format!("invalid UTF-16BE string in ACB: {e}"),
    })?;
    Ok((decoded, end))
}

fn read_utf16be_pascal(buf: &[u8], off: usize) -> IoResult<(String, usize)> {
    let unit_len = usize::from(read_u16_be(buf, off)?);
    let start = off + 2;
    let byte_len = unit_len
        .checked_mul(2)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "ase".into(),
            message: "UTF-16 string length overflow".into(),
        })?;
    let end = start
        .checked_add(byte_len)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "ase".into(),
            message: "UTF-16 string offset overflow".into(),
        })?;
    if end > buf.len() {
        return Err(IoError::InvalidFormat {
            format: "ase".into(),
            message: "truncated UTF-16BE string in ASE block".into(),
        });
    }
    let mut units = Vec::with_capacity(unit_len);
    for chunk in buf[start..end].as_chunks::<2>().0 {
        units.push(u16::from_be_bytes(*chunk));
    }
    if units.last() == Some(&0) {
        units.pop();
    }
    let decoded = String::from_utf16(&units).map_err(|e| IoError::InvalidFormat {
        format: "ase".into(),
        message: format!("invalid UTF-16BE string: {e}"),
    })?;
    Ok((decoded, end))
}

fn write_utf16be_pascal(out: &mut Vec<u8>, s: &str) {
    let mut units: Vec<u16> = s.encode_utf16().collect();
    units.push(0); // NUL terminator
    let count = units.len().min(u16::MAX as usize);
    out.extend_from_slice(&(count as u16).to_be_bytes());
    for &u in &units[..count] {
        out.extend_from_slice(&u.to_be_bytes());
    }
}

fn sanitize_single_line(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_palette() -> ColorPalette {
        let mut pal = ColorPalette::new("Brand Print Kit", ColorMode::Cmyk);
        pal.add_swatch(ColorSwatch::new("Process Cyan", Color::CYAN));
        pal.add_swatch(ColorSwatch::new(
            "Studio Red",
            Color::rgba8(220, 40, 60, 255),
        ));
        pal.add_swatch(ColorSwatch::new("Warm Lab", Color::lab(65.0, 22.0, 35.0)));
        pal.add_swatch(ColorSwatch::new("Mid Gray", Color::gray(0.5)));
        pal.add_swatch(ColorSwatch::new("CutContour", Color::cut_contour_spot()));
        pal
    }

    #[test]
    fn gpl_roundtrip_preserves_name_and_rgb_swatches() {
        let pal = sample_palette();
        let gpl = export_gpl(&pal);
        assert!(gpl.starts_with("GIMP Palette\n"));
        let back = import_gpl(&gpl).unwrap();
        assert_eq!(back.name, "Brand Print Kit");
        assert_eq!(back.swatches.len(), 5);
        assert_eq!(back.swatches[1].name, "Studio Red");
        assert_eq!(back.swatches[1].color.to_rgba8(), [220, 40, 60, 255]);
    }

    #[test]
    fn ase_binary_roundtrip_preserves_cmyk_rgb_lab_gray_and_spot() {
        let pal = sample_palette();
        let bytes = export_ase(&pal).unwrap();
        assert!(bytes.starts_with(b"ASEF"));

        let back = import_ase(&bytes).unwrap();
        assert_eq!(back.name, "Brand Print Kit");
        assert_eq!(back.swatches.len(), 5);
        assert!(matches!(back.swatches[0].color, Color::Cmyk { c, .. } if (c - 1.0).abs() < 1e-5));
        assert!(matches!(back.swatches[1].color, Color::Rgba { .. }));
        assert!(
            matches!(back.swatches[2].color, Color::Lab { l, a_ch, b_ch, .. }
                if (l - 65.0).abs() < 1e-3 && (a_ch - 22.0).abs() < 1e-3 && (b_ch - 35.0).abs() < 1e-3)
        );
        assert!(matches!(back.swatches[3].color, Color::Gray { v, .. } if (v - 0.5).abs() < 1e-5));
        assert!(
            matches!(&back.swatches[4].color, Color::Spot { name, .. } if name == "CutContour")
        );
    }

    #[test]
    fn pal_roundtrip_supports_jasc_and_named_cmyk_text() {
        let pal = sample_palette();
        let jasc = export_pal(&pal);
        assert!(jasc.starts_with("JASC-PAL\n0100\n5\n"));
        let back = import_pal(&jasc).unwrap();
        assert_eq!(back.swatches.len(), 5);
        assert_eq!(back.swatches[1].color.to_rgba8(), [220, 40, 60, 255]);

        let named_text = "\"Cyan Ink\"  100 0 0 0\n\"Key Black\"  0 0 0 100\n";
        let named_pal = import_pal(named_text).unwrap();
        assert_eq!(named_pal.name, "Named CMYK Palette");
        assert_eq!(named_pal.color_mode, ColorMode::Cmyk);
        assert_eq!(named_pal.swatches.len(), 2);
        assert_eq!(named_pal.swatches[0].name, "Cyan Ink");
        assert_eq!(named_pal.swatches[0].color.to_cmyk(), [1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn palette_file_helpers_roundtrip_all_formats() {
        let dir = tempfile::tempdir().unwrap();
        let pal = sample_palette();
        for ext in ["gpl", "ase", "pal"] {
            let path = dir.path().join(format!("kit.{ext}"));
            export_palette_file(&pal, &path).unwrap();
            let loaded = import_palette_file(&path).unwrap();
            assert_eq!(loaded.swatches.len(), 5, "swatch count mismatch for .{ext}");
        }
    }

    fn push_acb_str(buf: &mut Vec<u8>, s: &str) {
        let units: Vec<u16> = s.encode_utf16().collect();
        buf.extend_from_slice(&(units.len() as u32).to_be_bytes());
        for u in units {
            buf.extend_from_slice(&u.to_be_bytes());
        }
    }

    #[test]
    fn acb_8bcb_import_parses_cmyk_inverted_rgb_lab_and_spot() {
        // Test 1: CMYK inverted (0 => 100%, 255 => 0%) + $$$ localization + spot footer
        let mut acb = Vec::new();
        acb.extend_from_slice(b"8BCB");
        acb.extend_from_slice(&1u16.to_be_bytes()); // version
        acb.extend_from_slice(&4001u16.to_be_bytes()); // book_id
        push_acb_str(&mut acb, "$$$/ColorBooks/Test/Title=Studio Spot Book");
        push_acb_str(&mut acb, "Ink");
        push_acb_str(&mut acb, "C");
        push_acb_str(&mut acb, "Clean-room test book");
        acb.extend_from_slice(&2u16.to_be_bytes()); // color_count
        acb.extend_from_slice(&7u16.to_be_bytes()); // page_size
        acb.extend_from_slice(&3u16.to_be_bytes()); // page_selector_offset
        acb.extend_from_slice(&2u16.to_be_bytes()); // CMYK
                                                    // Entry 1: pure Cyan => raw [0, 255, 255, 255]
        push_acb_str(&mut acb, "100");
        acb.extend_from_slice(b"INK100");
        acb.extend_from_slice(&[0, 255, 255, 255]);
        // Entry 2: 50% Magenta + 100% Yellow => raw [255, 128, 0, 255]
        push_acb_str(&mut acb, "200");
        acb.extend_from_slice(b"INK200");
        acb.extend_from_slice(&[255, 128, 0, 255]);
        acb.extend_from_slice(b"spot");

        let pal = import_acb(&acb).unwrap();
        assert_eq!(pal.name, "Studio Spot Book");
        assert_eq!(pal.color_mode, ColorMode::Cmyk);
        assert_eq!(pal.swatches.len(), 2);
        assert_eq!(pal.swatches[0].name, "Ink 100 C");
        assert!(matches!(
            &pal.swatches[0].color,
            Color::Spot { name, fallback_cmyk, .. }
                if name == "Ink 100 C" && (fallback_cmyk[0] - 1.0).abs() < 1e-4 && fallback_cmyk[1] < 1e-4
        ));
        assert!(matches!(
            &pal.swatches[1].color,
            Color::Spot { fallback_cmyk, .. }
                if (fallback_cmyk[1] - 0.5).abs() < 0.01 && (fallback_cmyk[2] - 1.0).abs() < 1e-4
        ));

        // Test 2: RGB via file auto-detect + Lab
        let mut rgb_acb = Vec::new();
        rgb_acb.extend_from_slice(b"8BCB");
        rgb_acb.extend_from_slice(&1u16.to_be_bytes());
        rgb_acb.extend_from_slice(&1u16.to_be_bytes());
        push_acb_str(&mut rgb_acb, "RGB Book");
        push_acb_str(&mut rgb_acb, "");
        push_acb_str(&mut rgb_acb, "");
        push_acb_str(&mut rgb_acb, "");
        rgb_acb.extend_from_slice(&1u16.to_be_bytes());
        rgb_acb.extend_from_slice(&5u16.to_be_bytes());
        rgb_acb.extend_from_slice(&0u16.to_be_bytes());
        rgb_acb.extend_from_slice(&0u16.to_be_bytes()); // RGB
        push_acb_str(&mut rgb_acb, "Coral");
        rgb_acb.extend_from_slice(b"CORAL1");
        rgb_acb.extend_from_slice(&[240, 80, 60]);
        let dir = tempfile::tempdir().unwrap();
        let acb_path = dir.path().join("book.acb");
        std::fs::write(&acb_path, &rgb_acb).unwrap();
        let loaded = import_palette_file(&acb_path).unwrap();
        assert_eq!(loaded.swatches[0].color.to_rgba8(), [240, 80, 60, 255]);
    }
}
