// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backend-agnostic 2D canvas rendering trait ([`CanvasBackend`]) and its Pure-Rust
//! CPU reference implementation ([`TinySkiaBackend`]).
//!
//! Keeping all scene rendering, lens compositing, effects, and text pipelines
//! strictly against [`CanvasBackend`] allows future GPU backends (`vello` / `wgpu`)
//! to drop in without modifying `coree-core` or `coree-app`.

use crate::error::{RenderError, RenderResult};
use bytemuck::{Pod, Zeroable};
use coree_core::math::{sanitize_f32, sanitize_f64};
use coree_core::{
    Affine2D, BezierPath, BlendMode, Color, GradientStop, LineCap, LineJoin, OutlineStyle, Point2D,
    Rect2D,
};
use std::sync::{Arc, OnceLock};
use tiny_skia as ts;
use wgpu::util::DeviceExt;

/// Maximum single-axis pixel dimension allowed for safe allocation (16384 px).
pub const MAX_SURFACE_DIM_PX: u32 = 16_384;

/// Maximum total pixel count allowed for a single surface (64M px = 256 MB RGBA).
pub const MAX_SURFACE_PIXELS: u64 = 64_000_000;

/// Integer pixel-space rectangle (`x, y` top-left, `width, height` extent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    /// Creates a pixel rectangle.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns `true` if either dimension is zero.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Right edge `x + width` computed in `i64` and saturated to `i32`.
    #[must_use]
    pub fn right(self) -> i32 {
        (i64::from(self.x) + i64::from(self.width)).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32
    }

    /// Bottom edge `y + height` computed in `i64` and saturated to `i32`.
    #[must_use]
    pub fn bottom(self) -> i32 {
        (i64::from(self.y) + i64::from(self.height)).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32
    }

    /// Intersects `self` with `[0..bounds_w, 0..bounds_h]` using overflow-safe `i64` math,
    /// returning `None` if disjoint.
    #[must_use]
    pub fn clamp_to_bounds(self, bounds_w: u32, bounds_h: u32) -> Option<Self> {
        let bw = i64::from(bounds_w);
        let bh = i64::from(bounds_h);
        let x0 = i64::from(self.x).clamp(0, bw);
        let y0 = i64::from(self.y).clamp(0, bh);
        let x1 = (i64::from(self.x) + i64::from(self.width)).clamp(0, bw);
        let y1 = (i64::from(self.y) + i64::from(self.height)).clamp(0, bh);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Self {
            x: x0 as i32,
            y: y0 as i32,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }

    /// Constructs a clamped pixel rectangle directly from a floating-point screen `Rect2D`
    /// plus integer pixel padding without overflowing `i32` at extreme zoom levels.
    #[must_use]
    pub fn from_f64_rect_clamped(
        rect: Rect2D,
        pad_px: i32,
        bounds_w: u32,
        bounds_h: u32,
    ) -> Option<Self> {
        if rect.is_empty() || bounds_w == 0 || bounds_h == 0 {
            return None;
        }
        let bw = f64::from(bounds_w);
        let bh = f64::from(bounds_h);
        let pad = f64::from(pad_px.max(0));
        let x0 = (rect.min_x.floor() - pad).clamp(0.0, bw) as i32;
        let y0 = (rect.min_y.floor() - pad).clamp(0.0, bh) as i32;
        let x1 = (rect.max_x.ceil() + pad).clamp(0.0, bw) as i32;
        let y1 = (rect.max_y.ceil() + pad).clamp(0.0, bh) as i32;
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Self {
            x: x0,
            y: y0,
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
        })
    }
}

/// Straight (unpremultiplied) 8-bit RGBA pixel buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaBuffer {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl RgbaBuffer {
    /// Creates a transparent `width x height` buffer.
    #[must_use]
    pub fn new_transparent(width: u32, height: u32) -> Self {
        let len = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        Self {
            width,
            height,
            rgba: vec![0u8; len],
        }
    }

    /// Creates a buffer filled with a uniform RGBA8 pixel.
    #[must_use]
    pub fn new_filled(width: u32, height: u32, pixel: [u8; 4]) -> Self {
        let count = (width as usize).saturating_mul(height as usize);
        let mut rgba = Vec::with_capacity(count.saturating_mul(4));
        for _ in 0..count {
            rgba.extend_from_slice(&pixel);
        }
        Self {
            width,
            height,
            rgba,
        }
    }

    /// Constructs from existing straight RGBA bytes, validating exact length.
    pub fn from_vec(width: u32, height: u32, rgba: Vec<u8>) -> RenderResult<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| RenderError::InvalidDimensions {
                width,
                height,
                reason: "byte length overflow".into(),
            })?;
        if rgba.len() != expected {
            return Err(RenderError::InvalidParameter(format!(
                "expected {expected} RGBA bytes for {width}x{height}, got {}",
                rgba.len()
            )));
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    /// Returns `true` if either dimension is zero.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.rgba.is_empty()
    }

    /// Reads straight `[r, g, b, a]` pixel at `(x, y)`, or `None` if out of bounds.
    #[inline]
    #[must_use]
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        Some([
            self.rgba[idx],
            self.rgba[idx + 1],
            self.rgba[idx + 2],
            self.rgba[idx + 3],
        ])
    }

    /// Writes straight `[r, g, b, a]` pixel at `(x, y)` if in bounds.
    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, px: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        self.rgba[idx..idx + 4].copy_from_slice(&px);
    }

    /// Bilinearly samples straight RGBA at floating-point pixel coordinates `(fx, fy)`.
    #[must_use]
    pub fn sample_bilinear(&self, fx: f64, fy: f64) -> [u8; 4] {
        if self.is_empty() || !fx.is_finite() || !fy.is_finite() {
            return [0, 0, 0, 0];
        }
        let max_x = (self.width - 1) as f64;
        let max_y = (self.height - 1) as f64;
        let cx = fx.clamp(0.0, max_x);
        let cy = fy.clamp(0.0, max_y);
        let x0 = cx.floor() as u32;
        let y0 = cy.floor() as u32;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let tx = cx - f64::from(x0);
        let ty = cy - f64::from(y0);

        let p00 = self.get_pixel(x0, y0).unwrap_or([0; 4]);
        let p10 = self.get_pixel(x1, y0).unwrap_or([0; 4]);
        let p01 = self.get_pixel(x0, y1).unwrap_or([0; 4]);
        let p11 = self.get_pixel(x1, y1).unwrap_or([0; 4]);

        let w00 = (1.0 - tx) * (1.0 - ty);
        let w10 = tx * (1.0 - ty);
        let w01 = (1.0 - tx) * ty;
        let w11 = tx * ty;

        let mut out = [0u8; 4];
        for ch in 0..4 {
            let v = f64::from(p00[ch]) * w00
                + f64::from(p10[ch]) * w10
                + f64::from(p01[ch]) * w01
                + f64::from(p11[ch]) * w11;
            out[ch] = v.round().clamp(0.0, 255.0) as u8;
        }
        out
    }

    /// Converts this straight RGBA buffer into a premultiplied `tiny_skia::Pixmap`.
    #[must_use]
    pub fn to_premultiplied_pixmap(&self) -> Option<ts::Pixmap> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let size = ts::IntSize::from_wh(self.width, self.height)?;
        let mut premul = Vec::with_capacity(self.rgba.len());
        for px in self.rgba.as_chunks::<4>().0 {
            let a = px[3];
            if a == 255 {
                premul.extend_from_slice(px);
            } else if a == 0 {
                premul.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let af = u16::from(a);
                let r = ((u16::from(px[0]) * af + 127) / 255) as u8;
                let g = ((u16::from(px[1]) * af + 127) / 255) as u8;
                let b = ((u16::from(px[2]) * af + 127) / 255) as u8;
                premul.extend_from_slice(&[r, g, b, a]);
            }
        }
        ts::Pixmap::from_vec(premul, size)
    }

    /// Converts a premultiplied `tiny_skia::PixmapRef` into a straight [`RgbaBuffer`].
    #[must_use]
    pub fn from_premultiplied_pixmap(pixmap: ts::PixmapRef<'_>) -> Self {
        let width = pixmap.width();
        let height = pixmap.height();
        let src = pixmap.data();
        let mut rgba = Vec::with_capacity(src.len());
        for px in src.as_chunks::<4>().0 {
            let a = px[3];
            if a == 255 {
                rgba.extend_from_slice(px);
            } else if a == 0 {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let af = f32::from(a) / 255.0;
                let r = (f32::from(px[0]) / af).round().clamp(0.0, 255.0) as u8;
                let g = (f32::from(px[1]) / af).round().clamp(0.0, 255.0) as u8;
                let b = (f32::from(px[2]) / af).round().clamp(0.0, 255.0) as u8;
                rgba.extend_from_slice(&[r, g, b, a]);
            }
        }
        Self {
            width,
            height,
            rgba,
        }
    }
}

/// Path fill winding rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillRule {
    #[default]
    Winding,
    EvenOdd,
}

/// Backend-agnostic fill shader description.
#[derive(Debug, Clone)]
pub enum PaintShader {
    Solid(Color),
    LinearGradient {
        start: Point2D,
        end: Point2D,
        stops: Vec<GradientStop>,
    },
    RadialGradient {
        focal: Point2D,
        center: Point2D,
        radius: f64,
        stops: Vec<GradientStop>,
    },
    SweepGradient {
        center: Point2D,
        angle_deg: f64,
        stops: Vec<GradientStop>,
    },
    PixmapPattern {
        tile: RgbaBuffer,
        tile_transform: Affine2D,
    },
}

/// Paint specification for [`CanvasBackend::fill_path`].
#[derive(Debug, Clone)]
pub struct BackendPaint {
    pub shader: PaintShader,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub anti_alias: bool,
}

impl BackendPaint {
    /// Creates a solid opaque paint with anti-aliasing enabled.
    #[must_use]
    pub fn solid(color: Color) -> Self {
        Self {
            shader: PaintShader::Solid(color),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: true,
        }
    }
}

/// Backend-agnostic stroke parameters for [`CanvasBackend::stroke_path`].
#[derive(Debug, Clone)]
pub struct StrokeParams {
    pub color: Color,
    pub width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f32,
    pub dash_pattern: Vec<f32>,
    pub dash_offset: f32,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub anti_alias: bool,
}

impl StrokeParams {
    /// Builds [`StrokeParams`] from a core [`OutlineStyle`] with an explicit stroke width in target units.
    #[must_use]
    pub fn from_outline(
        outline: &OutlineStyle,
        width: f32,
        opacity: f32,
        blend_mode: BlendMode,
        anti_alias: bool,
    ) -> Self {
        Self {
            color: outline.color.clone(),
            width: sanitize_f32(width, 1.0).max(0.0),
            cap: outline.cap,
            join: outline.join,
            miter_limit: sanitize_f64(outline.miter_limit, 4.0).max(1.0) as f32,
            dash_pattern: outline
                .dash_pattern
                .iter()
                .map(|&d| sanitize_f64(d, 0.0).max(0.0) as f32)
                .collect(),
            dash_offset: 0.0,
            opacity: sanitize_f32(opacity, 1.0).clamp(0.0, 1.0),
            blend_mode,
            anti_alias,
        }
    }
}

/// Backend-agnostic 2D vector + raster rendering surface trait.
///
/// Designed so [`TinySkiaBackend`] powers CPU rendering today while a future
/// GPU compute backend (`vello` / `wgpu`) can implement this exact trait with
/// zero changes to scene traversal, lenses, effects, or application code.
pub trait CanvasBackend {
    /// Surface width in pixels.
    fn width(&self) -> u32;

    /// Surface height in pixels.
    fn height(&self) -> u32;

    /// Clears the entire surface to `color`.
    fn clear(&mut self, color: Color);

    /// Sets an optional Bézier flattening tolerance (in path local units) for LOD dragging.
    fn set_flatten_tolerance(&mut self, tolerance: Option<f64>);

    /// Fills a vector `path` transformed by `transform`.
    fn fill_path(
        &mut self,
        path: &BezierPath,
        paint: &BackendPaint,
        rule: FillRule,
        transform: Affine2D,
    );

    /// Strokes a vector `path` transformed by `transform`.
    fn stroke_path(&mut self, path: &BezierPath, stroke: &StrokeParams, transform: Affine2D);

    /// Draws a straight-RGBA `image` transformed by `transform`.
    fn draw_pixmap(
        &mut self,
        image: &RgbaBuffer,
        transform: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        smooth: bool,
    );

