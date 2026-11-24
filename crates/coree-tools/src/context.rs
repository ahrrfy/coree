// SPDX-License-Identifier: MIT OR Apache-2.0

//! Headless input events and read-only scene/viewport/snap context for [`ToolCore`](crate::ToolCore).

use crate::types::ToolSettingsSnapshot;
use coree_core::{
    Document, Fill, LayerId, LensConfig, ObjectId, OutlineStyle, Point2D, SelectionState,
    SnapEngine, SnapOptions, TextStyle, Viewport,
};

/// Keyboard modifier state at the moment an event occurs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModifiersState {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// Mouse / stylus pointer button identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerButton {
    #[default]
    Primary,
    Secondary,
    Middle,
}

/// Headless pointer event in world millimeter & canvas-local screen pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerEvent {
    pub world_pt_mm: Point2D,
    pub screen_pt_px: [f32; 2],
    pub button: PointerButton,
    pub modifiers: ModifiersState,
    /// `1` = single press/click, `2` = double-click.
    pub click_count: u8,
}

impl PointerEvent {
    /// Creates a primary single-click/drag pointer event.
    #[must_use]
    pub const fn primary(world_pt_mm: Point2D, screen_pt_px: [f32; 2]) -> Self {
        Self {
            world_pt_mm,
            screen_pt_px,
            button: PointerButton::Primary,
            modifiers: ModifiersState {
                shift: false,
                ctrl: false,
                alt: false,
            },
            click_count: 1,
        }
    }

    /// Creates a primary pointer event with explicit keyboard modifiers.
    #[must_use]
    pub const fn with_modifiers(
        world_pt_mm: Point2D,
        screen_pt_px: [f32; 2],
        modifiers: ModifiersState,
    ) -> Self {
        Self {
            world_pt_mm,
            screen_pt_px,
            button: PointerButton::Primary,
            modifiers,
            click_count: 1,
        }
    }
}

/// Tool-relevant key inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKey {
    Enter,
    Escape,
    Delete,
    Backspace,
    Space,
}

/// Headless keyboard event for [`ToolCore`](crate::ToolCore).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: ToolKey,
    pub modifiers: ModifiersState,
}

/// Immutable read-only context passed into [`ToolCore`](crate::ToolCore) on every pointer/key step.
///
/// Strictly forbids mid-drag mutation of `Document` — commits flow solely through returned
/// [`CommandOp`](coree_core::CommandOp)s.
pub struct ToolReadContext<'a> {
    pub document: &'a Document,
    pub selection: &'a SelectionState,
    pub snap_engine: &'a SnapEngine,
    pub viewport: &'a Viewport,
    pub snap_options: &'a SnapOptions,
    pub active_layer_id: Option<LayerId>,
    pub creation_layer_id: Option<LayerId>,
    pub powerclip_edit_id: Option<ObjectId>,
    pub treat_all_objects_as_filled: bool,
    pub default_fill: &'a Fill,
    pub default_outline: &'a OutlineStyle,
    pub default_text_style: &'a TextStyle,
    pub default_lens: &'a LensConfig,
    pub settings: &'a ToolSettingsSnapshot,
}

impl<'a> ToolReadContext<'a> {
    /// Resolves the target layer ID for creating new objects on the active page.
    #[must_use]
    pub fn target_creation_layer_id(&self) -> Option<LayerId> {
        self.creation_layer_id
            .or(self.active_layer_id)
            .or_else(|| {
                self.document
                    .active_page()
                    .and_then(|p| p.layers.iter().rev().find(|l| !l.locked && l.visible))
                    .map(|l| l.id)
            })
            .or_else(|| {
                self.document
                    .active_page()
                    .and_then(|p| p.layers.last())
                    .map(|l| l.id)
            })
    }

    /// Logical screen-pixel-to-world-mm conversion ratio for handle hit tolerances.
    #[must_use]
    pub fn logical_px_per_mm(&self) -> f64 {
        self.viewport.logical_px_per_mm().max(1e-4)
    }

    /// Converts a screen-pixel hit radius into world millimeters at current zoom.
    #[must_use]
    pub fn hit_tol_mm(&self, px_radius: f64, min_mm: f64) -> f64 {
        (px_radius / self.logical_px_per_mm()).max(min_mm)
    }
}
