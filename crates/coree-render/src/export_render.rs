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

/// Renders an isolated [`coree_core::SceneObject`] (including deformations, drop/block shadows,
/// contours, 2.5D extrusions, blends, conical/square/mesh/pattern/texture fills, transparency
/// masks, symmetry clones, and live optical lenses) into a cropped straight-RGBA [`RgbaBuffer`]
/// paired with its page-space bounding rectangle in millimeters.
pub fn render_isolated_object_to_rgba(
    doc: &Document,
    page_idx: usize,
    obj_id: coree_core::ObjectId,
    parent_xf: coree_core::Affine2D,
    dpi: f64,
) -> RenderResult<Option<(coree_core::Rect2D, RgbaBuffer)>> {
    use crate::effects_render::{
        apply_geometry_effects, effect_bounds_padding_mm, extract_local_geometry_path,
        symmetry_effect_transforms,
    };
    use coree_core::{Affine2D, Color, Effect, Rect2D};

    if page_idx >= doc.pages.len() || !parent_xf.is_finite() {
        return Ok(None);
    }
    let Some(obj) = doc.objects.get(&obj_id) else {
        return Ok(None);
    };
    if !obj.visible || !obj.transform.is_finite() {
        return Ok(None);
    }
    let safe_dpi = sanitize_f64(dpi, 300.0).clamp(36.0, 600.0);
    let page = &doc.pages[page_idx];

    // Estimate object-local bounds including vector geometry deformations & Blends
    let mut local_bb = extract_local_geometry_path(obj)
        .and_then(|p| apply_geometry_effects(&p, &obj.effects).bounding_box());
    let mut renderer = SceneRenderer::new();
    let doc_obj_bb = match &obj.payload {
        coree_core::NodePayload::ArtisticText(at) => {
            Some(renderer.text_engine_mut().measure_artistic_bounds(at))
        }
        _ => doc.object_bounds(obj_id),
    };

    let mut page_bb: Option<Rect2D> = None;
    let sym_xfs = symmetry_effect_transforms(&obj.effects);
    for sym_xf in sym_xfs {
        let obj_page_xf = parent_xf.compose(&sym_xf).compose(&obj.transform);
        if !obj_page_xf.is_finite() {
            continue;
        }
        if let Some(ref lbb) = local_bb {
            let tb = obj_page_xf.transform_rect(*lbb);
            page_bb = Some(match page_bb {
                Some(acc) => acc.union(tb),
                None => tb,
            });
        } else if let Some(dbb) = doc_obj_bb {
            // `doc.object_bounds` already includes `obj.transform`; apply `parent_xf * sym_xf`
            let tb = parent_xf.compose(&sym_xf).transform_rect(dbb);
            page_bb = Some(match page_bb {
                Some(acc) => acc.union(tb),
                None => tb,
            });
        }
    }

    for eff in &obj.effects {
        match eff {
            Effect::Blend { target_id, .. } => {
                if let Some(tbb) = doc.object_bounds(*target_id) {
                    let tb = parent_xf.transform_rect(tbb);
                    page_bb = Some(match page_bb {
                        Some(acc) => acc.union(tb),
                        None => tb,
                    });
                }
            }
            Effect::Extrude25D {
                vanishing_point, ..
            } if vanishing_point.is_finite() => {
                let vp_rect = Rect2D::from_xywh(vanishing_point.x, vanishing_point.y, 0.1, 0.1);
                page_bb = Some(match page_bb {
                    Some(acc) => acc.union(vp_rect),
                    None => vp_rect,
                });
            }
            _ => {}
        }
    }

    let _ = &mut local_bb;
    let Some(raw_bb) = page_bb else {
        return Ok(None);
    };
    let pad = effect_bounds_padding_mm(&obj.effects)
        + obj.outline.effective_width_mm().max(0.5) * 2.0
        + 2.0;
    let padded = raw_bb.expand(pad);
    // Guard against runaway oversized coordinates while keeping generous bleed margin
    let safe_limit = Rect2D::from_xywh(
        -page.width_mm,
        -page.height_mm,
        page.width_mm * 3.0,
        page.height_mm * 3.0,
    );
    let Some(rect_mm) = padded.intersection(safe_limit) else {
        return Ok(None);
    };
    let w_mm = rect_mm.width().max(0.1);
    let h_mm = rect_mm.height().max(0.1);

    let base_px_per_mm = safe_dpi / MM_PER_INCH;
    let raw_w = (w_mm * base_px_per_mm).ceil();
    let raw_h = (h_mm * base_px_per_mm).ceil();
    let max_dim = 2048.0_f64;
    let scale_down = if raw_w.max(raw_h) > max_dim {
        max_dim / raw_w.max(raw_h)
    } else {
        1.0
    };
    let px_per_mm = base_px_per_mm * scale_down;
    let px_w = (w_mm * px_per_mm).ceil().clamp(1.0, 2048.0) as u32;
    let px_h = (h_mm * px_per_mm).ceil().clamp(1.0, 2048.0) as u32;

    let mut underlying_ids = Vec::new();
    if obj.lens.is_some() {
        for layer in &page.layers {
            if !layer.visible {
                continue;
            }
            for &oid in &layer.objects {
                if oid == obj_id {
                    break;
                }
                underlying_ids.push(oid);
            }
        }
    }

    let mut backend = TinySkiaBackend::new(px_w, px_h)?;
    backend.clear(Color::TRANSPARENT);
    let page_to_surface = Affine2D::scale(px_per_mm, px_per_mm)
        .compose(&Affine2D::translate(-rect_mm.min_x, -rect_mm.min_y));
    let parent_to_surface = page_to_surface.compose(&parent_xf);

    renderer.render_scene_object(
        doc,
        Some(page),
        obj,
        page_to_surface,
        parent_to_surface,
        ViewMode::Enhanced,
        &RenderOptions::export(),
        &mut backend,
        &underlying_ids,
        0,
    );
    let buf = backend.into_rgba();
    Ok(Some((rect_mm, buf)))
}