    /// Pushes a vector clip path intersected with any active parent clip masks.
    fn push_clip(&mut self, path: &BezierPath, rule: FillRule, transform: Affine2D);

    /// Pops the most recently pushed clip path.
    fn pop_clip(&mut self);

    /// Reads an integer pixel rectangle from the current surface as straight RGBA.
    fn read_region(&self, rect: PixelRect) -> RgbaBuffer;

    /// Composites a straight-RGBA region onto the surface at `rect.x, rect.y`.
    fn composite_region(
        &mut self,
        rect: PixelRect,
        src: &RgbaBuffer,
        opacity: f32,
        blend_mode: BlendMode,
    );

    /// Non-destructively copies the entire surface to a straight [`RgbaBuffer`].
    fn snapshot_rgba(&self) -> RgbaBuffer;

    /// Consumes the backend and returns the final straight [`RgbaBuffer`].
    fn into_rgba(self) -> RgbaBuffer;
}

/// Validates safe surface dimensions before allocating pixel buffers.
pub fn validate_surface_dimensions(width: u32, height: u32) -> RenderResult<()> {
    if width == 0 || height == 0 {
        return Err(RenderError::InvalidDimensions {
            width,
            height,
            reason: "width and height must be >= 1".into(),
        });
    }
    if width > MAX_SURFACE_DIM_PX || height > MAX_SURFACE_DIM_PX {
        return Err(RenderError::InvalidDimensions {
            width,
            height,
            reason: format!("dimension exceeds maximum {MAX_SURFACE_DIM_PX}px"),
        });
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_SURFACE_PIXELS {
        return Err(RenderError::InvalidDimensions {
            width,
            height,
            reason: format!("total pixel count {pixels} exceeds safe limit {MAX_SURFACE_PIXELS}"),
        });
    }
    Ok(())
}

/// Pure-Rust CPU implementation of [`CanvasBackend`] powered by `tiny-skia`.
pub struct TinySkiaBackend {
    pixmap: ts::Pixmap,
    clip_stack: Vec<ts::Mask>,
    flatten_tolerance: Option<f64>,
}

impl TinySkiaBackend {
    /// Allocates a new transparent `width x height` rendering backend.
    pub fn new(width: u32, height: u32) -> RenderResult<Self> {
        validate_surface_dimensions(width, height)?;
        let pixmap = ts::Pixmap::new(width, height)
            .ok_or(RenderError::AllocationFailed { width, height })?;
        Ok(Self {
            pixmap,
            clip_stack: Vec::new(),
            flatten_tolerance: None,
        })
    }

    /// Creates a backend pre-populated from a straight [`RgbaBuffer`].
    pub fn from_rgba(buf: &RgbaBuffer) -> RenderResult<Self> {
        validate_surface_dimensions(buf.width, buf.height)?;
        let pixmap = buf
            .to_premultiplied_pixmap()
            .ok_or(RenderError::AllocationFailed {
                width: buf.width,
                height: buf.height,
            })?;
        Ok(Self {
            pixmap,
            clip_stack: Vec::new(),
            flatten_tolerance: None,
        })
    }

    /// Borrows the underlying premultiplied `tiny_skia::PixmapRef`.
    #[must_use]
    pub fn as_pixmap_ref(&self) -> ts::PixmapRef<'_> {
        self.pixmap.as_ref()
    }
}

impl CanvasBackend for TinySkiaBackend {
    fn width(&self) -> u32 {
        self.pixmap.width()
    }

    fn height(&self) -> u32 {
        self.pixmap.height()
    }

    fn clear(&mut self, color: Color) {
        self.pixmap.fill(color_to_skia(&color, 1.0));
    }

    fn set_flatten_tolerance(&mut self, tolerance: Option<f64>) {
        self.flatten_tolerance = tolerance.filter(|t| t.is_finite() && *t > 0.0);
    }

    fn fill_path(
        &mut self,
        path: &BezierPath,
        paint: &BackendPaint,
        rule: FillRule,
        transform: Affine2D,
    ) {
        if !transform.is_finite() || transform.determinant().abs() <= 1e-12 {
            return;
        }
        let use_device_clip =
            needs_f64_device_clip(path, transform, self.pixmap.width(), self.pixmap.height());
        let Some(ts_path) = (if use_device_clip {
            build_clipped_device_skia_path(
                path,
                transform,
                self.pixmap.width(),
                self.pixmap.height(),
                self.flatten_tolerance,
            )
        } else {
            bezier_to_skia_path(path, self.flatten_tolerance)
        }) else {
            return;
        };
        let ts_xf = if use_device_clip {
            ts::Transform::identity()
        } else {
            affine_to_skia(transform)
        };
        let shader_xf = if use_device_clip {
            affine_to_skia(transform)
        } else {
            ts::Transform::identity()
        };
        let ts_rule = match rule {
            FillRule::Winding => ts::FillRule::Winding,
            FillRule::EvenOdd => ts::FillRule::EvenOdd,
        };
        let opacity = sanitize_f32(paint.opacity, 1.0).clamp(0.0, 1.0);
        if opacity <= 1e-6 {
            return;
        }
        let blend = blend_mode_to_skia(paint.blend_mode);

        // Pattern shader needs owned Pixmap to outlive Paint
        let owned_pattern_pixmap: Option<ts::Pixmap>;
        let mut ts_paint = ts::Paint {
            anti_alias: paint.anti_alias,
            blend_mode: blend,
            ..ts::Paint::default()
        };

        match &paint.shader {
            PaintShader::Solid(color) => {
                ts_paint.set_color(color_to_skia(color, opacity));
                self.pixmap
                    .fill_path(&ts_path, &ts_paint, ts_rule, ts_xf, self.clip_stack.last());
            }
            PaintShader::LinearGradient { start, end, stops } => {
                let sk_stops = convert_stops(stops, opacity);
                if sk_stops.is_empty() {
                    return;
                }
                if sk_stops.len() == 1 || start.distance_to(*end) <= 1e-6 {
                    ts_paint.set_color(color_to_skia(&stops[0].color, opacity));
                } else if let Some(shader) = ts::LinearGradient::new(
                    ts::Point::from_xy(start.x as f32, start.y as f32),
                    ts::Point::from_xy(end.x as f32, end.y as f32),
                    sk_stops,
                    ts::SpreadMode::Pad,
                    shader_xf,
                ) {
                    ts_paint.shader = shader;
                } else {
                    return;
                }
                self.pixmap
                    .fill_path(&ts_path, &ts_paint, ts_rule, ts_xf, self.clip_stack.last());
            }
            PaintShader::RadialGradient {
                focal,
                center,
                radius,
                stops,
            } => {
                let sk_stops = convert_stops(stops, opacity);
                if sk_stops.is_empty() {
                    return;
                }
                let r = sanitize_f64(*radius, 0.0).max(0.0);
                if sk_stops.len() == 1 || r <= 1e-6 {
                    ts_paint.set_color(color_to_skia(&stops[0].color, opacity));
                } else {
                    // Clamp focal point strictly inside radius so tiny-skia doesn't reject it
                    let mut f = *focal;
                    let d = f.distance_to(*center);
                    if d >= r * 0.995 && d > 1e-9 {
                        let scale = (r * 0.99) / d;
                        f = Point2D::new(
                            center.x + (f.x - center.x) * scale,
                            center.y + (f.y - center.y) * scale,
                        );
                    }
                    if let Some(shader) = ts::RadialGradient::new(
                        ts::Point::from_xy(f.x as f32, f.y as f32),
                        0.0,
                        ts::Point::from_xy(center.x as f32, center.y as f32),
                        r as f32,
                        sk_stops,
                        ts::SpreadMode::Pad,
                        shader_xf,
                    ) {
                        ts_paint.shader = shader;
                    } else {
                        return;
                    }
                }
                self.pixmap
                    .fill_path(&ts_path, &ts_paint, ts_rule, ts_xf, self.clip_stack.last());
            }
            PaintShader::SweepGradient {
                center,
                angle_deg,
                stops,
            } => {
                let sk_stops = convert_stops(stops, opacity);
                if sk_stops.is_empty() {
                    return;
                }
                if sk_stops.len() == 1 {
                    ts_paint.set_color(color_to_skia(&stops[0].color, opacity));
                } else {
                    let rot =
                        Affine2D::rotate_about(sanitize_f64(*angle_deg, 0.0).to_radians(), *center);
                    let sweep_xf = if use_device_clip {
                        affine_to_skia(transform.compose(&rot))
                    } else {
                        affine_to_skia(rot)
                    };
                    if let Some(shader) = ts::SweepGradient::new(
                        ts::Point::from_xy(center.x as f32, center.y as f32),
                        0.0,
                        360.0,
                        sk_stops,
                        ts::SpreadMode::Pad,
                        sweep_xf,
                    ) {
                        ts_paint.shader = shader;
                    } else {
                        return;
                    }
                }
                self.pixmap
                    .fill_path(&ts_path, &ts_paint, ts_rule, ts_xf, self.clip_stack.last());
            }
            PaintShader::PixmapPattern {
                tile,
                tile_transform,
            } => {
                owned_pattern_pixmap = tile.to_premultiplied_pixmap();
                let Some(pm) = owned_pattern_pixmap.as_ref() else {
                    return;
                };
                let pat_xf = if use_device_clip {
                    affine_to_skia(transform.compose(tile_transform))
                } else {
                    affine_to_skia(*tile_transform)
                };
                ts_paint.shader = ts::Pattern::new(
                    pm.as_ref(),
                    ts::SpreadMode::Repeat,
                    ts::FilterQuality::Bilinear,
                    opacity,
                    pat_xf,
                );
                self.pixmap
                    .fill_path(&ts_path, &ts_paint, ts_rule, ts_xf, self.clip_stack.last());
            }
        }
    }

    fn stroke_path(&mut self, path: &BezierPath, stroke: &StrokeParams, transform: Affine2D) {
        if !transform.is_finite()
            || transform.determinant().abs() <= 1e-12
            || stroke.width <= 1e-6
            || stroke.opacity <= 1e-6
        {
            return;
        }
        let Some(ts_path) = bezier_to_skia_path(path, self.flatten_tolerance) else {
            return;
        };
        let scale = estimate_scale_2d(transform);
        let effective_px = stroke.width * (scale as f32);
        let dash = build_stroke_dash(&stroke.dash_pattern, stroke.dash_offset);
        let ts_stroke = ts::Stroke {
            width: stroke.width,
            miter_limit: stroke.miter_limit.max(1.0),
            line_cap: match stroke.cap {
                LineCap::Butt => ts::LineCap::Butt,
                LineCap::Round => ts::LineCap::Round,
                LineCap::Square => ts::LineCap::Square,
            },
            line_join: match stroke.join {
                LineJoin::Miter => ts::LineJoin::Miter,
                LineJoin::Round => ts::LineJoin::Round,
                LineJoin::Bevel => ts::LineJoin::Bevel,
            },
            dash,
        };

        // Carryover #9 fix:
        // 1) For anti-aliased thin/hairline strokes (effective_px <= 1.05), bypass tiny-skia's
        //    `treat_as_hairline` -> `anti_hair_line_rgn` (which clips segments to tile bounds
        //    before converting to FDot6 and computing fdot16::div slope, causing ~0.4px seams).
        // 2) For non-AA guide strokes (`!stroke.anti_alias`, e.g. bleed/margin/guidelines),
        //    dash/expand in local space and transform in `f64` quantized to 1/64 px (`FDot6`)
        //    before drawing at identity transform so integer tile offsets produce zero ULP drift.
        if (stroke.anti_alias && effective_px <= 1.05) || !stroke.anti_alias {
            let res_scale = (scale as f32).clamp(0.1, 64.0);
            let dashed = if let Some(ref d) = ts_stroke.dash {
                // Guard huge unclipped guide extents at extreme zooms
                let est_dashes = estimate_dash_count(&ts_path, &stroke.dash_pattern);
                if est_dashes <= MAX_LOCAL_DASH_SEGMENTS {
                    ts_path.dash(d, res_scale)
                } else {
                    None
                }
            } else {
                None
            };
            let source_path = dashed.as_ref().unwrap_or(&ts_path);
            let (expanded_width, eff_opacity) = if effective_px < 1.0 {
                let min_w = (1.0 / scale.max(1e-6)) as f32;
                let op = if stroke.anti_alias {
                    (stroke.opacity * effective_px).clamp(0.0, 1.0)
                } else {
                    stroke.opacity
                };
                (min_w.max(stroke.width), op)
            } else {
                (stroke.width, stroke.opacity)
            };
            if eff_opacity <= 1e-6 {
                return;
            }
            let expand_stroke = ts::Stroke {
                width: expanded_width,
                miter_limit: ts_stroke.miter_limit,
                line_cap: ts_stroke.line_cap,
                line_join: ts_stroke.line_join,
                dash: None,
            };
            if let Some(stroked) = source_path.stroke(&expand_stroke, res_scale) {
                let quant = if stroke.anti_alias { 256.0 } else { 64.0 };
                if let Some(dev_path) = transform_skia_path_f64(&stroked, transform, quant) {
                    let mut ts_paint = ts::Paint {
                        anti_alias: stroke.anti_alias,
                        blend_mode: blend_mode_to_skia(stroke.blend_mode),
                        ..ts::Paint::default()
                    };
                    ts_paint.set_color(color_to_skia(&stroke.color, eff_opacity));
                    self.pixmap.fill_path(
                        &dev_path,
                        &ts_paint,
                        ts::FillRule::Winding,
                        ts::Transform::identity(),
                        self.clip_stack.last(),
                    );
                    return;
                }
            }
        }

        let ts_xf = affine_to_skia(transform);
        let mut ts_paint = ts::Paint {
            anti_alias: stroke.anti_alias,
            blend_mode: blend_mode_to_skia(stroke.blend_mode),
            ..ts::Paint::default()
        };
        ts_paint.set_color(color_to_skia(&stroke.color, stroke.opacity));

        self.pixmap.stroke_path(
            &ts_path,
            &ts_paint,
            &ts_stroke,
            ts_xf,
            self.clip_stack.last(),
        );
    }

