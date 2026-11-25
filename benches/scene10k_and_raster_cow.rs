// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Offline quantitative benchmarks (`IMP-12`):
//! 1. Headless `StudioApp::new_headless()` + `egui` first-frame cold start.
//! 2. `10,000+` objects continuous pan & render via `WorldTileKey` `DirtyTileCache` (`>= 60 FPS`).
//! 3. `300+ MB` 16-bit tiled CoW `RasterAsset` (`6400×6400`) clone + brushstroke (`>= 98%` shared tiles, RSS delta `<= 16 MiB`).

use coree_app::state::StudioApp;
use coree_bench_common::{parse_bench_iters, summarize_durations};
use coree_core::{
    BezierPath, Color, ColorMode, CowTile, Document, Fill, ImageAssetId, NodePayload, ObjectId,
    OutlineStyle, PixelDepth, Point2D, RasterAsset, Rect2D, SceneObject, Viewport, RASTER_TILE_DIM,
};
use coree_render::{paint_brush_stroke, DirtyTileCache, RenderOptions, SceneRenderer, ViewMode};
use egui::{pos2, vec2, Context, RawInput, Rect};
use std::sync::Arc;
use std::time::Instant;

#[cfg(target_os = "linux")]
fn read_process_rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages = statm.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    // Standard 4096-byte Linux page size
    Some(rss_pages.saturating_mul(4096))
}

#[cfg(not(target_os = "linux"))]
fn read_process_rss_bytes() -> Option<u64> {
    None
}

