// SPDX-License-Identifier: MIT OR Apache-2.0

//! Hardened raster import & export (PNG, JPEG, WebP, TIFF, BMP) with dimension guards,
//! PNG `pHYs` DPI metadata round-trip, and crash-safe atomic writes.

use crate::atomic::atomic_write_bytes;
use crate::error::{IoError, IoLimits, IoResult};
use coree_core::{ColorMode, PixelDepth, RasterAsset};
use flate2::Crc;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{
    ColorType, DynamicImage, ImageBuffer, ImageEncoder, ImageFormat, ImageReader, Limits, RgbImage,
    Rgba, RgbaImage,
};
use std::io::Cursor;
use std::path::Path;

/// Supported raster file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RasterFormat {
    Png,
    Jpeg,
    WebP,
    Tiff,
    Bmp,
}

impl RasterFormat {
    /// Detects format from a file extension (case-insensitive, with or without leading `.`).
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        let clean = ext.trim_start_matches('.').to_ascii_lowercase();
        match clean.as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "webp" => Some(Self::WebP),
            "tif" | "tiff" => Some(Self::Tiff),
            "bmp" => Some(Self::Bmp),
            _ => None,
        }
    }

    /// Detects format from a file path's extension.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        path.extension()
            .and_then(|s| s.to_str())
            .and_then(Self::from_extension)
    }

    /// Canonical lowercase file extension.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebP => "webp",
            Self::Tiff => "tiff",
            Self::Bmp => "bmp",
        }
    }
}

/// Configuration for raster image export.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RasterExportOptions {
    pub format: RasterFormat,
    /// Target physical resolution in Dots Per Inch (embedded in PNG `pHYs` chunk).
    pub dpi: f32,
    /// JPEG encoder quality (`1..=100`, ignored for non-JPEG formats).
    pub jpeg_quality: u8,
}

impl Default for RasterExportOptions {
    fn default() -> Self {
        Self {
            format: RasterFormat::Png,
            dpi: 300.0,
            jpeg_quality: 92,
        }
    }
}

impl RasterExportOptions {
    /// Convenience constructor for a format and DPI.
    #[must_use]
    pub fn new(format: RasterFormat, dpi: f32) -> Self {
        Self {
            format,
            dpi: if dpi.is_finite() && dpi > 0.0 {
                dpi
            } else {
                300.0
            },
            jpeg_quality: 92,
        }
    }
}

/// Imports a raster file (PNG/JPEG/WebP/TIFF/BMP) using default [`IoLimits`].
pub fn import_raster(path: &Path) -> IoResult<RasterAsset> {
    import_raster_with_limits(path, &IoLimits::default())
}

/// Imports a raster file (PNG/JPEG/WebP/TIFF/BMP) enforcing `limits`.
pub fn import_raster_with_limits(path: &Path, limits: &IoLimits) -> IoResult<RasterAsset> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let max_bytes = limits.max_uncompressed_bytes;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if (bytes.len() as u64) > max_bytes {
        return Err(IoError::LimitExceeded {
            limit: format!("max_uncompressed_bytes={max_bytes}"),
            actual: format!("raster file exceeds {max_bytes} bytes"),
        });
    }
    decode_raster_bytes_with_limits(&bytes, limits)
}

/// Decodes raw raster bytes (PNG/JPEG/WebP/TIFF/BMP) into a [`RasterAsset`],
/// validating dimensions before pixel allocation and extracting PNG `pHYs` DPI when present.
pub fn decode_raster_bytes_with_limits(bytes: &[u8], limits: &IoLimits) -> IoResult<RasterAsset> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let format = reader.format();
    let (width, height) = reader.into_dimensions()?;

    validate_raster_dimensions(width, height, limits)?;

    let mut decode_reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut img_limits = Limits::default();
    img_limits.max_image_width = Some(limits.max_raster_dimension);
    img_limits.max_image_height = Some(limits.max_raster_dimension);
    img_limits.max_alloc = Some(limits.max_raster_pixels.saturating_mul(8).max(64 * 1024));
    decode_reader.limits(img_limits);

    let decoded = decode_reader.decode()?;
    let dpi = if format == Some(ImageFormat::Png) {
        parse_png_phys_dpi(bytes).unwrap_or(96.0)
    } else {
        96.0
    };

    let is_16bit = matches!(
        decoded,
        DynamicImage::ImageLuma16(_)
            | DynamicImage::ImageLumaA16(_)
            | DynamicImage::ImageRgb16(_)
            | DynamicImage::ImageRgba16(_)
    );
    let asset = if is_16bit {
        let rgba16 = decoded.to_rgba16();
        RasterAsset::new_16bit(
            rgba16.width(),
            rgba16.height(),
            rgba16.into_raw(),
            dpi,
            ColorMode::Rgb,
        )?
    } else {
        let rgba = decoded.to_rgba8();
        RasterAsset::new(
            rgba.width(),
            rgba.height(),
            rgba.into_raw(),
            dpi,
            ColorMode::Rgb,
        )?
    };
    Ok(asset)
}