    fn draw_pixmap(
        &mut self,
        image: &RgbaBuffer,
        transform: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        smooth: bool,
    ) {
        let opacity = sanitize_f32(opacity, 1.0).clamp(0.0, 1.0);
        if !transform.is_finite()
            || transform.determinant().abs() <= 1e-12
            || opacity <= 1e-6
            || image.is_empty()
        {
            return;
        }
        let Some(pm) = image.to_premultiplied_pixmap() else {
            return;
        };
        let paint = ts::PixmapPaint {
            opacity,
            blend_mode: blend_mode_to_skia(blend_mode),
            quality: if smooth {
                ts::FilterQuality::Bilinear
            } else {
                ts::FilterQuality::Nearest
            },
        };
        self.pixmap.draw_pixmap(
            0,
            0,
            pm.as_ref(),
            &paint,
            affine_to_skia(transform),
            self.clip_stack.last(),
        );
    }

    fn push_clip(&mut self, path: &BezierPath, rule: FillRule, transform: Affine2D) {
        let w = self.pixmap.width();
        let h = self.pixmap.height();
        let Some(mut mask) = ts::Mask::new(w, h) else {
            return;
        };
        let ts_rule = match rule {
            FillRule::Winding => ts::FillRule::Winding,
            FillRule::EvenOdd => ts::FillRule::EvenOdd,
        };
        if transform.is_finite() && transform.determinant().abs() > 1e-12 {
            if needs_f64_device_clip(path, transform, w, h) {
                if let Some(ts_path) =
                    build_clipped_device_skia_path(path, transform, w, h, self.flatten_tolerance)
                {
                    mask.fill_path(&ts_path, ts_rule, true, ts::Transform::identity());
                }
            } else if let Some(ts_path) = bezier_to_skia_path(path, self.flatten_tolerance) {
                mask.fill_path(&ts_path, ts_rule, true, affine_to_skia(transform));
            }
        }
        if let Some(parent) = self.clip_stack.last() {
            let parent_data = parent.data();
            let cur_data = mask.data_mut();
            for (dst, &p) in cur_data.iter_mut().zip(parent_data.iter()) {
                *dst = ((u16::from(*dst) * u16::from(p) + 127) / 255) as u8;
            }
        }
        self.clip_stack.push(mask);
    }

    fn pop_clip(&mut self) {
        self.clip_stack.pop();
    }

    fn read_region(&self, rect: PixelRect) -> RgbaBuffer {
        let Some(clamped) = rect.clamp_to_bounds(self.pixmap.width(), self.pixmap.height()) else {
            return RgbaBuffer::new_transparent(0, 0);
        };
        let Some(int_rect) =
            ts::IntRect::from_xywh(clamped.x, clamped.y, clamped.width, clamped.height)
        else {
            return RgbaBuffer::new_transparent(0, 0);
        };
        match self.pixmap.clone_rect(int_rect) {
            Some(sub) => RgbaBuffer::from_premultiplied_pixmap(sub.as_ref()),
            None => RgbaBuffer::new_transparent(0, 0),
        }
    }

    fn composite_region(
        &mut self,
        rect: PixelRect,
        src: &RgbaBuffer,
        opacity: f32,
        blend_mode: BlendMode,
    ) {
        let opacity = sanitize_f32(opacity, 1.0).clamp(0.0, 1.0);
        if opacity <= 1e-6 || src.is_empty() || rect.is_empty() {
            return;
        }
        let Some(pm) = src.to_premultiplied_pixmap() else {
            return;
        };
        let paint = ts::PixmapPaint {
            opacity,
            blend_mode: blend_mode_to_skia(blend_mode),
            quality: ts::FilterQuality::Nearest,
        };
        self.pixmap.draw_pixmap(
            rect.x,
            rect.y,
            pm.as_ref(),
            &paint,
            ts::Transform::identity(),
            self.clip_stack.last(),
        );
    }

    fn snapshot_rgba(&self) -> RgbaBuffer {
        RgbaBuffer::from_premultiplied_pixmap(self.pixmap.as_ref())
    }

    fn into_rgba(self) -> RgbaBuffer {
        let width = self.pixmap.width();
        let height = self.pixmap.height();
        let rgba = self.pixmap.take_demultiplied();
        RgbaBuffer {
            width,
            height,
            rgba,
        }
    }
}

/// Converts a core [`Color`] scaled by `opacity` into a `tiny_skia::Color`.
#[must_use]
pub fn color_to_skia(color: &Color, opacity: f32) -> ts::Color {
    let [r, g, b, a] = color.to_rgba_f32();
    let op = sanitize_f32(opacity, 1.0).clamp(0.0, 1.0);
    ts::Color::from_rgba(r, g, b, (a * op).clamp(0.0, 1.0)).unwrap_or(ts::Color::TRANSPARENT)
}

/// Converts a core [`BlendMode`] into the matching `tiny_skia::BlendMode`.
#[must_use]
pub const fn blend_mode_to_skia(mode: BlendMode) -> ts::BlendMode {
    match mode {
        BlendMode::Normal => ts::BlendMode::SourceOver,
        BlendMode::Multiply => ts::BlendMode::Multiply,
        BlendMode::Screen => ts::BlendMode::Screen,
        BlendMode::Overlay => ts::BlendMode::Overlay,
        BlendMode::Darken => ts::BlendMode::Darken,
        BlendMode::Lighten => ts::BlendMode::Lighten,
        BlendMode::Difference => ts::BlendMode::Difference,
        BlendMode::Exclusion => ts::BlendMode::Exclusion,
        BlendMode::SoftLight => ts::BlendMode::SoftLight,
        BlendMode::HardLight => ts::BlendMode::HardLight,
        BlendMode::ColorDodge => ts::BlendMode::ColorDodge,
        BlendMode::ColorBurn => ts::BlendMode::ColorBurn,
        BlendMode::Hue => ts::BlendMode::Hue,
        BlendMode::Saturation => ts::BlendMode::Saturation,
        BlendMode::Color => ts::BlendMode::Color,
        BlendMode::Luminosity => ts::BlendMode::Luminosity,
    }
}

/// Converts a core [`Affine2D`] (`[a, b, c, d, tx, ty]`) into `tiny_skia::Transform`.
#[must_use]
pub fn affine_to_skia(affine: Affine2D) -> ts::Transform {
    let [a, b, c, d, tx, ty] = affine.m;
    ts::Transform::from_row(a as f32, b as f32, c as f32, d as f32, tx as f32, ty as f32)
}

/// Returns `true` when `path` transformed to device coordinates exceeds safe `f32`/fixed-point
/// limits for `tiny-skia` rasterization, requiring `f64` Sutherland-Hodgman viewport clipping.
fn needs_f64_device_clip(path: &BezierPath, transform: Affine2D, w: u32, h: u32) -> bool {
    let Some(bb) = path.bounding_box() else {
        return false;
    };
    let sb = transform.transform_rect(bb);
    if !sb.is_finite() {
        return true;
    }
    const LIMIT: f64 = 1_000_000.0;
    sb.min_x < -LIMIT
        || sb.min_y < -LIMIT
        || sb.max_x > f64::from(w) + LIMIT
        || sb.max_y > f64::from(h) + LIMIT
}

/// Transforms `path` to device space in full `f64` precision, clips polygons against an
/// expanded surface rectangle via Sutherland-Hodgman, and builds a device-space `tiny_skia::Path`.
fn build_clipped_device_skia_path(
    path: &BezierPath,
    transform: Affine2D,
    w: u32,
    h: u32,
    flatten_tol: Option<f64>,
) -> Option<ts::Path> {
    if path.is_empty() {
        return None;
    }
    let dev_path = path.transformed(&transform);
    let tol = flatten_tol.unwrap_or(0.25).max(0.25);
    let clip_rect = Rect2D::new(-4.0, -4.0, f64::from(w) + 4.0, f64::from(h) + 4.0);

    let mut pb = ts::PathBuilder::new();
    let mut any = false;
    for poly in dev_path.flatten(tol) {
        let finite: Vec<Point2D> = poly.into_iter().filter(|p| p.is_finite()).collect();
        let clipped = clip_polygon_to_rect_f64(&finite, clip_rect);
        if clipped.len() < 3 {
            continue;
        }
        any = true;
        pb.move_to(clipped[0].x as f32, clipped[0].y as f32);
        for pt in &clipped[1..] {
            pb.line_to(pt.x as f32, pt.y as f32);
        }
        pb.close();
    }
    if any {
        pb.finish()
    } else {
        None
    }
}