fn main() {
    let pan_iters = parse_bench_iters(60);

    // 1. Headless StudioApp + egui first-frame cold start
    let mut cold_samples_ms = Vec::with_capacity(3);
    for _ in 0..3 {
        let t0 = Instant::now();
        let mut app = StudioApp::new_headless();
        app.show_welcome = false;
        app.show_dockers = false;
        app.fonts_configured = true;
        let ctx = Context::default();
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0))),
            time: Some(0.01),
            ..RawInput::default()
        };
        let mut out = ctx.run_ui(input, |ui| app.render_ui(ui));
        out.textures_delta.clear();
        cold_samples_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let cold_stats = summarize_durations(&cold_samples_ms);
    println!("gui_cold_start: mean = {:.3} ms", cold_stats.mean_ms);

    // 2. 10,000+ objects continuous pan & render (>= 60 FPS gate in release)
    let mut doc = Document::new_a4("Scene10kPanBench");
    let lid = doc.pages()[0].layers[0].id;
    for i in 0..10_200 {
        let x = 5.0 + f64::from(i % 100) * 2.0;
        let y = 5.0 + f64::from(i / 100) * 2.5;
        let mut obj = SceneObject::new(
            ObjectId(0),
            format!("Obj{i}"),
            NodePayload::Path(BezierPath::from_rect(Rect2D::from_xywh(x, y, 1.5, 1.8))),
        )
        .with_fill(Fill::Solid(Color::rgba8(
            ((i * 29) % 240) as u8,
            ((i * 53) % 240) as u8,
            210,
            255,
        )))
        .with_outline(OutlineStyle::none());
        obj.effects.clear();
        doc.add_object(obj, Some(lid)).unwrap();
    }

    let mut viewport = Viewport::new(1280, 720);
    viewport.fit_rect(Rect2D::from_xywh(0.0, 0.0, 210.0, 297.0), 16.0);
    let render_opts = RenderOptions::interactive();
    let mut cache = DirtyTileCache::new(1280, 720);
    let mut renderer = SceneRenderer::new();
    // Warm initial viewport tiles & SpatialIndex before timed continuous pan loop
    let _ = cache.render_viewport(
        &doc,
        &viewport,
        ViewMode::Enhanced,
        &render_opts,
        &mut renderer,
    );

    let mut pan_ms = Vec::with_capacity(pan_iters);
    for step in 0..pan_iters {
        let dir = if (step / 30) % 2 == 0 { 0.45 } else { -0.45 };
        viewport.pan_mm.x += dir;
        let t0 = Instant::now();
        let _ = cache.render_dirty_tiles(
            &doc,
            &viewport,
            ViewMode::Enhanced,
            &render_opts,
            &mut renderer,
        );
        let _ = cache.composite_to_rgba();
        pan_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let pan_stats = summarize_durations(&pan_ms);
    let fps = 1000.0 / pan_stats.mean_ms.max(0.001);
    println!(
        "scene10k_pan_render: mean = {:.3} ms ({:.2} FPS)",
        pan_stats.mean_ms, fps
    );
    if !cfg!(debug_assertions) {
        assert!(
            fps >= 60.0,
            "10k scene continuous pan frame rate {fps:.2} FPS fell below 60.0 FPS gate"
        );
    }

    // 3. 300+ MB 16-bit tiled CoW RasterAsset (6400×6400 = 312.5 MiB) clone + brushstroke RAM gate
    let dim = 6400u32;
    let tiles_x = dim.div_ceil(RASTER_TILE_DIM);
    let tiles_y = dim.div_ceil(RASTER_TILE_DIM);
    let total_tiles = (tiles_x * tiles_y) as usize;
    let tile_bytes = (RASTER_TILE_DIM as usize) * (RASTER_TILE_DIM as usize) * 8;
    // Construct shared initial tile backing (real 625-tile 16-bit RasterAsset spanning 312.5 MiB logical)
    let proto_data = Arc::new(vec![0x80u8; tile_bytes]);
    let tiles = vec![
        CowTile {
            width: RASTER_TILE_DIM,
            height: RASTER_TILE_DIM,
            depth: PixelDepth::Bit16,
            data: Arc::clone(&proto_data),
        };
        total_tiles
    ];
    let asset = RasterAsset::from_tiles(
        ImageAssetId::new(),
        dim,
        dim,
        300.0,
        ColorMode::Rgb,
        PixelDepth::Bit16,
        tiles_x,
        tiles_y,
        tiles,
    )
    .expect("6400x6400 16-bit RasterAsset");
    assert!(
        asset.byte_len() >= 300 * 1024 * 1024,
        "Expected >= 300 MiB logical 16-bit raster, got {} bytes",
        asset.byte_len()
    );

    let rss_before = read_process_rss_bytes();
    let before_ptrs = asset.shared_tile_ptrs();
    let mut edited = asset.clone();
    paint_brush_stroke(
        &mut edited,
        &[Point2D::new(120.0, 120.0), Point2D::new(160.0, 120.0)],
        10.0,
        0.8,
        Color::rgba8(220, 40, 20, 255),
        1.0,
    );
    let after_ptrs = edited.shared_tile_ptrs();
    let shared_count = before_ptrs
        .iter()
        .zip(after_ptrs.iter())
        .filter(|(a, b)| a == b)
        .count();
    let shared_pct = (shared_count as f64 / total_tiles as f64) * 100.0;
    let rss_after = read_process_rss_bytes();
    let rss_delta_mb = match (rss_before, rss_after) {
        (Some(b), Some(a)) => a.saturating_sub(b) as f64 / (1024.0 * 1024.0),
        _ => 0.0,
    };
    println!(
        "cow_raster_300mb: shared_tiles = {shared_count}/{total_tiles} ({shared_pct:.2}%), rss_delta_mb = {rss_delta_mb:.2} MB"
    );
    assert!(
        shared_pct >= 98.0,
        "CoW brushstroke must keep >= 98% tiles shared (got {shared_pct:.2}%)"
    );
    if !cfg!(debug_assertions) {
        assert!(
            rss_delta_mb <= 16.0,
            "CoW brushstroke RSS delta {rss_delta_mb:.2} MiB exceeded 16 MiB ceiling"
        );
    }
}
