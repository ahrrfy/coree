// SPDX-License-Identifier: MPL-2.0

//! Canvas viewport camera model (`pan`, `zoom`, `rotation`, `DPI`, `LOD`, `Multipage View`)
//! and display [`ViewMode`] settings.

use coree_core::math::{sanitize_f64, DEFAULT_DPI, MM_PER_INCH};
use coree_core::{Affine2D, Point2D, Rect2D};

/// Canvas rendering fidelity mode matching professional illustration workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ViewMode {
    /// Vector skeletons and hairline outlines only (no fills or bitmaps).
    Wireframe,
    /// Standard anti-aliased vector rendering.
    Normal,
    /// Full high-precision anti-aliased rendering with all live lenses, meshes, and effects.
    #[default]
    Enhanced,
    /// Pixel-grid preview showing true device/document rasterization boundaries.
    Pixels,
}

/// Interactive camera mapping canonical canvas millimeters to viewport screen pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// World millimeter coordinate anchored at the viewport top-left before rotation.
    pub pan_mm: Point2D,
    /// Interactive zoom factor (`1.0` = 100% at `dpi`).
    pub zoom: f64,
    /// Canvas view rotation in degrees around the viewport center.
    pub rotation_deg: f64,
    /// Viewport pixel width.
    pub width_px: u32,
    /// Viewport pixel height.
    pub height_px: u32,
    /// Display or target surface DPI (default `96.0`).
    pub dpi: f64,
    /// When `true`, lowers Bézier curve flattening resolution for smooth 60fps interaction.
    pub lod_dragging: bool,
    /// When `true`, renders all document pages side-by-side in Multipage View.
    pub multipage_view: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new(800, 600)
    }
}

impl Viewport {
    /// Creates a default `100%` zoom viewport at `96 DPI`.
    #[must_use]
    pub fn new(width_px: u32, height_px: u32) -> Self {
        Self {
            pan_mm: Point2D::ZERO,
            zoom: 1.0,
            rotation_deg: 0.0,
            width_px: width_px.max(1),
            height_px: height_px.max(1),
            dpi: DEFAULT_DPI,
            lod_dragging: false,
            multipage_view: false,
        }
    }

    /// Creates a viewport tailored to export a millimeter rectangle `[0..w_mm, 0..h_mm]` at `dpi`.
    #[must_use]
    pub fn for_export(width_px: u32, height_px: u32, dpi: f64) -> Self {
        Self {
            pan_mm: Point2D::ZERO,
            zoom: 1.0,
            rotation_deg: 0.0,
            width_px: width_px.max(1),
            height_px: height_px.max(1),
            dpi: sanitize_f64(dpi, DEFAULT_DPI).max(1.0),
            lod_dragging: false,
            multipage_view: false,
        }
    }

    /// Screen pixels per canonical world millimeter (`zoom * dpi / 25.4`).
    #[must_use]
    pub fn px_per_mm(&self) -> f64 {
        let z = sanitize_f64(self.zoom, 1.0).clamp(0.01, 256.0);
        let d = sanitize_f64(self.dpi, DEFAULT_DPI).clamp(1.0, 4800.0);
        z * d / MM_PER_INCH
    }

    /// Affine transform mapping canonical world millimeters -> viewport screen pixels.
    #[must_use]
    pub fn world_to_screen_transform(&self) -> Affine2D {
        let s = self.px_per_mm();
        let pan = self.pan_mm.sanitized();
        let base = Affine2D::scale_uniform(s).compose(&Affine2D::translate(-pan.x, -pan.y));
        let rot = sanitize_f64(self.rotation_deg, 0.0);
        if rot.abs() <= 1e-9 {
            base
        } else {
            let center = Point2D::new(
                f64::from(self.width_px) * 0.5,
                f64::from(self.height_px) * 0.5,
            );
            Affine2D::rotate_about(rot.to_radians(), center).compose(&base)
        }
    }

    /// Affine transform mapping viewport screen pixels -> canonical world millimeters.
    #[must_use]
    pub fn screen_to_world_transform(&self) -> Affine2D {
        self.world_to_screen_transform()
            .inverse()
            .unwrap_or(Affine2D::IDENTITY)
    }

    /// Projects a world millimeter point into viewport pixel coordinates.
    #[must_use]
    pub fn world_to_screen(&self, pt_mm: Point2D) -> Point2D {
        self.world_to_screen_transform().transform_point(pt_mm)
    }

    /// Unprojects a viewport pixel coordinate into world millimeters.
    #[must_use]
    pub fn screen_to_world(&self, pt_px: Point2D) -> Point2D {
        self.screen_to_world_transform().transform_point(pt_px)
    }

    /// Axis-aligned world millimeter bounding box currently visible in the viewport.
    #[must_use]
    pub fn visible_world_rect(&self) -> Rect2D {
        let screen_rect = Rect2D::from_xywh(
            0.0,
            0.0,
            f64::from(self.width_px),
            f64::from(self.height_px),
        );
        self.screen_to_world_transform().transform_rect(screen_rect)
    }

    /// Adjusts `zoom` and `pan_mm` to fit `world_rect_mm` cleanly inside the viewport with `margin_px`.
    pub fn fit_rect(&mut self, world_rect_mm: Rect2D, margin_px: f64) {
        if world_rect_mm.is_empty() {
            return;
        }
        let m = sanitize_f64(margin_px, 0.0).max(0.0);
        let avail_w = (f64::from(self.width_px) - 2.0 * m).max(1.0);
        let avail_h = (f64::from(self.height_px) - 2.0 * m).max(1.0);
        let base_px_per_mm = sanitize_f64(self.dpi, DEFAULT_DPI).max(1.0) / MM_PER_INCH;
        let zx = avail_w / (world_rect_mm.width().max(1e-3) * base_px_per_mm);
        let zy = avail_h / (world_rect_mm.height().max(1e-3) * base_px_per_mm);
        self.zoom = zx.min(zy).clamp(0.01, 256.0);
        self.rotation_deg = 0.0;

        let s = self.px_per_mm();
        let c = world_rect_mm.center();
        self.pan_mm = Point2D::new(
            c.x - (f64::from(self.width_px) * 0.5) / s,
            c.y - (f64::from(self.height_px) * 0.5) / s,
        );
    }

    /// Zooms by `factor` while keeping the world point under `screen_pt` stationary.
    pub fn zoom_at_screen_point(&mut self, screen_pt: Point2D, factor: f64) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let world_before = self.screen_to_world(screen_pt);
        self.zoom = (self.zoom * factor).clamp(0.01, 256.0);
        let world_after = self.screen_to_world(screen_pt);
        let delta = world_before - world_after;
        self.pan_mm = self.pan_mm + delta;
    }

    /// Recommended Bézier curve flattening tolerance in mm when `lod_dragging` is active.
    #[must_use]
    pub fn lod_flatten_tolerance_mm(&self) -> Option<f64> {
        if self.lod_dragging {
            // ~2.5 screen pixels expressed in world millimeters
            Some((2.5 / self.px_per_mm()).max(0.1))
        } else {
            None
        }
    }
}