/// Sutherland-Hodgman `f64` polygon clipper against an axis-aligned rectangle.
fn clip_polygon_to_rect_f64(poly: &[Point2D], rect: Rect2D) -> Vec<Point2D> {
    if poly.len() < 3 {
        return Vec::new();
    }
    let clip_edge = |pts: &[Point2D],
                     inside: &dyn Fn(Point2D) -> bool,
                     intersect: &dyn Fn(Point2D, Point2D) -> Point2D|
     -> Vec<Point2D> {
        let mut out = Vec::with_capacity(pts.len() + 2);
        if pts.is_empty() {
            return out;
        }
        let mut prev = *pts.last().unwrap();
        let mut prev_in = inside(prev);
        for &cur in pts {
            let cur_in = inside(cur);
            if cur_in {
                if !prev_in {
                    out.push(intersect(prev, cur));
                }
                out.push(cur);
            } else if prev_in {
                out.push(intersect(prev, cur));
            }
            prev = cur;
            prev_in = cur_in;
        }
        out
    };

    let lerp_x = |a: Point2D, b: Point2D, x: f64| -> Point2D {
        let dx = b.x - a.x;
        let t = if dx.abs() > 1e-18 {
            ((x - a.x) / dx).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Point2D::new(x, a.y + (b.y - a.y) * t)
    };
    let lerp_y = |a: Point2D, b: Point2D, y: f64| -> Point2D {
        let dy = b.y - a.y;
        let t = if dy.abs() > 1e-18 {
            ((y - a.y) / dy).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Point2D::new(a.x + (b.x - a.x) * t, y)
    };

    let mut pts = poly.to_vec();
    pts = clip_edge(&pts, &|p| p.x >= rect.min_x, &|a, b| {
        lerp_x(a, b, rect.min_x)
    });
    pts = clip_edge(&pts, &|p| p.x <= rect.max_x, &|a, b| {
        lerp_x(a, b, rect.max_x)
    });
    pts = clip_edge(&pts, &|p| p.y >= rect.min_y, &|a, b| {
        lerp_y(a, b, rect.min_y)
    });
    pts = clip_edge(&pts, &|p| p.y <= rect.max_y, &|a, b| {
        lerp_y(a, b, rect.max_y)
    });
    pts
}

/// Converts a [`BezierPath`] into a `tiny_skia::Path`, optionally flattening curves for LOD dragging.
#[must_use]
pub fn bezier_to_skia_path(path: &BezierPath, flatten_tol: Option<f64>) -> Option<ts::Path> {
    if path.is_empty() {
        return None;
    }
    let mut pb = ts::PathBuilder::new();
    if let Some(tol) = flatten_tol {
        let mut any = false;
        for sp in &path.subpaths {
            if sp.nodes.is_empty() {
                continue;
            }
            let single = BezierPath::from_subpath(sp.clone());
            for poly in single.flatten(tol) {
                if poly.is_empty() {
                    continue;
                }
                any = true;
                pb.move_to(poly[0].x as f32, poly[0].y as f32);
                for pt in &poly[1..] {
                    pb.line_to(pt.x as f32, pt.y as f32);
                }
                if sp.closed {
                    pb.close();
                }
            }
        }
        return if any { pb.finish() } else { None };
    }

    let mut any = false;
    for sp in &path.subpaths {
        if sp.nodes.is_empty() {
            continue;
        }
        let first = sp.nodes[0];
        if !first.pos.is_finite() {
            continue;
        }
        any = true;
        pb.move_to(first.pos.x as f32, first.pos.y as f32);
        let seg_count = sp.segment_count();
        for seg_idx in 0..seg_count {
            let Some((i0, i1)) = sp.segment_node_indices(seg_idx) else {
                continue;
            };
            let n0 = sp.nodes[i0];
            let n1 = sp.nodes[i1];
            if !n1.pos.is_finite() {
                continue;
            }
            match (n0.handle_out, n1.handle_in) {
                (None, None) => pb.line_to(n1.pos.x as f32, n1.pos.y as f32),
                (h0, h1) => {
                    let c0 = h0.filter(|p| p.is_finite()).unwrap_or(n0.pos);
                    let c1 = h1.filter(|p| p.is_finite()).unwrap_or(n1.pos);
                    pb.cubic_to(
                        c0.x as f32,
                        c0.y as f32,
                        c1.x as f32,
                        c1.y as f32,
                        n1.pos.x as f32,
                        n1.pos.y as f32,
                    );
                }
            }
        }
        if sp.closed {
            pb.close();
        }
    }
    if any {
        pb.finish()
    } else {
        None
    }
}

fn convert_stops(stops: &[GradientStop], opacity: f32) -> Vec<ts::GradientStop> {
    let mut sorted: Vec<GradientStop> = stops.to_vec();
    sorted.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    sorted
        .into_iter()
        .map(|s| {
            ts::GradientStop::new(
                sanitize_f32(s.offset, 0.0).clamp(0.0, 1.0),
                color_to_skia(&s.color, opacity),
            )
        })
        .collect()
}

fn build_stroke_dash(pattern: &[f32], offset: f32) -> Option<ts::StrokeDash> {
    let mut clean: Vec<f32> = pattern
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v > 0.0)
        .collect();
    if clean.is_empty() {
        return None;
    }
    // tiny-skia requires an even number of dash intervals (duplicate odd arrays per SVG spec)
    if !clean.len().is_multiple_of(2) {
        let dup = clean.clone();
        clean.extend(dup);
    }
    ts::StrokeDash::new(clean, sanitize_f32(offset, 0.0))
}

#[inline]
fn estimate_scale_2d(xf: Affine2D) -> f64 {
    let [a, b, c, d, _, _] = xf.m;
    let sx = (a * a + b * b).sqrt();
    let sy = (c * c + d * d).sqrt();
    sx.max(sy).max(1e-6)
}

const MAX_LOCAL_DASH_SEGMENTS: usize = 262_144;

fn effective_dash_period_local(dash_pattern: &[f32]) -> Option<f64> {
    let mut count = 0usize;
    let mut sum = 0.0_f64;
    for v in dash_pattern
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v > 0.0)
    {
        sum += f64::from(v);
        count += 1;
    }
    if count == 0 {
        return None;
    }
    // `build_stroke_dash` duplicates odd-length dash arrays per SVG spec.
    if !count.is_multiple_of(2) {
        sum *= 2.0;
    }
    Some(sum.max(1e-4))
}

fn estimate_dash_count(path: &ts::Path, dash_pattern: &[f32]) -> usize {
    let Some(dash_period_local) = effective_dash_period_local(dash_pattern) else {
        return 0;
    };
    let mut approx_len = 0.0_f64;
    let mut cur = ts::Point::zero();
    let mut start = ts::Point::zero();
    for seg in path.segments() {
        match seg {
            ts::PathSegment::MoveTo(p) => {
                cur = p;
                start = p;
            }
            ts::PathSegment::LineTo(p) => {
                approx_len += f64::from((p.x - cur.x).hypot(p.y - cur.y));
                cur = p;
            }
            ts::PathSegment::QuadTo(p1, p2) => {
                approx_len += f64::from((p1.x - cur.x).hypot(p1.y - cur.y))
                    + f64::from((p2.x - p1.x).hypot(p2.y - p1.y));
                cur = p2;
            }
            ts::PathSegment::CubicTo(p1, p2, p3) => {
                approx_len += f64::from((p1.x - cur.x).hypot(p1.y - cur.y))
                    + f64::from((p2.x - p1.x).hypot(p2.y - p1.y))
                    + f64::from((p3.x - p2.x).hypot(p3.y - p2.y));
                cur = p3;
            }
            ts::PathSegment::Close => {
                approx_len += f64::from((start.x - cur.x).hypot(start.y - cur.y));
                cur = start;
            }
        }
    }
    let est = (approx_len / dash_period_local).ceil();
    if !est.is_finite() || est <= 0.0 {
        0
    } else if est >= (usize::MAX as f64) {
        usize::MAX
    } else {
        est as usize
    }
}

/// Transforms a `tiny_skia::Path` into device coordinates using `f64` precision and snaps
/// coordinates to `1.0 / quant_div` pixels (`64.0` = `FDot6`, `256.0` = subpixel) so
/// integer tile translations produce zero `f32` ULP drift across tile seams.
fn transform_skia_path_f64(
    path: &ts::Path,
    transform: Affine2D,
    quant_div: f64,
) -> Option<ts::Path> {
    let q = quant_div.max(1.0);
    let xf_pt = |p: ts::Point| -> (f32, f32) {
        let d = transform.transform_point(Point2D::new(f64::from(p.x), f64::from(p.y)));
        let sx = (d.x * q).round() / q;
        let sy = (d.y * q).round() / q;
        (sx as f32, sy as f32)
    };
    let mut pb = ts::PathBuilder::new();
    let mut any = false;
    for seg in path.segments() {
        match seg {
            ts::PathSegment::MoveTo(p) => {
                let (x, y) = xf_pt(p);
                pb.move_to(x, y);
                any = true;
            }
            ts::PathSegment::LineTo(p) => {
                let (x, y) = xf_pt(p);
                pb.line_to(x, y);
                any = true;
            }
            ts::PathSegment::QuadTo(p1, p2) => {
                let (x1, y1) = xf_pt(p1);
                let (x2, y2) = xf_pt(p2);
                pb.quad_to(x1, y1, x2, y2);
                any = true;
            }
            ts::PathSegment::CubicTo(p1, p2, p3) => {
                let (x1, y1) = xf_pt(p1);
                let (x2, y2) = xf_pt(p2);
                let (x3, y3) = xf_pt(p3);
                pb.cubic_to(x1, y1, x2, y2, x3, y3);
                any = true;
            }
            ts::PathSegment::Close => {
                pb.close();
            }
        }
    }
    if any {
        pb.finish()
    } else {
        None
    }
}

// =============================================================================
// Backend Selection & GPU Stencil-then-Cover WgpuBackend (IMP-01)
// =============================================================================

/// Selectable 2D canvas rendering backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RenderBackendKind {
    /// Automatic selection (`TinySkia` deterministic CPU reference unless hardware GPU is requested).
    #[default]
    Auto,
    /// Deterministic pure-Rust CPU reference (`tiny-skia`).
    TinySkia,
    /// Optional GPU Stencil-then-Cover + WGSL backend (`wgpu`).
    Wgpu,
}

impl RenderBackendKind {
    /// Reads `COREE_RENDER_BACKEND` (`cpu|tinyskia`, `gpu|wgpu`, or `auto`).
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var("COREE_RENDER_BACKEND")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "cpu" | "tinyskia" | "tiny-skia" => Self::TinySkia,
            "gpu" | "wgpu" => Self::Wgpu,
            _ => Self::Auto,
        }
    }

    /// Parses a CLI/config string (`auto`, `tinyskia`/`cpu`, `wgpu`/`gpu`).
    #[must_use]
    pub fn parse_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "cpu" | "tinyskia" | "tiny-skia" => Some(Self::TinySkia),
            "gpu" | "wgpu" => Some(Self::Wgpu),
            _ => None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    pos: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuPaintUniform {
    viewport: [f32; 4], // [width, height, 0, 0]
    p0: [f32; 4],       // [x0, y0, x1, y1] or [fx, fy, cx, cy]
    p1: [f32; 4],       // [radius, angle_rad, opacity, shader_kind]
    solid_rgba: [f32; 4],
    pat_mat0: [f32; 4], // inverse pattern matrix row 0 + stop_count
    pat_mat1: [f32; 4], // inverse pattern matrix row 1 + blend_mode
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuGradientStop {
    color: [f32; 4],
    offset: f32,
    _pad: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuCompositeUniform {
    rect_px: [f32; 4], // [x, y, w, h]
    surface: [f32; 4], // [dst_w, dst_h, opacity, blend_mode_id]
    has_clip_uv: [f32; 4],
}

const WGSL_CANVAS_SHADER: &str = r#"
struct PaintUniform {
    viewport: vec4<f32>,
    p0: vec4<f32>,
    p1: vec4<f32>,
    solid_rgba: vec4<f32>,
    pat_mat0: vec4<f32>,
    pat_mat1: vec4<f32>,
};

struct GradientStop {
    color: vec4<f32>,
    offset: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

struct CompositeUniform {
    rect_px: vec4<f32>,
    surface: vec4<f32>,
    flags: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u_paint: PaintUniform;
@group(0) @binding(1) var<storage, read> u_stops: array<GradientStop>;
@group(0) @binding(2) var u_pat_tex: texture_2d<f32>;
@group(0) @binding(3) var u_pat_samp: sampler;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) dev_pos: vec2<f32>,
};

@vertex
fn vs_path(@location(0) pos: vec2<f32>) -> VsOut {
    var out: VsOut;
    let w = max(u_paint.viewport.x, 1.0);
    let h = max(u_paint.viewport.y, 1.0);
    let ndc_x = (pos.x / w) * 2.0 - 1.0;
    let ndc_y = 1.0 - (pos.y / h) * 2.0;
    out.clip_pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.dev_pos = pos;
    return out;
}

@vertex
fn vs_cover(@builtin(vertex_index) vid: u32) -> VsOut {
    var out: VsOut;
    let w = max(u_paint.viewport.x, 1.0);
    let h = max(u_paint.viewport.y, 1.0);
    var p = vec2<f32>(0.0, 0.0);
    if (vid == 1u) { p = vec2<f32>(w, 0.0); }
    else if (vid == 2u) { p = vec2<f32>(0.0, h); }
    else if (vid == 3u) { p = vec2<f32>(0.0, h); }
    else if (vid == 4u) { p = vec2<f32>(w, 0.0); }
    else if (vid == 5u) { p = vec2<f32>(w, h); }
    let ndc_x = (p.x / w) * 2.0 - 1.0;
    let ndc_y = 1.0 - (p.y / h) * 2.0;
    out.clip_pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.dev_pos = p;
    return out;
}

@fragment
fn fs_stencil() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
}

fn eval_stops(t_raw: f32) -> vec4<f32> {
    let count = u32(max(u_paint.pat_mat0.w, 1.0));
    let t = clamp(t_raw, 0.0, 1.0);
    if (count <= 1u) {
        return u_stops[0].color;
    }
    if (t <= u_stops[0].offset) {
        return u_stops[0].color;
    }
    for (var i: u32 = 1u; i < count; i = i + 1u) {
        let s0 = u_stops[i - 1u];
        let s1 = u_stops[i];
        if (t <= s1.offset) {
            let span = max(s1.offset - s0.offset, 1e-6);
            let f = clamp((t - s0.offset) / span, 0.0, 1.0);
            return mix(s0.color, s1.color, f);
        }
    }
    return u_stops[count - 1u].color;
}

@fragment
fn fs_paint(in: VsOut) -> @location(0) vec4<f32> {
    let kind = i32(u_paint.p1.w + 0.5);
    let op = clamp(u_paint.p1.z, 0.0, 1.0);
    var col = u_paint.solid_rgba;
    if (kind == 1) {
        let a = u_paint.p0.xy;
        let b = u_paint.p0.zw;
        let d = b - a;
        let denom = max(dot(d, d), 1e-9);
        let t = dot(in.dev_pos - a, d) / denom;
        col = eval_stops(t);
    } else if (kind == 2) {
        let center = u_paint.p0.zw;
        let r = max(u_paint.p1.x, 1e-6);
        let t = length(in.dev_pos - center) / r;
        col = eval_stops(t);
    } else if (kind == 3) {
        let center = u_paint.p0.zw;
        let d = in.dev_pos - center;
        var ang = atan2(d.y, d.x) - u_paint.p1.y;
        let tau = 6.28318530718;
        ang = ang - floor(ang / tau) * tau;
        col = eval_stops(ang / tau);
    } else if (kind == 4) {
        let uv = vec2<f32>(
            u_paint.pat_mat0.x * in.dev_pos.x + u_paint.pat_mat0.y * in.dev_pos.y + u_paint.pat_mat0.z,
            u_paint.pat_mat1.x * in.dev_pos.x + u_paint.pat_mat1.y * in.dev_pos.y + u_paint.pat_mat1.z
        );
        let tex_sz = vec2<f32>(textureDimensions(u_pat_tex));
        col = textureSample(u_pat_tex, u_pat_samp, uv / max(tex_sz, vec2<f32>(1.0)));
    }
    let a = clamp(col.a * op, 0.0, 1.0);
    return vec4<f32>(col.rgb * a, a);
}

@group(1) @binding(0) var<uniform> u_comp: CompositeUniform;
@group(1) @binding(1) var u_src_tex: texture_2d<f32>;
@group(1) @binding(2) var u_dst_tex: texture_2d<f32>;
@group(1) @binding(3) var u_clip_tex: texture_2d<f32>;
@group(1) @binding(4) var u_comp_samp: sampler;

struct QuadOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) src_uv: vec2<f32>,
    @location(1) dst_uv: vec2<f32>,
};

