// SPDX-License-Identifier: MPL-2.0

//! Backend-agnostic 2D canvas rendering trait ([`CanvasBackend`]) and its Pure-Rust
//! CPU reference implementation ([`TinySkiaBackend`]).
//!
//! Keeping all scene rendering, lens compositing, effects, and text pipelines
//! strictly against [`CanvasBackend`] allows future GPU backends (`vello` / `wgpu`)
//! to drop in without modifying `coree-core` or `coree-app`.

use crate::error::{RenderError, RenderResult};
use coree_core::math::{sanitize_f32, sanitize_f64};
use coree_core::{
    Affine2D, BezierPath, BlendMode, Color, GradientStop, LineCap, LineJoin, OutlineStyle, Point2D,
    Rect2D,
};
use tiny_skia as ts;

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
        if stroke.width <= 1e-6 || stroke.opacity <= 1e-6 {
            return;
        }
        let Some(ts_path) = bezier_to_skia_path(path, self.flatten_tolerance) else {
            return;
        };
        let ts_xf = affine_to_skia(transform);
        let mut ts_paint = ts::Paint {
            anti_alias: stroke.anti_alias,
            blend_mode: blend_mode_to_skia(stroke.blend_mode),
            ..ts::Paint::default()
        };
        ts_paint.set_color(color_to_skia(&stroke.color, stroke.opacity));

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
        if opacity <= 1e-6 || image.is_empty() {
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
        if needs_f64_device_clip(path, transform, w, h) {
            if let Some(ts_path) =
                build_clipped_device_skia_path(path, transform, w, h, self.flatten_tolerance)
            {
                mask.fill_path(&ts_path, ts_rule, true, ts::Transform::identity());
            }
        } else if let Some(ts_path) = bezier_to_skia_path(path, self.flatten_tolerance) {
            mask.fill_path(&ts_path, ts_rule, true, affine_to_skia(transform));
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
