// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Headless, `egui`-independent interactive tool state machines (`ToolCore`),
//! read-only scene context (`ToolReadContext`), and transient overlay previews
//! (`TransientOverlay`) realizing **Layer 3** of the 6-Layer Atomic Kernel.

pub mod builders;
pub mod context;
pub mod math_helpers;
pub mod output;
pub mod overlay;
pub mod state;
pub mod types;

/// Crate identifier used during workspace initialization and diagnostics.
pub const CRATE_NAME: &str = "coree-tools";

pub use builders::{
    build_bspline_create_op, build_connector_op, build_drawn_box_output,
    build_interactive_effect_output, build_interactive_fill_op, build_knife_cut_op,
    build_mesh_fill_output, build_sculpt_stroke_output, build_three_point_curve_op,
    build_three_point_dimension_op, build_three_point_shape_op, build_vector_eraser_op,
    build_vector_eraser_ops, build_virtual_segment_delete_op,
    build_virtual_segment_delete_stroke_ops, collect_editable_fillable_targets, compute_lasso_hits,
    compute_lasso_hits_scoped, is_fillable_leaf_payload,
};
pub use context::{
    KeyEvent, ModifiersState, PointerButton, PointerEvent, ToolKey, ToolReadContext,
};
pub use math_helpers::{
    bspline_to_bezier_path, build_mesh_fill_for_bounds, build_world_transform_command,
    compute_constrained_box, compute_free_transform_affine, compute_scale_affine,
    conjugate_world_affine_for_object, constrain_line_15deg, editable_topmost_selection,
    ellipse_rect_to_bezier_path, enclosing_layer_of_object, find_nearest_object_anchor,
    find_nearest_object_anchor_in_doc, gradient_ray_local, gradient_stops_mut,
    hit_curve_segment_on_selection_world, hit_editable_bezier_node_or_handle_world,
    hit_parametric_shape_handle_world, hit_scale_handle_world, hit_test_canvas_guide,
    hit_test_canvas_point_in_doc, hit_test_powerclip_edit_point, interactive_effect_handle_points,
    interpolate_gradient_stop_color, is_object_editable_in_doc, object_preview_world_path,
    parametric_shape_for_tool_rect, parametric_shape_handles, point_in_polygon,
    rect_to_bezier_path, selection_handle_world_positions, snap_point_with_reference,
    three_point_arc_path, try_insert_gradient_stop_on_ray_world, unconjugate_local_affine_to_world,
    update_parametric_shape_live, world_inverse_transform_of, world_transform_of_or_local,
};
pub use output::{ToolActionRequest, ToolOutput};
pub use overlay::{
    CursorHint, HandleShape, OverlayHandle, OverlayPath, OverlayStrokeKind, TransientOverlay,
};
pub use state::{ActiveDragState, ToolCore, ToolHandler};
pub use types::{
    NodeDragPart, NodeSmoothness, ParametricHandleKind, RasterBrushOp, ScaleHandle, SculptSubMode,
    ToolId, ToolSettingsSnapshot, TOOL_GROUPS,
};