/// Validates raster dimensions against `limits`.
pub fn validate_raster_dimensions(width: u32, height: u32, limits: &IoLimits) -> IoResult<()> {
    if width == 0 || height == 0 {
        return Err(IoError::InvalidFormat {
            format: "raster".into(),
            message: format!("invalid zero raster dimensions ({width}x{height})"),
        });
    }
    if width > limits.max_raster_dimension || height > limits.max_raster_dimension {
        return Err(IoError::LimitExceeded {
            limit: format!("max_raster_dimension={}", limits.max_raster_dimension),
            actual: format!("{width}x{height}"),
        });
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > limits.max_raster_pixels {
        return Err(IoError::LimitExceeded {
            limit: format!("max_raster_pixels={}", limits.max_raster_pixels),
            actual: format!("{pixels} pixels ({width}x{height})"),
        });
    }
    Ok(())
}

/// Encodes an RGBA8 pixel buffer into bytes in the requested [`RasterFormat`].
///
/// For PNG, embeds a valid `pHYs` DPI metadata chunk right after `IHDR`.
pub fn export_raster_to_bytes(
    width: u32,
    height: u32,
    rgba: &[u8],
    opts: &RasterExportOptions,
) -> IoResult<Vec<u8>> {
    validate_raster_dimensions(width, height, &IoLimits::default())?;
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| IoError::InvalidFormat {
            format: "raster".into(),
            message: "pixel buffer size overflow".into(),
        })?;
    if rgba.len() != expected {
        return Err(IoError::InvalidFormat {
            format: "raster".into(),
            message: format!("expected {expected} RGBA bytes, got {}", rgba.len()),
        });
    }

    let mut out = Vec::new();
    match opts.format {
        RasterFormat::Png => {
            let encoder = PngEncoder::new(&mut out);
            encoder.write_image(rgba, width, height, ColorType::Rgba8.into())?;
            let dpi = if opts.dpi.is_finite() && opts.dpi > 0.0 {
                opts.dpi
            } else {
                300.0
            };
            out = inject_png_phys_chunk(&out, dpi)?;
        }
        RasterFormat::Jpeg => {
            let rgb = flatten_rgba_to_rgb_white(width, height, rgba);
            let quality = opts.jpeg_quality.clamp(1, 100);
            let mut encoder = JpegEncoder::new_with_quality(&mut out, quality);
            encoder.encode(rgb.as_raw(), width, height, ColorType::Rgb8.into())?;
        }
        RasterFormat::WebP => {
            let img = RgbaImage::from_raw(width, height, rgba.to_vec()).ok_or_else(|| {
                IoError::InvalidFormat {
                    format: "webp".into(),
                    message: "failed to build RGBA image".into(),
                }
            })?;
            let mut cursor = Cursor::new(&mut out);
            img.write_to(&mut cursor, ImageFormat::WebP)?;
        }
        RasterFormat::Tiff => {
            let img = RgbaImage::from_raw(width, height, rgba.to_vec()).ok_or_else(|| {
                IoError::InvalidFormat {
                    format: "tiff".into(),
                    message: "failed to build RGBA image".into(),
                }
            })?;
            let mut cursor = Cursor::new(&mut out);
            img.write_to(&mut cursor, ImageFormat::Tiff)?;
        }
        RasterFormat::Bmp => {
            let img = RgbaImage::from_raw(width, height, rgba.to_vec()).ok_or_else(|| {
                IoError::InvalidFormat {
                    format: "bmp".into(),
                    message: "failed to build RGBA image".into(),
                }
            })?;
            let mut cursor = Cursor::new(&mut out);
            img.write_to(&mut cursor, ImageFormat::Bmp)?;
        }
    }

    Ok(out)
}

/// Encodes an owned 16-bit RGBA pixel buffer (`Vec<u16>`) into a 16-bit PNG without extra copy.
pub fn export_raster_16_png_owned_to_bytes(
    width: u32,
    height: u32,
    rgba16: Vec<u16>,
    dpi: f32,
) -> IoResult<Vec<u8>> {
    validate_raster_dimensions(width, height, &IoLimits::default())?;
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| IoError::InvalidFormat {
            format: "raster".into(),
            message: "16-bit pixel buffer size overflow".into(),
        })?;
    if rgba16.len() != expected {
        return Err(IoError::InvalidFormat {
            format: "raster".into(),
            message: format!("expected {expected} RGBA16 channels, got {}", rgba16.len()),
        });
    }
    let img: ImageBuffer<Rgba<u16>, Vec<u16>> = ImageBuffer::from_raw(width, height, rgba16)
        .ok_or_else(|| IoError::InvalidFormat {
            format: "png".into(),
            message: "failed to build RGBA16 image".into(),
        })?;
    let mut out = Vec::new();
    let mut cursor = Cursor::new(&mut out);
    img.write_to(&mut cursor, ImageFormat::Png)?;
    let valid_dpi = if dpi.is_finite() && dpi > 0.0 {
        dpi
    } else {
        300.0
    };
    inject_png_phys_chunk(&out, valid_dpi)
}

