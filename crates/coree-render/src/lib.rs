// SPDX-License-Identifier: MPL-2.0
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
pub mod hit_test;
pub mod lens_render;
pub mod raster_engine;
pub mod renderer;
pub mod smart_fill;
pub mod text_layout;
pub mod tile_cache;
pub mod trace;
pub mod viewport;

/// Crate identifier used during workspace initialization and diagnostics.
pub const CRATE_NAME: &str = "coree-render";

// Ergonomic top-level re-exports for downstream crates (`coree-app`, `coree-io`, `coree-prod`)
pub use backend::{
    BackendPaint, CanvasBackend, FillRule, PaintShader, PixelRect, RgbaBuffer, StrokeParams,
    TinySkiaBackend,
};
pub use error::{RenderError, RenderResult};
pub use export_render::{
    render_page_to_rgba, render_page_to_rgba_with_options, render_svg_to_rgba, RenderedImage,
};
pub use hit_test::{
    hit_test_node, hit_test_point, hit_test_rect, DynamicGuideKind, DynamicGuideLine, NodeHit,
    SnapEngine, SnapOptions,
};
pub use raster_engine::{
    apply_cutout_background_removal, apply_raster_filter, apply_raster_filters, clone_stamp_stroke,
    convert_object_to_bitmap, erase_stroke, paint_brush_stroke, paint_brush_stroke_masked,
    resample, smudge_stroke, RasterMask,
};
pub use renderer::{compute_page_layouts, PageLayout, RenderOptions, SceneRenderer};
pub use smart_fill::{compute_smart_fill, create_smart_fill_object};
pub use text_layout::{
    apply_kashida_elongation, contains_arabic, paragraph_to_curves, text_on_path_to_curves,
    text_to_curves, TextEngine,
};
pub use tile_cache::{CachedTile, DirtyTileCache, TILE_SIZE_PX};
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
