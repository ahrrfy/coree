// SPDX-License-Identifier: MIT OR Apache-2.0

//! Modular headless tool state machines (`ToolCore`, `ToolHandler`, `ActiveDragState`).

use crate::builders::{
    build_bspline_create_op, build_connector_op, build_drawn_box_output,
    build_interactive_effect_output, build_interactive_fill_op, build_knife_cut_op,
    build_mesh_fill_output, build_replace_effect_op, build_three_point_curve_op,
    build_three_point_shape_op, build_vector_eraser_ops, build_virtual_segment_delete_stroke_ops,
    collect_editable_fillable_targets, collect_nodes_in_rect, compute_lasso_hits_scoped,
    hit_test_canvas_point_opt, hit_test_canvas_point_scoped_opt,
    hit_test_canvas_point_scoped_opt_tol, is_object_in_or_descendant_of_selection,
    order_selection_frontmost_first, resolve_sculpt_target_at, resolve_sculpt_target_in_selection,
    resolve_selected_object_at_or_first,
};
use crate::context::{KeyEvent, PointerButton, PointerEvent, ToolKey, ToolReadContext};
use crate::math_helpers::{
    bspline_to_bezier_path, build_world_transform_command, compute_constrained_box,
    compute_free_transform_affine, compute_scale_affine, constrain_line_15deg,
    editable_topmost_selection, ellipse_rect_to_bezier_path, find_nearest_object_anchor_in_doc,
    gradient_ray_local, gradient_stops_mut, hit_curve_segment_on_selection_world,
    hit_editable_bezier_node_or_handle_world, hit_parametric_shape_handle_world,
    hit_scale_handle_world, hit_test_canvas_guide, interactive_effect_handle_points,
    is_object_editable_in_doc, object_preview_world_path, parametric_shape_handles,
    rect_to_bezier_path, selection_handle_world_positions, three_point_arc_path,
    try_insert_gradient_stop_on_ray_world, update_parametric_shape_live,
    world_inverse_transform_of, world_transform_of_or_local,
};
use crate::output::{ToolActionRequest, ToolOutput};
use crate::overlay::{
    CursorHint, HandleShape, OverlayHandle, OverlayPath, OverlayStrokeKind, TransientOverlay,
};
use crate::types::{NodeDragPart, ParametricHandleKind, RasterBrushOp, ScaleHandle, ToolId};
use coree_core::{
    hit_test_node, hit_test_powerclip_contents_rect, hit_test_rect, Affine2D, ArtisticMedia,
    ArtisticMediaStyle, BSplineCurve, BezierPath, CommandOp, DynamicGuideLine, Effect, Fill,
    GuideOrientation, NodeKind, NodePayload, NodeRef, ObjectId, OutlineStyle, ParametricShape,
    PathNode, Point2D, Rect2D, SceneObject, SnapEngine, Subpath, TransformMode, TransparencyMode,
    Vec2D,
};

