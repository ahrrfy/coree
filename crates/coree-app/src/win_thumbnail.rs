// SPDX-License-Identifier: MIT OR Apache-2.0

//! Safe `#![forbid(unsafe_code)]` Windows `IThumbnailProvider` integration metadata,
//! premultiplied BGRA DIB conversion helpers, and Explorer cache notification bridge.
//!
//! The native COM in-process server (`coree_thumb.dll`) lives in
//! `packaging/windows/thumbnail_provider/` and delegates rendering to `coree.com thumbnail`.

use std::path::Path;

use coree_io::{decode_raster_bytes_with_limits, IoLimits};

/// COM CLSID registered for the native `coree` Explorer thumbnail provider (`coree_thumb.dll`).
pub const COREE_THUMB_CLSID: &str = "{B4E8A7C2-5F19-4D3A-9C6E-1A2B3C4D5E6F}";

/// Standard Windows `IThumbnailProvider` interface IID (`thumbcache.h`).
pub const IID_ITHUMBNAIL_PROVIDER: &str = "{e357fccd-a995-4576-b01f-234630154e96}";

/// Standard Windows `IInitializeWithStream` interface IID (`propsys.h`).
pub const IID_IINITIALIZE_WITH_STREAM: &str = "{b824b49d-22ac-4161-ac8a-9916e8fa3f7f}";

/// File extensions registered with the Windows Explorer `IThumbnailProvider` handler.
pub const THUMBNAIL_EXTENSIONS: &[&str] = &[".coree", ".cdr", ".cdt", ".cmx", ".cpt"];

/// Top-down 32-bpp premultiplied BGRA frame compatible with Win32 `CreateDIBSection` / `WTSAT_ARGB`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailBgraFrame {
    pub width: u32,
    pub height: u32,
    pub bgra_premul: Vec<u8>,
}

/// Converts straight RGBA8 pixels (`width * height * 4`) to top-down premultiplied BGRA8 (`WTSAT_ARGB`).
pub fn rgba_to_premultiplied_bgra(
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<ThumbnailBgraFrame, String> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| "thumbnail pixel buffer dimensions overflowed".to_owned())?;
    if width == 0 || height == 0 {
        return Err("thumbnail width and height must be non-zero".to_owned());
    }
    if rgba.len() != expected {
        return Err(format!(
            "thumbnail RGBA buffer length mismatch: expected {expected} bytes, got {}",
            rgba.len()
        ));
    }
    let mut bgra_premul = Vec::with_capacity(expected);
    for px in rgba.as_chunks::<4>().0 {
        let a = u16::from(px[3]);
        let r = ((u16::from(px[0]) * a + 127) / 255) as u8;
        let g = ((u16::from(px[1]) * a + 127) / 255) as u8;
        let b = ((u16::from(px[2]) * a + 127) / 255) as u8;
        bgra_premul.extend_from_slice(&[b, g, r, px[3]]);
    }
    Ok(ThumbnailBgraFrame {
        width,
        height,
        bgra_premul,
    })
}

/// Extracts a bounded Page 0 / embedded thumbnail for `path` (`1..=4096` px) and converts it
/// into top-down 32-bpp premultiplied BGRA pixels ready for Windows Explorer `HBITMAP` creation.
pub fn extract_file_thumbnail_bgra(
    path: &Path,
    max_size_px: u32,
) -> Result<ThumbnailBgraFrame, String> {
    if max_size_px == 0 || max_size_px > 4096 {
        return Err(format!(
            "thumbnail max_size_px must be within 1..=4096 (got {max_size_px})"
        ));
    }
    let png_bytes = crate::cdr_bridge::extract_file_thumbnail_png(path, max_size_px)?;
    let asset = decode_raster_bytes_with_limits(&png_bytes, &IoLimits::default())
        .map_err(|e| format!("failed to decode thumbnail PNG bytes: {e}"))?;
    let rgba = asset.to_rgba8_vec();
    rgba_to_premultiplied_bgra(asset.width, asset.height, &rgba)
}

/// Returns `(registry_subkey, value_name, value_data)` tuples (relative to `Classes\` /
/// `Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved`) needed to install
/// `coree_thumb.dll` cleanly in both WiX HKLM MSI and portable HKCU scripts.
#[must_use]
pub fn windows_thumbnail_registry_entries(dll_path: &str) -> Vec<(String, String, String)> {
    let mut entries = vec![
        (
            format!("Software\\Classes\\CLSID\\{COREE_THUMB_CLSID}"),
            String::new(),
            "coree Thumbnail Provider".to_owned(),
        ),
        (
            format!("Software\\Classes\\CLSID\\{COREE_THUMB_CLSID}\\InprocServer32"),
            String::new(),
            dll_path.to_owned(),
        ),
        (
            format!("Software\\Classes\\CLSID\\{COREE_THUMB_CLSID}\\InprocServer32"),
            "ThreadingModel".to_owned(),
            "Apartment".to_owned(),
        ),
        (
            "Software\\Microsoft\\Windows\\CurrentVersion\\Shell Extensions\\Approved".to_owned(),
            COREE_THUMB_CLSID.to_owned(),
            "coree Thumbnail Provider".to_owned(),
        ),
        (
            format!("Software\\Classes\\coree.Document\\ShellEx\\{IID_ITHUMBNAIL_PROVIDER}"),
            String::new(),
            COREE_THUMB_CLSID.to_owned(),
        ),
    ];
    for ext in THUMBNAIL_EXTENSIONS {
        entries.push((
            format!("Software\\Classes\\{ext}\\ShellEx\\{IID_ITHUMBNAIL_PROVIDER}"),
            String::new(),
            COREE_THUMB_CLSID.to_owned(),
        ));
    }
    entries
}

