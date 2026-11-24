// SPDX-License-Identifier: MIT OR Apache-2.0

//! UI-agnostic transient preview overlay types emitted during interactive tool gestures.

use coree_core::{BezierPath, Color, DynamicGuideLine, Point2D, Rect2D};

/// Semantic cursor shape hint for the host shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorHint {
    #[default]
    Default,
    Crosshair,
    Move,
    Grab,
    Grabbing,
    Text,
    ZoomIn,
    ZoomOut,
    Eyedropper,
    Knife,
    Eraser,
    NodeEdit,
}

/// Visual role for transient overlay paths rendered above cached canvas tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverlayStrokeKind {
    #[default]
    Accent,
    Secondary,
    Danger,
    Guide,
    MarqueeFill,
    CropMask,
    DashedGuide,
}

/// World-space vector preview path drawn on the transient overlay layer.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayPath {
    pub path: BezierPath,
    pub stroke_kind: OverlayStrokeKind,
    pub closed_fill: bool,
}

impl OverlayPath {
    /// Creates an outlined transient preview path.
    #[must_use]
    pub const fn stroke(path: BezierPath, stroke_kind: OverlayStrokeKind) -> Self {
        Self {
            path,
            stroke_kind,
            closed_fill: false,
        }
    }

    /// Creates a translucent filled + outlined marquee/region preview path.
    #[must_use]
    pub const fn filled(path: BezierPath, stroke_kind: OverlayStrokeKind) -> Self {
        Self {
            path,
            stroke_kind,
            closed_fill: true,
        }
    }
}

/// Geometric shape of a transient interactive handle rendered on canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HandleShape {
    #[default]
    Square,
    Circle,
    Diamond,
    Crosshair,
}

/// World-space interactive handle or indicator node on [`TransientOverlay`].
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayHandle {
    pub pos_mm: Point2D,
    pub shape: HandleShape,
    pub selected: bool,
    pub color_hint: Option<Color>,
}

impl OverlayHandle {
    /// Creates an unselected handle without a color override.
    #[must_use]
    pub const fn new(pos_mm: Point2D, shape: HandleShape) -> Self {
        Self {
            pos_mm,
            shape,
            selected: false,
            color_hint: None,
        }
    }
}

/// Complete UI-agnostic transient preview emitted by [`ToolCore`](crate::ToolCore).
///
/// Rendered directly on top of the cached document bitmap so mid-drag previews never mutate
/// `Document` and never call `DirtyTileCache::mark_all_dirty()`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TransientOverlay {
    pub preview_paths: Vec<OverlayPath>,
    pub preview_handles: Vec<OverlayHandle>,
    pub active_guides: Vec<DynamicGuideLine>,
    pub dirty_world_bbox_mm: Option<Rect2D>,
    pub cursor_hint: CursorHint,
}

impl TransientOverlay {
    /// Computes the tight world bounding box across all `preview_paths`, `preview_handles`,
    /// and `active_guides` in `self`.
    #[must_use]
    pub fn compute_current_bbox_mm(&self) -> Option<Rect2D> {
        let mut acc: Option<Rect2D> = None;
        let mut include = |r: Rect2D| {
            if r.is_finite() {
                acc = Some(match acc {
                    Some(prev) => prev.union(r),
                    None => r,
                });
            }
        };
        for p in &self.preview_paths {
            if let Some(bb) = p.path.bounding_box() {
                include(bb.expand(1.0));
            }
        }
        for h in &self.preview_handles {
            if h.pos_mm.is_finite() {
                include(Rect2D::from_xywh(
                    h.pos_mm.x - 1.5,
                    h.pos_mm.y - 1.5,
                    3.0,
                    3.0,
                ));
            }
        }
        for g in &self.active_guides {
            if g.start.is_finite() && g.end.is_finite() {
                include(Rect2D::from_points(g.start, g.end));
            }
        }
        acc
    }

    /// Updates `dirty_world_bbox_mm` to cover the union of the previous overlay bounds and
    /// the current overlay bounds so host incremental overlays or dirty rects remain exact.
    pub fn finalize_dirty_bbox(&mut self, prev_bbox: Option<Rect2D>) -> Option<Rect2D> {
        let cur = self.compute_current_bbox_mm();
        let combined = match (prev_bbox, cur) {
            (Some(a), Some(b)) => Some(a.union(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        self.dirty_world_bbox_mm = combined;
        cur
    }
}