/// Trait implemented by modular tool state machines.
pub trait ToolHandler {
    fn on_pointer_down(&mut self, ctx: &ToolReadContext<'_>, ev: &PointerEvent) -> ToolOutput;
    fn on_pointer_move(&mut self, ctx: &ToolReadContext<'_>, ev: &PointerEvent) -> ToolOutput;
    fn on_pointer_up(&mut self, ctx: &ToolReadContext<'_>, ev: &PointerEvent) -> ToolOutput;
    fn on_key(&mut self, ctx: &ToolReadContext<'_>, ev: &KeyEvent) -> ToolOutput;
    fn current_overlay(&self, ctx: &ToolReadContext<'_>) -> TransientOverlay;
}

/// Active interactive gesture state stored inside [`ToolCore`].
///
/// All mid-drag edits (`draft_paths`, `draft_shape`, `draft_fill`, `draft_effect`,
/// `draft_orientation`, `accum_dx`/`accum_dy`, `last_affine`) live here — never in `Document`.
#[derive(Debug, Clone, Default)]
pub enum ActiveDragState {
    #[default]
    Idle,
    Panning {
        start_screen: [f32; 2],
        orig_pan_mm: Point2D,
    },
    MarqueeSelect {
        start_mm: Point2D,
        current_mm: Point2D,
    },
    LassoSelect {
        points_mm: Vec<Point2D>,
    },
    MovingSelection {
        object_ids: Vec<ObjectId>,
        start_mm: Point2D,
        last_mm: Point2D,
        accum_dx: f64,
        accum_dy: f64,
        pending_click: Option<(ObjectId, bool)>,
    },
    ScalingSelection {
        handle: ScaleHandle,
        orig_bounds: Rect2D,
        current_mm: Point2D,
        orig_transforms: Vec<(ObjectId, Affine2D)>,
        last_affine: Affine2D,
    },
    RotatingSelection {
        pivot_mm: Point2D,
        start_angle_rad: f64,
        current_angle_rad: f64,
        orig_transforms: Vec<(ObjectId, Affine2D)>,
        last_affine: Affine2D,
    },
    SkewingSelection {
        horizontal: bool,
        pivot_mm: Point2D,
        start_mm: Point2D,
        current_mm: Point2D,
        orig_transforms: Vec<(ObjectId, Affine2D)>,
        last_affine: Affine2D,
    },
    FreeTransforming {
        anchor_mm: Point2D,
        start_mm: Point2D,
        current_mm: Point2D,
        orig_transforms: Vec<(ObjectId, Affine2D)>,
        last_affine: Affine2D,
    },
    DraggingPivot,
    DraggingNode {
        node: NodeRef,
        handle_part: NodeDragPart,
        drag_nodes: Vec<NodeRef>,
        draft_paths: Vec<(ObjectId, BezierPath)>,
        last_world_mm: Point2D,
    },
    NodeMarqueeSelect {
        start_mm: Point2D,
        current_mm: Point2D,
    },
    DraggingCurveSegment {
        object_id: ObjectId,
        subpath_index: usize,
        seg_index: usize,
        t: f64,
        last_local_mm: Point2D,
        draft_path: BezierPath,
    },
    DraggingParametricHandle {
        object_id: ObjectId,
        handle: ParametricHandleKind,
        orig_shape: ParametricShape,
        draft_shape: ParametricShape,
    },
    DraggingBSplineControlPoint {
        object_id: ObjectId,
        cp_index: usize,
        orig_bspline: BSplineCurve,
        draft_bspline: BSplineCurve,
    },
    DraggingArtisticMediaNode {
        object_id: ObjectId,
        subpath_index: usize,
        node_index: usize,
        handle_part: NodeDragPart,
        orig_spine: BezierPath,
        draft_spine: BezierPath,
        style: ArtisticMediaStyle,
    },
    Sculpting {
        object_id: ObjectId,
        was_shape: bool,
        draft_path: BezierPath,
        last_mm: Point2D,
        points_mm: Vec<Point2D>,
        step_counter: u64,
    },
    ZoomMarquee {
        start_mm: Point2D,
        current_mm: Point2D,
    },
    CropBoxHandle {
        handle: ScaleHandle,
        orig_rect: Rect2D,
    },
    PullingBezierHandle {
        anchor_mm: Point2D,
        handle_out_mm: Point2D,
        closing: bool,
    },
    DraggingGradientHandle {
        object_id: ObjectId,
        is_end: bool,
        orig_fill: Fill,
        draft_fill: Fill,
    },
    DraggingGradientFocal {
        object_id: ObjectId,
        orig_fill: Fill,
        draft_fill: Fill,
    },
    DraggingGradientStop {
        object_id: ObjectId,
        stop_index: usize,
        orig_fill: Fill,
        draft_fill: Fill,
    },
    DraggingEffectHandle {
        object_id: ObjectId,
        effect_index: usize,
        orig_effect: Effect,
        draft_effect: Effect,
    },
    DraggingCanvasGuide {
        guide_id: u64,
        orig_orientation: GuideOrientation,
        draft_orientation: GuideOrientation,
    },
    DraggingTransparencyHandle {
        object_id: ObjectId,
        effect_index: usize,
        is_end: bool,
        orig_effect: Effect,
        draft_effect: Effect,
    },
    DraggingInteractiveEffect {
        object_id: ObjectId,
        start_mm: Point2D,
        current_mm: Point2D,
    },
    DraggingEnvelopeHandle {
        object_id: ObjectId,
        effect_index: usize,
        handle_index: usize,
        orig_effect: Effect,
        draft_effect: Effect,
    },
    DraggingMeshNode {
        object_id: ObjectId,
        node_index: usize,
        orig_fill: Fill,
        draft_fill: Fill,
    },
    DraggingMeshHandle {
        object_id: ObjectId,
        node_index: usize,
        handle_slot: u8, // 0=N, 1=S, 2=E, 3=W
        orig_fill: Fill,
        draft_fill: Fill,
    },
    DrawingBox {
        start_mm: Point2D,
        current_mm: Point2D,
        ctrl: bool,
        shift: bool,
    },
    DrawingStroke {
        points_mm: Vec<Point2D>,
    },
    DraggingRulerGuide {
        horizontal: bool,
        position_mm: f64,
    },
}

/// Complete headless tool state machine (Layer 3 `ToolCore`) decoupled from `egui`.
#[derive(Debug, Clone)]
pub struct ToolCore {
    active_tool: ToolId,
    pub drag_state: ActiveDragState,
    pub poly_points_mm: Vec<Point2D>,
    pub bezier_draft_nodes: Vec<PathNode>,
    pub three_point_curve_chord: Option<(Point2D, Point2D)>,
    pub three_point_shape_chord: Option<(ToolId, Point2D, Point2D)>,
    pub live_sketch_strokes: Vec<Vec<Point2D>>,
    pub live_sketch_object_id: Option<ObjectId>,
    pub symmetry_enabled: bool,
    pub symmetry_folds: u32,
    pub symmetry_mirror: bool,
    pub symmetry_origin_mm: Option<Point2D>,
    pub pending_crop_rect: Option<Rect2D>,
    pub raster_mask_rect_mm: Option<Rect2D>,
    pub selected_mesh_node: Option<(ObjectId, usize)>,
    pub selected_guide_id: Option<u64>,
    pub active_guides: Vec<DynamicGuideLine>,
    pub cursor_world_mm: Point2D,
    prev_dirty_bbox_mm: Option<Rect2D>,
}

impl Default for ToolCore {
    fn default() -> Self {
        Self::new(ToolId::Pick)
    }
}

impl ToolCore {
    /// Creates a new headless tool engine initialized with `initial_tool`.
    #[must_use]
    pub fn new(initial_tool: ToolId) -> Self {
        Self {
            active_tool: initial_tool,
            drag_state: ActiveDragState::Idle,
            poly_points_mm: Vec::new(),
            bezier_draft_nodes: Vec::new(),
            three_point_curve_chord: None,
            three_point_shape_chord: None,
            live_sketch_strokes: Vec::new(),
            live_sketch_object_id: None,
            symmetry_enabled: false,
            symmetry_folds: 4,
            symmetry_mirror: true,
            symmetry_origin_mm: None,
            pending_crop_rect: None,
            raster_mask_rect_mm: None,
            selected_mesh_node: None,
            selected_guide_id: None,
            active_guides: Vec::new(),
            cursor_world_mm: Point2D::ZERO,
            prev_dirty_bbox_mm: None,
        }
    }

    /// Returns the currently active [`ToolId`].
    #[must_use]
    pub const fn active_tool(&self) -> ToolId {
        self.active_tool
    }

    /// Switches the active tool and clears uncommitted multi-click draft state.
    pub fn set_active_tool(&mut self, tool: ToolId) {
        if self.active_tool != tool {
            self.active_tool = tool;
            self.drag_state = ActiveDragState::Idle;
            self.clear_drafts();
            if tool != ToolId::Crop {
                self.pending_crop_rect = None;
            }
            self.active_guides.clear();
            self.prev_dirty_bbox_mm = None;
        }
    }

    /// Clears multi-click curve/3-point/LiveSketch draft buffers.
    pub fn clear_drafts(&mut self) {
        self.poly_points_mm.clear();
        self.bezier_draft_nodes.clear();
        self.three_point_curve_chord = None;
        self.three_point_shape_chord = None;
        self.live_sketch_strokes.clear();
        self.live_sketch_object_id = None;
    }

    /// Pops the last point from an active Bezier/Pen/B-Spline/Polyline draft.
    pub fn pop_last_draft_point(&mut self) -> bool {
        if !self.bezier_draft_nodes.is_empty() {
            self.bezier_draft_nodes.pop();
            self.poly_points_mm.pop();
            return true;
        }
        if !self.poly_points_mm.is_empty() {
            self.poly_points_mm.pop();
            return true;
        }
        false
    }

    /// Returns `true` if a drag gesture is currently active.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        !matches!(self.drag_state, ActiveDragState::Idle)
    }

    /// Returns `true` if an interactive selection transform drag (`Move`/`Scale`/`Rotate`/`Skew`/`FreeTransform`) is active.
    #[must_use]
    pub fn is_transform_dragging(&self) -> bool {
        matches!(
            self.drag_state,
            ActiveDragState::MovingSelection { .. }
                | ActiveDragState::ScalingSelection { .. }
                | ActiveDragState::RotatingSelection { .. }
                | ActiveDragState::SkewingSelection { .. }
                | ActiveDragState::FreeTransforming { .. }
        )
    }

    /// Returns `true` if any drag or uncommitted multi-click draft/crop/mask state is active.
    #[must_use]
    pub fn has_active_draft_or_drag(&self) -> bool {
        self.is_dragging()
            || !self.bezier_draft_nodes.is_empty()
            || !self.poly_points_mm.is_empty()
            || self.three_point_curve_chord.is_some()
            || self.three_point_shape_chord.is_some()
            || self.pending_crop_rect.is_some()
            || self.raster_mask_rect_mm.is_some()
    }

    /// Returns the live world-space transform delta for an active transform drag (used by Spacebar/Right-Click stamp duplicate).
    #[must_use]
    pub fn active_live_world_transform(&self) -> Option<Affine2D> {
        match &self.drag_state {
            ActiveDragState::MovingSelection {
                accum_dx, accum_dy, ..
            } => Some(Affine2D::translate(*accum_dx, *accum_dy)),
            ActiveDragState::ScalingSelection { last_affine, .. }
            | ActiveDragState::RotatingSelection { last_affine, .. }
            | ActiveDragState::SkewingSelection { last_affine, .. }
            | ActiveDragState::FreeTransforming { last_affine, .. } => Some(*last_affine),
            _ => None,
        }
    }

    /// Cancels any active drag gesture or draft state cleanly without mutating `Document`.
    pub fn cancel(&mut self) -> ToolOutput {
        self.drag_state = ActiveDragState::Idle;
        self.active_guides.clear();
        self.prev_dirty_bbox_mm = None;
        ToolOutput::Cancel
    }

    /// Cancels all drag and draft states (used by `Esc`).
    pub fn cancel_all(&mut self) -> ToolOutput {
        self.drag_state = ActiveDragState::Idle;
        self.clear_drafts();
        self.pending_crop_rect = None;
        self.raster_mask_rect_mm = None;
        self.active_guides.clear();
        self.prev_dirty_bbox_mm = None;
        ToolOutput::Cancel
    }

    /// Begins a ruler-originated guide drag.
    pub fn begin_ruler_guide_drag(&mut self, horizontal: bool, position_mm: f64) {
        self.drag_state = ActiveDragState::DraggingRulerGuide {
            horizontal,
            position_mm,
        };
    }

    /// Updates an active ruler guide drag.
    pub fn update_ruler_guide_drag(&mut self, horizontal: bool, position_mm: f64) {
        self.drag_state = ActiveDragState::DraggingRulerGuide {
            horizontal,
            position_mm,
        };
    }

    /// Finishes an active ruler guide drag and emits [`CommandOp::AddGuide`] if active.
    pub fn finish_ruler_guide_drag(&mut self) -> Option<CommandOp> {
        let st = std::mem::take(&mut self.drag_state);
        self.prev_dirty_bbox_mm = None;
        match st {
            ActiveDragState::DraggingRulerGuide {
                horizontal,
                position_mm,
            } => Some(CommandOp::AddGuide {
                orientation: if horizontal {
                    GuideOrientation::Horizontal(position_mm)
                } else {
                    GuideOrientation::Vertical(position_mm)
                },
                locked: false,
            }),
            _ => None,
        }
    }

    /// Appends a node to the active Bezier/Pen draft.
    pub fn bezier_add_point(&mut self, pos: Point2D, handle_out: Option<Point2D>) {
        let node = if let Some(ho) = handle_out {
            if ho.distance_to(pos) > 0.35 {
                let v = ho - pos;
                PathNode::new(pos, Some(pos - v), Some(ho), NodeKind::Symmetrical)
            } else {
                PathNode::corner(pos)
            }
        } else {
            PathNode::corner(pos)
        };
        self.bezier_draft_nodes.push(node);
        self.poly_points_mm.push(pos);
    }

    /// Finishes `bezier_draft_nodes` as a [`CommandOp::CreateObject`] if `>= 2` nodes exist.
    pub fn bezier_finish_op(
        &mut self,
        ctx: &ToolReadContext<'_>,
        closed: bool,
    ) -> Option<CommandOp> {
        if self.bezier_draft_nodes.len() < 2 {
            return None;
        }
        let layer_id = ctx.target_creation_layer_id()?;
        let nodes = std::mem::take(&mut self.bezier_draft_nodes);
        self.poly_points_mm.clear();
        let path = BezierPath::from_subpath(Subpath { nodes, closed });
        let fill = if closed {
            ctx.default_fill.clone()
        } else {
            Fill::None
        };
        let obj = SceneObject::new(
            ObjectId(0),
            self.active_tool.label(),
            NodePayload::Path(path),
        )
        .with_fill(fill)
        .with_outline(ctx.default_outline.clone());
        Some(CommandOp::create_object(obj, Some(layer_id)))
    }

    /// Confirms an in-progress multi-step tool (`Crop`, `Bezier`/`Pen`, `BSpline`/`Polyline`).
    pub fn confirm(&mut self, ctx: &ToolReadContext<'_>) -> ToolOutput {
        if let Some(crop) = self.pending_crop_rect.take() {
            return ToolOutput::actions(vec![ToolActionRequest::ApplyCropRect(crop)]);
        }
        if self.bezier_draft_nodes.len() >= 2 {
            if let Some(op) = self.bezier_finish_op(ctx, false) {
                return ToolOutput::Commit {
                    ops: vec![op],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                };
            }
        }
        if self.poly_points_mm.len() >= 2 {
            let pts = std::mem::take(&mut self.poly_points_mm);
            if self.active_tool == ToolId::BSpline {
                if let Some(op) = build_bspline_create_op(ctx, &pts, false) {
                    return ToolOutput::Commit {
                        ops: vec![op],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    };
                }
            } else if let Some(layer_id) = ctx.target_creation_layer_id() {
                let path = BezierPath::from_polygon(pts, false);
                let obj = SceneObject::new(
                    ObjectId(0),
                    self.active_tool.label(),
                    NodePayload::Path(path),
                )
                .with_fill(Fill::None)
                .with_outline(ctx.default_outline.clone());
                return ToolOutput::Commit {
                    ops: vec![CommandOp::create_object(obj, Some(layer_id))],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                };
            }
        }
        ToolOutput::noop()
    }

    fn finalize_overlay(&mut self, mut overlay: TransientOverlay) -> TransientOverlay {
        let cur_bbox = overlay.finalize_dirty_bbox(self.prev_dirty_bbox_mm);
        self.prev_dirty_bbox_mm = cur_bbox;
        overlay
    }

    fn handle_double_click(&mut self, ctx: &ToolReadContext<'_>, ev: &PointerEvent) -> ToolOutput {
        let pt = ev.world_pt_mm;
        match self.active_tool {
            ToolId::Pick | ToolId::FreehandPick | ToolId::ArtisticText | ToolId::ParagraphText => {
                let tol = ctx.hit_tol_mm(6.0, 1.2);
                let hit = crate::math_helpers::hit_test_canvas_point_in_doc(
                    ctx.document,
                    ctx.powerclip_edit_id,
                    ctx.treat_all_objects_as_filled,
                    pt,
                    tol,
                );
                if let Some(oid) = hit {
                    if let Some(obj) = ctx.document.object(oid) {
                        if matches!(
                            obj.payload,
                            NodePayload::ArtisticText(_) | NodePayload::ParagraphText(_)
                        ) {
                            return ToolOutput::actions(vec![
                                ToolActionRequest::OpenInlineTextEdit {
                                    object_id: Some(oid),
                                    world_pos_mm: pt,
                                    is_paragraph: false,
                                    box_size_mm: Vec2D::new(80.0, 20.0),
                                },
                            ]);
                        }
                    }
                }
                if matches!(self.active_tool, ToolId::Pick | ToolId::FreehandPick)
                    && !ev.modifiers.shift
                {
                    let handle_hit = ctx
                        .selection
                        .bounding_box(ctx.document)
                        .is_some_and(|bb| hit_scale_handle_world(ctx.viewport, bb, pt).is_some());
                    let sole_selected_hit =
                        hit.is_some_and(|oid| ctx.selection.objects == vec![oid]);
                    if handle_hit || sole_selected_hit {
                        return ToolOutput::actions(vec![ToolActionRequest::ToggleTransformMode]);
                    }
                }
            }
            ToolId::ShapeNode => {
                if let Some((nref, part, _)) = hit_editable_bezier_node_or_handle_world(
                    ctx.document,
                    &ctx.selection.objects,
                    ctx.viewport,
                    pt,
                ) {
                    if part == NodeDragPart::Anchor {
                        if let Some(obj) = ctx.document.object(nref.object_id) {
                            if let NodePayload::Path(ref orig) = obj.payload {
                                let mut path = orig.clone();
                                if path
                                    .delete_node(nref.subpath_index, nref.node_index)
                                    .is_ok()
                                {
                                    return ToolOutput::Commit {
                                        ops: vec![CommandOp::EditPath {
                                            id: nref.object_id,
                                            path,
                                        }],
                                        clear_overlay: true,
                                        post_actions: vec![
                                            ToolActionRequest::ClearNodeSelection,
                                            ToolActionRequest::Toast {
                                                message: "Deleted selected node(s)".to_owned(),
                                                is_error: false,
                                            },
                                        ],
                                    };
                                }
                            }
                        }
                    }
                }
                if let Some((oid, sp_idx, seg_idx, t)) = hit_curve_segment_on_selection_world(
                    ctx.document,
                    &ctx.selection.objects,
                    ctx.viewport,
                    pt,
                ) {
                    if let Some(obj) = ctx.document.object(oid) {
                        if let NodePayload::Path(ref p) = obj.payload {
                            let mut path = p.clone();
                            if let Ok(ins) = path.add_node_at(sp_idx, seg_idx, t) {
                                return ToolOutput::Commit {
                                    ops: vec![CommandOp::EditPath { id: oid, path }],
                                    clear_overlay: true,
                                    post_actions: vec![
                                        ToolActionRequest::SelectNodes {
                                            nodes: vec![NodeRef {
                                                object_id: oid,
                                                subpath_index: sp_idx,
                                                node_index: ins,
                                            }],
                                            additive: false,
                                        },
                                        ToolActionRequest::Toast {
                                            message: "Inserted node on curve segment".to_owned(),
                                            is_error: false,
                                        },
                                    ],
                                };
                            }
                        }
                    }
                }
            }
            ToolId::Crop => {
                if let Some(crop) = self.pending_crop_rect {
                    if crop.contains_point(pt) {
                        self.pending_crop_rect = None;
                        return ToolOutput::actions(vec![ToolActionRequest::ApplyCropRect(crop)]);
                    }
                }
            }
            ToolId::Bezier | ToolId::Pen => {
                if let Some(op) = self.bezier_finish_op(ctx, false) {
                    return ToolOutput::Commit {
                        ops: vec![op],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    };
                }
            }
            ToolId::BSpline => {
                let pts = std::mem::take(&mut self.poly_points_mm);
                if let Some(op) = build_bspline_create_op(ctx, &pts, false) {
                    return ToolOutput::Commit {
                        ops: vec![op],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    };
                }
            }
            ToolId::Polyline => {
                let pts = std::mem::take(&mut self.poly_points_mm);
                if pts.len() >= 2 {
                    if let Some(layer_id) = ctx.target_creation_layer_id() {
                        let path = BezierPath::from_polygon(pts, false);
                        let obj =
                            SceneObject::new(ObjectId(0), "Polyline", NodePayload::Path(path))
                                .with_fill(Fill::None)
                                .with_outline(ctx.default_outline.clone());
                        return ToolOutput::Commit {
                            ops: vec![CommandOp::create_object(obj, Some(layer_id))],
                            clear_overlay: true,
                            post_actions: Vec::new(),
                        };
                    }
                }
            }
            ToolId::InteractiveFill => {
                for oid in collect_editable_fillable_targets(ctx.document, &ctx.selection.objects) {
                    if let Some(obj) = ctx.document.object(oid) {
                        if let Some(new_fill) = try_insert_gradient_stop_on_ray_world(
                            ctx.document,
                            ctx.viewport,
                            obj,
                            pt,
                        ) {
                            return ToolOutput::Commit {
                                ops: vec![CommandOp::SetFill {
                                    ids: vec![oid],
                                    fill: new_fill,
                                }],
                                clear_overlay: true,
                                post_actions: vec![ToolActionRequest::Toast {
                                    message: "Inserted gradient color stop on fill ray".to_owned(),
                                    is_error: false,
                                }],
                            };
                        }
                    }
                }
            }
            _ => {}
        }
        ToolOutput::noop()
    }

    /// Handles pointer press (single-click or drag initiation).
    pub fn on_pointer_down(&mut self, ctx: &ToolReadContext<'_>, ev: &PointerEvent) -> ToolOutput {
        self.cursor_world_mm = ev.world_pt_mm;
        if ev.click_count >= 2 {
            return self.handle_double_click(ctx, ev);
        }

        let pt = ev.world_pt_mm;
        let shift = ev.modifiers.shift;
        let ctrl = ev.modifiers.ctrl;
        let alt = ev.modifiers.alt;

        // Right-click in Zoom tool zooms out immediately
        if ev.button == PointerButton::Secondary && self.active_tool == ToolId::Zoom {
            let local = ctx.viewport.world_to_screen(pt);
            return ToolOutput::actions(vec![ToolActionRequest::ZoomAtScreen {
                screen_px: local,
                factor: 1.0 / 1.5,
                one_shot_complete: true,
            }]);
        }

        // Complete stage 2 of 3-Point Curve / 3-Point Rect / 3-Point Ellipse on press
        if let Some((p1, p2)) = self.three_point_curve_chord.take() {
            if let Some(op) = build_three_point_curve_op(ctx, p1, p2, pt) {
                return ToolOutput::Commit {
                    ops: vec![op],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                };
            }
        }
        if let Some((shape_tool, p1, p2)) = self.three_point_shape_chord.take() {
            if let Some(op) = build_three_point_shape_op(ctx, shape_tool, p1, p2, pt) {
                return ToolOutput::Commit {
                    ops: vec![op],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                };
            }
        }

        let tol_mm = ctx.hit_tol_mm(7.0, 1.2);
        let close_tol_mm = ctx.hit_tol_mm(8.0, 1.2);
        let mut actions = Vec::new();

        match self.active_tool {
            ToolId::Pan => {
                self.drag_state = ActiveDragState::Panning {
                    start_screen: ev.screen_pt_px,
                    orig_pan_mm: ctx.viewport.pan_mm,
                };
            }
            ToolId::Zoom => {
                self.drag_state = ActiveDragState::ZoomMarquee {
                    start_mm: pt,
                    current_mm: pt,
                };
            }
            ToolId::FreehandPick => {
                self.drag_state = ActiveDragState::LassoSelect {
                    points_mm: vec![pt],
                };
            }
            ToolId::FreeTransform => {
                let sel_ids = if ctx.selection.is_empty() {
                    if let Some(hit_id) = hit_test_canvas_point_opt_tol(ctx, pt, tol_mm) {
                        actions.push(ToolActionRequest::SelectObjects {
                            ids: vec![hit_id],
                            additive: false,
                            toggle: false,
                        });
                        vec![hit_id]
                    } else {
                        Vec::new()
                    }
                } else {
                    ctx.selection.objects.clone()
                };
                let orig_transforms = capture_transforms_for_ids(ctx.document, &sel_ids);
                if !orig_transforms.is_empty() {
                    let anchor = ctx
                        .selection
                        .bounding_box(ctx.document)
                        .or_else(|| {
                            sel_ids
                                .first()
                                .and_then(|&id| ctx.document.world_object_bounds(id))
                        })
                        .map_or(pt, |b| b.center());
                    self.drag_state = ActiveDragState::FreeTransforming {
                        anchor_mm: anchor,
                        start_mm: pt,
                        current_mm: pt,
                        orig_transforms,
                        last_affine: Affine2D::IDENTITY,
                    };
                }
            }
            ToolId::Pick => {
                // 1. Selection handles (Scale / Rotate / Skew / Pivot)
                if let Some(bb) = ctx.selection.bounding_box(ctx.document) {
                    match ctx.selection.transform_mode {
                        TransformMode::ScaleAndMove => {
                            if let Some(handle) = hit_scale_handle_world(ctx.viewport, bb, pt) {
                                let orig_transforms = capture_transforms_for_ids(
                                    ctx.document,
                                    &ctx.selection.objects,
                                );
                                self.drag_state = ActiveDragState::ScalingSelection {
                                    handle,
                                    orig_bounds: bb,
                                    current_mm: pt,
                                    orig_transforms,
                                    last_affine: Affine2D::IDENTITY,
                                };
                                let overlay = self.finalize_overlay(self.build_overlay(ctx));
                                return ToolOutput::Continue { overlay };
                            }
                        }
                        TransformMode::RotateAndSkew => {
                            let pivot_tol = ctx.hit_tol_mm(9.0, 1.0);
                            if !shift && ctx.selection.pivot.distance_to(pt) <= pivot_tol {
                                self.drag_state = ActiveDragState::DraggingPivot;
                                let overlay = self.finalize_overlay(self.build_overlay(ctx));
                                return ToolOutput::Continue { overlay };
                            }
                            if let Some(handle) = hit_scale_handle_world(ctx.viewport, bb, pt) {
                                let orig_transforms = capture_transforms_for_ids(
                                    ctx.document,
                                    &ctx.selection.objects,
                                );
                                let is_corner = matches!(
                                    handle,
                                    ScaleHandle::TopLeft
                                        | ScaleHandle::TopRight
                                        | ScaleHandle::BottomLeft
                                        | ScaleHandle::BottomRight
                                );
                                if is_corner {
                                    let pv = ctx.selection.pivot;
                                    let ang = (pt.y - pv.y).atan2(pt.x - pv.x);
                                    self.drag_state = ActiveDragState::RotatingSelection {
                                        pivot_mm: pv,
                                        start_angle_rad: ang,
                                        current_angle_rad: ang,
                                        orig_transforms,
                                        last_affine: Affine2D::IDENTITY,
                                    };
                                } else {
                                    let horiz =
                                        matches!(handle, ScaleHandle::Top | ScaleHandle::Bottom);
                                    self.drag_state = ActiveDragState::SkewingSelection {
                                        horizontal: horiz,
                                        pivot_mm: ctx.selection.pivot,
                                        start_mm: pt,
                                        current_mm: pt,
                                        orig_transforms,
                                        last_affine: Affine2D::IDENTITY,
                                    };
                                }
                                let overlay = self.finalize_overlay(self.build_overlay(ctx));
                                return ToolOutput::Continue { overlay };
                            }
                        }
                    }
                }

                // 2. Hit object -> MoveSelection (or PowerClip enter/exit on Ctrl+Click)
                let hit = if ctrl {
                    hit_test_canvas_point_opt(ctx, pt)
                } else {
                    hit_test_canvas_point_scoped_opt(ctx, pt)
                };
                if ctrl {
                    let hit_is_pc = hit.filter(|&hid| {
                        matches!(
                            ctx.document.object(hid).map(|o| &o.payload),
                            Some(NodePayload::PowerClip { .. })
                        )
                    });
                    if let Some(pc_id) = hit_is_pc {
                        return ToolOutput::actions(vec![ToolActionRequest::EnterPowerClipEdit(
                            pc_id,
                        )]);
                    } else if let Some(pc_id) = ctx.powerclip_edit_id {
                        let in_pc = hit.is_some_and(|hid| {
                            let mut cur = hid;
                            while let Some(parent) = ctx.document.parent_container_of(cur) {
                                if parent == pc_id {
                                    return true;
                                }
                                cur = parent;
                            }
                            false
                        });
                        if !in_pc {
                            return ToolOutput::actions(vec![ToolActionRequest::ExitPowerClipEdit]);
                        }
                    }
                }

                if let Some(hit_id) = hit {
                    self.selected_guide_id = None;
                    actions.push(ToolActionRequest::SetSelectedGuide(None));
                    let hit_owns_selection = !ctx.selection.is_empty()
                        && ctx.selection.objects.iter().any(|&sid| {
                            let mut cur = sid;
                            let mut depth = 0usize;
                            while depth < 64 {
                                if let Some(pid) = ctx.document.parent_container_of(cur) {
                                    if pid == hit_id {
                                        return true;
                                    }
                                    cur = pid;
                                    depth += 1;
                                } else {
                                    break;
                                }
                            }
                            false
                        });
                    let (drag_ids, pending_click) =
                        if ctx.selection.contains(hit_id) || hit_owns_selection {
                            (
                                editable_topmost_selection(ctx.document, &ctx.selection.objects),
                                Some((hit_id, shift)),
                            )
                        } else {
                            actions.push(ToolActionRequest::SelectObjects {
                                ids: vec![hit_id],
                                additive: false,
                                toggle: shift,
                            });
                            let ids = if is_object_editable_in_doc(ctx.document, hit_id) {
                                vec![hit_id]
                            } else {
                                Vec::new()
                            };
                            (ids, None)
                        };
                    self.drag_state = ActiveDragState::MovingSelection {
                        object_ids: drag_ids,
                        start_mm: pt,
                        last_mm: pt,
                        accum_dx: 0.0,
                        accum_dy: 0.0,
                        pending_click,
                    };
                } else if let Some((guide_id, orig_orientation, locked)) =
                    hit_test_canvas_guide(ctx.document, pt, tol_mm.max(1.8))
                {
                    self.selected_guide_id = Some(guide_id);
                    actions.push(ToolActionRequest::ClearSelection);
                    actions.push(ToolActionRequest::SetSelectedGuide(Some(guide_id)));
                    if !locked {
                        self.drag_state = ActiveDragState::DraggingCanvasGuide {
                            guide_id,
                            orig_orientation,
                            draft_orientation: orig_orientation,
                        };
                    }
                } else {
                    self.selected_guide_id = None;
                    actions.push(ToolActionRequest::SetSelectedGuide(None));
                    self.drag_state = ActiveDragState::MarqueeSelect {
                        start_mm: pt,
                        current_mm: pt,
                    };
                }
            }
            ToolId::ShapeNode => {
                let bspline_tol = ctx.hit_tol_mm(8.0, 1.5);
                for &oid in &ctx.selection.objects {
                    if !is_object_editable_in_doc(ctx.document, oid) {
                        continue;
                    }
                    let Some(obj) = ctx.document.object(oid) else {
                        continue;
                    };
                    if let NodePayload::BSpline(ref bs) = obj.payload {
                        let xf = world_transform_of_or_local(ctx.document, oid);
                        if let Some((cp_idx, _)) = bs
                            .control_points
                            .iter()
                            .enumerate()
                            .map(|(i, &cp)| (i, xf.transform_point(cp).distance_to(pt)))
                            .filter(|(_, d)| *d <= bspline_tol)
                            .min_by(|a, b| a.1.total_cmp(&b.1))
                        {
                            self.drag_state = ActiveDragState::DraggingBSplineControlPoint {
                                object_id: oid,
                                cp_index: cp_idx,
                                orig_bspline: bs.clone(),
                                draft_bspline: bs.clone(),
                            };
                            let overlay = self.finalize_overlay(self.build_overlay(ctx));
                            return ToolOutput::Continue { overlay };
                        }
                    } else if let NodePayload::ArtisticMedia {
                        ref spine,
                        ref style,
                    } = obj.payload
                    {
                        let xf = world_transform_of_or_local(ctx.document, oid);
                        let mut best_hit: Option<(usize, usize, NodeDragPart, f64)> = None;
                        for (sp_idx, sp) in spine.subpaths.iter().enumerate() {
                            for (n_idx, node) in sp.nodes.iter().enumerate() {
                                if let Some(h_in) = node.handle_in {
                                    let d = xf.transform_point(h_in).distance_to(pt);
                                    if d <= bspline_tol && best_hit.is_none_or(|b| d < b.3) {
                                        best_hit = Some((sp_idx, n_idx, NodeDragPart::HandleIn, d));
                                    }
                                }
                                if let Some(h_out) = node.handle_out {
                                    let d = xf.transform_point(h_out).distance_to(pt);
                                    if d <= bspline_tol && best_hit.is_none_or(|b| d < b.3) {
                                        best_hit =
                                            Some((sp_idx, n_idx, NodeDragPart::HandleOut, d));
                                    }
                                }
                                let d = xf.transform_point(node.pos).distance_to(pt);
                                if d <= bspline_tol && best_hit.is_none_or(|b| d < b.3) {
                                    best_hit = Some((sp_idx, n_idx, NodeDragPart::Anchor, d));
                                }
                            }
                        }
                        if let Some((sp_idx, n_idx, handle_part, _)) = best_hit {
                            self.drag_state = ActiveDragState::DraggingArtisticMediaNode {
                                object_id: oid,
                                subpath_index: sp_idx,
                                node_index: n_idx,
                                handle_part,
                                orig_spine: spine.clone(),
                                draft_spine: spine.clone(),
                                style: style.clone(),
                            };
                            let overlay = self.finalize_overlay(self.build_overlay(ctx));
                            return ToolOutput::Continue { overlay };
                        }
                    }
                }
                if let Some((oid, handle, orig_shape)) = hit_parametric_shape_handle_world(
                    ctx.document,
                    &ctx.selection.objects,
                    ctx.viewport,
                    pt,
                ) {
                    self.drag_state = ActiveDragState::DraggingParametricHandle {
                        object_id: oid,
                        handle,
                        draft_shape: orig_shape.clone(),
                        orig_shape,
                    };
                    let overlay = self.finalize_overlay(self.build_overlay(ctx));
                    return ToolOutput::Continue { overlay };
                }
                if let Some((nref, part, _)) = hit_editable_bezier_node_or_handle_world(
                    ctx.document,
                    &ctx.selection.objects,
                    ctx.viewport,
                    pt,
                ) {
                    let mut drag_nodes = if part == NodeDragPart::Anchor
                        && ctx.selection.node_selection.contains(nref)
                    {
                        ctx.selection.node_selection.nodes.clone()
                    } else {
                        actions.push(ToolActionRequest::SelectNodes {
                            nodes: vec![nref],
                            additive: shift,
                        });
                        if shift {
                            let mut next = ctx.selection.node_selection.nodes.clone();
                            if !next.contains(&nref) {
                                next.push(nref);
                            }
                            next
                        } else {
                            vec![nref]
                        }
                    };
                    if drag_nodes.is_empty() {
                        drag_nodes.push(nref);
                    }
                    let mut seen = std::collections::HashSet::new();
                    let mut draft_paths = Vec::new();
                    for nr in &drag_nodes {
                        if seen.insert(nr.object_id) {
                            if let Some(obj) = ctx.document.object(nr.object_id) {
                                if let NodePayload::Path(ref p) = obj.payload {
                                    draft_paths.push((nr.object_id, p.clone()));
                                }
                            }
                        }
                    }
                    if seen.insert(nref.object_id) {
                        if let Some(obj) = ctx.document.object(nref.object_id) {
                            if let NodePayload::Path(ref p) = obj.payload {
                                draft_paths.push((nref.object_id, p.clone()));
                            }
                        }
                    }
                    self.drag_state = ActiveDragState::DraggingNode {
                        node: nref,
                        handle_part: part,
                        drag_nodes,
                        draft_paths,
                        last_world_mm: pt,
                    };
                    let overlay = self.finalize_overlay(self.build_overlay(ctx));
                    return ToolOutput::Consumed {
                        overlay: Some(overlay),
                        actions,
                    };
                }
                if let Some((oid, sp_idx, seg_idx, t)) = hit_curve_segment_on_selection_world(
                    ctx.document,
                    &ctx.selection.objects,
                    ctx.viewport,
                    pt,
                ) {
                    if let Some(obj) = ctx.document.object(oid) {
                        if let NodePayload::Path(ref orig_path) = obj.payload {
                            let inv = world_inverse_transform_of(ctx.document, oid);
                            let local_pt = inv.transform_point(pt);
                            self.drag_state = ActiveDragState::DraggingCurveSegment {
                                object_id: oid,
                                subpath_index: sp_idx,
                                seg_index: seg_idx,
                                t,
                                last_local_mm: local_pt,
                                draft_path: orig_path.clone(),
                            };
                            let overlay = self.finalize_overlay(self.build_overlay(ctx));
                            return ToolOutput::Continue { overlay };
                        }
                    }
                }
                self.drag_state = ActiveDragState::NodeMarqueeSelect {
                    start_mm: pt,
                    current_mm: pt,
                };
            }
            ToolId::Smooth
            | ToolId::Smear
            | ToolId::Twirl
            | ToolId::AttractRepel
            | ToolId::Roughen => {
                let sculpt_r = ctx.settings.sculpt_radius_mm.clamp(1.0, 200.0);
                let target = if !ctx.selection.objects.is_empty() {
                    resolve_sculpt_target_in_selection(
                        ctx.document,
                        &ctx.selection.objects,
                        &[pt],
                        sculpt_r,
                    )
                } else {
                    resolve_sculpt_target_at(ctx, pt, sculpt_r)
                };
                if let Some(oid) = target {
                    if let Some(obj) = ctx.document.object(oid) {
                        let (was_shape, draft_path) = match &obj.payload {
                            NodePayload::Path(p) => (false, p.clone()),
                            NodePayload::Shape(s) => (true, s.to_bezier_path()),
                            NodePayload::BSpline(bs) => (true, bs.to_bezier_path()),
                            _ => return ToolOutput::noop(),
                        };
                        if !is_object_in_or_descendant_of_selection(
                            ctx.document,
                            &ctx.selection.objects,
                            oid,
                        ) {
                            actions.push(ToolActionRequest::SelectObjects {
                                ids: vec![oid],
                                additive: false,
                                toggle: false,
                            });
                        }
                        self.drag_state = ActiveDragState::Sculpting {
                            object_id: oid,
                            was_shape,
                            draft_path,
                            last_mm: pt,
                            points_mm: vec![pt],
                            step_counter: 0,
                        };
                    }
                }
            }
            ToolId::Crop => {
                if let Some(existing_crop) = self.pending_crop_rect {
                    if let Some(handle) = hit_scale_handle_world(ctx.viewport, existing_crop, pt) {
                        self.drag_state = ActiveDragState::CropBoxHandle {
                            handle,
                            orig_rect: existing_crop,
                        };
                        let overlay = self.finalize_overlay(self.build_overlay(ctx));
                        return ToolOutput::Continue { overlay };
                    }
                }
                self.drag_state = ActiveDragState::DrawingBox {
                    start_mm: pt,
                    current_mm: pt,
                    ctrl,
                    shift,
                };
            }
            ToolId::Bezier | ToolId::Pen => {
                let (snapped, guides) = SnapEngine::snap_point(
                    ctx.document,
                    ctx.document.active_page_index(),
                    pt,
                    &[],
                    ctx.snap_options,
                );
                self.active_guides = guides;
                let closing = self.bezier_draft_nodes.len() >= 2
                    && self.bezier_draft_nodes[0].pos.distance_to(pt) <= close_tol_mm;
                let anchor = if closing {
                    self.bezier_draft_nodes[0].pos
                } else {
                    snapped
                };
                self.drag_state = ActiveDragState::PullingBezierHandle {
                    anchor_mm: anchor,
                    handle_out_mm: anchor,
                    closing,
                };
            }
            ToolId::BSpline | ToolId::Polyline => {
                let (snapped, guides) = SnapEngine::snap_point(
                    ctx.document,
                    ctx.document.active_page_index(),
                    pt,
                    &[],
                    ctx.snap_options,
                );
                self.active_guides = guides;
                let close_first = self.poly_points_mm.len() >= 2
                    && self.poly_points_mm[0].distance_to(pt) <= close_tol_mm;
                if close_first {
                    let pts = std::mem::take(&mut self.poly_points_mm);
                    if self.active_tool == ToolId::BSpline {
                        if let Some(op) = build_bspline_create_op(ctx, &pts, true) {
                            return ToolOutput::Commit {
                                ops: vec![op],
                                clear_overlay: true,
                                post_actions: Vec::new(),
                            };
                        }
                    } else if let Some(layer_id) = ctx.target_creation_layer_id() {
                        let path = BezierPath::from_polygon(pts, true);
                        let obj =
                            SceneObject::new(ObjectId(0), "Polyline", NodePayload::Path(path))
                                .with_fill(ctx.default_fill.clone())
                                .with_outline(ctx.default_outline.clone());
                        return ToolOutput::Commit {
                            ops: vec![CommandOp::create_object(obj, Some(layer_id))],
                            clear_overlay: true,
                            post_actions: Vec::new(),
                        };
                    }
                } else {
                    self.poly_points_mm.push(snapped);
                }
            }
            ToolId::InteractiveFill => {
                for oid in collect_editable_fillable_targets(ctx.document, &ctx.selection.objects) {
                    if let Some(obj) = ctx.document.object(oid) {
                        let world_xf = world_transform_of_or_local(ctx.document, oid);
                        let handle_tol = ctx.hit_tol_mm(10.0, 1.0);
                        if let Fill::RadialGradient { center, focal, .. } = &obj.fill {
                            let w_focal = world_xf.transform_point(*focal);
                            if (*focal - *center).length() > 0.2
                                && w_focal.distance_to(pt) <= handle_tol
                            {
                                self.drag_state = ActiveDragState::DraggingGradientFocal {
                                    object_id: oid,
                                    orig_fill: obj.fill.clone(),
                                    draft_fill: obj.fill.clone(),
                                };
                                let overlay = self.finalize_overlay(self.build_overlay(ctx));
                                return ToolOutput::Continue { overlay };
                            }
                        }
                        if let Some((start_l, end_l, stops)) = gradient_ray_local(&obj.fill) {
                            let w_start = world_xf.transform_point(start_l);
                            let w_end = world_xf.transform_point(end_l);
                            if w_start.distance_to(pt) <= handle_tol {
                                if shift && matches!(obj.fill, Fill::RadialGradient { .. }) {
                                    self.drag_state = ActiveDragState::DraggingGradientFocal {
                                        object_id: oid,
                                        orig_fill: obj.fill.clone(),
                                        draft_fill: obj.fill.clone(),
                                    };
                                } else {
                                    self.drag_state = ActiveDragState::DraggingGradientHandle {
                                        object_id: oid,
                                        is_end: false,
                                        orig_fill: obj.fill.clone(),
                                        draft_fill: obj.fill.clone(),
                                    };
                                }
                                let overlay = self.finalize_overlay(self.build_overlay(ctx));
                                return ToolOutput::Continue { overlay };
                            }
                            if w_end.distance_to(pt) <= handle_tol {
                                self.drag_state = ActiveDragState::DraggingGradientHandle {
                                    object_id: oid,
                                    is_end: true,
                                    orig_fill: obj.fill.clone(),
                                    draft_fill: obj.fill.clone(),
                                };
                                let overlay = self.finalize_overlay(self.build_overlay(ctx));
                                return ToolOutput::Continue { overlay };
                            }
                            let stop_tol = ctx.hit_tol_mm(9.0, 0.8);
                            for (s_idx, st) in stops.iter().enumerate() {
                                if s_idx == 0 || s_idx + 1 == stops.len() {
                                    continue;
                                }
                                let t = f64::from(st.offset);
                                let w_pt = world_xf.transform_point(Point2D::new(
                                    start_l.x + (end_l.x - start_l.x) * t,
                                    start_l.y + (end_l.y - start_l.y) * t,
                                ));
                                if w_pt.distance_to(pt) <= stop_tol {
                                    self.drag_state = ActiveDragState::DraggingGradientStop {
                                        object_id: oid,
                                        stop_index: s_idx,
                                        orig_fill: obj.fill.clone(),
                                        draft_fill: obj.fill.clone(),
                                    };
                                    let overlay = self.finalize_overlay(self.build_overlay(ctx));
                                    return ToolOutput::Continue { overlay };
                                }
                            }
                        }
                    }
                }
                self.drag_state = ActiveDragState::DrawingBox {
                    start_mm: pt,
                    current_mm: pt,
                    ctrl,
                    shift,
                };
            }
            ToolId::Transparency => {
                for oid in order_selection_frontmost_first(ctx.document, &ctx.selection.objects) {
                    if is_object_editable_in_doc(ctx.document, oid) {
                        if let Some(obj) = ctx.document.object(oid) {
                            let world_xf = world_transform_of_or_local(ctx.document, oid);
                            let handle_tol = ctx.hit_tol_mm(10.0, 1.0);
                            for (eff_idx, eff) in obj.effects.iter().enumerate() {
                                if let Effect::Transparency {
                                    mode: TransparencyMode::FountainLinear { start, end, .. },
                                    ..
                                } = eff
                                {
                                    let ws = world_xf.transform_point(*start);
                                    let we = world_xf.transform_point(*end);
                                    if ws.distance_to(pt) <= handle_tol {
                                        self.drag_state =
                                            ActiveDragState::DraggingTransparencyHandle {
                                                object_id: oid,
                                                effect_index: eff_idx,
                                                is_end: false,
                                                orig_effect: eff.clone(),
                                                draft_effect: eff.clone(),
                                            };
                                        let overlay =
                                            self.finalize_overlay(self.build_overlay(ctx));
                                        return ToolOutput::Continue { overlay };
                                    }
                                    if we.distance_to(pt) <= handle_tol {
                                        self.drag_state =
                                            ActiveDragState::DraggingTransparencyHandle {
                                                object_id: oid,
                                                effect_index: eff_idx,
                                                is_end: true,
                                                orig_effect: eff.clone(),
                                                draft_effect: eff.clone(),
                                            };
                                        let overlay =
                                            self.finalize_overlay(self.build_overlay(ctx));
                                        return ToolOutput::Continue { overlay };
                                    }
                                }
                            }
                        }
                    }
                }
                let target = if !ctx.selection.objects.is_empty() {
                    resolve_selected_object_at_or_first(ctx, pt, 8.0)
                } else {
                    hit_test_canvas_point_opt_tol(ctx, pt, tol_mm)
                        .filter(|&id| is_object_editable_in_doc(ctx.document, id))
                };
                if let Some(oid) = target {
                    self.drag_state = ActiveDragState::DraggingInteractiveEffect {
                        object_id: oid,
                        start_mm: pt,
                        current_mm: pt,
                    };
                }
            }
            ToolId::Envelope => {
                for oid in order_selection_frontmost_first(ctx.document, &ctx.selection.objects) {
                    if is_object_editable_in_doc(ctx.document, oid) {
                        if let Some(obj) = ctx.document.object(oid) {
                            let world_xf = world_transform_of_or_local(ctx.document, oid);
                            let handle_tol = ctx.hit_tol_mm(9.0, 0.8);
                            for (eff_idx, eff) in obj.effects.iter().enumerate() {
                                if let Effect::Envelope { cage, .. } = eff {
                                    for (h_idx, &c_pt) in cage.iter().enumerate() {
                                        if world_xf.transform_point(c_pt).distance_to(pt)
                                            <= handle_tol
                                        {
                                            self.drag_state =
                                                ActiveDragState::DraggingEnvelopeHandle {
                                                    object_id: oid,
                                                    effect_index: eff_idx,
                                                    handle_index: h_idx,
                                                    orig_effect: eff.clone(),
                                                    draft_effect: eff.clone(),
                                                };
                                            let overlay =
                                                self.finalize_overlay(self.build_overlay(ctx));
                                            return ToolOutput::Continue { overlay };
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                let target = if !ctx.selection.objects.is_empty() {
                    resolve_selected_object_at_or_first(ctx, pt, 8.0)
                } else {
                    hit_test_canvas_point_opt_tol(ctx, pt, tol_mm)
                        .filter(|&id| is_object_editable_in_doc(ctx.document, id))
                };
                if let Some(oid) = target {
                    self.drag_state = ActiveDragState::DraggingInteractiveEffect {
                        object_id: oid,
                        start_mm: pt,
                        current_mm: pt,
                    };
                }
            }
            ToolId::DropShadow
            | ToolId::BlockShadow
            | ToolId::Contour
            | ToolId::Blend
            | ToolId::Distort
            | ToolId::Extrude25D => {
                for oid in order_selection_frontmost_first(ctx.document, &ctx.selection.objects) {
                    if is_object_editable_in_doc(ctx.document, oid) {
                        if let Some(obj) = ctx.document.object(oid) {
                            let knob_tol = ctx.hit_tol_mm(10.0, 1.0);
                            for (eff_idx, eff) in obj.effects.iter().enumerate() {
                                if let Some((_anchor, knob_world)) =
                                    interactive_effect_handle_points(
                                        ctx.document,
                                        obj,
                                        eff,
                                        self.active_tool,
                                    )
                                {
                                    if knob_world.distance_to(pt) <= knob_tol {
                                        self.drag_state = ActiveDragState::DraggingEffectHandle {
                                            object_id: oid,
                                            effect_index: eff_idx,
                                            orig_effect: eff.clone(),
                                            draft_effect: eff.clone(),
                                        };
                                        let overlay =
                                            self.finalize_overlay(self.build_overlay(ctx));
                                        return ToolOutput::Continue { overlay };
                                    }
                                }
                            }
                        }
                    }
                }
                let target = if !ctx.selection.objects.is_empty() {
                    resolve_selected_object_at_or_first(ctx, pt, 8.0)
                } else {
                    hit_test_canvas_point_opt_tol(ctx, pt, tol_mm)
                        .filter(|&id| is_object_editable_in_doc(ctx.document, id))
                };
                if let Some(oid) = target {
                    if !ctx.selection.contains(oid) {
                        actions.push(ToolActionRequest::SelectObjects {
                            ids: vec![oid],
                            additive: false,
                            toggle: false,
                        });
                    }
                    self.drag_state = ActiveDragState::DraggingInteractiveEffect {
                        object_id: oid,
                        start_mm: pt,
                        current_mm: pt,
                    };
                }
            }
            ToolId::MeshFill => {
                for oid in collect_editable_fillable_targets(ctx.document, &ctx.selection.objects) {
                    if let Some(obj) = ctx.document.object(oid) {
                        if let Fill::MeshFill { nodes, .. } = &obj.fill {
                            let world_xf = world_transform_of_or_local(ctx.document, oid);
                            let node_tol = ctx.hit_tol_mm(9.0, 0.8);
                            if let Some((sel_oid, sel_idx)) = self.selected_mesh_node {
                                if sel_oid == oid {
                                    if let Some(mnode) = nodes.get(sel_idx) {
                                        for (slot, opt_h) in [
                                            (0u8, mnode.handle_n),
                                            (1u8, mnode.handle_s),
                                            (2u8, mnode.handle_e),
                                            (3u8, mnode.handle_w),
                                        ] {
                                            if let Some(rel) = opt_h {
                                                let abs_h = Point2D::new(
                                                    mnode.pos.x + rel.x,
                                                    mnode.pos.y + rel.y,
                                                );
                                                if world_xf.transform_point(abs_h).distance_to(pt)
                                                    <= node_tol
                                                {
                                                    self.drag_state =
                                                        ActiveDragState::DraggingMeshHandle {
                                                            object_id: oid,
                                                            node_index: sel_idx,
                                                            handle_slot: slot,
                                                            orig_fill: obj.fill.clone(),
                                                            draft_fill: obj.fill.clone(),
                                                        };
                                                    let overlay = self
                                                        .finalize_overlay(self.build_overlay(ctx));
                                                    return ToolOutput::Consumed {
                                                        overlay: Some(overlay),
                                                        actions,
                                                    };
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            for (n_idx, mnode) in nodes.iter().enumerate() {
                                if world_xf.transform_point(mnode.pos).distance_to(pt) <= node_tol {
                                    self.selected_mesh_node = Some((oid, n_idx));
                                    actions.push(ToolActionRequest::SetSelectedMeshNode(Some((
                                        oid, n_idx,
                                    ))));
                                    self.drag_state = ActiveDragState::DraggingMeshNode {
                                        object_id: oid,
                                        node_index: n_idx,
                                        orig_fill: obj.fill.clone(),
                                        draft_fill: obj.fill.clone(),
                                    };
                                    let overlay = self.finalize_overlay(self.build_overlay(ctx));
                                    return ToolOutput::Consumed {
                                        overlay: Some(overlay),
                                        actions,
                                    };
                                }
                            }
                        }
                    }
                }
                self.drag_state = ActiveDragState::DrawingBox {
                    start_mm: pt,
                    current_mm: pt,
                    ctrl,
                    shift,
                };
            }
            ToolId::Freehand
            | ToolId::TwoPointLine
            | ToolId::SmartDrawing
            | ToolId::LiveSketch
            | ToolId::ArtisticMedia
            | ToolId::Knife
            | ToolId::VirtualSegmentDelete
            | ToolId::Eraser
            | ToolId::BitmapBrush
            | ToolId::BitmapEraser
            | ToolId::CloneStamp
            | ToolId::Smudge => {
                if alt
                    && (self.active_tool == ToolId::CloneStamp
                        || (self.active_tool == ToolId::BitmapBrush
                            && matches!(
                                ctx.settings.raster_op,
                                RasterBrushOp::CloneStamp | RasterBrushOp::Healing
                            )))
                {
                    return ToolOutput::actions(vec![
                        ToolActionRequest::SetCloneSource(pt),
                        ToolActionRequest::Toast {
                            message: format!("Set sample source at ({:.1}, {:.1}) mm", pt.x, pt.y),
                            is_error: false,
                        },
                    ]);
                }
                self.drag_state = ActiveDragState::DrawingStroke {
                    points_mm: vec![pt],
                };
            }
            _ => {
                let (snapped, guides) = SnapEngine::snap_point(
                    ctx.document,
                    ctx.document.active_page_index(),
                    pt,
                    &[],
                    ctx.snap_options,
                );
                self.active_guides = guides;
                self.drag_state = ActiveDragState::DrawingBox {
                    start_mm: snapped,
                    current_mm: snapped,
                    ctrl,
                    shift,
                };
            }
        }

        let overlay = self.finalize_overlay(self.build_overlay(ctx));
        if actions.is_empty() {
            ToolOutput::Continue { overlay }
        } else {
            ToolOutput::Consumed {
                overlay: Some(overlay),
                actions,
            }
        }
    }

    /// Updates the active drag (or hover rubber-band preview) without mutating `Document`.
    pub fn on_pointer_move(&mut self, ctx: &ToolReadContext<'_>, ev: &PointerEvent) -> ToolOutput {
        self.cursor_world_mm = ev.world_pt_mm;
        let pt = ev.world_pt_mm;
        let ctrl = ev.modifiers.ctrl;
        let shift = ev.modifiers.shift;
        let sculpt_radius = ctx.settings.sculpt_radius_mm.clamp(1.0, 200.0);
        let sculpt_strength = ctx.settings.sculpt_strength.clamp(0.05, 1.0);
        let active_tool = self.active_tool;

        let mut post_actions = Vec::new();
        let mut state = std::mem::take(&mut self.drag_state);
        match &mut state {
            ActiveDragState::Panning {
                start_screen,
                orig_pan_mm,
            } => {
                let s = ctx.logical_px_per_mm();
                let dx_px = f64::from(ev.screen_pt_px[0] - start_screen[0]);
                let dy_px = f64::from(ev.screen_pt_px[1] - start_screen[1]);
                let new_pan = Point2D::new(orig_pan_mm.x - dx_px / s, orig_pan_mm.y - dy_px / s);
                post_actions.push(ToolActionRequest::PanTo {
                    pan_mm: new_pan,
                    lod_dragging: true,
                });
            }
            ActiveDragState::MarqueeSelect { current_mm, .. }
            | ActiveDragState::NodeMarqueeSelect { current_mm, .. }
            | ActiveDragState::ZoomMarquee { current_mm, .. }
            | ActiveDragState::DraggingInteractiveEffect { current_mm, .. } => {
                *current_mm = pt;
            }
            ActiveDragState::LassoSelect { points_mm } => {
                if points_mm
                    .last()
                    .is_none_or(|last| last.distance_to(pt) > 0.6)
                {
                    points_mm.push(pt);
                }
            }
            ActiveDragState::MovingSelection {
                object_ids,
                start_mm,
                last_mm,
                accum_dx,
                accum_dy,
                ..
            } => {
                let raw_pt = if ctrl {
                    let d = pt - *start_mm;
                    if d.x.abs() >= d.y.abs() {
                        Point2D::new(pt.x, start_mm.y)
                    } else {
                        Point2D::new(start_mm.x, pt.y)
                    }
                } else {
                    pt
                };
                let (snapped, guides) = SnapEngine::snap_point(
                    ctx.document,
                    ctx.document.active_page_index(),
                    raw_pt,
                    object_ids,
                    ctx.snap_options,
                );
                self.active_guides = guides;
                let dx = snapped.x - last_mm.x;
                let dy = snapped.y - last_mm.y;
                if dx.abs() > 1e-6 || dy.abs() > 1e-6 {
                    *accum_dx += dx;
                    *accum_dy += dy;
                    *last_mm = snapped;
                }
            }
            ActiveDragState::ScalingSelection {
                handle,
                orig_bounds,
                current_mm,
                last_affine,
                ..
            } => {
                *current_mm = pt;
                *last_affine = compute_scale_affine(*handle, *orig_bounds, pt, ctrl, shift)
                    .unwrap_or(Affine2D::IDENTITY);
            }
            ActiveDragState::RotatingSelection {
                pivot_mm,
                start_angle_rad,
                current_angle_rad,
                last_affine,
                ..
            } => {
                let mut delta = (pt.y - pivot_mm.y).atan2(pt.x - pivot_mm.x) - *start_angle_rad;
                if ctrl {
                    let step = std::f64::consts::PI / 12.0;
                    delta = (delta / step).round() * step;
                }
                *current_angle_rad = *start_angle_rad + delta;
                *last_affine = Affine2D::rotate_about(delta, *pivot_mm);
            }
            ActiveDragState::SkewingSelection {
                horizontal,
                pivot_mm,
                start_mm,
                current_mm,
                last_affine,
                ..
            } => {
                *current_mm = pt;
                let (sx, sy) = if *horizontal {
                    (((pt.x - start_mm.x) / 60.0).clamp(-1.2, 1.2), 0.0)
                } else {
                    (0.0, ((pt.y - start_mm.y) / 60.0).clamp(-1.2, 1.2))
                };
                *last_affine = Affine2D::translate(pivot_mm.x, pivot_mm.y)
                    .compose(&Affine2D::skew(sx, sy))
                    .compose(&Affine2D::translate(-pivot_mm.x, -pivot_mm.y));
            }
            ActiveDragState::FreeTransforming {
                anchor_mm,
                start_mm,
                current_mm,
                last_affine,
                ..
            } => {
                *current_mm = pt;
                *last_affine =
                    compute_free_transform_affine(*anchor_mm, *start_mm, pt, ctrl, shift)
                        .unwrap_or(Affine2D::IDENTITY);
            }
            ActiveDragState::DraggingPivot => {
                post_actions.push(ToolActionRequest::SetPivot { pivot_mm: pt });
            }
            ActiveDragState::DraggingNode {
                node,
                handle_part,
                drag_nodes,
                draft_paths,
                last_world_mm,
            } => {
                let world_delta = pt - *last_world_mm;
                *last_world_mm = pt;
                match handle_part {
                    NodeDragPart::Anchor => {
                        for nref in drag_nodes.iter() {
                            if !is_object_editable_in_doc(ctx.document, nref.object_id) {
                                continue;
                            }
                            let inv = world_inverse_transform_of(ctx.document, nref.object_id);
                            let local_delta = Vec2D::new(
                                inv.m[0] * world_delta.x + inv.m[2] * world_delta.y,
                                inv.m[1] * world_delta.x + inv.m[3] * world_delta.y,
                            );
                            if let Some((_, ref mut path)) = draft_paths
                                .iter_mut()
                                .find(|(oid, _)| *oid == nref.object_id)
                            {
                                if let Some(sp) = path.subpaths.get_mut(nref.subpath_index) {
                                    if let Some(pnode) = sp.nodes.get_mut(nref.node_index) {
                                        pnode.translate(local_delta);
                                    }
                                }
                            }
                        }
                    }
                    NodeDragPart::HandleIn | NodeDragPart::HandleOut => {
                        if is_object_editable_in_doc(ctx.document, node.object_id) {
                            let inv = world_inverse_transform_of(ctx.document, node.object_id);
                            let local_pt = inv.transform_point(pt);
                            if let Some((_, ref mut path)) = draft_paths
                                .iter_mut()
                                .find(|(oid, _)| *oid == node.object_id)
                            {
                                if let Some(sp) = path.subpaths.get_mut(node.subpath_index) {
                                    if let Some(pnode) = sp.nodes.get_mut(node.node_index) {
                                        if *handle_part == NodeDragPart::HandleIn {
                                            pnode.handle_in = Some(local_pt);
                                            pnode.enforce_continuity_for_handle(true);
                                        } else {
                                            pnode.handle_out = Some(local_pt);
                                            pnode.enforce_continuity_for_handle(false);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ActiveDragState::DraggingCurveSegment {
                object_id,
                subpath_index,
                seg_index,
                t,
                last_local_mm,
                draft_path,
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let local_pt = inv.transform_point(pt);
                    let delta = local_pt - *last_local_mm;
                    *last_local_mm = local_pt;
                    let _ = draft_path.convert_segment_to_curve(*subpath_index, *seg_index);
                    if let Some(sp) = draft_path.subpaths.get_mut(*subpath_index) {
                        if let Some((i0, i1)) = sp.segment_node_indices(*seg_index) {
                            let w0 = (1.0 - *t).clamp(0.15, 0.85) * 1.45;
                            let w1 = (*t).clamp(0.15, 0.85) * 1.45;
                            if let Some(h0) = sp.nodes[i0].handle_out.as_mut() {
                                *h0 = *h0 + delta * w0;
                            }
                            sp.nodes[i0].enforce_continuity_for_handle(false);
                            if let Some(h1) = sp.nodes[i1].handle_in.as_mut() {
                                *h1 = *h1 + delta * w1;
                            }
                            sp.nodes[i1].enforce_continuity_for_handle(true);
                        }
                    }
                }
            }
            ActiveDragState::DraggingBSplineControlPoint {
                object_id,
                cp_index,
                draft_bspline,
                ..
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let lp = inv.transform_point(pt);
                    if let Some(cp) = draft_bspline.control_points.get_mut(*cp_index) {
                        *cp = lp;
                    }
                }
            }
            ActiveDragState::DraggingArtisticMediaNode {
                object_id,
                subpath_index,
                node_index,
                handle_part,
                draft_spine,
                ..
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let local_pt = inv.transform_point(pt);
                    if let Some(sp) = draft_spine.subpaths.get_mut(*subpath_index) {
                        if let Some(pnode) = sp.nodes.get_mut(*node_index) {
                            match *handle_part {
                                NodeDragPart::Anchor => {
                                    let delta = local_pt - pnode.pos;
                                    pnode.translate(delta);
                                }
                                NodeDragPart::HandleIn => {
                                    pnode.handle_in = Some(local_pt);
                                    pnode.enforce_continuity_for_handle(true);
                                }
                                NodeDragPart::HandleOut => {
                                    pnode.handle_out = Some(local_pt);
                                    pnode.enforce_continuity_for_handle(false);
                                }
                            }
                        }
                    }
                }
            }
            ActiveDragState::DraggingParametricHandle {
                object_id,
                handle,
                draft_shape,
                ..
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let lp = inv.transform_point(pt);
                    update_parametric_shape_live(draft_shape, *handle, lp, ctrl, shift);
                }
            }
            ActiveDragState::Sculpting {
                object_id,
                draft_path,
                last_mm,
                points_mm,
                step_counter,
                ..
            } => {
                if last_mm.distance_to(pt) >= 0.35
                    && is_object_editable_in_doc(ctx.document, *object_id)
                {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let scale_est =
                        ((inv.m[0].hypot(inv.m[1]) + inv.m[2].hypot(inv.m[3])) * 0.5).max(1e-3);
                    let local_r = sculpt_radius * scale_est;
                    let lp = inv.transform_point(pt);
                    let prev_lp = inv.transform_point(*last_mm);
                    match active_tool {
                        ToolId::Smooth => draft_path.smooth_near(lp, local_r, sculpt_strength),
                        ToolId::Smear => draft_path.smear_near(lp, lp - prev_lp, local_r),
                        ToolId::Twirl => {
                            let sign = if shift { -1.0 } else { 1.0 };
                            draft_path.twirl_near(
                                lp,
                                local_r,
                                sign * (0.25 * sculpt_strength + 0.08),
                            );
                        }
                        ToolId::AttractRepel => {
                            draft_path.attract_repel_near(lp, local_r, sculpt_strength, !shift);
                        }
                        ToolId::Roughen => {
                            let amp = (local_r * 0.16 * sculpt_strength).max(0.35);
                            draft_path.roughen_near(
                                lp,
                                local_r,
                                amp,
                                0x51A7_u64.wrapping_add(*step_counter),
                            );
                        }
                        _ => {}
                    }
                    *step_counter = step_counter.saturating_add(1);
                    *last_mm = pt;
                    points_mm.push(pt);
                }
            }
            ActiveDragState::CropBoxHandle { handle, orig_rect } => {
                let mut r = *orig_rect;
                match handle {
                    ScaleHandle::TopLeft => {
                        r.min_x = pt.x.min(r.max_x - 2.0);
                        r.min_y = pt.y.min(r.max_y - 2.0);
                    }
                    ScaleHandle::Top => r.min_y = pt.y.min(r.max_y - 2.0),
                    ScaleHandle::TopRight => {
                        r.max_x = pt.x.max(r.min_x + 2.0);
                        r.min_y = pt.y.min(r.max_y - 2.0);
                    }
                    ScaleHandle::Right => r.max_x = pt.x.max(r.min_x + 2.0),
                    ScaleHandle::BottomRight => {
                        r.max_x = pt.x.max(r.min_x + 2.0);
                        r.max_y = pt.y.max(r.min_y + 2.0);
                    }
                    ScaleHandle::Bottom => r.max_y = pt.y.max(r.min_y + 2.0),
                    ScaleHandle::BottomLeft => {
                        r.min_x = pt.x.min(r.max_x - 2.0);
                        r.max_y = pt.y.max(r.min_y + 2.0);
                    }
                    ScaleHandle::Left => r.min_x = pt.x.min(r.max_x - 2.0),
                }
                self.pending_crop_rect = Some(r);
                post_actions.push(ToolActionRequest::SetPendingCropRect(Some(r)));
            }
            ActiveDragState::PullingBezierHandle { handle_out_mm, .. } => {
                *handle_out_mm = pt;
            }
            ActiveDragState::DraggingGradientHandle {
                object_id,
                is_end,
                draft_fill,
                ..
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let local_pt = inv.transform_point(pt);
                    match draft_fill {
                        Fill::LinearGradient { start, end, .. } => {
                            if *is_end {
                                *end = local_pt;
                            } else {
                                *start = local_pt;
                            }
                        }
                        Fill::RadialGradient {
                            center,
                            radius,
                            focal,
                            ..
                        } => {
                            if *is_end {
                                *radius = center.distance_to(local_pt).max(0.5);
                            } else {
                                let delta = local_pt - *center;
                                *center = local_pt;
                                *focal = *focal + delta;
                            }
                        }
                        Fill::ConicalGradient {
                            center, angle_deg, ..
                        } => {
                            if *is_end {
                                let d = local_pt - *center;
                                if d.length() > 1e-3 {
                                    *angle_deg = d.y.atan2(d.x).to_degrees();
                                }
                            } else {
                                *center = local_pt;
                            }
                        }
                        Fill::SquareGradient {
                            center,
                            size,
                            angle_deg,
                            ..
                        } => {
                            if *is_end {
                                let d = local_pt - *center;
                                *size = (d.length() * 2.0).max(1.0);
                                if d.length() > 1e-3 {
                                    *angle_deg = d.y.atan2(d.x).to_degrees();
                                }
                            } else {
                                *center = local_pt;
                            }
                        }
                        _ => {}
                    }
                }
            }
            ActiveDragState::DraggingGradientFocal {
                object_id,
                draft_fill,
                ..
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let local_pt = inv.transform_point(pt);
                    if let Fill::RadialGradient { focal, .. } = draft_fill {
                        *focal = local_pt;
                    }
                }
            }
            ActiveDragState::DraggingGradientStop {
                object_id,
                stop_index,
                draft_fill,
                ..
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let local_pt = inv.transform_point(pt);
                    if let Some((start, end, stops)) = gradient_stops_mut(draft_fill) {
                        let v = end - start;
                        let len_sq = v.length_sq().max(1e-6);
                        let t = ((local_pt - start).dot(v) / len_sq).clamp(0.01, 0.99) as f32;
                        if let Some(st) = stops.get_mut(*stop_index) {
                            st.offset = t;
                        }
                    }
                }
            }
            ActiveDragState::DraggingTransparencyHandle {
                object_id,
                is_end,
                draft_effect,
                ..
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let local_pt = inv.transform_point(pt);
                    if let Effect::Transparency {
                        mode: TransparencyMode::FountainLinear { start, end, .. },
                        ..
                    } = draft_effect
                    {
                        if *is_end {
                            *end = local_pt;
                        } else {
                            *start = local_pt;
                        }
                    }
                }
            }
            ActiveDragState::DraggingEnvelopeHandle {
                object_id,
                handle_index,
                draft_effect,
                ..
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let local_pt = inv.transform_point(pt);
                    if let Effect::Envelope { cage, .. } = draft_effect {
                        if let Some(slot) = cage.get_mut(*handle_index) {
                            *slot = local_pt;
                        }
                    }
                }
            }
            ActiveDragState::DraggingMeshNode {
                object_id,
                node_index,
                draft_fill,
                ..
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let local_pt = inv.transform_point(pt);
                    if let Fill::MeshFill { nodes, .. } = draft_fill {
                        if let Some(mn) = nodes.get_mut(*node_index) {
                            mn.pos = local_pt;
                        }
                    }
                }
            }
            ActiveDragState::DraggingMeshHandle {
                object_id,
                node_index,
                handle_slot,
                draft_fill,
                ..
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let local_pt = inv.transform_point(pt);
                    if let Fill::MeshFill { nodes, .. } = draft_fill {
                        if let Some(mn) = nodes.get_mut(*node_index) {
                            let rel = Point2D::new(local_pt.x - mn.pos.x, local_pt.y - mn.pos.y);
                            match *handle_slot {
                                0 => mn.handle_n = Some(rel),
                                1 => mn.handle_s = Some(rel),
                                2 => mn.handle_e = Some(rel),
                                _ => mn.handle_w = Some(rel),
                            }
                        }
                    }
                }
            }
            ActiveDragState::DraggingEffectHandle {
                object_id,
                draft_effect,
                ..
            } => {
                if is_object_editable_in_doc(ctx.document, *object_id) {
                    let obj_center = ctx
                        .document
                        .world_object_bounds(*object_id)
                        .map_or(Point2D::ZERO, |b| b.center());
                    let inv = world_inverse_transform_of(ctx.document, *object_id);
                    let local_pt = inv.transform_point(pt);
                    match draft_effect {
                        Effect::DropShadow { dx_mm, dy_mm, .. }
                        | Effect::BlockShadow { dx_mm, dy_mm, .. } => {
                            *dx_mm = (pt.x - obj_center.x).clamp(-150.0, 150.0);
                            *dy_mm = (pt.y - obj_center.y).clamp(-150.0, 150.0);
                        }
                        Effect::Contour { offset_mm, .. } => {
                            *offset_mm = obj_center.distance_to(pt).clamp(0.2, 60.0);
                        }
                        Effect::Extrude25D {
                            vanishing_point, ..
                        } => {
                            *vanishing_point = pt;
                        }
                        Effect::Distort {
                            amplitude, center, ..
                        } => {
                            *amplitude = center.distance_to(local_pt).clamp(0.5, 120.0);
                        }
                        Effect::Blend { rotation_deg, .. } => {
                            let d = pt - obj_center;
                            if d.length() > 1e-3 {
                                *rotation_deg = d.y.atan2(d.x).to_degrees();
                            }
                        }
                        _ => {}
                    }
                }
            }
            ActiveDragState::DraggingCanvasGuide {
                guide_id,
                draft_orientation,
                ..
            } => {
                if let Some(g) = ctx.document.guide(*guide_id) {
                    if !g.locked {
                        match draft_orientation {
                            GuideOrientation::Horizontal(y) => *y = pt.y,
                            GuideOrientation::Vertical(x) => *x = pt.x,
                            GuideOrientation::Angled { origin, .. } => *origin = pt,
                        }
                    }
                }
            }
            ActiveDragState::DrawingBox {
                current_mm,
                ctrl: d_ctrl,
                shift: d_shift,
                ..
            } => {
                let (snapped, guides) = SnapEngine::snap_point(
                    ctx.document,
                    ctx.document.active_page_index(),
                    pt,
                    &[],
                    ctx.snap_options,
                );
                self.active_guides = guides;
                *current_mm = snapped;
                *d_ctrl = ctrl;
                *d_shift = shift;
            }
            ActiveDragState::DrawingStroke { points_mm } => {
                let next_pt =
                    if active_tool == ToolId::TwoPointLine && ctrl && !points_mm.is_empty() {
                        constrain_line_15deg(points_mm[0], pt)
                    } else {
                        pt
                    };
                if active_tool == ToolId::TwoPointLine && points_mm.len() >= 2 {
                    *points_mm.last_mut().expect("len >= 2") = next_pt;
                } else if points_mm
                    .last()
                    .is_none_or(|last| last.distance_to(next_pt) > 0.35)
                {
                    points_mm.push(next_pt);
                }
            }
            ActiveDragState::Idle | ActiveDragState::DraggingRulerGuide { .. } => {}
        }
        self.drag_state = state;

        let overlay = self.finalize_overlay(self.build_overlay(ctx));
        if post_actions.is_empty() {
            ToolOutput::Continue { overlay }
        } else {
            ToolOutput::Consumed {
                overlay: Some(overlay),
                actions: post_actions,
            }
        }
    }

    /// Completes an active gesture, emitting atomic [`CommandOp`]s (or non-mutating UI actions).
    pub fn on_pointer_up(&mut self, ctx: &ToolReadContext<'_>, ev: &PointerEvent) -> ToolOutput {
        self.cursor_world_mm = ev.world_pt_mm;
        let pt = ev.world_pt_mm;
        let ctrl = ev.modifiers.ctrl;
        let shift = ev.modifiers.shift;
        let active_tool = self.active_tool;

        let state = std::mem::take(&mut self.drag_state);
        self.active_guides.clear();
        self.prev_dirty_bbox_mm = None;

        match state {
            ActiveDragState::Panning { .. } => {
                ToolOutput::actions(vec![ToolActionRequest::PanTo {
                    pan_mm: ctx.viewport.pan_mm,
                    lod_dragging: false,
                }])
            }
            ActiveDragState::MarqueeSelect {
                start_mm,
                current_mm,
            } => {
                let rect = Rect2D::from_points(start_mm, current_mm);
                if rect.width() < 0.5 && rect.height() < 0.5 {
                    let tol = ctx.hit_tol_mm(6.0, 0.8);
                    let hit = hit_test_canvas_point_scoped_opt_tol(ctx, pt, tol);
                    if let Some(hid) = hit {
                        ToolOutput::actions(vec![ToolActionRequest::SelectObjects {
                            ids: vec![hid],
                            additive: false,
                            toggle: shift,
                        }])
                    } else if !shift {
                        ToolOutput::actions(vec![ToolActionRequest::ClearSelection])
                    } else {
                        ToolOutput::noop()
                    }
                } else {
                    let hits = if let Some(pc_id) = ctx.powerclip_edit_id {
                        hit_test_powerclip_contents_rect(ctx.document, pc_id, rect, false)
                    } else {
                        hit_test_rect(ctx.document, ctx.document.active_page_index(), rect, false)
                    };
                    ToolOutput::actions(vec![ToolActionRequest::SelectObjects {
                        ids: hits,
                        additive: shift,
                        toggle: false,
                    }])
                }
            }
            ActiveDragState::LassoSelect { points_mm } => {
                if points_mm.len() < 3 {
                    if let Some(hid) = hit_test_canvas_point_scoped_opt(ctx, pt) {
                        ToolOutput::actions(vec![ToolActionRequest::SelectObjects {
                            ids: vec![hid],
                            additive: false,
                            toggle: shift,
                        }])
                    } else if !shift {
                        ToolOutput::actions(vec![ToolActionRequest::ClearSelection])
                    } else {
                        ToolOutput::noop()
                    }
                } else {
                    let hits =
                        compute_lasso_hits_scoped(ctx.document, ctx.powerclip_edit_id, &points_mm);
                    ToolOutput::actions(vec![ToolActionRequest::SelectObjects {
                        ids: hits,
                        additive: shift,
                        toggle: false,
                    }])
                }
            }
            ActiveDragState::NodeMarqueeSelect {
                start_mm,
                current_mm,
            } => {
                let rect = Rect2D::from_points(start_mm, current_mm);
                if rect.width() < 0.5 && rect.height() < 0.5 {
                    let tol = ctx.hit_tol_mm(8.0, 1.5);
                    if let Some(nh) = hit_test_node(ctx.document, &ctx.selection.objects, pt, tol) {
                        return ToolOutput::actions(vec![ToolActionRequest::SelectNodes {
                            nodes: vec![nh.node],
                            additive: shift,
                        }]);
                    }
                    if let Some(oid) = hit_test_canvas_point_scoped_opt(ctx, pt) {
                        return ToolOutput::actions(vec![
                            ToolActionRequest::SelectObjects {
                                ids: vec![oid],
                                additive: false,
                                toggle: false,
                            },
                            ToolActionRequest::ClearNodeSelection,
                        ]);
                    }
                    ToolOutput::noop()
                } else if ctx.selection.is_empty() {
                    let hits = if let Some(pc_id) = ctx.powerclip_edit_id {
                        hit_test_powerclip_contents_rect(ctx.document, pc_id, rect, false)
                    } else {
                        hit_test_rect(ctx.document, ctx.document.active_page_index(), rect, false)
                    };
                    ToolOutput::actions(vec![ToolActionRequest::SelectObjects {
                        ids: hits,
                        additive: false,
                        toggle: false,
                    }])
                } else {
                    let nodes = collect_nodes_in_rect(ctx.document, &ctx.selection.objects, rect);
                    ToolOutput::actions(vec![ToolActionRequest::SelectNodes {
                        nodes,
                        additive: shift,
                    }])
                }
            }
            ActiveDragState::ZoomMarquee {
                start_mm,
                current_mm,
            } => {
                let rect = Rect2D::from_points(start_mm, current_mm);
                if rect.width() >= 1.0 && rect.height() >= 1.0 {
                    ToolOutput::actions(vec![ToolActionRequest::ZoomFitRect {
                        rect_mm: rect,
                        margin_px: 12.0,
                        one_shot_complete: true,
                    }])
                } else {
                    let local = ctx.viewport.world_to_screen(pt);
                    let factor = if shift { 1.0 / 1.5 } else { 1.5 };
                    ToolOutput::actions(vec![ToolActionRequest::ZoomAtScreen {
                        screen_px: local,
                        factor,
                        one_shot_complete: true,
                    }])
                }
            }
            ActiveDragState::MovingSelection {
                object_ids,
                accum_dx,
                accum_dy,
                pending_click,
                ..
            } => {
                if accum_dx.abs() > 1e-5 || accum_dy.abs() > 1e-5 {
                    let world_fwd = Affine2D::translate(accum_dx, accum_dy);
                    let target_ids = if object_ids.is_empty() {
                        &ctx.selection.objects
                    } else {
                        &object_ids
                    };
                    if let Some(cmd) =
                        build_world_transform_command(ctx.document, target_ids, world_fwd)
                    {
                        return ToolOutput::Commit {
                            ops: vec![cmd],
                            clear_overlay: true,
                            post_actions: Vec::new(),
                        };
                    }
                } else if let Some((cid, click_shift)) = pending_click {
                    return ToolOutput::actions(vec![ToolActionRequest::SelectObjects {
                        ids: vec![cid],
                        additive: false,
                        toggle: click_shift,
                    }]);
                }
                ToolOutput::noop()
            }
            ActiveDragState::ScalingSelection {
                orig_transforms,
                last_affine,
                ..
            }
            | ActiveDragState::RotatingSelection {
                orig_transforms,
                last_affine,
                ..
            }
            | ActiveDragState::SkewingSelection {
                orig_transforms,
                last_affine,
                ..
            }
            | ActiveDragState::FreeTransforming {
                orig_transforms,
                last_affine,
                ..
            } => {
                if !orig_transforms.is_empty() && last_affine != Affine2D::IDENTITY {
                    let ids: Vec<ObjectId> = orig_transforms.iter().map(|(id, _)| *id).collect();
                    if let Some(cmd) =
                        build_world_transform_command(ctx.document, &ids, last_affine)
                    {
                        return ToolOutput::Commit {
                            ops: vec![cmd],
                            clear_overlay: true,
                            post_actions: Vec::new(),
                        };
                    }
                }
                ToolOutput::noop()
            }
            ActiveDragState::DraggingNode { draft_paths, .. } => {
                let mut ops = Vec::new();
                for (oid, edited) in draft_paths {
                    let unchanged = ctx.document.object(oid).is_some_and(
                        |o| matches!(&o.payload, NodePayload::Path(orig) if *orig == edited),
                    );
                    if !unchanged {
                        ops.push(CommandOp::EditPath {
                            id: oid,
                            path: edited,
                        });
                    }
                }
                if ops.is_empty() {
                    ToolOutput::noop()
                } else {
                    ToolOutput::Commit {
                        ops,
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    }
                }
            }
            ActiveDragState::DraggingCurveSegment {
                object_id,
                draft_path,
                ..
            } => {
                let unchanged = ctx.document.object(object_id).is_some_and(
                    |o| matches!(&o.payload, NodePayload::Path(orig) if *orig == draft_path),
                );
                if unchanged {
                    ToolOutput::noop()
                } else {
                    ToolOutput::Commit {
                        ops: vec![CommandOp::EditPath {
                            id: object_id,
                            path: draft_path,
                        }],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    }
                }
            }
            ActiveDragState::DraggingParametricHandle {
                object_id,
                orig_shape,
                draft_shape,
                ..
            } => {
                if orig_shape == draft_shape || !is_object_editable_in_doc(ctx.document, object_id)
                {
                    ToolOutput::noop()
                } else {
                    ToolOutput::Commit {
                        ops: vec![CommandOp::EditShape {
                            id: object_id,
                            shape: draft_shape,
                        }],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    }
                }
            }
            ActiveDragState::DraggingBSplineControlPoint {
                object_id,
                orig_bspline,
                draft_bspline,
                ..
            } => {
                if orig_bspline == draft_bspline
                    || !is_object_editable_in_doc(ctx.document, object_id)
                {
                    ToolOutput::noop()
                } else {
                    ToolOutput::Commit {
                        ops: vec![CommandOp::EditBSpline {
                            id: object_id,
                            bspline: draft_bspline,
                        }],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    }
                }
            }
            ActiveDragState::DraggingArtisticMediaNode {
                object_id,
                orig_spine,
                draft_spine,
                style,
                ..
            } => {
                if orig_spine == draft_spine || !is_object_editable_in_doc(ctx.document, object_id)
                {
                    ToolOutput::noop()
                } else {
                    ToolOutput::Commit {
                        ops: vec![CommandOp::EditArtisticMedia {
                            id: object_id,
                            spine: draft_spine,
                            style,
                        }],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    }
                }
            }
            ActiveDragState::Sculpting {
                object_id,
                was_shape,
                draft_path,
                points_mm,
                step_counter,
                ..
            } => {
                if step_counter == 0 {
                    return crate::builders::build_sculpt_stroke_output(
                        ctx,
                        active_tool,
                        &points_mm,
                        shift,
                    );
                }
                let Some(obj) = ctx.document.object(object_id) else {
                    return ToolOutput::noop();
                };
                if !is_object_editable_in_doc(ctx.document, object_id) {
                    return ToolOutput::noop();
                }
                let unchanged = match &obj.payload {
                    NodePayload::Path(orig) => *orig == draft_path,
                    NodePayload::Shape(s) => s.to_bezier_path() == draft_path,
                    NodePayload::BSpline(bs) => bs.to_bezier_path() == draft_path,
                    _ => return ToolOutput::noop(),
                };
                if unchanged {
                    return ToolOutput::noop();
                }
                let _ = was_shape;
                let op = crate::builders::build_edit_path_op(ctx.document, object_id, draft_path);
                ToolOutput::Commit {
                    ops: vec![op],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                }
            }
            ActiveDragState::PullingBezierHandle {
                anchor_mm,
                handle_out_mm,
                closing,
            } => {
                let handle = (handle_out_mm.distance_to(anchor_mm) > 0.35).then_some(handle_out_mm);
                if closing {
                    if let (Some(first), Some(ho)) = (self.bezier_draft_nodes.first_mut(), handle) {
                        let v = ho - first.pos;
                        first.handle_out = Some(ho);
                        first.handle_in = Some(first.pos - v);
                        first.kind = NodeKind::Symmetrical;
                    }
                    if let Some(op) = self.bezier_finish_op(ctx, true) {
                        return ToolOutput::Commit {
                            ops: vec![op],
                            clear_overlay: true,
                            post_actions: Vec::new(),
                        };
                    }
                } else {
                    self.bezier_add_point(anchor_mm, handle);
                }
                let overlay = self.finalize_overlay(self.build_overlay(ctx));
                ToolOutput::Continue { overlay }
            }
            ActiveDragState::DraggingGradientHandle {
                object_id,
                orig_fill,
                mut draft_fill,
                ..
            }
            | ActiveDragState::DraggingGradientFocal {
                object_id,
                orig_fill,
                mut draft_fill,
            }
            | ActiveDragState::DraggingGradientStop {
                object_id,
                orig_fill,
                mut draft_fill,
                ..
            }
            | ActiveDragState::DraggingMeshNode {
                object_id,
                orig_fill,
                mut draft_fill,
                ..
            }
            | ActiveDragState::DraggingMeshHandle {
                object_id,
                orig_fill,
                mut draft_fill,
                ..
            } => {
                if let Some((_, _, stops)) = gradient_stops_mut(&mut draft_fill) {
                    stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
                }
                if draft_fill == orig_fill {
                    ToolOutput::noop()
                } else {
                    ToolOutput::Commit {
                        ops: vec![CommandOp::SetFill {
                            ids: vec![object_id],
                            fill: draft_fill,
                        }],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    }
                }
            }
            ActiveDragState::DraggingTransparencyHandle {
                object_id,
                effect_index,
                orig_effect,
                draft_effect,
                ..
            }
            | ActiveDragState::DraggingEnvelopeHandle {
                object_id,
                effect_index,
                orig_effect,
                draft_effect,
                ..
            }
            | ActiveDragState::DraggingEffectHandle {
                object_id,
                effect_index,
                orig_effect,
                draft_effect,
            } => {
                if draft_effect == orig_effect {
                    ToolOutput::noop()
                } else {
                    ToolOutput::Commit {
                        ops: vec![build_replace_effect_op(
                            object_id,
                            Some(effect_index),
                            draft_effect,
                        )],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    }
                }
            }
            ActiveDragState::DraggingCanvasGuide {
                guide_id,
                orig_orientation,
                draft_orientation,
            } => {
                if draft_orientation != orig_orientation {
                    ToolOutput::Commit {
                        ops: vec![CommandOp::MoveGuide {
                            id: guide_id,
                            orientation: draft_orientation,
                        }],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    }
                } else {
                    ToolOutput::noop()
                }
            }
            ActiveDragState::DraggingInteractiveEffect {
                object_id,
                start_mm,
                current_mm,
            } => {
                if active_tool == ToolId::Envelope && start_mm.distance_to(current_mm) < 0.5 {
                    let mut actions = Vec::new();
                    if !ctx.selection.contains(object_id) {
                        actions.push(ToolActionRequest::SelectObjects {
                            ids: vec![object_id],
                            additive: false,
                            toggle: false,
                        });
                    }
                    actions.push(ToolActionRequest::EnsureEnvelopeOnObject { object_id });
                    ToolOutput::actions(actions)
                } else {
                    build_interactive_effect_output(ctx, active_tool, start_mm, current_mm)
                }
            }
            ActiveDragState::DrawingBox {
                start_mm,
                current_mm,
                ..
            } => self.finish_drawn_box(ctx, active_tool, start_mm, current_mm, ctrl, shift),
            ActiveDragState::DrawingStroke { points_mm } => {
                self.finish_drawn_stroke(ctx, active_tool, points_mm, shift)
            }
            ActiveDragState::Idle | ActiveDragState::DraggingRulerGuide { .. } => {
                self.handle_zero_drag_click(ctx, pt, shift)
            }
            ActiveDragState::DraggingPivot | ActiveDragState::CropBoxHandle { .. } => {
                ToolOutput::noop()
            }
        }
    }

    fn handle_zero_drag_click(
        &mut self,
        ctx: &ToolReadContext<'_>,
        pt: Point2D,
        shift: bool,
    ) -> ToolOutput {
        match self.active_tool {
            ToolId::ArtisticText => {
                let tol = ctx.hit_tol_mm(6.0, 1.2);
                let existing = hit_test_canvas_point_opt_tol(ctx, pt, tol).filter(|oid| {
                    ctx.document.object(*oid).is_some_and(|o| {
                        matches!(
                            o.payload,
                            NodePayload::ArtisticText(_) | NodePayload::ParagraphText(_)
                        )
                    })
                });
                ToolOutput::actions(vec![ToolActionRequest::OpenInlineTextEdit {
                    object_id: existing,
                    world_pos_mm: pt,
                    is_paragraph: false,
                    box_size_mm: Vec2D::new(90.0, 18.0),
                }])
            }
            ToolId::ParagraphText => {
                let tol = ctx.hit_tol_mm(6.0, 1.2);
                let existing = hit_test_canvas_point_opt_tol(ctx, pt, tol).filter(|oid| {
                    ctx.document.object(*oid).is_some_and(|o| {
                        matches!(
                            o.payload,
                            NodePayload::ParagraphText(_) | NodePayload::ArtisticText(_)
                        )
                    })
                });
                ToolOutput::actions(vec![ToolActionRequest::OpenInlineTextEdit {
                    object_id: existing,
                    world_pos_mm: pt,
                    is_paragraph: true,
                    box_size_mm: Vec2D::new(80.0, 40.0),
                }])
            }
            ToolId::SmartFill => {
                ToolOutput::actions(vec![ToolActionRequest::SmartFillAt { pt_mm: pt }])
            }
            ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
                ToolOutput::actions(vec![ToolActionRequest::ApplyEyedropper {
                    pt_mm: pt,
                    attributes_mode: self.active_tool == ToolId::AttributesEyedropper,
                    force_sample: shift,
                }])
            }
            ToolId::Envelope => {
                let hit = if !ctx.selection.objects.is_empty() {
                    resolve_selected_object_at_or_first(ctx, pt, 6.0)
                } else {
                    hit_test_canvas_point_opt_tol(ctx, pt, 6.0)
                };
                if let Some(oid) = hit {
                    let mut actions = Vec::new();
                    if !ctx.selection.contains(oid) {
                        actions.push(ToolActionRequest::SelectObjects {
                            ids: vec![oid],
                            additive: false,
                            toggle: false,
                        });
                    }
                    actions.push(ToolActionRequest::EnsureEnvelopeOnObject { object_id: oid });
                    ToolOutput::actions(actions)
                } else {
                    ToolOutput::noop()
                }
            }
            ToolId::MeshFill => build_mesh_fill_output(ctx, pt, pt, shift),
            ToolId::LensTool => {
                if let Some(oid) = hit_test_canvas_point_opt_tol(ctx, pt, 6.0) {
                    ToolOutput::actions(vec![
                        ToolActionRequest::SelectObjects {
                            ids: vec![oid],
                            additive: false,
                            toggle: false,
                        },
                        ToolActionRequest::ApplyLensToObject { object_id: oid },
                    ])
                } else {
                    ToolOutput::noop()
                }
            }
            _ => ToolOutput::noop(),
        }
    }

    fn finish_drawn_box(
        &mut self,
        ctx: &ToolReadContext<'_>,
        active_tool: ToolId,
        start_mm: Point2D,
        current_mm: Point2D,
        ctrl: bool,
        shift: bool,
    ) -> ToolOutput {
        match active_tool {
            ToolId::InteractiveFill => {
                if let Some(op) = build_interactive_fill_op(ctx, start_mm, current_mm) {
                    return ToolOutput::Commit {
                        ops: vec![op],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    };
                }
                ToolOutput::noop()
            }
            ToolId::MeshFill => build_mesh_fill_output(ctx, start_mm, current_mm, shift),
            ToolId::StraightConnector | ToolId::RightAngleConnector => {
                if let Some(op) = build_connector_op(
                    ctx,
                    start_mm,
                    current_mm,
                    active_tool == ToolId::RightAngleConnector,
                ) {
                    ToolOutput::Commit {
                        ops: vec![op],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    }
                } else {
                    ToolOutput::noop()
                }
            }
            ToolId::ThreePointCurve => {
                if start_mm.distance_to(current_mm) >= 1.0 {
                    self.three_point_curve_chord = Some((start_mm, current_mm));
                    let overlay = self.finalize_overlay(self.build_overlay(ctx));
                    return ToolOutput::Consumed {
                        overlay: Some(overlay),
                        actions: vec![ToolActionRequest::Toast {
                            message: "Move cursor to bend arc, then click to commit 3-Point Curve"
                                .to_owned(),
                            is_error: false,
                        }],
                    };
                }
                ToolOutput::noop()
            }
            ToolId::ThreePointRectangle | ToolId::ThreePointEllipse => {
                if start_mm.distance_to(current_mm) >= 1.0 {
                    self.three_point_shape_chord = Some((active_tool, start_mm, current_mm));
                    let overlay = self.finalize_overlay(self.build_overlay(ctx));
                    return ToolOutput::Consumed {
                        overlay: Some(overlay),
                        actions: vec![ToolActionRequest::Toast {
                            message: "Move cursor perpendicular to baseline, then click to finish"
                                .to_owned(),
                            is_error: false,
                        }],
                    };
                }
                ToolOutput::noop()
            }
            ToolId::Crop => {
                let rect = compute_constrained_box(start_mm, current_mm, ctrl, shift);
                if rect.width() >= 2.0 && rect.height() >= 2.0 {
                    self.pending_crop_rect = Some(rect);
                    let overlay = self.finalize_overlay(self.build_overlay(ctx));
                    return ToolOutput::Consumed {
                        overlay: Some(overlay),
                        actions: vec![
                            ToolActionRequest::SetPendingCropRect(Some(rect)),
                            ToolActionRequest::Toast {
                                message: "Adjust crop handles, then press Enter or double-click inside to Crop".to_owned(),
                                is_error: false,
                            },
                        ],
                    };
                }
                ToolOutput::noop()
            }
            ToolId::ParagraphText => {
                let rect = compute_constrained_box(start_mm, current_mm, ctrl, shift);
                if rect.width() >= 4.0 && rect.height() >= 4.0 {
                    return ToolOutput::actions(vec![ToolActionRequest::OpenInlineTextEdit {
                        object_id: None,
                        world_pos_mm: rect.min(),
                        is_paragraph: true,
                        box_size_mm: Vec2D::new(rect.width(), rect.height()),
                    }]);
                }
                let tol = ctx.hit_tol_mm(6.0, 1.2);
                let existing = hit_test_canvas_point_opt_tol(ctx, start_mm, tol).filter(|oid| {
                    ctx.document.object(*oid).is_some_and(|o| {
                        matches!(
                            o.payload,
                            NodePayload::ParagraphText(_) | NodePayload::ArtisticText(_)
                        )
                    })
                });
                ToolOutput::actions(vec![ToolActionRequest::OpenInlineTextEdit {
                    object_id: existing,
                    world_pos_mm: start_mm,
                    is_paragraph: true,
                    box_size_mm: Vec2D::new(80.0, 40.0),
                }])
            }
            ToolId::ArtisticText => {
                let tol = ctx.hit_tol_mm(6.0, 1.2);
                let existing = hit_test_canvas_point_opt_tol(ctx, start_mm, tol).filter(|oid| {
                    ctx.document.object(*oid).is_some_and(|o| {
                        matches!(
                            o.payload,
                            NodePayload::ArtisticText(_) | NodePayload::ParagraphText(_)
                        )
                    })
                });
                ToolOutput::actions(vec![ToolActionRequest::OpenInlineTextEdit {
                    object_id: existing,
                    world_pos_mm: start_mm,
                    is_paragraph: false,
                    box_size_mm: Vec2D::new(90.0, 18.0),
                }])
            }
            ToolId::SmartFill => {
                ToolOutput::actions(vec![ToolActionRequest::SmartFillAt { pt_mm: start_mm }])
            }
            ToolId::ColorEyedropper | ToolId::AttributesEyedropper => {
                ToolOutput::actions(vec![ToolActionRequest::ApplyEyedropper {
                    pt_mm: start_mm,
                    attributes_mode: active_tool == ToolId::AttributesEyedropper,
                    force_sample: shift,
                }])
            }
            ToolId::LensTool if start_mm.distance_to(current_mm) < 1.0 => {
                if let Some(oid) = hit_test_canvas_point_opt_tol(ctx, start_mm, 6.0) {
                    ToolOutput::actions(vec![
                        ToolActionRequest::SelectObjects {
                            ids: vec![oid],
                            additive: false,
                            toggle: false,
                        },
                        ToolActionRequest::ApplyLensToObject { object_id: oid },
                    ])
                } else {
                    ToolOutput::noop()
                }
            }
            _ => {
                let rect = compute_constrained_box(start_mm, current_mm, ctrl, shift);
                build_drawn_box_output(ctx, active_tool, rect)
            }
        }
    }

    fn finish_drawn_stroke(
        &mut self,
        ctx: &ToolReadContext<'_>,
        tool: ToolId,
        points: Vec<Point2D>,
        shift: bool,
    ) -> ToolOutput {
        if points.len() < 2 {
            if points.len() == 1 {
                match tool {
                    ToolId::VirtualSegmentDelete => {
                        let ops = build_virtual_segment_delete_stroke_ops(ctx, &points);
                        if !ops.is_empty() {
                            return ToolOutput::Commit {
                                ops,
                                clear_overlay: true,
                                post_actions: Vec::new(),
                            };
                        }
                    }
                    ToolId::BitmapBrush
                    | ToolId::BitmapEraser
                    | ToolId::CloneStamp
                    | ToolId::Smudge => {
                        return ToolOutput::actions(vec![ToolActionRequest::ApplyRasterStroke {
                            tool,
                            points_mm: points,
                        }]);
                    }
                    _ => {}
                }
            }
            return ToolOutput::noop();
        }
        match tool {
            ToolId::TwoPointLine => {
                let Some(layer_id) = ctx.target_creation_layer_id() else {
                    return ToolOutput::noop();
                };
                let path = BezierPath::from_line(points[0], *points.last().expect("len >= 2"));
                let obj = SceneObject::new(ObjectId(0), "Line", NodePayload::Path(path))
                    .with_fill(Fill::None)
                    .with_outline(ctx.default_outline.clone());
                ToolOutput::Commit {
                    ops: vec![CommandOp::create_object(obj, Some(layer_id))],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                }
            }
            ToolId::Freehand => {
                let Some(layer_id) = ctx.target_creation_layer_id() else {
                    return ToolOutput::noop();
                };
                let mut path = BezierPath::from_polygon(points, false);
                path.reduce_nodes(0.15);
                let obj = SceneObject::new(ObjectId(0), "Freehand Curve", NodePayload::Path(path))
                    .with_fill(Fill::None)
                    .with_outline(ctx.default_outline.clone());
                ToolOutput::Commit {
                    ops: vec![CommandOp::create_object(obj, Some(layer_id))],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                }
            }
            ToolId::SmartDrawing => {
                let Some(layer_id) = ctx.target_creation_layer_id() else {
                    return ToolOutput::noop();
                };
                let recognized = coree_core::SmartDrawing::recognize_rough_stroke(&points);
                let path = recognized.to_bezier_path();
                let obj = SceneObject::new(ObjectId(0), "Smart Shape", NodePayload::Path(path))
                    .with_fill(ctx.default_fill.clone())
                    .with_outline(ctx.default_outline.clone());
                ToolOutput::Commit {
                    ops: vec![CommandOp::create_object(obj, Some(layer_id))],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                }
            }
            ToolId::LiveSketch => {
                let Some(layer_id) = ctx.target_creation_layer_id() else {
                    return ToolOutput::noop();
                };
                self.live_sketch_strokes.push(points);
                let path = coree_core::LiveSketch::average_strokes(&self.live_sketch_strokes, 32);
                let existing_id = self
                    .live_sketch_object_id
                    .filter(|&id| {
                        ctx.document
                            .object(id)
                            .is_some_and(|o| matches!(o.payload, NodePayload::Path(_)))
                    })
                    .or_else(|| {
                        if self.live_sketch_strokes.len() <= 1 {
                            return None;
                        }
                        ctx.selection
                            .objects
                            .iter()
                            .rev()
                            .copied()
                            .find(|&id| {
                                ctx.document.object(id).is_some_and(|o| {
                                    o.name == "LiveSketch Curve"
                                        && matches!(o.payload, NodePayload::Path(_))
                                })
                            })
                            .or_else(|| {
                                ctx.document
                                    .find_layer(layer_id)
                                    .and_then(|l| l.objects.last().copied())
                                    .filter(|&id| {
                                        ctx.document.object(id).is_some_and(|o| {
                                            o.name == "LiveSketch Curve"
                                                && matches!(o.payload, NodePayload::Path(_))
                                        })
                                    })
                            })
                    });
                if let Some(oid) = existing_id {
                    self.live_sketch_object_id = Some(oid);
                    let inv = world_inverse_transform_of(ctx.document, oid);
                    return ToolOutput::Commit {
                        ops: vec![CommandOp::EditPath {
                            id: oid,
                            path: path.transformed(&inv),
                        }],
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    };
                }
                let mut obj =
                    SceneObject::new(ObjectId(0), "LiveSketch Curve", NodePayload::Path(path))
                        .with_fill(Fill::None)
                        .with_outline(ctx.default_outline.clone());
                if ctx.settings.symmetry_enabled || self.symmetry_enabled {
                    let folds = if self.symmetry_enabled {
                        self.symmetry_folds
                    } else {
                        ctx.settings.symmetry_folds
                    }
                    .max(2);
                    let mirror = if self.symmetry_enabled {
                        self.symmetry_mirror
                    } else {
                        ctx.settings.symmetry_mirror
                    };
                    let origin = self
                        .symmetry_origin_mm
                        .or(ctx.settings.symmetry_center_mm)
                        .unwrap_or_else(|| {
                            ctx.document.active_page().map_or(Point2D::ZERO, |p| {
                                Point2D::new(p.width_mm * 0.5, p.height_mm * 0.5)
                            })
                        });
                    obj.effects.push(Effect::Symmetry {
                        axes: folds,
                        mirror,
                        origin,
                    });
                }
                ToolOutput::Commit {
                    ops: vec![CommandOp::create_object(obj, Some(layer_id))],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                }
            }
            ToolId::ArtisticMedia => {
                let Some(layer_id) = ctx.target_creation_layer_id() else {
                    return ToolOutput::noop();
                };
                let centerline = BezierPath::from_polygon(points, false);
                let style = ArtisticMediaStyle::Preset {
                    max_width_mm: ctx.settings.artistic_media_width_mm,
                    profile: ctx.settings.artistic_media_profile.clone(),
                };
                let mut obj = SceneObject::new(
                    ObjectId(0),
                    "Artistic Media",
                    NodePayload::ArtisticMedia {
                        spine: centerline,
                        style,
                    },
                )
                .with_fill(ctx.default_fill.clone())
                .with_outline(OutlineStyle::none());
                if ctx.settings.symmetry_enabled || self.symmetry_enabled {
                    let folds = if self.symmetry_enabled {
                        self.symmetry_folds
                    } else {
                        ctx.settings.symmetry_folds
                    }
                    .max(2);
                    let mirror = if self.symmetry_enabled {
                        self.symmetry_mirror
                    } else {
                        ctx.settings.symmetry_mirror
                    };
                    let origin = self
                        .symmetry_origin_mm
                        .or(ctx.settings.symmetry_center_mm)
                        .unwrap_or_else(|| {
                            ctx.document.active_page().map_or(Point2D::ZERO, |p| {
                                Point2D::new(p.width_mm * 0.5, p.height_mm * 0.5)
                            })
                        });
                    obj.effects.push(Effect::Symmetry {
                        axes: folds,
                        mirror,
                        origin,
                    });
                }
                ToolOutput::Commit {
                    ops: vec![CommandOp::create_object(obj, Some(layer_id))],
                    clear_overlay: true,
                    post_actions: Vec::new(),
                }
            }
            ToolId::Knife => {
                if let Some((op, new_ids)) = build_knife_cut_op(
                    ctx.document,
                    &ctx.selection.objects,
                    ctx.active_layer_id,
                    points[0],
                    *points.last().expect("len >= 2"),
                ) {
                    let count = new_ids.len();
                    return ToolOutput::Commit {
                        ops: vec![op],
                        clear_overlay: true,
                        post_actions: vec![
                            ToolActionRequest::SelectObjects {
                                ids: new_ids,
                                additive: false,
                                toggle: false,
                            },
                            ToolActionRequest::Toast {
                                message: format!("Knife sliced shape into {count} piece(s)"),
                                is_error: false,
                            },
                        ],
                    };
                }
                ToolOutput::noop()
            }
            ToolId::VirtualSegmentDelete => {
                let ops = build_virtual_segment_delete_stroke_ops(ctx, &points);
                if ops.is_empty() {
                    ToolOutput::noop()
                } else {
                    ToolOutput::Commit {
                        ops,
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    }
                }
            }
            ToolId::Eraser => {
                let ops = build_vector_eraser_ops(ctx, &points);
                if ops.is_empty() {
                    ToolOutput::noop()
                } else {
                    ToolOutput::Commit {
                        ops,
                        clear_overlay: true,
                        post_actions: Vec::new(),
                    }
                }
            }
            ToolId::BitmapBrush | ToolId::BitmapEraser | ToolId::CloneStamp | ToolId::Smudge => {
                let _ = shift;
                ToolOutput::actions(vec![ToolActionRequest::ApplyRasterStroke {
                    tool,
                    points_mm: points,
                }])
            }
            _ => ToolOutput::noop(),
        }
    }

    /// Handles keyboard input (`Escape`, `Enter`, `Space` stamp duplicate, `Delete`/`Backspace` draft pop).
    pub fn on_key(&mut self, ctx: &ToolReadContext<'_>, ev: &KeyEvent) -> ToolOutput {
        match ev.key {
            ToolKey::Escape => self.cancel_all(),
            ToolKey::Enter => self.confirm(ctx),
            ToolKey::Delete | ToolKey::Backspace => {
                if self.pop_last_draft_point() {
                    let overlay = self.finalize_overlay(self.build_overlay(ctx));
                    ToolOutput::Continue { overlay }
                } else {
                    ToolOutput::noop()
                }
            }
            ToolKey::Space => {
                if let Some(live_world_affine) = self.active_live_world_transform() {
                    let overlay = self.finalize_overlay(self.build_overlay(ctx));
                    ToolOutput::Consumed {
                        overlay: Some(overlay),
                        actions: vec![ToolActionRequest::StampDuplicateDuringDrag {
                            live_world_affine,
                        }],
                    }
                } else {
                    ToolOutput::noop()
                }
            }
        }
    }

    /// Builds the UI-agnostic [`TransientOverlay`] representing all live previews,
    /// handles, rubber-band lines, and dynamic snap guides for the current state.
    #[must_use]
    pub fn build_overlay(&self, ctx: &ToolReadContext<'_>) -> TransientOverlay {
        let mut preview_paths = Vec::new();
        let mut preview_handles = Vec::new();
        let active_guides = self.active_guides.clone();

        let cursor_hint = match self.active_tool {
            ToolId::Pan => {
                if self.is_dragging() {
                    CursorHint::Grabbing
                } else {
                    CursorHint::Grab
                }
            }
            ToolId::Zoom => CursorHint::ZoomIn,
            ToolId::ArtisticText | ToolId::ParagraphText => CursorHint::Text,
            ToolId::ColorEyedropper | ToolId::AttributesEyedropper => CursorHint::Eyedropper,
            ToolId::Knife => CursorHint::Knife,
            ToolId::Eraser | ToolId::BitmapEraser => CursorHint::Eraser,
            ToolId::ShapeNode => CursorHint::NodeEdit,
            ToolId::Pick | ToolId::FreehandPick | ToolId::FreeTransform => {
                if self.is_transform_dragging() {
                    CursorHint::Move
                } else {
                    CursorHint::Default
                }
            }
            _ => CursorHint::Crosshair,
        };

        // 1. Active drag state previews
        match &self.drag_state {
            ActiveDragState::MovingSelection {
                object_ids,
                accum_dx,
                accum_dy,
                ..
            } => {
                let world_delta = Affine2D::translate(*accum_dx, *accum_dy);
                let ids = if object_ids.is_empty() {
                    &ctx.selection.objects
                } else {
                    object_ids
                };
                for &oid in ids {
                    if let Some(wp) = object_preview_world_path(ctx.document, oid, &world_delta) {
                        preview_paths.push(OverlayPath::stroke(wp, OverlayStrokeKind::Accent));
                    }
                }
            }
            ActiveDragState::ScalingSelection {
                orig_transforms,
                last_affine,
                ..
            }
            | ActiveDragState::RotatingSelection {
                orig_transforms,
                last_affine,
                ..
            }
            | ActiveDragState::SkewingSelection {
                orig_transforms,
                last_affine,
                ..
            }
            | ActiveDragState::FreeTransforming {
                orig_transforms,
                last_affine,
                ..
            } => {
                for &(oid, _) in orig_transforms {
                    if let Some(wp) = object_preview_world_path(ctx.document, oid, last_affine) {
                        preview_paths.push(OverlayPath::stroke(wp, OverlayStrokeKind::Accent));
                    }
                }
            }
            ActiveDragState::DraggingNode { draft_paths, .. } => {
                for &(oid, ref p) in draft_paths {
                    let xf = world_transform_of_or_local(ctx.document, oid);
                    preview_paths.push(OverlayPath::stroke(
                        p.transformed(&xf),
                        OverlayStrokeKind::Accent,
                    ));
                    for sp in &p.subpaths {
                        for node in &sp.nodes {
                            preview_handles.push(OverlayHandle::new(
                                xf.transform_point(node.pos),
                                HandleShape::Square,
                            ));
                            for h in [node.handle_in, node.handle_out].into_iter().flatten() {
                                preview_handles.push(OverlayHandle::new(
                                    xf.transform_point(h),
                                    HandleShape::Circle,
                                ));
                            }
                        }
                    }
                }
            }
            ActiveDragState::Sculpting {
                object_id,
                draft_path,
                ..
            }
            | ActiveDragState::DraggingCurveSegment {
                object_id,
                draft_path,
                ..
            } => {
                let xf = world_transform_of_or_local(ctx.document, *object_id);
                preview_paths.push(OverlayPath::stroke(
                    draft_path.transformed(&xf),
                    OverlayStrokeKind::Accent,
                ));
            }
            ActiveDragState::DraggingParametricHandle {
                object_id,
                draft_shape,
                ..
            } => {
                let xf = world_transform_of_or_local(ctx.document, *object_id);
                preview_paths.push(OverlayPath::stroke(
                    draft_shape.to_bezier_path().transformed(&xf),
                    OverlayStrokeKind::Accent,
                ));
                for (_, wpt) in parametric_shape_handles(draft_shape, &xf) {
                    preview_handles.push(OverlayHandle::new(wpt, HandleShape::Diamond));
                }
            }
            ActiveDragState::DraggingBSplineControlPoint {
                object_id,
                cp_index,
                draft_bspline,
                ..
            } => {
                let xf = world_transform_of_or_local(ctx.document, *object_id);
                preview_paths.push(OverlayPath::stroke(
                    draft_bspline.to_bezier_path().transformed(&xf),
                    OverlayStrokeKind::Accent,
                ));
                let wpts: Vec<Point2D> = draft_bspline
                    .control_points
                    .iter()
                    .map(|&p| xf.transform_point(p))
                    .collect();
                if wpts.len() >= 2 {
                    preview_paths.push(OverlayPath::stroke(
                        BezierPath::from_polygon(wpts.clone(), draft_bspline.closed),
                        OverlayStrokeKind::Secondary,
                    ));
                }
                for (idx, wpt) in wpts.into_iter().enumerate() {
                    preview_handles.push(OverlayHandle {
                        pos_mm: wpt,
                        shape: HandleShape::Diamond,
                        selected: idx == *cp_index,
                        color_hint: None,
                    });
                }
            }
            ActiveDragState::DraggingArtisticMediaNode {
                object_id,
                subpath_index,
                node_index,
                handle_part,
                draft_spine,
                style,
                ..
            } => {
                let xf = world_transform_of_or_local(ctx.document, *object_id);
                let expanded = ArtisticMedia::expand_stroke(draft_spine, style);
                if !expanded.is_empty() {
                    preview_paths.push(OverlayPath::stroke(
                        expanded.transformed(&xf),
                        OverlayStrokeKind::Accent,
                    ));
                }
                preview_paths.push(OverlayPath::stroke(
                    draft_spine.transformed(&xf),
                    OverlayStrokeKind::Secondary,
                ));
                for (sp_idx, sp) in draft_spine.subpaths.iter().enumerate() {
                    for (n_idx, node) in sp.nodes.iter().enumerate() {
                        let is_node = sp_idx == *subpath_index && n_idx == *node_index;
                        preview_handles.push(OverlayHandle {
                            pos_mm: xf.transform_point(node.pos),
                            shape: HandleShape::Square,
                            selected: is_node && *handle_part == NodeDragPart::Anchor,
                            color_hint: None,
                        });
                        if let Some(h_in) = node.handle_in {
                            preview_handles.push(OverlayHandle {
                                pos_mm: xf.transform_point(h_in),
                                shape: HandleShape::Circle,
                                selected: is_node && *handle_part == NodeDragPart::HandleIn,
                                color_hint: None,
                            });
                        }
                        if let Some(h_out) = node.handle_out {
                            preview_handles.push(OverlayHandle {
                                pos_mm: xf.transform_point(h_out),
                                shape: HandleShape::Circle,
                                selected: is_node && *handle_part == NodeDragPart::HandleOut,
                                color_hint: None,
                            });
                        }
                    }
                }
            }
            ActiveDragState::DraggingGradientHandle {
                object_id,
                draft_fill,
                ..
            }
            | ActiveDragState::DraggingGradientFocal {
                object_id,
                draft_fill,
                ..
            }
            | ActiveDragState::DraggingGradientStop {
                object_id,
                draft_fill,
                ..
            } => {
                let xf = world_transform_of_or_local(ctx.document, *object_id);
                if let Some((start_l, end_l, stops)) = gradient_ray_local(draft_fill) {
                    let a = xf.transform_point(start_l);
                    let b = xf.transform_point(end_l);
                    preview_paths.push(OverlayPath::stroke(
                        BezierPath::from_line(a, b),
                        OverlayStrokeKind::Accent,
                    ));
                    preview_handles.push(OverlayHandle::new(a, HandleShape::Square));
                    preview_handles.push(OverlayHandle::new(b, HandleShape::Circle));
                    for (idx, st) in stops.iter().enumerate() {
                        if idx == 0 || idx + 1 == stops.len() {
                            continue;
                        }
                        let t = f64::from(st.offset);
                        let wpt = xf.transform_point(start_l.lerp(end_l, t));
                        preview_handles.push(OverlayHandle {
                            pos_mm: wpt,
                            shape: HandleShape::Square,
                            selected: false,
                            color_hint: Some(st.color.clone()),
                        });
                    }
                }
            }
            ActiveDragState::DraggingMeshNode {
                object_id,
                node_index,
                draft_fill,
                ..
            }
            | ActiveDragState::DraggingMeshHandle {
                object_id,
                node_index,
                draft_fill,
                ..
            } => {
                let active_handle_slot = match &self.drag_state {
                    ActiveDragState::DraggingMeshHandle { handle_slot, .. } => Some(*handle_slot),
                    _ => None,
                };
                let xf = world_transform_of_or_local(ctx.document, *object_id);
                if let Fill::MeshFill { rows, cols, nodes } = draft_fill {
                    let wpts: Vec<Point2D> =
                        nodes.iter().map(|n| xf.transform_point(n.pos)).collect();
                    for r in 0..*rows {
                        for c in 0..*cols {
                            let idx = r * cols + c;
                            if c + 1 < *cols && idx + 1 < wpts.len() {
                                preview_paths.push(OverlayPath::stroke(
                                    BezierPath::from_line(wpts[idx], wpts[idx + 1]),
                                    OverlayStrokeKind::Accent,
                                ));
                            }
                            if r + 1 < *rows && idx + cols < wpts.len() {
                                preview_paths.push(OverlayPath::stroke(
                                    BezierPath::from_line(wpts[idx], wpts[idx + cols]),
                                    OverlayStrokeKind::Accent,
                                ));
                            }
                        }
                    }
                    if let (Some(sel_mn), Some(&node_wpt)) =
                        (nodes.get(*node_index), wpts.get(*node_index))
                    {
                        for (slot, opt_h) in [
                            (0u8, sel_mn.handle_n),
                            (1u8, sel_mn.handle_s),
                            (2u8, sel_mn.handle_e),
                            (3u8, sel_mn.handle_w),
                        ] {
                            if let Some(rel) = opt_h {
                                let h_wpt = xf.transform_point(Point2D::new(
                                    sel_mn.pos.x + rel.x,
                                    sel_mn.pos.y + rel.y,
                                ));
                                preview_paths.push(OverlayPath::stroke(
                                    BezierPath::from_line(node_wpt, h_wpt),
                                    OverlayStrokeKind::Secondary,
                                ));
                                preview_handles.push(OverlayHandle {
                                    pos_mm: h_wpt,
                                    shape: HandleShape::Diamond,
                                    selected: active_handle_slot == Some(slot),
                                    color_hint: None,
                                });
                            }
                        }
                    }
                    for (idx, (mn, &wpt)) in nodes.iter().zip(wpts.iter()).enumerate() {
                        preview_handles.push(OverlayHandle {
                            pos_mm: wpt,
                            shape: HandleShape::Circle,
                            selected: idx == *node_index,
                            color_hint: Some(mn.color.clone()),
                        });
                    }
                }
            }
            ActiveDragState::DraggingTransparencyHandle {
                object_id,
                draft_effect,
                ..
            } => {
                let xf = world_transform_of_or_local(ctx.document, *object_id);
                if let Effect::Transparency {
                    mode: TransparencyMode::FountainLinear { start, end, .. },
                    ..
                } = draft_effect
                {
                    let a = xf.transform_point(*start);
                    let b = xf.transform_point(*end);
                    preview_paths.push(OverlayPath::stroke(
                        BezierPath::from_line(a, b),
                        OverlayStrokeKind::Secondary,
                    ));
                    preview_handles.push(OverlayHandle::new(a, HandleShape::Square));
                    preview_handles.push(OverlayHandle::new(b, HandleShape::Square));
                }
            }
            ActiveDragState::DraggingEnvelopeHandle {
                object_id,
                handle_index,
                draft_effect,
                ..
            } => {
                let xf = world_transform_of_or_local(ctx.document, *object_id);
                if let Effect::Envelope { cage, .. } = draft_effect {
                    if cage.len() >= 4 {
                        let wpts: Vec<Point2D> =
                            cage.iter().map(|&p| xf.transform_point(p)).collect();
                        preview_paths.push(OverlayPath::stroke(
                            BezierPath::from_polygon(wpts.clone(), true),
                            OverlayStrokeKind::Danger,
                        ));
                        for (i, wpt) in wpts.into_iter().enumerate() {
                            preview_handles.push(OverlayHandle {
                                pos_mm: wpt,
                                shape: HandleShape::Square,
                                selected: i == *handle_index,
                                color_hint: None,
                            });
                        }
                    }
                }
            }
            ActiveDragState::DraggingEffectHandle {
                object_id,
                draft_effect,
                ..
            } => {
                if let Some(obj) = ctx.document.object(*object_id) {
                    if let Some((a, b)) = interactive_effect_handle_points(
                        ctx.document,
                        obj,
                        draft_effect,
                        self.active_tool,
                    ) {
                        preview_paths.push(OverlayPath::stroke(
                            BezierPath::from_line(a, b),
                            OverlayStrokeKind::Secondary,
                        ));
                        preview_handles.push(OverlayHandle::new(a, HandleShape::Square));
                        preview_handles.push(OverlayHandle::new(b, HandleShape::Circle));
                    }
                }
            }
            ActiveDragState::DraggingCanvasGuide {
                draft_orientation, ..
            } => {
                let vis = ctx.viewport.visible_world_rect();
                let line = match *draft_orientation {
                    GuideOrientation::Horizontal(y) => BezierPath::from_line(
                        Point2D::new(vis.min_x, y),
                        Point2D::new(vis.max_x, y),
                    ),
                    GuideOrientation::Vertical(x) => BezierPath::from_line(
                        Point2D::new(x, vis.min_y),
                        Point2D::new(x, vis.max_y),
                    ),
                    GuideOrientation::Angled { origin, angle_deg } => {
                        let rad = angle_deg.to_radians();
                        let dir = Vec2D::new(rad.cos(), rad.sin()) * 2000.0;
                        BezierPath::from_line(origin - dir, origin + dir)
                    }
                };
                preview_paths.push(OverlayPath::stroke(line, OverlayStrokeKind::Danger));
            }
            ActiveDragState::MarqueeSelect {
                start_mm,
                current_mm,
            }
            | ActiveDragState::NodeMarqueeSelect {
                start_mm,
                current_mm,
            } => {
                let rect = Rect2D::from_points(*start_mm, *current_mm);
                preview_paths.push(OverlayPath::filled(
                    rect_to_bezier_path(rect),
                    OverlayStrokeKind::MarqueeFill,
                ));
            }
            ActiveDragState::ZoomMarquee {
                start_mm,
                current_mm,
            } => {
                let rect = Rect2D::from_points(*start_mm, *current_mm);
                preview_paths.push(OverlayPath::filled(
                    rect_to_bezier_path(rect),
                    OverlayStrokeKind::Secondary,
                ));
            }
            ActiveDragState::LassoSelect { points_mm } => {
                if points_mm.len() >= 2 {
                    preview_paths.push(OverlayPath::stroke(
                        BezierPath::from_polygon(points_mm.clone(), true),
                        OverlayStrokeKind::Secondary,
                    ));
                }
            }
            ActiveDragState::DraggingInteractiveEffect {
                start_mm,
                current_mm,
                ..
            } => {
                preview_paths.push(OverlayPath::stroke(
                    BezierPath::from_line(*start_mm, *current_mm),
                    OverlayStrokeKind::Secondary,
                ));
                preview_handles.push(OverlayHandle::new(*start_mm, HandleShape::Circle));
                preview_handles.push(OverlayHandle::new(*current_mm, HandleShape::Circle));
            }
            ActiveDragState::PullingBezierHandle {
                anchor_mm,
                handle_out_mm,
                ..
            } => {
                let hi = *anchor_mm - (*handle_out_mm - *anchor_mm);
                preview_paths.push(OverlayPath::stroke(
                    BezierPath::from_line(hi, *handle_out_mm),
                    OverlayStrokeKind::Secondary,
                ));
                preview_handles.push(OverlayHandle::new(*anchor_mm, HandleShape::Square));
                preview_handles.push(OverlayHandle::new(*handle_out_mm, HandleShape::Circle));
                preview_handles.push(OverlayHandle::new(hi, HandleShape::Circle));
            }
            ActiveDragState::DrawingBox {
                start_mm,
                current_mm,
                ctrl,
                shift,
            } => {
                if self.active_tool == ToolId::InteractiveFill {
                    preview_paths.push(OverlayPath::stroke(
                        BezierPath::from_line(*start_mm, *current_mm),
                        OverlayStrokeKind::Accent,
                    ));
                    preview_handles.push(OverlayHandle::new(*start_mm, HandleShape::Square));
                    preview_handles.push(OverlayHandle::new(*current_mm, HandleShape::Circle));
                } else if matches!(
                    self.active_tool,
                    ToolId::ThreePointCurve
                        | ToolId::ThreePointRectangle
                        | ToolId::ThreePointEllipse
                        | ToolId::StraightConnector
                        | ToolId::RightAngleConnector
                ) {
                    preview_paths.push(OverlayPath::stroke(
                        BezierPath::from_line(*start_mm, *current_mm),
                        OverlayStrokeKind::Accent,
                    ));
                    preview_handles.push(OverlayHandle::new(*start_mm, HandleShape::Circle));
                    preview_handles.push(OverlayHandle::new(*current_mm, HandleShape::Circle));
                } else {
                    let box_mm = compute_constrained_box(*start_mm, *current_mm, *ctrl, *shift);
                    if self.active_tool == ToolId::Table {
                        preview_paths.push(OverlayPath::stroke(
                            rect_to_bezier_path(box_mm),
                            OverlayStrokeKind::Accent,
                        ));
                        let rows = ctx.settings.table_rows.clamp(1, 30);
                        let cols = ctx.settings.table_cols.clamp(1, 30);
                        for r in 1..rows {
                            let y = box_mm.min_y + box_mm.height() * (r as f64 / rows as f64);
                            preview_paths.push(OverlayPath::stroke(
                                BezierPath::from_line(
                                    Point2D::new(box_mm.min_x, y),
                                    Point2D::new(box_mm.max_x, y),
                                ),
                                OverlayStrokeKind::Secondary,
                            ));
                        }
                        for c in 1..cols {
                            let x = box_mm.min_x + box_mm.width() * (c as f64 / cols as f64);
                            preview_paths.push(OverlayPath::stroke(
                                BezierPath::from_line(
                                    Point2D::new(x, box_mm.min_y),
                                    Point2D::new(x, box_mm.max_y),
                                ),
                                OverlayStrokeKind::Secondary,
                            ));
                        }
                    } else {
                        let p = if matches!(self.active_tool, ToolId::Ellipse | ToolId::LensTool) {
                            ellipse_rect_to_bezier_path(box_mm)
                        } else {
                            rect_to_bezier_path(box_mm)
                        };
                        let p = if ctx.settings.draw_in_perspective {
                            if let Some(hm) = ctx
                                .document
                                .perspective_grid()
                                .plane_homography_for_rect(box_mm)
                            {
                                hm.transform_bezier_path(&p)
                            } else {
                                p
                            }
                        } else {
                            p
                        };
                        preview_paths.push(OverlayPath::stroke(p, OverlayStrokeKind::Accent));
                    }
                }
            }
            ActiveDragState::DrawingStroke { points_mm } => {
                if points_mm.len() >= 2 {
                    let kind = if self.active_tool == ToolId::Knife {
                        OverlayStrokeKind::Danger
                    } else {
                        OverlayStrokeKind::Accent
                    };
                    preview_paths.push(OverlayPath::stroke(
                        BezierPath::from_polygon(points_mm.clone(), false),
                        kind,
                    ));
                }
            }
            ActiveDragState::DraggingRulerGuide {
                horizontal,
                position_mm,
            } => {
                let vis = ctx.viewport.visible_world_rect();
                let line = if *horizontal {
                    BezierPath::from_line(
                        Point2D::new(vis.min_x, *position_mm),
                        Point2D::new(vis.max_x, *position_mm),
                    )
                } else {
                    BezierPath::from_line(
                        Point2D::new(*position_mm, vis.min_y),
                        Point2D::new(*position_mm, vis.max_y),
                    )
                };
                preview_paths.push(OverlayPath::stroke(line, OverlayStrokeKind::Guide));
            }
            ActiveDragState::Idle
            | ActiveDragState::Panning { .. }
            | ActiveDragState::DraggingPivot
            | ActiveDragState::CropBoxHandle { .. } => {}
        }

        // 2. Bezier/Pen multi-click rubber-band preview
        if !self.bezier_draft_nodes.is_empty() {
            let mut nodes = self.bezier_draft_nodes.clone();
            nodes.push(PathNode::corner(self.cursor_world_mm));
            let preview_path = BezierPath::from_subpath(Subpath {
                nodes,
                closed: false,
            });
            preview_paths.push(OverlayPath::stroke(preview_path, OverlayStrokeKind::Accent));
            for (i, n) in self.bezier_draft_nodes.iter().enumerate() {
                preview_handles.push(OverlayHandle {
                    pos_mm: n.pos,
                    shape: HandleShape::Square,
                    selected: i == 0,
                    color_hint: None,
                });
            }
        }

        // 3. B-Spline / Polyline multi-click rubber-band preview
        if matches!(self.active_tool, ToolId::BSpline | ToolId::Polyline)
            && !self.poly_points_mm.is_empty()
        {
            let mut pts = self.poly_points_mm.clone();
            pts.push(self.cursor_world_mm);
            let path = if self.active_tool == ToolId::BSpline {
                bspline_to_bezier_path(&pts, false)
            } else {
                BezierPath::from_polygon(pts, false)
            };
            preview_paths.push(OverlayPath::stroke(path, OverlayStrokeKind::Accent));
        }

        // 4. 3-Point Curve & 3-Point Rect/Ellipse stage 2 previews
        if let Some((p1, p2)) = self.three_point_curve_chord {
            let arc = three_point_arc_path(p1, p2, self.cursor_world_mm);
            preview_paths.push(OverlayPath::stroke(arc, OverlayStrokeKind::Secondary));
        }
        if let Some((_, p1, p2)) = self.three_point_shape_chord {
            let base = p2 - p1;
            let len = base.length().max(1e-3);
            let u = base / len;
            let perp = Vec2D::new(-u.y, u.x);
            let h = (self.cursor_world_mm - p1).dot(perp);
            let corners = vec![p1, p2, p2 + perp * h, p1 + perp * h];
            preview_paths.push(OverlayPath::stroke(
                BezierPath::from_polygon(corners, true),
                OverlayStrokeKind::Accent,
            ));
        }

        // 5. Connector snap target ring indicator
        if matches!(
            self.active_tool,
            ToolId::StraightConnector | ToolId::RightAngleConnector
        ) {
            if let Some((_, _, snap_pt)) = find_nearest_object_anchor_in_doc(
                ctx.document,
                ctx.document.active_page_index(),
                ctx.powerclip_edit_id,
                ctx.treat_all_objects_as_filled,
                self.cursor_world_mm,
                8.0,
            ) {
                preview_handles.push(OverlayHandle::new(snap_pt, HandleShape::Circle));
            }
        }

        // 6. Crop box handles (when pending_crop_rect is active)
        if let Some(crop) = self.pending_crop_rect {
            for (_, pos) in selection_handle_world_positions(crop) {
                preview_handles.push(OverlayHandle::new(pos, HandleShape::Square));
            }
        }

        // 7. Symmetry axis guides (when symmetry mode is active)
        if ctx.settings.symmetry_enabled || self.symmetry_enabled {
            let folds = if self.symmetry_enabled {
                self.symmetry_folds
            } else {
                ctx.settings.symmetry_folds
            }
            .max(2);
            let origin = self
                .symmetry_origin_mm
                .or(ctx.settings.symmetry_center_mm)
                .unwrap_or_else(|| {
                    ctx.document.active_page().map_or(Point2D::ZERO, |p| {
                        Point2D::new(p.width_mm * 0.5, p.height_mm * 0.5)
                    })
                });
            let vis = ctx.viewport.visible_world_rect();
            let radius = vis.width().hypot(vis.height()).max(500.0);
            for k in 0..folds {
                let ang = std::f64::consts::TAU * (k as f64) / (folds as f64);
                let dir = Vec2D::new(ang.cos(), ang.sin()) * radius;
                preview_paths.push(OverlayPath::stroke(
                    BezierPath::from_line(origin, origin + dir),
                    OverlayStrokeKind::Guide,
                ));
            }
            preview_handles.push(OverlayHandle::new(origin, HandleShape::Crosshair));
        }

        // 8. Selected BSpline control polygon + handles in ShapeNode idle mode
        if self.active_tool == ToolId::ShapeNode && matches!(self.drag_state, ActiveDragState::Idle)
        {
            for &oid in &ctx.selection.objects {
                if let Some(obj) = ctx.document.object(oid) {
                    if let NodePayload::BSpline(ref bs) = obj.payload {
                        let xf = world_transform_of_or_local(ctx.document, oid);
                        let wpts: Vec<Point2D> = bs
                            .control_points
                            .iter()
                            .map(|&p| xf.transform_point(p))
                            .collect();
                        if wpts.len() >= 2 {
                            preview_paths.push(OverlayPath::stroke(
                                BezierPath::from_polygon(wpts.clone(), bs.closed),
                                OverlayStrokeKind::Secondary,
                            ));
                        }
                        for wpt in wpts {
                            preview_handles.push(OverlayHandle::new(wpt, HandleShape::Diamond));
                        }
                    } else if let NodePayload::ArtisticMedia { ref spine, .. } = obj.payload {
                        let xf = world_transform_of_or_local(ctx.document, oid);
                        preview_paths.push(OverlayPath::stroke(
                            spine.transformed(&xf),
                            OverlayStrokeKind::Secondary,
                        ));
                        for sp in &spine.subpaths {
                            for node in &sp.nodes {
                                preview_handles.push(OverlayHandle::new(
                                    xf.transform_point(node.pos),
                                    HandleShape::Square,
                                ));
                                for h in [node.handle_in, node.handle_out].into_iter().flatten() {
                                    preview_handles.push(OverlayHandle::new(
                                        xf.transform_point(h),
                                        HandleShape::Circle,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        TransientOverlay {
            preview_paths,
            preview_handles,
            active_guides,
            dirty_world_bbox_mm: None,
            cursor_hint,
        }
    }
}

impl ToolHandler for ToolCore {
    fn on_pointer_down(&mut self, ctx: &ToolReadContext<'_>, ev: &PointerEvent) -> ToolOutput {
        Self::on_pointer_down(self, ctx, ev)
    }

    fn on_pointer_move(&mut self, ctx: &ToolReadContext<'_>, ev: &PointerEvent) -> ToolOutput {
        Self::on_pointer_move(self, ctx, ev)
    }

    fn on_pointer_up(&mut self, ctx: &ToolReadContext<'_>, ev: &PointerEvent) -> ToolOutput {
        Self::on_pointer_up(self, ctx, ev)
    }

    fn on_key(&mut self, ctx: &ToolReadContext<'_>, ev: &KeyEvent) -> ToolOutput {
        Self::on_key(self, ctx, ev)
    }

    fn current_overlay(&self, ctx: &ToolReadContext<'_>) -> TransientOverlay {
        let mut ov = self.build_overlay(ctx);
        let _ = ov.finalize_dirty_bbox(self.prev_dirty_bbox_mm);
        ov
    }
}

fn capture_transforms_for_ids(
    doc: &coree_core::Document,
    selection: &[ObjectId],
) -> Vec<(ObjectId, Affine2D)> {
    editable_topmost_selection(doc, selection)
        .into_iter()
        .filter_map(|id| {
            let obj = doc.object(id)?;
            Some((id, obj.transform))
        })
        .collect()
}

fn hit_test_canvas_point_opt_tol(
    ctx: &ToolReadContext<'_>,
    world_pt: Point2D,
    tol_mm: f64,
) -> Option<ObjectId> {
    crate::math_helpers::hit_test_canvas_point_in_doc(
        ctx.document,
        ctx.powerclip_edit_id,
        ctx.treat_all_objects_as_filled,
        world_pt,
        tol_mm,
    )
}