/// Encodes a 16-bit RGBA pixel buffer (`u16` per channel) into a 16-bit PNG with embedded `pHYs` DPI.
pub fn export_raster_16_png_to_bytes(
    width: u32,
    height: u32,
    rgba16: &[u16],
    dpi: f32,
) -> IoResult<Vec<u8>> {
    export_raster_16_png_owned_to_bytes(width, height, rgba16.to_vec(), dpi)
}

/// Encodes a [`RasterAsset`] preserving 16-bit PNG depth when `asset.pixel_depth == PixelDepth::Bit16`.
pub fn export_raster_asset_to_bytes(
    asset: &RasterAsset,
    opts: &RasterExportOptions,
) -> IoResult<Vec<u8>> {
    if asset.pixel_depth == PixelDepth::Bit16 && opts.format == RasterFormat::Png {
        export_raster_16_png_owned_to_bytes(
            asset.width,
            asset.height,
            asset.to_rgba16_vec(),
            opts.dpi,
        )
    } else {
        export_raster_to_bytes(asset.width, asset.height, &asset.to_rgba8_vec(), opts)
    }
}

/// Atomically exports an RGBA8 pixel buffer to `path`.
pub fn export_raster(
    width: u32,
    height: u32,
    rgba: &[u8],
    opts: &RasterExportOptions,
    path: &Path,
) -> IoResult<()> {
    let bytes = export_raster_to_bytes(width, height, rgba, opts)?;
    atomic_write_bytes(path, &bytes)
}

/// Composites RGBA pixels over opaque white to produce RGB8 for JPEG output.
fn flatten_rgba_to_rgb_white(width: u32, height: u32, rgba: &[u8]) -> RgbImage {
    let mut rgb = Vec::with_capacity((width as usize) * (height as usize) * 3);
    for px in rgba.as_chunks::<4>().0 {
        let a = u16::from(px[3]);
        let inv_a = 255 - a;
        let r = ((u16::from(px[0]) * a + 255 * inv_a + 127) / 255) as u8;
        let g = ((u16::from(px[1]) * a + 255 * inv_a + 127) / 255) as u8;
        let b = ((u16::from(px[2]) * a + 255 * inv_a + 127) / 255) as u8;
        rgb.push(r);
        rgb.push(g);
        rgb.push(b);
    }
    RgbImage::from_raw(width, height, rgb).expect("rgb buffer exact size")
}

const PNG_SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

/// Injects (or replaces) a PNG `pHYs` chunk immediately after `IHDR` with the given DPI.
pub fn inject_png_phys_chunk(png_bytes: &[u8], dpi: f32) -> IoResult<Vec<u8>> {
    if png_bytes.len() < 33 || png_bytes[..8] != PNG_SIG {
        return Err(IoError::InvalidFormat {
            format: "png".into(),
            message: "invalid PNG signature or truncated header".into(),
        });
    }
    let ihdr_len =
        u32::from_be_bytes([png_bytes[8], png_bytes[9], png_bytes[10], png_bytes[11]]) as usize;
    if &png_bytes[12..16] != b"IHDR" {
        return Err(IoError::InvalidFormat {
            format: "png".into(),
            message: "missing IHDR chunk".into(),
        });
    }
    let Some(ihdr_end) = 16usize.checked_add(ihdr_len).and_then(|v| v.checked_add(4)) else {
        return Err(IoError::InvalidFormat {
            format: "png".into(),
            message: "IHDR chunk length overflow".into(),
        });
    };
    if ihdr_end > png_bytes.len() {
        return Err(IoError::InvalidFormat {
            format: "png".into(),
            message: "truncated IHDR chunk".into(),
        });
    }

    let ppm = ((f64::from(dpi) / 0.0254)
        .round()
        .clamp(1.0, f64::from(u32::MAX))) as u32;
    let mut phys_data = [0u8; 9];
    phys_data[0..4].copy_from_slice(&ppm.to_be_bytes());
    phys_data[4..8].copy_from_slice(&ppm.to_be_bytes());
    phys_data[8] = 1; // unit specifier: meter

    let mut crc = Crc::new();
    crc.update(b"pHYs");
    crc.update(&phys_data);
    let checksum = crc.sum();

    let mut out = Vec::with_capacity(png_bytes.len() + 21);
    out.extend_from_slice(&png_bytes[..ihdr_end]);
    out.extend_from_slice(&9u32.to_be_bytes());
    out.extend_from_slice(b"pHYs");
    out.extend_from_slice(&phys_data);
    out.extend_from_slice(&checksum.to_be_bytes());

    // Copy remaining chunks, skipping any pre-existing pHYs chunk to avoid duplicates.
    let mut pos = ihdr_end;
    while pos + 12 <= png_bytes.len() {
        let len = u32::from_be_bytes([
            png_bytes[pos],
            png_bytes[pos + 1],
            png_bytes[pos + 2],
            png_bytes[pos + 3],
        ]) as usize;
        let chunk_type = &png_bytes[pos + 4..pos + 8];
        let chunk_end = pos.saturating_add(12).saturating_add(len);
        if chunk_end > png_bytes.len() {
            out.extend_from_slice(&png_bytes[pos..]);
            break;
        }
        if chunk_type != b"pHYs" {
            out.extend_from_slice(&png_bytes[pos..chunk_end]);
        }
        pos = chunk_end;
    }

    Ok(out)
}

