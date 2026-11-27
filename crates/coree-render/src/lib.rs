// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! High-precision Pure-Rust 2D rendering engine with backend-agnostic [`CanvasBackend`],
//! spatial R-Tree dirty-tile rasterization, full Arabic RTL/BiDi/Kashida typography,
//! all 11 live optical lenses, non-destructive vector effects, raster studio
//! painting/filtering, outline & centerline bitmap tracing, Smart Fill, snapping,
//! hit-testing, and headless high-DPI export.

pub mod backend;
pub mod effects_render;
pub mod error;
pub mod export_render;
pub use coree_core::hit_test;
pub mod lens_render;
pub mod raster_engine;
pub mod renderer;
pub mod smart_fill;
pub mod text_layout;
pub mod tile_cache;
pub mod trace;
pub use coree_core::viewport;

/// Crate identifier used during workspace initialization and diagnostics.
pub const CRATE_NAME: &str = "coree-render";

// Ergonomic top-level re-exports for downstream crates (`coree-app`, `coree-io`, `coree-prod`)
pub use backend::{
    BackendPaint, CanvasBackend, FillRule, PaintShader, PixelRect, RenderBackend,
    RenderBackendKind, RgbaBuffer, StrokeParams, TinySkiaBackend, WgpuBackend,
};
pub use effects_render::lower_effects_to_primitives;
pub use error::{RenderError, RenderResult};
pub use export_render::{
    render_isolated_object_to_rgba, render_isolated_object_to_rgba_with_options,
    render_page_to_rgba, render_page_to_rgba_with_backend, render_page_to_rgba_with_options,
    render_svg_to_rgba, ExportIsolationSession, RenderedImage,
};
pub use hit_test::{
    hit_test_node, hit_test_point, hit_test_powerclip_contents, hit_test_rect, DynamicGuideKind,
    DynamicGuideLine, NodeHit, SnapEngine, SnapOptions,
};
pub use lens_render::capture_frozen_lens_snapshot;
pub use raster_engine::{
    apply_cutout_background_removal, apply_dodge_burn_stroke, apply_dodge_burn_stroke_stylus,
    apply_dodge_burn_stroke_stylus_masked, apply_healing_stroke, apply_healing_stroke_stylus,
    apply_healing_stroke_stylus_masked, apply_raster_filter, apply_raster_filters,
    clone_stamp_stroke, clone_stamp_stroke_stylus, clone_stamp_stroke_stylus_masked,
    compute_histogram, convert_object_to_bitmap, convert_object_to_bitmap_with_bounds,
    convert_raster_color_mode, convert_raster_depth, cutout_lab, erase_stroke, erase_stroke_stylus,
    erase_stroke_stylus_masked, paint_brush_stroke, paint_brush_stroke_masked,
    paint_brush_stroke_stylus, raster_flood_fill, raster_flood_fill_masked, resample,
    resample_with_algorithm, smudge_stroke, smudge_stroke_stylus, smudge_stroke_stylus_masked,
    DodgeBurnMode, RasterHistogram, RasterMask, ResampleAlgorithm, StylusPoint, ToneRange,
};
pub use renderer::{
    arrowhead_geometry, compute_page_layouts, outline_arrowhead_geometries, PageLayout,
    RenderOptions, SceneRenderer,
};
pub use smart_fill::{compute_smart_fill, create_smart_fill_object, dcel_smart_fill, PlanarDcel};
pub use text_layout::{
    apply_kashida_elongation, contains_arabic, list_glyphs_for_block, paragraph_to_curves,
    text_on_path_to_curves, text_to_curves, GlyphBlockFilter, GlyphCatalogEntry, ShapedGlyphDraw,
    ShapedPdfGlyph, ShapedPdfRun, TextEngine,
};
pub use tile_cache::{
    object_visual_world_bounds, CachedTile, DirtyTileCache, GpuTileView, WorldTileKey,
    TILE_SIZE_PX,
};
pub use trace::{trace_raster, trace_raster_to_mm, OutlinePreset, TraceConfig, TraceMode};
pub use viewport::{ViewMode, Viewport};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_metadata_is_valid() {
        assert_eq!(CRATE_NAME, "coree-render");
    }
}
