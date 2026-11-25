// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Offline quantitative benchmark (`IMP-12`):
//! 1. Cold studio core initialization (`Document`, `ToolCore`, `DirtyTileCache`, `StorageDb`, initial render).
//! 2. Interactive `ToolCore` drag loop with targeted world-space `DirtyTileCache` invalidation (`>= 60 FPS`).

use coree_bench_common::{parse_bench_iters, summarize_durations};
use coree_core::{
    BezierPath, Color, CommandHistory, Document, Fill, LensConfig, LensKind, NodePayload, ObjectId,
    OutlineStyle, Point2D, Rect2D, SceneObject, SelectionState, SnapEngine, SnapOptions, TextStyle,
    Viewport,
};
use coree_render::{DirtyTileCache, RenderOptions, SceneRenderer, ViewMode};
use coree_storage::StorageDb;
use coree_tools::{
    PointerEvent, ToolCore, ToolId, ToolOutput, ToolReadContext, ToolSettingsSnapshot,
};
use std::time::Instant;

fn to_screen_px(vp: &Viewport, world_mm: Point2D) -> [f32; 2] {
    let sp = vp.world_to_screen(world_mm);
    [sp.x as f32, sp.y as f32]
}

fn main() {
    let drag_iters = parse_bench_iters(60);

    // 1. Cold start init benchmark
    let t0 = Instant::now();
    let mut doc = Document::new_a4("BenchColdStart");
    let layer_id = doc.page(0).unwrap().layers[0].id;
    let mut tool_core = ToolCore::new(ToolId::Pick);
    let mut tile_cache = DirtyTileCache::new(1280, 720);
    let mut renderer = SceneRenderer::new();
    let db = StorageDb::open_in_memory().expect("in-memory StorageDb");
    assert_eq!(db.schema_version().unwrap(), coree_storage::SCHEMA_VERSION);
    let mut viewport = Viewport::new(1280, 720);
    viewport.fit_rect(Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0), 20.0);
    let render_opts = RenderOptions::interactive();
    tile_cache.mark_all_dirty();
    let _ = tile_cache.render_dirty_tiles(
        &doc,
        &viewport,
        ViewMode::Enhanced,
        &render_opts,
        &mut renderer,
    );
    let cold_ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!("cold_start_init: mean = {cold_ms:.3} ms");

    // 2. Populate 250 vector objects and warm tile cache before drag loop
    let mut first_id = None;
    for i in 0..250 {
        let x = 15.0 + (i % 15) as f64 * 12.0;
        let y = 15.0 + (i / 15) as f64 * 14.0;
        let oid = doc
            .add_object(
                SceneObject::new(
                    ObjectId(0),
                    format!("TileObj{i}"),
                    NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(x, y, 8.0, 8.0))),
                )
                .with_fill(Fill::Solid(Color::rgba8(
                    ((i * 17) % 255) as u8,
                    ((i * 43) % 255) as u8,
                    200,
                    255,
                ))),
                Some(layer_id),
            )
            .unwrap();
        if first_id.is_none() {
            first_id = Some(oid);
        }
    }
    let drag_id = first_id.unwrap();
    tile_cache.mark_all_dirty();
    let _ = tile_cache.render_dirty_tiles(
        &doc,
        &viewport,
        ViewMode::Enhanced,
        &render_opts,
        &mut renderer,
    );

    let mut selection = SelectionState::new();
    selection.select_single(&doc, drag_id);
    let snap_engine = SnapEngine;
    let snap_options = SnapOptions::default();
    let default_fill = Fill::Solid(Color::CYAN);
    let default_outline = OutlineStyle::default();
    let default_text_style = TextStyle::default();
    let default_lens = LensConfig::new(LensKind::Brighten { rate_pct: 20.0 });
    let settings = ToolSettingsSnapshot::default();

    let start_world = Point2D::new(19.0, 19.0);
    let down_ev = PointerEvent::primary(start_world, to_screen_px(&viewport, start_world));
    {
        let rctx = ToolReadContext {
            document: &doc,
            selection: &selection,
            snap_engine: &snap_engine,
            viewport: &viewport,
            snap_options: &snap_options,
            active_layer_id: Some(layer_id),
            creation_layer_id: Some(layer_id),
            powerclip_edit_id: None,
            treat_all_objects_as_filled: false,
            default_fill: &default_fill,
            default_outline: &default_outline,
            default_text_style: &default_text_style,
            default_lens: &default_lens,
            settings: &settings,
        };
        let _ = tool_core.on_pointer_down(&rctx, &down_ev);
    }

    let mut frame_ms = Vec::with_capacity(drag_iters);
    let mut prev_dirty = doc.world_object_bounds(drag_id);
    for step in 1..=drag_iters {
        let cur_world = Point2D::new(19.0 + step as f64 * 0.5, 19.0 + step as f64 * 0.3);
        let move_ev = PointerEvent::primary(cur_world, to_screen_px(&viewport, cur_world));

        let frame_t0 = Instant::now();
        let dirty_bbox = {
            let rctx = ToolReadContext {
                document: &doc,
                selection: &selection,
                snap_engine: &snap_engine,
                viewport: &viewport,
                snap_options: &snap_options,
                active_layer_id: Some(layer_id),
                creation_layer_id: Some(layer_id),
                powerclip_edit_id: None,
                treat_all_objects_as_filled: false,
                default_fill: &default_fill,
                default_outline: &default_outline,
                default_text_style: &default_text_style,
                default_lens: &default_lens,
                settings: &settings,
            };
            match tool_core.on_pointer_move(&rctx, &move_ev) {
                ToolOutput::Continue { overlay } => overlay.dirty_world_bbox_mm,
                _ => None,
            }
        };
        if let Some(prev) = prev_dirty {
            tile_cache.mark_dirty(prev.expand(1.0));
        }
        if let Some(cur_bb) = dirty_bbox {
            tile_cache.mark_dirty(cur_bb.expand(1.0));
        }
        prev_dirty = dirty_bbox;
        let _ = tile_cache.render_dirty_tiles(
            &doc,
            &viewport,
            ViewMode::Enhanced,
            &render_opts,
            &mut renderer,
        );
        let _ = tile_cache.composite_to_rgba();
        frame_ms.push(frame_t0.elapsed().as_secs_f64() * 1000.0);
    }

    let end_world = Point2D::new(
        19.0 + drag_iters as f64 * 0.5,
        19.0 + drag_iters as f64 * 0.3,
    );
    let up_ev = PointerEvent::primary(end_world, to_screen_px(&viewport, end_world));
    let mut history = CommandHistory::default();
    let rctx = ToolReadContext {
        document: &doc,
        selection: &selection,
        snap_engine: &snap_engine,
        viewport: &viewport,
        snap_options: &snap_options,
        active_layer_id: Some(layer_id),
        creation_layer_id: Some(layer_id),
        powerclip_edit_id: None,
        treat_all_objects_as_filled: false,
        default_fill: &default_fill,
        default_outline: &default_outline,
        default_text_style: &default_text_style,
        default_lens: &default_lens,
        settings: &settings,
    };
    if let ToolOutput::Commit { ops, .. } = tool_core.on_pointer_up(&rctx, &up_ev) {
        for op in ops {
            let _ = history.execute(&mut doc, op);
        }
    }

    let stats = summarize_durations(&frame_ms);
    let fps = 1000.0 / stats.mean_ms.max(0.001);
    println!(
        "tool_drag_latency: mean = {:.3} ms ({:.2} FPS)",
        stats.mean_ms, fps
    );
    if !cfg!(debug_assertions) {
        assert!(
            fps >= 60.0,
            "drag frame rate {fps:.2} FPS fell below 60.0 FPS gate"
        );
    }
}