/// Parses DPI from a PNG `pHYs` chunk when unit specifier == 1 (pixels per meter).
#[must_use]
pub fn parse_png_phys_dpi(png_bytes: &[u8]) -> Option<f32> {
    if png_bytes.len() < 33 || png_bytes[..8] != PNG_SIG {
        return None;
    }
    let mut pos = 8usize;
    while pos + 12 <= png_bytes.len() {
        let len = u32::from_be_bytes([
            png_bytes[pos],
            png_bytes[pos + 1],
            png_bytes[pos + 2],
            png_bytes[pos + 3],
        ]) as usize;
        let chunk_type = &png_bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = data_start.checked_add(len)?;
        let chunk_end = data_end.checked_add(4)?;
        if chunk_end > png_bytes.len() {
            return None;
        }
        if chunk_type == b"pHYs" && len == 9 {
            let data = &png_bytes[data_start..data_end];
            if data[8] == 1 {
                let ppm_x = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                if ppm_x > 0 {
                    return Some((f64::from(ppm_x) * 0.0254).round() as f32);
                }
            }
            return None;
        }
        if chunk_type == b"IEND" {
            break;
        }
        pos = chunk_end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rgba(w: u32, h: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                buf.push(((x * 32) & 0xFF) as u8);
                buf.push(((y * 32) & 0xFF) as u8);
                buf.push(180);
                buf.push(255);
            }
        }
        buf
    }

    #[test]
    fn png_roundtrip_preserves_phys_dpi_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("badge.png");
        let rgba = sample_rgba(8, 6);
        export_raster(
            8,
            6,
            &rgba,
            &RasterExportOptions::new(RasterFormat::Png, 300.0),
            &path,
        )
        .unwrap();

        let imported = import_raster(&path).unwrap();
        assert_eq!(imported.width, 8);
        assert_eq!(imported.height, 6);
        assert_eq!(imported.to_rgba8_vec(), rgba);
        assert!((imported.source_dpi - 300.0).abs() <= 1.0);
    }

    #[test]
    fn all_five_raster_formats_export_and_reimport() {
        let dir = tempfile::tempdir().unwrap();
        let rgba = sample_rgba(12, 10);
        for fmt in [
            RasterFormat::Png,
            RasterFormat::Jpeg,
            RasterFormat::WebP,
            RasterFormat::Tiff,
            RasterFormat::Bmp,
        ] {
            let path = dir.path().join(format!("img.{}", fmt.extension()));
            export_raster(12, 10, &rgba, &RasterExportOptions::new(fmt, 150.0), &path).unwrap();
            let loaded = import_raster(&path).unwrap();
            assert_eq!(loaded.width, 12, "width mismatch for {fmt:?}");
            assert_eq!(loaded.height, 10, "height mismatch for {fmt:?}");
        }
    }

    #[test]
    fn raster_dimension_limit_is_enforced_before_decode() {
        let rgba = sample_rgba(16, 16);
        let bytes = export_raster_to_bytes(
            16,
            16,
            &rgba,
            &RasterExportOptions::new(RasterFormat::Png, 96.0),
        )
        .unwrap();

        let strict = IoLimits {
            max_raster_dimension: 8,
            ..IoLimits::default()
        };
        let err = decode_raster_bytes_with_limits(&bytes, &strict).unwrap_err();
        assert!(matches!(err, IoError::LimitExceeded { .. }));
    }
}
