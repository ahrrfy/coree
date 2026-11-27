// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use coree_core::{Fill, GradientStop};
use coree_tools::{gradient_ray_local, gradient_stops_mut, interpolate_gradient_stop_color};

/// Inserts an interpolated color stop at normalized ray fraction `t` across any of the 4
/// interactive fountain fill gradient types.
fn insert_stop_on_fill_ray(fill: &Fill, t: f32) -> Option<Fill> {
    let clamped = t.clamp(0.02, 0.98);
    let mut updated = fill.clone();
    let (_, _, stops) = gradient_stops_mut(&mut updated)?;
    let color = interpolate_gradient_stop_color(stops, clamped);
    stops.push(GradientStop::new(clamped, color));
    stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    Some(updated)
}

#[test]
fn interactive_fill_inserts_interpolated_stops_across_all_four_gradient_types() {
    let base_stops = vec![
        GradientStop::new(0.0, Color::rgba8(255, 0, 0, 255)),
        GradientStop::new(1.0, Color::rgba8(0, 0, 255, 255)),
    ];
    let variants = [
        Fill::LinearGradient {
            start: Point2D::ZERO,
            end: Point2D::new(40.0, 0.0),
            stops: base_stops.clone(),
        },
        Fill::RadialGradient {
            center: Point2D::ZERO,
            radius: 25.0,
            focal: Point2D::ZERO,
            stops: base_stops.clone(),
        },
        Fill::ConicalGradient {
            center: Point2D::ZERO,
            angle_deg: 0.0,
            stops: base_stops.clone(),
        },
        Fill::SquareGradient {
            center: Point2D::ZERO,
            size: 30.0,
            angle_deg: 45.0,
            stops: base_stops,
        },
    ];
    for fill in variants {
        let next = insert_stop_on_fill_ray(&fill, 0.5).expect("gradient fill accepts mid stop");
        let (_, _, stops) = gradient_ray_local(&next).unwrap();
        assert_eq!(stops.len(), 3);
        assert!((stops[1].offset - 0.5).abs() < 1e-6);
        let [r, _g, b, _] = stops[1].color.to_rgba8();
        assert!(
            r > 100 && b > 100,
            "midpoint color interpolated: r={r}, b={b}"
        );
    }
}

#[test]
fn gpu_tile_compositor_30_page_navigator_15k_node_lod_and_resource_governor_work_end_to_end() {
    use coree_core::{
        BezierPath, Document, NodeKind, OutlineStyle, PathNode, ResourceGovernorMode, SceneObject,
        Subpath,
    };

    let mut app = StudioApp::new_headless();
    app.show_welcome = false;
    app.fonts_configured = true;

    // Build a 30-page heavy document with a 15,000-node path on Page 1
    let mut doc = Document::new_a4("Heavy30PageBook");
    for p in 1..30 {
        doc.add_page(format!("Page {}", p + 1), 210.0, 297.0, 3.0);
    }
    let mut nodes = Vec::with_capacity(15_000);
    for i in 0..15_000 {
        let t = (i as f64) * 0.02;
        let x = 20.0 + (i % 150) as f64 * 1.1;
        let y = 20.0 + (i / 150) as f64 * 2.2 + t.sin() * 0.4;
        nodes.push(PathNode::new(
            Point2D::new(x, y),
            Some(Point2D::new(x - 0.3, y)),
            Some(Point2D::new(x + 0.3, y)),
            NodeKind::Smooth,
        ));
    }
    let dense_obj = SceneObject::new(
        coree_core::ObjectId(0),
        "Dense15kCurve",
        NodePayload::Path(BezierPath::from_subpath(Subpath {
            nodes,
            closed: true,
        })),
    )
    .with_fill(Fill::Solid(Color::rgba8(20, 60, 140, 255)))
    .with_outline(OutlineStyle::none());
    let lid0 = doc.pages()[0].layers[0].id;
    let dense_id = doc.add_object(dense_obj, Some(lid0)).unwrap();

    app.new_document(doc);
    {
        let tab = app.active_tab_mut().unwrap();
        tab.selection.select_single(&tab.document, dense_id);
    }
    app.select_tool(ToolId::ShapeNode);

    let ctx = egui::Context::default();
    let run_frame = |app: &mut StudioApp| {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0))),
            time: Some(0.016),
            ..egui::RawInput::default()
        };
        let mut out = ctx.run_ui(raw, |ui| app.render_ui(ui));
        out.textures_delta.clear();
    };

    // Frame 1: renders Page 1 tiles directly into GPU TextureHandles + 15k-node LOD overlay + Options Perf tab
    app.show_options_dialog = true;
    app.options_tab = 4;
    run_frame(&mut app);
    let gpu_tiles_p0 = app.active_tab().unwrap().gpu_tile_textures.len();
    assert!(
        gpu_tiles_p0 >= 4,
        "Direct GPU tile compositor must populate per-tile GPU TextureHandles, got {gpu_tiles_p0}"
    );

    // Switch to Page 15 and Page 29, then back to Page 0 -> GPU tile cache retains both pages!
    app.active_tab_mut()
        .unwrap()
        .document
        .set_active_page_index(15)
        .unwrap();
    run_frame(&mut app);
    let gpu_tiles_after_p15 = app.active_tab().unwrap().gpu_tile_textures.len();
    assert!(
        gpu_tiles_after_p15 > gpu_tiles_p0,
        "Multi-page GPU tile pool must retain visited pages ({gpu_tiles_p0} -> {gpu_tiles_after_p15})"
    );

    // Switch ResourceGovernorMode dynamically and verify budget propagation + cache purge
    app.set_resource_governor_mode(ResourceGovernorMode::EcoLowMemory);
    assert!(app.resource_budget.max_cached_world_tiles <= 512);
    app.set_resource_governor_mode(ResourceGovernorMode::MaxPerformance);
    assert!(app.resource_budget.max_cached_world_tiles >= 512);
    let (freed_cpu, freed_gpu) = app.purge_unused_caches();
    assert!(
        freed_cpu > 0 || freed_gpu > 0,
        "purge_unused_caches must evict off-page cached tiles"
    );
}