@vertex
fn vs_composite(@builtin(vertex_index) vid: u32) -> QuadOut {
    var out: QuadOut;
    let w = max(u_comp.surface.x, 1.0);
    let h = max(u_comp.surface.y, 1.0);
    let rx = u_comp.rect_px.x;
    let ry = u_comp.rect_px.y;
    let rw = u_comp.rect_px.z;
    let rh = u_comp.rect_px.w;
    var uv = vec2<f32>(0.0, 0.0);
    if (vid == 1u) { uv = vec2<f32>(1.0, 0.0); }
    else if (vid == 2u) { uv = vec2<f32>(0.0, 1.0); }
    else if (vid == 3u) { uv = vec2<f32>(0.0, 1.0); }
    else if (vid == 4u) { uv = vec2<f32>(1.0, 0.0); }
    else if (vid == 5u) { uv = vec2<f32>(1.0, 1.0); }
    let px = vec2<f32>(rx + uv.x * rw, ry + uv.y * rh);
    let ndc_x = (px.x / w) * 2.0 - 1.0;
    let ndc_y = 1.0 - (px.y / h) * 2.0;
    out.clip_pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.src_uv = uv;
    out.dst_uv = px / vec2<f32>(w, h);
    return out;
}

fn lum(c: vec3<f32>) -> f32 {
    return 0.3 * c.r + 0.59 * c.g + 0.11 * c.b;
}

fn clip_color(c_in: vec3<f32>) -> vec3<f32> {
    var c = c_in;
    let l = lum(c);
    let n = min(min(c.r, c.g), c.b);
    let x = max(max(c.r, c.g), c.b);
    if (n < 0.0) {
        c = l + (((c - l) * l) / max(l - n, 1e-6));
    }
    if (x > 1.0) {
        c = l + (((c - l) * (1.0 - l)) / max(x - l, 1e-6));
    }
    return c;
}

fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> {
    return clip_color(c + vec3<f32>(l - lum(c)));
}

fn sat(c: vec3<f32>) -> f32 {
    return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b);
}

fn set_sat(c_in: vec3<f32>, s: f32) -> vec3<f32> {
    let mn = min(min(c_in.r, c_in.g), c_in.b);
    let mx = max(max(c_in.r, c_in.g), c_in.b);
    let span = mx - mn;
    if (span <= 1e-6) {
        return vec3<f32>(0.0);
    }
    return ((c_in - vec3<f32>(mn)) / span) * s;
}

fn blend_separable(b: vec3<f32>, s: vec3<f32>, mode: i32) -> vec3<f32> {
    if (mode == 1) { return b * s; }
    if (mode == 2) { return b + s - b * s; }
    if (mode == 3) {
        return vec3<f32>(
            select(1.0 - 2.0 * (1.0 - b.r) * (1.0 - s.r), 2.0 * b.r * s.r, b.r <= 0.5),
            select(1.0 - 2.0 * (1.0 - b.g) * (1.0 - s.g), 2.0 * b.g * s.g, b.g <= 0.5),
            select(1.0 - 2.0 * (1.0 - b.b) * (1.0 - s.b), 2.0 * b.b * s.b, b.b <= 0.5)
        );
    }
    if (mode == 4) { return min(b, s); }
    if (mode == 5) { return max(b, s); }
    if (mode == 6) { return abs(b - s); }
    if (mode == 7) { return b + s - 2.0 * b * s; }
    if (mode == 8) {
        let d = sqrt(max(b, vec3<f32>(0.0)));
        return mix(
            b + (2.0 * s - 1.0) * (d - b),
            b - (1.0 - 2.0 * s) * b * (1.0 - b),
            vec3<f32>(f32(s.r <= 0.5), f32(s.g <= 0.5), f32(s.b <= 0.5))
        );
    }
    if (mode == 9) {
        return vec3<f32>(
            select(1.0 - 2.0 * (1.0 - b.r) * (1.0 - s.r), 2.0 * b.r * s.r, s.r <= 0.5),
            select(1.0 - 2.0 * (1.0 - b.g) * (1.0 - s.g), 2.0 * b.g * s.g, s.g <= 0.5),
            select(1.0 - 2.0 * (1.0 - b.b) * (1.0 - s.b), 2.0 * b.b * s.b, s.b <= 0.5)
        );
    }
    if (mode == 10) {
        return vec3<f32>(
            select(min(1.0, b.r / max(1.0 - s.r, 1e-6)), 0.0, b.r <= 1e-6),
            select(min(1.0, b.g / max(1.0 - s.g, 1e-6)), 0.0, b.g <= 1e-6),
            select(min(1.0, b.b / max(1.0 - s.b, 1e-6)), 0.0, b.b <= 1e-6)
        );
    }
    if (mode == 11) {
        return vec3<f32>(
            select(1.0 - min(1.0, (1.0 - b.r) / max(s.r, 1e-6)), 1.0, b.r >= 1.0 - 1e-6),
            select(1.0 - min(1.0, (1.0 - b.g) / max(s.g, 1e-6)), 1.0, b.g >= 1.0 - 1e-6),
            select(1.0 - min(1.0, (1.0 - b.b) / max(s.b, 1e-6)), 1.0, b.b >= 1.0 - 1e-6)
        );
    }
    if (mode == 12) { return set_lum(set_sat(s, sat(b)), lum(b)); }
    if (mode == 13) { return set_lum(set_sat(b, sat(s)), lum(b)); }
    if (mode == 14) { return set_lum(s, lum(b)); }
    if (mode == 15) { return set_lum(b, lum(s)); }
    return s;
}

@fragment
fn fs_composite(in: QuadOut) -> @location(0) vec4<f32> {
    var src = textureSample(u_src_tex, u_comp_samp, in.src_uv);
    let dst = textureSample(u_dst_tex, u_comp_samp, in.dst_uv);
    let clip_a = select(1.0, textureSample(u_clip_tex, u_comp_samp, in.dst_uv).r, u_comp.flags.x > 0.5);
    let scale = clamp(u_comp.surface.z * clip_a, 0.0, 1.0);
    src = src * scale;
    let sa = src.a;
    let da = dst.a;
    if (sa <= 1e-6) {
        return dst;
    }
    let mode = i32(u_comp.surface.w + 0.5);
    if (mode == 0) {
        return src + dst * (1.0 - sa);
    }
    let s_str = src.rgb / max(sa, 1e-6);
    let d_str = select(vec3<f32>(0.0), dst.rgb / max(da, 1e-6), da > 1e-6);
    let blended = blend_separable(d_str, s_str, mode);
    let rgb = (1.0 - da) * src.rgb + (1.0 - sa) * dst.rgb + sa * da * blended;
    let a = sa + da - sa * da;
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), clamp(a, 0.0, 1.0));
}
"#;

struct WgpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    is_gpu_raster: bool,
    msaa_samples: u32,
    paint_bgl: wgpu::BindGroupLayout,
    comp_bgl: wgpu::BindGroupLayout,
    stencil_winding_pipe: wgpu::RenderPipeline,
    stencil_evenodd_pipe: wgpu::RenderPipeline,
    cover_pipe: wgpu::RenderPipeline,
    composite_pipe: wgpu::RenderPipeline,
    nearest_sampler: wgpu::Sampler,
    linear_sampler: wgpu::Sampler,
    dummy_tex: wgpu::Texture,
    dummy_clip_tex: wgpu::Texture,
}

impl WgpuContext {
    fn get() -> Option<Arc<Self>> {
        static CTX: OnceLock<Option<Arc<WgpuContext>>> = OnceLock::new();
        CTX.get_or_init(Self::init).clone()
    }

    fn init() -> Option<Arc<Self>> {
        let enabled = wgpu::Instance::enabled_backend_features();
        if enabled.is_empty() {
            return None;
        }
        let gpu_backends = wgpu::Backends::VULKAN
            | wgpu::Backends::GL
            | wgpu::Backends::DX12
            | wgpu::Backends::METAL;
        let has_real_hal = enabled.intersects(gpu_backends);
        let desc = wgpu::InstanceDescriptor {
            backends: if has_real_hal {
                gpu_backends
            } else {
                wgpu::Backends::all()
            },
            backend_options: wgpu::BackendOptions {
                noop: wgpu::NoopBackendOptions {
                    enable: !has_real_hal,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        };
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .or_else(|_| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
                ..Default::default()
            }))
        })
        .ok()?;

        let adapter_info = adapter.get_info();
        let is_gpu_raster = adapter_info.backend != wgpu::Backend::Noop;
        let mut required_limits =
            wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());
        required_limits.max_texture_dimension_2d = required_limits
            .max_texture_dimension_2d
            .max(2048)
            .min(adapter.limits().max_texture_dimension_2d);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("coree-wgpu-device"),
            required_features: wgpu::Features::empty(),
            required_limits,
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::default(),
        }))
        .ok()?;

        let color_flags = adapter
            .get_texture_format_features(wgpu::TextureFormat::Rgba8Unorm)
            .flags;
        let depth_flags = adapter
            .get_texture_format_features(wgpu::TextureFormat::Depth24PlusStencil8)
            .flags;
        let msaa_samples = if is_gpu_raster
            && color_flags.contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4)
            && color_flags.contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE)
            && depth_flags.contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4)
        {
            4
        } else {
            1
        };

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("coree-canvas-wgsl"),
            source: wgpu::ShaderSource::Wgsl(WGSL_CANVAS_SHADER.into()),
        });

        let paint_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("coree-paint-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let comp_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("coree-comp-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let paint_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("coree-paint-pl"),
            bind_group_layouts: &[Some(&paint_bgl)],
            immediate_size: 0,
        });
        let comp_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("coree-comp-pl"),
            bind_group_layouts: &[None, Some(&comp_bgl)],
            immediate_size: 0,
        });

        let vtx_layout = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x2,
            }],
        })];

        let make_stencil_pipe = |label: &str, even_odd: bool| {
            let (front_pass, back_pass) = if even_odd {
                (
                    wgpu::StencilOperation::Invert,
                    wgpu::StencilOperation::Invert,
                )
            } else {
                (
                    wgpu::StencilOperation::IncrementWrap,
                    wgpu::StencilOperation::DecrementWrap,
                )
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&paint_pl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_path"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &vtx_layout,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_stencil"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::empty(),
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth24PlusStencil8,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: wgpu::StencilState {
                        front: wgpu::StencilFaceState {
                            compare: wgpu::CompareFunction::Always,
                            fail_op: wgpu::StencilOperation::Keep,
                            depth_fail_op: wgpu::StencilOperation::Keep,
                            pass_op: front_pass,
                        },
                        back: wgpu::StencilFaceState {
                            compare: wgpu::CompareFunction::Always,
                            fail_op: wgpu::StencilOperation::Keep,
                            depth_fail_op: wgpu::StencilOperation::Keep,
                            pass_op: back_pass,
                        },
                        read_mask: 0xff,
                        write_mask: 0xff,
                    },
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: msaa_samples,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };

        let stencil_winding_pipe = make_stencil_pipe("coree-stencil-winding", false);
        let stencil_evenodd_pipe = make_stencil_pipe("coree-stencil-evenodd", true);

        let cover_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("coree-cover"),
            layout: Some(&paint_pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_cover"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_paint"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState {
                    front: wgpu::StencilFaceState {
                        compare: wgpu::CompareFunction::NotEqual,
                        fail_op: wgpu::StencilOperation::Zero,
                        depth_fail_op: wgpu::StencilOperation::Zero,
                        pass_op: wgpu::StencilOperation::Zero,
                    },
                    back: wgpu::StencilFaceState {
                        compare: wgpu::CompareFunction::NotEqual,
                        fail_op: wgpu::StencilOperation::Zero,
                        depth_fail_op: wgpu::StencilOperation::Zero,
                        pass_op: wgpu::StencilOperation::Zero,
                    },
                    read_mask: 0xff,
                    write_mask: 0xff,
                },
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: msaa_samples,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        let composite_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("coree-composite"),
            layout: Some(&comp_pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_composite"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_composite"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let nearest_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("coree-nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            ..Default::default()
        });
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("coree-linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("coree-dummy-1x1"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let dummy_clip_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("coree-dummy-clip-1x1"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            dummy_clip_tex.as_image_copy(),
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );

        Some(Arc::new(Self {
            device,
            queue,
            is_gpu_raster,
            msaa_samples,
            paint_bgl,
            comp_bgl,
            stencil_winding_pipe,
            stencil_evenodd_pipe,
            cover_pipe,
            composite_pipe,
            nearest_sampler,
            linear_sampler,
            dummy_tex,
            dummy_clip_tex,
        }))
    }
}