/// Notifies Windows Explorer shell to refresh file association & thumbnail handler caches.
#[cfg(windows)]
pub fn notify_windows_shell_assoc_changed() -> Result<(), String> {
    let status = std::process::Command::new("ie4uinit.exe")
        .arg("-show")
        .status()
        .map_err(|e| format!("failed to invoke ie4uinit.exe: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ie4uinit.exe exited with status {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coree_core::{
        Affine2D, Color, ColorMode, Document, Fill, OutlineStyle, ParametricShape, SceneObject,
    };

    #[test]
    fn windows_thumbnail_clsid_and_registry_entries_cover_all_extensions() {
        assert!(COREE_THUMB_CLSID.starts_with('{') && COREE_THUMB_CLSID.ends_with('}'));
        assert_eq!(COREE_THUMB_CLSID.len(), 38);
        assert_eq!(
            IID_ITHUMBNAIL_PROVIDER,
            "{e357fccd-a995-4576-b01f-234630154e96}"
        );
        assert_eq!(
            IID_IINITIALIZE_WITH_STREAM,
            "{b824b49d-22ac-4161-ac8a-9916e8fa3f7f}"
        );

        let dll = r"C:\Program Files\coree\coree_thumb.dll";
        let entries = windows_thumbnail_registry_entries(dll);
        assert!(entries.iter().any(|(k, v, d)| {
            k.ends_with("\\InprocServer32") && v == "ThreadingModel" && d == "Apartment"
        }));
        assert!(entries.iter().any(|(k, v, d)| {
            k.ends_with("Shell Extensions\\Approved")
                && v == COREE_THUMB_CLSID
                && d == "coree Thumbnail Provider"
        }));
        for ext in [".coree", ".cdr", ".cdt", ".cmx", ".cpt"] {
            let expected_key =
                format!("Software\\Classes\\{ext}\\ShellEx\\{IID_ITHUMBNAIL_PROVIDER}");
            assert!(
                entries
                    .iter()
                    .any(|(k, _, d)| k == &expected_key && d == COREE_THUMB_CLSID),
                "missing ShellEx registration for {ext}"
            );
        }
    }

    #[test]
    fn rgba_to_premultiplied_bgra_premultiplies_and_swaps_channels() {
        // Pixel 0: opaque red (255, 0, 0, 255) -> BGRA (0, 0, 255, 255)
        // Pixel 1: half-transparent green (0, 200, 100, 128) -> B~50, G~100, R=0, A=128
        let frame = rgba_to_premultiplied_bgra(2, 1, &[255, 0, 0, 255, 0, 200, 100, 128]).unwrap();
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 1);
        assert_eq!(&frame.bgra_premul[0..4], &[0, 0, 255, 255]);
        assert_eq!(frame.bgra_premul[6], 0);
        assert!((frame.bgra_premul[5] as i16 - 100).abs() <= 1);
        assert!((frame.bgra_premul[4] as i16 - 50).abs() <= 1);
        assert_eq!(frame.bgra_premul[7], 128);
        assert!(rgba_to_premultiplied_bgra(0, 1, &[]).is_err());
    }

    #[test]
    fn extract_file_thumbnail_bgra_renders_coree_document_cross_platform() {
        let mut doc = Document::new("WinThumb", 100.0, 100.0, ColorMode::Rgb, 300.0);
        let obj = SceneObject::new(
            coree_core::ObjectId(0),
            "Box",
            coree_core::NodePayload::Shape(ParametricShape::Rectangle {
                width: 60.0,
                height: 60.0,
                corner_radii: [0.0; 4],
                corner_style: coree_core::CornerStyle::Rounded,
                lock_corners: true,
            }),
        )
        .with_fill(Fill::Solid(Color::rgba8(0, 160, 220, 255)))
        .with_outline(OutlineStyle::none())
        .with_transform(Affine2D::translate(20.0, 20.0));
        doc.add_object(obj, None).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample_thumb.coree");
        coree_io::save_coree(&doc, &path).unwrap();

        assert!(extract_file_thumbnail_bgra(&path, 0).is_err());
        let bgra = extract_file_thumbnail_bgra(&path, 128).unwrap();
        assert!(bgra.width > 0 && bgra.width <= 128);
        assert!(bgra.height > 0 && bgra.height <= 128);
        assert_eq!(
            bgra.bgra_premul.len(),
            (bgra.width * bgra.height * 4) as usize
        );
    }
}
