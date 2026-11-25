// SPDX-License-Identifier: MIT OR Apache-2.0

//! Outputs and host-delegated action requests returned by [`ToolCore`](crate::ToolCore).

use crate::overlay::TransientOverlay;
use crate::types::ToolId;
use coree_core::{
    Affine2D, CommandOp, NodeRef, ObjectId, Point2D, RasterMaskSelection, Rect2D, StylusSample,
    Vec2D,
};

/// Non-`CommandOp` / `coree-render`-dependent / UI-shell side-effect requested by [`ToolCore`](crate::ToolCore).
#[derive(Debug, Clone, PartialEq)]
pub enum ToolActionRequest {
    SelectObjects {
        ids: Vec<ObjectId>,
        additive: bool,
        toggle: bool,
    },
    ClearSelection,
    ToggleTransformMode,
    SelectNodes {
        nodes: Vec<NodeRef>,
        additive: bool,
    },
    ClearNodeSelection,
    SetPivot {
        pivot_mm: Point2D,
    },
    PanTo {
        pan_mm: Point2D,
        lod_dragging: bool,
    },
    ZoomFitRect {
        rect_mm: Rect2D,
        margin_px: f64,
        one_shot_complete: bool,
    },
    ZoomAtScreen {
        screen_px: Point2D,
        factor: f64,
        one_shot_complete: bool,
    },
    SetPendingCropRect(Option<Rect2D>),
    ApplyCropRect(Rect2D),
    SetRasterMaskRect(Option<Rect2D>),
    SetRasterMask(Option<RasterMaskSelection>),
    ClearRasterMask,
    SetSelectedGuide(Option<u64>),
    SetSelectedMeshNode(Option<(ObjectId, usize)>),
    EnterPowerClipEdit(ObjectId),
    ExitPowerClipEdit,
    OpenInlineTextEdit {
        object_id: Option<ObjectId>,
        world_pos_mm: Point2D,
        is_paragraph: bool,
        box_size_mm: Vec2D,
    },
    CommitInlineTextEdit,
    SmartFillAt {
        pt_mm: Point2D,
    },
    ApplyEyedropper {
        pt_mm: Point2D,
        attributes_mode: bool,
        force_sample: bool,
    },
    ApplyRasterStroke {
        tool: ToolId,
        points_mm: Vec<Point2D>,
        samples: Vec<StylusSample>,
    },
    SetCloneSource(Point2D),
    StampDuplicateDuringDrag {
        live_world_affine: Affine2D,
    },
    ApplyLensToObject {
        object_id: ObjectId,
    },
    EnsureEnvelopeOnObject {
        object_id: ObjectId,
    },
    MeshFillInteract {
        start_mm: Point2D,
        end_mm: Point2D,
        shift_recolor: bool,
    },
    Toast {
        message: String,
        is_error: bool,
    },
}

/// Output returned by [`ToolCore`](crate::ToolCore) on pointer or keyboard events.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutput {
    /// Interaction continues; host updates its transient overlay without touching `Document`.
    Continue { overlay: TransientOverlay },
    /// Gesture completed; host executes `ops` atomically via `CommandHistory::execute`.
    Commit {
        ops: Vec<CommandOp>,
        clear_overlay: bool,
        post_actions: Vec<ToolActionRequest>,
    },
    /// Active drag or draft cancelled cleanly; `Document` remains untouched.
    Cancel,
    /// Event handled purely via UI/selection/viewport/render-delegated actions.
    Consumed {
        overlay: Option<TransientOverlay>,
        actions: Vec<ToolActionRequest>,
    },
}

impl ToolOutput {
    /// Convenience constructor for a non-mutating `Consumed` output with actions only.
    #[must_use]
    pub fn actions(actions: Vec<ToolActionRequest>) -> Self {
        Self::Consumed {
            overlay: None,
            actions,
        }
    }

    /// Convenience constructor for an empty `Consumed` output.
    #[must_use]
    pub const fn noop() -> Self {
        Self::Consumed {
            overlay: None,
            actions: Vec::new(),
        }
    }
}