struct WgpuGpuSurface {
    ctx: Arc<WgpuContext>,
    width: u32,
    height: u32,
    target_tex: wgpu::Texture,
    backdrop_tex: wgpu::Texture,
    shape_tex: wgpu::Texture,
    shape_msaa_tex: Option<wgpu::Texture>,
    stencil_tex: wgpu::Texture,
    clip_stack: Vec<wgpu::Texture>,
}

impl WgpuGpuSurface {
    fn new(ctx: Arc<WgpuContext>, width: u32, height: u32) -> Self {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let make_color_tex = |label: &str, samples: u32| {
            ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let target_tex = make_color_tex("coree-wgpu-target", 1);
        let backdrop_tex = make_color_tex("coree-wgpu-backdrop", 1);
        let shape_tex = make_color_tex("coree-wgpu-shape", 1);
        let shape_msaa_tex = if ctx.msaa_samples > 1 {
            Some(make_color_tex("coree-wgpu-shape-msaa", ctx.msaa_samples))
        } else {
            None
        };
        let stencil_tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("coree-wgpu-stencil"),
            size,
            mip_level_count: 1,
            sample_count: ctx.msaa_samples,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth24PlusStencil8,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        Self {
            ctx,
            width,
            height,
            target_tex,
            backdrop_tex,
            shape_tex,
            shape_msaa_tex,
            stencil_tex,
            clip_stack: Vec::new(),
        }
    }

    fn clear(&mut self, color: Color) {
        let [r, g, b, a] = color.to_rgba_f32();
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("coree-wgpu-clear"),
            });
        let view = self
            .target_tex
            .create_view(&wgpu::TextureViewDescriptor::default());
        {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("coree-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(r * a),
                            g: f64::from(g * a),
                            b: f64::from(b * a),
                            a: f64::from(a),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.ctx.queue.submit(Some(enc.finish()));
    }

    fn fill_path_gpu(
        &mut self,
        path: &BezierPath,
        paint: &BackendPaint,
        rule: FillRule,
        transform: Affine2D,
        flatten_tolerance: Option<f64>,
    ) {
        let tol = flatten_tolerance.unwrap_or(0.25).clamp(0.05, 4.0);
        let dev_path = path.transformed(&transform);
        let mut verts: Vec<GpuVertex> = Vec::new();
        for poly in dev_path.flatten(tol) {
            if poly.len() < 3 {
                continue;
            }
            let anchor = [poly[0].x as f32, poly[0].y as f32];
            for i in 1..(poly.len() - 1) {
                verts.push(GpuVertex { pos: anchor });
                verts.push(GpuVertex {
                    pos: [poly[i].x as f32, poly[i].y as f32],
                });
                verts.push(GpuVertex {
                    pos: [poly[i + 1].x as f32, poly[i + 1].y as f32],
                });
            }
        }
        if verts.is_empty() {
            return;
        }
        let vbuf = self
            .ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("coree-fan-vbuf"),
                contents: bytemuck::cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let opacity = sanitize_f32(paint.opacity, 1.0).clamp(0.0, 1.0);
        let (uniform, stops, pat_tex_opt) =
            build_gpu_paint(&self.ctx, &paint.shader, opacity, transform);
        let ubuf = self
            .ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("coree-paint-ubuf"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let sbuf = self
            .ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("coree-stops-sbuf"),
                contents: bytemuck::cast_slice(&stops),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let pat_tex = pat_tex_opt.as_ref().unwrap_or(&self.ctx.dummy_tex);
        let pat_view = pat_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let paint_bg = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("coree-paint-bg"),
                layout: &self.ctx.paint_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ubuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: sbuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&pat_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.ctx.linear_sampler),
                    },
                ],
            });

        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("coree-wgpu-fill"),
            });
        let shape_view = self
            .shape_tex
            .create_view(&wgpu::TextureViewDescriptor::default());
        let msaa_view = self
            .shape_msaa_tex
            .as_ref()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()));
        let stencil_view = self
            .stencil_tex
            .create_view(&wgpu::TextureViewDescriptor::default());
        {
            let (color_view, resolve_target) = match &msaa_view {
                Some(mv) => (mv, Some(&shape_view)),
                None => (&shape_view, None),
            };
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("coree-stencil-cover-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    depth_slice: None,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &stencil_view,
                    depth_ops: None,
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0),
                        store: wgpu::StoreOp::Store,
                    }),
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &paint_bg, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.set_pipeline(match rule {
                FillRule::Winding => &self.ctx.stencil_winding_pipe,
                FillRule::EvenOdd => &self.ctx.stencil_evenodd_pipe,
            });
            pass.draw(0..verts.len() as u32, 0..1);
            pass.set_pipeline(&self.ctx.cover_pipe);
            pass.draw(0..6, 0..1);
        }

        // Copy target -> backdrop and composite shape_tex -> target_tex
        enc.copy_texture_to_texture(
            self.target_tex.as_image_copy(),
            self.backdrop_tex.as_image_copy(),
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.encode_composite(
            &mut enc,
            &self.shape_tex,
            PixelRect::new(0, 0, self.width, self.height),
            1.0,
            paint.blend_mode,
        );
        self.ctx.queue.submit(Some(enc.finish()));
    }

    fn encode_composite(
        &self,
        enc: &mut wgpu::CommandEncoder,
        src_tex: &wgpu::Texture,
        rect: PixelRect,
        opacity: f32,
        blend_mode: BlendMode,
    ) {
        let cu = GpuCompositeUniform {
            rect_px: [
                rect.x as f32,
                rect.y as f32,
                rect.width as f32,
                rect.height as f32,
            ],
            surface: [
                self.width as f32,
                self.height as f32,
                opacity,
                blend_mode_id(blend_mode) as f32,
            ],
            has_clip_uv: [
                if self.clip_stack.is_empty() { 0.0 } else { 1.0 },
                0.0,
                0.0,
                0.0,
            ],
        };
        let ubuf = self
            .ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("coree-comp-ubuf"),
                contents: bytemuck::bytes_of(&cu),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let src_view = src_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = self
            .backdrop_tex
            .create_view(&wgpu::TextureViewDescriptor::default());
        let clip_tex = self.clip_stack.last().unwrap_or(&self.ctx.dummy_clip_tex);
        let clip_view = clip_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bg = self
            .ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("coree-comp-bg"),
                layout: &self.ctx.comp_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ubuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&dst_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&clip_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&self.ctx.nearest_sampler),
                    },
                ],
            });
        let target_view = self
            .target_tex
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("coree-comp-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.ctx.composite_pipe);
        pass.set_bind_group(1, &bg, &[]);
        pass.draw(0..6, 0..1);
    }

    fn readback_rect(&self, rect: PixelRect) -> RgbaBuffer {
        let Some(clamped) = rect.clamp_to_bounds(self.width, self.height) else {
            return RgbaBuffer::new_transparent(0, 0);
        };
        let row_bytes = clamped.width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_row = row_bytes.div_ceil(align) * align;
        let buf_size = u64::from(padded_row) * u64::from(clamped.height);
        let staging = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("coree-wgpu-readback"),
            size: buf_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("coree-wgpu-readback-enc"),
            });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target_tex,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: clamped.x as u32,
                    y: clamped.y as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(clamped.height),
                },
            },
            wgpu::Extent3d {
                width: clamped.width,
                height: clamped.height,
                depth_or_array_layers: 1,
            },
        );
        self.ctx.queue.submit(Some(enc.finish()));
        let slice = staging.slice(..);
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_cb = Arc::clone(&done);
        slice.map_async(wgpu::MapMode::Read, move |_| {
            done_cb.store(true, std::sync::atomic::Ordering::Release);
        });
        let _ = self.ctx.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        let Ok(view) = slice.get_mapped_range() else {
            return RgbaBuffer::new_transparent(clamped.width, clamped.height);
        };
        let mut rgba = Vec::with_capacity((clamped.width as usize) * (clamped.height as usize) * 4);
        for row in 0..(clamped.height as usize) {
            let row_slice = &view
                [row * (padded_row as usize)..row * (padded_row as usize) + (row_bytes as usize)];
            for px in row_slice.as_chunks::<4>().0 {
                let a = px[3];
                if a == 255 {
                    rgba.extend_from_slice(px);
                } else if a == 0 {
                    rgba.extend_from_slice(&[0, 0, 0, 0]);
                } else {
                    let af = f32::from(a) / 255.0;
                    rgba.push((f32::from(px[0]) / af).round().clamp(0.0, 255.0) as u8);
                    rgba.push((f32::from(px[1]) / af).round().clamp(0.0, 255.0) as u8);
                    rgba.push((f32::from(px[2]) / af).round().clamp(0.0, 255.0) as u8);
                    rgba.push(a);
                }
            }
        }
        drop(view);
        staging.unmap();
        RgbaBuffer {
            width: clamped.width,
            height: clamped.height,
            rgba,
        }
    }
}

