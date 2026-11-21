// SPDX-License-Identifier: MIT OR Apache-2.0

//! Headless high-DPI page export & visual regression rendering entry points.

use crate::backend::{validate_surface_dimensions, CanvasBackend, RgbaBuffer, TinySkiaBackend};
use crate::error::{RenderError, RenderResult};
use crate::renderer::{RenderOptions, SceneRenderer};
use crate::viewport::{ViewMode, Viewport};
use coree_core::math::{sanitize_f64, MM_PER_INCH};
use coree_core::Document;
use image::{ImageFormat, RgbaImage};
use std::io::Cursor;
use std::path::Path;

/// Straight-RGBA rendered page output produced by [`render_page_to_rgba`].
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedImage {
    pub width: u32,
    pub height: u32,
    pub dpi: f64,
    pub rgba: Vec<u8>,
}

impl RenderedImage {
    /// Reads `[r, g, b, a]` at pixel `(x, y)`, or `[0, 0, 0, 0]` if out of bounds.
    #[inline]
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        [
            self.rgba[idx],
            self.rgba[idx + 1],
            self.rgba[idx + 2],
            self.rgba[idx + 3],
        ]
    }

    /// Encodes this rendered image as PNG bytes in memory.
    pub fn encode_png(&self) -> RenderResult<Vec<u8>> {
        let img = RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .ok_or_else(|| RenderError::Raster("rendered RGBA buffer length mismatch".into()))?;
        let mut cursor = Cursor::new(Vec::new());
        img.write_to(&mut cursor, ImageFormat::Png)
            .map_err(|e| RenderError::Raster(format!("PNG encode failed: {e}")))?;
        Ok(cursor.into_inner())
    }

    /// Writes this rendered image directly to a PNG file on disk.
    pub fn save_png(&self, path: impl AsRef<Path>) -> RenderResult<()> {
        let img = RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .ok_or_else(|| RenderError::Raster("rendered RGBA buffer length mismatch".into()))?;
        img.save_with_format(path, ImageFormat::Png)
            .map_err(|e| RenderError::Raster(format!("PNG save failed: {e}")))?;
        Ok(())
    }
}

/// Renders page `page_idx` of `doc` headlessly at `dpi` into a straight-RGBA [`RenderedImage`].
pub fn render_page_to_rgba(
    doc: &Document,
    page_idx: usize,
    dpi: f64,
    view_mode: ViewMode,
) -> RenderResult<RenderedImage> {
    render_page_to_rgba_with_options(doc, page_idx, dpi, view_mode, &RenderOptions::export())
}

/// Renders page `page_idx` headlessly with custom [`RenderOptions`] (e.g. transparent background or guides).
pub fn render_page_to_rgba_with_options(
    doc: &Document,
    page_idx: usize,
    dpi: f64,
    view_mode: ViewMode,
    options: &RenderOptions,
) -> RenderResult<RenderedImage> {
    if page_idx >= doc.pages.len() {
        return Err(RenderError::PageOutOfRange {
            index: page_idx,
            len: doc.pages.len(),
        });
    }
    if !dpi.is_finite() || dpi <= 0.0 {
        return Err(RenderError::InvalidParameter(format!(
            "DPI must be positive and finite, got {dpi}"
        )));
    }
    let safe_dpi = sanitize_f64(dpi, 96.0).clamp(1.0, 2400.0);
    let page = &doc.pages[page_idx];
    let px_per_mm = safe_dpi / MM_PER_INCH;
    let width = (page.width_mm * px_per_mm).round().max(1.0) as u32;
    let height = (page.height_mm * px_per_mm).round().max(1.0) as u32;

    let mut backend = TinySkiaBackend::new(width, height)?;
    let viewport = Viewport::for_export(width, height, safe_dpi);

    // Clone only active_page_index / multipage flag onto a lightweight render call
    let mut doc_view = doc.clone();
    doc_view.active_page_index = page_idx;
    doc_view.multipage_view = false;

    let mut renderer = SceneRenderer::new();
    renderer.render_document(&doc_view, &viewport, view_mode, options, &mut backend);
    let buf = backend.into_rgba();

    Ok(RenderedImage {
        width: buf.width,
        height: buf.height,
        dpi: safe_dpi,
        rgba: buf.rgba,
    })
}

/// Rasterizes raw SVG bytes into a straight-RGBA [`RgbaBuffer`] of `width × height` pixels
/// using `usvg` + `resvg` (useful for SVG preview thumbnails & visual regression checks).
pub fn render_svg_to_rgba(svg_bytes: &[u8], width: u32, height: u32) -> RenderResult<RgbaBuffer> {
    validate_surface_dimensions(width, height)?;
    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg_bytes, &opts)
        .map_err(|e| RenderError::Raster(format!("SVG parse failed: {e}")))?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or(RenderError::AllocationFailed { width, height })?;
    let size = tree.size();
    let sx = if size.width() > 0.0 {
        width as f32 / size.width()
    } else {
        1.0
    };
    let sy = if size.height() > 0.0 {
        height as f32 / size.height()
    } else {
        1.0
    };
    let mut pm_mut = pixmap.as_mut();
    resvg::render(&tree, tiny_skia::Transform::from_scale(sx, sy), &mut pm_mut);
    let rgba = pixmap.take_demultiplied();
    Ok(RgbaBuffer {
        width,
        height,
        rgba,
    })
}