fn blend_mode_id(mode: BlendMode) -> i32 {
    match mode {
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

fn build_gpu_paint(
    ctx: &WgpuContext,
    shader: &PaintShader,
    opacity: f32,
    transform: Affine2D,
) -> (GpuPaintUniform, Vec<GpuGradientStop>, Option<wgpu::Texture>) {
    let mut stops = vec![GpuGradientStop {
        color: [0.0; 4],
        offset: 0.0,
        _pad: [0.0; 3],
    }];
    let mut uniform = GpuPaintUniform {
        viewport: [1.0, 1.0, 0.0, 0.0],
        p0: [0.0; 4],
        p1: [0.0, 0.0, opacity, 0.0],
        solid_rgba: [0.0; 4],
        pat_mat0: [1.0, 0.0, 0.0, 1.0],
        pat_mat1: [0.0, 1.0, 0.0, 0.0],
    };
    let push_stops = |in_stops: &[GradientStop], out: &mut Vec<GpuGradientStop>| {
        out.clear();
        let mut sorted = in_stops.to_vec();
        sorted.sort_by(|a, b| a.offset.total_cmp(&b.offset));
        for s in sorted {
            out.push(GpuGradientStop {
                color: s.color.to_rgba_f32(),
                offset: sanitize_f32(s.offset, 0.0).clamp(0.0, 1.0),
                _pad: [0.0; 3],
            });
        }
        if out.is_empty() {
            out.push(GpuGradientStop {
                color: [0.0; 4],
                offset: 0.0,
                _pad: [0.0; 3],
            });
        }
    };
    let mut pat_tex = None;
    match shader {
        PaintShader::Solid(c) => {
            uniform.solid_rgba = c.to_rgba_f32();
            uniform.p1[3] = 0.0;
        }
        PaintShader::LinearGradient {
            start,
            end,
            stops: st,
        } => {
            let a = transform.transform_point(*start);
            let b = transform.transform_point(*end);
            uniform.p0 = [a.x as f32, a.y as f32, b.x as f32, b.y as f32];
            uniform.p1[3] = 1.0;
            push_stops(st, &mut stops);
            uniform.pat_mat0[3] = stops.len() as f32;
        }
        PaintShader::RadialGradient {
            focal,
            center,
            radius,
            stops: st,
        } => {
            let f = transform.transform_point(*focal);
            let c = transform.transform_point(*center);
            uniform.p0 = [f.x as f32, f.y as f32, c.x as f32, c.y as f32];
            uniform.p1[0] = (radius * estimate_scale_2d(transform)) as f32;
            uniform.p1[3] = 2.0;
            push_stops(st, &mut stops);
            uniform.pat_mat0[3] = stops.len() as f32;
        }
        PaintShader::SweepGradient {
            center,
            angle_deg,
            stops: st,
        } => {
            let c = transform.transform_point(*center);
            uniform.p0 = [c.x as f32, c.y as f32, c.x as f32, c.y as f32];
            uniform.p1[1] = sanitize_f64(*angle_deg, 0.0).to_radians() as f32;
            uniform.p1[3] = 3.0;
            push_stops(st, &mut stops);
            uniform.pat_mat0[3] = stops.len() as f32;
        }
        PaintShader::PixmapPattern {
            tile,
            tile_transform,
        } => {
            let inv = transform
                .compose(tile_transform)
                .inverse()
                .unwrap_or(Affine2D::IDENTITY);
            let [a, b, c, d, tx, ty] = inv.m;
            uniform.pat_mat0 = [a as f32, c as f32, tx as f32, 1.0];
            uniform.pat_mat1 = [b as f32, d as f32, ty as f32, 0.0];
            uniform.p1[3] = 4.0;
            if !tile.is_empty() {
                let tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("coree-pattern-tex"),
                    size: wgpu::Extent3d {
                        width: tile.width,
                        height: tile.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                ctx.queue.write_texture(
                    tex.as_image_copy(),
                    &tile.rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(tile.width * 4),
                        rows_per_image: Some(tile.height),
                    },
                    wgpu::Extent3d {
                        width: tile.width,
                        height: tile.height,
                        depth_or_array_layers: 1,
                    },
                );
                pat_tex = Some(tex);
            }
        }
    }
    (uniform, stops, pat_tex)
}

/// Optional GPU Stencil-then-Cover + WGSL implementation of [`CanvasBackend`] powered by `wgpu`.
///
/// Uses a shared `OnceLock` [`WgpuContext`] (hardware GPU -> software fallback adapter ->
/// deterministic [`TinySkiaBackend`] reference fallback when no GPU HAL adapter is active).
pub struct WgpuBackend {
    width: u32,
    height: u32,
    ctx: Option<Arc<WgpuContext>>,
    gpu: Option<WgpuGpuSurface>,
    fallback: TinySkiaBackend,
    flatten_tolerance: Option<f64>,
}

impl WgpuBackend {
    /// Allocates a new `width x height` `wgpu` rendering surface (falling back gracefully to
    /// `TinySkiaBackend` if the host/build has no active GPU HAL adapter).
    pub fn new(width: u32, height: u32) -> RenderResult<Self> {
        validate_surface_dimensions(width, height)?;
        let fallback = TinySkiaBackend::new(width, height)?;
        let ctx = WgpuContext::get();
        let gpu = ctx
            .as_ref()
            .filter(|c| c.is_gpu_raster)
            .map(|c| WgpuGpuSurface::new(Arc::clone(c), width, height));
        Ok(Self {
            width,
            height,
            ctx,
            gpu,
            fallback,
            flatten_tolerance: None,
        })
    }

    /// Returns `true` when rendering executes directly on a non-`Noop` `wgpu` GPU adapter.
    #[must_use]
    pub fn is_gpu_active(&self) -> bool {
        self.gpu.is_some()
    }

    /// Returns `true` when `wgpu` initialized and compiled/validated the WGSL shader pipelines.
    #[must_use]
    pub fn is_wgpu_initialized(&self) -> bool {
        self.ctx.is_some()
    }
}

impl CanvasBackend for WgpuBackend {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn clear(&mut self, color: Color) {
        if let Some(ref mut gpu) = self.gpu {
            gpu.clear(color);
        } else {
            self.fallback.clear(color);
        }
    }

    fn set_flatten_tolerance(&mut self, tolerance: Option<f64>) {
        self.flatten_tolerance = tolerance.filter(|t| t.is_finite() && *t > 0.0);
        self.fallback.set_flatten_tolerance(tolerance);
    }

    fn fill_path(
        &mut self,
        path: &BezierPath,
        paint: &BackendPaint,
        rule: FillRule,
        transform: Affine2D,
    ) {
        if let Some(ref mut gpu) = self.gpu {
            gpu.fill_path_gpu(path, paint, rule, transform, self.flatten_tolerance);
        } else {
            self.fallback.fill_path(path, paint, rule, transform);
        }
    }

    fn stroke_path(&mut self, path: &BezierPath, stroke: &StrokeParams, transform: Affine2D) {
        if let Some(ref mut gpu) = self.gpu {
            let Some(ts_path) = bezier_to_skia_path(path, self.flatten_tolerance) else {
                return;
            };
            let scale = estimate_scale_2d(transform);
            let res_scale = (scale as f32).clamp(0.1, 64.0);
            let dash = build_stroke_dash(&stroke.dash_pattern, stroke.dash_offset);
            let dashed = dash.as_ref().and_then(|d| ts_path.dash(d, res_scale));
            let src = dashed.as_ref().unwrap_or(&ts_path);
            let eff_px = stroke.width * (scale as f32);
            let (w, op) = if eff_px < 1.0 {
                (
                    ((1.0 / scale.max(1e-6)) as f32).max(stroke.width),
                    (stroke.opacity * eff_px).clamp(0.0, 1.0),
                )
            } else {
                (stroke.width, stroke.opacity)
            };
            let ts_stroke = ts::Stroke {
                width: w,
                miter_limit: stroke.miter_limit.max(1.0),
                line_cap: match stroke.cap {
                    LineCap::Butt => ts::LineCap::Butt,
                    LineCap::Round => ts::LineCap::Round,
                    LineCap::Square => ts::LineCap::Square,
                },
                line_join: match stroke.join {
                    LineJoin::Miter => ts::LineJoin::Miter,
                    LineJoin::Round => ts::LineJoin::Round,
                    LineJoin::Bevel => ts::LineJoin::Bevel,
                },
                dash: None,
            };
            if let Some(stroked) = src.stroke(&ts_stroke, res_scale) {
                let mut bp = BezierPath::new();
                let mut cur_sp = coree_core::Subpath::new(false);
                for seg in stroked.segments() {
                    match seg {
                        ts::PathSegment::MoveTo(p) => {
                            if !cur_sp.nodes.is_empty() {
                                bp.subpaths.push(cur_sp);
                            }
                            cur_sp = coree_core::Subpath::new(false);
                            cur_sp.nodes.push(coree_core::PathNode::new(
                                Point2D::new(f64::from(p.x), f64::from(p.y)),
                                None,
                                None,
                                coree_core::NodeKind::Cusp,
                            ));
                        }
                        ts::PathSegment::LineTo(p) => {
                            cur_sp.nodes.push(coree_core::PathNode::new(
                                Point2D::new(f64::from(p.x), f64::from(p.y)),
                                None,
                                None,
                                coree_core::NodeKind::Cusp,
                            ));
                        }
                        ts::PathSegment::QuadTo(p1, p2) => {
                            let prev = cur_sp.nodes.last().map_or(Point2D::ZERO, |n| n.pos);
                            let q1 = Point2D::new(f64::from(p1.x), f64::from(p1.y));
                            let q2 = Point2D::new(f64::from(p2.x), f64::from(p2.y));
                            if let Some(last) = cur_sp.nodes.last_mut() {
                                last.handle_out = Some(prev.lerp(q1, 2.0 / 3.0));
                            }
                            let n = coree_core::PathNode::new(
                                q2,
                                Some(q2.lerp(q1, 2.0 / 3.0)),
                                None,
                                coree_core::NodeKind::Cusp,
                            );
                            cur_sp.nodes.push(n);
                        }
                        ts::PathSegment::CubicTo(p1, p2, p3) => {
                            if let Some(last) = cur_sp.nodes.last_mut() {
                                last.handle_out =
                                    Some(Point2D::new(f64::from(p1.x), f64::from(p1.y)));
                            }
                            let n = coree_core::PathNode::new(
                                Point2D::new(f64::from(p3.x), f64::from(p3.y)),
                                Some(Point2D::new(f64::from(p2.x), f64::from(p2.y))),
                                None,
                                coree_core::NodeKind::Cusp,
                            );
                            cur_sp.nodes.push(n);
                        }
                        ts::PathSegment::Close => {
                            cur_sp.closed = true;
                        }
                    }
                }
                if !cur_sp.nodes.is_empty() {
                    bp.subpaths.push(cur_sp);
                }
                let paint = BackendPaint {
                    shader: PaintShader::Solid(stroke.color.clone()),
                    opacity: op,
                    blend_mode: stroke.blend_mode,
                    anti_alias: stroke.anti_alias,
                };
                gpu.fill_path_gpu(
                    &bp,
                    &paint,
                    FillRule::Winding,
                    transform,
                    self.flatten_tolerance,
                );
            }
        } else {
            self.fallback.stroke_path(path, stroke, transform);
        }
    }

    fn draw_pixmap(
        &mut self,
        image: &RgbaBuffer,
        transform: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        smooth: bool,
    ) {
        if let Some(ref mut gpu) = self.gpu {
            if image.is_empty() || opacity <= 1e-6 {
                return;
            }
            let paint = BackendPaint {
                shader: PaintShader::PixmapPattern {
                    tile: image.clone(),
                    tile_transform: Affine2D::IDENTITY,
                },
                opacity,
                blend_mode,
                anti_alias: smooth,
            };
            let rect = BezierPath::from_rect(Rect2D::from_xywh(
                0.0,
                0.0,
                f64::from(image.width),
                f64::from(image.height),
            ));
            gpu.fill_path_gpu(&rect, &paint, FillRule::Winding, transform, None);
        } else {
            self.fallback
                .draw_pixmap(image, transform, opacity, blend_mode, smooth);
        }
    }

    fn push_clip(&mut self, path: &BezierPath, rule: FillRule, transform: Affine2D) {
        if let Some(ref mut gpu) = self.gpu {
            let tex = gpu.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("coree-wgpu-clip"),
                size: wgpu::Extent3d {
                    width: gpu.width,
                    height: gpu.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            if let Ok(mut mc) = TinySkiaBackend::new(gpu.width, gpu.height) {
                mc.fill_path(path, &BackendPaint::solid(Color::WHITE), rule, transform);
                let buf = mc.into_rgba();
                gpu.ctx.queue.write_texture(
                    tex.as_image_copy(),
                    &buf.rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(gpu.width * 4),
                        rows_per_image: Some(gpu.height),
                    },
                    wgpu::Extent3d {
                        width: gpu.width,
                        height: gpu.height,
                        depth_or_array_layers: 1,
                    },
                );
            }
            gpu.clip_stack.push(tex);
        } else {
            self.fallback.push_clip(path, rule, transform);
        }
    }

    fn pop_clip(&mut self) {
        if let Some(ref mut gpu) = self.gpu {
            gpu.clip_stack.pop();
        } else {
            self.fallback.pop_clip();
        }
    }

    fn read_region(&self, rect: PixelRect) -> RgbaBuffer {
        if let Some(ref gpu) = self.gpu {
            gpu.readback_rect(rect)
        } else {
            self.fallback.read_region(rect)
        }
    }

    fn composite_region(
        &mut self,
        rect: PixelRect,
        src: &RgbaBuffer,
        opacity: f32,
        blend_mode: BlendMode,
    ) {
        if let Some(ref mut gpu) = self.gpu {
            if src.is_empty() || rect.is_empty() || opacity <= 1e-6 {
                return;
            }
            let Some(pm) = src.to_premultiplied_pixmap() else {
                return;
            };
            let tex = gpu.ctx.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("coree-comp-src"),
                size: wgpu::Extent3d {
                    width: src.width,
                    height: src.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            gpu.ctx.queue.write_texture(
                tex.as_image_copy(),
                pm.data(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(src.width * 4),
                    rows_per_image: Some(src.height),
                },
                wgpu::Extent3d {
                    width: src.width,
                    height: src.height,
                    depth_or_array_layers: 1,
                },
            );
            let mut enc = gpu
                .ctx
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("coree-comp-region"),
                });
            enc.copy_texture_to_texture(
                gpu.target_tex.as_image_copy(),
                gpu.backdrop_tex.as_image_copy(),
                wgpu::Extent3d {
                    width: gpu.width,
                    height: gpu.height,
                    depth_or_array_layers: 1,
                },
            );
            gpu.encode_composite(&mut enc, &tex, rect, opacity, blend_mode);
            gpu.ctx.queue.submit(Some(enc.finish()));
        } else {
            self.fallback
                .composite_region(rect, src, opacity, blend_mode);
        }
    }

    fn snapshot_rgba(&self) -> RgbaBuffer {
        if let Some(ref gpu) = self.gpu {
            gpu.readback_rect(PixelRect::new(0, 0, gpu.width, gpu.height))
        } else {
            self.fallback.snapshot_rgba()
        }
    }

    fn into_rgba(self) -> RgbaBuffer {
        if let Some(gpu) = self.gpu {
            gpu.readback_rect(PixelRect::new(0, 0, gpu.width, gpu.height))
        } else {
            self.fallback.into_rgba()
        }
    }
}

/// Polymorphic backend wrapper selecting either [`TinySkiaBackend`] or [`WgpuBackend`].
pub enum RenderBackend {
    TinySkia(TinySkiaBackend),
    Wgpu(Box<WgpuBackend>),
}

impl RenderBackend {
    /// Creates a rendering surface for `kind`.
    pub fn new_for_kind(kind: RenderBackendKind, width: u32, height: u32) -> RenderResult<Self> {
        match kind {
            RenderBackendKind::TinySkia => Ok(Self::TinySkia(TinySkiaBackend::new(width, height)?)),
            RenderBackendKind::Wgpu => Ok(Self::Wgpu(Box::new(WgpuBackend::new(width, height)?))),
            RenderBackendKind::Auto => {
                if let Some(ctx) = WgpuContext::get() {
                    if ctx.is_gpu_raster {
                        return Ok(Self::Wgpu(Box::new(WgpuBackend::new(width, height)?)));
                    }
                }
                Ok(Self::TinySkia(TinySkiaBackend::new(width, height)?))
            }
        }
    }
}

impl CanvasBackend for RenderBackend {
    fn width(&self) -> u32 {
        match self {
            Self::TinySkia(b) => b.width(),
            Self::Wgpu(b) => b.width(),
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::TinySkia(b) => b.height(),
            Self::Wgpu(b) => b.height(),
        }
    }

    fn clear(&mut self, color: Color) {
        match self {
            Self::TinySkia(b) => b.clear(color),
            Self::Wgpu(b) => b.clear(color),
        }
    }

    fn set_flatten_tolerance(&mut self, tolerance: Option<f64>) {
        match self {
            Self::TinySkia(b) => b.set_flatten_tolerance(tolerance),
            Self::Wgpu(b) => b.set_flatten_tolerance(tolerance),
        }
    }

    fn fill_path(
        &mut self,
        path: &BezierPath,
        paint: &BackendPaint,
        rule: FillRule,
        transform: Affine2D,
    ) {
        match self {
            Self::TinySkia(b) => b.fill_path(path, paint, rule, transform),
            Self::Wgpu(b) => b.fill_path(path, paint, rule, transform),
        }
    }

    fn stroke_path(&mut self, path: &BezierPath, stroke: &StrokeParams, transform: Affine2D) {
        match self {
            Self::TinySkia(b) => b.stroke_path(path, stroke, transform),
            Self::Wgpu(b) => b.stroke_path(path, stroke, transform),
        }
    }

    fn draw_pixmap(
        &mut self,
        image: &RgbaBuffer,
        transform: Affine2D,
        opacity: f32,
        blend_mode: BlendMode,
        smooth: bool,
    ) {
        match self {
            Self::TinySkia(b) => b.draw_pixmap(image, transform, opacity, blend_mode, smooth),
            Self::Wgpu(b) => b.draw_pixmap(image, transform, opacity, blend_mode, smooth),
        }
    }

    fn push_clip(&mut self, path: &BezierPath, rule: FillRule, transform: Affine2D) {
        match self {
            Self::TinySkia(b) => b.push_clip(path, rule, transform),
            Self::Wgpu(b) => b.push_clip(path, rule, transform),
        }
    }

    fn pop_clip(&mut self) {
        match self {
            Self::TinySkia(b) => b.pop_clip(),
            Self::Wgpu(b) => b.pop_clip(),
        }
    }

    fn read_region(&self, rect: PixelRect) -> RgbaBuffer {
        match self {
            Self::TinySkia(b) => b.read_region(rect),
            Self::Wgpu(b) => b.read_region(rect),
        }
    }

    fn composite_region(
        &mut self,
        rect: PixelRect,
        src: &RgbaBuffer,
        opacity: f32,
        blend_mode: BlendMode,
    ) {
        match self {
            Self::TinySkia(b) => b.composite_region(rect, src, opacity, blend_mode),
            Self::Wgpu(b) => b.composite_region(rect, src, opacity, blend_mode),
        }
    }

    fn snapshot_rgba(&self) -> RgbaBuffer {
        match self {
            Self::TinySkia(b) => b.snapshot_rgba(),
            Self::Wgpu(b) => b.snapshot_rgba(),
        }
    }

    fn into_rgba(self) -> RgbaBuffer {
        match self {
            Self::TinySkia(b) => b.into_rgba(),
            Self::Wgpu(b) => (*b).into_rgba(),
        }
    }
}

#[cfg(test)]
mod wgpu_tests {
    use super::*;

    #[test]
    fn wgpu_backend_initializes_validates_wgsl_and_matches_tinyskia_parity() {
        let mut cpu = TinySkiaBackend::new(128, 128).unwrap();
        let mut gpu = WgpuBackend::new(128, 128).unwrap();
        assert!(
            gpu.is_wgpu_initialized(),
            "WgpuContext must initialize and validate WGSL shaders"
        );

        cpu.clear(Color::WHITE);
        gpu.clear(Color::WHITE);

        let rect = BezierPath::from_rect(Rect2D::from_xywh(16.0, 16.0, 80.0, 64.0));
        let grad = BackendPaint {
            shader: PaintShader::LinearGradient {
                start: Point2D::new(16.0, 16.0),
                end: Point2D::new(96.0, 80.0),
                stops: vec![
                    GradientStop::new(0.0, Color::rgba8(220, 40, 60, 255)),
                    GradientStop::new(1.0, Color::rgba8(20, 120, 240, 255)),
                ],
            },
            opacity: 0.9,
            blend_mode: BlendMode::Multiply,
            anti_alias: true,
        };
        cpu.fill_path(&rect, &grad, FillRule::Winding, Affine2D::IDENTITY);
        gpu.fill_path(&rect, &grad, FillRule::Winding, Affine2D::IDENTITY);

        let stroke = StrokeParams {
            color: Color::rgba8(10, 20, 30, 255),
            width: 3.0,
            cap: LineCap::Round,
            join: LineJoin::Round,
            miter_limit: 4.0,
            dash_pattern: vec![8.0, 4.0],
            dash_offset: 1.0,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            anti_alias: true,
        };
        cpu.stroke_path(&rect, &stroke, Affine2D::IDENTITY);
        gpu.stroke_path(&rect, &stroke, Affine2D::IDENTITY);

        let cpu_buf = cpu.into_rgba();
        let gpu_buf = gpu.into_rgba();
        assert_eq!(cpu_buf.width, gpu_buf.width);
        assert_eq!(cpu_buf.height, gpu_buf.height);

        let total: f64 = cpu_buf
            .rgba
            .iter()
            .zip(gpu_buf.rgba.iter())
            .map(|(&a, &b)| (f64::from(a) - f64::from(b)).abs())
            .sum();
        let mean_diff = total / (cpu_buf.rgba.len() as f64);
        assert!(mean_diff < 2.0, "CPU vs GPU mean_abs_diff={mean_diff}");
    }

    #[test]
    fn wgpu_backend_blend_modes_clips_pixmaps_and_regions() {
        let mut gpu = WgpuBackend::new(64, 64).unwrap();
        gpu.clear(Color::rgba8(240, 240, 240, 255));

        let clip_rect = BezierPath::from_rect(Rect2D::from_xywh(8.0, 8.0, 48.0, 48.0));
        gpu.push_clip(&clip_rect, FillRule::EvenOdd, Affine2D::IDENTITY);

        let pat_tile = RgbaBuffer::new_filled(4, 4, [200, 80, 40, 255]);
        let full = BezierPath::from_rect(Rect2D::from_xywh(0.0, 0.0, 64.0, 64.0));
        gpu.fill_path(
            &full,
            &BackendPaint {
                shader: PaintShader::PixmapPattern {
                    tile: pat_tile,
                    tile_transform: Affine2D::IDENTITY,
                },
                opacity: 1.0,
                blend_mode: BlendMode::Screen,
                anti_alias: true,
            },
            FillRule::Winding,
            Affine2D::IDENTITY,
        );
        gpu.pop_clip();

        let sub = gpu.read_region(PixelRect::new(12, 12, 8, 8));
        assert_eq!(sub.width, 8);
        assert_ne!(sub.get_pixel(0, 0).unwrap(), [240, 240, 240, 255]);
        assert_eq!(
            gpu.snapshot_rgba().get_pixel(2, 2).unwrap(),
            [240, 240, 240, 255]
        );

        let patch = RgbaBuffer::new_filled(8, 8, [10, 200, 90, 255]);
        gpu.composite_region(PixelRect::new(2, 2, 8, 8), &patch, 0.75, BlendMode::Overlay);
        assert_ne!(
            gpu.into_rgba().get_pixel(3, 3).unwrap(),
            [240, 240, 240, 255]
        );
    }

    #[test]
    fn estimate_dash_count_uses_local_dash_period_and_guards_extremes() {
        let mut bp = BezierPath::new();
        let extent = 297.0 * 4.0;
        for idx in 0..8 {
            let y = 20.0 + f64::from(idx) * 20.0;
            bp.subpaths.push(coree_core::Subpath::from_points(
                [Point2D::new(-extent, y), Point2D::new(extent, y)],
                false,
            ));
        }
        let ts_path = bezier_to_skia_path(&bp, None).unwrap();

        let hairline = (25.4_f32) / 96.0;
        let a4_pattern = [hairline * 6.0, hairline * 3.0];
        let a4_est = estimate_dash_count(&ts_path, &a4_pattern);
        assert!(
            (7_900..=8_100).contains(&a4_est),
            "8 A4 guidelines should estimate ~7,983 dashes, got {a4_est}"
        );
        assert!(a4_est <= MAX_LOCAL_DASH_SEGMENTS);

        let single = bezier_to_skia_path(
            &BezierPath::from_subpath(coree_core::Subpath::from_points(
                [Point2D::new(0.0, 0.0), Point2D::new(100.0, 0.0)],
                false,
            )),
            None,
        )
        .unwrap();
        assert_eq!(estimate_dash_count(&single, &[5.0]), 10);
        assert_eq!(estimate_dash_count(&single, &[]), 0);
        assert_eq!(estimate_dash_count(&single, &[0.0, -2.0, f32::NAN]), 0);

        let huge = bezier_to_skia_path(
            &BezierPath::from_subpath(coree_core::Subpath::from_points(
                [Point2D::new(0.0, 0.0), Point2D::new(10_000.0, 0.0)],
                false,
            )),
            None,
        )
        .unwrap();
        assert!(
            estimate_dash_count(&huge, &[1e-5, 1e-5]) > MAX_LOCAL_DASH_SEGMENTS,
            "Pathological sub-micron dash density must remain guarded"
        );
    }
}
